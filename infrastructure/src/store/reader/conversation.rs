//! The reader's read of a conversation (ML-057): every row of one message's conversation in one
//! snapshot, walked from the conversation index in date order; `conversation::assemble` decides
//! what shows.

use super::{Reader, version};
use crate::store::conversation::{Conversation, Row, assemble};
use crate::store::views::{SUMMARY_COLUMNS, summary_row};
use crate::store::{StoreError, Versioned};
use rusqlite::OptionalExtension;
use sealwax_domain::model::{FolderRole, MessageId, ThreadId};

impl Reader {
    pub(super) fn conversation(
        &self,
        id: MessageId,
    ) -> Result<Versioned<Option<Conversation>>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let version = version(&tx)?;
        let thread: Option<i64> = tx
            .prepare_cached("SELECT thread_id FROM messages WHERE id = ?1")?
            .query_row([id.0], |row| row.get(0))
            .optional()?;
        let Some(thread) = thread else {
            return Ok(Versioned {
                version,
                value: None,
            });
        };
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS}, m.flags, f.role, m.message_id \
             FROM messages m INDEXED BY messages_thread \
             JOIN threads t ON t.id = m.thread_id JOIN folders f ON f.id = m.folder_id \
             WHERE m.thread_id = ?1 ORDER BY m.date, m.id"
        );
        let rows = tx
            .prepare_cached(&sql)?
            .query_map([thread], |row| {
                Ok(Row {
                    summary: summary_row(row)?,
                    stored_flags: row.get(12)?,
                    role: FolderRole::parse(&row.get::<_, String>(13)?),
                    message_id: row.get(14)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Versioned {
            version,
            value: assemble(ThreadId(thread), id, rows),
        })
    }
}
