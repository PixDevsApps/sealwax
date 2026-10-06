//! Submitting a message (ML-075; ADR-007, and ADR-015's at-most-once send) on lettre's
//! connection, command by command, so that the caller can make a durable mark between the
//! envelope and the data:
//!
//! 1. [`Submitter::connect`] (or [`connect_oauth`], which refreshes a refused token once):
//!    TLS, `EHLO [127.0.0.1]`, the extensions, AUTH. Nothing of the message has been sent.
//! 2. [`Submitter::prepare`]: the checks that need no server (bare CR or LF, addresses,
//!    SMTPUTF8 and 8BITMIME where needed, EHLO's SIZE), then `MAIL FROM` with `SIZE=` and every
//!    `RCPT TO`. A refused recipient refuses the whole send (`RSET`), so the user fixes the
//!    address instead of some recipients getting the message and others not.
//! 3. The caller marks the send as started, durably.
//! 4. [`Prepared::send`]: `DATA`, the bytes (dot-stuffed) and the end of data. Only a missing
//!    answer to the end of data leaves the outcome unknown ([`SubmitError::Uncertain`]); every
//!    reply the server sends is definite, and a connection lost before the content went out
//!    delivered nothing.
//!
//! Errors and logs carry reply codes, counts and recipient indices, never an address, the
//! server's text or the message.

use super::{Extensions, Line, authenticate, extensions, open, quit};
use crate::api::SendError;
use crate::imap::Credentials;
use crate::net::Trust;
use crate::oauth::Fetched;
use lettre::transport::smtp::client::AsyncSmtpConnection;
use lettre::transport::smtp::commands::{Data, Rset};
use lettre::transport::smtp::response::Response;
use sealwax_domain::account::{Endpoint, LoginFailure};
use std::fmt;
use std::future::Future;
use std::time::{Duration, Instant};

/// How long each step may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// TCP, TLS, EHLO and AUTH, each.
    pub connect: Duration,
    /// A command's reply: MAIL, RCPT, DATA, RSET.
    pub command: Duration,
    /// The message's bytes and the reply to their end (RFC 5321 4.5.3.2 asks clients to wait 10
    /// minutes for it; past this the outcome is unknown).
    pub data: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(30),
            command: Duration::from_secs(60),
            data: Duration::from_secs(300),
        }
    }
}

/// Who the message goes from and to, as SMTP's envelope says (RFC 5321): the sender's address
/// and every recipient, Bcc included. Plain addresses, without names or angle brackets. Its
/// `Debug` prints only how many.
#[derive(Clone, Copy)]
pub struct Envelope<'a> {
    pub from: &'a str,
    pub to: &'a [String],
}

impl fmt::Debug for Envelope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Envelope")
            .field("recipients", &self.to.len())
            .finish_non_exhaustive()
    }
}

/// Why a message wasn't submitted, or may have been. Indices name recipients of the
/// envelope's `to`; nothing here holds an address or the server's text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// The server couldn't be reached, secured or greeted. Nothing was sent.
    #[error("the submission server couldn't be reached or secured")]
    Connect(LoginFailure),
    /// The server refused the login. Nothing was sent.
    #[error("the submission server refused the login")]
    Auth(LoginFailure),
    /// The message has a bare CR, a bare LF or a NUL (an SMTP smuggling vector), or no
    /// recipients.
    #[error("the message can't be sent as it is")]
    Malformed,
    /// Addresses that can't go into an envelope: no `@`, spaces, controls or angle brackets.
    #[error("addresses that can't be sent")]
    InvalidAddress {
        sender: bool,
        recipients: Vec<usize>,
    },
    /// Addresses with non-ASCII local parts, and the server lacks SMTPUTF8.
    #[error("addresses that need SMTPUTF8, which the server lacks")]
    NeedsSmtpUtf8 {
        sender: bool,
        recipients: Vec<usize>,
    },
    /// Message bytes above 127, and the server lacks 8BITMIME.
    #[error("8-bit message bytes, which the server doesn't take")]
    NeedsEightBit,
    /// Larger than the server takes: EHLO's SIZE, or a 552 reply (with SIZE's limit when the
    /// server named one).
    #[error("the message is larger than the server takes")]
    TooLarge { limit: Option<u64> },
    /// The server refused these recipients for good (5xx to RCPT); the transaction was reset
    /// and nothing was sent.
    #[error("the server refused {} recipients", recipients.len())]
    RecipientsRefused { recipients: Vec<usize> },
    /// The server refused the sender or the message for good (5xx).
    #[error("the server refused the message (reply {code})")]
    Refused { code: u16 },
    /// Nothing was delivered, and trying again later may work: a 4xx reply, a lost connection
    /// or a timeout before the message's end was sent.
    #[error("the message wasn't sent; try again later")]
    Transient,
    /// The end of the message was sent and no reply came: it may have been delivered. Never
    /// sent again without the user (ADR-015).
    #[error("the message may have been sent")]
    Uncertain,
}

