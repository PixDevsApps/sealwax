//! A local HTTP canary, from S2: a `TcpListener` on 127.0.0.1 with a random port that answers
//! every request and records what reached it, including connections that send nothing (a
//! preconnect) and TLS ClientHellos (an upgraded or https request).

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    pub connections: u64,
    /// `METHOD /path` of every request.
    pub requests: Vec<String>,
    pub silent: u64,
    pub tls_hellos: u64,
}

impl Log {
    /// What happened after `earlier`, a snapshot of the same canary.
    pub fn since(&self, earlier: &Log) -> Log {
        Log {
            connections: self.connections - earlier.connections,
            requests: self.requests[earlier.requests.len()..].to_vec(),
            silent: self.silent - earlier.silent,
            tls_hellos: self.tls_hellos - earlier.tls_hellos,
        }
    }

    pub fn is_quiet(&self) -> bool {
        self.connections == 0 && self.requests.is_empty() && self.tls_hellos == 0
    }

    /// S2's matrix notation: `N2` requests, `T1` TLS hellos, `C` a connection with nothing
    /// sent, `.` nothing.
    pub fn mark(&self) -> String {
        let mut mark = String::new();
        if !self.requests.is_empty() {
            mark.push_str(&format!("N{}", self.requests.len()));
        }
        if self.tls_hellos > 0 {
            mark.push_str(&format!("T{}", self.tls_hellos));
        }
        if mark.is_empty() && self.connections > 0 {
            mark.push('C');
        }
        mark
    }
}

pub struct Canary {
    port: u16,
    log: Arc<Mutex<Log>>,
}

impl Canary {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let log = Arc::new(Mutex::new(Log::default()));
        let accept_log = Arc::clone(&log);
        thread::Builder::new()
            .name("canary".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    lock(&accept_log).connections += 1;
                    let log = Arc::clone(&accept_log);
                    let _ = thread::Builder::new()
                        .name("canary-conn".into())
                        .spawn(move || serve(stream, &log));
                }
            })?;
        Ok(Self { port, log })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn snapshot(&self) -> Log {
        lock(&self.log).clone()
    }
}

fn lock(log: &Mutex<Log>) -> std::sync::MutexGuard<'_, Log> {
    log.lock().unwrap_or_else(PoisonError::into_inner)
}

fn serve(mut stream: TcpStream, log: &Mutex<Log>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                request.extend_from_slice(&buffer[..read]);
                if request.first() == Some(&0x16) || request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    if request.is_empty() {
        lock(log).silent += 1;
        return;
    }
    if request[0] == 0x16 {
        lock(log).tls_hellos += 1;
        return;
    }
    let line = String::from_utf8_lossy(&request);
    let mut words = line.lines().next().unwrap_or_default().split_whitespace();
    let method = words.next().unwrap_or_default().to_owned();
    let path = words.next().unwrap_or_default().to_owned();
    let (status, content_type, body) = response_for(&path);
    lock(log).requests.push(format!("{method} {path}"));
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(&body);
    let _ = stream.write_all(&response);
}

const GIF_1X1: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

fn response_for(path: &str) -> (&'static str, &'static str, Vec<u8>) {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let extension = path
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or_default();
    match extension {
        "gif" | "png" | "jpg" | "webp" | "ico" => ("200 OK", "image/gif", GIF_1X1.to_vec()),
        "css" => ("200 OK", "text/css", b"body{outline:0}".to_vec()),
        "js" => ("200 OK", "text/javascript", b";".to_vec()),
        "svg" => (
            "200 OK",
            "image/svg+xml",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>".to_vec(),
        ),
        _ if path.contains("/ping") => ("204 No Content", "text/plain", Vec::new()),
        _ => (
            "200 OK",
            "text/html",
            b"<!doctype html><title>canary</title><p>canary".to_vec(),
        ),
    }
}
