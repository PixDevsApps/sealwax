//! TCP and TLS for the engine's connections. Certificates are verified with rustls and the
//! platform verifier; a testbed CA is trusted through its own constructor that trusts exactly
//! that CA, never by weakening verification (SECURITY.md, Transport).

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use rustls_platform_verifier::BuilderVerifierExt;
use sealwax_domain::account::LoginFailure;
use socket2::{SockRef, TcpKeepalive};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

#[cfg(test)]
pub(crate) mod testing;

/// Idle time before the first keepalive probe, and the time between probes.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_RETRIES: u32 = 4;

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
}

impl ConnectError {
    pub fn failure(&self) -> LoginFailure {
        match self {
            ConnectError::Resolve(_) | ConnectError::Connect(_) => LoginFailure::Unreachable,
            ConnectError::Timeout => LoginFailure::Timeout,
            ConnectError::Tls(error) => tls_failure(error),
            ConnectError::ServerName => LoginFailure::Other,
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
        let mut last = None;
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => {
                    keepalive(&stream).map_err(ConnectError::Connect)?;
                    stream.set_nodelay(true).map_err(ConnectError::Connect)?;
                    return Ok(stream);
                }
                Err(error) => last = Some(error),
            }
        }
        Err(ConnectError::Connect(last.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no address")
        })))
    })
    .await
    .map_err(|_| ConnectError::Timeout)?
}

fn keepalive(stream: &TcpStream) -> io::Result<()> {
    let keepalive = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    SockRef::from(stream).set_tcp_keepalive(&keepalive)
}

/// The TLS handshake on `tcp`, verifying the certificate against `host`.
pub(crate) async fn tls(
    tcp: TcpStream,
    config: Arc<ClientConfig>,
    host: &str,
    timeout: Duration,
) -> Result<TlsStream<TcpStream>, ConnectError> {
    let name = ServerName::try_from(host.to_owned()).map_err(|_| ConnectError::ServerName)?;
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
