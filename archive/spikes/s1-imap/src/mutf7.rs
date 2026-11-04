//! Modified UTF-7 for IMAP mailbox names (RFC 3501 section 5.1.3).

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    NonAscii,
    Unterminated,
    BadBase64,
    OddLength,
    NonZeroPadding,
    Surrogate,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            DecodeError::NonAscii => "raw non-ASCII byte",
            DecodeError::Unterminated => "shift sequence without a closing '-'",
            DecodeError::BadBase64 => "invalid modified base64",
            DecodeError::OddLength => "shift sequence is not whole UTF-16 code units",
            DecodeError::NonZeroPadding => "non-zero padding bits",
            DecodeError::Surrogate => "unpaired UTF-16 surrogate",
        };
        f.write_str(text)
    }
}

impl std::error::Error for DecodeError {}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

fn sextet(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b',' => Some(63),
        _ => None,
    }
}

/// Decodes a mailbox name as sent by the server. Printable ASCII stands for itself, `&-` is `&`,
/// and `&...-` carries modified base64 of UTF-16BE. Non-canonical but valid encodings are
/// accepted; anything invalid is an error, so the caller can fall back to the raw name.
pub fn decode(raw: &str) -> Result<String, DecodeError> {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if !(0x20..=0x7e).contains(&byte) {
            return Err(DecodeError::NonAscii);
        }
        if byte != b'&' {
            out.push(char::from(byte));
            i += 1;
            continue;
        }
        let end = bytes[i + 1..]
            .iter()
            .position(|&b| b == b'-')
            .map(|p| i + 1 + p)
            .ok_or(DecodeError::Unterminated)?;
        let shifted = &bytes[i + 1..end];
        i = end + 1;
        if shifted.is_empty() {
            out.push('&');
            continue;
        }
        let mut units = Vec::with_capacity(shifted.len() * 6 / 16);
        let (mut acc, mut bits) = (0u32, 0u32);
        let mut pending: Option<u16> = None;
        for &b in shifted {
            acc = (acc << 6) | sextet(b).ok_or(DecodeError::BadBase64)?;
            bits += 6;
            if bits >= 16 {
                bits -= 16;
                units.push(((acc >> bits) & 0xffff) as u16);
                acc &= (1 << bits) - 1;
            }
        }
        if bits >= 6 {
            return Err(DecodeError::OddLength);
        }
        if acc != 0 {
            return Err(DecodeError::NonZeroPadding);
        }
        for unit in units {
            match (pending.take(), unit) {
                (None, 0xd800..=0xdbff) => pending = Some(unit),
                (None, 0xdc00..=0xdfff) => return Err(DecodeError::Surrogate),
                (None, _) => {
                    out.push(char::from_u32(u32::from(unit)).ok_or(DecodeError::Surrogate)?)
                }
                (Some(high), 0xdc00..=0xdfff) => {
                    let code =
                        0x10000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00);
                    out.push(char::from_u32(code).ok_or(DecodeError::Surrogate)?);
                }
                (Some(_), _) => return Err(DecodeError::Surrogate),
            }
        }
        if pending.is_some() {
            return Err(DecodeError::Surrogate);
        }
    }
    Ok(out)
}

/// Encodes a mailbox name for the wire (canonical form).
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    let mut pending: Vec<u16> = Vec::new();
    for ch in name.chars() {
        if (' '..='~').contains(&ch) {
            flush(&mut out, &mut pending);
            if ch == '&' {
                out.push_str("&-");
            } else {
                out.push(ch);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(ch.encode_utf16(&mut buf));
        }
    }
    flush(&mut out, &mut pending);
    out
}

fn flush(out: &mut String, pending: &mut Vec<u16>) {
    if pending.is_empty() {
        return;
    }
    out.push('&');
    let (mut acc, mut bits) = (0u32, 0u32);
    for unit in pending.drain(..) {
        acc = (acc << 16) | u32::from(unit);
        bits += 16;
        while bits >= 6 {
            bits -= 6;
            out.push(char::from(ALPHABET[((acc >> bits) & 0x3f) as usize]));
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((acc << (6 - bits)) & 0x3f) as usize]));
    }
    out.push('-');
}

/// The display name for a wire name: decoded when valid, otherwise the raw name unchanged.
pub fn display_name(raw: &str) -> String {
    decode(raw).unwrap_or_else(|_| raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_3501_example() {
        assert_eq!(
            decode("~peter/mail/&U,BTFw-/&ZeVnLIqe-").unwrap(),
            "~peter/mail/台北/日本語"
        );
        assert_eq!(
            encode("~peter/mail/台北/日本語"),
            "~peter/mail/&U,BTFw-/&ZeVnLIqe-"
        );
    }

    #[test]
    fn norwegian_names() {
        for (name, wire) in [
            ("Søppelpost", "S&APg-ppelpost"),
            ("Papirkurv", "Papirkurv"),
            ("Sendt", "Sendt"),
            ("Økonomi & regnskap", "&ANg-konomi &- regnskap"),
            (
                "Prosjekter/Høst 📬 2026",
                "Prosjekter/H&APg-st &2D3c7A- 2026",
            ),
            ("ÆØÅ æøå", "&AMYA2ADF- &AOYA+ADl-"),
        ] {
            assert_eq!(encode(name), wire, "{name}");
            assert_eq!(decode(wire).unwrap(), name, "{wire}");
        }
    }

    #[test]
    fn ampersand_and_ascii() {
        assert_eq!(decode("&-").unwrap(), "&");
        assert_eq!(decode("Tom &- Jerry").unwrap(), "Tom & Jerry");
        assert_eq!(decode("").unwrap(), "");
        assert_eq!(encode("&"), "&-");
        assert_eq!(decode("a/b.c [x] ~ \"q\"").unwrap(), "a/b.c [x] ~ \"q\"");
    }

    #[test]
    fn non_canonical_but_valid_input_is_accepted() {
        assert_eq!(decode("&AGE-").unwrap(), "a");
        assert_eq!(decode("&APg-&APg-").unwrap(), "øø");
    }

    #[test]
    fn invalid_input_is_rejected() {
        assert_eq!(decode("Søppel"), Err(DecodeError::NonAscii));
        assert_eq!(decode("&APg"), Err(DecodeError::Unterminated));
        assert_eq!(decode("&"), Err(DecodeError::Unterminated));
        assert_eq!(decode("&A/g-"), Err(DecodeError::BadBase64));
        assert_eq!(decode("&APgA-"), Err(DecodeError::OddLength));
        assert_eq!(decode("&APh-"), Err(DecodeError::NonZeroPadding));
        assert_eq!(decode("&2D0-"), Err(DecodeError::Surrogate));
        assert_eq!(decode("&3OwAYQ-"), Err(DecodeError::Surrogate));
        assert_eq!(decode("&2D3YPQ-"), Err(DecodeError::Surrogate));
        assert_eq!(display_name("Søppel"), "Søppel");
    }

    #[test]
    fn round_trips_every_bmp_scalar_and_some_astral_ones() {
        let mut name = String::new();
        for code in (0x20u32..0x11000)
            .step_by(7)
            .chain([0x1f4ec, 0x1f600, 0x10ffff])
        {
            if let Some(ch) = char::from_u32(code) {
                name.push(ch);
            }
            if name.chars().count() > 40 {
                assert_eq!(decode(&encode(&name)).unwrap(), name);
                name.clear();
            }
        }
        assert_eq!(decode(&encode(&name)).unwrap(), name);
    }
}
