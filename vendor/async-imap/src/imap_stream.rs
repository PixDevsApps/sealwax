use std::fmt;
use std::pin::Pin;

#[cfg(feature = "runtime-async-std")]
use async_std::io::{Read, Write, WriteExt};
use bytes::BytesMut;
use futures_util::stream::Stream;
use futures_util::task::{Context, Poll};
use futures_util::{io, ready};
#[cfg(feature = "runtime-tokio")]
use tokio::io::{AsyncRead as Read, AsyncWrite as Write, AsyncWriteExt};

use crate::types::{Request, ResponseData};

/// Wraps a stream, and parses incoming data as imap server messages. Writes outgoing data
/// as imap client messages.
#[derive(Debug)]
pub struct ImapStream<R: Read + Write> {
    /// The underlying stream
    pub(crate) inner: R,
    /// Number of bytes the response being read needs if known (the end of a literal), so the
    /// buffer can grow towards it. If unknown, 0.
    decode_needs: usize,
    /// The buffer.
    buffer: Buffer,
    /// Where the response being read ends, found as its bytes arrive (Sealwax's patch).
    framing: Framing,
    /// How many times imap-proto was asked to parse, and to look at a response before its end
    /// (Sealwax's tests).
    #[cfg(test)]
    parses: usize,
    #[cfg(test)]
    probes: usize,

    /// True if the stream should not return any more items.
    ///
    /// This is set when reading from a stream
    /// returns an error.
    /// Afterwards the stream returns only `None`
    /// and `poll_next()` does not read
    /// from the underlying stream.
    read_closed: bool,
}

impl<R: Read + Write + Unpin> ImapStream<R> {
    /// Creates a new `ImapStream` based on the given `Read`er.
    pub fn new(inner: R) -> Self {
        ImapStream {
            inner,
            buffer: Buffer::new(),
            decode_needs: 0,
            framing: Framing::new(),
            #[cfg(test)]
            parses: 0,
            #[cfg(test)]
            probes: 0,
            read_closed: false,
        }
    }

    pub async fn encode(&mut self, msg: Request) -> Result<(), io::Error> {
        log::trace!(
            "encode: input: {:?}, {:?}",
            msg.0,
            std::str::from_utf8(&msg.1)
        );

        if let Some(tag) = msg.0 {
            self.inner.write_all(tag.as_bytes()).await?;
            self.inner.write_all(b" ").await?;
        }
        self.inner.write_all(&msg.1).await?;
        self.inner.write_all(b"\r\n").await?;

        Ok(())
    }

    /// Gets a reference to the underlying stream.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Gets a mutable reference to the underlying stream.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Returns underlying stream.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Flushes the underlying stream.
    pub async fn flush(&mut self) -> Result<(), io::Error> {
        self.inner.flush().await
    }

    pub fn as_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Attempts to decode a single response from the buffer.
    ///
    /// Returns `None` if the buffer does not contain enough data.
    ///
    /// Sealwax's patch: a response is parsed once it has all arrived, a line end that doesn't
    /// announce a literal, found by looking at each new byte once. The whole buffer was parsed
    /// after every read and a read brought at most 4 KiB, so a long response without literals
    /// (a FLAGS list, a SEARCH result) took time growing with the square of its size.
    fn decode(&mut self) -> io::Result<Option<ResponseData>> {
        loop {
            let used = self.buffer.used();
            let end = match self.framing.scan(&self.buffer.block[..used]) {
                Frame::More => {
                    if used >= self.framing.probe_at {
                        self.framing.probe_at = used.saturating_mul(2);
                        self.probe(used)?;
                    }
                    return Ok(None);
                }
                Frame::End(end) => end,
                Frame::Literal { line_end, len } => {
                    // A status response's text can end in what looks like a literal
                    // (`* OK {5}`), and only the first line of a response can be one.
                    if self.framing.first_line && self.parses_alone(line_end)? {
                        line_end
                    } else {
                        let literal_end = self.framing.skip_literal(line_end, len)?;
                        self.decode_needs = literal_end;
                        continue;
                    }
                }
            };
            if let Some(response) = self.parse(end)? {
                return Ok(Some(response));
            }
            // imap-proto wants more than a whole line: parse again only once the buffer has
            // doubled, so even then the parses' cost stays linear in the response's size.
            self.framing.retry_from = end.saturating_mul(2);
        }
    }

    /// Fails at once on bytes imap-proto already refuses, as async-imap did by parsing after
    /// every read, before the response's end has come. Done only each time the response has
    /// doubled, so its cost stays linear.
    fn probe(&mut self, used: usize) -> io::Result<()> {
        #[cfg(test)]
        {
            self.probes += 1;
        }
        match imap_proto::Response::parse(&self.buffer.block[..used]) {
            Err(nom::Err::Error(error) | nom::Err::Failure(error)) => {
                Err(parse_error(&error, used))
            }
            _ => Ok(()),
        }
    }

