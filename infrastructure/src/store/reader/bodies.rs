//! The reads behind body downloads (ML-044): where each message is on the server, where one
//! attachment is, which Inbox messages the prefetch may download, and what the bodies take.

use super::Reader;
use crate::store::StoreError;
use crate::store::api::{BodyCandidate, BodyState, BodyTarget, BodyUsage, PartSource};
use rusqlite::OptionalExtension;
use sealwax_domain::model::{AccountId, BlobKey, FolderId, FolderRole, MessageId};

impl Reader {
    /// The stored messages of `ids`, in that order; ids the store doesn't hold are left out.
    pub(super) fn body_targets(&self, ids: &[MessageId]) -> Result<Vec<BodyTarget>, StoreError> {
        let mut select = self.conn.prepare_cached(
            "SELECT f.account_id, m.folder_id, m.uid, m.size, m.body_state, m.blob_key \
             FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.id = ?1",
        )?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let target = select
                .query_row([id.0], |row| {
                    Ok(BodyTarget {
                        message: *id,
                        account: AccountId(row.get(0)?),
                        folder: FolderId(row.get(1)?),
                        uid: row.get(2)?,
                        size: row.get::<_, i64>(3)?.max(0) as u64,
                        body_state: BodyState::from_code(row.get(4)?),
                        blob: row
                            .get::<_, Option<Vec<u8>>>(5)?
                            .and_then(|key| BlobKey::from_slice(&key)),
                    })
                })
                .optional()?;
            out.extend(target);
        }
        Ok(out)
    }

    /// Where the attachment at mail-parser index `part` of `message` is on the server.
    pub(super) fn part_source(
        &self,
        message: MessageId,
        part: u32,
    ) -> Result<Option<PartSource>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT m.folder_id, m.uid, p.section, p.mime_type FROM message_parts p \
                   JOIN messages m ON m.id = p.message \
                 WHERE p.message = ?1 AND p.part = ?2 AND p.role = 2 AND p.section IS NOT NULL",
            )?
            .query_row(rusqlite::params![message.0, part], |row| {
                Ok(PartSource {
                    message,
                    folder: FolderId(row.get(0)?),
                    uid: row.get(1)?,
                    section: row.get(2)?,
                    mime_type: row.get(3)?,
                })
            })
            .optional()?)
    }

    /// Messages without a body that arrived since `since` in the Inboxes of server accounts
    /// (`accounts.toml` lists them), newest first; hidden ones are left out.
    pub(super) fn body_candidates(
        &self,
        since: i64,
        limit: u32,
    ) -> Result<Vec<BodyCandidate>, StoreError> {
        let candidates = self
            .conn
            .prepare_cached(
                "SELECT m.id, f.account_id, m.folder_id, f.role, m.date, m.size \
                 FROM folders f JOIN accounts a ON a.id = f.account_id \
                   JOIN messages m ON m.folder_id = f.id \
                 WHERE f.role = 'inbox' AND a.key IS NOT NULL AND a.detached = 0 \
                   AND m.blob_key IS NULL AND m.date >= ?1 AND m.flags & 264 = 0 \
                 ORDER BY m.date DESC, m.id DESC LIMIT ?2",
            )?
            .query_map(rusqlite::params![since, limit], |row| {
                Ok(BodyCandidate {
                    message: MessageId(row.get(0)?),
                    account: AccountId(row.get(1)?),
                    folder: FolderId(row.get(2)?),
                    role: FolderRole::parse(&row.get::<_, String>(3)?),
                    date: row.get(4)?,
                    size: row.get::<_, i64>(5)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(candidates)
    }

    pub(super) fn body_usage(&self) -> Result<BodyUsage, StoreError> {
        Ok(self.conn.query_row(
            "SELECT count(*), coalesce(sum(size), 0) FROM bodies",
            [],
            |row| {
                Ok(BodyUsage {
                    bodies: row.get::<_, i64>(0)?.max(0) as u64,
                    bytes: row.get::<_, i64>(1)?.max(0) as u64,
                })
            },
        )?)
    }
}
