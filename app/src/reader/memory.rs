//! What the reader shows that the store doesn't hold (ADR-030), kept in memory only:
//!
//! - a decrypted message's content (#32): its attachment cards' part numbers are the content's,
//!   so Open and Save read them from here; Show Original and Save as .eml still give the
//!   message as stored, encrypted;
//! - attached messages opened in the reader (#40): a `message/rfc822` attachment shown as a
//!   message of its own, under an id below zero that no stored message has, with a bar that
//!   leads back. Opening one inside it goes a level deeper; Back goes up a level, and from
//!   the first level back to the stored message.
//!
//! Nothing here is written to disk. Decrypted content is dropped when the next conversation
//! is shown, attached messages when the reader shows a stored message again.

use sealwax_domain::message;
use sealwax_domain::model::{Flags, FolderId, MessageDetail, MessageId, MessageSummary, ThreadId};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// How deep attached messages may be opened inside each other.
pub(super) const MAX_DEPTH: usize = 16;

/// One attached message the reader shows.
#[derive(Clone)]
pub(super) struct Attached {
    pub id: MessageId,
    pub detail: MessageDetail,
    pub raw: Rc<Vec<u8>>,
    /// The attachment's name, for the bar.
    pub name: String,
}

#[derive(Default)]
pub(super) struct Memory {
    /// Decrypted content, by the stored message it came from.
    content: RefCell<HashMap<MessageId, Rc<Vec<u8>>>>,
    /// The attached messages opened, outermost first.
    attached: RefCell<Vec<Attached>>,
    /// The stored message the first attached message came from.
    enclosing: Cell<Option<MessageId>>,
    next: Cell<i64>,
}

/// Whether `id` is an attached message's, not a stored one's.
pub(super) fn is_attached(id: MessageId) -> bool {
    id.0 < 0
}

impl Memory {
    /// Keeps `content` as what `id`'s cards read their parts from; `None` forgets it.
    pub(super) fn remember_content(&self, id: MessageId, content: Option<Vec<u8>>) {
        let mut held = self.content.borrow_mut();
        match content {
            Some(content) => {
                held.insert(id, Rc::new(content));
            }
            None => {
                held.remove(&id);
            }
        }
    }

    /// Drops every decrypted content: a new conversation is being shown.
    pub(super) fn forget_content(&self) {
        self.content.borrow_mut().clear();
    }

    /// The bytes `id`'s attachment parts are read from, when they aren't the stored message's.
    pub(super) fn parts_of(&self, id: MessageId) -> Option<Rc<Vec<u8>>> {
        if let Some(content) = self.content.borrow().get(&id) {
            return Some(content.clone());
        }
        self.attached(id).map(|attached| attached.raw)
    }

    /// An attached message's own bytes, for Show Original, Save as .eml and Print.
    pub(super) fn original_of(&self, id: MessageId) -> Option<Rc<Vec<u8>>> {
        self.attached(id).map(|attached| attached.raw)
    }

    pub(super) fn attached(&self, id: MessageId) -> Option<Attached> {
        self.attached
            .borrow()
            .iter()
            .find(|attached| attached.id == id)
            .cloned()
    }

    /// Opens `raw`, the attachment `name` of the message `from`, a level below it, with
    /// `detail` read from it ([`detail`], off the GTK thread): the levels below `from` are
    /// closed first. `None` when it is nested too deep.
    pub(super) fn open(
        &self,
        from: MessageId,
        raw: Vec<u8>,
        mut detail: MessageDetail,
        name: &str,
    ) -> Option<MessageId> {
        let mut attached = self.attached.borrow_mut();
        if is_attached(from) {
            let level = attached.iter().position(|level| level.id == from)?;
            attached.truncate(level + 1);
        } else {
            attached.clear();
            self.enclosing.set(Some(from));
        }
        if attached.len() >= MAX_DEPTH {
            return None;
        }
        let next = self.next.get() - 1;
        self.next.set(next);
        let id = MessageId(next);
        detail.summary.id = id;
        detail.summary.thread = ThreadId(next);
        attached.push(Attached {
            id,
            detail,
            raw: Rc::new(raw),
            name: name.to_owned(),
        });
        Some(id)
    }

