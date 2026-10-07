//! POP3 (M8, ML-087; RFC 1939, 2449, 2595, 3206, 5034): a session over `crate::net`'s sockets
//! and TLS that only ever authenticates over TLS (implicit TLS on 995, or STLS on 110 that must
//! succeed). A POP3 account's actor works through [`Mailbox`], so it can be tested with an
//! in-memory mailbox.
//!
//! M8's contract: the types, the trait and the entry points. [`connect`] and [`check_login`]
//! answer [`Pop3Error::NotBuilt`] until phase A builds the client.

use crate::net::ConnectError;
use rustls::ClientConfig;
use sealwax_domain::account::{Endpoint, LoginFailure};
use std::future::Future;
use std::sync::Arc;

pub use crate::imap::Credentials;

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

#[derive(Debug, thiserror::Error)]
pub enum Pop3Error {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error("the server refused STLS")]
    StartTls,
    #[error("the server refused the login")]
    Rejected(LoginFailure),
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
    /// The message is larger than Sealwax downloads.
    #[error("the message is too large ({0} octets)")]
    TooLarge(u64),
    /// M8's contract: the client isn't built yet.
    #[error("POP3 isn't built yet")]
    NotBuilt,
}

/// A POP3 maildrop in the TRANSACTION state.
pub trait Mailbox: Send {
    /// Every message with its UIDL and size (UIDL and LIST).
    fn listing(&mut self) -> impl Future<Output = Result<Vec<Listing>, Pop3Error>> + Send;
    /// The message's bytes (RETR), dot-unstuffed, line ends as the server sent them.
    fn retrieve(&mut self, number: u32) -> impl Future<Output = Result<Vec<u8>, Pop3Error>> + Send;
    /// Marks the message deleted (DELE); the server deletes it only at a successful QUIT.
    fn delete(&mut self, number: u32) -> impl Future<Output = Result<(), Pop3Error>> + Send;
    /// Ends the session (QUIT): the server deletes what was marked and answers whether it did.
    fn quit(self) -> impl Future<Output = Result<(), Pop3Error>> + Send
    where
        Self: Sized;
}

/// A logged-in POP3 session.
#[derive(Debug)]
pub struct Session {
    _private: (),
}

impl Mailbox for Session {
    async fn listing(&mut self) -> Result<Vec<Listing>, Pop3Error> {
        Err(Pop3Error::NotBuilt)
    }

    async fn retrieve(&mut self, _number: u32) -> Result<Vec<u8>, Pop3Error> {
        Err(Pop3Error::NotBuilt)
    }

    async fn delete(&mut self, _number: u32) -> Result<(), Pop3Error> {
        Err(Pop3Error::NotBuilt)
    }

    async fn quit(self) -> Result<(), Pop3Error> {
        Err(Pop3Error::NotBuilt)
    }
}

/// Connects (implicit TLS, or STLS that must succeed), reads CAPA and logs in. `tls` comes from
/// [`crate::net::Trust`].
pub async fn connect(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
) -> Result<Session, Pop3Error> {
    let _ = (endpoint, credentials, tls);
    Err(Pop3Error::NotBuilt)
}

/// The setup assistant's login check: connect, log in, QUIT.
pub async fn check_login(
    endpoint: &Endpoint,
    credentials: Credentials<'_>,
    tls: Arc<ClientConfig>,
) -> Result<(), LoginFailure> {
    connect(endpoint, credentials, tls)
        .await
        .map(drop)
        .map_err(|error| match error {
            Pop3Error::Rejected(failure) => failure,
            Pop3Error::Connect(error) => error.failure(),
            _ => LoginFailure::Unreachable,
        })
}