    /// Whether the buffer's first `line_end` bytes are a whole response by themselves.
    fn parses_alone(&mut self, line_end: usize) -> io::Result<bool> {
        #[cfg(test)]
        {
            self.parses += 1;
        }
        match imap_proto::Response::parse(&self.buffer.block[..line_end]) {
            Ok(_) => Ok(true),
            Err(nom::Err::Incomplete(_)) => Ok(false),
            Err(nom::Err::Error(error) | nom::Err::Failure(error)) => {
                Err(parse_error(&error, line_end))
            }
        }
    }

    /// Parses the response that takes the buffer's first `end` bytes.
    fn parse(&mut self, end: usize) -> io::Result<Option<ResponseData>> {
        #[cfg(test)]
        {
            self.parses += 1;
        }
        let block = self.buffer.split_to(end);
        let res =
            ResponseData::try_new_or_recover(block, |buf| match imap_proto::Response::parse(buf) {
                Ok(([], response)) => Ok(response),
                Ok(_) => Err(Some(io::Error::other(format!(
                    "a response ending early during parsing of a {}-byte response",
                    buf.len()
                )))),
                Err(nom::Err::Incomplete(_)) => Err(None),
                Err(nom::Err::Error(error) | nom::Err::Failure(error)) => {
                    Err(Some(parse_error(&error, buf.len())))
                }
            });
        match res {
            Ok(response) => {
                self.decode_needs = 0;
                self.framing = Framing::new();
                Ok(Some(response))
            }
            Err((block, None)) => {
                self.buffer.unsplit_front(block);
                Ok(None)
            }
            Err((_, Some(err))) => Err(err),
        }
    }

    fn do_poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<io::Result<ResponseData>>> {
        let this = &mut *self;
        if let Some(response) = this.decode()? {
            return Poll::Ready(Some(Ok(response)));
        }
        loop {
            this.buffer.ensure_capacity(this.decode_needs)?;
            let buf = this.buffer.free_as_mut_slice();

            // The buffer should have at least one byte free
            // before we try reading into it
            // so we can treat 0 bytes read as EOF.
            // This is guaranteed by `ensure_capacity()` above
            // even if it is called with 0 as an argument.
            debug_assert!(!buf.is_empty());

            #[cfg(feature = "runtime-async-std")]
            let num_bytes_read = ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;

            #[cfg(feature = "runtime-tokio")]
            let num_bytes_read = {
                let buf = &mut tokio::io::ReadBuf::new(buf);
                let start = buf.filled().len();
                ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
                buf.filled().len() - start
            };

            if num_bytes_read == 0 {
                if this.buffer.used() > 0 {
                    return Poll::Ready(Some(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "bytes remaining in stream",
                    ))));
                }
                return Poll::Ready(None);
            }
            this.buffer.extend_used(num_bytes_read);
            if let Some(response) = this.decode()? {
                return Poll::Ready(Some(Ok(response)));
            }
        }
    }
}

/// Sealwax's patch: an error that says where parsing failed, without the response. The error
/// held the whole buffer twice over (nom's input as a list of numbers, then the text), several
/// times the size of a response a server could make as large as 512 MiB.
fn parse_error(error: &nom::error::Error<&[u8]>, len: usize) -> io::Error {
    io::Error::other(format!(
        "{:?} at byte {} during parsing of a {}-byte response",
        error.code,
        len - error.input.len(),
        len
    ))
}

/// What [`Framing::scan`] found.
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    /// The response goes on past the bytes that arrived.
    More,
    /// A line end that announces no literal: the response ends here.
    End(usize),
    /// A line that ends at `line_end` announcing a literal of `len` bytes.
    Literal { line_end: usize, len: usize },
}

/// Sealwax's patch: finds where a response ends by looking at each byte once.
///
/// A response ends at the first LF that isn't in a literal and doesn't end a line announcing
/// one (`{n}` or `{n+}` before the CRLF). Quoted strings can't hold CR or LF.
#[derive(Debug)]
struct Framing {
    /// The bytes before this offset have been looked at.
    scanned: usize,
    /// Where the line being looked at starts: after the last line end or literal.
    line_start: usize,
    /// No literal has come yet in this response.
    first_line: bool,
    /// After a parse wanted more than a whole line, line ends before this offset are passed.
    retry_from: usize,
    /// Before its end, the response is next looked at once this much of it has arrived.
    probe_at: usize,
}

