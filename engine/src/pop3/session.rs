//! Connecting, logging in and the maildrop's commands.

use super::reply::{Refusal, Status};
use super::wire::Wire;
use super::{Credentials, Listing, Mailbox, Pop3Error};
use crate::imap::xoauth2_response;
use crate::net;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustls::ClientConfig;
use sealwax_domain::account::{Endpoint, LoginFailure, Security};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::client::TlsStream;
use zeroize::Zeroizing;

type Stream = TlsStream<TcpStream>;

/// TCP, TLS, the greeting, STLS, CAPA and the login together.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest wait for any reply line, and between the reads of a multi-line reply.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Messages larger than this aren't downloaded (a proposal for the gate).
pub const MAX_MESSAGE: u64 = 100 * 1024 * 1024;
/// The slowest rate a message's data may arrive at: RETR may take [`COMMAND_TIMEOUT`] plus the
/// LIST size at this rate.
pub const MIN_RATE: u64 = 16 * 1024;
/// QUIT after the login check, whose answer doesn't matter.
const CHECK_QUIT: Duration = Duration::from_secs(2);
/// A header from TOP.
const MAX_HEADER: usize = 1024 * 1024;
/// Lines in a UIDL or LIST reply, and the longest such line.
const MAX_MESSAGES: usize = 1_000_000;
const LISTING_LINE: usize = 1024;
/// RFC 1939 allows 70 characters; some servers use more.
const MAX_UIDL: usize = 255;
/// CAPA's lines.
const CAPA_LINES: usize = 256;
/// The longest AUTH command with an initial response, CRLF included (RFC 5034, 4).
const AUTH_LINE: usize = 255;

/// How a session is run; [`Options::default`] is what the engine uses, tests shorten it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// [`MAX_MESSAGE`].
    pub max_message: u64,
    /// [`CONNECT_TIMEOUT`].
    pub connect_timeout: Duration,
    /// [`COMMAND_TIMEOUT`].
    pub command_timeout: Duration,
    /// [`MIN_RATE`], in octets a second.
    pub min_rate: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_message: MAX_MESSAGE,
            connect_timeout: CONNECT_TIMEOUT,
            command_timeout: COMMAND_TIMEOUT,
            min_rate: MIN_RATE,
        }
    }
}

impl Options {
    /// The time a message of `size` octets may take, from RETR to its last line.
    fn data_time(&self, size: u64) -> Duration {
        self.command_timeout + Duration::from_secs(size / self.min_rate.max(1))
    }
}

/// How the session logged in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mechanism {
    /// SASL PLAIN (RFC 5034).
    Plain,
    /// USER and PASS (RFC 1939), over TLS.
    User,
    XOAuth2,
}

/// How long the server keeps mail after it was retrieved (RFC 2449's EXPIRE).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expire {
    Never,
    /// This many days; 0 means mail may not be left on the server.
    Days(u32),
}

/// What the server's CAPA says (RFC 2449): the list read after the login, or before it when
/// the server answered only then. The server's own name (IMPLEMENTATION) is dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub top: bool,
    pub uidl: bool,
    pub user: bool,
    pub stls: bool,
    pub pipelining: bool,
    pub resp_codes: bool,
    /// SASL's mechanisms, upper case; `None` when CAPA has no SASL line.
    pub sasl: Option<Vec<String>>,
    /// LOGIN-DELAY: the least time between logins.
    pub login_delay: Option<Duration>,
    pub expire: Option<Expire>,
}

impl Capabilities {
    fn read(&mut self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        let mut words = line.split_ascii_whitespace();
        let Some(name) = words.next() else {
            return;
        };
        match name.to_ascii_uppercase().as_str() {
            "TOP" => self.top = true,
            "UIDL" => self.uidl = true,
            "USER" => self.user = true,
            "STLS" => self.stls = true,
            "PIPELINING" => self.pipelining = true,
            "RESP-CODES" => self.resp_codes = true,
            "SASL" => {
                self.sasl = Some(words.map(str::to_ascii_uppercase).collect());
            }
            "LOGIN-DELAY" => {
                self.login_delay = words
                    .next()
                    .and_then(|seconds| seconds.parse().ok())
                    .map(Duration::from_secs);
            }
            "EXPIRE" => {
                self.expire = words.next().and_then(|value| {
                    if value.eq_ignore_ascii_case("NEVER") {
                        Some(Expire::Never)
                    } else {
                        value.parse().ok().map(Expire::Days)
                    }
                });
            }
            _ => {}
        }
    }