/// What the outbox does after a failed submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitFailure {
    /// Nothing was delivered; send again later (offline, a timeout, a 4xx reply).
    Retry,
    /// The account's login needs the user: a refused password or token, a certificate that
    /// doesn't verify, a server without TLS. The send waits.
    Account,
    /// It can't be sent as it is: back to the user as a draft whose send failed.
    Refuse,
    /// It may have been delivered: never sent again without the user.
    Uncertain,
}

impl SubmitError {
    pub fn failure(&self) -> SubmitFailure {
        match self {
            SubmitError::Connect(LoginFailure::Certificate | LoginFailure::NoTls) => {
                SubmitFailure::Account
            }
            SubmitError::Connect(_) => SubmitFailure::Retry,
            SubmitError::Auth(
                LoginFailure::WrongPassword
                | LoginFailure::OAuthRequired
                | LoginFailure::AppPasswordRequired
                | LoginFailure::Certificate
                | LoginFailure::NoTls,
            ) => SubmitFailure::Account,
            SubmitError::Auth(_) | SubmitError::Transient => SubmitFailure::Retry,
            SubmitError::Uncertain => SubmitFailure::Uncertain,
            SubmitError::Malformed
            | SubmitError::InvalidAddress { .. }
            | SubmitError::NeedsSmtpUtf8 { .. }
            | SubmitError::NeedsEightBit
            | SubmitError::TooLarge { .. }
            | SubmitError::RecipientsRefused { .. }
            | SubmitError::Refused { .. } => SubmitFailure::Refuse,
        }
    }

    /// The error the app hears (`Event::SendFailed`), with the refused recipients' addresses
    /// taken from the envelope's `to`.
    pub fn send_error(&self, to: &[String]) -> SendError {
        let named = |indices: &[usize]| -> Vec<String> {
            indices.iter().filter_map(|i| to.get(*i).cloned()).collect()
        };
        match self {
            SubmitError::Connect(LoginFailure::Unreachable | LoginFailure::Timeout)
            | SubmitError::Transient => SendError::Offline,
            SubmitError::Auth(
                LoginFailure::WrongPassword
                | LoginFailure::OAuthRequired
                | LoginFailure::AppPasswordRequired,
            ) => SendError::NeedsSignIn,
            SubmitError::InvalidAddress {
                sender: false,
                recipients,
            }
            | SubmitError::NeedsSmtpUtf8 {
                sender: false,
                recipients,
            }
            | SubmitError::RecipientsRefused { recipients } => {
                SendError::RecipientsRefused(named(recipients))
            }
            SubmitError::TooLarge { limit: Some(limit) } => SendError::TooLarge { limit: *limit },
            SubmitError::InvalidAddress { .. }
            | SubmitError::NeedsSmtpUtf8 { .. }
            | SubmitError::NeedsEightBit
            | SubmitError::Malformed
            | SubmitError::TooLarge { limit: None }
            | SubmitError::Refused { .. } => SendError::Refused,
            SubmitError::Connect(_) | SubmitError::Auth(_) | SubmitError::Uncertain => {
                SendError::Other
            }
        }
    }
}

