//! A connection's TLS stream, counting the bytes it reads, so the raw layer can hold each
//! command to its budget ([`super::raw::Budget`]).

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

#[derive(Debug)]
pub(crate) struct Stream {
    tls: TlsStream<TcpStream>,
    read: u64,
}

impl Stream {
    pub(crate) fn new(tls: TlsStream<TcpStream>) -> Self {
        Stream { tls, read: 0 }
    }

    /// The bytes read from the server since the connection was made.
    pub(crate) fn read(&self) -> u64 {
        self.read
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        std::task::ready!(Pin::new(&mut self.tls).poll_read(cx, buf))?;
        self.read += (buf.filled().len() - before) as u64;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.tls).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.tls).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.tls).poll_shutdown(cx)
    }
}
