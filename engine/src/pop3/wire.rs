//! The line layer: commands out, status lines and dot-terminated multi-line replies in. Every
//! read waits at most the command timeout for the next bytes (and a multi-line reply may also
//! have a deadline for the whole), and every line has a length limit, so a server can neither
//! hang a session nor fill memory. Nothing here logs.

use super::Pop3Error;
use super::reply::Status;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::Instant;
use zeroize::Zeroizing;

/// The longest status line accepted. RFC 2449 allows 512 octets; greetings run longer.
pub(super) const STATUS_LINE: usize = 4096;

/// The read buffer: a message's data comes in reads of up to this size.
const BUFFER: usize = 64 * 1024;

/// What a multi-line reply read line by line may hold.
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    /// Octets in a line, its line end included.
    pub line: usize,
    pub lines: usize,
    /// Octets in all, line ends included.
    pub octets: usize,
}

pub(super) struct Wire<S> {
    stream: BufReader<S>,
    timeout: Duration,
}

fn read_error(error: io::Error) -> Pop3Error {
    if error.kind() == io::ErrorKind::TimedOut {
        Pop3Error::Timeout
    } else {
        Pop3Error::ConnectionLost
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Wire<S> {
    pub fn new(stream: S, timeout: Duration) -> Self {
        Wire {
            stream: BufReader::with_capacity(BUFFER, stream),
            timeout,
        }
    }

    /// The stream, when the server sent nothing past the last reply read: after STLS's `+OK`,
    /// anything buffered came in plaintext and must never be read as if it came over TLS.
    pub fn into_clean_inner(self) -> Result<S, Pop3Error> {
        if self.stream.buffer().is_empty() {
            Ok(self.stream.into_inner())
        } else {
            Err(Pop3Error::StartTls)
        }
    }

    pub fn get_mut(&mut self) -> &mut S {
        self.stream.get_mut()
    }

    /// Sends `line` and CRLF in one write. The copy is wiped afterwards (PASS and AUTH lines).
    pub async fn send(&mut self, line: &[u8]) -> Result<(), Pop3Error> {
        let mut bytes = Zeroizing::new(Vec::with_capacity(line.len() + 2));
        bytes.extend_from_slice(line);
        bytes.extend_from_slice(b"\r\n");
        let stream = self.stream.get_mut();
        let write = async {
            stream.write_all(&bytes).await?;
            stream.flush().await
        };
        match tokio::time::timeout(self.timeout, write).await {
            Err(_) => Err(Pop3Error::Timeout),
            Ok(Err(error)) => Err(read_error(error)),
            Ok(Ok(())) => Ok(()),
        }
    }

    /// The time the next read may take.
    fn wait(&self, deadline: Option<Instant>) -> Result<Duration, Pop3Error> {
        match deadline {
            None => Ok(self.timeout),
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    Err(Pop3Error::Timeout)
                } else {
                    Ok(left.min(self.timeout))
                }
            }
        }
    }

    /// Appends one line, up to and including its LF, to `out`. A line longer than `max` octets
    /// is `TooLarge` with what was read of it so far.
    async fn line_into(
        &mut self,
        out: &mut Vec<u8>,
        max: usize,
        deadline: Option<Instant>,
    ) -> Result<(), Pop3Error> {
        let start = out.len();
        loop {
            let wait = self.wait(deadline)?;
            let available = match tokio::time::timeout(wait, self.stream.fill_buf()).await {
                Err(_) => return Err(Pop3Error::Timeout),
                Ok(Err(error)) => return Err(read_error(error)),
                Ok(Ok(available)) => available,
            };
            if available.is_empty() {
                return Err(Pop3Error::ConnectionLost);
            }
            let (take, done) = match available.iter().position(|&byte| byte == b'\n') {
                Some(at) => (at + 1, true),
                None => (available.len(), false),
            };
            let length = out.len() - start + take;
            if length > max {
                return Err(Pop3Error::TooLarge(length as u64));
            }
            out.extend_from_slice(&available[..take]);
            self.stream.consume(take);
            if done {
                return Ok(());
            }
        }
    }

    /// One status line.
    pub async fn status(&mut self) -> Result<Status, Pop3Error> {
        let mut line = Vec::new();
        self.line_into(&mut line, STATUS_LINE, None)
            .await
            .map_err(|error| match error {
                Pop3Error::TooLarge(_) => Pop3Error::Protocol("status line too long"),
                error => error,
            })?;
        Status::parse(trim_line_end(&line))
    }

    /// A multi-line reply's data after its `+OK`, up to the line `.`: dot-unstuffed (RFC 1939,
    /// 3), every other byte and line end as sent. More than `max` octets is `TooLarge`, with
    /// the reply left unread.
    pub async fn data(
        &mut self,
        max: usize,
        deadline: Option<Instant>,
    ) -> Result<Vec<u8>, Pop3Error> {
        let mut out = Vec::new();
        loop {
            let start = out.len();
            let room = (max - start).saturating_add(3);
            self.line_into(&mut out, room, deadline)
                .await
                .map_err(|error| match error {
                    Pop3Error::TooLarge(length) => Pop3Error::TooLarge(start as u64 + length),
                    error => error,
                })?;
            if &out[start..] == b".\r\n" {
                out.truncate(start);
                return Ok(out);
            }
            if out[start] == b'.' {
                out.remove(start);
            }
            if out.len() > max {
                return Err(Pop3Error::TooLarge(out.len() as u64));
            }
        }
    }

    /// Calls `each` with every line of a multi-line reply, dot-unstuffed and without its line
    /// end, within `limits`. When `deadline` gives a time for the octets read so far, the next
    /// line must have come by then.
    pub async fn lines(
        &mut self,
        limits: Limits,
        deadline: impl Fn(u64) -> Option<Instant>,
        mut each: impl FnMut(&[u8]) -> Result<(), Pop3Error>,
    ) -> Result<(), Pop3Error> {
        let mut line = Vec::with_capacity(128);
        let mut read = 0;
        for _ in 0..=limits.lines {
            line.clear();
            self.line_into(&mut line, limits.line, deadline(read as u64))
                .await
                .map_err(|error| match error {
                    Pop3Error::TooLarge(_) => Pop3Error::Protocol("line too long"),
                    error => error,
                })?;
            read += line.len();
            if read > limits.octets {
                return Err(Pop3Error::Protocol("reply too long"));
            }
            if line == b".\r\n" {
                return Ok(());
            }
            let text = trim_line_end(&line);
            each(text.strip_prefix(b".").unwrap_or(text))?;
        }
        Err(Pop3Error::Protocol("too many lines"))
    }
}

