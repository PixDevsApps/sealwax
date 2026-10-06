//! `mailto:` URIs per RFC 6068. Only the safe header fields become part of a draft: to, cc, bcc,
//! subject, body and in-reply-to. Everything else — `attach`, `attachment`, `from`, `x-…` — is
//! dropped and only its name is reported. Header values lose control characters, so an encoded
//! CR LF can't smuggle another header line in. Ported from spike S6.

use crate::compose::{self, Draft, address, text};
use crate::message::normalize_id;
use crate::model::Address;

const MAX_HEADER_CHARS: usize = 998;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_ADDRESSES: usize = 100;

/// A draft's fields from a `mailto:` URI. Its contents are the user's mail: never log them; its
/// `Debug` prints counts, lengths and the names of the dropped fields only.
#[derive(Clone, Default, PartialEq, Eq)]
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

impl std::fmt::Debug for Mailto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailto")
            .field("to", &self.to.len())
            .field("cc", &self.cc.len())
            .field("bcc", &self.bcc.len())
            .field("subject_len", &self.subject.as_ref().map(String::len))
            .field("body_len", &self.body.as_ref().map(String::len))
            .field("in_reply_to", &self.in_reply_to.is_some())
            .field("ignored", &self.ignored)
            .finish()
    }
}

impl Mailto {
    /// Fills `draft` (a new one, [`compose::new_draft`], with its signature) from the link: its
    /// recipients after any the draft has (each once; addresses Sealwax can't send to are left
    /// out), its subject, its body as the editor's lines in place of the empty line to type in,
    /// and its In-Reply-To.
    pub fn fill(&self, draft: &mut Draft) {
        let add = |list: &mut Vec<Address>, emails: &[String]| {
            let added = emails
                .iter()
                .filter(|email| address::is_valid(email))
                .map(|email| Address::new("", email.clone()));
            *list = address::dedupe(list.drain(..).chain(added));
        };
        add(&mut draft.to, &self.to);
        add(&mut draft.cc, &self.cc);
        add(&mut draft.bcc, &self.bcc);
        if let Some(subject) = &self.subject {
            draft.subject = text::header_text(subject);
        }
        if let Some(body) = self.body.as_deref().filter(|body| !body.trim().is_empty()) {
            let body = text::to_html(body.trim_end_matches('\n'));
            draft.html = match draft.html.strip_prefix(compose::EMPTY_LINE) {
                Some(rest) => format!("{body}{rest}"),
                None => format!("{body}{}", draft.html),
            };
        }
        if let Some(id) = self
            .in_reply_to
            .as_deref()
            .and_then(normalize_id)
            .filter(|id| compose::ids::is_header_safe(id))
        {
            if draft.references.is_empty() {
                draft.references = vec![id.clone()];
            }
            draft.in_reply_to = Some(id);
        }
    }
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
        assert!(!debug.contains("a@example.com"), "{debug}");
        assert!(
            debug.contains("ignored: [\"attach\", \"attachment\"]"),
            "{debug}"
        );
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

    #[test]
    fn bodies_and_address_lists_are_capped() {
        let body = "é".repeat(MAX_BODY_BYTES);
        let parsed = parse(&format!("mailto:?body={body}")).unwrap();
        let kept = parsed.body.unwrap();
        assert!(kept.len() <= MAX_BODY_BYTES && kept.len() > MAX_BODY_BYTES - 2);
        let many: Vec<String> = (0..150).map(|n| format!("p{n}@example.com")).collect();
        let parsed = parse(&format!("mailto:{}", many.join(","))).unwrap();
        assert_eq!(parsed.to.len(), MAX_ADDRESSES);
        assert!(parsed.ignored.is_empty());
    }

    #[test]
    fn a_link_fills_a_new_draft() {
        let sender = crate::account::Identity {
            name: "Ada".into(),
            address: "ada@example.test".into(),
            signature: Some("Ada".into()),
        };
        let mut draft = compose::new_draft(
            crate::model::AccountId(1),
            &sender,
            "id@example.test".into(),
        );
        draft.to = vec![Address::new("Bob", "bob@example.test")];
        let link = parse(
            "mailto:BOB@example.test,%C3%A6@example.org?cc=c@example.test&bcc=d@example.test\
             &subject=Hei%0D%0Ader&body=Linje%20%3Cen%3E%0D%0A%0D%0Ato%0A&in-reply-to=%3Cm1@example.org%3E",
        )
        .unwrap();
        link.fill(&mut draft);
        let emails = |list: &[Address]| list.iter().map(|a| a.email.clone()).collect::<Vec<_>>();
        assert_eq!(emails(&draft.to), ["bob@example.test"]);
        assert_eq!(emails(&draft.cc), ["c@example.test"]);
        assert_eq!(emails(&draft.bcc), ["d@example.test"]);
        assert_eq!(draft.subject, "Hei der");
        assert_eq!(
            draft.html,
            "<div>Linje &lt;en&gt;</div><div><br></div><div>to</div><div><br></div>\
             <div class=\"sw-signature\"><div>--&nbsp;</div><div>Ada</div></div>"
        );
        assert_eq!(draft.in_reply_to.as_deref(), Some("m1@example.org"));
        assert_eq!(draft.references, ["m1@example.org"]);
        let mut bare = Draft::default();
        parse("mailto:x@example.test?body=b")
            .unwrap()
            .fill(&mut bare);
        assert_eq!(bare.html, "<div>b</div>");
    }

    #[test]
    fn the_scheme_is_case_insensitive_and_needs_its_colon() {
        assert!(is_mailto("MailTo:a@example.com"));
        assert!(is_mailto("mailto:"));
        assert!(!is_mailto("mailto"));
        assert!(!is_mailto("mailtox:a@example.com"));
        assert!(!is_mailto("é"));
        assert_eq!(parse("mailto:").unwrap(), Mailto::default());
    }
}
