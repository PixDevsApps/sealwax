//! A minimal Broadway web client, from Stet (and its spike S1). Without a browser connected,
//! Broadway's screen is 1024×768, so a window can't be made wider, and GDK paints a surface
//! only once a second. This client tells the daemon the screen is [`SCREEN`], answers it the
//! way `broadway.js` does, and draws nothing.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;

/// Room for the widest window the scripts make.
pub const SCREEN: (i64, i64) = (2560, 1600);

const OP_GRAB_POINTER: u8 = 0;
const OP_UNGRAB_POINTER: u8 = 1;
const OP_NEW_SURFACE: u8 = 2;
const OP_SHOW_SURFACE: u8 = 3;
const OP_HIDE_SURFACE: u8 = 4;
const OP_RAISE_SURFACE: u8 = 5;
const OP_LOWER_SURFACE: u8 = 6;
const OP_DESTROY_SURFACE: u8 = 7;
const OP_MOVE_RESIZE: u8 = 8;
const OP_SET_TRANSIENT_FOR: u8 = 9;
const OP_DISCONNECTED: u8 = 10;
const OP_SET_SHOW_KEYBOARD: u8 = 12;
const OP_UPLOAD_TEXTURE: u8 = 13;
const OP_RELEASE_TEXTURE: u8 = 14;
const OP_SET_NODES: u8 = 15;
const OP_ROUNDTRIP: u8 = 16;
const EVENT_CONFIGURE_NOTIFY: i64 = 11;
const EVENT_SCREEN_SIZE_CHANGED: i64 = 12;
const EVENT_ROUNDTRIP_NOTIFY: i64 = 14;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// Connects to the Broadway daemon of `$BROADWAY_DISPLAY` and serves it from a thread.
pub fn connect() -> io::Result<()> {
    let display = std::env::var("BROADWAY_DISPLAY").map_err(|_| invalid("no BROADWAY_DISPLAY"))?;
    let number: u16 = display
        .trim_start_matches(':')
        .parse()
        .map_err(|_| invalid("BROADWAY_DISPLAY is not a number"))?;
    let mut stream = TcpStream::connect(("127.0.0.1", 8080 + number))?;
    stream.write_all(
        b"GET /socket HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n\
          Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
          Sec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: broadway\r\n\
          Origin: http://127.0.0.1\r\n\r\n",
    )?;
    let mut response = Vec::new();
    let mut byte = [0];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte)?;
        response.push(byte[0]);
    }
    if !response.starts_with(b"HTTP/1.1 101") {
        return Err(invalid("the WebSocket upgrade was refused"));
    }
    let mut client = Client {
        writer: stream.try_clone()?,
        serial: 0,
        surfaces: HashMap::new(),
    };
    client.send(&[EVENT_SCREEN_SIZE_CHANGED, SCREEN.0, SCREEN.1, 1])?;
    std::thread::Builder::new()
        .name("sealwax-broadway-client".into())
        .spawn(move || {
            if let Err(error) = client.serve(stream) {
                tracing::debug!(%error, "the Broadway web client stopped");
            }
        })?;
    Ok(())
}

struct Client {
    writer: TcpStream,
    serial: u32,
    surfaces: HashMap<u16, [i64; 4]>,
}

impl Client {
    fn serve(&mut self, mut stream: TcpStream) -> io::Result<()> {
        let mut message = Vec::new();
        loop {
            let mut head = [0; 2];
            stream.read_exact(&mut head)?;
            let length = match head[1] & 0x7f {
                126 => {
                    let mut length = [0; 2];
                    stream.read_exact(&mut length)?;
                    u64::from(u16::from_be_bytes(length))
                }
                127 => {
                    let mut length = [0; 8];
                    stream.read_exact(&mut length)?;
                    u64::from_be_bytes(length)
                }
                length => u64::from(length),
            };
            let mut mask = [0; 4];
            if head[1] & 0x80 != 0 {
                stream.read_exact(&mut mask)?;
            }
            let start = message.len();
            let length = usize::try_from(length).map_err(|_| invalid("frame too large"))?;
            message.resize(start + length, 0);
            stream.read_exact(&mut message[start..])?;
            for (index, byte) in message[start..].iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
            match head[0] & 0x0f {
                8 => return Ok(()),
                0..=2 if head[0] & 0x80 != 0 => {
                    self.handle(&message)?;
                    message.clear();
                }
                0..=2 => {}
                _ => message.truncate(start),
            }
        }
    }

