//! TCP and TLS for the engine's connections. Certificates are verified with rustls and the
//! platform verifier; a testbed CA is trusted through its own constructor that trusts exactly
//! that CA, never by weakening verification (SECURITY.md, Transport). A server on this computer
//! can instead be trusted with one pinned certificate ([`pinned`]; issue #9, Proton Mail Bridge).

use futures_util::stream::{FuturesUnordered, StreamExt};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use rustls_platform_verifier::BuilderVerifierExt;
use sealwax_domain::account::{Endpoint, LoginFailure};
use socket2::{SockRef, TcpKeepalive};
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

mod pinned;
#[cfg(test)]
pub(crate) mod testing;

pub use pinned::{fingerprint, read_certificate};
pub(crate) use pinned::{loopback_peer, pinned_config};

/// Idle time before the first keepalive probe, and the time between probes.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_RETRIES: u32 = 4;

/// How long a connection attempt to one of a host's addresses goes on alone before the next
/// starts beside it (RFC 8305's Connection Attempt Delay).
const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Which certificates a connection trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The system trust store through rustls-platform-verifier: the only choice of release
    /// builds.
    Platform,
    /// Exactly the certificates in this PEM file and nothing else (the testbed CA).
    Roots(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("the CA file could not be read")]
    Read(#[source] io::Error),
    #[error("the CA file holds no usable certificate")]
    Certificates,
    #[error("TLS setup: {0}")]
    Tls(#[from] rustls::Error),
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl Trust {
    /// The rustls configuration of IMAP connections.
    pub fn client_config(&self) -> Result<Arc<ClientConfig>, TrustError> {
        let builder = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?;
        let config = match self.root_pem()? {
            None => builder.with_platform_verifier()?.with_no_client_auth(),
            Some(pem) => builder
                .with_root_certificates(roots(&pem)?)
                .with_no_client_auth(),
        };
        Ok(Arc::new(config))
    }

    /// The PEM text of `Roots` (also for lettre's own TLS setup); `None` for `Platform`.
    pub(crate) fn root_pem(&self) -> Result<Option<Vec<u8>>, TrustError> {
        match self {
            Trust::Platform => Ok(None),
            Trust::Roots(path) => std::fs::read(path).map(Some).map_err(TrustError::Read),
        }
    }
}

fn roots(pem: &[u8]) -> Result<RootCertStore, TrustError> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem) {
        roots
            .add(cert.map_err(|_| TrustError::Certificates)?)
            .map_err(|_| TrustError::Certificates)?;
    }
    if roots.is_empty() {
        return Err(TrustError::Certificates);
    }
    Ok(roots)
}

/// Why a connection couldn't be made. Displays never include server text or names.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("the host name did not resolve")]
    Resolve(#[source] io::Error),
    #[error("the host refused or could not be reached")]
    Connect(#[source] io::Error),
    #[error("the connection timed out")]
    Timeout,
    #[error("the TLS handshake failed")]
    Tls(#[source] io::Error),
    #[error("the host name is not valid for TLS")]
    ServerName,
    /// A server with a pinned certificate answered from an address that isn't this computer's.
    #[error("the pinned server is not on this computer")]
    NotLoopback,
}

impl ConnectError {
    pub fn failure(&self) -> LoginFailure {
        match self {
            ConnectError::Resolve(_) | ConnectError::Connect(_) => LoginFailure::Unreachable,
            ConnectError::Timeout => LoginFailure::Timeout,
            ConnectError::Tls(error) => tls_failure(error),
            ConnectError::ServerName => LoginFailure::Other,
            ConnectError::NotLoopback => LoginFailure::Certificate,
        }
    }
}

/// A failed handshake: a certificate that didn't verify, or no TLS at all.
pub(crate) fn tls_failure(error: &io::Error) -> LoginFailure {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        Some(rustls::Error::InvalidCertificate(_)) => LoginFailure::Certificate,
        _ if error.kind() == io::ErrorKind::TimedOut => LoginFailure::Timeout,
        _ => LoginFailure::NoTls,
    }
}

/// Resolves `host` and connects to the first address that answers, within `timeout`, with TCP
/// keepalive on and Nagle off.
pub(crate) async fn tcp(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<TcpStream, ConnectError> {
    tokio::time::timeout(timeout, async {
        let addresses: Vec<_> = tokio::net::lookup_host((host, port))
            .await
            .map_err(ConnectError::Resolve)?
            .collect();
        let stream = first_to_answer(addresses)
            .await
            .map_err(ConnectError::Connect)?;
        keepalive(&stream).map_err(ConnectError::Connect)?;
        stream.set_nodelay(true).map_err(ConnectError::Connect)?;
        Ok(stream)
    })
    .await
    .map_err(|_| ConnectError::Timeout)?
}

/// Connects to `addresses` in a staggered race (RFC 8305, Happy Eyeballs): the address families
/// take turns, and the next address is tried as soon as an attempt fails, or after
/// [`ATTEMPT_DELAY`] while the earlier attempts go on. An address that never answers, as through
/// a broken IPv6 route, then holds up the others by a quarter second instead of the whole
/// timeout. The first connection made wins and the other attempts are dropped.
async fn first_to_answer(addresses: Vec<SocketAddr>) -> io::Result<TcpStream> {
    let mut queue = interleaved(addresses);
    let mut attempts = FuturesUnordered::new();
    let mut last = None;
    loop {
        if attempts.is_empty() {
            let Some(address) = queue.pop_front() else {
                return Err(
                    last.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))
                );
            };
            attempts.push(TcpStream::connect(address));
        }
        tokio::select! {
            Some(result) = attempts.next() => match result {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    last = Some(error);
                    attempts.extend(queue.pop_front().map(TcpStream::connect));
                }
            },
            () = tokio::time::sleep(ATTEMPT_DELAY), if !queue.is_empty() => {
                attempts.extend(queue.pop_front().map(TcpStream::connect));
            }
        }
    }
}

