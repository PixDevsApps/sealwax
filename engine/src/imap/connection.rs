//! Connecting and logging in (ML-035): implicit TLS, or STARTTLS that must succeed; AUTHENTICATE
//! PLAIN, LOGIN only when PLAIN is missing and LOGINDISABLED isn't set, XOAUTH2 with a token from
//! the caller; ID with generic values; ENABLE QRESYNC where offered. Nothing is ever sent in
//! plaintext but the STARTTLS command itself.

use super::error::{ImapError, rejection};
use super::hidden;
use super::raw::{self, Session};
use crate::net;
use async_imap::imap_proto::{Capability, Response, ResponseCode, Status};
use async_imap::types::UnsolicitedResponse;
use rustls::ClientConfig;
use sealwax_domain::account::{Endpoint, Security};
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

type Client = async_imap::Client<super::Stream>;

/// TCP, TLS, the greeting and the login together.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// What a session logs in with. The secrets are borrowed and never printed.
#[derive(Clone, Copy)]
pub enum Credentials<'a> {
    /// A password or app password: AUTHENTICATE PLAIN, else LOGIN.
    Password {
        username: &'a str,
        password: &'a str,
    },
    /// An OAuth2 access token: AUTHENTICATE XOAUTH2.
    OAuth2 { username: &'a str, token: &'a str },
}

impl fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials(..)")
    }
}

/// The server's capabilities, upper case.
#[derive(Clone, Debug, Default)]
pub struct Caps(HashSet<String>);

impl Caps {
    fn from_iter<'a>(caps: impl IntoIterator<Item = &'a Capability<'a>>) -> Self {
        Caps(
            caps.into_iter()
                .map(|capability| match capability {
                    Capability::Imap4rev1 => "IMAP4REV1".to_owned(),
                    Capability::Auth(mechanism) => {
                        format!("AUTH={}", mechanism.to_ascii_uppercase())
                    }
                    Capability::Atom(atom) => atom.to_ascii_uppercase(),
                })
                .collect(),
        )
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.contains(&name.to_ascii_uppercase())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mechanism {
    Plain,
    Login,
    XOAuth2,
}

/// A logged-in IMAP session.
pub struct Connection {
    pub(crate) session: Session,
    caps: Caps,
    qresync: bool,
    mechanism: Mechanism,
    /// The server is Microsoft's (Outlook.com or Microsoft 365), by its host.
    microsoft: bool,
    /// The wire name of the mailbox selected or examined, if any.
    pub(crate) selected: Option<String>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("mechanism", &self.mechanism)
            .field("qresync", &self.qresync)
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Capabilities after login.
    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    pub fn mechanism(&self) -> Mechanism {
        self.mechanism
    }

    /// QRESYNC is enabled (and with it CONDSTORE).
    pub fn qresync(&self) -> bool {
        self.qresync
    }

    /// CONDSTORE's HIGHESTMODSEQ is available.
    pub fn condstore(&self) -> bool {
        self.qresync || self.caps.has("CONDSTORE")
    }

    /// The server is Microsoft's (Outlook.com or Microsoft 365), by its host.
    pub fn microsoft(&self) -> bool {
        self.microsoft
    }

    /// Ends the session politely; errors don't matter any more.
    pub async fn logout(self) {
        self.logout_within(Duration::from_secs(2)).await;
    }

    /// LOGOUT, waiting at most `wait` for the server's answer.
    pub async fn logout_within(mut self, wait: Duration) {
        let _ = tokio::time::timeout(wait, raw::run(&mut self.session, "LOGOUT", |_| {})).await;
    }

    /// The session, apart from what the connection knows about it (for IDLE, which takes the
    /// session and gives it back).
    pub(crate) fn into_parts(self) -> (Session, Parts) {
        let Connection {
            session,
            caps,
            qresync,
            mechanism,
            microsoft,
            selected,
        } = self;
        (
            session,
            Parts {
                caps,
                qresync,
                mechanism,
                microsoft,
                selected,
            },
        )
    }

