//! SMTP on lettre 0.11 (ADR-007): the login check (PRD A3) and submission ([`submit`], M6). A
//! connection is implicit TLS (465) or STARTTLS that must succeed (587), `hello_name`
//! 127.0.0.1, then AUTH. TCP comes from [`crate::net`] (keepalive, and DNS and connect failures
//! told apart), and so does the implicit-TLS handshake, with the same rustls configuration as
//! IMAP, because lettre takes a connected stream only before its greeting. STARTTLS uses
//! lettre's own TLS parameters, built from the same [`Trust`]. lettre's typed EHLO answer keeps
//! neither SIZE nor keywords in any case but upper, so Sealwax sends EHLO once more after TLS
//! and reads the extensions itself ([`Extensions`]).

pub mod submit;

use crate::imap::Credentials;
use crate::net::{self, Trust, tls_failure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use lettre::transport::smtp::authentication::{self, Mechanism};
use lettre::transport::smtp::client::{
    AsyncSmtpConnection, AsyncTokioStream, Certificate, CertificateStore, TlsParameters,
};
use lettre::transport::smtp::commands::Ehlo;
use lettre::transport::smtp::extension::ClientId;
use lettre::transport::smtp::response::Response;
use sealwax_domain::account::{Endpoint, LoginFailure, Security};
use std::error::Error as _;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use zeroize::Zeroizing;

/// The whole check: connect, TLS, EHLO, AUTH, QUIT.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const QUIT_TIMEOUT: Duration = Duration::from_secs(2);

/// `EHLO [127.0.0.1]`: no host name leaves the machine (SECURITY.md, Outgoing mail).
fn hello() -> ClientId {
    ClientId::Ipv4(Ipv4Addr::LOCALHOST)
}

/// Logs in to the submission server and quits.
pub async fn check_login(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    trust: &Trust,
) -> Result<(), LoginFailure> {
    let result = tokio::time::timeout(CHECK_TIMEOUT, login(endpoint, credentials, trust))
        .await
        .unwrap_or(Err(LoginFailure::Timeout));
    if let Err(failure) = result {
        tracing::debug!(?failure, "smtp login check failed");
    }
    result
}

async fn login(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    trust: &Trust,
) -> Result<(), LoginFailure> {
    let mut conn = open(endpoint, trust, CHECK_TIMEOUT).await?;
    authenticate(&mut conn, credentials).await?;
    quit(conn).await;
    Ok(())
}

/// Connects, greets with EHLO and secures the connection: TLS from the first byte, or STARTTLS
/// that must succeed. Nothing but EHLO and STARTTLS is sent before TLS.
pub(crate) async fn open(
    endpoint: &Endpoint,
    trust: &Trust,
    timeout: Duration,
) -> Result<AsyncSmtpConnection, LoginFailure> {
    let tcp = net::tcp(&endpoint.host, endpoint.port, timeout)
        .await
        .map_err(|error| error.failure())?;
    let stream: Box<dyn AsyncTokioStream> = match endpoint.security {
        Security::Tls => {
            let config = trust.client_config().map_err(|_| LoginFailure::Other)?;
            let tls = net::tls(tcp, config, &endpoint.host, timeout)
                .await
                .map_err(|error| error.failure())?;
            Box::new(Tls(tls))
        }
        Security::StartTls => Box::new(Plain::new(tcp)),
    };
    let mut conn = AsyncSmtpConnection::connect_with_transport(stream, &hello())
        .await
        .map_err(|error| failure(&error))?;
    if endpoint.security == Security::StartTls {
        let parameters = tls_parameters(&endpoint.host, trust)?;
        conn.starttls(parameters, &hello())
            .await
            .map_err(|error| starttls_failure(&error))?;
    }
    Ok(conn)
}

/// What a server's EHLO answer names that submission needs. Its name and any other text in the
/// answer are dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extensions {
    /// SIZE (RFC 1870): the largest message the server takes, in bytes; `None` when it names
    /// none, or names 0 (no fixed limit).
    pub size: Option<u64>,
    /// SMTPUTF8 (RFC 6531): addresses with non-ASCII local parts.
    pub smtputf8: bool,
    /// 8BITMIME (RFC 6152): message bytes above 127.
    pub eight_bit: bool,
}

