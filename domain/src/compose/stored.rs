//! A message from a Drafts folder read back as a draft (Edit Draft): its fields, its HTML
//! through the allowlist cleaner (another client may have written it), and its parts' bytes
//! for the caller to put in the blob store. Bcc is kept.

use super::clean;
use super::ids;
use super::text;
use super::{Draft, DraftPart, address};
use crate::attachment::safe_file_name;
use crate::flowed::{self, Flowed};
use crate::message;
use crate::model::{AccountId, Address, BlobKey};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use std::fmt;

/// A stored draft's fields. Its `Debug` prints counts and lengths only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct StoredDraft {
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    /// Cleaned HTML; a plain-text draft as the editor's lines.
    pub html: String,
    /// Its Message-ID when it is one Sealwax can send under (on the From's domain); otherwise
    /// the caller makes a new one.
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub parts: Vec<StoredPart>,
}

impl fmt::Debug for StoredDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredDraft")
            .field(
                "recipients",
                &(self.to.len() + self.cc.len() + self.bcc.len()),
            )
            .field("html_len", &self.html.len())
            .field("parts", &self.parts.len())
            .finish()
    }
}

/// An attachment or inline image of a stored draft, with its decoded bytes. Its `Debug` prints
/// the type and size only.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredPart {
    /// Made safe (`domain::attachment`).
    pub name: String,
    pub mime_type: String,
    /// Set for an inline image the HTML shows.
    pub content_id: Option<String>,
    pub data: Vec<u8>,
}

impl fmt::Debug for StoredPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredPart")
            .field("mime_type", &self.mime_type)
            .field("size", &self.data.len())
            .field("inline", &self.content_id.is_some())
            .finish()
    }
}

impl StoredPart {
    /// This part as a draft part, its bytes stored at `blob`.
    pub fn draft_part(&self, blob: BlobKey) -> DraftPart {
        DraftPart {
            name: self.name.clone(),
            mime_type: self.mime_type.clone(),
            size: self.data.len() as u64,
            blob,
            content_id: self.content_id.clone(),
        }
    }
}

impl StoredDraft {
    /// The draft to edit: `from` when the stored one has none (or one the account doesn't send
    /// as, which the caller decides), `message_id` the stored one or a new one, `parts` from
    /// [`StoredPart::draft_part`] in order.
    pub fn draft(
        &self,
        account: AccountId,
        from: Address,
        message_id: String,
        parts: Vec<DraftPart>,
    ) -> Draft {
        Draft {
            account,
            from,
            to: self.to.clone(),
            cc: self.cc.clone(),
            bcc: self.bcc.clone(),
            subject: self.subject.clone(),
            html: self.html.clone(),
            message_id,
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            source: None,
            parts,
        }
    }
}

/// Reads a Drafts folder's message; `None` when the bytes aren't a message.
pub fn parse(raw: &[u8]) -> Option<StoredDraft> {
    let message = MessageParser::default().parse(raw)?;
    let parsed = message::parse(raw);
    let headers = parsed.headers;
    let addresses = |value: Option<&mail_parser::Address<'_>>| -> Vec<Address> {
        value
            .map(|value| {
                value
                    .iter()
                    .filter_map(|addr| {
                        let email = addr.address()?.trim().to_owned();
                        address::is_valid(&email).then(|| {
                            Address::new(text::header_text(addr.name().unwrap_or_default()), email)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let from = addresses(message.from()).into_iter().next();
    let parts_with_ids: Vec<&str> = parsed
        .attachments
        .iter()
        .filter_map(|part| part.content_id.as_deref())
        .filter(|id| ids::is_header_safe(id))
        .collect();
    let mut html = String::new();
    for index in &parsed.html_parts {
        if let Some(PartType::Html(body)) = message.parts.get(*index as usize).map(|p| &p.body) {
            html.push_str(body);
        }
    }
    let (html, shown) = if html.trim().is_empty() {
        let mut plain = String::new();
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
                plain.push_str(&match flowed {
                    Some(flowed) => flowed::decode(body, flowed),
                    None => body.to_string(),
                });
            }
        }
        (
            text::to_html(plain.trim_end_matches(['\r', '\n'])),
            Vec::new(),
        )
    } else {
        let cleaned = clean::clean_with(&html, |id| parts_with_ids.contains(&id));
        (cleaned.html, cleaned.content_ids)
    };
    let parts = parsed
        .attachments
        .iter()
        .filter_map(|attachment| {
            let part = message.parts.get(attachment.part as usize)?;
            let content_id = attachment
                .content_id
                .clone()
                .filter(|id| shown.contains(id));
            Some(StoredPart {
                name: safe_file_name(attachment.name.as_deref(), &attachment.mime_type),
                mime_type: attachment.mime_type.clone(),
                content_id,
                data: part.contents().to_vec(),
            })
        })
        .collect();
    let from_domain = from
        .as_ref()
        .and_then(|from| ids::ascii_domain(&from.email));
    let message_id = headers.message_id.filter(|id| {
        ids::is_header_safe(id)
            && from_domain.is_some()
            && id.rsplit_once('@').map(|(_, domain)| domain) == from_domain.as_deref()
    });
    Some(StoredDraft {
        from,
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        bcc: addresses(message.bcc()),
        subject: headers.subject,
        html,
        message_id,
        in_reply_to: headers.in_reply_to.into_iter().next(),
        references: headers.references,
        parts,
    })
}
