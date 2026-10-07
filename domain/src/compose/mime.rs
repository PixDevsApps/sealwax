//! The MIME builder (ADR-007 and its S7 amendment, ML-073), ported from S7
//! (`spikes/s7-composer/src/mime.rs`). mail-builder 1.0 without `gethostname`, the tree built
//! by hand as `mixed{alternative{text, related{html, inline…}}, attachments…}`, `related` left
//! out without inline images and `mixed` without attachments. Three copies of a draft:
//!
//! - **the transmitted copy**, handed to SMTP: never a Bcc field, `To: "undisclosed-recipients":;`
//!   when every recipient is in Bcc;
//! - **the Sent copy**: the transmitted bytes plus one `Bcc:` field before `Subject:`;
//! - **the Drafts folder's copy**: Bcc kept, no undisclosed-recipients group.
//!
//! The Message-ID is the draft's own, on the sender's domain; `Date:` is UTC; the plain part is
//! the HTML through html2text at 78 columns. No host name anywhere, and no User-Agent or
//! X-Mailer field (accepted at the M6 gate). Everything is 7-bit with CRLF line ends. Part bytes come from
//! the caller (the blob store); nothing here does I/O.

use super::address;
use super::ids;
use super::text::header_text;
use super::{Draft, DraftPart};
use crate::model::Address;
use crate::{html_depth, message};
use mail_builder::MessageBuilder;
use mail_builder::encoders::base64::Base64Encoder;
use mail_builder::headers::Header;
use mail_builder::headers::address::Address as Mailbox;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::mime::MimePart;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The plain part's line width.
pub const TEXT_WIDTH: usize = 78;

/// A header field with an address in it; errors name the field, never the address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    From,
    To,
    Cc,
    Bcc,
}

/// Why a draft can't be built. Never quotes the draft's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// An address that isn't valid, or whose local part isn't ASCII (1.0 sends without
    /// SMTPUTF8).
    Address(Field),
    /// No To, Cc or Bcc.
    NoRecipients,
    /// The draft's Message-ID is missing, malformed, or not on the sender's domain (it is made
    /// again when the sender's domain changes, so it never names another identity's domain).
    MessageId,
    /// `parts` doesn't hold one slice per draft part of its size, or a Content-ID can't be
    /// written.
    Parts,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Address(field) => write!(f, "an address in {field:?} can't be sent to"),
            BuildError::NoRecipients => f.write_str("the message has no recipients"),
            BuildError::MessageId => f.write_str("the Message-ID is not on the sender's domain"),
            BuildError::Parts => f.write_str("the attachments don't match the draft"),
        }
    }
}

impl std::error::Error for BuildError {}

/// SMTP's envelope: MAIL FROM and every RCPT TO once, in ASCII. Its `Debug` prints counts only.
#[derive(Clone, PartialEq, Eq)]
pub struct Envelope {
    pub mail_from: String,
    pub recipients: Vec<String>,
}

impl fmt::Debug for Envelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Envelope")
            .field("recipients", &self.recipients.len())
            .finish()
    }
}

/// A draft ready to send. Its `Debug` prints lengths only.
#[derive(Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// The copy handed to SMTP, built without ever seeing the Bcc list.
    pub transmitted: Vec<u8>,
    /// The Sent folder's copy: the transmitted bytes plus one Bcc field.
    pub sent_copy: Vec<u8>,
    pub envelope: Envelope,
}

impl fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outgoing")
            .field("transmitted", &self.transmitted.len())
            .field("sent_copy", &self.sent_copy.len())
            .field("envelope", &self.envelope)
            .finish()
    }
}

/// The transmitted copy, the Sent copy and the envelope of `draft`, dated `date` (Unix seconds,
/// written in UTC). `parts` holds the bytes of `draft.parts`, in order.
pub fn build(draft: &Draft, date: i64, parts: &[&[u8]]) -> Result<Outgoing, BuildError> {
    let envelope = envelope(draft)?;
    let transmitted = write(draft, date, parts, Copy::Transmitted)?;
    let sent_copy = with_bcc(&transmitted, &mailboxes(&draft.bcc, Field::Bcc)?);
    Ok(Outgoing {
        transmitted,
        sent_copy,
        envelope,
    })
}

/// The Drafts folder's copy of `draft`, saved at `date`: Bcc kept, recipients optional.
pub fn drafts_copy(draft: &Draft, date: i64, parts: &[&[u8]]) -> Result<Vec<u8>, BuildError> {
    write(draft, date, parts, Copy::Drafts)
}

/// MAIL FROM and RCPT TO for `draft`: To, Cc and Bcc, each mailbox once.
pub fn envelope(draft: &Draft) -> Result<Envelope, BuildError> {
    let mail_from = address::ascii(&draft.from.email).ok_or(BuildError::Address(Field::From))?;
    let mut recipients: Vec<String> = Vec::new();
    for (field, list) in [
        (Field::To, &draft.to),
        (Field::Cc, &draft.cc),
        (Field::Bcc, &draft.bcc),
    ] {
        for recipient in list {
            let ascii = address::ascii(&recipient.email).ok_or(BuildError::Address(field))?;
            if !recipients.iter().any(|known| address::same(known, &ascii)) {
                recipients.push(ascii);
            }
        }
    }
    if recipients.is_empty() {
        return Err(BuildError::NoRecipients);
    }
    Ok(Envelope {
        mail_from,
        recipients,
    })
}