    pub(crate) fn from_parts(session: Session, parts: Parts) -> Connection {
        let Parts {
            caps,
            qresync,
            mechanism,
            microsoft,
            selected,
        } = parts;
        Connection {
            session,
            caps,
            qresync,
            mechanism,
            microsoft,
            selected,
        }
    }
}

/// What a [`Connection`] knows besides its session.
pub(crate) struct Parts {
    caps: Caps,
    qresync: bool,
    mechanism: Mechanism,
    microsoft: bool,
    selected: Option<String>,
}

/// The SASL PLAIN response: no authorization identity.
pub(crate) fn plain_response(username: &str, password: &str) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(format!("\0{username}\0{password}").into_bytes())
}

/// The SASL XOAUTH2 response (Google's and Microsoft's format).
pub(crate) fn xoauth2_response(username: &str, token: &str) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(format!("user={username}\x01auth=Bearer {token}\x01\x01").into_bytes())
}

/// Answers the first challenge with the response and any later one (XOAUTH2's error challenge)
/// with an empty line, after which the server sends its NO.
pub(crate) struct OneShot(Option<Zeroizing<Vec<u8>>>);

impl async_imap::Authenticator for OneShot {
    type Response = Zeroizing<Vec<u8>>;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        self.0.take().unwrap_or_else(|| Zeroizing::new(Vec::new()))
    }
}

enum Greeting {
    Ok(Option<Caps>),
    PreAuth,
}

async fn greeting<T>(client: &mut async_imap::Client<T>) -> Result<Greeting, ImapError>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + fmt::Debug + Send,
{
    let response = client
        .read_response()
        .await
        .map_err(|error| ImapError::reading(error, "the greeting"))?
        .ok_or(ImapError::ConnectionLost)?;
    match response.parsed() {
        Response::Data {
            status: Status::Ok,
            outcome,
        } => Ok(Greeting::Ok(match &outcome.code {
            Some(ResponseCode::Capabilities(caps)) => Some(Caps::from_iter(caps)),
            _ => None,
        })),
        Response::Data {
            status: Status::PreAuth,
            ..
        } => Ok(Greeting::PreAuth),
        Response::Data {
            status: Status::Bye,
            ..
        } => Err(ImapError::Bye(super::error::ServerText(String::new()))),
        _ => Err(ImapError::Protocol("unexpected greeting")),
    }
}

/// CAPABILITY before login: `Client` only offers `run_command_and_check_ok`, which forwards the
/// untagged response to a channel.
async fn client_capabilities(client: &mut Client) -> Result<Caps, ImapError> {
    let (sender, receiver) = async_channel::bounded(16);
    client
        .run_command_and_check_ok("CAPABILITY", Some(sender))
        .await?;
    while let Ok(response) = receiver.try_recv() {
        if let UnsolicitedResponse::Other(data) = response
            && let Response::Capabilities(caps) = data.parsed()
        {
            return Ok(Caps::from_iter(caps));
        }
    }
    Err(ImapError::Protocol("no CAPABILITY response"))
}

async fn capabilities(session: &mut Session) -> Result<Caps, ImapError> {
    let mut caps = None;
    raw::run(session, "CAPABILITY", |response| {
        if let Response::Capabilities(found) = response {
            caps = Some(Caps::from_iter(found));
        }
    })
    .await?;
    caps.ok_or(ImapError::Protocol("no CAPABILITY response"))
}

/// How a session is prepared after login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectOptions {
    /// ENABLE QRESYNC where offered. Off, a CONDSTORE server is synced without VANISHED (a
    /// quirk switch, and how the tests reach the CONDSTORE-only path on the testbed).
    pub qresync: bool,
    /// UID MOVE where offered. Off, moves are COPY, `\Deleted` and UID EXPUNGE as without MOVE
    /// (a quirk switch, and how the tests reach that path on servers that offer MOVE).
    pub moves: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions {
            qresync: true,
            moves: true,
        }
    }
}

/// Connects, logs in and prepares the session. `tls` comes from [`crate::net::Trust`].
pub async fn connect(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
) -> Result<Connection, ImapError> {
    connect_with(endpoint, credentials, tls, ConnectOptions::default()).await
}

/// [`connect`] with `options`.
pub async fn connect_with(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
    options: ConnectOptions,
) -> Result<Connection, ImapError> {
    let started = std::time::Instant::now();
    let connection = tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_inner(endpoint, credentials, tls, options),
    )
    .await
    .map_err(|_| ImapError::Timeout)??;
    tracing::debug!(
        mechanism = ?connection.mechanism,
        qresync = connection.qresync,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "imap connected"
    );
    Ok(connection)
}