    /// SASL lists `mechanism`.
    pub fn sasl(&self, mechanism: &str) -> bool {
        self.sasl
            .as_ref()
            .is_some_and(|list| list.iter().any(|offered| offered == mechanism))
    }
}

/// A logged-in POP3 session.
pub struct Session {
    wire: Wire<Stream>,
    caps: Capabilities,
    mechanism: Mechanism,
    options: Options,
    /// LIST's sizes from the last listing.
    sizes: HashMap<u32, u64>,
    /// A reply was left unread or the connection failed: nothing more is sent.
    broken: bool,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("mechanism", &self.mechanism)
            .field("listed", &self.sizes.len())
            .field("broken", &self.broken)
            .finish_non_exhaustive()
    }
}

fn number(text: &[u8]) -> Option<u32> {
    std::str::from_utf8(text).ok()?.parse().ok()
}

/// The size at the start of a LIST value (more information may follow it, RFC 1939, 5).
fn octets(value: &[u8]) -> Result<u64, Pop3Error> {
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.split_ascii_whitespace().next()?.parse().ok())
        .ok_or(Pop3Error::Protocol("LIST without a size"))
}

/// `n value` from a UIDL or LIST line.
fn pair(line: &[u8]) -> Result<(u32, &[u8]), Pop3Error> {
    let line = line.trim_ascii();
    let space = line
        .iter()
        .position(u8::is_ascii_whitespace)
        .ok_or(Pop3Error::Protocol("a listing line without a value"))?;
    let n = number(&line[..space]).ok_or(Pop3Error::Protocol("a listing line without a number"))?;
    Ok((n, line[space..].trim_ascii()))
}

fn uidl(value: &[u8]) -> Result<String, Pop3Error> {
    let valid = !value.is_empty()
        && value.len() <= MAX_UIDL
        && value.iter().all(|byte| (0x21..=0x7e).contains(byte));
    if !valid {
        return Err(Pop3Error::Protocol("a UIDL that isn't printable ASCII"));
    }
    Ok(String::from_utf8_lossy(value).into_owned())
}

/// A refused UIDL: a server without it, unless the refusal says the server is busy.
fn no_uidl(refusal: &Refusal) -> Pop3Error {
    match refusal.command() {
        Pop3Error::Err => Pop3Error::NoUidl,
        busy => busy,
    }
}

impl Session {
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    pub fn mechanism(&self) -> Mechanism {
        self.mechanism
    }

    fn usable(&self) -> Result<(), Pop3Error> {
        if self.broken {
            Err(Pop3Error::ConnectionLost)
        } else {
            Ok(())
        }
    }

    /// Notes an error that leaves the connection unusable.
    fn note<T>(&mut self, result: Result<T, Pop3Error>) -> Result<T, Pop3Error> {
        if let Err(error) = &result
            && error.is_connection()
        {
            self.broken = true;
        }
        result
    }

    /// One command with a single-line answer: `+OK`'s text, or the refusal as a command's.
    async fn simple(&mut self, command: &[u8]) -> Result<Vec<u8>, Pop3Error> {
        self.usable()?;
        let result = async {
            self.wire.send(command).await?;
            match self.wire.status().await? {
                Status::Ok(text) => Ok(text),
                Status::Err(refusal) => Err(refusal.command()),
                Status::Continue => Err(Pop3Error::Protocol("a continuation outside AUTH")),
            }
        }
        .await;
        self.note(result)
    }

    /// One command with a multi-line answer, read line by line.
    async fn multi(
        &mut self,
        command: &[u8],
        refused: impl FnOnce(&Refusal) -> Pop3Error,
        each: impl FnMut(&[u8]) -> Result<(), Pop3Error>,
    ) -> Result<(), Pop3Error> {
        self.usable()?;
        let result = async {
            self.wire.send(command).await?;
            match self.wire.status().await? {
                Status::Ok(_) => self.wire.lines(LISTING_LINE, MAX_MESSAGES, each).await,
                Status::Err(refusal) => Err(refused(&refusal)),
                Status::Continue => Err(Pop3Error::Protocol("a continuation outside AUTH")),
            }
        }
        .await;
        self.note(result)
    }