    /// Where Back from `id` goes: the level above, or the stored message.
    pub(super) fn back(&self, id: MessageId) -> Option<MessageId> {
        let mut attached = self.attached.borrow_mut();
        let level = attached.iter().position(|level| level.id == id)?;
        attached.truncate(level);
        match level.checked_sub(1) {
            Some(above) => attached.get(above).map(|level| level.id),
            None => self.enclosing.get(),
        }
    }

    /// The stored message the attached messages came from, while one is open.
    #[cfg(test)]
    fn enclosing(&self) -> Option<MessageId> {
        if self.attached.borrow().is_empty() {
            None
        } else {
            self.enclosing.get()
        }
    }

    /// Closes every attached message: the reader shows a stored message.
    pub(super) fn close_attached(&self) {
        self.attached.borrow_mut().clear();
        self.enclosing.set(None);
    }
}

/// An attached message's detail, as the store would keep it; [`Memory::open`] gives it its id.
pub(super) fn detail(raw: &[u8]) -> MessageDetail {
    let id = MessageId(0);
    let parsed = message::parse(raw);
    MessageDetail {
        summary: MessageSummary {
            id,
            folder: FolderId(0),
            thread: ThreadId(id.0),
            sender_name: parsed
                .headers
                .from
                .first()
                .map(|from| from.name.clone())
                .unwrap_or_default(),
            sender_email: parsed
                .headers
                .from
                .first()
                .map(|from| from.email.clone())
                .unwrap_or_default(),
            subject: parsed.headers.subject.clone(),
            snippet: parsed.snippet.clone(),
            date: parsed.headers.date.unwrap_or(0),
            flags: Flags::SEEN,
            has_attachments: parsed.has_attachments(),
            thread_count: 1,
            subscription: false,
            pinned: false,
        },
        headers: parsed.headers,
        body_kind: parsed.body_kind,
        text_parts: parsed.text_parts,
        html_parts: parsed.html_parts,
        attachments: parsed.attachments,
        keywords: Vec::new(),
        size: parsed.size,
        blob: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &[u8] = b"From: Ada <ada@example.com>\r\nSubject: Inside\r\n\r\nHello\r\n";

    #[test]
    fn attached_messages_open_a_level_down_and_back_leads_up() {
        let memory = Memory::default();
        let stored = MessageId(7);
        let first = memory.open(stored, RAW.to_vec(), detail(RAW), "postacert.eml");
        let first = first.unwrap_or_else(|| panic!("not opened"));
        assert!(is_attached(first));
        assert_eq!(memory.enclosing(), Some(stored));
        assert_eq!(
            memory.attached(first).map(|attached| attached.detail.headers.subject),
            Some("Inside".to_owned())
        );
        let second = memory
            .open(first, RAW.to_vec(), detail(RAW), "inner.eml")
            .unwrap_or_else(|| panic!("not opened"));
        assert_ne!(first, second);
        assert_eq!(memory.back(second), Some(first));
        assert!(memory.attached(second).is_none());
        assert_eq!(memory.back(first), Some(stored));
        assert_eq!(memory.enclosing(), None);
    }

    #[test]
    fn opening_from_a_stored_message_starts_over_and_depth_is_capped() {
        let memory = Memory::default();
        let mut from = MessageId(1);
        for _ in 0..MAX_DEPTH {
            from = memory
                .open(from, RAW.to_vec(), detail(RAW), "a.eml")
                .unwrap_or_else(|| panic!("not opened"));
        }
        assert_eq!(memory.open(from, RAW.to_vec(), detail(RAW), "a.eml"), None);
        let fresh = memory.open(MessageId(2), RAW.to_vec(), detail(RAW), "b.eml");
        assert!(fresh.is_some());
        assert_eq!(memory.enclosing(), Some(MessageId(2)));
        assert_eq!(fresh.and_then(|id| memory.back(id)), Some(MessageId(2)));
    }

    #[test]
    fn decrypted_content_is_read_for_parts_but_not_as_the_original() {
        let memory = Memory::default();
        memory.remember_content(MessageId(3), Some(b"content".to_vec()));
        assert!(memory.parts_of(MessageId(3)).is_some());
        assert!(memory.original_of(MessageId(3)).is_none());
        memory.forget_content();
        assert!(memory.parts_of(MessageId(3)).is_none());
    }
}