/// `addresses` in the resolver's order (RFC 6724), the two families taking turns from the
/// first address's (RFC 8305, section 4).
fn interleaved(addresses: Vec<SocketAddr>) -> VecDeque<SocketAddr> {
    let first_v6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    let (mut first, mut second): (VecDeque<_>, VecDeque<_>) = addresses
        .into_iter()
        .partition(|address| address.is_ipv6() == first_v6);
    let mut order = VecDeque::with_capacity(first.len() + second.len());
    while !first.is_empty() || !second.is_empty() {
        order.extend(first.pop_front());
        order.extend(second.pop_front());
    }
    order
}

fn keepalive(stream: &TcpStream) -> io::Result<()> {
    let keepalive = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    SockRef::from(stream).set_tcp_keepalive(&keepalive)
}

/// The TLS handshake on `tcp`, verifying the certificate against the endpoint's host with
/// `config`; a server with a pinned certificate ([`Endpoint::pinned`]) must be on this computer
/// and show exactly that certificate.
pub(crate) async fn tls(
    tcp: TcpStream,
    config: Arc<ClientConfig>,
    endpoint: &Endpoint,
    timeout: Duration,
) -> Result<TlsStream<TcpStream>, ConnectError> {
    let config = match endpoint.pinned() {
        Some(pin) => {
            loopback_peer(&tcp)?;
            pinned_config(pin)?
        }
        None => config,
    };
    let name = ServerName::try_from(endpoint.host.clone()).map_err(|_| ConnectError::ServerName)?;
    tokio::time::timeout(timeout, TlsConnector::from(config).connect(name, tcp))
        .await
        .map_err(|_| ConnectError::Timeout)?
        .map_err(ConnectError::Tls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_verifier_builds() {
        Trust::Platform.client_config().unwrap();
    }

    #[test]
    fn roots_need_a_certificate() {
        assert!(matches!(roots(b""), Err(TrustError::Certificates)));
        let missing = Trust::Roots("/nonexistent/ca.pem".into());
        assert!(matches!(missing.client_config(), Err(TrustError::Read(_))));
    }

    #[test]
    fn handshake_errors_are_classified() {
        let certificate = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
        );
        assert_eq!(tls_failure(&certificate), LoginFailure::Certificate);
        let plaintext = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidMessage(rustls::InvalidMessage::InvalidContentType),
        );
        assert_eq!(tls_failure(&plaintext), LoginFailure::NoTls);
        assert_eq!(
            ConnectError::Resolve(io::Error::other("x")).failure(),
            LoginFailure::Unreachable
        );
        assert_eq!(ConnectError::Timeout.failure(), LoginFailure::Timeout);
    }

    /// A listener whose accept queue is full and never drained: the kernel drops new SYNs, so a
    /// connect to it hangs as one through a route that drops packets does.
    struct Unanswering {
        listener: tokio::net::TcpListener,
        _queued: Vec<TcpStream>,
    }

    async fn unanswering() -> Unanswering {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(0).unwrap();
        let address = listener.local_addr().unwrap();
        let mut queued = Vec::new();
        while let Ok(Ok(stream)) =
            tokio::time::timeout(Duration::from_millis(200), TcpStream::connect(address)).await
        {
            queued.push(stream);
            assert!(queued.len() < 8, "the accept queue doesn't fill");
        }
        Unanswering {
            listener,
            _queued: queued,
        }
    }

    #[test]
    fn the_families_take_turns() {
        let address = |text: &str| text.parse::<SocketAddr>().unwrap();
        let (v6a, v6b, v6c) = (
            address("[2001:db8::1]:993"),
            address("[2001:db8::2]:993"),
            address("[2001:db8::3]:993"),
        );
        let (v4a, v4b) = (address("192.0.2.1:993"), address("192.0.2.2:993"));
        assert_eq!(
            interleaved(vec![v6a, v6b, v6c, v4a, v4b]),
            [v6a, v4a, v6b, v4b, v6c]
        );
        assert_eq!(interleaved(vec![v4a, v6a, v4b]), [v4a, v6a, v4b]);
        assert_eq!(interleaved(vec![v4a, v4b]), [v4a, v4b]);
        assert!(interleaved(Vec::new()).is_empty());
    }

    #[tokio::test]
    async fn an_address_that_never_answers_doesnt_hold_up_the_next() {
        let refused = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let silent = unanswering().await;
        let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addresses = vec![
            refused,
            silent.listener.local_addr().unwrap(),
            live.local_addr().unwrap(),
        ];
        let stream = tokio::time::timeout(Duration::from_secs(5), first_to_answer(addresses))
            .await
            .expect("the live address answers within the limit")
            .unwrap();
        assert_eq!(stream.peer_addr().unwrap(), live.local_addr().unwrap());
    }

    #[tokio::test]
    async fn a_closed_port_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = tcp("127.0.0.1", port, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(error.failure(), LoginFailure::Unreachable);
    }
}
