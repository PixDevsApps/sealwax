//! `mailto:` URIs per RFC 6068. Only the safe header fields become part of a draft: to, cc, bcc,
//! subject, body and in-reply-to. Everything else — `attach`, `attachment`, `from`, `x-…` — is
//! dropped and only its name is reported. Header values lose control characters, so an encoded
//! CR LF can't smuggle another header line in.

const MAX_HEADER_CHARS: usize = 998;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_ADDRESSES: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailto {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub in_reply_to: Option<String>,
    /// Lower-cased names of the header fields that were dropped, plus `invalid-address`.
    pub ignored: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotMailto,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not a mailto: URI")
    }
}

impl std::error::Error for Error {}

pub fn is_mailto(uri: &str) -> bool {
    uri.get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
}

pub fn parse(uri: &str) -> Result<Mailto, Error> {
    if !is_mailto(uri) {
        return Err(Error::NotMailto);
    }
    let rest = &uri[7..];
    let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let mut out = Mailto::default();
    add_addresses(&mut out.to, path, &mut out.ignored);
    for field in query.into_iter().flat_map(|query| query.split('&')) {
        if field.is_empty() {
            continue;
        }
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        let name = String::from_utf8_lossy(&decode(name)).to_ascii_lowercase();
        match name.as_str() {
            "to" => add_addresses(&mut out.to, value, &mut out.ignored),
            "cc" => add_addresses(&mut out.cc, value, &mut out.ignored),
            "bcc" => add_addresses(&mut out.bcc, value, &mut out.ignored),
            "subject" if out.subject.is_none() => out.subject = Some(header_value(value)),
            "body" if out.body.is_none() => out.body = Some(body_value(value)),
            "in-reply-to" if out.in_reply_to.is_none() => {
                out.in_reply_to = Some(header_value(value));
            }
            "subject" | "body" | "in-reply-to" => {}
            _ => ignore(&mut out.ignored, name),
        }
    }
    Ok(out)
}

fn ignore(ignored: &mut Vec<String>, name: String) {
    if !ignored.contains(&name) {
        ignored.push(name);
    }
}

