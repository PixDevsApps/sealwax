//! Message-IDs (version 4 UUIDs) and Content-IDs (ADR-007) from random bits, `@` the
//! sender's domain in ASCII. Never the machine's host name. The caller passes the random bytes,
//! so this crate does no I/O.

use std::fmt::Write as _;

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// 128 bits as 26 lowercase base32 characters, safe in Message-IDs, Content-IDs and URLs.
pub fn token(random: [u8; 16]) -> String {
    let mut output = String::with_capacity(26);
    let mut bits = 0u32;
    let mut pending = 0;
    for byte in random {
        bits = (bits << 8) | u32::from(byte);
        pending += 8;
        while pending >= 5 {
            pending -= 5;
            output.push(char::from(BASE32[((bits >> pending) & 31) as usize]));
        }
        bits &= (1 << pending) - 1;
    }
    if pending > 0 {
        output.push(char::from(BASE32[((bits << (5 - pending)) & 31) as usize]));
    }
    output
}

pub fn uuid(mut random: [u8; 16]) -> String {
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let mut output = String::with_capacity(36);
    for (index, byte) in random.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            output.push('-');
        }
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Whether `text` is a token as [`token`] writes it.
pub fn is_token(text: &str) -> bool {
    text.len() == 26 && text.bytes().all(|byte| BASE32.contains(&byte))
}

/// The domain of `address` in ASCII (IDN labels as punycode), lower case; `None` when it has no
/// domain that could name a host.
pub fn ascii_domain(address: &str) -> Option<String> {
    let (_, domain) = address.trim().rsplit_once('@')?;
    let domain = domain.trim_end_matches('.');
    if domain.is_empty() || domain.len() > 253 {
        return None;
    }
    let ascii = idna::domain_to_ascii(domain).ok()?.to_ascii_lowercase();
    let valid = !ascii.is_empty()
        && ascii.len() <= 253
        && ascii.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    valid.then_some(ascii)
}

/// A new Message-ID (without angle brackets) for mail from `sender`; `None` when the sender has
/// no valid domain.
pub fn message_id(sender: &str, random: [u8; 16]) -> Option<String> {
    Some(format!("{}@{}", uuid(random), ascii_domain(sender)?))
}

/// A new Content-ID for an inline image in mail from `sender`, in the same form.
pub fn content_id(sender: &str, random: [u8; 16]) -> Option<String> {
    message_id(sender, random)
}

/// Whether a Message-ID or Content-ID (without angle brackets) can be written into a header as
/// it is: visible ASCII without brackets, quotes, backslashes or spaces, at most 250 bytes.
/// Ids from other mail that aren't are left out of what Sealwax writes.
pub fn is_header_safe(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 250
        && id
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"<>\"\\".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_26_base32_characters() {
        let zero = token([0; 16]);
        assert_eq!(zero, "a".repeat(26));
        let ones = token([0xff; 16]);
        assert_eq!(ones.len(), 26);
        assert!(ones.starts_with("7777"));
        assert!(
            ones.ends_with('4'),
            "the last 3 bits pad with zeros: {ones}"
        );
        let mixed = token(*b"0123456789abcdef");
        assert!(is_token(&mixed), "{mixed}");
        assert_ne!(mixed, token(*b"0123456789abcdeg"));
        assert!(!is_token("ABC"));
    }

    #[test]
    fn message_ids_are_version_4_uuids() {
        assert_eq!(uuid([0; 16]), "00000000-0000-4000-8000-000000000000");
        assert_eq!(uuid([0xff; 16]), "ffffffff-ffff-4fff-bfff-ffffffffffff");
        assert_eq!(
            message_id("kari@example.com", *b"0123456789abcdef").unwrap(),
            "30313233-3435-4637-b839-616263646566@example.com"
        );
    }

    #[test]
    fn ids_use_the_sender_domain_in_ascii() {
        let id = message_id("kari@Example.COM", [7; 16]).unwrap();
        assert!(id.ends_with("@example.com"), "{id}");
        let idn = message_id("ola@blåbær.example", [7; 16]).unwrap();
        assert!(idn.ends_with("@xn--blbr-roah.example"), "{idn}");
        assert_eq!(ascii_domain("a@example.test."), Some("example.test".into()));
        for bad in [
            "no-domain",
            "a@",
            "a@bad domain.example",
            "a@-x.example",
            "a@x..y",
        ] {
            assert_eq!(ascii_domain(bad), None, "{bad}");
        }
        assert_eq!(content_id("a@example.org", [1; 16]).unwrap().len(), 36 + 12);
    }
}
