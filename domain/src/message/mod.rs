//! Parsing raw RFC 5322 messages into what the store keeps: decoded headers, a snippet, the
//! body structure and attachment metadata, and the body as text for the search index. Built on
//! mail-parser with `full_encoding`; never panics, whatever the input.

mod html;

use crate::model::{Address, Attachment, BodyKind, Headers, ThreadIndex};
use crate::snippet::{self, SNIPPET_CHARS};
use mail_parser::decoders::base64::base64_decode;
use mail_parser::{HeaderValue, Message, MessageParser, MimeHeaders, PartType};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use html::{HTML_PARSE_LIMIT, html_to_text};

/// Body text kept for the search index, in bytes.
pub const SEARCH_TEXT_LIMIT: usize = 256 * 1024;
/// Subjects and display names are cut to this many characters.
const FIELD_CHARS: usize = 2_000;
/// References kept for threading: the first (the root) and the newest ones.
const MAX_REFERENCES: usize = 50;

/// What the store needs from a raw message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedMessage {
    pub headers: Headers,
    /// About 200 characters of the message's own text.
    pub snippet: String,
    pub body_kind: BodyKind,
    /// mail-parser part indexes of the text/plain bodies.
    pub text_parts: Vec<u32>,
    /// mail-parser part indexes of the text/html bodies.
    pub html_parts: Vec<u32>,
    pub attachments: Vec<Attachment>,
    /// The body as plain text (the HTML converted when there is no plain alternative), at most
    /// [`SEARCH_TEXT_LIMIT`] bytes.
    pub body_text: String,
    /// The raw length in bytes.
    pub size: u64,
}

impl ParsedMessage {
    /// Whether the list shows the attachment clip: any attachment that isn't an image the HTML
    /// body embeds through `cid:`.
    pub fn has_attachments(&self) -> bool {
        self.attachments
            .iter()
            .any(|attachment| !attachment.is_embedded())
    }
}

/// Parses a raw message. Input without any header yields an empty result with only the size,
/// and so does a panic in a parser beneath: one message must never stop a sync (ML-093).
pub fn parse(raw: &[u8]) -> ParsedMessage {
    let size = raw.len() as u64;
    catch_unwind(AssertUnwindSafe(|| parse_message(raw))).unwrap_or_else(|_| ParsedMessage {
        size,
        ..ParsedMessage::default()
    })
}

fn parse_message(raw: &[u8]) -> ParsedMessage {
    let size = raw.len() as u64;
    let Some(message) = MessageParser::default().parse(raw) else {
        return ParsedMessage {
            size,
            ..ParsedMessage::default()
        };
    };
    let text_parts = body_parts(&message, &message.text_body, |body| {
        matches!(body, PartType::Text(_))
    });
    let html_parts = body_parts(&message, &message.html_body, |body| {
        matches!(body, PartType::Html(_))
    });
    let body_text = body_text(&message);
    ParsedMessage {
        headers: headers(&message),
        snippet: snippet::snippet(&body_text, SNIPPET_CHARS),
        body_kind: BodyKind::from_parts(!text_parts.is_empty(), !html_parts.is_empty()),
        text_parts,
        html_parts,
        attachments: attachments(&message),
        body_text,
        size,
    }
}

/// One header field as rules read it (ADR-020): the name in lower case, the value unfolded and
/// RFC 2047-decoded, and for an address field each address as `local@domain`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeaderField {
    pub name: String,
    pub value: String,
    pub addresses: Vec<String>,
}

/// The header fields of a raw message, or of its header alone, in order. Input without a header
/// yields none, and so does a panic in the parser beneath.
pub fn header_fields(raw: &[u8]) -> Vec<HeaderField> {
    catch_unwind(AssertUnwindSafe(|| fields(raw))).unwrap_or_default()
}

fn fields(raw: &[u8]) -> Vec<HeaderField> {
    let Some(message) = MessageParser::default().parse_headers(raw) else {
        return Vec::new();
    };
    message
        .headers()
        .iter()
        .map(|header| {
            let (value, addresses) = match header.value() {
                HeaderValue::Text(text) => (text.to_string(), Vec::new()),
                HeaderValue::TextList(list) => (list.join(", "), Vec::new()),
                HeaderValue::Address(address) => {
                    let text = address
                        .iter()
                        .map(|addr| match (addr.name(), addr.address()) {
                            (Some(name), Some(email)) => format!("{name} <{email}>"),
                            (None, Some(email)) => email.to_owned(),
                            (Some(name), None) => name.to_owned(),
                            (None, None) => String::new(),
                        })
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let emails = address
                        .iter()
                        .filter_map(|addr| addr.address())
                        .map(str::to_owned)
                        .collect();
                    (text, emails)
                }
                _ => (
                    unfold(raw, header.offset_start, header.offset_end),
                    Vec::new(),
                ),
            };
            HeaderField {
                name: header.name().to_ascii_lowercase(),
                value: unfold_text(&value).trim().to_owned(),
                addresses,
            }
        })
        .collect()
}

