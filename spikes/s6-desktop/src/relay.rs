//! A plain TCP relay in front of a testbed server, so a run can break the IMAP connection on
//! purpose. TLS passes through untouched (the client still verifies the server). `cut_all`
//! closes every relayed connection at once, as a dead link the peer notices; `freeze_all` keeps
//! the sockets open but forwards nothing more on them, as after a suspend on a real network,
//! where nothing tells the client that its connection is gone. New connections always relay.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

struct Link {
    id: u64,
    kill: Arc<Notify>,
    frozen: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    links: Vec<Link>,
}

pub struct Relay {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accepted: Arc<AtomicUsize>,
}

impl Relay {
    pub async fn start(upstream: SocketAddr) -> std::io::Result<Relay> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let ids = Arc::new(AtomicU64::new(0));
        {
            let state = state.clone();
            let accepted = accepted.clone();
            tokio::spawn(async move {
                while let Ok((client, _)) = listener.accept().await {
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let id = ids.fetch_add(1, Ordering::SeqCst);
                    let kill = Arc::new(Notify::new());
                    let frozen = Arc::new(AtomicBool::new(false));
                    lock(&state).links.push(Link {
                        id,
                        kill: kill.clone(),
                        frozen: frozen.clone(),
                    });
                    let state = state.clone();
                    tokio::spawn(async move {
                        let _ = relay(client, upstream, kill, frozen).await;
                        lock(&state).links.retain(|link| link.id != id);
                    });
                }
            });
        }
        Ok(Relay {
            addr,
            state,
            accepted,
        })
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn open_links(&self) -> usize {
        lock(&self.state).links.len()
    }

    /// Closes every open connection now; returns how many there were.
    pub fn cut_all(&self) -> usize {
        let state = lock(&self.state);
        for link in &state.links {
            link.kill.notify_one();
        }
        state.links.len()
    }

    /// Stops forwarding on every open connection; their sockets stay open.
    pub fn freeze_all(&self) -> usize {
        let state = lock(&self.state);
        for link in &state.links {
            link.frozen.store(true, Ordering::SeqCst);
        }
        state.links.len()
    }
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

async fn relay(
    client: TcpStream,
    upstream: SocketAddr,
    kill: Arc<Notify>,
    frozen: Arc<AtomicBool>,
) -> std::io::Result<()> {
    client.set_nodelay(true)?;
    let server = TcpStream::connect(upstream).await?;
    server.set_nodelay(true)?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let up = pump(&mut client_read, &mut server_write, &frozen);
    let down = pump(&mut server_read, &mut client_write, &frozen);
    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = kill.notified() => {}
    }
    Ok(())
}

/// Copies until EOF; once frozen, reads and discards, so the far side sees nothing more.
async fn pump<R, W>(read: &mut R, write: &mut W, frozen: &AtomicBool) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = read.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        if frozen.load(Ordering::SeqCst) {
            continue;
        }
        write.write_all(&buf[..n]).await?;
        write.flush().await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = socket.read(&mut buf).await {
                        if n == 0 || socket.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    async fn round_trip(stream: &mut TcpStream, text: &[u8]) -> Option<Vec<u8>> {
        stream.write_all(text).await.ok()?;
        let mut buf = vec![0u8; text.len()];
        tokio::time::timeout(Duration::from_millis(300), stream.read_exact(&mut buf))
            .await
            .ok()?
            .ok()?;
        Some(buf)
    }

    #[tokio::test]
    async fn relays_cuts_and_freezes() {
        let relay = Relay::start(echo_server().await).await.unwrap();
        let mut first = TcpStream::connect(relay.addr).await.unwrap();
        assert_eq!(
            round_trip(&mut first, b"hei").await.as_deref(),
            Some(&b"hei"[..])
        );
        assert_eq!(relay.freeze_all(), 1);
        assert_eq!(round_trip(&mut first, b"lost").await, None);
        let mut second = TcpStream::connect(relay.addr).await.unwrap();
        assert_eq!(
            round_trip(&mut second, b"new").await.as_deref(),
            Some(&b"new"[..])
        );
        assert_eq!(relay.cut_all(), 2);
        let mut buf = [0u8; 8];
        let read = tokio::time::timeout(Duration::from_secs(1), second.read(&mut buf))
            .await
            .unwrap();
        assert!(matches!(read, Ok(0) | Err(_)));
        assert_eq!(relay.accepted(), 2);
    }
}