impl Framing {
    fn new() -> Self {
        Self {
            scanned: 0,
            line_start: 0,
            first_line: true,
            retry_from: 0,
            probe_at: 0,
        }
    }

    fn scan(&mut self, buf: &[u8]) -> Frame {
        while self.scanned < buf.len() {
            let Some(at) = buf[self.scanned..].iter().position(|&byte| byte == b'\n') else {
                self.scanned = buf.len();
                break;
            };
            let line_end = self.scanned + at + 1;
            let line = &buf[self.line_start..line_end - 1];
            self.scanned = line_end;
            self.line_start = line_end;
            if let Some(len) = literal_len(line) {
                return Frame::Literal { line_end, len };
            }
            if line_end >= self.retry_from {
                return Frame::End(line_end);
            }
        }
        Frame::More
    }

    /// Passes over a literal's data; returns where it ends.
    fn skip_literal(&mut self, line_end: usize, len: usize) -> io::Result<usize> {
        let literal_end = line_end
            .checked_add(len)
            .filter(|&end| end <= Buffer::MAX_CAPACITY)
            .ok_or_else(|| io::Error::other("incoming data too large"))?;
        self.scanned = literal_end;
        self.line_start = literal_end;
        self.first_line = false;
        Ok(literal_end)
    }
}

/// The size a line announces for a literal after it: `{n}` or `{n+}`, then CR, at its end.
fn literal_len(line: &[u8]) -> Option<usize> {
    let line = line.strip_suffix(b"\r")?.strip_suffix(b"}")?;
    let line = line.strip_suffix(b"+").unwrap_or(line);
    let digits = line.len()
        - line
            .iter()
            .rev()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    let (head, number) = line.split_at(digits);
    if number.is_empty() || !head.ends_with(b"{") {
        return None;
    }
    number
        .iter()
        .try_fold(0usize, |len, &digit| {
            len.checked_mul(10)?.checked_add(usize::from(digit - b'0'))
        })
        .or(Some(usize::MAX))
}

/// Abstraction around needed buffer management.
struct Buffer {
    /// The buffer itself.
    block: BytesMut,
    /// Offset where used bytes range ends.
    offset: usize,
}

impl Buffer {
    const BLOCK_SIZE: usize = 1024 * 4;
    const MAX_CAPACITY: usize = 512 * 1024 * 1024; // 512 MiB

    fn new() -> Self {
        Self {
            block: BytesMut::zeroed(Self::BLOCK_SIZE),
            offset: 0,
        }
    }

    /// Returns the number of bytes in the buffer containing data.
    fn used(&self) -> usize {
        self.offset
    }