/// Percent-decodes to bytes. `+` stays `+` (RFC 6068 is not form encoding); a `%` that doesn't
/// start a valid escape is kept as it is.
fn decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = bytes.get(index + 1..index + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

fn header_value(raw: &str) -> String {
    let text = String::from_utf8_lossy(&decode(raw)).into_owned();
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().chars().take(MAX_HEADER_CHARS).collect()
}

fn body_value(raw: &str) -> String {
    let text = String::from_utf8_lossy(&decode(raw)).replace("\r\n", "\n");
    let mut out: String = text
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    if out.len() > MAX_BODY_BYTES {
        let mut end = MAX_BODY_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

fn add_addresses(list: &mut Vec<String>, raw: &str, ignored: &mut Vec<String>) {
    let text = String::from_utf8_lossy(&decode(raw)).into_owned();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match address(part) {
            Some(address) if list.len() < MAX_ADDRESSES => list.push(address),
            Some(_) => {}
            None => ignore(ignored, "invalid-address".to_owned()),
        }
    }
}

/// An `addr-spec`, also accepted inside `<…>` as some generators write it.
fn address(text: &str) -> Option<String> {
    let inner = match (text.find('<'), text.rfind('>')) {
        (Some(open), Some(close)) if open < close => &text[open + 1..close],
        _ => text,
    };
    let inner = inner.trim();
    let (local, domain) = inner.rsplit_once('@')?;
    let valid = !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && inner.len() <= 254
        && !inner.chars().any(|c| c.is_control() || c.is_whitespace());
    valid.then(|| inner.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spike_example() {
        let parsed = parse("mailto:test@example.com?subject=Hi").unwrap();
        assert_eq!(parsed.to, ["test@example.com"]);
        assert_eq!(parsed.subject.as_deref(), Some("Hi"));
        assert!(parsed.ignored.is_empty());
    }

    #[test]
    fn attachments_are_dropped_in_any_spelling() {
        let parsed = parse(
            "mailto:a@example.com?attach=%2Fetc%2Fpasswd&ATTACHMENT=~/.ssh/id_ed25519\
             &Attach=file:///home&subject=x",
        )
        .unwrap();
        assert_eq!(parsed.ignored, ["attach", "attachment"]);
        assert_eq!(parsed.subject.as_deref(), Some("x"));
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("passwd") && !debug.contains("ssh") && !debug.contains("file:"));
    }

    #[test]
    fn unsafe_and_unknown_fields_are_dropped() {
        let parsed =
            parse("mailto:a@example.com?from=boss@example.org&X-Mailer=x&reply-to=c@example.org")
                .unwrap();
        assert_eq!(parsed.ignored, ["from", "x-mailer", "reply-to"]);
        assert_eq!(parsed.to, ["a@example.com"]);
    }

    #[test]
    fn encoded_line_breaks_cannot_add_headers() {
        let parsed = parse("mailto:a@example.com?subject=Hi%0D%0ABcc:%20evil@example.org").unwrap();
        assert_eq!(parsed.subject.as_deref(), Some("Hi  Bcc: evil@example.org"));
        assert!(parsed.bcc.is_empty());
        let parsed = parse("mailto:a@example.com?in-reply-to=%3Cx@example.org%3E%0ATo:b").unwrap();
        assert!(!parsed.in_reply_to.unwrap().contains('\n'));
    }

    #[test]
    fn utf8_plus_and_line_breaks() {
        let parsed =
            parse("mailto:?to=x@example.com&subject=Bl%C3%A5b%C3%A6r+syltet%C3%B8y&body=Hei%0D%0Ap%C3%A5 deg%00")
                .unwrap();
        assert_eq!(parsed.to, ["x@example.com"]);
        assert_eq!(parsed.subject.as_deref(), Some("Blåbær+syltetøy"));
        assert_eq!(parsed.body.as_deref(), Some("Hei\npå deg"));
    }

    #[test]
    fn several_recipients_and_fields() {
        let parsed = parse(
            "MAILTO:a@example.com,b@example.com?to=c@example.com%2Cd@example.com&cc=e@example.com\
             &bcc=f@example.com&subject=first&subject=second&body=b",
        )
        .unwrap();
        assert_eq!(
            parsed.to,
            [
                "a@example.com",
                "b@example.com",
                "c@example.com",
                "d@example.com"
            ]
        );
        assert_eq!(parsed.cc, ["e@example.com"]);
        assert_eq!(parsed.bcc, ["f@example.com"]);
        assert_eq!(parsed.subject.as_deref(), Some("first"));
    }

    #[test]
    fn invalid_input() {
        assert_eq!(parse("https://example.com"), Err(Error::NotMailto));
        assert_eq!(parse("mail"), Err(Error::NotMailto));
        let parsed = parse("mailto:not-an-address,ok@example.com?to=%00x@y&subject=%ZZ%2").unwrap();
        assert_eq!(parsed.to, ["ok@example.com"]);
        assert_eq!(parsed.ignored, ["invalid-address"]);
        assert_eq!(parsed.subject.as_deref(), Some("%ZZ%2"));
        let parsed = parse("mailto:Kari%20Nordmann%20%3Ckari@example.com%3E#frag").unwrap();
        assert_eq!(parsed.to, ["kari@example.com"]);
    }

    #[test]
    fn long_values_are_bounded() {
        let long = "a".repeat(5000);
        let parsed = parse(&format!("mailto:x@example.com?subject={long}&body={long}")).unwrap();
        assert_eq!(parsed.subject.unwrap().len(), MAX_HEADER_CHARS);
        assert_eq!(parsed.body.unwrap().len(), 5000);
    }
}
