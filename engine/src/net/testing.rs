//! Scripted servers for protocol tests: a throwaway CA and certificate for 127.0.0.1, and a peer
//! that reads the client's lines and writes canned replies, in plaintext or over TLS.

use super::Trust;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A CA in a scratch folder, trusted through `trust`, and a server certificate it signed.
pub(crate) struct TestCa {
    _dir: tempfile::TempDir,
    pub trust: Trust,
    pub server: Arc<ServerConfig>,
}

pub(crate) fn test_ca() -> TestCa {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()]).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.signed_by(&key, &ca).unwrap();
    let server =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ca.pem");
    std::fs::write(&path, ca.pem()).unwrap();
    TestCa {
        _dir: dir,
        trust: Trust::Roots(path),
        server: Arc::new(server),
    }
}

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The server's side of one connection. `received` keeps every line the client sent.
pub(crate) struct Peer {
    stream: BufReader<Box<dyn Io>>,
    pub received: String,
}

impl Peer {
    /// Accepts one connection on `listener`, plaintext.
    pub async fn accept(listener: &TcpListener) -> Peer {
        let (socket, _) = listener.accept().await.unwrap();
        Peer {
            stream: BufReader::new(Box::new(socket)),
            received: String::new(),
        }
    }

    /// Accepts one connection and completes the TLS handshake as the server.
    pub async fn accept_tls(listener: &TcpListener, config: Arc<ServerConfig>) -> Peer {
        Peer::accept(listener).await.tls(config).await
    }

    /// Switches to TLS (after STARTTLS). Anything the client sent ahead is dropped.
    pub async fn tls(self, config: Arc<ServerConfig>) -> Peer {
        let plain = self.stream.into_inner();
        let tls = tokio_rustls::TlsAcceptor::from(config)
            .accept(plain)
            .await
            .unwrap();
        Peer {
            stream: BufReader::new(Box::new(tls)),
            received: self.received,
        }
    }

    /// The connection itself, for a handshake that is meant to fail.
    pub fn into_inner(self) -> Box<dyn Io> {
        self.stream.into_inner()
    }

    pub async fn send(&mut self, text: &str) {
        let stream = self.stream.get_mut();
        stream.write_all(text.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
    }

    /// The client's next line without CRLF; `None` once it closed.
    pub async fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.stream.read_line(&mut line).await {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                self.received.push_str(&line);
                Some(line.trim_end_matches(['\r', '\n']).to_owned())
            }
        }
    }

    /// The tag of the client's next command, checking that the command starts with `command`.
    pub async fn expect(&mut self, command: &str) -> String {
        let line = self.line().await.unwrap_or_default();
        let (tag, rest) = line.split_once(' ').unwrap_or_default();
        assert!(
            rest.to_ascii_uppercase()
                .starts_with(&command.to_ascii_uppercase()),
            "expected {command}, got {line}"
        );
        tag.to_owned()
    }

    /// Reads until the client closes, for checks on everything it sent.
    pub async fn drain(mut self) -> String {
        while self.line().await.is_some() {}
        self.received
    }
}

pub(crate) async fn bind() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}
