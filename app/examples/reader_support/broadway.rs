//! A stand-in Broadway web client, from the self-test harness (`app/src/selftest/broadway.rs`),
//! that can also send input: key presses, pointer clicks and wheel steps arrive in GTK as real
//! events from the backend, the way a browser client would send them. It reports a 2560×1600
//! screen, so windows can be large and GDK paints without its once-a-second pacing.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, PoisonError};

const SCREEN: (i64, i64) = (2560, 1600);

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

const EVENT_ENTER: i64 = 0;
const EVENT_POINTER_MOVE: i64 = 2;
const EVENT_BUTTON_PRESS: i64 = 3;
const EVENT_BUTTON_RELEASE: i64 = 4;
const EVENT_SCROLL: i64 = 6;
const EVENT_KEY_PRESS: i64 = 7;
const EVENT_KEY_RELEASE: i64 = 8;
const EVENT_CONFIGURE_NOTIFY: i64 = 11;
const EVENT_SCREEN_SIZE_CHANGED: i64 = 12;
const EVENT_ROUNDTRIP_NOTIFY: i64 = 14;

/// GDK's `BUTTON1_MASK`.
const BUTTON1: i64 = 1 << 8;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

#[derive(Default)]
struct Shared {
    serial: u32,
    /// Surface id → x, y, width, height.
    surfaces: HashMap<u16, [i64; 4]>,
}

/// Sends input to the Broadway daemon; cheap to clone.
#[derive(Clone)]
pub struct Input {
    writer: Arc<Mutex<TcpStream>>,
    shared: Arc<Mutex<Shared>>,
}

/// Connects to the daemon of `$BROADWAY_DISPLAY` and serves it from a thread.
pub fn connect() -> io::Result<Input> {
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
    let input = Input {
        writer: Arc::new(Mutex::new(stream.try_clone()?)),
        shared: Arc::default(),
    };
    input.send(&[EVENT_SCREEN_SIZE_CHANGED, SCREEN.0, SCREEN.1, 1])?;
    let serving = input.clone();
    std::thread::Builder::new()
        .name("reader-broadway-client".into())
        .spawn(move || {
            if let Err(error) = serving.serve(stream) {
                eprintln!("broadway client stopped: {error}");
            }
        })?;
    Ok(input)
}

impl Input {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The biggest surface: the main window.
    fn window(&self) -> Option<(u16, [i64; 4])> {
        self.lock()
            .surfaces
            .iter()
            .max_by_key(|(_, [_, _, width, height])| width * height)
            .map(|(id, geometry)| (*id, *geometry))
    }

    /// A key press and its release on the focused surface; `keyval` is GDK's.
    pub fn key(&self, keyval: u32, state: u32) -> io::Result<()> {
        let (keyval, state) = (i64::from(keyval), i64::from(state));
        self.send(&[EVENT_KEY_PRESS, keyval, state])?;
        self.send(&[EVENT_KEY_RELEASE, keyval, state])
    }

    fn pointer(
        &self,
        event: i64,
        x: f64,
        y: f64,
        state: i64,
        extra: Option<i64>,
    ) -> io::Result<()> {
        let (id, [left, top, _, _]) = self.window().ok_or_else(|| invalid("no window yet"))?;
        let (x, y) = (x.round() as i64, y.round() as i64);
        let id = i64::from(id);
        let mut words = vec![event, id, id, left + x, top + y, x, y, state];
        words.extend(extra);
        self.send(&words)
    }

    /// Moves the pointer to `x, y` in the window's surface.
    pub fn motion(&self, x: f64, y: f64) -> io::Result<()> {
        self.pointer(EVENT_ENTER, x, y, 0, Some(0))?;
        self.pointer(EVENT_POINTER_MOVE, x, y, 0, None)
    }

    /// A left click at `x, y` in the window's surface.
    pub fn click(&self, x: f64, y: f64) -> io::Result<()> {
        self.motion(x, y)?;
        self.pointer(EVENT_BUTTON_PRESS, x, y, 0, Some(1))?;
        self.pointer(EVENT_BUTTON_RELEASE, x, y, BUTTON1, Some(1))
    }

    /// One wheel step at `x, y`: down when `down`, else up.
    pub fn scroll(&self, x: f64, y: f64, down: bool) -> io::Result<()> {
        self.motion(x, y)?;
        self.pointer(EVENT_SCROLL, x, y, 0, Some(i64::from(down)))
    }

    fn serve(&self, mut stream: TcpStream) -> io::Result<()> {
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

    fn handle(&self, message: &[u8]) -> io::Result<()> {
        let mut cursor = Cursor {
            data: message,
            pos: 0,
        };
        while cursor.pos < message.len() {
            let op = cursor.u8()?;
            let serial = cursor.u32()?;
            self.lock().serial = serial;
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
                    self.lock().surfaces.insert(id, geometry);
                    self.configure(id)?;
                }
                OP_DESTROY_SURFACE => {
                    let id = cursor.u16()?;
                    self.lock().surfaces.remove(&id);
                }
                OP_SHOW_SURFACE | OP_HIDE_SURFACE | OP_RAISE_SURFACE | OP_LOWER_SURFACE
                | OP_SET_SHOW_KEYBOARD => cursor.skip(2)?,
                OP_MOVE_RESIZE => {
                    let id = cursor.u16()?;
                    let flags = cursor.u8()?;
                    let mut position = None;
                    let mut size = None;
                    if flags & 1 != 0 {
                        position = Some((i64::from(cursor.i16()?), i64::from(cursor.i16()?)));
                    }
                    if flags & 2 != 0 {
                        size = Some((i64::from(cursor.u16()?), i64::from(cursor.u16()?)));
                    }
                    {
                        let mut shared = self.lock();
                        let geometry = shared.surfaces.entry(id).or_default();
                        if let Some((x, y)) = position {
                            geometry[0] = x;
                            geometry[1] = y;
                        }
                        if let Some((width, height)) = size {
                            geometry[2] = width;
                            geometry[3] = height;
                        }
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

    fn configure(&self, id: u16) -> io::Result<()> {
        let [x, y, width, height] = self.lock().surfaces.get(&id).copied().unwrap_or_default();
        self.send(&[EVENT_CONFIGURE_NOTIFY, i64::from(id), x, y, width, height])
    }

    /// Sends `[event, last serial, timestamp, args...]` as big-endian 32-bit words in a masked
    /// binary frame.
    fn send(&self, event_and_args: &[i64]) -> io::Result<()> {
        let (event, args) = event_and_args
            .split_first()
            .ok_or_else(|| invalid("empty event"))?;
        let serial = i64::from(self.lock().serial);
        let words = [*event, serial, 0].into_iter().chain(args.iter().copied());
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
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_all(&frame)
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
