//! The reader's read of a conversation (ML-057; ADR-016's M5 amendment): the messages of one
//! message's conversation, oldest first, each with what the reader needs to draw a collapsed row
//! or decide what expands. The views' visibility holds: rows a pending move or expunge hides and
//! rows flagged `\Deleted` are left out, and flags are the overlay's. Copies of one message
//! (one Message-ID in several folders) show once, and the Trash and Junk stay out of a
//! conversation opened elsewhere (Gmail's rule).

use super::reader::ReadJob;
use super::{Reply, Store, Versioned, overlay};
use sealwax_domain::model::{FolderRole, MessageId, MessageSummary, ThreadId};
use std::collections::HashMap;

/// One message as the conversation shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationMessage {
    pub summary: MessageSummary,
    /// The role of the folder it is in.
    pub role: FolderRole,
    /// Other stored copies of it (the same Message-ID in other folders), left out.
    pub copies: Vec<MessageId>,
}

/// A conversation as the reader shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub thread: ThreadId,
    /// The message asked for. It is always among `messages`, even when hidden: the list only
    /// offers shown rows, and the reader keeps showing it until the list moves on.
    pub selected: MessageId,
    /// Oldest first, by date and then id.
    pub messages: Vec<ConversationMessage>,
    /// The conversation's other rows, left out: hidden by a pending move or expunge or by
    /// `\Deleted`, or in the Trash or Junk, by id. A change to one of them, or to a copy, changes
    /// what the conversation shows.
    pub left_out: Vec<MessageId>,
}

impl Conversation {
    /// Every row of the conversation the store holds: shown, copies and left out.
    pub fn members(&self) -> impl Iterator<Item = MessageId> + '_ {
        self.messages
            .iter()
            .flat_map(|message| {
                std::iter::once(message.summary.id).chain(message.copies.iter().copied())
            })
            .chain(self.left_out.iter().copied())
    }

    /// The position of `id` among the messages shown.
    pub fn position(&self, id: MessageId) -> Option<usize> {
        self.messages
            .iter()
            .position(|message| message.summary.id == id)
    }
}

/// One stored row of a conversation, as the reader thread reads it.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) summary: MessageSummary,
    /// `messages.flags` as stored: the summary's flags drop the overlay's hidden bit.
    pub(crate) stored_flags: u32,
    pub(crate) role: FolderRole,
    pub(crate) message_id: Option<String>,
}

/// Which copy of a message the conversation shows: the one the user selected, else by the role
/// of its folder (Inbox, Archive, All Mail, Sent, other folders, Drafts, Junk, Trash), then the
/// oldest stored.
fn rank(role: FolderRole) -> u8 {
    match role {
        FolderRole::Inbox => 0,
        FolderRole::Archive => 1,
        FolderRole::All => 2,
        FolderRole::Sent => 3,
        FolderRole::Other => 4,
        FolderRole::Drafts => 5,
        FolderRole::Junk => 6,
        FolderRole::Trash => 7,
    }
}

fn binned(role: FolderRole) -> bool {
    matches!(role, FolderRole::Trash | FolderRole::Junk)
}

/// The conversation of `selected` from its stored rows (oldest first); `None` when `selected`
/// isn't among them.
pub(crate) fn assemble(
    thread: ThreadId,
    selected: MessageId,
    rows: Vec<Row>,
) -> Option<Conversation> {
    let chosen = rows.iter().find(|row| row.summary.id == selected)?;
    let with_bin = binned(chosen.role);
    let mut left_out = Vec::new();
    let mut shown: Vec<Row> = Vec::new();
    for row in rows {
        let visible = overlay::shown(row.stored_flags) && (with_bin || !binned(row.role));
        if row.summary.id == selected || visible {
            shown.push(row);
        } else {
            left_out.push(row.summary.id);
        }
    }
    let mut groups: HashMap<String, usize> = HashMap::new();
    let mut messages: Vec<(ConversationMessage, bool)> = Vec::new();
    for row in shown {
        let key = row.message_id.clone().filter(|key| !key.is_empty());
        let Some(key) = key else {
            messages.push((plain(row), false));
            continue;
        };
        match groups.get(&key) {
            None => {
                groups.insert(key, messages.len());
                let picked = row.summary.id == selected;
                messages.push((plain(row), picked));
            }
            Some(&at) => {
                let (current, picked) = &mut messages[at];
                let better = row.summary.id == selected || (!*picked && better_copy(&row, current));
                if better {
                    let previous = std::mem::replace(current, plain(row));
                    current.copies = previous.copies;
                    current.copies.push(previous.summary.id);
                    *picked = current.summary.id == selected;
                } else {
                    current.copies.push(row.summary.id);
                }
            }
        }
    }
    let mut messages: Vec<ConversationMessage> = messages
        .into_iter()
        .map(|(mut message, _)| {
            message.copies.sort();
            message
        })
        .collect();
    messages.sort_by_key(|message| (message.summary.date, message.summary.id));
    left_out.sort();
    Some(Conversation {
        thread,
        selected,
        messages,
        left_out,
    })
}

