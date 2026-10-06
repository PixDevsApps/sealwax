//! Addresses as the recipient chips take them: what a user types or pastes ("Ada Lovelace
//! <ada@example.test>, bob@example.test; \"Last, First\" <x@example.org>") split into
//! addresses and the parts that aren't addresses, and an address formatted for a chip and for
//! text. Local parts must be ASCII: 1.0 sends without SMTPUTF8 (ADR-007). Domains may be
//! international; they go on the wire in ASCII.

use super::ids;
use super::text::header_text;
use crate::model::Address;
use std::fmt;

/// What [`parse_list`] found. Its `Debug` prints counts only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AddressList {
    /// Each address once, in the order typed.
    pub addresses: Vec<Address>,
    /// The pieces that aren't addresses, trimmed, for the chip entry to keep as text.
    pub rest: Vec<String>,
}

impl fmt::Debug for AddressList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AddressList")
            .field("addresses", &self.addresses.len())
            .field("rest", &self.rest.len())
            .finish()
    }
}

/// Characters an unquoted display name can't hold (RFC 5322 `specials`).
const SPECIALS: &[char] = &[
    '(', ')', '<', '>', '[', ']', ':', ';', '@', '\\', ',', '.', '"',
];

/// Whether `email` is an address Sealwax can send to: an ASCII local part of at most 64 bytes
/// without spaces or specials, and a domain that is a host name (international names allowed).
pub fn is_valid(email: &str) -> bool {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && email.len() <= 254
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~.".contains(&byte))
        && !domain.starts_with('[')
        && ids::ascii_domain(email).is_some()
}

/// The address as it goes on the wire: the local part as written, the domain in ASCII and lower
/// case; `None` when it isn't [`is_valid`].
pub fn ascii(email: &str) -> Option<String> {
    let email = email.trim();
    if !is_valid(email) {
        return None;
    }
    let (local, _) = email.rsplit_once('@')?;
    Some(format!("{local}@{}", ids::ascii_domain(email)?))
}

/// The form two addresses are compared in: case-insensitive, domains in ASCII.
pub fn key(email: &str) -> String {
    let email = email.trim();
    match (email.rsplit_once('@'), ids::ascii_domain(email)) {
        (Some((local, _)), Some(domain)) => format!("{}@{domain}", local.to_lowercase()),
        _ => email.to_lowercase(),
    }
}

/// Whether two addresses are the same mailbox (case-insensitive, IDN-aware).
pub fn same(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// `list` with each mailbox once, first occurrence first; a name from a later occurrence fills
/// an empty one.
pub fn dedupe(list: impl IntoIterator<Item = Address>) -> Vec<Address> {
    let mut out: Vec<Address> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for address in list {
        let address_key = key(&address.email);
        match keys.iter().position(|known| *known == address_key) {
            Some(index) => {
                if out[index].name.is_empty() && !address.name.is_empty() {
                    out[index].name = address.name;
                }
            }
            None => {
                keys.push(address_key);
                out.push(address);
            }
        }
    }
    out
}

/// What a chip shows: the name, or the address when there is none.
pub fn chip_label(address: &Address) -> &str {
    address.display_name()
}

/// An address as text that [`parse_list`] reads back: `Name <email>`, the name quoted when it
/// holds specials, or the bare address.
pub fn format(address: &Address) -> String {
    let name = header_text(&address.name);
    if name.is_empty() {
        return address.email.clone();
    }
    if name.contains(SPECIALS) || name.starts_with('\'') {
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\" <{}>", address.email)
    } else {
        format!("{name} <{}>", address.email)
    }
}

/// A list as text, separated by commas.
pub fn format_list(addresses: &[Address]) -> String {
    addresses.iter().map(format).collect::<Vec<_>>().join(", ")
}