    /// Returns the unused part of the buffer to which new data can be written.
    fn free_as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.block[self.offset..]
    }

    /// Indicate how many new bytes were written into the buffer.
    ///
    /// When new bytes are written into the slice returned by [`free_as_mut_slice`] this method
    /// should be called to extend the used portion of the buffer to include the new data.
    ///
    /// You can not write past the end of the buffer, so extending more then there is free
    /// space marks the entire buffer as used.
    ///
    /// [`free_as_mut_slice`]: Self::free_as_mut_slice
    // aka advance()?
    fn extend_used(&mut self, num_bytes: usize) {
        self.offset += num_bytes;
        if self.offset > self.block.len() {
            self.offset = self.block.len();
        }
    }

    /// Ensure the buffer has free capacity, growing towards a minimum buffer size as data
    /// arrives.
    ///
    /// Sealwax's patch: the buffer grows only once it is full, and by at most its own size at a
    /// time towards `required`, a literal's announced size, so memory follows the data that
    /// arrived and not the size a server announced. It grew to `required` at once, so a single
    /// line announcing a literal of 500 MB made it allocate and zero 500 MB before any of it came.
    /// With no size known, it doubles: it grew 4 KiB at a time, so a read brought at most 4 KiB
    /// and each growth could move everything already read.
    fn ensure_capacity(&mut self, required: usize) -> io::Result<()> {
        let free_bytes: usize = self.block.len() - self.offset;
        let extra_bytes_needed: usize = required.saturating_sub(self.block.len());
        if free_bytes == 0 {
            let towards = match extra_bytes_needed {
                0 => self.block.len(),
                extra => std::cmp::min(extra, self.block.len()),
            };
            let room = Self::MAX_CAPACITY.saturating_sub(self.block.len());
            let increase = match std::cmp::min(towards, room) {
                0 => Buffer::BLOCK_SIZE,
                increase => std::cmp::max(Buffer::BLOCK_SIZE.min(room), increase),
            };
            self.grow(increase)?;
        }

        // Assert that the buffer at least one free byte.
        debug_assert!(self.offset < self.block.len());
        Ok(())
    }

    /// Grows the buffer, ensuring there are free bytes in the tail.
    ///
    /// The specified number of bytes is only a minimum.  The buffer could grow by more as
    /// it will always grow in multiples of [`BLOCK_SIZE`].
    ///
    /// If the size would be larger than [`MAX_CAPACITY`] an error is returned.
    ///
    /// [`BLOCK_SIZE`]: Self::BLOCK_SIZE
    /// [`MAX_CAPACITY`]: Self::MAX_CAPACITY
    fn grow(&mut self, num_bytes: usize) -> io::Result<()> {
        let min_size = self.block.len() + num_bytes;
        let new_size = match min_size % Self::BLOCK_SIZE {
            0 => min_size,
            n => min_size + (Self::BLOCK_SIZE - n),
        };
        if new_size > Self::MAX_CAPACITY {
            Err(io::Error::other("incoming data too large"))
        } else {
            self.block.resize(new_size, 0);
            Ok(())
        }
    }

    /// Sealwax's patch: takes the first `end` used bytes out of the buffer without copying
    /// them, for a response to own. What is left is copied into a block of its own once it is
    /// small, so it doesn't keep a large block alive.
    fn split_to(&mut self, end: usize) -> BytesMut {
        debug_assert!(end <= self.offset);
        let front = self.block.split_to(end);
        self.offset -= end;
        if self.offset <= Self::BLOCK_SIZE && self.block.len() > 2 * Self::BLOCK_SIZE {
            let rest = self.block.split_to(self.offset);
            self.reset_with_data(&rest);
        }
        front
    }

    /// Puts bytes taken by [`split_to`] back in front, when they weren't a whole response.
    ///
    /// [`split_to`]: Self::split_to
    fn unsplit_front(&mut self, mut front: BytesMut) {
        self.offset += front.len();
        front.unsplit(std::mem::take(&mut self.block));
        self.block = front;
    }

    /// Reset the buffer to be a new allocation with given data copied in.
    ///
    /// This does not do any bounds checking to see if the new buffer would exceed the
    /// maximum size.  It will however ensure that there is at least some free space at the
    /// end of the buffer so that the next reading operation won't need to realloc right
    /// away.  This could be wasteful if the next action on the buffer is another decode
    /// rather than a read, but we don't know.
    fn reset_with_data(&mut self, data: &[u8]) {
        let min_size = data.len();
        let new_size = match min_size % Self::BLOCK_SIZE {
            0 => min_size + Self::BLOCK_SIZE,
            n => min_size + (Self::BLOCK_SIZE - n),
        };
        self.block = BytesMut::zeroed(new_size);
        self.block[..data.len()].copy_from_slice(data);

        self.offset = data.len();
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("used", &self.used())
            .field("capacity", &self.block.capacity())
            .finish()
    }
}

impl<R: Read + Write + Unpin> Stream for ImapStream<R> {
    type Item = io::Result<ResponseData>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.read_closed {
            return Poll::Ready(None);
        }
        let res = match ready!(self.as_mut().do_poll_next(cx)) {
            None => None,
            Some(Err(err)) => {
                self.read_closed = true;
                Some(Err(err))
            }
            Some(Ok(item)) => Some(Ok(item)),
        };
        Poll::Ready(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pin_project::pin_project;
    use std::io::Write as _;

    /// Wrapper for a stream that
    /// fails once on a first read.
    ///
    /// Writes are discarded.
    #[pin_project]
    struct FailingStream {
        #[pin]
        inner: &'static [u8],
        has_failed: bool,
    }

    impl FailingStream {
        fn new(buf: &'static [u8]) -> Self {
            Self {
                inner: buf,
                has_failed: false,
            }
        }
    }

    #[cfg(feature = "runtime-tokio")]
    impl Read for FailingStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<tokio::io::Result<()>> {
            let this = self.project();
            if !*this.has_failed {
                *this.has_failed = true;

                Poll::Ready(Err(std::io::Error::other("Failure")))
            } else {
                this.inner.poll_read(cx, buf)
            }
        }
    }