fn plain(row: Row) -> ConversationMessage {
    ConversationMessage {
        summary: row.summary,
        role: row.role,
        copies: Vec::new(),
    }
}

/// Whether `row` is a better copy to show than `current` (neither selected).
fn better_copy(row: &Row, current: &ConversationMessage) -> bool {
    (rank(row.role), row.summary.id) < (rank(current.role), current.summary.id)
}

impl Store {
    /// The conversation of `id` as the reader shows it, read on a reader thread; `None` when
    /// the store doesn't hold `id`.
    pub fn conversation(&self, id: MessageId) -> Reply<Versioned<Option<Conversation>>> {
        self.read(|reply| ReadJob::Conversation { id, reply })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId};

    fn row(id: i64, folder: i64, role: FolderRole, message_id: Option<&str>, date: i64) -> Row {
        Row {
            summary: MessageSummary {
                id: MessageId(id),
                folder: FolderId(folder),
                thread: ThreadId(1),
                sender_name: String::new(),
                sender_email: String::new(),
                subject: String::new(),
                snippet: String::new(),
                date,
                flags: Flags::NONE,
                has_attachments: false,
                thread_count: 1,
                subscription: false,
            },
            stored_flags: 0,
            role,
            message_id: message_id.map(str::to_owned),
        }
    }

    fn ids(conversation: &Conversation) -> Vec<i64> {
        conversation
            .messages
            .iter()
            .map(|message| message.summary.id.0)
            .collect()
    }

    #[test]
    fn copies_show_once_preferring_the_selected_then_the_folders_role() {
        let rows = vec![
            row(1, 3, FolderRole::Sent, Some("a@example.test"), 10),
            row(2, 2, FolderRole::Archive, Some("a@example.test"), 11),
            row(3, 1, FolderRole::Inbox, Some("a@example.test"), 12),
            row(4, 1, FolderRole::Inbox, Some("b@example.test"), 20),
            row(5, 2, FolderRole::Archive, Some("b@example.test"), 21),
            row(6, 1, FolderRole::Inbox, None, 30),
            row(7, 1, FolderRole::Inbox, None, 30),
        ];
        let conversation = assemble(ThreadId(1), MessageId(5), rows.clone()).unwrap();
        assert_eq!(ids(&conversation), [3, 5, 6, 7]);
        assert_eq!(
            conversation.messages[0].copies,
            [MessageId(1), MessageId(2)]
        );
        assert_eq!(conversation.messages[1].copies, [MessageId(4)]);
        assert!(conversation.left_out.is_empty());
        let conversation = assemble(ThreadId(1), MessageId(1), rows).unwrap();
        assert_eq!(ids(&conversation), [1, 4, 6, 7]);
        assert_eq!(conversation.position(MessageId(4)), Some(1));
        assert_eq!(conversation.members().count(), 7);
    }

    #[test]
    fn the_bin_stays_out_unless_the_selected_message_is_in_it() {
        let rows = vec![
            row(1, 1, FolderRole::Inbox, Some("a@example.test"), 10),
            row(2, 5, FolderRole::Trash, Some("b@example.test"), 11),
            row(3, 6, FolderRole::Junk, Some("c@example.test"), 12),
            row(4, 5, FolderRole::Trash, Some("a@example.test"), 13),
        ];
        let conversation = assemble(ThreadId(1), MessageId(1), rows.clone()).unwrap();
        assert_eq!(ids(&conversation), [1]);
        assert_eq!(
            conversation.left_out,
            [MessageId(2), MessageId(3), MessageId(4)]
        );
        let conversation = assemble(ThreadId(1), MessageId(2), rows).unwrap();
        assert_eq!(ids(&conversation), [1, 2, 3]);
        assert_eq!(conversation.messages[0].copies, [MessageId(4)]);
    }

    #[test]
    fn hidden_rows_stay_out_except_the_selected_one() {
        let mut rows = vec![
            row(1, 1, FolderRole::Inbox, Some("a@example.test"), 10),
            row(2, 1, FolderRole::Inbox, Some("b@example.test"), 11),
            row(3, 1, FolderRole::Inbox, Some("c@example.test"), 12),
        ];
        rows[1].stored_flags = Flags::DELETED.bits();
        rows[2].stored_flags = overlay::HIDDEN;
        let conversation = assemble(ThreadId(1), MessageId(1), rows.clone()).unwrap();
        assert_eq!(ids(&conversation), [1]);
        assert_eq!(conversation.left_out, [MessageId(2), MessageId(3)]);
        let conversation = assemble(ThreadId(1), MessageId(3), rows).unwrap();
        assert_eq!(ids(&conversation), [1, 3]);
        assert!(assemble(ThreadId(1), MessageId(9), Vec::new()).is_none());
    }
}