/// Reads typed or pasted recipients: separated by commas, semicolons or line breaks (outside
/// quotes and angle brackets), each `Name <email>`, `"Last, First" <email>`, `email (Name)`,
/// a bare address, `mailto:` an address, or several bare addresses separated by spaces.
pub fn parse_list(text: &str) -> AddressList {
    let mut list = AddressList::default();
    for piece in split(text) {
        match parse_piece(piece) {
            Some(addresses) => list.addresses.extend(addresses),
            None => list.rest.push(piece.to_owned()),
        }
    }
    list.addresses = dedupe(std::mem::take(&mut list.addresses));
    list
}

/// One address, or `None` when `text` isn't exactly one.
pub fn parse_one(text: &str) -> Option<Address> {
    let list = parse_list(text);
    match (list.addresses.as_slice(), list.rest.is_empty()) {
        ([one], true) => Some(one.clone()),
        _ => None,
    }
}

fn split(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut angle = 0usize;
    let mut paren = 0usize;
    for (at, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => angle += 1,
            '>' if !quoted => angle = angle.saturating_sub(1),
            '(' if !quoted => paren += 1,
            ')' if !quoted => paren = paren.saturating_sub(1),
            ',' | ';' | '\n' | '\r' if !quoted && angle == 0 && paren == 0 => {
                pieces.push(&text[start..at]);
                start = at + c.len_utf8();
            }
            _ => {}
        }
    }
    pieces.push(&text[start..]);
    pieces
        .into_iter()
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .collect()
}

fn parse_piece(piece: &str) -> Option<Vec<Address>> {
    if let Some(open) = top_level(piece, '<') {
        let close = piece[open..].find('>')? + open;
        let after = piece[close + 1..].trim();
        if !(after.is_empty() || after.starts_with('(') && after.ends_with(')')) {
            return None;
        }
        let email = bare(&piece[open + 1..close])?;
        return Some(vec![Address::new(display_name(&piece[..open]), email)]);
    }
    if let Some(open) = top_level(piece, '(')
        && piece.ends_with(')')
    {
        let email = bare(&piece[..open])?;
        let name = display_name(&piece[open + 1..piece.len() - 1]);
        return Some(vec![Address::new(name, email)]);
    }
    if let Some(email) = bare(piece) {
        return Some(vec![Address::new("", email)]);
    }
    let words: Vec<&str> = piece.split_whitespace().collect();
    let emails: Option<Vec<String>> = (words.len() > 1)
        .then(|| words.iter().map(|word| bare(word)).collect())
        .flatten();
    emails.map(|emails| {
        emails
            .into_iter()
            .map(|email| Address::new("", email))
            .collect()
    })
}

/// The byte offset of `wanted` outside quotes.
fn top_level(text: &str, wanted: char) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quoted {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == wanted && !quoted {
            return Some(at);
        }
    }
    None
}

/// A bare address, with an optional `mailto:` before it.
fn bare(text: &str) -> Option<String> {
    let text = text.trim();
    let text = match text.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => &text[7..],
        _ => text,
    };
    is_valid(text).then(|| text.to_owned())
}

