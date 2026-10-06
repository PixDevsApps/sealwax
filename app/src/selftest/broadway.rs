//! A minimal Broadway web client, from Stet (and its spike S1). Without a browser connected,
//! Broadway's screen is 1024×768, so a window can't be made wider, and GDK paints a surface
//! only once a second. This client tells the daemon the screen is [`SCREEN`], answers it the
//! way `broadway.js` does, and draws nothing. It also types and clicks as `broadway.js` does
//! ([`key`], [`click`]): the events reach GTK as real key and button presses through GDK's
//! Broadway backend, with the keysym already chosen, so no keyboard layout applies.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

/// Room for the widest window the scripts make.
pub const SCREEN: (i64, i64) = (2560, 1600);

/// GDK's modifier bits, as `broadway.js` sends them.
pub const SHIFT: u32 = 1 << 0;
pub const CONTROL: u32 = 1 << 2;
pub const ALT: u32 = 1 << 3;
const BUTTON1: u32 = 1 << 8;

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
const EVENT_POINTER_MOVE: i64 = 2;
const EVENT_BUTTON_PRESS: i64 = 3;
const EVENT_BUTTON_RELEASE: i64 = 4;
const EVENT_KEY_PRESS: i64 = 7;
const EVENT_KEY_RELEASE: i64 = 8;
const EVENT_CONFIGURE_NOTIFY: i64 = 11;
const EVENT_SCREEN_SIZE_CHANGED: i64 = 12;
const EVENT_ROUNDTRIP_NOTIFY: i64 = 14;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// What the input functions share with the client's thread.
struct Shared {
    writer: Mutex<TcpStream>,
    serial: AtomicU32,
    surfaces: Mutex<HashMap<u16, [i64; 4]>>,
    started: Instant,
    /// Milliseconds added to the events' times after each click, so two clicks never make a
    /// double click.
    skew: AtomicU32,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

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
    let shared = Arc::new(Shared {
        writer: Mutex::new(stream.try_clone()?),
        serial: AtomicU32::new(0),
        surfaces: Mutex::new(HashMap::new()),
        started: Instant::now(),
        skew: AtomicU32::new(0),
    });
    let _ = SHARED.set(shared.clone());
    let mut client = Client { shared };
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

/// Presses and releases `keysym` with the modifiers in `state`, on the surface Broadway has
/// focused (the window).
pub fn key(keysym: u32, state: u32) -> io::Result<()> {
    let shared = shared()?;
    for event in [EVENT_KEY_PRESS, EVENT_KEY_RELEASE] {
        shared.send(&[event, i64::from(keysym), i64::from(state)])?;
    }
    Ok(())
}

/// Moves the pointer to `(x, y)` in the window and clicks `button` there (1 the primary, 3 the
/// secondary), holding the modifiers in `state`.
pub fn click(x: f64, y: f64, state: u32, button: u32) -> io::Result<()> {
    let shared = shared()?;
    let (id, [left, top, _, _]) = shared.window()?;
    let (x, y) = (x.round() as i64, y.round() as i64);
    let id = i64::from(id);
    let at = [id, id, left + x, top + y, x, y];
    let mut moved = vec![EVENT_POINTER_MOVE];
    moved.extend(at);
    moved.push(i64::from(state));
    shared.send(&moved)?;
    // As broadway.js: the press carries the button's mask, the release no longer does.
    let held = BUTTON1 << (button.clamp(1, 5) - 1);
    for (event, state) in [
        (EVENT_BUTTON_PRESS, state | held),
        (EVENT_BUTTON_RELEASE, state),
    ] {
        let mut words = vec![event];
        words.extend(at);
        words.extend([i64::from(state), i64::from(button)]);
        shared.send(&words)?;
    }
    shared.skew.fetch_add(1_000, Ordering::Relaxed);
    Ok(())
}

/// Moves the pointer to `(x, y)` in the window, as hovering does.
pub fn hover(x: f64, y: f64) -> io::Result<()> {
    let shared = shared()?;
    let (id, [left, top, _, _]) = shared.window()?;
    let (x, y) = (x.round() as i64, y.round() as i64);
    let id = i64::from(id);
    shared.send(&[EVENT_POINTER_MOVE, id, id, left + x, top + y, x, y, 0])
}

fn shared() -> io::Result<&'static Arc<Shared>> {
    SHARED
        .get()
        .ok_or_else(|| invalid("no Broadway client is connected"))
}

impl Shared {
    fn surfaces(&self) -> MutexGuard<'_, HashMap<u16, [i64; 4]>> {
        self.surfaces.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The largest surface: the window, not a tooltip or a popover.
    fn window(&self) -> io::Result<(u16, [i64; 4])> {
        self.surfaces()
            .iter()
            .max_by_key(|(_, geometry)| geometry[2] * geometry[3])
            .map(|(id, geometry)| (*id, *geometry))
            .ok_or_else(|| invalid("Broadway shows no surface"))
    }

    /// Sends `[event, last serial, time, args...]` as big-endian 32-bit words in a masked
    /// binary frame.
    fn send(&self, event_and_args: &[i64]) -> io::Result<()> {
        let (event, args) = event_and_args
            .split_first()
            .ok_or_else(|| invalid("empty event"))?;
        let time = self.started.elapsed().as_millis() as i64
            + i64::from(self.skew.load(Ordering::Relaxed));
        let serial = i64::from(self.serial.load(Ordering::Relaxed));
        let words = [*event, serial, time]
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
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_all(&frame)
    }
}

struct Client {
    shared: Arc<Shared>,
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
            self.shared.serial.store(cursor.u32()?, Ordering::Relaxed);
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
                    self.shared.surfaces().insert(id, geometry);
                    self.configure(id)?;
                }
                OP_DESTROY_SURFACE => {
                    let id = cursor.u16()?;
                    self.shared.surfaces().remove(&id);
                }
                OP_SHOW_SURFACE | OP_HIDE_SURFACE | OP_RAISE_SURFACE | OP_LOWER_SURFACE
                | OP_SET_SHOW_KEYBOARD => cursor.skip(2)?,
                OP_MOVE_RESIZE => {
                    let id = cursor.u16()?;
                    let flags = cursor.u8()?;
                    let mut geometry = self.shared.surfaces().get(&id).copied().unwrap_or_default();
                    if flags & 1 != 0 {
                        geometry[0] = i64::from(cursor.i16()?);
                        geometry[1] = i64::from(cursor.i16()?);
                    }
                    if flags & 2 != 0 {
                        geometry[2] = i64::from(cursor.u16()?);
                        geometry[3] = i64::from(cursor.u16()?);
                    }
                    self.shared.surfaces().insert(id, geometry);
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
        let [x, y, width, height] = self.shared.surfaces().get(&id).copied().unwrap_or_default();
        self.send(&[EVENT_CONFIGURE_NOTIFY, i64::from(id), x, y, width, height])
    }

    fn send(&mut self, event_and_args: &[i64]) -> io::Result<()> {
        self.shared.send(event_and_args)
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
