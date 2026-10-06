//! S1's fault-injection proxy, for the testbed suites. It terminates TLS with the testbed's
//! server certificate, opens its own TLS connection to the server (trusting the testbed CA) and
//! relays plaintext, so a rule can match IMAP text. A cut drops both TCP connections without a
//! TLS close_notify, like a dead network. A black hole (M3) is a network that stopped answering:
//! every open connection goes silent for good, and connections made meanwhile are never
//! answered, as after a suspend or on a lost route; lifted, new connections work again. For the
//! op queue's writes (M4), rules on what the client sends: the reply to a command lost (or held,
//! the connection left silent) after the server acted on it, or the connection cut as the
//! command goes out (S1); and a record of every line the clients sent.

#![allow(dead_code)]

use rustls::{ClientConfig, ServerConfig};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// After the `nth` occurrence of `needle` in the server's data, pass `extra` more bytes, then
/// cut: a connection lost in the middle of a FETCH.
#[derive(Clone, Debug)]
pub struct Rule {
    pub needle: Vec<u8>,
    pub nth: usize,
    pub extra: usize,
}

#[derive(Debug)]
enum Phase {
    Counting(usize),
    Passing(usize),
}

#[derive(Default)]
struct State {
    armed: Option<(Rule, Option<u64>, Phase)>,
    sent: Option<Sent>,
    /// Every line the clients sent, in order, when recording.
    recorded: Option<Vec<String>>,
    /// The tail of a client's data that didn't end a line yet, per connection.
    partial: std::collections::HashMap<u64, Vec<u8>>,
}

/// What the server's reply to a watched command meets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Pass,
    /// Dropped, and the connection cut.
    Lose,
    /// Dropped, and the connection silent from then on, both ways.
    Hold,
}

/// A one-shot rule on a command the client sends.
struct Sent {
    needle: Vec<u8>,
    /// Cut (or freeze, with `hold`) as the command goes out, before the server sees it;
    /// otherwise let it through and lose (or hold) the server's reply.
    before: bool,
    hold: bool,
    /// The connection that sent it.
    owner: Option<u64>,
}

/// What happens to a chunk the client sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Judged {
    Forward,
    Cut,
    /// Dropped, and the connection silent from then on, both ways.
    Freeze,
}

impl State {
    /// Whether to forward a client chunk, cut the connection or freeze it.
    fn judge_sent(&mut self, connection: u64, chunk: &[u8]) -> Judged {
        let Some(sent) = self.sent.as_mut() else {
            return Judged::Forward;
        };
        if sent.owner.is_some() || find(chunk, &sent.needle, 0).is_none() {
            return Judged::Forward;
        }
        if sent.before {
            let hold = sent.hold;
            self.sent = None;
            return if hold { Judged::Freeze } else { Judged::Cut };
        }
        sent.owner = Some(connection);
        Judged::Forward
    }

    /// Whether a server chunk is the reply to lose: anything but a continuation request, once
    /// the command went out on this connection.
    fn fate(&mut self, connection: u64, chunk: &[u8]) -> Fate {
        let Some(sent) = self.sent.as_ref() else {
            return Fate::Pass;
        };
        if sent.owner != Some(connection) || chunk.starts_with(b"+") {
            return Fate::Pass;
        }
        let fate = if sent.hold { Fate::Hold } else { Fate::Lose };
        self.sent = None;
        fate
    }

    fn record(&mut self, connection: u64, chunk: &[u8]) {
        let Some(recorded) = self.recorded.as_mut() else {
            return;
        };
        let partial = self.partial.entry(connection).or_default();
        partial.extend_from_slice(chunk);
        while let Some(end) = find(partial, b"\r\n", 0) {
            let line: Vec<u8> = partial.drain(..end + 2).collect();
            recorded.push(String::from_utf8_lossy(&line[..end]).into_owned());
        }
    }
}

/// Faults on every connection: cuts and black holes so far, and whether one is in place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Faults {
    cuts: u64,
    holes: u64,
    holed: bool,
}