    /// One command whose `+OK` is followed by data (RETR, TOP), at most `max` octets.
    async fn data(
        &mut self,
        command: &[u8],
        max: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, Pop3Error> {
        self.usable()?;
        let result = async {
            self.wire.send(command).await?;
            match self.wire.status().await? {
                Status::Ok(_) => self.wire.data(max, Some(deadline)).await,
                Status::Err(refusal) => Err(refusal.command()),
                Status::Continue => Err(Pop3Error::Protocol("a continuation outside AUTH")),
            }
        }
        .await;
        if matches!(result, Err(Pop3Error::TooLarge(_))) {
            self.broken = true;
        }
        self.note(result)
    }

    /// LIST's size for one message, from the last listing or LIST n.
    async fn size(&mut self, number: u32) -> Result<u64, Pop3Error> {
        if let Some(size) = self.sizes.get(&number) {
            return Ok(*size);
        }
        let text = self.simple(format!("LIST {number}").as_bytes()).await?;
        let size = octets(pair(&text)?.1)?;
        self.sizes.insert(number, size);
        Ok(size)
    }

    /// NOOP.
    pub async fn noop(&mut self) -> Result<(), Pop3Error> {
        self.simple(b"NOOP").await.map(drop)
    }

    /// RSET: every message marked deleted in this session is unmarked.
    pub async fn reset(&mut self) -> Result<(), Pop3Error> {
        self.simple(b"RSET").await.map(drop)
    }

    /// The login check's question: does the server number its messages for good (UIDL)?
    async fn check_uidl(&mut self) -> Result<(), Pop3Error> {
        if self.caps.uidl {
            return Ok(());
        }
        self.multi(b"UIDL", no_uidl, |_| Ok(())).await
    }

    /// QUIT, waiting at most `wait` for the answer; errors don't matter any more.
    async fn quit_within(mut self, wait: Duration) {
        if self.broken {
            return;
        }
        let _ = tokio::time::timeout(wait, async {
            self.wire.send(b"QUIT").await?;
            self.wire.status().await
        })
        .await;
    }
}

impl Mailbox for Session {
    async fn listing(&mut self) -> Result<Vec<Listing>, Pop3Error> {
        let started = std::time::Instant::now();
        let mut uidls: Vec<(u32, String)> = Vec::new();
        self.multi(b"UIDL", no_uidl, |line| {
            let (n, value) = pair(line)?;
            uidls.push((n, uidl(value)?));
            Ok(())
        })
        .await?;
        let mut sizes = HashMap::with_capacity(uidls.len());
        self.multi(b"LIST", Refusal::command, |line| {
            let (n, value) = pair(line)?;
            sizes.insert(n, octets(value)?);
            Ok(())
        })
        .await?;
        if sizes.len() != uidls.len() {
            return Err(Pop3Error::Protocol("UIDL and LIST disagree"));
        }
        let mut counts: HashMap<&str, usize> = HashMap::with_capacity(uidls.len());
        for (_, uidl) in &uidls {
            *counts.entry(uidl).or_default() += 1;
        }
        let mut listing = Vec::with_capacity(uidls.len());
        let mut duplicates = 0;
        for (number, uidl) in &uidls {
            let size = *sizes
                .get(number)
                .ok_or(Pop3Error::Protocol("UIDL and LIST disagree"))?;
            if counts[uidl.as_str()] > 1 {
                duplicates += 1;
                continue;
            }
            listing.push(Listing {
                number: *number,
                uidl: uidl.clone(),
                size,
            });
        }
        if duplicates > 0 {
            tracing::warn!(duplicates, "pop3 messages sharing a UIDL left out");
        }
        let octets: u64 = sizes.values().sum();
        self.sizes = sizes;
        tracing::debug!(
            messages = listing.len(),
            octets,
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "pop3 listed"
        );
        Ok(listing)
    }