/// How a command was answered when it wasn't with a positive reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// A 4xx or 5xx reply.
    Code(u16),
    /// No reply: the connection failed, timed out or answered something unreadable.
    Lost,
}

/// Sends `command` and waits `timeout` for its reply.
async fn ask(
    conn: &mut AsyncSmtpConnection,
    command: impl fmt::Display,
    timeout: Duration,
) -> Result<Response, Answer> {
    match tokio::time::timeout(timeout, conn.command(command)).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(error
            .status()
            .map_or(Answer::Lost, |code| Answer::Code(u16::from(code)))),
        Err(_) => Err(Answer::Lost),
    }
}

/// A path for MAIL FROM or RCPT TO: the address, with a non-ASCII domain as its A-label, and
/// whether its local part needs SMTPUTF8.
struct Path {
    text: String,
    utf8: bool,
}

fn path(address: &str) -> Option<Path> {
    let address = address.trim();
    let (local, domain) = address.rsplit_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || address
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>'))
    {
        return None;
    }
    let domain = if domain.is_ascii() {
        domain.to_owned()
    } else {
        idna::domain_to_ascii(domain).ok()?
    };
    Some(Path {
        text: format!("{local}@{domain}"),
        utf8: !local.is_ascii(),
    })
}

/// A CR not followed by LF, an LF not preceded by CR, or a NUL.
fn malformed(message: &[u8]) -> bool {
    message.iter().enumerate().any(|(i, byte)| match byte {
        b'\r' => message.get(i + 1) != Some(&b'\n'),
        b'\n' => i == 0 || message[i - 1] != b'\r',
        0 => true,
        _ => false,
    })
}

/// The server's size limit for the composer's check (`Command::SubmissionLimit`): TLS and EHLO
/// only, no login. `None` when the server names no limit before login.
pub async fn submission_limit(
    endpoint: &Endpoint,
    trust: &Trust,
    timeouts: Timeouts,
) -> Result<Option<u64>, SubmitError> {
    let mut conn = tokio::time::timeout(timeouts.connect, open(endpoint, trust, timeouts.connect))
        .await
        .map_err(|_| SubmitError::Connect(LoginFailure::Timeout))?
        .map_err(SubmitError::Connect)?;
    let found = tokio::time::timeout(timeouts.command, extensions(&mut conn))
        .await
        .map_err(|_| SubmitError::Connect(LoginFailure::Timeout))?
        .map_err(|error| SubmitError::Connect(super::failure(&error)))?;
    quit(conn).await;
    tracing::debug!(limit = ?found.size, "smtp limit read");
    Ok(found.size)
}

/// A logged-in submission session.
pub struct Submitter {
    /// `None` once a failure left the connection unusable.
    conn: Option<AsyncSmtpConnection>,
    extensions: Extensions,
    timeouts: Timeouts,
    /// A transaction was started and not finished: the next one begins with RSET.
    open: bool,
}

impl fmt::Debug for Submitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Submitter")
            .field("extensions", &self.extensions)
            .field("connected", &self.conn.is_some())
            .finish_non_exhaustive()
    }
}

impl Submitter {
    /// Connects, secures, reads the extensions and logs in, with the default timeouts.
    pub async fn connect(
        endpoint: &Endpoint,
        credentials: Credentials<'_>,
        trust: &Trust,
    ) -> Result<Submitter, SubmitError> {
        Submitter::connect_with(endpoint, credentials, trust, Timeouts::default()).await
    }