pub struct Proxy {
    pub port: u16,
    state: Arc<Mutex<State>>,
    fired: Arc<AtomicUsize>,
    faults: watch::Sender<Faults>,
    accepted: Arc<AtomicUsize>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

impl State {
    /// How many bytes of a server chunk to forward, and whether to cut after them.
    fn judge(&mut self, connection: u64, chunk: &[u8]) -> (usize, bool) {
        let Some((rule, owner, phase)) = self.armed.as_mut() else {
            return (chunk.len(), false);
        };
        if owner.is_some_and(|owner| owner != connection) {
            return (chunk.len(), false);
        }
        let mut offset = 0;
        loop {
            match *phase {
                Phase::Counting(seen) => match find(chunk, &rule.needle, offset) {
                    Some(at) => {
                        *owner = Some(connection);
                        offset = at + rule.needle.len();
                        *phase = if seen + 1 >= rule.nth {
                            Phase::Passing(rule.extra)
                        } else {
                            Phase::Counting(seen + 1)
                        };
                    }
                    None => return (chunk.len(), false),
                },
                Phase::Passing(left) => {
                    let available = chunk.len() - offset;
                    if available >= left {
                        self.armed = None;
                        return (offset + left, true);
                    }
                    *phase = Phase::Passing(left - available);
                    return (chunk.len(), false);
                }
            }
        }
    }
}

impl Proxy {
    /// Listens on 127.0.0.1 and relays every connection to the TLS port `upstream`.
    pub async fn start(upstream: u16, ca: &Path) -> Proxy {
        let certs = ca.with_file_name("server.pem");
        let key = ca.with_file_name("server.key");
        let chain = CertificateDer::pem_file_iter(&certs)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(chain, PrivateKeyDer::from_pem_file(&key).unwrap())
                .unwrap();
        let client: Arc<ClientConfig> = sealwax_engine::net::Trust::Roots(ca.to_owned())
            .client_config()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let fired = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let (faults, _) = watch::channel(Faults::default());
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let connector = TlsConnector::from(client);
        let upstream: SocketAddr = format!("127.0.0.1:{upstream}").parse().unwrap();
        {
            let (state, fired, accepted, faults) = (
                state.clone(),
                fired.clone(),
                accepted.clone(),
                faults.clone(),
            );
            tokio::spawn(async move {
                let mut ids = 0u64;
                while let Ok((socket, _)) = listener.accept().await {
                    ids += 1;
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let relay = Relay {
                        id: ids,
                        upstream,
                        acceptor: acceptor.clone(),
                        connector: connector.clone(),
                        state: state.clone(),
                        fired: fired.clone(),
                        faults: faults.subscribe(),
                    };
                    tokio::spawn(relay.run(socket));
                }
            });
        }
        Proxy {
            port,
            state,
            fired,
            faults,
            accepted,
        }
    }

    /// Arms a one-shot rule for the next matching traffic.
    pub fn arm(&self, rule: Rule) {
        self.state.lock().unwrap().armed = Some((rule, None, Phase::Counting(0)));
    }

    /// Once the client sends `needle`, the server's next data other than a continuation request
    /// is dropped and the connection cut: the server acted, and the client never hears how.
    pub fn lose_reply(&self, needle: &str) {
        self.state.lock().unwrap().sent = Some(Sent {
            needle: needle.as_bytes().to_vec(),
            before: false,
            hold: false,
            owner: None,
        });
    }

    /// Once the client sends `needle`, the server's next data other than a continuation request
    /// is dropped and the connection goes silent both ways, left open: the server acted, and
    /// the client waits for an answer that never comes.
    pub fn hold_reply(&self, needle: &str) {
        self.state.lock().unwrap().sent = Some(Sent {
            needle: needle.as_bytes().to_vec(),
            before: false,
            hold: true,
            owner: None,
        });
    }

    /// As the client sends `needle`, that data is dropped and the connection goes silent both
    /// ways, left open: the client is stuck part of the way through a command or its data
    /// (M6: an engine stopped while the data goes out).
    pub fn freeze_on_send(&self, needle: &str) {
        self.state.lock().unwrap().sent = Some(Sent {
            needle: needle.as_bytes().to_vec(),
            before: true,
            hold: true,
            owner: None,
        });
    }

    /// The connection is cut as the client sends `needle`, before the server sees it.
    pub fn cut_on_send(&self, needle: &str) {
        self.state.lock().unwrap().sent = Some(Sent {
            needle: needle.as_bytes().to_vec(),
            before: true,
            hold: false,
            owner: None,
        });
    }

    /// Starts keeping every line the clients send (test credentials included: never printed).
    pub fn record(&self) {
        self.state
            .lock()
            .unwrap()
            .recorded
            .get_or_insert_with(Vec::new);
    }

    /// The lines the clients sent since `record`, without their tags.
    pub fn sent_lines(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        state
            .recorded
            .iter()
            .flatten()
            .map(|line| {
                line.split_once(' ')
                    .map_or(line.as_str(), |(_, rest)| rest)
                    .to_owned()
            })
            .collect()
    }

    /// The lines the clients sent since `record`, whole (SMTP has no tags; M6 counts a message's
    /// data by its header lines).
    pub fn sent_raw_lines(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        state.recorded.iter().flatten().cloned().collect()
    }

    /// Whether a rule on what the client sends is still waiting.
    pub fn sent_armed(&self) -> bool {
        self.state.lock().unwrap().sent.is_some()
    }