    fn handle(&mut self, message: &[u8]) -> io::Result<()> {
        let mut cursor = Cursor {
            data: message,
            pos: 0,
        };
        while cursor.pos < message.len() {
            let op = cursor.u8()?;
            self.serial = cursor.u32()?;
            match op {
                OP_GRAB_POINTER => cursor.skip(3)?,
                OP_UNGRAB_POINTER | OP_DISCONNECTED => {}
                OP_NEW_SURFACE => {
                    let id = cursor.u16()?;
                    let geometry = [
                        i64::from(cursor.i16()?),
                        i64::from(cursor.i16()?),
                        i64::from(cursor.u16()?),
                        i64::from(cursor.u16()?),
                    ];
                    self.surfaces.insert(id, geometry);
                    self.configure(id)?;
                }
                OP_DESTROY_SURFACE => {
                    self.surfaces.remove(&cursor.u16()?);
                }
                OP_SHOW_SURFACE | OP_HIDE_SURFACE | OP_RAISE_SURFACE | OP_LOWER_SURFACE
                | OP_SET_SHOW_KEYBOARD => cursor.skip(2)?,
                OP_MOVE_RESIZE => {
                    let id = cursor.u16()?;
                    let flags = cursor.u8()?;
                    let geometry = self.surfaces.entry(id).or_default();
                    if flags & 1 != 0 {
                        geometry[0] = i64::from(cursor.i16()?);
                        geometry[1] = i64::from(cursor.i16()?);
                    }
                    if flags & 2 != 0 {
                        geometry[2] = i64::from(cursor.u16()?);
                        geometry[3] = i64::from(cursor.u16()?);
                    }
                    self.configure(id)?;
                }
                OP_SET_TRANSIENT_FOR | OP_RELEASE_TEXTURE => cursor.skip(4)?,
                OP_UPLOAD_TEXTURE => {
                    cursor.skip(4)?;
                    let size = cursor.u32()?;
                    cursor.skip(size as usize)?;
                }
                OP_SET_NODES => {
                    cursor.skip(2)?;
                    let words = cursor.u32()?;
                    cursor.skip(words as usize * 4)?;
                }
                OP_ROUNDTRIP => {
                    let id = cursor.u16()?;
                    let tag = cursor.u32()?;
                    self.send(&[EVENT_ROUNDTRIP_NOTIFY, i64::from(id), i64::from(tag)])?;
                }
                _ => return Err(invalid("unknown Broadway op")),
            }
        }
        Ok(())
    }

    fn configure(&mut self, id: u16) -> io::Result<()> {
        let [x, y, width, height] = self.surfaces.get(&id).copied().unwrap_or_default();
        self.send(&[EVENT_CONFIGURE_NOTIFY, i64::from(id), x, y, width, height])
    }

    /// Sends `[event, last serial, timestamp, args...]` as big-endian 32-bit words in a masked
    /// binary frame.
    fn send(&mut self, event_and_args: &[i64]) -> io::Result<()> {
        let (event, args) = event_and_args
            .split_first()
            .ok_or_else(|| invalid("empty event"))?;
        let words = [*event, i64::from(self.serial), 0]
            .into_iter()
            .chain(args.iter().copied());
        let payload: Vec<u8> = words.flat_map(|word| (word as i32).to_be_bytes()).collect();
        let mask = [0x6e, 0x6f, 0x74, 0x65];
        let length = u8::try_from(payload.len()).map_err(|_| invalid("event too long"))?;
        let mut frame = vec![0x82, 0x80 | length];
        frame.extend(mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        self.writer.write_all(&frame)
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let bytes = self
            .data
            .get(self.pos..self.pos + N)
            .ok_or_else(|| invalid("truncated Broadway message"))?;
        self.pos += N;
        bytes
            .try_into()
            .map_err(|_| invalid("truncated Broadway message"))
    }

    fn skip(&mut self, count: usize) -> io::Result<()> {
        if self.pos + count > self.data.len() {
            return Err(invalid("truncated Broadway message"));
        }
        self.pos += count;
        Ok(())
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }

    fn i16(&mut self) -> io::Result<i16> {
        Ok(i16::from_le_bytes(self.take()?))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
}
