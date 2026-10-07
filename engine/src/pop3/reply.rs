//! Status lines and what a `-ERR` means. A refusal's text is kept only to classify it and is
//! never printed: it can hold the username.

use super::{Busy, Pop3Error};
use sealwax_domain::account::LoginFailure;
use std::fmt;
use std::time::Duration;

/// One status line, without its line end.
#[derive(Debug)]
pub(super) enum Status {
    /// `+OK` and the text after it.
    Ok(Vec<u8>),
    Err(Refusal),
    /// A SASL continuation (RFC 5034): `+ ` and a base64 challenge.
    Continue,
}

impl Status {
    pub fn parse(line: &[u8]) -> Result<Status, Pop3Error> {
        if let Some(rest) = after_word(line, b"+OK") {
            Ok(Status::Ok(rest.trim_ascii().to_vec()))
        } else if let Some(rest) = after_word(line, b"-ERR") {
            Ok(Status::Err(Refusal::parse(rest)))
        } else if line == b"+" || line.starts_with(b"+ ") {
            Ok(Status::Continue)
        } else {
            Err(Pop3Error::Protocol("unexpected reply"))
        }
    }
}

/// The rest of `line` after `word` (any case) when the word stands alone.
fn after_word<'a>(line: &'a [u8], word: &[u8]) -> Option<&'a [u8]> {
    let head = line.get(..word.len())?;
    if !head.eq_ignore_ascii_case(word) {
        return None;
    }
    let rest = &line[word.len()..];
    (rest.is_empty() || rest[0] == b' ').then_some(rest)
}

/// A response code (RFC 2449, RFC 3206).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Code {
    InUse,
    LoginDelay,
    SysTemp,
    SysPerm,
    Auth,
    Other,
}

impl Code {
    fn parse(name: &str) -> Code {
        let name = name.to_ascii_uppercase();
        let is = |code: &str| {
            name == code
                || name
                    .strip_prefix(code)
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        if is("IN-USE") {
            Code::InUse
        } else if is("LOGIN-DELAY") {
            Code::LoginDelay
        } else if is("SYS/TEMP") {
            Code::SysTemp
        } else if is("SYS/PERM") {
            Code::SysPerm
        } else if is("AUTH") {
            Code::Auth
        } else {
            Code::Other
        }
    }
}

/// A `-ERR` reply.
pub(super) struct Refusal {
    pub code: Option<Code>,
    /// Lower case, for classification only.
    text: String,
}

impl fmt::Debug for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Refusal")
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

const APP_PASSWORD: &[&str] = &[
    "application-specific password",
    "app-specific password",
    "app password",
];
const DISABLED: &[&str] = &[
    "not enabled for pop",
    "enable pop",
    "pop access is disabled",
    "pop3 access is disabled",
    "pop is disabled",
    "pop3 is disabled",
    "pop disabled",
    "pop3 disabled",
];
const OAUTH: &[&str] = &["basic authentication is disabled", "use oauth"];
const WEB: &[&str] = &["web browser", "web login"];
const IN_USE: &[&str] = &[
    "mailbox in use",
    "maildrop in use",
    "already in use",
    "in use by another",
    "already locked",
    "unable to lock",
    "maildrop is locked",
    "mailbox is locked",
    "maildrop locked",
    "mailbox locked",
];
const TEMPORARY: &[&str] = &["try again later", "temporar"];

impl Refusal {
    fn parse(rest: &[u8]) -> Refusal {
        let text = String::from_utf8_lossy(rest.trim_ascii()).to_lowercase();
        let code = text
            .strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .map(|(name, _)| Code::parse(name));
        Refusal { code, text }
    }

    fn has(&self, needles: &[&str]) -> bool {
        needles.iter().any(|needle| self.text.contains(needle))
    }

    /// What a refused login (USER, PASS or AUTH) means: the codes first, then provider texts;
    /// anything else refused is a wrong password, as for IMAP. `delay` is CAPA's LOGIN-DELAY.
    pub fn login(&self, delay: Option<Duration>) -> Pop3Error {
        let rejected = Pop3Error::Rejected;
        match self.code {
            Some(Code::InUse) => Pop3Error::Busy(Busy::InUse),
            Some(Code::LoginDelay) => Pop3Error::Busy(Busy::LoginDelay(delay)),
            Some(Code::SysTemp) => Pop3Error::Busy(Busy::Temporary),
            _ if self.has(APP_PASSWORD) => rejected(LoginFailure::AppPasswordRequired),
            _ if self.has(OAUTH) => rejected(LoginFailure::OAuthRequired),
            _ if self.has(WEB) => rejected(LoginFailure::Other),
            _ if self.has(DISABLED) => rejected(LoginFailure::ImapDisabled),
            Some(Code::Auth) => rejected(LoginFailure::WrongPassword),
            Some(Code::SysPerm) => rejected(LoginFailure::Other),
            _ if self.has(IN_USE) => Pop3Error::Busy(Busy::InUse),
            _ if self.has(TEMPORARY) => Pop3Error::Busy(Busy::Temporary),
            _ => rejected(LoginFailure::WrongPassword),
        }
    }

