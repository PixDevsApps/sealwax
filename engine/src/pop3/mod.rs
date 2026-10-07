//! POP3 (M8, ML-087; RFC 1939, 2449, 2595, 3206, 5034): a session over `crate::net`'s sockets
//! and TLS that only ever authenticates over TLS (implicit TLS on 995, or STLS on 110 that must
//! succeed). A POP3 account's actor works through [`Mailbox`], so it can be tested with an
//! in-memory mailbox.
//!
//! - **Connect:** TLS from the first byte, or the greeting and STLS as the only plaintext; a
//!   refused STLS or anything the server sent after its `+OK` ends the attempt. CAPA over TLS
//!   before the login and again after it.
//! - **Login:** SASL PLAIN (an initial response where it fits), else USER and PASS where the
//!   server offers them or says nothing; XOAUTH2 for a token. Refusals are classified by RFC 2449
//!   and RFC 3206 codes, then by provider texts, and the text is dropped.
//! - **The maildrop:** UIDL and LIST for the listing, RETR with dot-unstuffing and the line ends
//!   as sent, TOP for a header, DELE, RSET, NOOP and QUIT. Every reply has a timeout, a message's
//!   data one proportionate to its size, and messages over [`MAX_MESSAGE`] aren't downloaded.
//!   Marked messages are deleted only when QUIT succeeds; a session dropped without it deletes
//!   nothing.
//! - **Logs** carry counts, sizes, timings and the mechanism, never a username, a UIDL, message
//!   text or a protocol line (ADR-018).

mod reply;
mod session;
mod wire;

pub use crate::imap::Credentials;
pub use session::{
    COMMAND_TIMEOUT, CONNECT_TIMEOUT, Capabilities, Expire, MAX_MESSAGE, MIN_RATE, Mechanism,
    Options, Session, check_login, connect, connect_with,
};

use crate::net::ConnectError;
use sealwax_domain::account::LoginFailure;
use std::future::Future;
use std::time::Duration;

/// One message on the server, as UIDL and LIST give it.
#[derive(Clone, PartialEq, Eq)]
pub struct Listing {
    /// The message's number in this session (1-based; it changes between sessions).
    pub number: u32,
    /// The server's unique id for the message, stable across sessions (UIDL).
    pub uidl: String,
    /// Its size in octets, as LIST says.
    pub size: u64,
}

impl std::fmt::Debug for Listing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listing")
            .field("number", &self.number)
            .field("uidl", &self.uidl.len())
            .field("size", &self.size)
            .finish()
    }
}

/// Why the server can't open the maildrop now (RFC 2449, RFC 3206): worth trying again later
/// without the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// Another session holds the maildrop (`[IN-USE]`): another mail app is downloading.
    InUse,
    /// The last login was too recent (`[LOGIN-DELAY]`), with the least time between logins
    /// when the server's CAPA names one.
    LoginDelay(Option<Duration>),
    /// A temporary problem on the server (`[SYS/TEMP]`).
    Temporary,
}

/// What went wrong. Neither the display nor the debug form carries server text: a reply can
/// hold the username.
#[derive(Debug, thiserror::Error)]
pub enum Pop3Error {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error("the server refused STLS")]
    StartTls,
    #[error("the server refused the login")]
    Rejected(LoginFailure),
    #[error("the server can't open the maildrop now ({0:?})")]
    Busy(Busy),
    #[error("the server offers no authentication Sealwax can use over TLS")]
    NoMechanism,
    #[error("the server offers no UIDL")]
    NoUidl,
    #[error("the server answered -ERR")]
    Err,
    #[error("the connection was lost")]
    ConnectionLost,
    #[error("timed out waiting for the server")]
    Timeout,
    #[error("protocol error: {0}")]
    Protocol(&'static str),
    /// The message is larger than Sealwax downloads. When LIST's size said so, nothing was sent
    /// and the session goes on; when the data outgrew the limit, the session is closed.
    #[error("the message is too large ({0} octets)")]
    TooLarge(u64),
}

impl Pop3Error {
    /// What the setup assistant or the account's status tells the user. A busy maildrop is
    /// `Busy`, which the account's status treats as offline and retries.
    pub fn failure(&self) -> LoginFailure {
        match self {
            Pop3Error::Connect(error) => error.failure(),
            Pop3Error::StartTls => LoginFailure::NoTls,
            Pop3Error::Rejected(failure) => *failure,
            Pop3Error::Timeout => LoginFailure::Timeout,
            Pop3Error::NoUidl => LoginFailure::NoUidl,
            Pop3Error::Busy(_) => LoginFailure::Busy,
            _ => LoginFailure::Other,
        }
    }

    /// The connection can't be used any more and must be replaced.
    pub fn is_connection(&self) -> bool {
        matches!(
            self,
            Pop3Error::Connect(_)
                | Pop3Error::StartTls
                | Pop3Error::ConnectionLost
                | Pop3Error::Timeout
                | Pop3Error::Protocol(_)
        )
    }
}

/// A POP3 maildrop in the TRANSACTION state.
pub trait Mailbox: Send {
    /// Every message with its UIDL and size (UIDL and LIST), in the server's order. A UIDL the
    /// server gives to more than one message is left out with all of them: Sealwax could neither
    /// tell them apart nor delete one safely.
    fn listing(&mut self) -> impl Future<Output = Result<Vec<Listing>, Pop3Error>> + Send;
    /// The message's bytes (RETR), dot-unstuffed, line ends as the server sent them.
    fn retrieve(&mut self, number: u32) -> impl Future<Output = Result<Vec<u8>, Pop3Error>> + Send;
    /// Marks the message deleted (DELE); the server deletes it only at a successful QUIT.
    fn delete(&mut self, number: u32) -> impl Future<Output = Result<(), Pop3Error>> + Send;
    /// Ends the session (QUIT): the server deletes what was marked and answers whether it did.
    fn quit(self) -> impl Future<Output = Result<(), Pop3Error>> + Send
    where
        Self: Sized;
    /// The message's header and the blank line after it (TOP n 0), for a message too large to
    /// download. Servers needn't offer TOP; without it, and by default, the answer is `Err`.
    fn header(&mut self, number: u32) -> impl Future<Output = Result<Vec<u8>, Pop3Error>> + Send {
        let _ = number;
        async { Err(Pop3Error::Err) }
    }
}