fn display_name(text: &str) -> String {
    let text = text.trim();
    let unquoted = if text.len() >= 2
        && (text.starts_with('"') && text.ends_with('"')
            || text.starts_with('\'') && text.ends_with('\''))
    {
        let inner = &text[1..text.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => out.extend(chars.next()),
                c => out.push(c),
            }
        }
        out
    } else {
        text.to_owned()
    };
    header_text(&unquoted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn emails(list: &AddressList) -> Vec<(&str, &str)> {
        list.addresses
            .iter()
            .map(|a| (a.name.as_str(), a.email.as_str()))
            .collect()
    }

    #[test]
    fn reads_what_people_paste() {
        let list = parse_list(
            "Ada Lovelace <ada@example.test>, bob@example.test; \"Last, First\" <x@example.org>",
        );
        assert_eq!(
            emails(&list),
            [
                ("Ada Lovelace", "ada@example.test"),
                ("", "bob@example.test"),
                ("Last, First", "x@example.org"),
            ]
        );
        assert!(list.rest.is_empty());
        let list = parse_list(
            "'Kari Nordmann' <kari@example.org>\nola@example.org (Ola N.)\r\n\
             mailto:per@example.org a@example.test b@example.test",
        );
        assert_eq!(
            emails(&list),
            [
                ("Kari Nordmann", "kari@example.org"),
                ("Ola N.", "ola@example.org"),
                ("", "per@example.org"),
                ("", "a@example.test"),
                ("", "b@example.test"),
            ]
        );
    }

    #[test]
    fn keeps_what_isnt_an_address() {
        let list = parse_list(
            "Ada, bob@example, <>, x@@example.org, ok@example.org, \"a\\\"b\" <q@example.org> trailing",
        );
        assert_eq!(emails(&list), [("", "bob@example"), ("", "ok@example.org")]);
        assert_eq!(
            list.rest,
            [
                "Ada",
                "<>",
                "x@@example.org",
                "\"a\\\"b\" <q@example.org> trailing"
            ]
        );
        assert_eq!(format!("{list:?}"), "AddressList { addresses: 2, rest: 4 }");
    }

    #[test]
    fn duplicates_collapse_case_insensitively() {
        let list = parse_list(
            "bob@Example.test, Bob <BOB@example.TEST>, ola@blåbær.example, Ola <ola@xn--blbr-roah.example>",
        );
        assert_eq!(
            emails(&list),
            [("Bob", "bob@Example.test"), ("Ola", "ola@blåbær.example")]
        );
        assert!(same("A@Blåbær.Example", "a@xn--blbr-roah.example"));
    }

    #[test]
    fn validity() {
        for good in [
            "a@example.test",
            "a.b+c@example.org",
            "x@blåbær.example",
            "o'neil@example.com",
            "a@localhost",
        ] {
            assert!(is_valid(good), "{good}");
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
            assert!(!is_valid(bad), "{bad}");
        }
        assert_eq!(
            ascii("Ola@Blåbær.Example").as_deref(),
            Some("Ola@xn--blbr-roah.example")
        );
        assert_eq!(ascii("æ@example.org"), None);
    }

    #[test]
    fn formats_for_chips_and_text() {
        let plain = Address::new("Ada Lovelace", "ada@example.test");
        assert_eq!(chip_label(&plain), "Ada Lovelace");
        assert_eq!(format(&plain), "Ada Lovelace <ada@example.test>");
        let special = Address::new("Lovelace, Ada \"the\" Countess", "ada@example.test");
        assert_eq!(
            format(&special),
            "\"Lovelace, Ada \\\"the\\\" Countess\" <ada@example.test>"
        );
        let bare = Address::new("", "bob@example.test");
        assert_eq!(chip_label(&bare), "bob@example.test");
        assert_eq!(
            format_list(&[plain, bare]),
            "Ada Lovelace <ada@example.test>, bob@example.test"
        );
        assert_eq!(parse_one("Bob <bob@example.test>").unwrap().name, "Bob");
        assert_eq!(parse_one("a@example.test, b@example.test"), None);
    }

    fn name() -> impl Strategy<Value = String> {
        "[A-Za-zæøåÆØÅ0-9 ,.;:@<>()\"'\\\\-]{0,24}"
    }

    fn email() -> impl Strategy<Value = String> {
        (
            "[a-z0-9][a-z0-9._+-]{0,10}[a-z0-9]",
            "[a-zæø][a-z0-9]{0,8}",
            "(test|org|example)",
        )
            .prop_map(|(local, host, tld)| format!("{local}@{host}.{tld}"))
            .prop_filter("valid", |email| is_valid(email))
    }

    proptest! {
        #[test]
        fn formatted_lists_read_back(entries in prop::collection::vec((name(), email()), 0..6)) {
            let addresses = dedupe(entries.into_iter().map(|(name, email)| Address::new(header_text(&name), email)));
            let parsed = parse_list(&format_list(&addresses));
            prop_assert!(parsed.rest.is_empty(), "{:?}", parsed.rest);
            prop_assert_eq!(parsed.addresses, addresses);
        }

        #[test]
        fn never_panics(text in "\\PC{0,80}") {
            let list = parse_list(&text);
            for address in list.addresses {
                prop_assert!(is_valid(&address.email));
            }
        }
    }
}
