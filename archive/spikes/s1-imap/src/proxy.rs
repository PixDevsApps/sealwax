//! Fault-injection proxy. It terminates TLS with the testbed certificate, opens its own TLS
//! connection to the server and relays plaintext, so cut rules can match IMAP content. A cut
//! drops both TCP connections without a TLS close_notify, like a dead network.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use rustls::{ClientConfig, ServerConfig};
use rustls_pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Clone, Debug)]
pub enum Rule {
    /// After the `nth` occurrence of `needle` in server-to-client data, pass `extra` more bytes,
    /// then cut (mid-FETCH).
    ServerAfter {
        needle: Vec<u8>,
        nth: usize,
        extra: usize,
    },
    /// After the `nth` occurrence of `needle` in client-to-server data, pass `extra` more bytes,
    /// then cut (mid-APPEND literal).
    ClientAfter {
        needle: Vec<u8>,
        nth: usize,
        extra: usize,
    },
    /// Once the client has sent `needle`, pass continuation lines (`+`) and cut on the first
    /// other server data: the server has acted, the client never sees the result.
    DropReplyAfterClient { needle: Vec<u8> },
}

#[derive(Debug)]
enum Phase {
    Counting { seen: usize },
    Passing { left: usize },
    Armed,
}

#[derive(Debug)]
struct Armed {
    rule: Rule,
    connection: Option<u64>,
    phase: Phase,
}

#[derive(Default)]
struct State {
    armed: Option<Armed>,
    connections: Vec<(u64, Arc<Notify>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    ToServer,
    ToClient,
}

/// What to do with a chunk: forward the first `n` bytes and maybe cut afterwards.
struct Verdict {
    forward: usize,
    cut: bool,
}

pub struct Proxy {
    pub addr: SocketAddr,
    /// One-way delay added to every relayed byte (pipelined, so throughput is unaffected).
    pub one_way: Duration,
    state: Arc<Mutex<State>>,
    fired: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    to_client: Arc<AtomicU64>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

impl State {
    fn judge(&mut self, connection: u64, direction: Direction, chunk: &[u8]) -> Verdict {
        let pass = Verdict {
            forward: chunk.len(),
            cut: false,
        };
        let Some(armed) = self.armed.as_mut() else {
            return pass;
        };
        if armed.connection.is_some_and(|c| c != connection) {
            return pass;
        }
        match armed.rule.clone() {
            Rule::ServerAfter { needle, nth, extra } | Rule::ClientAfter { needle, nth, extra } => {
                let wanted = if matches!(armed.rule, Rule::ServerAfter { .. }) {
                    Direction::ToClient
                } else {
                    Direction::ToServer
                };
                if direction != wanted {
                    return pass;
                }
                let mut offset = 0;
                loop {
                    match armed.phase {
                        Phase::Counting { seen } => match find(chunk, &needle, offset) {
                            Some(at) => {
                                armed.connection = Some(connection);
                                offset = at + needle.len();
                                armed.phase = if seen + 1 >= nth {
                                    Phase::Passing { left: extra }
                                } else {
                                    Phase::Counting { seen: seen + 1 }
                                };
                            }
                            None => return pass,
                        },
                        Phase::Passing { left } => {
                            let available = chunk.len() - offset;
                            if available >= left {
                                self.armed = None;
                                return Verdict {
                                    forward: offset + left,
                                    cut: true,
                                };
                            }
                            armed.phase = Phase::Passing {
                                left: left - available,
                            };
                            return pass;
                        }
                        Phase::Armed => return pass,
                    }
                }
            }
            Rule::DropReplyAfterClient { needle } => match (direction, &armed.phase) {
                (Direction::ToServer, Phase::Counting { .. }) => {
                    if find(chunk, &needle, 0).is_some() {
                        armed.connection = Some(connection);
                        armed.phase = Phase::Armed;
                    }
                    pass
                }
                (Direction::ToClient, Phase::Armed) if !chunk.starts_with(b"+") => {
                    self.armed = None;
                    Verdict {
                        forward: 0,
                        cut: true,
                    }
                }
                _ => pass,
            },
        }
    }
}

impl Proxy {
    /// Listens on 127.0.0.1 and relays every connection to `upstream` (TLS on both sides).
    pub async fn start(
        upstream: SocketAddr,
        server: Arc<ServerConfig>,
        client: Arc<ClientConfig>,
    ) -> Result<Proxy> {
        Self::start_with_latency(upstream, server, client, Duration::ZERO).await
    }

