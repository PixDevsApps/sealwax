//! Reply, Reply All and Forward (ML-069): who the answer goes to, its sender, subject and
//! threading headers, and the quote, planned from the message being answered.
//!
//! - **Reply** goes to Reply-To when the original has one, else to its From. Replying to a
//!   message the user sent (its From is one of their addresses) goes to its original To.
//! - **Reply All** adds the original's To, and its Cc as Cc, leaving out every one of the
//!   user's addresses; each address once, case-insensitively.
//! - **Forward** has no recipients; it carries the original's attachments.
//! - **From** is the identity the original was sent to (To, then Cc), else the account's own.
//! - **Subjects** get "Re: " or "Fwd: " once: existing reply prefixes (Re, RE, Sv, SV, AW,
//!   Antw…) or forward prefixes (Fwd, Fw, VS, WG…) are taken off first.
//! - **Threading:** In-Reply-To is the original's Message-ID; References are the original's
//!   (or its In-Reply-To when it has none) plus its Message-ID, at most [`MAX_REFERENCES`]: the
//!   first, which names the thread's root, and the most recent. A forward has neither (RFC 5322
//!   3.6.4 gives them to replies; they would show the new recipients the thread's ids).

use super::clean;
use super::identity::{self, Signature};
use super::quote::{self, QuotedDate};
use super::text;
use super::{Draft, DraftPart, DraftSource, ReplyKind, address, ids};
use crate::account::Identity;
use crate::attachment::safe_file_name;
use crate::flowed::{self, Flowed};
use crate::message;
use crate::model::{AccountId, Address, Attachment, BlobKey, Headers, MessageId};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use std::fmt;

/// The most References a reply carries: the root and the 19 most recent. Long threads would
/// otherwise grow the header without bound; 20 ids are about 1.2 KB.
pub const MAX_REFERENCES: usize = 20;

/// The message being answered or forwarded. Its `Debug` prints counts and lengths only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Original {
    pub headers: Headers,
    /// The Date header with its offset, for the attribution.
    pub date: Option<QuotedDate>,
    /// The HTML body as it came (hostile: it is cleaned before it is quoted).
    pub html: Option<String>,
    /// The plain-text body, format=flowed decoded.
    pub text: Option<String>,
    /// Its non-body parts, as the store lists them.
    pub parts: Vec<Attachment>,
}

impl fmt::Debug for Original {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Original")
            .field("html_len", &self.html.as_ref().map(String::len))
            .field("text_len", &self.text.as_ref().map(String::len))
            .field("parts", &self.parts.len())
            .finish()
    }
}

impl Original {
    /// Reads what a reply needs from the raw message. Never panics; a message without any
    /// header gives an empty original.
    pub fn parse(raw: &[u8]) -> Original {
        let parsed = message::parse(raw);
        let mut original = Original {
            headers: parsed.headers,
            parts: parsed.attachments,
            ..Original::default()
        };
        let Some(message) = MessageParser::default().parse(raw) else {
            return original;
        };
        original.date = message.date().and_then(|date| {
            let utc = (1970..=2200)
                .contains(&date.year)
                .then(|| date.to_timestamp())?;
            let minutes = i32::from(date.tz_hour) * 60 + i32::from(date.tz_minute);
            Some(QuotedDate {
                utc,
                offset_minutes: if date.tz_before_gmt {
                    -minutes
                } else {
                    minutes
                },
            })
        });
        let mut html = String::new();
        let mut text = String::new();
        for index in &parsed.html_parts {
            if let Some(PartType::Html(body)) =
                message.parts.get(*index as usize).map(|part| &part.body)
            {
                html.push_str(body);
            }
        }
        for index in &parsed.text_parts {
            let Some(part) = message.parts.get(*index as usize) else {
                continue;
            };
            if let PartType::Text(body) = &part.body {
                let flowed = part.content_type().and_then(|content_type| {
                    Flowed::from_params(
                        content_type.attribute("format"),
                        content_type.attribute("delsp"),
                    )
                });
                if !text.is_empty() {
                    text.push('\n');
                }
                match flowed {
                    Some(flowed) => text.push_str(&flowed::decode(body, flowed)),
                    None => text.push_str(body),
                }
            }
        }
        original.html = (!html.trim().is_empty()).then_some(html);
        original.text = (!text.trim().is_empty()).then_some(text);
        original
    }
}