    /// [`Submitter::connect`] with `timeouts`.
    pub async fn connect_with(
        endpoint: &Endpoint,
        credentials: Credentials<'_>,
        trust: &Trust,
        timeouts: Timeouts,
    ) -> Result<Submitter, SubmitError> {
        let started = Instant::now();
        let result = Submitter::log_in(endpoint, credentials, trust, timeouts).await;
        match &result {
            Ok(submitter) => tracing::debug!(
                limit = ?submitter.extensions.size,
                ms = started.elapsed().as_secs_f64() * 1000.0,
                "smtp connected"
            ),
            Err(error) => tracing::debug!(?error, "smtp connect failed"),
        }
        result
    }

    async fn log_in(
        endpoint: &Endpoint,
        credentials: Credentials<'_>,
        trust: &Trust,
        timeouts: Timeouts,
    ) -> Result<Submitter, SubmitError> {
        let timeout = || SubmitError::Connect(LoginFailure::Timeout);
        let mut conn =
            tokio::time::timeout(timeouts.connect, open(endpoint, trust, timeouts.connect))
                .await
                .map_err(|_| timeout())?
                .map_err(SubmitError::Connect)?;
        let extensions = tokio::time::timeout(timeouts.command, extensions(&mut conn))
            .await
            .map_err(|_| timeout())?
            .map_err(|error| SubmitError::Connect(super::failure(&error)))?;
        tokio::time::timeout(timeouts.connect, authenticate(&mut conn, credentials))
            .await
            .map_err(|_| timeout())?
            .map_err(|failure| match failure {
                LoginFailure::Unreachable | LoginFailure::Timeout => SubmitError::Connect(failure),
                _ => SubmitError::Auth(failure),
            })?;
        Ok(Submitter {
            conn: Some(conn),
            extensions,
            timeouts,
            open: false,
        })
    }

    /// What the server's EHLO named after TLS.
    pub fn extensions(&self) -> Extensions {
        self.extensions
    }

    /// The envelope: checks what needs no server, then `MAIL FROM` and every `RCPT TO`. On
    /// success the server has accepted the sender and every recipient and nothing of the
    /// message was sent; the caller marks the send as started and then calls
    /// [`Prepared::send`]. On failure nothing was sent.
    pub async fn prepare<'s, 'm>(
        &'s mut self,
        envelope: Envelope<'_>,
        message: &'m [u8],
    ) -> Result<Prepared<'s, 'm>, SubmitError> {
        let started = Instant::now();
        match self.envelope(envelope, message).await {
            Ok(()) => {
                tracing::debug!(
                    recipients = envelope.to.len(),
                    bytes = message.len(),
                    ms = started.elapsed().as_secs_f64() * 1000.0,
                    "smtp envelope accepted"
                );
                Ok(Prepared {
                    submitter: self,
                    message,
                })
            }
            Err(error) => {
                tracing::debug!(
                    recipients = envelope.to.len(),
                    ?error,
                    "smtp envelope not accepted"
                );
                Err(error)
            }
        }
    }

    /// Drops the connection after a failure that left it in an unknown state.
    fn lost(&mut self) -> SubmitError {
        self.conn = None;
        SubmitError::Transient
    }

    /// RSET, ending a transaction the server may still hold.
    async fn reset(&mut self) -> Result<(), SubmitError> {
        let timeout = self.timeouts.command;
        let conn = self.conn.as_mut().ok_or(SubmitError::Transient)?;
        match ask(conn, Rset, timeout).await {
            Ok(_) => {
                self.open = false;
                Ok(())
            }
            Err(_) => Err(self.lost()),
        }
    }