    /// What a refused command after the login means: a busy server, or a plain `-ERR`.
    pub fn command(&self) -> Pop3Error {
        match self.code {
            Some(Code::InUse) => Pop3Error::Busy(Busy::InUse),
            Some(Code::LoginDelay) => Pop3Error::Busy(Busy::LoginDelay(None)),
            Some(Code::SysTemp) => Pop3Error::Busy(Busy::Temporary),
            _ => Pop3Error::Err,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(line: &str) -> Refusal {
        match Status::parse(line.as_bytes()).unwrap() {
            Status::Err(refusal) => refusal,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn status_lines_are_parsed() {
        assert!(matches!(Status::parse(b"+OK").unwrap(), Status::Ok(text) if text.is_empty()));
        assert!(
            matches!(Status::parse(b"+ok 2 messages ").unwrap(), Status::Ok(text) if text == b"2 messages")
        );
        assert!(matches!(Status::parse(b"+ ").unwrap(), Status::Continue));
        assert!(matches!(Status::parse(b"+").unwrap(), Status::Continue));
        assert!(matches!(
            Status::parse(b"+ eyJzdGF0dXMiOiI0MDEifQ==").unwrap(),
            Status::Continue
        ));
        assert!(matches!(Status::parse(b"-ERR").unwrap(), Status::Err(r) if r.code.is_none()));
        for bad in [&b"+OKAY"[..], b"-ERROR x", b"OK", b"", b"* OK imap"] {
            assert!(Status::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn response_codes_are_read_with_their_hierarchy() {
        for (line, code) in [
            ("-ERR [IN-USE] locked", Some(Code::InUse)),
            ("-ERR [LOGIN-DELAY] wait", Some(Code::LoginDelay)),
            ("-ERR [SYS/TEMP] later", Some(Code::SysTemp)),
            ("-ERR [SYS/TEMP/DISK] later", Some(Code::SysTemp)),
            ("-ERR [sys/perm] no", Some(Code::SysPerm)),
            ("-ERR [AUTH] bad", Some(Code::Auth)),
            ("-ERR [AUTHX] bad", Some(Code::Other)),
            ("-ERR [SYSTEMP] bad", Some(Code::Other)),
            ("-ERR no code [AUTH]", None),
            ("-ERR [unterminated", None),
        ] {
            assert_eq!(refusal(line).code, code, "{line}");
        }
    }

    #[test]
    fn refused_logins_are_classified() {
        let delay = Some(Duration::from_secs(900));
        for (line, expected) in [
            (
                "-ERR [AUTH] Authentication failed.",
                Pop3Error::Rejected(LoginFailure::WrongPassword),
            ),
            (
                "-ERR [IN-USE] Mailbox is locked",
                Pop3Error::Busy(Busy::InUse),
            ),
            (
                "-ERR [LOGIN-DELAY] Too soon",
                Pop3Error::Busy(Busy::LoginDelay(delay)),
            ),
            (
                "-ERR [SYS/TEMP] Try later",
                Pop3Error::Busy(Busy::Temporary),
            ),
            (
                "-ERR [SYS/PERM] Your account is not enabled for POP access. Please visit your \
                 settings page.",
                Pop3Error::Rejected(LoginFailure::ImapDisabled),
            ),
            (
                "-ERR [SYS/PERM] Account disabled",
                Pop3Error::Rejected(LoginFailure::Other),
            ),
            (
                "-ERR [AUTH] Application-specific password required: https://example.test/x",
                Pop3Error::Rejected(LoginFailure::AppPasswordRequired),
            ),
            (
                "-ERR Basic authentication is disabled",
                Pop3Error::Rejected(LoginFailure::OAuthRequired),
            ),
            (
                "-ERR [AUTH] Web login required: https://example.test/answer",
                Pop3Error::Rejected(LoginFailure::Other),
            ),
            (
                "-ERR Please log in via your web browser",
                Pop3Error::Rejected(LoginFailure::Other),
            ),
            ("-ERR Unable to lock maildrop", Pop3Error::Busy(Busy::InUse)),
            (
                "-ERR Mailbox in use by another client",
                Pop3Error::Busy(Busy::InUse),
            ),
            (
                "-ERR Login used a wrong password",
                Pop3Error::Rejected(LoginFailure::WrongPassword),
            ),
            ("-ERR Temporary failure", Pop3Error::Busy(Busy::Temporary)),
            (
                "-ERR Authentication failed",
                Pop3Error::Rejected(LoginFailure::WrongPassword),
            ),
            ("-ERR", Pop3Error::Rejected(LoginFailure::WrongPassword)),
        ] {
            let found = refusal(line).login(delay);
            assert_eq!(format!("{found:?}"), format!("{expected:?}"), "{line}");
        }
    }

    #[test]
    fn refused_commands_are_busy_or_err() {
        assert!(matches!(
            refusal("-ERR [SYS/TEMP] Disk").command(),
            Pop3Error::Busy(Busy::Temporary)
        ));
        assert!(matches!(
            refusal("-ERR [IN-USE] x").command(),
            Pop3Error::Busy(Busy::InUse)
        ));
        assert!(matches!(
            refusal("-ERR no such message").command(),
            Pop3Error::Err
        ));
        assert!(matches!(refusal("-ERR [AUTH] x").command(), Pop3Error::Err));
    }

    #[test]
    fn a_refusal_never_prints_its_text() {
        let found = refusal("-ERR [AUTH] Wrong password for kari@example.test");
        assert!(!format!("{found:?}").contains("kari"));
        let error = found.login(None);
        assert!(!format!("{error} {error:?}").contains("kari"));
    }
}