impl Extensions {
    /// Reads an EHLO answer: one keyword per line after the first, which names the server.
    pub(crate) fn from_response(response: &Response) -> Extensions {
        Extensions::from_lines(response.message().skip(1))
    }

    fn from_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Extensions {
        let mut found = Extensions::default();
        for line in lines {
            let mut words = line.split_whitespace();
            let keyword = words.next().unwrap_or_default().to_ascii_uppercase();
            match keyword.as_str() {
                "SIZE" => {
                    found.size = words
                        .next()
                        .and_then(|limit| limit.parse::<u64>().ok())
                        .filter(|limit| *limit > 0);
                }
                "SMTPUTF8" => found.smtputf8 = true,
                "8BITMIME" => found.eight_bit = true,
                _ => {}
            }
        }
        found
    }
}

/// EHLO once more on a secured connection, before AUTH, for its extensions.
pub(crate) async fn extensions(
    conn: &mut AsyncSmtpConnection,
) -> Result<Extensions, lettre::transport::smtp::Error> {
    let response = conn.command(Ehlo::new(hello())).await?;
    Ok(Extensions::from_response(&response))
}

/// One command line as given, ended with CRLF.
pub(crate) struct Line<'a>(pub &'a str);

impl fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\r\n", self.0)
    }
}

/// Logs in on a secured connection: PLAIN, else LOGIN, for a password; XOAUTH2 for a token,
/// spoken by Sealwax so that an error challenge is answered (lettre's AUTH gives up on it and
/// leaves the exchange open). A refused login leaves the connection to be dropped: lettre's
/// abort would wait for QUIT's reply without a timeout.
pub(crate) async fn authenticate(
    conn: &mut AsyncSmtpConnection,
    credentials: Credentials<'_>,
) -> Result<(), LoginFailure> {
    let info = conn.server_info();
    match credentials {
        Credentials::Password { username, password } => {
            let mechanisms: Vec<Mechanism> = [Mechanism::Plain, Mechanism::Login]
                .into_iter()
                .filter(|m| info.supports_auth_mechanism(*m))
                .collect();
            if mechanisms.is_empty() {
                return Err(if info.supports_auth_mechanism(Mechanism::Xoauth2) {
                    LoginFailure::OAuthRequired
                } else {
                    LoginFailure::Other
                });
            }
            let credentials =
                authentication::Credentials::new(username.to_owned(), password.to_owned());
            let result = conn.auth(&mechanisms, &credentials).await;
            drop(credentials);
            result.map(drop).map_err(|error| failure(&error))
        }
        Credentials::OAuth2 { username, token } => {
            if !info.supports_auth_mechanism(Mechanism::Xoauth2) {
                return Err(LoginFailure::Other);
            }
            let response = crate::imap::xoauth2_response(username, token);
            let command = Zeroizing::new(format!("AUTH XOAUTH2 {}", STANDARD.encode(&*response)));
            let mut reply = conn.command(Line(&command)).await;
            drop(command);
            // An error challenge (334 with a JSON status, RFC 7628 3.2.3) is answered with an
            // empty line, after which the server sends its refusal.
            if reply.as_ref().is_ok_and(|response| response.has_code(334)) {
                reply = conn.command(Line("")).await;
            }
            match reply {
                Ok(response) if response.has_code(235) => Ok(()),
                Ok(_) => Err(LoginFailure::Other),
                Err(error) => Err(failure(&error)),
            }
        }
    }
}

/// QUIT, waiting [`QUIT_TIMEOUT`] at most for its reply.
pub(crate) async fn quit(mut conn: AsyncSmtpConnection) {
    let _ = tokio::time::timeout(QUIT_TIMEOUT, conn.quit()).await;
}

fn tls_parameters(host: &str, trust: &Trust) -> Result<TlsParameters, LoginFailure> {
    let builder = TlsParameters::builder(host.to_owned());
    let builder = match trust.root_pem().map_err(|_| LoginFailure::Other)? {
        None => builder,
        Some(pem) => builder
            .certificate_store(CertificateStore::None)
            .add_root_certificate(Certificate::from_pem(&pem).map_err(|_| LoginFailure::Other)?),
    };
    builder.build_rustls().map_err(|_| LoginFailure::Other)
}