    async fn envelope(
        &mut self,
        envelope: Envelope<'_>,
        message: &[u8],
    ) -> Result<(), SubmitError> {
        if envelope.to.is_empty() || malformed(message) {
            return Err(SubmitError::Malformed);
        }
        let from = path(envelope.from);
        let to: Vec<Option<Path>> = envelope.to.iter().map(|address| path(address)).collect();
        let invalid: Vec<usize> = (0..to.len()).filter(|i| to[*i].is_none()).collect();
        let (Some(from), true) = (from, invalid.is_empty()) else {
            return Err(SubmitError::InvalidAddress {
                sender: path(envelope.from).is_none(),
                recipients: invalid,
            });
        };
        let to: Vec<Path> = to.into_iter().flatten().collect();
        let utf8 = from.utf8 || to.iter().any(|path| path.utf8);
        if utf8 && !self.extensions.smtputf8 {
            return Err(SubmitError::NeedsSmtpUtf8 {
                sender: from.utf8,
                recipients: (0..to.len()).filter(|i| to[*i].utf8).collect(),
            });
        }
        let eight_bit = !message.is_ascii();
        if eight_bit && !self.extensions.eight_bit {
            return Err(SubmitError::NeedsEightBit);
        }
        let size = message.len() as u64;
        if let Some(limit) = self.extensions.size
            && size > limit
        {
            return Err(SubmitError::TooLarge { limit: Some(limit) });
        }
        if self.conn.is_none() {
            return Err(SubmitError::Transient);
        }
        if self.open {
            self.reset().await?;
        }
        let mut parameters = String::new();
        if self.extensions.size.is_some() {
            parameters.push_str(&format!(" SIZE={size}"));
        }
        if eight_bit {
            parameters.push_str(" BODY=8BITMIME");
        }
        if utf8 {
            parameters.push_str(" SMTPUTF8");
        }
        let timeout = self.timeouts.command;
        let limit = self.extensions.size;
        let mail = format!("MAIL FROM:<{}>{parameters}", from.text);
        let conn = self.conn.as_mut().ok_or(SubmitError::Transient)?;
        self.open = true;
        match ask(conn, Line(&mail), timeout).await {
            Ok(_) => {}
            Err(Answer::Code(552)) => {
                self.reset().await?;
                return Err(SubmitError::TooLarge { limit });
            }
            Err(Answer::Code(code)) if code < 500 => {
                self.reset().await?;
                return Err(SubmitError::Transient);
            }
            Err(Answer::Code(code)) => {
                self.reset().await?;
                return Err(SubmitError::Refused { code });
            }
            Err(Answer::Lost) => return Err(self.lost()),
        }
        let mut refused = Vec::new();
        let mut deferred = false;
        for (index, recipient) in to.iter().enumerate() {
            let rcpt = format!("RCPT TO:<{}>", recipient.text);
            let conn = self.conn.as_mut().ok_or(SubmitError::Transient)?;
            match ask(conn, Line(&rcpt), timeout).await {
                Ok(_) => {}
                // RFC 5321 4.5.3.1.10: a 552 to RCPT is to be taken as 452.
                Err(Answer::Code(code)) if code < 500 || code == 552 => deferred = true,
                Err(Answer::Code(_)) => refused.push(index),
                Err(Answer::Lost) => return Err(self.lost()),
            }
        }
        if !refused.is_empty() {
            self.reset().await?;
            return Err(SubmitError::RecipientsRefused {
                recipients: refused,
            });
        }
        if deferred {
            self.reset().await?;
            return Err(SubmitError::Transient);
        }
        Ok(())
    }

    /// QUIT, waiting a moment at most for its reply.
    pub async fn quit(self) {
        if let Some(conn) = self.conn {
            quit(conn).await;
        }
    }
}

/// An envelope the server accepted, waiting for its message. Dropped without [`Prepared::send`]
/// or [`Prepared::cancel`], the next [`Submitter::prepare`] resets the transaction first.
pub struct Prepared<'s, 'm> {
    submitter: &'s mut Submitter,
    message: &'m [u8],
}

impl fmt::Debug for Prepared<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prepared")
            .field("bytes", &self.message.len())
            .finish_non_exhaustive()
    }
}

