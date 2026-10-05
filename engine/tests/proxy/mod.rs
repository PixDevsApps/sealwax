//! S1's fault-injection proxy, for the testbed suites. It terminates TLS with the testbed's
//! server certificate, opens its own TLS connection to the server (trusting the testbed CA) and
//! relays plaintext, so a rule can match IMAP text. A cut drops both TCP connections without a
//! TLS close_notify, like a dead network.

#![allow(dead_code)]

use rustls::{ClientConfig, ServerConfig};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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
}

pub struct Proxy {
    pub port: u16,
    state: Arc<Mutex<State>>,
    fired: Arc<AtomicUsize>,
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
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let connector = TlsConnector::from(client);
        let upstream: SocketAddr = format!("127.0.0.1:{upstream}").parse().unwrap();
        {
            let (state, fired) = (state.clone(), fired.clone());
            tokio::spawn(async move {
                let mut ids = 0u64;
                while let Ok((socket, _)) = listener.accept().await {
                    ids += 1;
                    let (acceptor, connector, state, fired) = (
                        acceptor.clone(),
                        connector.clone(),
                        state.clone(),
                        fired.clone(),
                    );
                    tokio::spawn(relay(
                        ids, socket, upstream, acceptor, connector, state, fired,
                    ));
                }
            });
        }
        Proxy { port, state, fired }
    }

    /// Arms a one-shot rule for the next matching traffic.
    pub fn arm(&self, rule: Rule) {
        self.state.lock().unwrap().armed = Some((rule, None, Phase::Counting(0)));
    }

    /// Cuts made so far.
    pub fn fired(&self) -> usize {
        self.fired.load(Ordering::SeqCst)
    }
}

async fn relay(
    id: u64,
    socket: TcpStream,
    upstream: SocketAddr,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
    state: Arc<Mutex<State>>,
    fired: Arc<AtomicUsize>,
) {
    let _ = socket.set_nodelay(true);
    let Ok(client) = acceptor.accept(socket).await else {
        return;
    };
    let Ok(tcp) = TcpStream::connect(upstream).await else {
        return;
    };
    let _ = tcp.set_nodelay(true);
    let Ok(server) = connector
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
    else {
        return;
    };
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let to_server = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = client_read.read(&mut buf).await?;
            if n == 0 {
                return std::io::Result::Ok(());
            }
            server_write.write_all(&buf[..n]).await?;
            server_write.flush().await?;
        }
    };
    let to_client = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = server_read.read(&mut buf).await?;
            if n == 0 {
                return std::io::Result::Ok(false);
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
        _ = to_server => false,
        result = to_client => result.unwrap_or(false),
    };
    if cut {
        fired.fetch_add(1, Ordering::SeqCst);
    }
}
