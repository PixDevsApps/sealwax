//! The composer's and the window's reads (M6): a draft, the drafts to reopen, the local draft a
//! Drafts-folder message is a copy of, the outbox, and recipient suggestions.

use super::{Reader, Sender};
use crate::store::compose::{self, state_from};
use crate::store::compose_api::{Contact, OutboxEntry, StoredDraft};
use crate::store::{StoreError, contacts};
use rusqlite::{OptionalExtension, params};
use sealwax_domain::model::{AccountId, DraftId, MessageId};

pub(crate) enum ComposeRead {
    Draft {
        id: DraftId,
        reply: Sender<Option<StoredDraft>>,
    },
    OpenDrafts {
        reply: Sender<Vec<DraftId>>,
    },
    DraftForMessage {
        message: MessageId,
        reply: Sender<Option<DraftId>>,
    },
    ReplyDraft {
        message: MessageId,
        reply: Sender<Option<StoredDraft>>,
    },
    Outbox {
        account: Option<AccountId>,
        reply: Sender<Vec<OutboxEntry>>,
    },
    Recipients {
        account: AccountId,
        prefix: String,
        limit: u32,
        reply: Sender<Vec<Contact>>,
    },
}

impl ComposeRead {
    pub(super) fn cancelled(&self) -> bool {
        match self {
            ComposeRead::Draft { reply, .. } => reply.is_closed(),
            ComposeRead::OpenDrafts { reply } => reply.is_closed(),
            ComposeRead::DraftForMessage { reply, .. } => reply.is_closed(),
            ComposeRead::ReplyDraft { reply, .. } => reply.is_closed(),
            ComposeRead::Outbox { reply, .. } => reply.is_closed(),
            ComposeRead::Recipients { reply, .. } => reply.is_closed(),
        }
    }
}

impl Reader {
    pub(super) fn compose_read(&self, read: ComposeRead) {
        match read {
            ComposeRead::Draft { id, reply } => {
                let _ = reply.try_send(compose::read(&self.conn, id).map_err(StoreError::from));
            }
            ComposeRead::OpenDrafts { reply } => {
                let _ = reply.try_send(self.open_drafts());
            }
            ComposeRead::DraftForMessage { message, reply } => {
                let _ = reply.try_send(self.draft_for_message(message));
            }
            ComposeRead::ReplyDraft { message, reply } => {
                let _ = reply.try_send(self.reply_draft(message));
            }
            ComposeRead::Outbox { account, reply } => {
                let _ = reply.try_send(self.outbox(account));
            }
            ComposeRead::Recipients {
                account,
                prefix,
                limit,
                reply,
            } => {
                let _ = reply.try_send(self.recipients(account, &prefix, limit));
            }
        }
    }

    fn open_drafts(&self) -> Result<Vec<DraftId>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT id FROM drafts d WHERE open = 1 \
                   AND NOT EXISTS (SELECT 1 FROM outbox WHERE draft = d.id) ORDER BY saved, id",
            )?
            .query_map([], |row| row.get(0).map(DraftId))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn draft_for_message(&self, message: MessageId) -> Result<Option<DraftId>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT d.id FROM messages m JOIN folders f ON f.id = m.folder_id \
                 JOIN drafts d INDEXED BY drafts_message_id ON d.message_id = m.message_id \
                   AND d.account_id = f.account_id \
                 WHERE m.id = ?1 AND f.role = 'drafts' ORDER BY d.id DESC LIMIT 1",
            )?
            .query_row([message.0], |row| row.get(0).map(DraftId))
            .optional()?)
    }

    /// The newest closed, editable draft that replies to `message` (the quick reply's).
    fn reply_draft(&self, message: MessageId) -> Result<Option<StoredDraft>, StoreError> {
        let id: Option<i64> = self
            .conn
            .prepare_cached(
                "SELECT d.id FROM drafts d INDEXED BY drafts_source \
                 WHERE d.source_message = ?1 AND d.source_kind = 0 AND d.open = 0 \
                   AND NOT EXISTS (SELECT 1 FROM outbox WHERE draft = d.id) \
                 ORDER BY d.saved DESC, d.id DESC LIMIT 1",
            )?
            .query_row([message.0], |row| row.get(0))
            .optional()?;
        match id {
            Some(id) => Ok(compose::read(&self.conn, DraftId(id))?),
            None => Ok(None),
        }
    }

    fn outbox(&self, account: Option<AccountId>) -> Result<Vec<OutboxEntry>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT o.draft, o.account_id, o.state, o.not_before, o.attempts, d.subject, \
                        o.started \
                 FROM outbox o JOIN drafts d ON d.id = o.draft \
                 WHERE ?1 IS NULL OR o.account_id = ?1 ORDER BY o.queued, o.draft",
            )?
            .query_map([account.map(|account| account.0)], |row| {
                Ok(OutboxEntry {
                    draft: DraftId(row.get(0)?),
                    account: AccountId(row.get(1)?),
                    state: state_from(row.get(2)?),
                    not_before: row.get(3)?,
                    attempts: row.get(4)?,
                    subject: row.get(5)?,
                    started: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Prefix matches through `contact_terms`, ranked: written to first (most first), then
    /// seen most often, then most recently.
    fn recipients(
        &self,
        account: AccountId,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<Contact>, StoreError> {
        let prefix = contacts::fold(prefix.trim());
        if prefix.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut upper = prefix.clone();
        upper.push(char::MAX);
        Ok(self
            .conn
            .prepare_cached(
                "SELECT address, name, seen, written, last FROM contacts \
                 WHERE id IN (SELECT contact FROM contact_terms WHERE term >= ?2 AND term < ?3) \
                   AND account_id = ?1 \
                 ORDER BY written DESC, seen DESC, last DESC, id LIMIT ?4",
            )?
            .query_map(params![account.0, prefix, upper, limit], |row| {
                Ok(Contact {
                    address: row.get(0)?,
                    name: row.get(1)?,
                    seen: row.get(2)?,
                    written: row.get(3)?,
                    last: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}
