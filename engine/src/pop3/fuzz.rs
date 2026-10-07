//! The POP3 fuzz target's entry point (`crate::fuzz`): a server's bytes through the status-line,
//! CAPA and listing parsers and the line layer, which must agree with a plain reading of RFC 1939
//! and must never hand bytes read past STLS's `+OK` to TLS.

use super::Pop3Error;
use super::reply::Status;
use super::session::{Capabilities, octets, pair, uidl};
use super::wire::{STATUS_LINE, Wire};
use std::io;
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime")
});

/// A server that sends `data` in reads of at most `chunk` bytes and takes any write.
struct Server<'a> {
    data: &'a [u8],
    at: usize,
    chunk: usize,
}

impl AsyncRead for Server<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let take = self
            .chunk
            .min(buf.remaining())
            .min(self.data.len() - self.at);
        let at = self.at;
        buf.put_slice(&self.data[at..at + take]);
        self.at += take;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Server<'_> {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn wire(data: &[u8], chunk: usize) -> Wire<Server<'_>> {
    Wire::new(Server { data, at: 0, chunk }, Duration::from_secs(60))
}

/// A multi-line reply's data as RFC 1939 reads it: lines up to `.` CRLF, each without one
/// leading dot. `None` when the terminating line never comes.
fn data_reference(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for line in data.split_inclusive(|&byte| byte == b'\n') {
        if !line.ends_with(b"\n") {
            return None;
        }
        if line == b".\r\n" {
            return Some(out);
        }
        out.extend_from_slice(line.strip_prefix(b".").unwrap_or(line));
    }
    None
}

/// A listing's lines as RFC 1939 reads them, without their line ends; `None` when the reply is
/// cut short or a line is longer than `max_line`.
fn lines_reference(data: &[u8], max_line: usize) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for line in data.split_inclusive(|&byte| byte == b'\n') {
        if line.len() > max_line || !line.ends_with(b"\n") {
            return None;
        }
        if line == b".\r\n" {
            return Some(out);
        }
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        out.push(text.strip_prefix(b".").unwrap_or(text).to_vec());
    }
    None
}

pub fn pop3(data: &[u8]) {
    let mut caps = Capabilities::default();
    for line in data.split(|&byte| byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Ok(Status::Err(refusal)) = Status::parse(line) {
            let _ = refusal.login(Some(Duration::from_secs(300)));
            let _ = refusal.command();
            let _ = format!("{refusal:?}");
        }
        caps.read(line);
        if let Ok((_, value)) = pair(line) {
            let _ = uidl(value);
            let _ = octets(value);
        }
    }
    let _ = caps.sasl("PLAIN");
    let _ = format!("{caps:?}");

    let Some((&first, server)) = data.split_first() else {
        return;
    };
    let chunk = usize::from(first % 64) + 1;
    RUNTIME.block_on(async {
        let mut status = wire(server, chunk);
        let line = server
            .iter()
            .position(|&byte| byte == b'\n')
            .map(|at| at + 1);
        let result = status.status().await;
        let too_long = matches!(result, Err(Pop3Error::Protocol("status line too long")));
        match line {
            Some(end) => assert_eq!(too_long, end > STATUS_LINE, "{result:?}"),
            None if server.len() > STATUS_LINE => assert!(too_long, "{result:?}"),
            None => assert!(
                matches!(result, Err(Pop3Error::ConnectionLost)),
                "{result:?}"
            ),
        }
        if let Some(end) = line.filter(|end| *end <= STATUS_LINE) {
            match status.into_clean_inner() {
                Ok(inner) => assert_eq!(inner.at, end, "bytes after the line went to TLS"),
                Err(error) => assert!(matches!(error, Pop3Error::StartTls)),
            }
        }

        let max = server.len() + 8;
        let got = wire(server, chunk).data(max, None).await;
        match (got, data_reference(server)) {
            (Ok(got), Some(expected)) => assert_eq!(got, expected, "data unstuffed differently"),
            (Err(Pop3Error::ConnectionLost), None) => {}
            (got, expected) => panic!("data: {got:?} where RFC 1939 reads {expected:?}"),
        }

        let mut got = Vec::new();
        let result = wire(server, chunk)
            .lines(1024, 1_000_000, |line| {
                got.push(line.to_vec());
                Ok(())
            })
            .await;
        match (result, lines_reference(server, 1024)) {
            (Ok(()), Some(expected)) => assert_eq!(got, expected, "lines read differently"),
            (Err(_), None) => {}
            (result, expected) => panic!("lines: {result:?} where RFC 1939 reads {expected:?}"),
        }
    });
}
