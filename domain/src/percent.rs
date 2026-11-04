//! Percent-decoding (RFC 3986 section 2.1) as Sealwax reads URLs: `mailto:` links (RFC 6068),
//! `cid:` URLs (RFC 2392, the Content-ID escaped), link targets, and autoconfig redirects. A `%`
//! that doesn't start a valid escape is kept as it is; nothing here fails.

/// `text` with its `%xx` escapes decoded, as bytes. `+` stays `+`: URLs aren't form encoding.
pub fn decode_bytes(text: &str) -> Vec<u8> {
    decode_with(text, false)
}

/// [`decode_bytes`] read as UTF-8, invalid sequences replaced.
pub fn decode(text: &str) -> String {
    String::from_utf8_lossy(&decode_bytes(text)).into_owned()
}

/// A form value (`application/x-www-form-urlencoded`): [`decode`], with `+` a space.
pub fn decode_form(text: &str) -> String {
    String::from_utf8_lossy(&decode_with(text, true)).into_owned()
}

fn decode_with(text: &str, plus_is_space: bool) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| char::from(byte).to_digit(16);
        match (
            bytes[at],
            bytes.get(at + 1).copied().and_then(hex),
            bytes.get(at + 2).copied().and_then(hex),
        ) {
            (b'%', Some(high), Some(low)) => {
                out.extend(u8::try_from(high * 16 + low));
                at += 3;
            }
            (b'+', ..) if plus_is_space => {
                out.push(b' ');
                at += 1;
            }
            (byte, ..) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_decode_and_broken_ones_stay() {
        assert_eq!(decode("logo@example.test"), "logo@example.test");
        assert_eq!(decode("image001.png%4001D9"), "image001.png@01D9");
        assert_eq!(decode("a%2"), "a%2");
        assert_eq!(decode("a%zz%"), "a%zz%");
        assert_eq!(decode("%zz%41"), "%zzA");
        assert_eq!(decode("%C3%A6+x"), "æ+x");
        assert_eq!(decode("%FF"), "\u{fffd}");
        assert_eq!(decode_bytes("%FF%00"), [0xff, 0]);
        assert_eq!(decode_form("a+b%2Bc"), "a b+c");
    }
}
