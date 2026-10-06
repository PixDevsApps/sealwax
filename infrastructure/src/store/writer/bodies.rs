//! The writer's body operations (ML-044): a downloaded body upgrades a header-only row in place
//! (same id, same place in every list), opening a message marks its body used, and the disk cap
//! sends the bodies used longest ago back to their headers and preview.

use super::{RowChange, Writer, people, removed_positions, view_changes};
use crate::store::api::{BodyState, BodyUpgrade, ChangeEvent, Evicted, StoreVersion, Upgraded};
use crate::store::{StoreError, codec, compose, fts};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{Address, BlobKey, FolderId, MessageId};
use std::collections::HashSet;
use std::sync::Arc;

/// Bodies evicted per transaction.
const EVICT_BATCH: usize = 200;
/// Opening a message this soon after its body was last used writes nothing.
const TOUCH_SLACK: i64 = 60;

/// Whether any message, draft or outbox entry still refers to `key`.
fn referenced(tx: &Connection, key: &BlobKey) -> rusqlite::Result<bool> {
    Ok(tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM messages INDEXED BY messages_blob WHERE blob_key = ?1)",
        )?
        .query_row([key.0.as_slice()], |row| row.get(0))?
        || compose::held(tx, key)?)
}

/// The columns an upgrade reads from the row it replaces.
struct Stored {
    folder: FolderId,
    date: i64,
    flags: u32,
    has_attachments: bool,
    blob: Option<BlobKey>,
    subject: String,
    sender: Address,
    recipients: Vec<Address>,
}