/// A planned reply or forward. Its `Debug` prints counts and lengths only.
#[derive(Clone, PartialEq, Eq)]
pub struct Reply {
    pub kind: ReplyKind,
    /// The identity it goes out from, with its signature.
    pub sender: Identity,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// The attribution and the cleaned original in a cite quote, or the forwarded block.
    pub quote_html: String,
    /// The same as text, `> ` lines for a reply.
    pub quote_text: String,
    /// The original's parts the draft carries, by the original's part index: the inline images
    /// the quote shows (with their Content-ID) and, for a forward, its attachments (without).
    /// The caller copies their bytes into the blob store and makes them [`DraftPart`]s with
    /// [`draft_part`].
    pub parts: Vec<Attachment>,
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reply")
            .field("kind", &self.kind)
            .field("recipients", &(self.to.len() + self.cc.len()))
            .field("references", &self.references.len())
            .field("quote_len", &self.quote_html.len())
            .field("parts", &self.parts.len())
            .finish()
    }
}

/// Plans a `kind` answer to `original` from an account that sends as `senders` (its own
/// address first, [`identity::senders`]).
pub fn plan(original: &Original, kind: ReplyKind, senders: &[Identity]) -> Reply {
    let headers = &original.headers;
    let sender = identity::reply_sender(senders, headers)
        .cloned()
        .unwrap_or_else(|| Identity {
            name: String::new(),
            address: String::new(),
            signature: None,
        });
    let (to, cc) = recipients(headers, kind, senders);
    let subject = match kind {
        ReplyKind::Reply | ReplyKind::ReplyAll => reply_subject(&headers.subject),
        ReplyKind::Forward => forward_subject(&headers.subject),
    };
    let (in_reply_to, references) = match kind {
        ReplyKind::Reply | ReplyKind::ReplyAll => threading(headers),
        ReplyKind::Forward => (None, Vec::new()),
    };
    let available = |id: &str| {
        ids::is_header_safe(id)
            && original
                .parts
                .iter()
                .any(|part| part.content_id.as_deref() == Some(id))
    };
    let (body_html, content_ids) = match &original.html {
        Some(html) => {
            let cleaned = clean::clean_with(html, available);
            (cleaned.html, cleaned.content_ids)
        }
        None => (
            text::to_html(original.text.as_deref().unwrap_or_default()),
            Vec::new(),
        ),
    };
    let body_text = match (&original.text, &original.html) {
        (Some(text), _) => text.clone(),
        (None, Some(html)) => message::html_to_text(html),
        (None, None) => String::new(),
    };
    let (quote_html, quote_text) = match kind {
        ReplyKind::Reply | ReplyKind::ReplyAll => {
            let attribution = quote::attribution(original.date, headers.from.first());
            (
                quote::reply_html(&attribution, &body_html),
                quote::reply_text(&attribution, &body_text),
            )
        }
        ReplyKind::Forward => {
            let fields = quote::forward_fields(headers, original.date);
            (
                quote::forward_html(&fields, &body_html),
                quote::forward_text(&fields, &body_text),
            )
        }
    };
    let mut parts: Vec<Attachment> = Vec::new();
    for id in &content_ids {
        if let Some(part) = original
            .parts
            .iter()
            .find(|part| part.content_id.as_deref() == Some(id.as_str()))
            && !parts.iter().any(|known| known.part == part.part)
        {
            parts.push(part.clone());
        }
    }
    if kind == ReplyKind::Forward {
        for part in &original.parts {
            let shown = parts.iter().any(|known| known.part == part.part);
            if !shown && !is_signature_part(&part.mime_type) {
                parts.push(Attachment {
                    content_id: None,
                    inline: false,
                    ..part.clone()
                });
            }
        }
    }
    Reply {
        kind,
        sender,
        to,
        cc,
        subject,
        in_reply_to,
        references,
        quote_html,
        quote_text,
        parts,
    }
}

impl Reply {
    /// The draft's HTML: what the user typed (an empty line when nothing yet), the sender's
    /// signature, then the quote.
    pub fn html(&self, typed_html: &str) -> String {
        let signature = self
            .sender
            .signature
            .as_deref()
            .and_then(identity::signature);
        super::body(typed_html, signature.as_ref(), Some(&self.quote_html))
    }

    /// The draft of this answer: `message_id` made by the caller ([`super::ids::message_id`]),
    /// `parts` from [`Reply::parts`] in the blob store, `source` the stored message answered.
    pub fn draft(
        &self,
        account: AccountId,
        source: MessageId,
        message_id: String,
        typed_html: &str,
        parts: Vec<DraftPart>,
    ) -> Draft {
        Draft {
            account,
            from: identity::from_address(&self.sender),
            to: self.to.clone(),
            cc: self.cc.clone(),
            bcc: Vec::new(),
            subject: self.subject.clone(),
            html: self.html(typed_html),
            message_id,
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            source: Some(DraftSource {
                message: source,
                kind: self.kind,
            }),
            parts,
        }
    }

