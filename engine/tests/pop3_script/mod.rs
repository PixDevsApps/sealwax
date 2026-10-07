//! A scripted POP3 server on 127.0.0.1 for the client's suites (ML-087): a throwaway CA, one
//! connection per server answered by a script, implicit TLS or plaintext switched to TLS after
//! STLS, and the usual greeting, CAPA and login. Synthetic data only.

#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use sealwax_domain::account::{Endpoint, Security};
use sealwax_engine::net::Trust;
use sealwax_engine::pop3::{self, Credentials, Options, Pop3Error, Session};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub const USERNAME: &str = "kari.nordmann";
pub const SECRET: &str = "secret-password";
pub const PASSWORD: Credentials<'static> = Credentials::Password {
    username: USERNAME,
    password: SECRET,
};
/// What the scripted server says it can do before and after the login, as Dovecot does.
pub const CAPS: &[&str] = &[
    "TOP",
    "UIDL",
    "RESP-CODES",
    "PIPELINING",
    "USER",
    "SASL PLAIN",
];
pub const CAPS_AFTER: &[&str] = &["TOP", "UIDL", "RESP-CODES", "PIPELINING"];

pub struct Ca {
    _dir: tempfile::TempDir,
    pub trust: Trust,
    pub server: Arc<ServerConfig>,
}

pub fn ca() -> Ca {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
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
    Ca {
        _dir: dir,
        trust: Trust::Roots(path),
        server: Arc::new(server),
    }
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The server's side of one connection; `received` keeps every line the client sent.
pub struct Peer {
    stream: BufReader<Box<dyn Io>>,
    pub received: Vec<String>,
}

impl Peer {
    pub async fn send(&mut self, text: &str) {
        self.send_bytes(text.as_bytes()).await;
    }

    pub async fn send_bytes(&mut self, bytes: &[u8]) {
        let stream = self.stream.get_mut();
        stream.write_all(bytes).await.unwrap();
        stream.flush().await.unwrap();
    }

    /// The client's next line without its line end; `None` once it closed.
    pub async fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.stream.read_line(&mut line).await {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                let line = line.trim_end_matches(['\r', '\n']).to_owned();
                self.received.push(line.clone());
                Some(line)
            }
        }
    }

    /// The next line, which must start with `command`.
    pub async fn expect(&mut self, command: &str) -> String {
        let line = self.line().await.unwrap_or_default();
        assert!(
            line.starts_with(command),
            "expected {command}, got {line:?}"
        );
        line
    }

    /// Switches to TLS as the server (after STLS).
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

    /// The connection closed here, as a dropped network would.
    pub fn hang_up(self) -> Peer {
        Peer {
            stream: BufReader::new(Box::new(tokio::io::empty())),
            received: self.received,
        }
    }

    /// Reads until the client closes.
    pub async fn drain(mut self) -> Vec<String> {
        while self.line().await.is_some() {}
        self.received
    }

    /// CAPA's answer.
    pub async fn caps(&mut self, caps: &[&str]) {
        self.expect("CAPA").await;
        let mut reply = "+OK Capability list follows\r\n".to_owned();
        for line in caps {
            reply.push_str(line);
            reply.push_str("\r\n");
        }
        reply.push_str(".\r\n");
        self.send(&reply).await;
    }

    /// The greeting and CAPA.
    pub async fn hello(&mut self, caps: &[&str]) {
        self.send("+OK POP3 ready\r\n").await;
        self.caps(caps).await;
    }

    /// The usual login: the greeting, CAPA, AUTH PLAIN accepted, CAPA again.
    pub async fn logged_in(&mut self) {
        self.hello(CAPS).await;
        self.expect("AUTH PLAIN ").await;
        self.send("+OK Logged in.\r\n").await;
        self.caps(CAPS_AFTER).await;
    }
}

/// A server for one connection, TLS from the first byte when `tls` is given; the handle answers
/// every line the client sent.
pub async fn serve<F, Fut>(
    tls: Option<Arc<ServerConfig>>,
    script: F,
) -> (u16, JoinHandle<Vec<String>>)
where
    F: FnOnce(Peer) -> Fut + Send + 'static,
    Fut: Future<Output = Peer> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let stream: Box<dyn Io> = match tls {
            Some(config) => Box::new(
                tokio_rustls::TlsAcceptor::from(config)
                    .accept(socket)
                    .await
                    .unwrap(),
            ),
            None => Box::new(socket),
        };
        let peer = Peer {
            stream: BufReader::new(stream),
            received: Vec::new(),
        };
        script(peer).await.drain().await
    });
    (port, handle)
}

pub fn endpoint(port: u16, security: Security) -> Endpoint {
    Endpoint {
        host: "127.0.0.1".into(),
        port,
        security,
    }
}

/// Short timeouts, so a silent server is noticed quickly.
pub fn quick() -> Options {
    Options {
        connect_timeout: Duration::from_secs(5),
        command_timeout: Duration::from_millis(500),
        ..Options::default()
    }
}

pub async fn connect(
    ca: &Ca,
    port: u16,
    credentials: Credentials<'_>,
) -> Result<Session, Pop3Error> {
    pop3::connect_with(
        &endpoint(port, Security::Tls),
        credentials,
        ca.trust.client_config().unwrap(),
        quick(),
    )
    .await
}

pub fn plain_response(line: &str) -> Vec<u8> {
    STANDARD
        .decode(line.trim_start_matches("AUTH PLAIN "))
        .unwrap()
}

pub fn assert_no_credentials(lines: &[String]) {
    for line in lines {
        for secret in ["USER", "PASS", "AUTH", SECRET, USERNAME] {
            assert!(!line.contains(secret), "{secret} sent: {lines:?}");
        }
    }
}

/// A UIDL and LIST reply for `sizes`, numbered from 1, with UIDLs `uid-<n>`.
pub fn listing_replies(sizes: &[u64]) -> (String, String) {
    let mut uidl = "+OK\r\n".to_owned();
    let mut list = format!("+OK {} messages\r\n", sizes.len());
    for (i, size) in sizes.iter().enumerate() {
        uidl.push_str(&format!("{} uid-{}\r\n", i + 1, i + 1));
        list.push_str(&format!("{} {size}\r\n", i + 1));
    }
    uidl.push_str(".\r\n");
    list.push_str(".\r\n");
    (uidl, list)
}

pub async fn answer_listing(peer: &mut Peer, sizes: &[u64]) {
    let (uidl, list) = listing_replies(sizes);
    peer.expect("UIDL").await;
    peer.send(&uidl).await;
    peer.expect("LIST").await;
    peer.send(&list).await;
}

/// A message as stored and as it goes on the wire: dot-stuffed, ending in `CRLF . CRLF`.
pub fn wire_form(message: &[u8]) -> Vec<u8> {
    let mut out = b"+OK message follows\r\n".to_vec();
    for line in message.split_inclusive(|&byte| byte == b'\n') {
        if line.starts_with(b".") {
            out.push(b'.');
        }
        out.extend_from_slice(line);
    }
    out.extend_from_slice(b".\r\n");
    out
}
