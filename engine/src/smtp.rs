//! The SMTP half of the login check (PRD A3) on lettre 0.11 (ADR-007): implicit TLS (465) or
//! STARTTLS that must succeed (587), `hello_name` 127.0.0.1, then AUTH and QUIT; nothing is
//! sent. TCP comes from [`crate::net`] (keepalive, and DNS and connect failures told apart),
//! and so does the implicit-TLS handshake, with the same rustls configuration as IMAP, because
//! lettre takes a connected stream only before its greeting. STARTTLS uses lettre's own TLS
//! parameters, built from the same [`Trust`].

use crate::imap::Credentials;
use crate::net::{self, Trust, tls_failure};
use lettre::transport::smtp::authentication::{self, Mechanism};
use lettre::transport::smtp::client::{
    AsyncSmtpConnection, AsyncTokioStream, Certificate, CertificateStore, TlsParameters,
};
use lettre::transport::smtp::extension::ClientId;
use sealwax_domain::account::{Endpoint, LoginFailure, Security};
use std::error::Error as _;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

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
    let tcp = net::tcp(&endpoint.host, endpoint.port, CHECK_TIMEOUT)
        .await
        .map_err(|error| error.failure())?;
    let stream: Box<dyn AsyncTokioStream> = match endpoint.security {
        Security::Tls => {
            let config = trust.client_config().map_err(|_| LoginFailure::Other)?;
            let tls = net::tls(tcp, config, &endpoint.host, CHECK_TIMEOUT)
                .await
                .map_err(|error| error.failure())?;
            Box::new(Tls(tls))
        }
        Security::StartTls => Box::new(tcp),
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
    let info = conn.server_info();
    let (username, secret, mechanisms) = match credentials {
        Credentials::Password { username, password } => (
            username,
            password,
            [Mechanism::Plain, Mechanism::Login]
                .into_iter()
                .filter(|m| info.supports_auth_mechanism(*m))
                .collect::<Vec<_>>(),
        ),
        Credentials::OAuth2 { username, token } => (
            username,
            token,
            [Mechanism::Xoauth2]
                .into_iter()
                .filter(|m| info.supports_auth_mechanism(*m))
                .collect(),
        ),
    };
    if mechanisms.is_empty() {
        let oauth_only = matches!(credentials, Credentials::Password { .. })
            && info.supports_auth_mechanism(Mechanism::Xoauth2);
        return Err(if oauth_only {
            LoginFailure::OAuthRequired
        } else {
            LoginFailure::Other
        });
    }
    let credentials = authentication::Credentials::new(username.to_owned(), secret.to_owned());
    let result = conn.auth(&mechanisms, &credentials).await;
    drop(credentials);
    // A refused login just drops the connection: lettre's abort would wait for QUIT's reply.
    result.map_err(|error| failure(&error))?;
    let _ = tokio::time::timeout(QUIT_TIMEOUT, conn.quit()).await;
    Ok(())
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