/// A header's raw value.
fn unfold(raw: &[u8], start: u32, end: u32) -> String {
    let bytes = raw.get(start as usize..end as usize).unwrap_or_default();
    String::from_utf8_lossy(bytes).into_owned()
}

/// `text` without its line breaks: a folded line's leading space stays (RFC 5322 section 2.2.3).
fn unfold_text(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '\r' | '\n')).collect()
}

/// The subject without reply and forward prefixes (English, Nordic, German, French, Chinese).
pub fn base_subject(subject: &str) -> &str {
    const PREFIXES: [&str; 14] = [
        "re", "sv", "fw", "fwd", "vs", "aw", "wg", "antw", "ref", "tr", "回复", "答复", "转发",
        "回覆",
    ];
    let mut rest = subject.trim_start();
    loop {
        let Some((colon, width)) = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ':' || *c == '：')
            .map(|(index, c)| (index, c.len_utf8()))
        else {
            return rest;
        };
        let prefix = rest[..colon].trim_end();
        let prefix = match prefix.find('[') {
            Some(bracket) if prefix.ends_with(']') => &prefix[..bracket],
            _ => prefix,
        };
        if !PREFIXES
            .iter()
            .any(|known| prefix.eq_ignore_ascii_case(known))
        {
            return rest;
        }
        rest = rest[colon + width..].trim_start();
    }
}

/// One MIME part of a raw message, by its mail-parser index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub mime_type: String,
    /// Decoded text for text parts.
    pub text: Option<String>,
    /// The decoded content.
    pub data: Vec<u8>,
}

/// The part at `index`, as [`ParsedMessage`]'s part references name it.
pub fn part(raw: &[u8], index: u32) -> Option<Part> {
    let message = MessageParser::default().parse(raw)?;
    let part = message.parts.get(index as usize)?;
    if part.is_multipart() {
        return None;
    }
    Some(Part {
        mime_type: mime_type(part),
        text: match &part.body {
            PartType::Text(text) | PartType::Html(text) => Some(text.to_string()),
            _ => None,
        },
        data: part.contents().to_vec(),
    })
}

/// The part whose Content-ID is `content_id` (without angle brackets), for `cid:` links.
pub fn part_by_content_id(raw: &[u8], content_id: &str) -> Option<Part> {
    let message = MessageParser::default().parse(raw)?;
    let index = message.parts.iter().position(|part| {
        !part.is_multipart()
            && part.content_id().and_then(normalize_id).as_deref() == Some(content_id)
    })?;
    part(raw, u32::try_from(index).ok()?)
}

fn headers(message: &Message<'_>) -> Headers {
    Headers {
        from: addresses(message.from()),
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        reply_to: addresses(message.reply_to()),
        subject: message.subject().map(clean).unwrap_or_default(),
        date: message.date().and_then(|date| {
            (1970..=2200)
                .contains(&date.year)
                .then(|| date.to_timestamp())
        }),
        message_id: message.message_id().and_then(normalize_id),
        in_reply_to: ids(message.in_reply_to()),
        references: limit_references(ids(message.references())),
        list_id: list_id(message.list_id()),
        thread_index: message.header_raw("Thread-Index").and_then(thread_index),
    }
}

fn clean(text: &str) -> String {
    snippet::collapse(text, FIELD_CHARS)
}

fn addresses(value: Option<&mail_parser::Address<'_>>) -> Vec<Address> {
    let Some(value) = value else {
        return Vec::new();
    };
    value
        .iter()
        .filter_map(|addr| {
            let name = clean(addr.name().unwrap_or_default());
            let email = clean(addr.address().unwrap_or_default());
            (!name.is_empty() || !email.is_empty()).then_some(Address { name, email })
        })
        .collect()
}