/// `line` without its CRLF or bare LF.
fn trim_line_end(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    async fn server(sent: &[u8]) -> Wire<DuplexStream> {
        let (client, mut far) = tokio::io::duplex(1 << 20);
        far.write_all(sent).await.unwrap();
        tokio::spawn(async move {
            let mut sink = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut far, &mut sink).await;
        });
        Wire::new(client, Duration::from_secs(5))
    }

    #[tokio::test]
    async fn data_is_unstuffed_with_line_ends_and_bytes_kept() {
        let sent: &[u8] =
            b"Subject: x\r\n\r\n..\r\n...two\r\n.\nbare\nlatin \xe6\xf8\xe5 utf8 \xc3\xa6\r\n\
                          .x\r\nlast\r\n.\r\n+OK next\r\n";
        let mut wire = server(sent).await;
        let data = wire.data(1 << 20, None).await.unwrap();
        assert_eq!(
            data,
            b"Subject: x\r\n\r\n.\r\n..two\r\n\nbare\nlatin \xe6\xf8\xe5 utf8 \xc3\xa6\r\nx\r\nlast\r\n"
        );
        assert!(matches!(wire.status().await.unwrap(), Status::Ok(text) if text == b"next"));
    }

    #[tokio::test]
    async fn data_over_the_limit_is_too_large() {
        let mut wire = server(b"0123456789\r\n0123456789\r\n.\r\n").await;
        assert!(matches!(
            wire.data(15, None).await,
            Err(Pop3Error::TooLarge(n)) if n > 15
        ));
        let mut wire = server(b"0123456789\r\n.\r\n").await;
        assert_eq!(wire.data(12, None).await.unwrap(), b"0123456789\r\n");
        let mut wire = server(b".\r\n").await;
        assert_eq!(wire.data(0, None).await.unwrap(), b"");
    }

    #[tokio::test]
    async fn a_cut_or_a_silent_server_ends_the_read() {
        let (client, mut far) = tokio::io::duplex(1024);
        far.write_all(b"half a line").await.unwrap();
        drop(far);
        let mut cut = Wire::new(client, Duration::from_secs(5));
        assert!(matches!(
            cut.data(1024, None).await,
            Err(Pop3Error::ConnectionLost)
        ));
        let (client, mut far) = tokio::io::duplex(1024);
        far.write_all(b"+OK\r\nsome").await.unwrap();
        let mut silent = Wire::new(client, Duration::from_millis(50));
        assert!(matches!(silent.status().await, Ok(Status::Ok(_))));
        assert!(matches!(
            silent.data(1024, None).await,
            Err(Pop3Error::Timeout)
        ));
        let deadline = Instant::now() + Duration::from_millis(30);
        let mut wire = Wire::new(far, Duration::from_secs(5));
        assert!(matches!(
            wire.data(1024, Some(deadline)).await,
            Err(Pop3Error::Timeout)
        ));
    }

    #[tokio::test]
    async fn lines_and_status_lines_have_limits() {
        let long = format!("+OK {}\r\n", "x".repeat(STATUS_LINE));
        let mut wire = server(long.as_bytes()).await;
        assert!(matches!(
            wire.status().await,
            Err(Pop3Error::Protocol("status line too long"))
        ));
        let limits = Limits {
            line: 64,
            lines: 10,
            octets: 1024,
        };
        let mut wire = server(b"1 a\r\n2 b\n..3 c\r\n.\r\n").await;
        let mut seen = Vec::new();
        wire.lines(
            limits,
            |_| None,
            |line| {
                seen.push(line.to_vec());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(seen, [&b"1 a"[..], b"2 b", b".3 c"]);
        let mut wire = server(b"1\r\n2\r\n3\r\n.\r\n").await;
        let few = Limits { lines: 2, ..limits };
        assert!(matches!(
            wire.lines(few, |_| None, |_| Ok(())).await,
            Err(Pop3Error::Protocol("too many lines"))
        ));
        let mut wire = server(b"12345\r\n12345\r\n.\r\n").await;
        let short = Limits {
            octets: 15,
            ..limits
        };
        assert!(matches!(
            wire.lines(short, |_| None, |_| Ok(())).await,
            Err(Pop3Error::Protocol("reply too long"))
        ));
        let mut wire = server(b"12345\r\n12345\r\n.\r\n").await;
        let exact = Limits {
            octets: 17,
            ..limits
        };
        wire.lines(exact, |_| None, |_| Ok(())).await.unwrap();
    }

    #[tokio::test]
    async fn a_listing_has_until_its_deadline_for_the_octets_read() {
        let (client, mut far) = tokio::io::duplex(1024);
        let limits = Limits {
            line: 64,
            lines: 100,
            octets: 1024,
        };
        tokio::spawn(async move {
            for n in 0..20 {
                let line = format!("{n} uid-{n}\r\n");
                if far.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let _ = far.write_all(b".\r\n").await;
        });
        let started = Instant::now();
        let mut wire = Wire::new(client, Duration::from_secs(5));
        let mut seen = 0;
        let deadline = |read: u64| Some(started + Duration::from_millis(120 + read));
        let result = wire
            .lines(limits, deadline, |_| {
                seen += 1;
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(Pop3Error::Timeout)), "{result:?}");
        assert!((1..10).contains(&seen), "{seen}");
        assert!(started.elapsed() < Duration::from_millis(700));
    }

    #[tokio::test]
    async fn plaintext_after_stls_is_refused() {
        let mut wire = server(b"+OK Begin TLS\r\n+OK injected\r\n").await;
        assert!(matches!(wire.status().await, Ok(Status::Ok(_))));
        assert!(matches!(wire.into_clean_inner(), Err(Pop3Error::StartTls)));
    }
}
