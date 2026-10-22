//! What can go wrong on an IMAP connection, and what a refused login means for the user.
//! Neither the display nor the debug form of an error carries server text: a server's reply can
//! hold the user's address or a folder name, and async-imap's own errors can hold raw protocol
//! bytes (S1), so they are classified and dropped, never logged.

use crate::net::{ConnectError, tls_failure};
use sealwax_domain::account::LoginFailure;
use sealwax_infrastructure::store::StoreError;
use std::fmt;
use std::io;

/// A server's response text, kept only for classification.
pub struct ServerText(pub(crate) String);

impl ServerText {
    /// A server's words, as a stand-in engine gives them (the app's self-test).
    pub fn new(text: String) -> Self {
        ServerText(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The response code the text starts with, without its arguments (`TRYCREATE`,
    /// `OVERQUOTA`, `READ-ONLY`), as the server wrote it.
    pub fn code(&self) -> Option<&str> {
        let rest = self.0.trim_start().strip_prefix('[')?;
        let end = rest.find([']', ' '])?;
        Some(&rest[..end]).filter(|code| !code.is_empty())
    }
}

impl fmt::Debug for ServerText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerText(..)")
    }
}

#[derive(thiserror::Error)]
pub enum ImapError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error("the server refused STARTTLS")]
    StartTls,
    #[error("the server refused the login")]
    Rejected(LoginFailure),
    #[error("the server offers no authentication mechanism Sealwax can use")]
    NoMechanism {
        /// It offers XOAUTH2 or OAUTHBEARER.
        oauth: bool,
    },
    #[error("the connection was lost")]
    ConnectionLost,
    #[error("i/o error ({})", .0.kind())]
    Io(io::Error),
    /// async-imap couldn't parse the server's reply to the command (issue #20); the bytes it
    /// couldn't read stay in its buffer, so the connection is done.
    #[error("the server's reply to {0} couldn't be read")]
    Unreadable(&'static str),
    #[error("the server said NO")]
    No(ServerText),
    #[error("the server said BAD")]
    Bad(ServerText),
    #[error("the server said BYE")]
    Bye(ServerText),
    #[error("timed out waiting for the server")]
    Timeout,
    #[error("protocol error: {0}")]
    Protocol(&'static str),
}

impl fmt::Debug for ImapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ImapError({self})")
    }
}

impl From<io::Error> for ImapError {
    fn from(error: io::Error) -> Self {
        ImapError::Io(error)
    }
}

impl From<async_imap::error::Error> for ImapError {
    fn from(error: async_imap::error::Error) -> Self {
        use async_imap::error::Error;
        match error {
            Error::Io(error) => ImapError::Io(error),
            Error::Bad(text) => ImapError::Bad(ServerText(text)),
            Error::No(text) => ImapError::No(ServerText(text)),
            Error::ConnectionLost => ImapError::ConnectionLost,
            _ => ImapError::Protocol("unexpected response"),
        }
    }
}

impl ImapError {
    /// An error reading the reply to `command` (its verb only, never its arguments): async-imap
    /// 0.12 reports a reply imap-proto can't parse, and one past its 512 MiB buffer, as
    /// `ErrorKind::Other` with these texts.
    pub(crate) fn reading(error: io::Error, command: &'static str) -> Self {
        let unreadable = error.kind() == io::ErrorKind::Other
            && error.get_ref().is_some_and(|inner| {
                let text = inner.to_string();
                text.contains(" during parsing of ") || text == "incoming data too large"
            });
        if unreadable {
            ImapError::Unreadable(command)
        } else {
            ImapError::Io(error)
        }
    }

    /// What the setup assistant or the sidebar tells the user.
    pub fn failure(&self) -> LoginFailure {
        match self {
            ImapError::Connect(error) => error.failure(),
            ImapError::StartTls => LoginFailure::NoTls,
            ImapError::Rejected(failure) => *failure,
            ImapError::NoMechanism { oauth: true } => LoginFailure::OAuthRequired,
            ImapError::Timeout => LoginFailure::Timeout,
            ImapError::Io(error) if error.kind() == io::ErrorKind::TimedOut => {
                LoginFailure::Timeout
            }
            ImapError::Io(error) if error.get_ref().is_some_and(|e| e.is::<rustls::Error>()) => {
                tls_failure(error)
            }
            _ => LoginFailure::Other,
        }
    }

    /// The connection can't be used any more and must be replaced.
    pub fn is_connection(&self) -> bool {
        !matches!(self, ImapError::No(_) | ImapError::Bad(_))
    }
}