/// The encoded size of `draft` as sent, for the composer's check against the server's SIZE
/// limit while the user writes, without reading the parts: an estimate a little above what
/// [`build`] makes (the parts' base64 and the HTML's quoted-printable are counted exactly, the
/// plain part as large as the HTML, headers generously).
pub fn estimate(draft: &Draft) -> u64 {
    let html = encoded_text_len(&html_document(&draft.html)) as u64;
    let recipients = (draft.to.len() + draft.cc.len() + draft.bcc.len()) as u64;
    let headers = 1_024
        + 2 * draft.subject.len() as u64
        + 160 * recipients
        + 80 * draft.references.len() as u64;
    let parts: u64 = draft
        .parts
        .iter()
        .map(|part| base64_len(part.size) + 2 * part.name.len() as u64 + 400)
        .sum();
    headers + 2 * html + 600 + parts
}

/// Quoted-printable or base64, whichever mail-builder would pick, for UTF-8 text with CRLF
/// line ends: an upper bound.
fn encoded_text_len(text: &str) -> usize {
    let quoted: usize = text
        .bytes()
        .map(|byte| {
            if byte.is_ascii_graphic() || byte == b' ' || byte == b'\n' {
                1
            } else {
                3
            }
        })
        .sum::<usize>()
        * 77
        / 73
        + text.matches('\n').count();
    quoted.min(base64_len(text.len() as u64) as usize)
}

fn base64_len(size: u64) -> u64 {
    let encoded = size.div_ceil(3) * 4;
    encoded + encoded.div_ceil(76) * 2
}

/// The full HTML document a draft's body is sent as.
pub fn html_document(fragment: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta http-equiv=\"Content-Type\" \
         content=\"text/html; charset=utf-8\">\n</head>\n<body>\n{fragment}\n</body>\n</html>\n"
    )
}

/// The plain part: the HTML through html2text at [`TEXT_WIDTH`] columns, the signature's
/// separator line as `-- ` (RFC 3676). HTML nested deeper than [`html_depth::MAX_DEPTH`] goes
/// through mail-parser's simple converter, whole.
pub fn plain_text(html: &str) -> String {
    let text = if html_depth::too_deep(html) {
        mail_parser::decoders::html::html_to_text(html)
    } else {
        let converted = catch_unwind(AssertUnwindSafe(|| {
            html_depth::render(html2text::config::plain(), html, TEXT_WIDTH)
        }));
        match converted {
            Ok(Ok(text)) => text,
            _ => message::html_to_text(html),
        }
    };
    text.lines()
        .map(|line| match line {
            "--\u{a0}" | "-- \u{a0}" => "-- ",
            line => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Copy {
    Transmitted,
    Drafts,
}

fn write(draft: &Draft, date: i64, parts: &[&[u8]], copy: Copy) -> Result<Vec<u8>, BuildError> {
    let message_id = draft.message_id.trim();
    let id_domain = message_id.rsplit_once('@').map(|(_, domain)| domain);
    if !ids::is_header_safe(message_id)
        || id_domain.is_none()
        || id_domain != ids::ascii_domain(&draft.from.email).as_deref()
    {
        return Err(BuildError::MessageId);
    }
    if parts.len() != draft.parts.len()
        || draft
            .parts
            .iter()
            .zip(parts)
            .any(|(part, bytes)| part.size != bytes.len() as u64)
    {
        return Err(BuildError::Parts);
    }
    let from = mailbox(&draft.from, Field::From)?;
    let to = mailboxes(&draft.to, Field::To)?;
    let cc = mailboxes(&draft.cc, Field::Cc)?;
    let bcc = mailboxes(&draft.bcc, Field::Bcc)?;
    let mut builder = MessageBuilder::new().from(from);
    match (&to, &cc) {
        (None, None) if copy == Copy::Transmitted => {
            builder = builder.to(Mailbox::new_group(
                Some("undisclosed-recipients"),
                Vec::new(),
            ));
        }
        _ => {
            if let Some(to) = to {
                builder = builder.to(to);
            }
        }
    }
    if let Some(cc) = cc {
        builder = builder.cc(cc);
    }
    if copy == Copy::Drafts
        && let Some(bcc) = bcc
    {
        builder = builder.bcc(bcc);
    }
    builder = builder
        .subject(header_text(&draft.subject))
        .date(Date::new(date))
        .message_id(message_id.to_owned());
    if let Some(id) = draft
        .in_reply_to
        .as_deref()
        .filter(|id| ids::is_header_safe(id))
    {
        builder = builder.in_reply_to(id.to_owned());
    }
    let references: Vec<String> = draft
        .references
        .iter()
        .filter(|id| ids::is_header_safe(id))
        .cloned()
        .collect();
    if !references.is_empty() {
        builder = builder.references(MessageId::from(references));
    }
    let html = crlf(&html_document(&draft.html));
    let text = crlf(&plain_text(&html));
    let body = body(&html, &text, &draft.parts, parts)?;
    builder
        .body(body)
        .write_to_vec()
        .map_err(|_| BuildError::Parts)
}

/// `mixed{ alternative{ text, related{ html, inline… } }, attachments… }`. An inline part the
/// HTML no longer shows is left out.
fn body<'a>(
    html: &'a str,
    text: &'a str,
    parts: &'a [DraftPart],
    bytes: &'a [&'a [u8]],
) -> Result<MimePart<'a>, BuildError> {
    let mut inline = Vec::new();
    let mut attachments = Vec::new();
    for (part, data) in parts.iter().zip(bytes) {
        let mime_type = mime_type(&part.mime_type);
        match &part.content_id {
            Some(id) if !ids::is_header_safe(id) => return Err(BuildError::Parts),
            Some(id) if html.contains(&format!("cid:{id}")) => {
                let mut disposition = ContentType::new("inline");
                if !part.name.is_empty() {
                    disposition = disposition.attribute("filename", part.name.as_str());
                }
                inline.push(
                    MimePart::new(mime_type, *data)
                        .cid(id.as_str())
                        .header("Content-Disposition", disposition),
                );
            }
            Some(_) => {}
            None => {
                let mut content_type = ContentType::new(mime_type);
                if mime_type.starts_with("text/") && std::str::from_utf8(data).is_ok() {
                    content_type = content_type.attribute("charset", "utf-8");
                }
                let content_type = content_type.attribute("name", rfc2047_parameter(&part.name));
                attachments.push(MimePart::new(content_type, *data).attachment(part.name.as_str()));
            }
        }
    }
    let html_part = MimePart::new("text/html", html);
    let rich = if inline.is_empty() {
        html_part
    } else {
        let mut children = vec![html_part];
        children.extend(inline);
        MimePart::new(
            ContentType::new("multipart/related").attribute("type", "text/html"),
            children,
        )
    };
    let alternative = MimePart::new(
        "multipart/alternative",
        vec![MimePart::new("text/plain", text), rich],
    );
    if attachments.is_empty() {
        return Ok(alternative);
    }
    let mut children = vec![alternative];
    children.extend(attachments);
    Ok(MimePart::new("multipart/mixed", children))
}

