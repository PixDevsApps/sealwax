//! Email addresses as Sealwax uses them. Three rules, each in one place:
//!
//! - **Sendable** ([`EmailAddress`], [`is_sendable`]): an ASCII local part (1.0 sends without
//!   SMTPUTF8, ADR-007) and a domain that is a host name, international names allowed; they go
//!   on the wire in ASCII ([`EmailAddress::wire`]).
//! - **Looks like an address** ([`looks_like_address`]): what `accounts.toml` accepts for an
//!   account or an identity, one `@` with text on both sides and no spaces. More lenient than
//!   sendable, so an account whose address can't be sent from still loads and reads its mail.
//! - **The same mailbox** ([`AddressKey`], [`same`]): case-insensitive, the domain in ASCII, so
//!   `Kari@Blåbær.example` and `kari@xn--blbr-roah.example` are one person.
//!
//! The store's own keys (`senders::sender_key`, the remote-content senders) fold differently,
//! as SQLite does, and are saved that way; they are not these.

use crate::compose::ids::ascii_domain;
use std::fmt;

/// Characters a local part may hold besides ASCII letters and digits (RFC 5322 `atext`, dots
/// between them).
const LOCAL_PART_SYMBOLS: &[u8] = b"!#$%&'*+-/=?^_`{|}~.";

/// Why text isn't an address Sealwax can send to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    /// No `@`.
    NoAt,
    /// More than 254 bytes.
    TooLong,
    /// Empty, longer than 64 bytes, not ASCII, or with spaces, specials or stray dots.
    LocalPart,
    /// Not a host name (an address literal such as `[192.0.2.1]` included).
    Domain,
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoAt => "an address needs an @",
            Self::TooLong => "the address is longer than 254 characters",
            Self::LocalPart => "the part before the @ can't be sent to",
            Self::Domain => "the part after the @ isn't a host name",
        })
    }
}

impl std::error::Error for AddressError {}

/// An address Sealwax can send to, trimmed, as written. Its `Debug` doesn't show it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EmailAddress {
    text: String,
    wire: String,
}

impl EmailAddress {
    pub fn parse(text: &str) -> Result<Self, AddressError> {
        let text = text.trim();
        let (local, domain) = check(text)?;
        Ok(Self {
            text: text.to_owned(),
            wire: format!("{local}@{domain}"),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The address as it goes on the wire: the local part as written, the domain in ASCII and
    /// lower case.
    pub fn wire(&self) -> &str {
        &self.wire
    }

    pub fn key(&self) -> AddressKey {
        AddressKey::of(&self.text)
    }
}

impl fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl fmt::Debug for EmailAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EmailAddress(…)")
    }
}

impl AsRef<str> for EmailAddress {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

/// The local part and the domain in ASCII of a sendable address.
fn check(email: &str) -> Result<(&str, String), AddressError> {
    let (local, domain) = email.rsplit_once('@').ok_or(AddressError::NoAt)?;
    if email.len() > 254 {
        return Err(AddressError::TooLong);
    }
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || LOCAL_PART_SYMBOLS.contains(&byte));
    if !local_ok {
        return Err(AddressError::LocalPart);
    }
    if domain.starts_with('[') {
        return Err(AddressError::Domain);
    }
    let domain = ascii_domain(email).ok_or(AddressError::Domain)?;
    Ok((local, domain))
}

/// Whether `email` (as it is, untrimmed) is an address Sealwax can send to.
pub fn is_sendable(email: &str) -> bool {
    check(email).is_ok()
}

/// The address as it goes on the wire, `None` when it can't be sent to.
pub fn wire(email: &str) -> Option<String> {
    EmailAddress::parse(email).ok().map(|email| email.wire)
}

/// Whether `address` looks like an email address: one `@` with text on both sides, no spaces.
/// What `accounts.toml` holds for an account or an identity.
pub fn looks_like_address(address: &str) -> bool {
    match address.rsplit_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && address.len() <= 254
                && !domain.contains('@')
                && !address.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        None => false,
    }
}