/// A refused or failed STARTTLS: lettre refuses a server that doesn't offer it, and anything
/// but a working TLS session ends the check.
fn starttls_failure(error: &lettre::transport::smtp::Error) -> LoginFailure {
    match failure(error) {
        LoginFailure::Certificate | LoginFailure::Timeout | LoginFailure::Unreachable => {
            failure(error)
        }
        _ => LoginFailure::NoTls,
    }
}

/// Classifies what lettre reported, from the reply code and text or the I/O error beneath.
pub(crate) fn failure(error: &lettre::transport::smtp::Error) -> LoginFailure {
    let text = error
        .source()
        .map(|source| source.to_string().to_lowercase())
        .unwrap_or_default();
    if let Some(code) = error.status() {
        return reply(u16::from(code), &text);
    }
    let mut source = error.source();
    while let Some(inner) = source {
        if let Some(io) = inner.downcast_ref::<io::Error>() {
            return match io.kind() {
                io::ErrorKind::TimedOut => LoginFailure::Timeout,
                io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset => {
                    LoginFailure::Unreachable
                }
                _ if io.get_ref().is_some_and(|e| e.is::<rustls::Error>()) => tls_failure(io),
                _ => LoginFailure::Other,
            };
        }
        source = inner.source();
    }
    if error.is_timeout() {
        LoginFailure::Timeout
    } else {
        LoginFailure::Other
    }
}

/// An SMTP reply to AUTH (RFC 4954 codes, provider texts as documented or reported; see
/// docs/m2/imap.md for which are unverified).
pub(crate) fn reply(code: u16, text: &str) -> LoginFailure {
    let has = |needle: &str| text.contains(needle);
    if has("application-specific password") || (code == 534 && has("5.7.9")) {
        LoginFailure::AppPasswordRequired
    } else if has("basic authentication is disabled") {
        LoginFailure::OAuthRequired
    } else if code == 538 || (code == 530 && has("starttls")) {
        LoginFailure::NoTls
    } else if code == 535 && !has("smtpclientauthentication is disabled") {
        LoginFailure::WrongPassword
    } else {
        LoginFailure::Other
    }
}

/// The first byte of a TLS handshake record; no SMTP command starts with it.
const HANDSHAKE_RECORD: u8 = 0x16;

/// The plaintext stream of a STARTTLS connection, handed to lettre. lettre reads replies
/// through a buffer it keeps across its TLS upgrade, so bytes a man in the middle sends after
/// STARTTLS's 220 would be read as if they came over TLS (STARTTLS response injection, found in
/// ML-093). Until the handshake starts this hands lettre at most one line per read and keeps
/// the rest; the handshake's first record fails if anything is kept.
#[derive(Debug)]
struct Plain {
    tcp: TcpStream,
    /// Read from the socket and not yet handed on.
    ahead: Vec<u8>,
    tls: bool,
}

impl Plain {
    fn new(tcp: TcpStream) -> Self {
        Plain {
            tcp,
            ahead: Vec::new(),
            tls: false,
        }
    }
}

impl AsyncTokioStream for Plain {
    fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.peer_addr()
    }
}

impl AsyncRead for Plain {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.tls {
            return Pin::new(&mut self.tcp).poll_read(cx, buf);
        }
        if self.ahead.is_empty() {
            let mut chunk = [0; 4096];
            let mut read = ReadBuf::new(&mut chunk);
            std::task::ready!(Pin::new(&mut self.tcp).poll_read(cx, &mut read))?;
            self.ahead.extend_from_slice(read.filled());
        }
        let line = self
            .ahead
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(self.ahead.len(), |at| at + 1);
        let take = line.min(buf.remaining());
        buf.put_slice(&self.ahead[..take]);
        self.ahead.drain(..take);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Plain {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if !self.tls && buf.first() == Some(&HANDSHAKE_RECORD) {
            if !self.ahead.is_empty() {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "data sent ahead of the TLS handshake",
                )));
            }
            self.tls = true;
        }
        Pin::new(&mut self.tcp).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.tcp).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.tcp).poll_shutdown(cx)
    }
}

/// The implicit-TLS stream handed to lettre.
#[derive(Debug)]
struct Tls(TlsStream<TcpStream>);

impl AsyncTokioStream for Tls {
    fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0.get_ref().0.peer_addr()
    }
}

impl AsyncRead for Tls {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tls {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests;
