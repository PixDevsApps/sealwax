//! Just enough MIME writing for the corpus: base64 and quoted-printable bodies, RFC 2047
//! encoded words, RFC 2231 file names, RFC 5322 dates and folded header lines, all with CRLF.

use encoding_rs::Encoding;

pub const CRLF: &str = "\r\n";

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(BASE64[(n >> shift & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Base64 in lines of 76 characters.
pub fn base64_lines(bytes: &[u8]) -> String {
    let encoded = base64(bytes);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 38);
    for line in encoded.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        out.push_str(CRLF);
    }
    out
}

/// Quoted-printable (RFC 2045) with soft line breaks at 76 characters.
pub fn quoted_printable(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3 / 2);
    for (index, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if index > 0 {
            out.push_str(CRLF);
        }
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let mut width = 0;
        for (at, byte) in line.iter().enumerate() {
            let last = at + 1 == line.len();
            let literal = matches!(byte, b'!'..=b'<' | b'>'..=b'~') || (*byte == b' ' && !last);
            let piece = if literal {
                char::from(*byte).to_string()
            } else {
                format!("={byte:02X}")
            };
            if width + piece.len() > 75 {
                out.push('=');
                out.push_str(CRLF);
                width = 0;
            }
            width += piece.len();
            out.push_str(&piece);
        }
    }
    out
}

/// A display name or subject as RFC 2047 encoded words in `encoding`, base64, split at
/// character boundaries so no word passes 75 characters.
pub fn encoded_words(text: &str, encoding: &'static Encoding) -> String {
    let mut words = Vec::new();
    let mut piece = String::new();
    for c in text.chars() {
        piece.push(c);
        if encoding.encode(&piece).0.len() > 36 {
            piece.pop();
            words.push(std::mem::take(&mut piece));
            piece.push(c);
        }
    }
    if !piece.is_empty() {
        words.push(piece);
    }
    words
        .iter()
        .map(|word| {
            format!(
                "=?{}?B?{}?=",
                encoding.name(),
                base64(&encoding.encode(word).0)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// RFC 2047 Q-encoding in ISO-8859-1, for text whose characters all fit it.
pub fn latin1_q(text: &str) -> Option<String> {
    let mut out = String::from("=?ISO-8859-1?Q?");
    for c in text.chars() {
        let code = u32::from(c);
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => out.push(c),
            ' ' => out.push('_'),
            _ if code <= 0xFF => out.push_str(&format!("={code:02X}")),
            _ => return None,
        }
    }
    out.push_str("?=");
    Some(out)
}

/// A header value: ASCII as it is, anything else as UTF-8 encoded words.
pub fn text(value: &str) -> String {
    if value.is_ascii() {
        value.to_owned()
    } else {
        encoded_words(value, encoding_rs::UTF_8)
    }
}

/// `"Name" <addr>`, with the name encoded or quoted as needed.
pub fn mailbox(name: &str, addr: &str, name_encoding: NameEncoding) -> String {
    if name.is_empty() {
        return format!("<{addr}>");
    }
    let name = if name.is_ascii() {
        if name.chars().any(|c| "()<>[]:;@\\,.\"".contains(c)) {
            format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
        } else {
            name.to_owned()
        }
    } else {
        match name_encoding {
            NameEncoding::Utf8Base64 => encoded_words(name, encoding_rs::UTF_8),
            NameEncoding::Latin1Q => {
                latin1_q(name).unwrap_or_else(|| encoded_words(name, encoding_rs::UTF_8))
            }
            NameEncoding::Legacy(encoding) if !encoding.encode(name).2 => {
                encoded_words(name, encoding)
            }
            NameEncoding::Legacy(_) => encoded_words(name, encoding_rs::UTF_8),
        }
    };
    format!("{name} <{addr}>")
}

#[derive(Clone, Copy, Debug)]
pub enum NameEncoding {
    Utf8Base64,
    Latin1Q,
    Legacy(&'static Encoding),
}

/// A header field folded at spaces so lines stay under 78 characters where possible.
pub fn field(name: &str, value: &str) -> String {
    let mut out = format!("{name}:");
    let mut width = out.len();
    for word in value.split(' ').filter(|word| !word.is_empty()) {
        if width + 1 + word.len() > 76 && width > name.len() + 1 {
            out.push_str(CRLF);
            width = 0;
        }
        out.push(' ');
        out.push_str(word);
        width += 1 + word.len();
    }
    out.push_str(CRLF);
    out
}

/// RFC 2231 `filename*` for a UTF-8 name.
pub fn filename_star(name: &str) -> String {
    let mut out = String::from("UTF-8''");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b".-_".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Days since 1970-01-01 as (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// An RFC 5322 date for `time` shown in the zone `offset_minutes` east of UTC.
pub fn date(time: i64, offset_minutes: i32) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let local = time + i64::from(offset_minutes) * 60;
    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let offset = offset_minutes.unsigned_abs();
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} {sign}{:02}{:02}",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[month as usize - 1],
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        offset / 60,
        offset % 60,
    )
}

pub fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_the_references() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(
            encoded_words("Åse Bråten", encoding_rs::UTF_8),
            "=?UTF-8?B?w4VzZSBCcsOldGVu?="
        );
        assert_eq!(
            encoded_words("会議の予定", encoding_rs::ISO_2022_JP),
            "=?ISO-2022-JP?B?GyRCMnE1RCROTT1EahsoQg==?="
        );
        assert_eq!(
            latin1_q("Øystein Grønli").unwrap(),
            "=?ISO-8859-1?Q?=D8ystein_Gr=F8nli?="
        );
        assert_eq!(filename_star("møte.pdf"), "UTF-8''m%C3%B8te.pdf");
        assert_eq!(date(1_791_185_400, 120), "Mon, 05 Oct 2026 09:30:00 +0200");
        assert_eq!(date(0, -300), "Wed, 31 Dec 1969 19:00:00 -0500");
        assert_eq!(
            quoted_printable("blåbær = ok \n".as_bytes()),
            "bl=C3=A5b=C3=A6r =3D ok=20\r\n"
        );
        let long = quoted_printable("x".repeat(200).as_bytes());
        assert!(long.split("\r\n").all(|line| line.len() <= 76));
    }

    #[test]
    fn folds_long_fields() {
        let subject = "word ".repeat(40);
        let folded = field("Subject", subject.trim());
        assert!(
            folded.split("\r\n").all(|line| line.len() <= 78),
            "{folded}"
        );
        assert!(folded.starts_with("Subject: word"));
    }
}