    #[cfg(feature = "runtime-async-std")]
    impl Read for FailingStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<async_std::io::Result<usize>> {
            let this = self.project();
            if !*this.has_failed {
                *this.has_failed = true;

                Poll::Ready(Err(std::io::Error::other("Failure")))
            } else {
                this.inner.poll_read(cx, buf)
            }
        }
    }

    #[cfg(feature = "runtime-tokio")]
    impl Write for FailingStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<tokio::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<tokio::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<tokio::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[cfg(feature = "runtime-async-std")]
    impl Write for FailingStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<async_std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<async_std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<async_std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Tests that stream returns `None` after
    /// a single error of the underlying stream.
    ///
    /// This is need to prevent accidental
    /// reading from a network stream
    /// after a temporary error such as a timeout
    /// or returning an inifinite stream of errors.
    #[cfg_attr(feature = "runtime-tokio", tokio::test)]
    #[cfg_attr(feature = "runtime-async-std", async_std::test)]
    async fn test_imap_stream_error() {
        use futures_util::StreamExt;

        let mock_stream = FailingStream::new(b"* OK\r\n");
        let mut imap_stream = ImapStream::new(mock_stream);

        // First call is an error because underlying stream fails.
        assert!(imap_stream.next().await.unwrap().is_err());

        // IMAP stream should end even though underlying stream fails only once.
        assert!(imap_stream.next().await.is_none());
    }

    #[test]
    fn test_buffer_empty() {
        let buf = Buffer::new();
        assert_eq!(buf.used(), 0);

        let mut buf = Buffer::new();
        let slice: &[u8] = buf.free_as_mut_slice();
        assert_eq!(slice.len(), Buffer::BLOCK_SIZE);
        assert_eq!(slice.len(), buf.block.len());
    }

    #[test]
    fn test_buffer_extend_use() {
        let mut buf = Buffer::new();
        buf.extend_used(3);
        assert_eq!(buf.used(), 3);
        let slice = buf.free_as_mut_slice();
        assert_eq!(slice.len(), Buffer::BLOCK_SIZE - 3);

        // Extend past the end of the buffer.
        buf.extend_used(Buffer::BLOCK_SIZE);
        assert_eq!(buf.used(), Buffer::BLOCK_SIZE);
        assert_eq!(buf.offset, Buffer::BLOCK_SIZE);
        assert_eq!(buf.block.len(), buf.offset);
        let slice = buf.free_as_mut_slice();
        assert_eq!(slice.len(), 0);
    }

    #[test]
    fn test_buffer_write_read() {
        let mut buf = Buffer::new();
        let mut slice = buf.free_as_mut_slice();
        slice.write_all(b"hello").unwrap();
        buf.extend_used(b"hello".len());

        let slice = &buf.block[..buf.used()];
        assert_eq!(slice, b"hello");
        assert_eq!(buf.free_as_mut_slice().len(), buf.block.len() - buf.offset);
    }

    #[test]
    fn test_buffer_grow() {
        let mut buf = Buffer::new();
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);
        buf.grow(1).unwrap();
        assert_eq!(buf.block.len(), 2 * Buffer::BLOCK_SIZE);

        buf.grow(Buffer::BLOCK_SIZE + 1).unwrap();
        assert_eq!(buf.block.len(), 4 * Buffer::BLOCK_SIZE);

        let ret = buf.grow(Buffer::MAX_CAPACITY);
        assert!(ret.is_err());
    }

    #[test]
    fn test_buffer_ensure_capacity() {
        // Initial state: 1 byte capacity left, initial size.
        let mut buf = Buffer::new();
        buf.extend_used(Buffer::BLOCK_SIZE - 1);
        assert_eq!(buf.free_as_mut_slice().len(), 1);
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);

        // Still has capacity, no size request.
        buf.ensure_capacity(0).unwrap();
        assert_eq!(buf.free_as_mut_slice().len(), 1);
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);

        // No more capacity, initial size.
        buf.extend_used(1);
        assert_eq!(buf.free_as_mut_slice().len(), 0);
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);

        // No capacity, no size request.
        buf.ensure_capacity(0).unwrap();
        assert_eq!(buf.free_as_mut_slice().len(), Buffer::BLOCK_SIZE);
        assert_eq!(buf.block.len(), 2 * Buffer::BLOCK_SIZE);

        // Some capacity, size request: Sealwax's patch grows nothing until the buffer is full.
        buf.extend_used(5);
        assert_eq!(buf.offset, Buffer::BLOCK_SIZE + 5);
        buf.ensure_capacity(3 * Buffer::BLOCK_SIZE - 6).unwrap();
        assert_eq!(buf.free_as_mut_slice().len(), Buffer::BLOCK_SIZE - 5);
        assert_eq!(buf.block.len(), 2 * Buffer::BLOCK_SIZE);

        // No capacity, size request: what is still needed, at most the buffer's own size.
        buf.extend_used(Buffer::BLOCK_SIZE - 5);
        buf.ensure_capacity(3 * Buffer::BLOCK_SIZE - 6).unwrap();
        assert_eq!(buf.block.len(), 3 * Buffer::BLOCK_SIZE);
    }

    /// Sealwax's patch: a literal announced as 400 MB grows the buffer only as its data arrives,
    /// at most twofold at a time.
    #[test]
    fn test_ensure_capacity_follows_the_data() {
        let announced = 400_000_000;
        let mut buf = Buffer::new();
        buf.ensure_capacity(announced).unwrap();
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);

        let mut arrived = 0;
        while arrived < 1024 * 1024 {
            buf.ensure_capacity(announced).unwrap();
            let free = buf.free_as_mut_slice().len();
            buf.extend_used(free);
            arrived += free;
            assert!(buf.block.len() <= 2 * arrived.max(Buffer::BLOCK_SIZE));
        }
    }

    /// Regression test for a bug in ensure_capacity() caused
    /// by a bug in byte-pool crate 0.2.2 dependency.
    ///
    /// ensure_capacity() sometimes did not ensure that
    /// at least one byte is available, which in turn
    /// resulted in attempt to read into a buffer of zero size.
    /// When poll_read() reads into a buffer of zero size,
    /// it can only read zero bytes, which is indistinguishable
    /// from EOF and resulted in an erroneous detection of EOF
    /// when in fact the stream was not closed.
    #[test]
    fn test_ensure_capacity_loop() {
        let mut buf = Buffer::new();

        for i in 1..500 {
            // Sealwax's patch: a full buffer doubles, so start again before the limit.
            if buf.block.len() > 1024 * 1024 {
                buf = Buffer::new();
            }

            // Ask for `i` bytes.
            buf.ensure_capacity(i).unwrap();

            // Test that we can read at least 1 byte.
            let free = buf.free_as_mut_slice();
            let used = free.len();
            assert!(used > 0);

            // Use as much as allowed.
            buf.extend_used(used);

            // Test that we can read at least as much as requested.
            assert!(buf.block.len() >= i);
        }
    }

    /// Sealwax's patch: a response taken out of the buffer, and put back when incomplete.
    #[test]
    fn test_buffer_split_to_and_unsplit_front() {
        let mut buf = Buffer::new();
        buf.free_as_mut_slice()[..10].copy_from_slice(b"* OK a\r\n* ");
        buf.extend_used(10);

        let front = buf.split_to(8);
        assert_eq!(&front[..], b"* OK a\r\n");
        assert_eq!(buf.used(), 2);
        assert_eq!(&buf.block[..buf.used()], b"* ");

        buf.unsplit_front(front);
        assert_eq!(buf.used(), 10);
        assert_eq!(&buf.block[..buf.used()], b"* OK a\r\n* ");
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);
    }

    /// Sealwax's patch: what is left after a response in a large block gets a small block.
    #[test]
    fn test_buffer_split_to_leaves_a_small_block() {
        let mut buf = Buffer::new();
        buf.grow(16 * Buffer::BLOCK_SIZE).unwrap();
        let used = 10 * Buffer::BLOCK_SIZE + 3;
        buf.extend_used(used);
        let front = buf.split_to(used - 3);
        assert_eq!(front.len(), used - 3);
        assert_eq!(buf.used(), 3);
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);
    }

    /// Sealwax's patch: with no size known, a full buffer doubles, up to the limit.
    #[test]
    fn test_buffer_doubles_when_full() {
        let mut buf = Buffer::new();
        for size in [2, 4, 8, 16] {
            let free = buf.free_as_mut_slice().len();
            buf.extend_used(free);
            buf.ensure_capacity(0).unwrap();
            assert_eq!(buf.block.len(), size * Buffer::BLOCK_SIZE);
        }

        let mut buf = Buffer::new();
        buf.grow(Buffer::MAX_CAPACITY - 3 * Buffer::BLOCK_SIZE)
            .unwrap();
        let free = buf.free_as_mut_slice().len();
        buf.extend_used(free);
        buf.ensure_capacity(0).unwrap();
        assert_eq!(buf.block.len(), Buffer::MAX_CAPACITY);
        buf.extend_used(Buffer::MAX_CAPACITY);
        assert!(buf.ensure_capacity(0).is_err());
    }

    #[test]
    fn test_literal_len() {
        assert_eq!(literal_len(b"* 1 FETCH (BODY[] {12}\r"), Some(12));
        assert_eq!(literal_len(b"{0}\r"), Some(0));
        assert_eq!(literal_len(b"x {7+}\r"), Some(7));
        assert_eq!(
            literal_len(b"x {99999999999999999999999}\r"),
            Some(usize::MAX)
        );
        assert_eq!(literal_len(b"* 1 FETCH (BODY[] {12}"), None);
        assert_eq!(literal_len(b"* OK }\r"), None);
        assert_eq!(literal_len(b"* OK {}\r"), None);
        assert_eq!(literal_len(b"* OK 12}\r"), None);
        assert_eq!(literal_len(b"* OK {1a}\r"), None);
        assert_eq!(literal_len(b"\r"), None);
    }

    #[test]
    fn test_framing_passes_over_literals() {
        let response = b"* 1 FETCH (BODY[] {5}\r\nab\r\nc FLAGS ())\r\n* 2 EXISTS\r\n";
        let mut framing = Framing::new();
        assert_eq!(framing.scan(&response[..10]), Frame::More);
        assert_eq!(
            framing.scan(response),
            Frame::Literal {
                line_end: 23,
                len: 5
            }
        );
        assert_eq!(framing.skip_literal(23, 5).unwrap(), 28);
        assert_eq!(framing.scan(&response[..27]), Frame::More);
        assert_eq!(framing.scan(&response[..35]), Frame::More);
        assert_eq!(framing.scan(response), Frame::End(40));
        assert!(Framing::new().skip_literal(10, usize::MAX).is_err());
        assert!(
            Framing::new()
                .skip_literal(10, Buffer::MAX_CAPACITY)
                .is_err()
        );
    }

    /// Delivers `data` to an `ImapStream` in pieces of `piece` bytes and reads every response.
    #[cfg(feature = "runtime-tokio")]
    async fn read_in_pieces(
        data: Vec<u8>,
        piece: usize,
    ) -> (Vec<io::Result<ResponseData>>, usize, usize) {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let (client, mut server) = tokio::io::duplex(piece);
        let writer = tokio::spawn(async move {
            for chunk in data.chunks(piece) {
                if server.write_all(chunk).await.is_err() {
                    break;
                }
            }
        });
        let mut stream = ImapStream::new(client);
        let mut responses = Vec::new();
        while let Some(response) = stream.next().await {
            let failed = response.is_err();
            responses.push(response);
            if failed {
                break;
            }
        }
        let (parses, probes) = (stream.parses, stream.probes);
        drop(stream);
        writer.await.unwrap();
        (responses, parses, probes)
    }

    /// Sealwax's patch: a long response without literals is parsed once, however it arrives.
    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn test_a_long_response_is_parsed_once() {
        let mut response = b"* 1 FETCH (FLAGS (".to_vec();
        while response.len() < 1_000_000 {
            response.extend_from_slice(b"\\a ");
        }
        response.extend_from_slice(b"\\a))\r\n");
        let (responses, parses, probes) = read_in_pieces(response, 1024).await;
        assert_eq!(responses.len(), 1);
        assert!(responses[0].is_ok());
        assert_eq!(parses, 1);
        // Looked at before its end at most once each time it doubled, from the first byte.
        assert!(probes <= 21, "{probes}");
    }

    /// Sealwax's patch: responses with literals, in any pieces, each parsed once, and a status
    /// whose text looks like a literal announcement.
    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn test_responses_in_pieces() {
        let data = b"* OK greeting {5}\r\n\
            * 1 FETCH (UID 7 BODY[] {13}\r\nab\r\n{3}\r\ncd\r\n FLAGS (\\Seen))\r\n\
            * 2 FETCH (BODY[HEADER] {0}\r\n BODY[TEXT] {4}\r\nxyz\n)\r\n\
            * SEARCH 1 2 3\r\n\
            A1 OK done\r\n"
            .to_vec();
        for piece in [1, 2, 3, 7, 64, 4096] {
            let (responses, parses, _) = read_in_pieces(data.clone(), piece).await;
            let responses: Vec<_> = responses.into_iter().map(|r| r.unwrap()).collect();
            assert_eq!(responses.len(), 5, "pieces of {piece}");
            assert!(matches!(
                responses[0].parsed(),
                imap_proto::Response::Data { .. }
            ));
            match responses[1].parsed() {
                imap_proto::Response::Fetch(1, attributes) => assert!(attributes.contains(
                    &imap_proto::AttributeValue::BodySection {
                        section: None,
                        index: None,
                        data: Some(std::borrow::Cow::Borrowed(&b"ab\r\n{3}\r\ncd\r\n"[..])),
                    }
                )),
                other => panic!("{other:?}"),
            }
            assert!(matches!(
                responses[2].parsed(),
                imap_proto::Response::Fetch(2, _)
            ));
            assert!(matches!(
                responses[4].parsed(),
                imap_proto::Response::Done { .. }
            ));
            // The greeting's and the FETCHes' first lines are each parsed once more, to tell a
            // literal from a status's text.
            assert_eq!(parses, 8, "pieces of {piece}");
        }
    }

    /// Sealwax's patch: responses split at random places come out as imap-proto reads them
    /// whole, each parsed once or, for a first line that announces a literal, twice.
    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn test_random_pieces_read_as_whole() {
        const RESPONSES: &[&[u8]] = &[
            b"* OK [CAPABILITY IMAP4rev1 IDLE] ready {3}\r\n",
            b"* 3 EXISTS\r\n",
            b"* FLAGS (\\Seen \\Answered $Forwarded)\r\n",
            b"* SEARCH 2 84 882\r\n",
            b"* 1 FETCH (UID 4 FLAGS (\\Seen) BODY[] {7}\r\nab\r\n}\r\n)\r\n",
            b"* 2 FETCH (UID 5 BODY[HEADER] {0}\r\n BODY[TEXT] {7}\r\n{2}\r\n\r\n)\r\n",
            b"* 3 FETCH (UID 6 ENVELOPE (\"d\" {4}\r\nsubj NIL NIL NIL NIL NIL NIL NIL \"<id>\"))\r\n",
            b"* LIST (\\HasNoChildren) \"/\" \"a {1}\"\r\n",
            b"+ idling\r\n",
            b"A7 OK [READ-WRITE] SELECT completed\r\n",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as usize
        };
        for _ in 0..200 {
            let mut data = Vec::new();
            for _ in 0..1 + next(12) {
                data.extend_from_slice(RESPONSES[next(RESPONSES.len())]);
            }
            let mut expected = Vec::new();
            let mut rest = &data[..];
            while !rest.is_empty() {
                let (remaining, response) = imap_proto::Response::parse(rest).unwrap();
                expected.push(format!("{response:?}"));
                rest = remaining;
            }
            let piece = 1 + next(40);
            let (responses, parses, _) = read_in_pieces(data.clone(), piece).await;
            let got: Vec<String> = responses
                .into_iter()
                .map(|response| format!("{:?}", response.unwrap().parsed()))
                .collect();
            assert_eq!(
                got,
                expected,
                "{:?} in pieces of {piece}",
                String::from_utf8_lossy(&data)
            );
            assert!(parses <= 2 * expected.len());
        }
    }

    /// Sealwax's patch: bytes imap-proto refuses fail at once, before a line end comes, as they
    /// did when every read was parsed.
    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn test_refused_bytes_fail_before_a_line_end() {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let (client, mut server) = tokio::io::duplex(64 * 1024);
        server
            .write_all(format!("* 1 FETCH (BODYSTRUCTURE {}", "(".repeat(10_000)).as_bytes())
            .await
            .unwrap();
        let mut stream = ImapStream::new(client);
        let error = stream.next().await.unwrap().unwrap_err();
        assert!(error.to_string().contains(" during parsing of "), "{error}");
        drop(server);
    }

    /// Sealwax's patch: a response imap-proto can't read gives a short error that holds none of
    /// the response's bytes.
    #[cfg(feature = "runtime-tokio")]
    #[tokio::test]
    async fn test_a_parse_error_holds_no_response() {
        let mut data = b"* 1 FETCH (BODY[] {1000000}\r\n".to_vec();
        data.resize(data.len() + 1_000_000, 0x01);
        data.extend_from_slice(b"X\r\n");
        let (responses, _, _) = read_in_pieces(data, 64 * 1024).await;
        let error = responses.last().unwrap().as_ref().unwrap_err().to_string();
        assert!(error.contains(" during parsing of "), "{error}");
        assert!(error.len() < 120, "{error}");
        assert!(
            !error.contains("\\u{1}") && !error.contains("[1, 1"),
            "{error}"
        );

        let (responses, _, _) = read_in_pieces(b"* 1 FETCH (x@example.test\r\n".to_vec(), 7).await;
        let error = responses.last().unwrap().as_ref().unwrap_err().to_string();
        assert!(
            error.contains(" during parsing of a 27-byte response"),
            "{error}"
        );
        assert!(!error.contains("example"), "{error}");
    }

    #[test]
    fn test_buffer_reset_with_data() {
        // This test identifies blocks by their size.
        let data: [u8; 2 * Buffer::BLOCK_SIZE] = [b'a'; 2 * Buffer::BLOCK_SIZE];
        let mut buf = Buffer::new();
        let block_size = buf.block.len();
        assert_eq!(block_size, Buffer::BLOCK_SIZE);
        buf.reset_with_data(&data);
        assert_ne!(buf.block.len(), block_size);
        assert_eq!(buf.block.len(), 3 * Buffer::BLOCK_SIZE);
        assert!(!buf.free_as_mut_slice().is_empty());

        let data: [u8; 0] = [];
        let mut buf = Buffer::new();
        buf.reset_with_data(&data);
        assert_eq!(buf.block.len(), Buffer::BLOCK_SIZE);
    }

    #[test]
    fn test_buffer_debug() {
        assert_eq!(
            format!("{:?}", Buffer::new()),
            format!(r#"Buffer {{ used: 0, capacity: {} }}"#, Buffer::BLOCK_SIZE)
        );
    }
}