/// A Message-ID or Content-ID without angle brackets; `None` when empty or not an id at all.
pub fn normalize_id(id: &str) -> Option<String> {
    let id = id.trim();
    let id = id.strip_prefix('<').unwrap_or(id);
    let id = id.strip_suffix('>').unwrap_or(id).trim();
    (!id.is_empty() && id.len() <= 998 && !id.chars().any(|c| c.is_whitespace() || c.is_control()))
        .then(|| id.to_owned())
}

fn ids(value: &HeaderValue<'_>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        if let Some(id) = normalize_id(id)
            && !out.contains(&id)
        {
            out.push(id);
        }
    };
    match value {
        HeaderValue::Text(id) => push(id),
        HeaderValue::TextList(list) => list.iter().for_each(|id| push(id)),
        _ => {}
    }
    out
}

/// The conversation of an Outlook `Thread-Index` value: base64 of a 22-byte header block
/// (reserved byte 0x01, a time, a GUID) and a 5-byte block per reply. Anything else, or a GUID
/// of zeros, is ignored.
pub fn thread_index(value: &str) -> Option<ThreadIndex> {
    let bytes = base64_decode(value.trim().as_bytes())?;
    let (head, replies) = bytes.split_at_checked(22)?;
    if head[0] != 1 || replies.len() % 5 != 0 || head[6..].iter().all(|byte| *byte == 0) {
        return None;
    }
    ThreadIndex::from_slice(head)
}

fn limit_references(mut references: Vec<String>) -> Vec<String> {
    if references.len() > MAX_REFERENCES {
        references.drain(1..references.len() - (MAX_REFERENCES - 1));
    }
    references
}

fn list_id(value: &HeaderValue<'_>) -> Option<String> {
    let text = match value {
        HeaderValue::Address(address) => {
            let first = address.first()?;
            first.address().or(first.name())?.to_owned()
        }
        HeaderValue::Text(text) => text.to_string(),
        _ => return None,
    };
    let text = match (text.find('<'), text.rfind('>')) {
        (Some(open), Some(close)) if open < close => text[open + 1..close].to_owned(),
        _ => text,
    };
    normalize_id(&text)
}

fn body_parts(
    message: &Message<'_>,
    ids: &[u32],
    keep: impl Fn(&PartType<'_>) -> bool,
) -> Vec<u32> {
    ids.iter()
        .copied()
        .filter(|id| {
            message
                .parts
                .get(*id as usize)
                .is_some_and(|part| keep(&part.body))
        })
        .collect()
}

fn body_text(message: &Message<'_>) -> String {
    let mut out = String::new();
    for id in &message.text_body {
        if out.len() >= SEARCH_TEXT_LIMIT {
            break;
        }
        let text = match message.parts.get(*id as usize).map(|part| &part.body) {
            Some(PartType::Text(text)) => truncate(text, SEARCH_TEXT_LIMIT).to_owned(),
            Some(PartType::Html(html)) => html_to_text(html),
            _ => continue,
        };
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&text);
    }
    truncate(&out, SEARCH_TEXT_LIMIT).to_owned()
}

/// The longest prefix of `text` that is at most `limit` bytes and ends on a character boundary.
pub fn truncate(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn mime_type(part: &mail_parser::MessagePart<'_>) -> String {
    match part.content_type() {
        Some(content_type) => match content_type.subtype() {
            Some(subtype) => format!("{}/{}", content_type.ctype(), subtype),
            None => content_type.ctype().to_owned(),
        },
        None => match part.body {
            PartType::Message(_) => "message/rfc822".to_owned(),
            PartType::Text(_) => "text/plain".to_owned(),
            PartType::Html(_) => "text/html".to_owned(),
            _ => "application/octet-stream".to_owned(),
        },
    }
    .to_ascii_lowercase()
}

fn attachments(message: &Message<'_>) -> Vec<Attachment> {
    message
        .attachments
        .iter()
        .filter_map(|id| {
            let part = message.parts.get(*id as usize)?;
            let content_id = part.content_id().and_then(normalize_id);
            let inline = match part
                .content_disposition()
                .map(|disposition| disposition.ctype().to_ascii_lowercase())
                .as_deref()
            {
                Some("attachment") => false,
                Some("inline") => true,
                _ => content_id.is_some() || matches!(part.body, PartType::InlineBinary(_)),
            };
            Some(Attachment {
                part: *id,
                name: part
                    .attachment_name()
                    .map(clean)
                    .filter(|name| !name.is_empty()),
                size: part.len() as u64,
                mime_type: mime_type(part),
                content_id,
                inline,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