    async fn retrieve(&mut self, number: u32) -> Result<Vec<u8>, Pop3Error> {
        let started = std::time::Instant::now();
        let size = self.size(number).await?;
        if size > self.options.max_message {
            tracing::debug!(octets = size, "pop3 message too large to retrieve");
            return Err(Pop3Error::TooLarge(size));
        }
        let deadline = Instant::now() + self.options.data_time(size);
        let max = usize::try_from(self.options.max_message).unwrap_or(usize::MAX);
        let message = self
            .data(format!("RETR {number}").as_bytes(), max, deadline)
            .await;
        match &message {
            Ok(message) => tracing::debug!(
                octets = message.len(),
                listed = size,
                ms = started.elapsed().as_secs_f64() * 1000.0,
                "pop3 retrieved"
            ),
            Err(error) => tracing::debug!(%error, listed = size, "pop3 retrieve failed"),
        }
        message
    }

    async fn delete(&mut self, number: u32) -> Result<(), Pop3Error> {
        self.simple(format!("DELE {number}").as_bytes()).await?;
        self.sizes.remove(&number);
        Ok(())
    }

    async fn quit(mut self) -> Result<(), Pop3Error> {
        self.usable()?;
        let started = std::time::Instant::now();
        let result = async {
            self.wire.send(b"QUIT").await?;
            match self.wire.status().await? {
                Status::Ok(_) => Ok(()),
                Status::Err(refusal) => Err(refusal.command()),
                Status::Continue => Err(Pop3Error::Protocol("a continuation outside AUTH")),
            }
        }
        .await;
        if result.is_ok() {
            let stream = self.wire.get_mut();
            let _ = tokio::time::timeout(Duration::from_secs(2), stream.shutdown()).await;
        }
        match &result {
            Ok(()) => tracing::debug!(ms = started.elapsed().as_secs_f64() * 1000.0, "pop3 quit"),
            Err(error) => tracing::debug!(%error, "pop3 quit failed"),
        }
        result
    }

    async fn header(&mut self, number: u32) -> Result<Vec<u8>, Pop3Error> {
        let deadline = Instant::now() + self.options.command_timeout;
        self.data(format!("TOP {number} 0").as_bytes(), MAX_HEADER, deadline)
            .await
    }
}

/// Connects (implicit TLS, or STLS that must succeed), reads CAPA and logs in. `tls` comes from
/// [`crate::net::Trust`].
pub async fn connect(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
) -> Result<Session, Pop3Error> {
    connect_with(endpoint, credentials, tls, Options::default()).await
}

/// [`connect`] with `options`.
pub async fn connect_with(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
    options: Options,
) -> Result<Session, Pop3Error> {
    let started = std::time::Instant::now();
    let session = tokio::time::timeout(
        options.connect_timeout,
        connect_inner(endpoint, credentials, tls, options),
    )
    .await
    .map_err(|_| Pop3Error::Timeout)??;
    tracing::debug!(
        mechanism = ?session.mechanism,
        stls = endpoint.security == Security::StartTls,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "pop3 connected"
    );
    Ok(session)
}

async fn connect_inner(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
    options: Options,
) -> Result<Session, Pop3Error> {
    let tcp = net::tcp(&endpoint.host, endpoint.port, options.connect_timeout).await?;
    let mut wire = match endpoint.security {
        Security::Tls => {
            let stream = net::tls(tcp, tls, &endpoint.host, options.connect_timeout).await?;
            let mut wire = Wire::new(stream, options.command_timeout);
            greeting(&mut wire).await?;
            wire
        }
        Security::StartTls => {
            let mut plain = Wire::new(tcp, options.command_timeout);
            greeting(&mut plain).await?;
            // STLS goes out whatever a plaintext CAPA would say (an attacker can strip it), and
            // anything but +OK ends the attempt: there is no plaintext path.
            plain.send(b"STLS").await?;
            match plain.status().await? {
                Status::Ok(_) => {}
                Status::Err(_) | Status::Continue => return Err(Pop3Error::StartTls),
            }
            let tcp = plain.into_clean_inner()?;
            let stream = net::tls(tcp, tls, &endpoint.host, options.connect_timeout).await?;
            Wire::new(stream, options.command_timeout)
        }
    };
    let before = capabilities(&mut wire).await?;
    let mechanism = login(&mut wire, before.as_ref(), credentials).await?;
    let after = capabilities(&mut wire).await?;
    Ok(Session {
        wire,
        caps: after.or(before).unwrap_or_default(),
        mechanism,
        options,
        sizes: HashMap::new(),
        broken: false,
    })
}

async fn greeting<S>(wire: &mut Wire<S>) -> Result<(), Pop3Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match wire.status().await? {
        Status::Ok(_) => Ok(()),
        Status::Err(refusal) => Err(refusal.command()),
        Status::Continue => Err(Pop3Error::Protocol("unexpected greeting")),
    }
}