/// Why listing folders or a first sync failed.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Imap(#[from] ImapError),
    #[error("the local store failed: {0}")]
    Store(#[from] StoreError),
    #[error("the folder can't hold messages")]
    NotSelectable,
    #[error("parsing the fetched headers stopped")]
    Parse,
}

impl SyncError {
    /// Why a sync failed, for the account's status (`Event::AccountStatus`).
    pub fn failure(&self) -> LoginFailure {
        match self {
            SyncError::Imap(error) => error.failure(),
            _ => LoginFailure::Other,
        }
    }
}

/// Classifies the server's refusal of AUTHENTICATE or LOGIN from its text and response code.
/// RFC 5530 codes come first; provider texts are matched as documented or as reported by users
/// (docs/m2/imap.md lists which are unverified). Anything else refused is a wrong password.
pub(crate) fn rejection(text: &str, bad: bool) -> LoginFailure {
    let text = text.to_lowercase();
    let has = |needle: &str| text.contains(needle);
    if has("application-specific password") || has("app-specific password") || has("app password") {
        LoginFailure::AppPasswordRequired
    } else if has("not enabled for imap")
        || has("imap access is disabled")
        || has("imap is disabled")
        || has("authenticated but not connected")
    {
        LoginFailure::ImapDisabled
    } else if has("basic authentication is disabled") || has("use oauth") {
        LoginFailure::OAuthRequired
    } else if has("privacyrequired") {
        LoginFailure::NoTls
    } else if has("web browser") || has("unavailable") || has("contactadmin") || bad {
        LoginFailure::Other
    } else {
        LoginFailure::WrongPassword
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_classified() {
        for (text, failure) in [
            (
                "code: None, info: Some(\"[AUTHENTICATIONFAILED] Authentication failed.\")",
                LoginFailure::WrongPassword,
            ),
            (
                "[AUTHENTICATIONFAILED] Invalid credentials (Failure)",
                LoginFailure::WrongPassword,
            ),
            (
                "code: Some(Alert), info: Some(\"Application-specific password required: \
                 https://support.google.com/accounts/answer/185833 (Failure)\")",
                LoginFailure::AppPasswordRequired,
            ),
            (
                "[ALERT] Your account is not enabled for IMAP use. Please visit your Gmail \
                 settings page and enable your account for IMAP access. (Failure)",
                LoginFailure::ImapDisabled,
            ),
            (
                "User is authenticated but not connected.",
                LoginFailure::ImapDisabled,
            ),
            (
                "[ALERT] Please log in via your web browser: \
                 https://support.google.com/mail/accounts/answer/78754 (Failure)",
                LoginFailure::Other,
            ),
            ("[PRIVACYREQUIRED] Use TLS", LoginFailure::NoTls),
            ("[UNAVAILABLE] Try later", LoginFailure::Other),
            ("LOGIN failed.", LoginFailure::WrongPassword),
        ] {
            assert_eq!(rejection(text, false), failure, "{text}");
        }
        assert_eq!(rejection("Invalid command", true), LoginFailure::Other);
    }

    #[test]
    fn errors_never_print_server_text() {
        let error = ImapError::No(ServerText("Mailbox Søppelpost: test@example.test".into()));
        assert!(!format!("{error} {error:?}").contains("example.test"));
        assert!(!format!("{error:?}").contains("Søppelpost"));
        assert_eq!(error.failure(), LoginFailure::Other);
        assert!(!error.is_connection());
        assert!(ImapError::Timeout.is_connection());
    }

    #[test]
    fn unreadable_replies_are_told_apart_from_io_errors() {
        let parse = io::Error::other(
            "Error(Error { input: [41], code: Tag }) during parsing of \"* 1 FETCH (x@example.test\"",
        );
        let error = ImapError::reading(parse, "UID FETCH");
        assert!(
            matches!(error, ImapError::Unreadable("UID FETCH")),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "the server's reply to UID FETCH couldn't be read"
        );
        assert!(!format!("{error:?}").contains("example.test"));
        assert!(error.is_connection());
        let large = io::Error::other("incoming data too large");
        assert!(matches!(
            ImapError::reading(large, "LIST"),
            ImapError::Unreadable("LIST")
        ));
        let reset = io::Error::from(io::ErrorKind::ConnectionReset);
        assert!(matches!(
            ImapError::reading(reset, "LIST"),
            ImapError::Io(_)
        ));
        let other = io::Error::other("Failure");
        assert!(matches!(
            ImapError::reading(other, "LIST"),
            ImapError::Io(_)
        ));
    }
}