    /// Drops a rule that never fired.
    pub fn disarm(&self) {
        let mut state = self.state.lock().unwrap();
        state.sent = None;
        state.armed = None;
    }

    /// Cuts made so far, by rules and by `cut_all`.
    pub fn fired(&self) -> usize {
        self.fired.load(Ordering::SeqCst)
    }

    /// Connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Drops every open connection now, without a close_notify.
    pub fn cut_all(&self) {
        self.faults.send_modify(|faults| faults.cuts += 1);
    }

    /// `true`: every open connection goes silent for good, and new ones are never answered;
    /// `false`: new connections work again.
    pub fn black_hole(&self, on: bool) {
        self.faults.send_modify(|faults| {
            if on && !faults.holed {
                faults.holes += 1;
            }
            faults.holed = on;
        });
    }
}

/// What ended a relayed connection.
enum End {
    Closed,
    Cut,
}

struct Relay {
    id: u64,
    upstream: SocketAddr,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
    state: Arc<Mutex<State>>,
    fired: Arc<AtomicUsize>,
    faults: watch::Receiver<Faults>,
}

impl Relay {
    async fn run(mut self, socket: TcpStream) {
        let born = *self.faults.borrow_and_update();
        if born.holed {
            silence(socket).await;
            return;
        }
        let frozen = Arc::new(AtomicBool::new(false));
        let relayed = relay(
            self.id,
            socket,
            self.upstream,
            self.acceptor,
            self.connector,
            self.state,
            frozen.clone(),
        );
        let mut faults = self.faults;
        let faulted = async move {
            loop {
                if faults.changed().await.is_err() {
                    return std::future::pending::<End>().await;
                }
                let now = *faults.borrow_and_update();
                if now.cuts > born.cuts {
                    return End::Cut;
                }
                if now.holes > born.holes {
                    frozen.store(true, Ordering::SeqCst);
                }
            }
        };
        let end = tokio::select! {
            end = relayed => end,
            end = faulted => end,
        };
        if matches!(end, End::Cut) {
            self.fired.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A connection nobody answers: what the client sends is read and dropped until it gives up.
async fn silence(mut socket: TcpStream) {
    let mut buf = vec![0u8; 16 * 1024];
    while let Ok(n) = socket.read(&mut buf).await {
        if n == 0 {
            return;
        }
    }
}

/// Relays one connection until either side closes or a rule cuts it. Frozen, nothing more is
/// forwarded either way and the client's side stays open, as on a dead route.
async fn relay(
    id: u64,
    socket: TcpStream,
    upstream: SocketAddr,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
    state: Arc<Mutex<State>>,
    frozen: Arc<AtomicBool>,
) -> End {
    let _ = socket.set_nodelay(true);
    let Ok(client) = acceptor.accept(socket).await else {
        return End::Closed;
    };
    let Ok(tcp) = TcpStream::connect(upstream).await else {
        return End::Closed;
    };
    let _ = tcp.set_nodelay(true);
    let Ok(server) = connector
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
    else {
        return End::Closed;
    };
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let inbound = frozen.clone();
    let sending = state.clone();
    let to_server = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = client_read.read(&mut buf).await?;
            if n == 0 {
                return std::io::Result::Ok(false);
            }
            if inbound.load(Ordering::SeqCst) {
                continue;
            }
            let judged = {
                let mut state = sending.lock().unwrap();
                state.record(id, &buf[..n]);
                state.judge_sent(id, &buf[..n])
            };
            match judged {
                Judged::Forward => {}
                Judged::Cut => return Ok(true),
                Judged::Freeze => {
                    inbound.store(true, Ordering::SeqCst);
                    continue;
                }
            }
            server_write.write_all(&buf[..n]).await?;
            server_write.flush().await?;
        }
    };
    let to_client = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = server_read.read(&mut buf).await?;
            if frozen.load(Ordering::SeqCst) {
                if n == 0 {
                    std::future::pending::<()>().await;
                }
                continue;
            }
            if n == 0 {
                return std::io::Result::Ok(false);
            }
            match state.lock().unwrap().fate(id, &buf[..n]) {
                Fate::Pass => {}
                Fate::Lose => return Ok(true),
                Fate::Hold => {
                    frozen.store(true, Ordering::SeqCst);
                    continue;
                }
            }
            let (forward, cut) = state.lock().unwrap().judge(id, &buf[..n]);
            client_write.write_all(&buf[..forward]).await?;
            client_write.flush().await?;
            if cut {
                return Ok(true);
            }
        }
    };
    let cut = tokio::select! {
        result = to_server => result.unwrap_or(false),
        result = to_client => result.unwrap_or(false),
    };
    if cut { End::Cut } else { End::Closed }
}