/// CAPA; `None` when the server doesn't know it.
async fn capabilities(wire: &mut Wire<Stream>) -> Result<Option<Capabilities>, Pop3Error> {
    wire.send(b"CAPA").await?;
    match wire.status().await? {
        Status::Ok(_) => {}
        Status::Err(_) => return Ok(None),
        Status::Continue => return Err(Pop3Error::Protocol("a continuation outside AUTH")),
    }
    let mut caps = Capabilities::default();
    wire.lines(LISTING_LINE, CAPA_LINES, |line| {
        caps.read(line);
        Ok(())
    })
    .await?;
    Ok(Some(caps))
}

/// Logs in on the TLS session: SASL PLAIN where offered; else USER and PASS where offered or
/// when the server names nothing; XOAUTH2 for a token unless SASL is listed without it.
async fn login(
    wire: &mut Wire<Stream>,
    caps: Option<&Capabilities>,
    credentials: Credentials<'_>,
) -> Result<Mechanism, Pop3Error> {
    let delay = caps.and_then(|caps| caps.login_delay);
    match credentials {
        Credentials::Password { username, password } => {
            if [username, password]
                .iter()
                .any(|text| text.contains(['\r', '\n', '\0']))
            {
                return Err(Pop3Error::Rejected(LoginFailure::WrongPassword));
            }
            let sasl_listed = caps.is_some_and(|caps| caps.sasl.is_some());
            if caps.is_some_and(|caps| caps.sasl("PLAIN")) {
                let response = Zeroizing::new(format!("\0{username}\0{password}"));
                authenticate(wire, "PLAIN", response.as_bytes(), delay, false).await?;
                Ok(Mechanism::Plain)
            } else if caps.is_none_or(|caps| caps.user || !sasl_listed) {
                user_pass(wire, username, password, delay).await?;
                Ok(Mechanism::User)
            } else if caps.is_some_and(|caps| caps.sasl("XOAUTH2") || caps.sasl("OAUTHBEARER")) {
                Err(Pop3Error::Rejected(LoginFailure::OAuthRequired))
            } else {
                Err(Pop3Error::NoMechanism)
            }
        }
        Credentials::OAuth2 { username, token } => {
            let listed = caps.and_then(|caps| caps.sasl.as_ref());
            if listed.is_some_and(|list| !list.iter().any(|m| m == "XOAUTH2")) {
                return Err(Pop3Error::NoMechanism);
            }
            let response = xoauth2_response(username, token);
            authenticate(wire, "XOAUTH2", &response, delay, true).await?;
            Ok(Mechanism::XOAuth2)
        }
    }
}

/// AUTH (RFC 5034) with an initial response when the command fits in 255 octets, else after
/// the server's empty challenge. A later challenge is XOAUTH2's error report, answered with an
/// empty line (`answer_error`), or cancelled with `*`; the refusal follows either way.
async fn authenticate(
    wire: &mut Wire<Stream>,
    mechanism: &str,
    response: &[u8],
    delay: Option<Duration>,
    answer_error: bool,
) -> Result<(), Pop3Error> {
    let encoded = Zeroizing::new(STANDARD.encode(response));
    let command = Zeroizing::new(format!("AUTH {mechanism} {}", encoded.as_str()));
    if command.len() + 2 <= AUTH_LINE {
        wire.send(command.as_bytes()).await?;
    } else {
        wire.send(format!("AUTH {mechanism}").as_bytes()).await?;
        match wire.status().await? {
            Status::Continue => wire.send(encoded.as_bytes()).await?,
            Status::Err(refusal) => return Err(refusal.login(delay)),
            Status::Ok(_) => return Err(Pop3Error::Protocol("AUTH accepted without credentials")),
        }
    }
    drop((command, encoded));
    match wire.status().await? {
        Status::Ok(_) => Ok(()),
        Status::Err(refusal) => Err(refusal.login(delay)),
        Status::Continue => {
            let answer: &[u8] = if answer_error { b"" } else { b"*" };
            wire.send(answer).await?;
            match wire.status().await? {
                Status::Err(refusal) => Err(refusal.login(delay)),
                _ => Err(Pop3Error::Protocol("an unexpected SASL challenge")),
            }
        }
    }
}