async fn connect_inner(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
    options: ConnectOptions,
) -> Result<Connection, ImapError> {
    let tcp = net::tcp(&endpoint.host, endpoint.port, CONNECT_TIMEOUT).await?;
    let (mut client, greeting_caps) = match endpoint.security {
        Security::Tls => {
            let stream = net::tls(tcp, tls, endpoint, CONNECT_TIMEOUT).await?;
            let mut client = Client::new(stream);
            match greeting(&mut client).await? {
                Greeting::Ok(caps) => (client, caps),
                Greeting::PreAuth => return Err(ImapError::Protocol("PREAUTH greeting")),
            }
        }
        Security::StartTls => {
            let mut plain = async_imap::Client::new(tcp);
            // A PREAUTH greeting would skip STARTTLS; an attacker can inject it.
            if let Greeting::PreAuth = greeting(&mut plain).await? {
                return Err(ImapError::StartTls);
            }
            // STARTTLS is sent whatever the plaintext capabilities say (an attacker can strip
            // them), and anything but OK ends the attempt: there is no plaintext path.
            match plain.run_command_and_check_ok("STARTTLS", None).await {
                Ok(()) => {}
                Err(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)) => {
                    return Err(ImapError::StartTls);
                }
                Err(error) => return Err(error.into()),
            }
            // into_inner drops whatever was buffered after the OK, so plaintext injected there
            // is never read as if it came over TLS. Capabilities are asked again over TLS.
            let stream = net::tls(plain.into_inner(), tls, endpoint, CONNECT_TIMEOUT).await?;
            (Client::new(stream), None)
        }
    };
    let caps = match greeting_caps {
        Some(caps) => caps,
        None => client_capabilities(&mut client).await?,
    };
    let (mut session, mechanism) = login(client, &caps, credentials).await?;
    let mut caps = capabilities(&mut session).await?;
    if !options.moves {
        caps.0.remove("MOVE");
    }
    if caps.has("ID") {
        let id = format!(
            "ID (\"name\" \"Sealwax\" \"version\" \"{}\")",
            env!("CARGO_PKG_VERSION")
        );
        match raw::run(&mut session, &id, |_| {}).await {
            Ok(()) | Err(ImapError::No(_) | ImapError::Bad(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let mut qresync = false;
    if options.qresync && caps.has("QRESYNC") && caps.has("ENABLE") {
        raw::run(&mut session, "ENABLE QRESYNC", |response| {
            if let Response::Capabilities(enabled) = response {
                qresync |= enabled.iter().any(
                    |c| matches!(c, Capability::Atom(atom) if atom.eq_ignore_ascii_case("QRESYNC")),
                );
            }
        })
        .await?;
    }
    Ok(Connection {
        session,
        caps,
        qresync,
        mechanism,
        microsoft: hidden::microsoft_host(&endpoint.host),
        selected: None,
    })
}

async fn login(
    client: Client,
    caps: &Caps,
    credentials: Credentials<'_>,
) -> Result<(Session, Mechanism), ImapError> {
    let oauth = caps.has("AUTH=XOAUTH2") || caps.has("AUTH=OAUTHBEARER");
    let result = match credentials {
        Credentials::OAuth2 { username, token } => {
            if !caps.has("AUTH=XOAUTH2") {
                return Err(ImapError::NoMechanism { oauth });
            }
            let response = OneShot(Some(xoauth2_response(username, token)));
            client
                .authenticate("XOAUTH2", response)
                .await
                .map(|session| (session, Mechanism::XOAuth2))
        }
        Credentials::Password { username, password } if caps.has("AUTH=PLAIN") => {
            let response = OneShot(Some(plain_response(username, password)));
            client
                .authenticate("PLAIN", response)
                .await
                .map(|session| (session, Mechanism::Plain))
        }
        Credentials::Password { username, password } if !caps.has("LOGINDISABLED") => client
            .login(username, password)
            .await
            .map(|session| (session, Mechanism::Login)),
        Credentials::Password { .. } => return Err(ImapError::NoMechanism { oauth }),
    };
    result.map_err(|(error, _client)| match error {
        async_imap::error::Error::No(text) => ImapError::Rejected(rejection(&text, false)),
        async_imap::error::Error::Bad(text) => ImapError::Rejected(rejection(&text, true)),
        other => other.into(),
    })
}

#[cfg(test)]
mod tests;