/// A part's type as it may be written: `type/subtype` of token characters, else
/// `application/octet-stream` (a forwarded part's type comes from hostile mail).
fn mime_type(mime_type: &str) -> &str {
    let token = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    match mime_type.split_once('/') {
        Some((kind, subtype)) if token(kind) && token(subtype) => mime_type,
        _ => "application/octet-stream",
    }
}

/// The legacy `name=` parameter as one RFC 2047 encoded word, beside the RFC 2231 `filename*`
/// mail-builder writes; clients that predate RFC 2231 read this one.
fn rfc2047_parameter(value: &str) -> String {
    if value.is_ascii() {
        return value.to_owned();
    }
    let encoded = Base64Encoder::new()
        .encode(value.as_bytes())
        .unwrap_or_default();
    format!("=?UTF-8?B?{}?=", String::from_utf8_lossy(&encoded))
}

fn mailbox(address: &Address, field: Field) -> Result<Mailbox<'static>, BuildError> {
    let email = address::ascii(&address.email).ok_or(BuildError::Address(field))?;
    let name = header_text(&address.name);
    Ok(Mailbox::new_address(
        (!name.is_empty()).then_some(name),
        email,
    ))
}

fn mailboxes(list: &[Address], field: Field) -> Result<Option<Mailbox<'static>>, BuildError> {
    let mut boxes = list
        .iter()
        .map(|address| mailbox(address, field))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match boxes.len() {
        0 => None,
        1 => boxes.pop(),
        _ => Some(Mailbox::new_list(boxes)),
    })
}

/// Inserts the Bcc field before `Subject:`; every other byte stays the same.
fn with_bcc(transmitted: &[u8], bcc: &Option<Mailbox<'_>>) -> Vec<u8> {
    let Some(bcc) = bcc else {
        return transmitted.to_vec();
    };
    let header_end = find(transmitted, b"\r\n\r\n").unwrap_or(transmitted.len());
    let at = find(&transmitted[..header_end], b"\r\nSubject: ").map_or(0, |at| at + 2);
    let mut field = b"Bcc: ".to_vec();
    bcc.write_header(&mut field, 5);
    let mut copy = Vec::with_capacity(transmitted.len() + field.len());
    copy.extend_from_slice(&transmitted[..at]);
    copy.extend_from_slice(&field);
    copy.extend_from_slice(&transmitted[at..]);
    copy
}

fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests;