    /// The signature under the typed text, for a composer that shows it apart.
    pub fn signature(&self) -> Option<Signature> {
        self.sender
            .signature
            .as_deref()
            .and_then(identity::signature)
    }
}

/// One of [`Reply::parts`] as a draft part, its bytes at `blob`. The name is made safe
/// (`domain::attachment`); an inline image keeps its Content-ID.
pub fn draft_part(part: &Attachment, blob: BlobKey) -> DraftPart {
    DraftPart {
        name: safe_file_name(part.name.as_deref(), &part.mime_type),
        mime_type: part.mime_type.clone(),
        size: part.size,
        blob,
        content_id: part.content_id.clone(),
    }
}

/// Signature parts don't verify once forwarded inline; they are left out.
fn is_signature_part(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "application/pgp-signature"
            | "application/pkcs7-signature"
            | "application/x-pkcs7-signature"
    )
}

fn recipients(
    headers: &Headers,
    kind: ReplyKind,
    senders: &[Identity],
) -> (Vec<Address>, Vec<Address>) {
    let valid = |list: &[Address]| -> Vec<Address> {
        list.iter()
            .filter(|address| address::is_valid(&address.email))
            .cloned()
            .collect()
    };
    let own = |address: &Address| identity::is_own(senders, &address.email);
    let sent_by_me = headers.from.iter().any(own);
    let primary = if sent_by_me {
        valid(&headers.to)
    } else if !valid(&headers.reply_to).is_empty() {
        valid(&headers.reply_to)
    } else {
        valid(&headers.from)
    };
    match kind {
        ReplyKind::Forward => (Vec::new(), Vec::new()),
        ReplyKind::Reply => (address::dedupe(primary), Vec::new()),
        ReplyKind::ReplyAll => {
            let to: Vec<Address> = address::dedupe(
                primary
                    .iter()
                    .cloned()
                    .chain(valid(&headers.to))
                    .filter(|address| !own(address)),
            );
            let cc: Vec<Address> = address::dedupe(valid(&headers.cc))
                .into_iter()
                .filter(|address| {
                    !own(address)
                        && !to
                            .iter()
                            .any(|known| address::same(&known.email, &address.email))
                })
                .collect();
            if to.is_empty() && cc.is_empty() {
                (address::dedupe(primary), Vec::new())
            } else {
                (to, cc)
            }
        }
    }
}

fn threading(headers: &Headers) -> (Option<String>, Vec<String>) {
    let mut references: Vec<String> = if headers.references.is_empty() {
        headers.in_reply_to.clone()
    } else {
        headers.references.clone()
    };
    if let Some(id) = &headers.message_id {
        references.retain(|known| known != id);
        references.push(id.clone());
    }
    let mut seen: Vec<String> = Vec::new();
    references.retain(|id| {
        let new = !seen.contains(id);
        seen.push(id.clone());
        new
    });
    if references.len() > MAX_REFERENCES {
        references.drain(1..references.len() - (MAX_REFERENCES - 1));
    }
    (headers.message_id.clone(), references)
}

const REPLY_PREFIXES: [&str; 8] = ["re", "sv", "aw", "antw", "ref", "回复", "答复", "回覆"];
const FORWARD_PREFIXES: [&str; 6] = ["fwd", "fw", "vs", "wg", "tr", "转发"];

/// `subject` without the leading prefixes in `known` ("Re:", "SV:", "Re[2]:"…).
fn strip_prefixes<'s>(subject: &'s str, known: &[&str]) -> &'s str {
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
        let prefix = match prefix.find(['[', '(']) {
            Some(open) if prefix.ends_with([']', ')']) => &prefix[..open],
            _ => prefix,
        };
        if !known.iter().any(|known| prefix.eq_ignore_ascii_case(known)) {
            return rest;
        }
        rest = rest[colon + width..].trim_start();
    }
}

/// "Re: " and the subject without its reply prefixes; a forwarded subject keeps its "Fwd:".
pub fn reply_subject(subject: &str) -> String {
    prefixed(
        "Re:",
        strip_prefixes(&text::header_text(subject), &REPLY_PREFIXES),
    )
}

/// "Fwd: " and the subject without its forward prefixes; a reply keeps its "Re:".
pub fn forward_subject(subject: &str) -> String {
    prefixed(
        "Fwd:",
        strip_prefixes(&text::header_text(subject), &FORWARD_PREFIXES),
    )
}

fn prefixed(prefix: &str, rest: &str) -> String {
    if rest.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix} {rest}")
    }
}

#[cfg(test)]
mod tests;
