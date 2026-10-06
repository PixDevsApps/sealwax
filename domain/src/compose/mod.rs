//! Writing mail (M6): the draft the composer edits, the store keeps and the MIME builder turns
//! into the bytes that are sent. The rules that fill a draft (who a reply goes to, the quote,
//! threading headers, identities and signatures) and the builder itself (ADR-007) live in
//! submodules of this one.
//!
//! A draft is the user's mail: its `Debug` prints only counts and lengths, and nothing here
//! ever logs.

pub mod address;
pub mod clean;
pub mod identity;
pub mod ids;
pub mod mime;
pub mod quote;
pub mod reply;
pub mod stored;
pub mod text;

use crate::account::Identity;
use crate::model::{AccountId, Address, BlobKey, MessageId};
use identity::Signature;
use std::fmt;

/// An empty line in the editor's HTML.
pub const EMPTY_LINE: &str = "<div><br></div>";

/// A draft's HTML: what the user typed (an empty line to type in when nothing yet), then the
/// signature and the quote, each after an empty line.
pub fn body(typed_html: &str, signature: Option<&Signature>, quote: Option<&str>) -> String {
    let mut html = if typed_html.trim().is_empty() {
        EMPTY_LINE.to_owned()
    } else {
        typed_html.to_owned()
    };
    if let Some(signature) = signature {
        html.push_str(EMPTY_LINE);
        html.push_str(&signature.html);
    }
    if let Some(quote) = quote {
        html.push_str(EMPTY_LINE);
        html.push_str(quote);
    }
    html
}

/// A new, empty draft from `sender` (one of [`identity::senders`]) with its signature;
/// `message_id` from [`ids::message_id`].
pub fn new_draft(account: AccountId, sender: &Identity, message_id: String) -> Draft {
    let signature = sender.signature.as_deref().and_then(identity::signature);
    Draft {
        account,
        from: identity::from_address(sender),
        html: body("", signature.as_ref(), None),
        message_id,
        ..Draft::default()
    }
}

/// A message being written. The composer edits it, the store keeps it at most 5 s behind the
/// typing (PRD C5), and at send time the MIME builder makes the transmitted copy, the Sent copy
/// and the Drafts folder's copy from it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// The account it is sent from.
    pub account: AccountId,
    /// The sender: the account's own address or one of its identities (PRD A6).
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Never written into the transmitted copy (ADR-007).
    pub bcc: Vec<Address>,
    pub subject: String,
    /// The body as the editor serialises it: cleaned HTML whose inline images are
    /// `cid:<content_id>` references to `parts`.
    pub html: String,
    /// Message-ID without angle brackets, on the sender's domain, made when the draft is created
    /// and kept until it is sent, so its server copies and the sent message share it.
    pub message_id: String,
    /// In-Reply-To without angle brackets (RFC 5322 3.6.4).
    pub in_reply_to: Option<String>,
    /// References without angle brackets, oldest first.
    pub references: Vec<String>,
    /// The message this answers or forwards, flagged `\Answered` or `$Forwarded` once it is
    /// sent.
    pub source: Option<DraftSource>,
    /// Attachments and inline images, in the order they are shown and sent.
    pub parts: Vec<DraftPart>,
}

impl fmt::Debug for Draft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Draft")
            .field("account", &self.account)
            .field(
                "recipients",
                &(self.to.len() + self.cc.len() + self.bcc.len()),
            )
            .field("html_len", &self.html.len())
            .field("source", &self.source)
            .field("parts", &self.parts.len())
            .finish()
    }
}

/// What a draft answers or forwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DraftSource {
    pub message: MessageId,
    pub kind: ReplyKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReplyKind {
    Reply,
    ReplyAll,
    Forward,
}

/// An attachment or inline image of a draft. Its bytes are in the blob store from the moment
/// it is added, so a crash loses none of them.
#[derive(Clone, PartialEq, Eq)]
pub struct DraftPart {
    /// The name it is sent under, already made safe (`domain::attachment`).
    pub name: String,
    /// Lower case, `type/subtype`.
    pub mime_type: String,
    /// Decoded size in bytes.
    pub size: u64,
    pub blob: BlobKey,
    /// An inline image the HTML shows as `cid:<content_id>` (without angle brackets); `None`
    /// for an attachment.
    pub content_id: Option<String>,
}

impl fmt::Debug for DraftPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DraftPart")
            .field("mime_type", &self.mime_type)
            .field("size", &self.size)
            .field("inline", &self.content_id.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_prints_no_content() {
        let draft = Draft {
            from: Address::new("Ada", "ada@example.test"),
            to: vec![Address::new("Bob", "bob@example.test")],
            subject: "Secret plans".into(),
            html: "<p>The plans</p>".into(),
            message_id: "abc@example.test".into(),
            parts: vec![DraftPart {
                name: "plans.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 3,
                blob: BlobKey([0; 32]),
                content_id: None,
            }],
            ..Draft::default()
        };
        let printed = format!("{draft:?} {:?}", draft.parts[0]);
        for secret in ["Ada", "ada@", "bob@", "Secret", "plans", "abc@"] {
            assert!(!printed.contains(secret), "{printed}");
        }
    }
}
