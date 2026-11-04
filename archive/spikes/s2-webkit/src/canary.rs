//! A local HTTP canary: a `TcpListener` on 127.0.0.1 with a random port that answers every
//! request and records what reached it. A connection that sends nothing (a preconnect) and a
//! TLS ClientHello (an upgraded or https request) are recorded too.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Log {
    pub connections: u64,
    pub hits: Vec<Hit>,
    pub silent_connections: u64,
    pub tls_hellos: u64,
}

impl Log {
    /// What happened after `earlier`, a snapshot of the same canary.
    pub fn since(&self, earlier: &Log) -> Log {
        Log {
            connections: self.connections - earlier.connections,
            hits: self.hits[earlier.hits.len()..].to_vec(),
            silent_connections: self.silent_connections - earlier.silent_connections,
            tls_hellos: self.tls_hellos - earlier.tls_hellos,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.connections == 0 && self.hits.is_empty()
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
        lock(log).silent_connections += 1;
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
    lock(log).hits.push(Hit { method, path });
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(canary: &Canary, check: impl Fn(&Log) -> bool) -> Log {
        for _ in 0..200 {
            let log = canary.snapshot();
            if check(&log) {
                return log;
            }
            thread::sleep(Duration::from_millis(10));
        }
        canary.snapshot()
    }

    #[test]
    fn records_requests_silent_connections_and_tls() {
        let canary = Canary::start().unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", canary.port())).unwrap();
        stream
            .write_all(b"GET /h/test/p.gif HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        assert!(response.ends_with(GIF_1X1));

        drop(TcpStream::connect(("127.0.0.1", canary.port())).unwrap());
        let mut tls = TcpStream::connect(("127.0.0.1", canary.port())).unwrap();
        tls.write_all(&[0x16, 0x03, 0x01, 0x00, 0x10]).unwrap();
        drop(tls);

        let log = wait_for(&canary, |log| {
            log.connections == 3 && log.silent_connections == 1 && log.tls_hellos == 1
        });
        assert_eq!(log.connections, 3);
        assert_eq!(
            log.hits,
            vec![Hit {
                method: "GET".into(),
                path: "/h/test/p.gif".into()
            }]
        );
        assert_eq!(log.silent_connections, 1);
        assert_eq!(log.tls_hellos, 1);
        assert!(log.since(&log).is_empty());
    }
}