/// The form two addresses are compared in: trimmed, the local part lower case, the domain in
/// ASCII and lower case (text without a host name, lower case as a whole). Its `Debug` doesn't
/// show it.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AddressKey(String);

impl AddressKey {
    pub fn of(email: &str) -> Self {
        let email = email.trim();
        Self(match (email.rsplit_once('@'), ascii_domain(email)) {
            (Some((local, _)), Some(domain)) => format!("{}@{domain}", local.to_lowercase()),
            _ => email.to_lowercase(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AddressKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AddressKey(…)")
    }
}

/// Whether two addresses are the same mailbox (case-insensitive, IDN-aware).
pub fn same(a: &str, b: &str) -> bool {
    AddressKey::of(a) == AddressKey::of(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sendable_addresses_have_an_ascii_local_part_and_a_host() {
        for good in [
            "kari@example.test",
            "first.last+tag@sub.example.org",
            "ola@blåbær.example",
            "o'brien@example.ie",
        ] {
            assert!(is_sendable(good), "{good}");
        }
        for (bad, why) in [
            ("no-at", AddressError::NoAt),
            ("@example.test", AddressError::LocalPart),
            ("jörg@example.de", AddressError::LocalPart),
            ("a b@example.test", AddressError::LocalPart),
            (".a@example.test", AddressError::LocalPart),
            ("a..b@example.test", AddressError::LocalPart),
            ("a@[192.0.2.1]", AddressError::Domain),
            ("a@bad domain.example", AddressError::Domain),
            ("a@", AddressError::Domain),
        ] {
            assert_eq!(EmailAddress::parse(bad), Err(why), "{bad}");
        }
        let long = format!("{}@{}.example", "a".repeat(64), "b".repeat(200));
        assert_eq!(EmailAddress::parse(&long), Err(AddressError::TooLong));
    }

    #[test]
    fn the_wire_form_has_the_domain_in_ascii() {
        let email = EmailAddress::parse("  Ola@Blåbær.Example ").expect("sendable");
        assert_eq!(email.as_str(), "Ola@Blåbær.Example");
        assert_eq!(email.wire(), "Ola@xn--blbr-roah.example");
        assert_eq!(wire("nobody"), None);
        assert_eq!(format!("{email:?}"), "EmailAddress(…)");
    }

    #[test]
    fn accounts_take_what_looks_like_an_address() {
        assert!(looks_like_address("jörg@example.de"));
        assert!(!is_sendable("jörg@example.de"));
        for bad in ["", "kari", "@example.test", "kari@", "a b@example.test"] {
            assert!(!looks_like_address(bad), "{bad}");
        }
    }

    #[test]
    fn one_mailbox_whatever_the_case_or_the_domain_form() {
        assert!(same("Kari@Blåbær.Example", " kari@xn--blbr-roah.example"));
        assert!(same("ÅSE@example.test", "åse@EXAMPLE.test"));
        assert!(!same("kari@example.test", "kari@example.org"));
        assert_eq!(
            AddressKey::of("Not An Address"),
            AddressKey::of("not an address")
        );
        assert_eq!(
            EmailAddress::parse("Kari@Example.Test")
                .expect("sendable")
                .key(),
            AddressKey::of("kari@example.test")
        );
        assert_eq!(format!("{:?}", AddressKey::of("a@b.c")), "AddressKey(…)");
    }

    #[test]
    fn the_chips_cases() {
        for good in [
            "a@example.test",
            "a.b+c@example.org",
            "x@blåbær.example",
            "o'neil@example.com",
            "a@localhost",
        ] {
            assert!(is_sendable(good), "{good}");
        }
        for bad in [
            "",
            "a",
            "@example.org",
            "a@",
            "a b@example.org",
            "æ@example.org",
            ".a@example.org",
            "a..b@example.org",
            "a@[127.0.0.1]",
            "a@exa mple.org",
            "a<b@example.org",
        ] {
            assert!(!is_sendable(bad), "{bad}");
        }
        assert_eq!(
            wire("Ola@Blåbær.Example").as_deref(),
            Some("Ola@xn--blbr-roah.example")
        );
        assert_eq!(wire("æ@example.org"), None);
    }
}