async fn user_pass(
    wire: &mut Wire<Stream>,
    username: &str,
    password: &str,
    delay: Option<Duration>,
) -> Result<(), Pop3Error> {
    for line in [
        Zeroizing::new(format!("USER {username}")),
        Zeroizing::new(format!("PASS {password}")),
    ] {
        wire.send(line.as_bytes()).await?;
        match wire.status().await? {
            Status::Ok(_) => {}
            Status::Err(refusal) => return Err(refusal.login(delay)),
            Status::Continue => return Err(Pop3Error::Protocol("a continuation outside AUTH")),
        }
    }
    Ok(())
}

/// The setup assistant's login check: connect, log in, make sure the server has UIDL (CAPA, or
/// UIDL itself), QUIT. Nothing is retrieved or deleted.
pub async fn check_login(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
) -> Result<(), LoginFailure> {
    let result = async {
        let mut session = connect(endpoint, credentials, tls).await?;
        session.check_uidl().await?;
        session.quit_within(CHECK_QUIT).await;
        Ok::<_, Pop3Error>(())
    }
    .await;
    result.map_err(|error| {
        tracing::debug!(%error, "pop3 login check failed");
        error.failure()
    })
}

#[cfg(test)]
mod tests {
    use super::super::Busy;
    use super::*;

    #[test]
    fn capa_lines_are_read() {
        let mut caps = Capabilities::default();
        for line in [
            &b"TOP"[..],
            b"uidl",
            b"USER",
            b"SASL PLAIN oauthbearer XOAUTH2",
            b"RESP-CODES",
            b"PIPELINING",
            b"LOGIN-DELAY 900 USER",
            b"EXPIRE NEVER",
            b"IMPLEMENTATION Example Server 1.0",
            b"",
        ] {
            caps.read(line);
        }
        assert!(caps.top && caps.uidl && caps.user && caps.pipelining && caps.resp_codes);
        assert!(!caps.stls);
        assert!(caps.sasl("PLAIN") && caps.sasl("OAUTHBEARER") && !caps.sasl("LOGIN"));
        assert_eq!(caps.login_delay, Some(Duration::from_secs(900)));
        assert_eq!(caps.expire, Some(Expire::Never));
        caps.read(b"EXPIRE 0");
        assert_eq!(caps.expire, Some(Expire::Days(0)));
        assert!(!format!("{caps:?}").contains("Example"));
    }

    #[test]
    fn listing_lines_and_uidls_are_checked() {
        assert_eq!(pair(b"1 abc").unwrap(), (1, &b"abc"[..]));
        assert_eq!(pair(b" 12   345 ").unwrap(), (12, &b"345"[..]));
        assert!(pair(b"x abc").is_err());
        assert!(pair(b"12").is_err());
        assert!(uidl(b"000000016ac391ba").is_ok());
        assert!(uidl(&[b'a'; 255]).is_ok());
        for bad in [&b""[..], &[b'a'; 256], b"a b", b"\xc3\xa6", b"a\x7f"] {
            assert!(uidl(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_message_has_time_in_proportion_to_its_size() {
        let options = Options::default();
        assert_eq!(options.data_time(0), COMMAND_TIMEOUT);
        assert_eq!(
            options.data_time(16 * 1024 * 1024),
            COMMAND_TIMEOUT + Duration::from_secs(1024)
        );
    }

    #[test]
    fn errors_say_what_the_user_is_told() {
        assert_eq!(Pop3Error::StartTls.failure(), LoginFailure::NoTls);
        assert_eq!(Pop3Error::Timeout.failure(), LoginFailure::Timeout);
        assert_eq!(Pop3Error::Busy(Busy::InUse).failure(), LoginFailure::Other);
        assert_eq!(
            Pop3Error::Rejected(LoginFailure::AppPasswordRequired).failure(),
            LoginFailure::AppPasswordRequired
        );
        assert_eq!(Pop3Error::NoUidl.failure(), LoginFailure::Other);
        assert!(Pop3Error::ConnectionLost.is_connection());
        assert!(!Pop3Error::Err.is_connection());
        assert!(!Pop3Error::TooLarge(1).is_connection());
        assert!(!Pop3Error::Busy(Busy::Temporary).is_connection());
    }
}