    /// Like `start`, adding `one_way` delay in each direction (RTT = 2 x `one_way`) to the
    /// relayed IMAP traffic; the TCP and TLS handshakes themselves are not delayed. Cut rules
    /// are ignored in this mode.
    pub async fn start_with_latency(
        upstream: SocketAddr,
        server: Arc<ServerConfig>,
        client: Arc<ClientConfig>,
        one_way: Duration,
    ) -> Result<Proxy> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let fired = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let to_client = Arc::new(AtomicU64::new(0));
        let ids = Arc::new(AtomicU64::new(0));
        let acceptor = TlsAcceptor::from(server);
        let connector = TlsConnector::from(client);
        {
            let (state, fired, accepted, counter) = (
                state.clone(),
                fired.clone(),
                accepted.clone(),
                to_client.clone(),
            );
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let id = ids.fetch_add(1, Ordering::SeqCst);
                    let kill = Arc::new(Notify::new());
                    state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .connections
                        .push((id, kill.clone()));
                    let (acceptor, connector, state, fired, counter) = (
                        acceptor.clone(),
                        connector.clone(),
                        state.clone(),
                        fired.clone(),
                        counter.clone(),
                    );
                    tokio::spawn(async move {
                        let link = Link {
                            fired,
                            kill,
                            one_way,
                            to_client: counter,
                        };
                        let _ = relay(
                            id,
                            socket,
                            upstream,
                            acceptor,
                            connector,
                            state.clone(),
                            link,
                        )
                        .await;
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .connections
                            .retain(|(c, _)| *c != id);
                    });
                }
            });
        }
        Ok(Proxy {
            addr,
            one_way,
            state,
            fired,
            accepted,
            to_client,
        })
    }

    /// Arms a one-shot rule for the next matching traffic.
    pub fn arm(&self, rule: Rule) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).armed = Some(Armed {
            rule,
            connection: None,
            phase: Phase::Counting { seen: 0 },
        });
    }

    pub fn disarm(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).armed = None;
    }

    /// How many cuts have happened (rules fired plus `cut_all` calls that hit a connection).
    pub fn fired(&self) -> usize {
        self.fired.load(Ordering::SeqCst)
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Plaintext IMAP bytes relayed from the server to clients so far.
    pub fn bytes_to_client(&self) -> u64 {
        self.to_client.load(Ordering::SeqCst)
    }

    /// Cuts every open connection now.
    pub fn cut_all(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for (_, kill) in &state.connections {
            kill.notify_one();
        }
        let n = state.connections.len();
        self.fired.fetch_add(n, Ordering::SeqCst);
        n
    }
}

struct Link {
    fired: Arc<AtomicUsize>,
    kill: Arc<Notify>,
    one_way: Duration,
    to_client: Arc<AtomicU64>,
}

async fn relay(
    id: u64,
    socket: TcpStream,
    upstream: SocketAddr,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
    state: Arc<Mutex<State>>,
    link: Link,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let client = acceptor.accept(socket).await?;
    let upstream = TcpStream::connect(upstream).await?;
    upstream.set_nodelay(true)?;
    let server = connector
        .connect(ServerName::try_from("localhost")?, upstream)
        .await?;
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let Link {
        fired,
        kill,
        one_way,
        to_client: counter,
    } = link;
    if !one_way.is_zero() {
        tokio::select! {
            _ = delayed(&mut client_read, &mut server_write, one_way, None) => {}
            _ = delayed(&mut server_read, &mut client_write, one_way, Some(counter)) => {}
            _ = kill.notified() => {}
        }
        return Ok(());
    }

    let to_server = {
        let state = state.clone();
        async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = client_read.read(&mut buf).await?;
                if n == 0 {
                    return anyhow::Ok(false);
                }
                let verdict = state.lock().unwrap_or_else(|e| e.into_inner()).judge(
                    id,
                    Direction::ToServer,
                    &buf[..n],
                );
                server_write.write_all(&buf[..verdict.forward]).await?;
                server_write.flush().await?;
                if verdict.cut {
                    return Ok(true);
                }
            }
        }
    };
    let to_client = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = server_read.read(&mut buf).await?;
            if n == 0 {
                return anyhow::Ok(false);
            }
            let verdict = state.lock().unwrap_or_else(|e| e.into_inner()).judge(
                id,
                Direction::ToClient,
                &buf[..n],
            );
            counter.fetch_add(verdict.forward as u64, Ordering::SeqCst);
            client_write.write_all(&buf[..verdict.forward]).await?;
            client_write.flush().await?;
            if verdict.cut {
                return Ok(true);
            }
        }
    };
    let rule_fired = tokio::select! {
        result = to_server => result.unwrap_or(false),
        result = to_client => result.unwrap_or(false),
        _ = kill.notified() => false,
    };
    if rule_fired {
        fired.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
}

/// Relays with a fixed one-way delay: chunks are stamped when read and written once the delay
/// has passed, so several chunks can be in flight like on a real link.
async fn delayed<R, W>(
    read: &mut R,
    write: &mut W,
    one_way: Duration,
    counter: Option<Arc<AtomicU64>>,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
    let reader = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = read.read(&mut buf).await?;
            if let Some(counter) = &counter {
                counter.fetch_add(n as u64, Ordering::SeqCst);
            }
            if n == 0
                || tx
                    .send((tokio::time::Instant::now() + one_way, buf[..n].to_vec()))
                    .is_err()
            {
                return anyhow::Ok(());
            }
        }
    };
    let writer = async move {
        while let Some((at, data)) = rx.recv().await {
            tokio::time::sleep_until(at).await;
            write.write_all(&data).await?;
            write.flush().await?;
        }
        anyhow::Ok(())
    };
    let (a, b) = tokio::join!(reader, writer);
    a.and(b)
}