impl Prepared<'_, '_> {
    /// `DATA`, the message and its end. `Ok` when the server took it for delivery. A reply
    /// refusing `DATA` or the message is definite (nothing delivered); only a missing reply to
    /// the message's end, or a connection lost while the message was going out, is
    /// [`SubmitError::Uncertain`].
    pub async fn send(self) -> Result<(), SubmitError> {
        let started = Instant::now();
        let bytes = self.message.len();
        let result = self.data().await;
        match &result {
            Ok(()) => tracing::debug!(
                bytes,
                ms = started.elapsed().as_secs_f64() * 1000.0,
                "smtp message accepted"
            ),
            Err(error) => tracing::debug!(bytes, ?error, "smtp message not accepted"),
        }
        result
    }

    async fn data(self) -> Result<(), SubmitError> {
        let Prepared { submitter, message } = self;
        let timeouts = submitter.timeouts;
        let limit = submitter.extensions.size;
        let conn = submitter.conn.as_mut().ok_or(SubmitError::Transient)?;
        match ask(conn, Data, timeouts.command).await {
            Ok(response) if response.has_code(354) => {}
            // Nothing of the message went out, so nothing can have been delivered.
            Ok(_) | Err(Answer::Lost) => return Err(submitter.lost()),
            Err(Answer::Code(552)) => {
                submitter.reset().await?;
                return Err(SubmitError::TooLarge { limit });
            }
            Err(Answer::Code(code)) if code < 500 => {
                submitter.reset().await?;
                return Err(SubmitError::Transient);
            }
            Err(Answer::Code(code)) => {
                submitter.reset().await?;
                return Err(SubmitError::Refused { code });
            }
        }
        // lettre ends the data with CRLF . CRLF; the message's own final CRLF is that first
        // CRLF, so the server stores exactly these bytes.
        let body = message.strip_suffix(b"\r\n").unwrap_or(message);
        let conn = submitter.conn.as_mut().ok_or(SubmitError::Uncertain)?;
        let reply = tokio::time::timeout(timeouts.data, conn.message(body)).await;
        submitter.open = false;
        match reply {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => match error.status().map(u16::from) {
                Some(552) => Err(SubmitError::TooLarge { limit }),
                Some(code) if code < 500 => Err(SubmitError::Transient),
                Some(code) => Err(SubmitError::Refused { code }),
                None => {
                    submitter.conn = None;
                    Err(SubmitError::Uncertain)
                }
            },
            Err(_) => {
                submitter.conn = None;
                Err(SubmitError::Uncertain)
            }
        }
    }

    /// Gives the envelope up: RSET, and nothing is sent.
    pub async fn cancel(self) -> Result<(), SubmitError> {
        self.submitter.reset().await
    }
}

/// [`Submitter::connect_with`] with an OAuth2 access token from `token`, called with `renew`
/// false first. When the server refuses a token that didn't come from a refresh just now, a
/// new one is asked for (`renew` true) and the login tried once more (ADR-007); a refused
/// fresh token means the account must sign in again (`OAuthRequired`).
pub async fn connect_oauth<F, Fut>(
    endpoint: &Endpoint,
    username: &str,
    trust: &Trust,
    timeouts: Timeouts,
    mut token: F,
) -> Result<Submitter, SubmitError>
where
    F: FnMut(bool) -> Fut,
    Fut: Future<Output = Result<Fetched, LoginFailure>>,
{
    let mut renew = false;
    loop {
        let fetched = token(renew).await.map_err(|failure| match failure {
            LoginFailure::Unreachable | LoginFailure::Timeout => SubmitError::Connect(failure),
            _ => SubmitError::Auth(failure),
        })?;
        let credentials = Credentials::OAuth2 {
            username,
            token: &fetched.token,
        };
        match Submitter::connect_with(endpoint, credentials, trust, timeouts).await {
            Ok(submitter) => return Ok(submitter),
            Err(SubmitError::Auth(_)) if !fetched.refreshed && !renew => {
                tracing::debug!("the submission server refused the access token; refreshing it");
                renew = true;
            }
            Err(SubmitError::Auth(LoginFailure::WrongPassword)) => {
                return Err(SubmitError::Auth(LoginFailure::OAuthRequired));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests;