impl Writer {
    /// Puts a downloaded body on its header-only row in one transaction: the blob key, the
    /// body state and kind, the parts and their sections, the snippet and a full-text row with
    /// the whole text. The change event lists the message as changed, with its row in place in
    /// every open view.
    pub(super) fn upgrade_body(&mut self, upgrade: &BodyUpgrade) -> Result<Upgraded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = tx
            .prepare_cached(
                "SELECT folder_id, date, flags, has_attachments, blob_key, subject, from_name, \
                   from_addr, to_addrs, cc_addrs FROM messages WHERE id = ?1 AND uid = ?2",
            )?
            .query_row(params![upgrade.message.0, upgrade.uid], |row| {
                Ok(Stored {
                    folder: FolderId(row.get(0)?),
                    date: row.get(1)?,
                    flags: row.get(2)?,
                    has_attachments: row.get(3)?,
                    blob: row
                        .get::<_, Option<Vec<u8>>>(4)?
                        .and_then(|key| BlobKey::from_slice(&key)),
                    subject: row.get(5)?,
                    sender: Address::new(row.get::<_, String>(6)?, row.get::<_, String>(7)?),
                    recipients: [
                        codec::decode_addresses(&row.get::<_, String>(8)?),
                        codec::decode_addresses(&row.get::<_, String>(9)?),
                    ]
                    .concat(),
                })
            })
            .optional()?;
        let Some(stored) = stored else {
            let orphaned_blobs = if referenced(&tx, &upgrade.blob)? {
                Vec::new()
            } else {
                vec![upgrade.blob]
            };
            return Ok(Upgraded {
                version: StoreVersion(self.state.version),
                found: false,
                orphaned_blobs,
            });
        };
        let id = upgrade.message;
        let row = RowChange {
            id,
            folder: stored.folder,
            date: stored.date,
            before: Some((stored.flags, stored.has_attachments)),
            after: Some((stored.flags, upgrade.has_attachments)),
        };
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let removed = removed_positions(&tx, &open.open, &[row])?;
        tx.prepare_cached(
            "UPDATE messages SET blob_key = ?2, body_state = ?3, body_kind = ?4, \
               has_attachments = ?5, snippet = CASE WHEN ?6 = '' THEN snippet ELSE ?6 END \
             WHERE id = ?1",
        )?
        .execute(params![
            id.0,
            upgrade.blob.0.as_slice(),
            upgrade.body_state.code(),
            upgrade.body_kind.code(),
            upgrade.has_attachments,
            upgrade.snippet,
        ])?;
        tx.prepare_cached("DELETE FROM message_parts WHERE message = ?1")?
            .execute([id.0])?;
        {
            let mut part = tx.prepare_cached(
                "INSERT OR IGNORE INTO message_parts(message, part, role, mime_type, name, size, \
                   content_id, inline, section) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for (role, indexes, mime_type) in [
                (0, &upgrade.text_parts, "text/plain"),
                (1, &upgrade.html_parts, "text/html"),
            ] {
                for index in indexes {
                    part.execute(params![
                        id.0,
                        index,
                        role,
                        mime_type,
                        None::<&str>,
                        0,
                        None::<&str>,
                        false,
                        None::<&str>,
                    ])?;
                }
            }
            for attachment in &upgrade.attachments {
                let section = upgrade
                    .sections
                    .iter()
                    .find(|(part, _)| *part == attachment.part)
                    .map(|(_, section)| section.as_str());
                part.execute(params![
                    id.0,
                    attachment.part,
                    2,
                    attachment.mime_type,
                    attachment.name,
                    attachment.size as i64,
                    attachment.content_id,
                    attachment.inline,
                    section,
                ])?;
            }
        }
        tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?
            .execute([id.0])?;
        tx.prepare_cached(
            "INSERT INTO message_fts(rowid, subject, sender, recipients, body) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            id.0,
            fts::index_text(&stored.subject),
            fts::index_text(&people(std::slice::from_ref(&stored.sender))),
            fts::index_text(&people(&stored.recipients)),
            fts::index_text(&upgrade.search_text),
        ])?;
        tx.prepare_cached(
            "INSERT INTO bodies(blob_key, size, used) VALUES (?1, ?2, ?3) \
             ON CONFLICT(blob_key) DO UPDATE SET size = excluded.size, \
               used = max(bodies.used, excluded.used)",
        )?
        .execute(params![
            upgrade.blob.0.as_slice(),
            upgrade.blob_size as i64,
            upgrade.used
        ])?;
        let mut orphaned_blobs = Vec::new();
        if let Some(old) = stored.blob.filter(|old| *old != upgrade.blob)
            && !referenced(&tx, &old)?
        {
            tx.prepare_cached("DELETE FROM bodies WHERE blob_key = ?1")?
                .execute([old.0.as_slice()])?;
            orphaned_blobs.push(old);
        }
        let views = view_changes(&tx, &open.open, &[row], &removed)?;
        let event = ChangeEvent {
            changed: vec![id],
            views,
            ..ChangeEvent::default()
        };
        let version = self.state.commit(tx, event)?;
        drop(open);
        Ok(Upgraded {
            version,
            found: true,
            orphaned_blobs,
        })
    }

    /// Marks the body of `message` used at `at`, for the disk cap's order. Nothing the lists
    /// show changes, so no version and no event.
    pub(super) fn touch_body(&mut self, message: MessageId, at: i64) -> Result<(), StoreError> {
        let changed = self
            .conn
            .prepare_cached(
                "UPDATE bodies SET used = ?2 WHERE used < ?2 - ?3 \
                   AND blob_key = (SELECT blob_key FROM messages WHERE id = ?1)",
            )?
            .execute(params![message.0, at, TOUCH_SLACK])?;
        if changed > 0 {
            self.state.after_commit();
        }
        Ok(())
    }

    /// While the downloaded bodies take more than `limit` bytes, sends those used longest ago
    /// back to their headers and preview (rows, snippets and full text stay), sparing the
    /// bodies of `keep` and any blob a draft or the outbox holds. At most [`EVICT_BATCH`] bodies per call; the reply says whether to
    /// call again. Nothing the lists show changes, so no version and no event.
    pub(super) fn evict_bodies(
        &mut self,
        limit: u64,
        keep: &[MessageId],
    ) -> Result<Evicted, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let total: i64 = tx.query_row("SELECT coalesce(sum(size), 0) FROM bodies", [], |row| {
            row.get(0)
        })?;
        let total = u64::try_from(total).unwrap_or(0);
        if total <= limit {
            return Ok(Evicted {
                bytes: total,
                ..Evicted::default()
            });
        }
        let mut kept = HashSet::new();
        {
            let mut blob = tx.prepare_cached("SELECT blob_key FROM messages WHERE id = ?1")?;
            for id in keep {
                if let Some(Some(key)) = blob
                    .query_row([id.0], |row| row.get::<_, Option<Vec<u8>>>(0))
                    .optional()?
                {
                    kept.insert(key);
                }
            }
        }
        let mut excess = total - limit;
        let mut chosen: Vec<(Vec<u8>, u64)> = Vec::new();
        {
            let mut oldest =
                tx.prepare_cached("SELECT blob_key, size FROM bodies ORDER BY used, blob_key")?;
            let mut rows = oldest.query([])?;
            while excess > 0 && chosen.len() < EVICT_BATCH {
                let Some(row) = rows.next()? else {
                    break;
                };
                let key: Vec<u8> = row.get(0)?;
                if kept.contains(&key)
                    || BlobKey::from_slice(&key)
                        .map(|blob| compose::held(&tx, &blob))
                        .transpose()?
                        .unwrap_or(false)
                {
                    continue;
                }
                let size = u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0);
                excess = excess.saturating_sub(size);
                chosen.push((key, size));
            }
        }
        let mut messages = Vec::new();
        let mut freed = 0;
        {
            let mut holders = tx.prepare_cached(
                "SELECT id FROM messages INDEXED BY messages_blob WHERE blob_key = ?1",
            )?;
            let mut header_only = tx.prepare_cached(
                "UPDATE messages SET blob_key = NULL, body_state = ?2 WHERE id = ?1",
            )?;
            let mut parts = tx.prepare_cached("DELETE FROM message_parts WHERE message = ?1")?;
            let mut forget = tx.prepare_cached("DELETE FROM bodies WHERE blob_key = ?1")?;
            for (key, size) in &chosen {
                let ids = holders
                    .query_map([key.as_slice()], |row| row.get(0).map(MessageId))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for id in ids {
                    header_only.execute(params![id.0, BodyState::Partial.code()])?;
                    parts.execute([id.0])?;
                    messages.push(id);
                }
                forget.execute([key.as_slice()])?;
                freed += size;
            }
        }
        tx.commit()?;
        self.state.after_commit();
        let bytes = total - freed.min(total);
        tracing::debug!(
            bodies = chosen.len(),
            messages = messages.len(),
            "bodies evicted under the disk cap"
        );
        Ok(Evicted {
            messages,
            blobs: chosen
                .iter()
                .filter_map(|(key, _)| BlobKey::from_slice(key))
                .collect(),
            bytes,
            more: bytes > limit && chosen.len() == EVICT_BATCH,
        })
    }
}
