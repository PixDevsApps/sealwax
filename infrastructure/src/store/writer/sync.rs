//! The writer's sync jobs (ML-041, ML-043): a folder's changes by UID, and the folder list made
//! to match the server's. Each is one transaction whose change event is exact row by row, as
//! for `set_flags`, `remove` and `ingest`; new rows get their full text in a second transaction
//! right after, as an ingest batch does.

use super::{
    RowChange, Writer, folder_counts, free_uid, fts_doc, insert_message, recompute_thread,
    removed_positions, resolve_thread, thread_counts, upsert_rows, view_changes,
};
use crate::store::api::ViewScope;
use crate::store::api::{ChangeEvent, FolderSpec, StoreVersion, ThreadMerge, ViewChange, ViewDiff};
use crate::store::sync_api::{FlagUpdate, FolderChanges, FolderSynced, FoldersReplaced};
use crate::store::{StoreError, codec, views};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{AccountId, BlobKey, Flags, FolderId, MessageId, ThreadId};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

pub(crate) enum SyncJob {
    Changes {
        folder: FolderId,
        changes: FolderChanges,
        reply: Sender<FolderSynced>,
    },
    ReplaceFolders {
        account: AccountId,
        folders: Vec<FolderSpec>,
        reply: Sender<FoldersReplaced>,
    },
}

/// A stored row a step touches.
struct Stored {
    row: RowChange,
    keywords: String,
    thread: ThreadId,
    blob: Option<BlobKey>,
}

impl Writer {
    pub(super) fn sync_job(&mut self, job: SyncJob) {
        match job {
            SyncJob::Changes {
                folder,
                changes,
                reply,
            } => {
                let _ = reply.try_send(self.sync_changes(folder, &changes));
            }
            SyncJob::ReplaceFolders {
                account,
                folders,
                reply,
            } => {
                let _ = reply.try_send(self.replace_folders(account, &folders));
            }
        }
    }

    fn sync_changes(
        &mut self,
        folder: FolderId,
        changes: &FolderChanges,
    ) -> Result<FolderSynced, StoreError> {
        let account = self.state.account(folder)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = tx
            .query_row(
                "SELECT sync_state FROM folders WHERE id = ?1",
                [folder.0],
                |row| row.get(0),
            )
            .optional()?;
        match state.as_deref() {
            None => return Err(StoreError::UnknownFolder(folder)),
            Some("ready") => {}
            Some(_) => return Err(StoreError::SyncState(folder)),
        }
        let gone: BTreeSet<u32> = changes.removed.iter().copied().collect();
        let updates: BTreeMap<u32, &FlagUpdate> = changes
            .flags
            .iter()
            .filter(|update| !gone.contains(&update.uid))
            .map(|update| (update.uid, update))
            .collect();
        let mut removed = Vec::new();
        let mut flagged = Vec::new();
        {
            let mut select = tx.prepare_cached(
                "SELECT id, date, flags, has_attachments, keywords, thread_id, blob_key \
                 FROM messages WHERE folder_id = ?1 AND uid = ?2",
            )?;
            let mut stored = |uid: u32| {
                select
                    .query_row(params![folder.0, uid], |row| {
                        Ok(Stored {
                            row: RowChange {
                                id: MessageId(row.get(0)?),
                                folder,
                                date: row.get(1)?,
                                before: Some((row.get(2)?, row.get(3)?)),
                                after: None,
                            },
                            keywords: row.get(4)?,
                            thread: ThreadId(row.get(5)?),
                            blob: row
                                .get::<_, Option<Vec<u8>>>(6)?
                                .and_then(|key| BlobKey::from_slice(&key)),
                        })
                    })
                    .optional()
            };
            for uid in &gone {
                if let Some(found) = stored(*uid)? {
                    removed.push(found);
                }
            }
            for (uid, update) in &updates {
                let Some(mut found) = stored(*uid)? else {
                    continue;
                };
                let (flags, has_attachments) = found.row.before.expect("stored rows have a before");
                let keywords = codec::encode_words(&update.keywords);
                if flags == update.flags.bits() && found.keywords == keywords {
                    continue;
                }
                found.row.after = Some((update.flags.bits(), has_attachments));
                found.keywords = keywords;
                flagged.push(found);
            }
        }
        let mut rows: Vec<RowChange> = removed
            .iter()
            .chain(&flagged)
            .map(|stored| stored.row)
            .collect();
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let positions = removed_positions(&tx, &open.open, &rows)?;
        let mut threads = BTreeSet::new();
        let mut blobs = BTreeSet::new();
        {
            let mut delete = tx.prepare_cached("DELETE FROM messages WHERE id = ?1")?;
            let mut unindex = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
            let mut remove_counts = tx.prepare_cached(
                "UPDATE folders SET total_count = total_count - 1, \
                 unread_count = unread_count - ?2 WHERE id = ?1",
            )?;
            for stored in &removed {
                let (flags, _) = stored.row.before.expect("stored rows have a before");
                delete.execute([stored.row.id.0])?;
                unindex.execute([stored.row.id.0])?;
                remove_counts.execute(params![folder.0, unread(flags)])?;
                threads.insert(stored.thread);
                blobs.extend(stored.blob);
            }
            let mut update =
                tx.prepare_cached("UPDATE messages SET flags = ?2, keywords = ?3 WHERE id = ?1")?;
            let mut unread_counts = tx.prepare_cached(
                "UPDATE folders SET unread_count = unread_count + ?2 WHERE id = ?1",
            )?;
            for stored in &flagged {
                let (before, _) = stored.row.before.expect("stored rows have a before");
                let (after, _) = stored.row.after.expect("flag changes have an after");
                update.execute(params![stored.row.id.0, after, stored.keywords])?;
                let delta = unread(after) - unread(before);
                if delta != 0 {
                    unread_counts.execute(params![folder.0, delta])?;
                }
            }
        }
        for thread in &threads {
            recompute_thread(&tx, *thread)?;
        }
        let mut orphaned_blobs = Vec::new();
        {
            let mut used = tx.prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM messages INDEXED BY messages_blob WHERE blob_key = ?1)",
            )?;
            for key in blobs {
                if !used.query_row([key.0.as_slice()], |row| row.get::<_, bool>(0))? {
                    orphaned_blobs.push(key);
                }
            }
        }
        let mut inserted = Vec::new();
        let mut docs = Vec::new();
        let mut merged_threads = Vec::new();
        for message in changes.new.iter().filter(|message| {
            message.folder == folder && message.uid.is_some_and(|uid| !gone.contains(&uid))
        }) {
            let Some(uid) = free_uid(&tx, message)? else {
                continue;
            };
            let (thread, merged) = resolve_thread(&tx, account, message)?;
            merged_threads.extend(merged.iter().map(|from| ThreadMerge {
                from: *from,
                into: thread,
            }));
            let id = insert_message(&tx, thread, uid, message)?;
            threads.insert(thread);
            rows.push(RowChange {
                id,
                folder,
                date: message.date,
                before: None,
                after: Some((message.flags.bits(), message.has_attachments)),
            });
            inserted.push(id);
            docs.push(fts_doc(id, message));
        }
        for merge in &merged_threads {
            threads.remove(&merge.from);
        }
        if let Some(uidnext) = changes.uidnext {
            tx.prepare_cached("UPDATE folders SET uidnext = max(uidnext, ?2) WHERE id = ?1")?
                .execute(params![folder.0, uidnext])?;
        }
        if let Some(modseq) = changes.highestmodseq {
            tx.prepare_cached("UPDATE folders SET highestmodseq = ?2 WHERE id = ?1")?
                .execute(params![folder.0, modseq as i64])?;
        }
        if let Some(floor) = changes.floor {
            tx.prepare_cached("UPDATE folders SET sync_floor = ?2 WHERE id = ?1")?
                .execute(params![folder.0, floor])?;
        }
        if let Some(below) = changes.below {
            tx.prepare_cached("UPDATE folders SET below_floor = ?2 WHERE id = ?1")?
                .execute(params![folder.0, below])?;
        }
        let mut synced = FolderSynced {
            version: StoreVersion(self.state.version),
            inserted: count(inserted.len()),
            changed: count(flagged.len()),
            removed: count(removed.len()),
            orphaned_blobs,
        };
        if rows.is_empty() {
            tx.commit()?;
            drop(open);
            self.state.after_commit();
            return Ok(synced);
        }
        let views = view_changes(&tx, &open.open, &rows, &positions)?;
        let event = ChangeEvent {
            inserted,
            removed: removed.iter().map(|stored| stored.row.id).collect(),
            changed: flagged.iter().map(|stored| stored.row.id).collect(),
            folders: folder_counts(&tx, &BTreeSet::from([folder]))?,
            threads: thread_counts(&tx, &threads)?,
            merged_threads,
            views,
            ..ChangeEvent::default()
        };
        synced.version = self.state.commit(tx, event)?;
        drop(open);
        if !docs.is_empty()
            && let Err(error) = self.index_docs(&docs)
        {
            tracing::warn!(%error, "full-text batch of a sync step failed");
        }
        Ok(synced)
    }

    fn replace_folders(
        &mut self,
        account: AccountId,
        folders: &[FolderSpec],
    ) -> Result<FoldersReplaced, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (ids, counted) = upsert_rows(&tx, folders)?;
        let listed: HashSet<FolderId> = ids.iter().copied().collect();
        let gone: Vec<FolderId> = tx
            .prepare_cached("SELECT id FROM folders WHERE account_id = ?1 ORDER BY id")?
            .query_map([account.0], |row| row.get(0).map(FolderId))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|folder| !listed.contains(folder))
            .collect();
        let mut removed = Vec::new();
        let mut threads = BTreeSet::new();
        let mut blobs = BTreeSet::new();
        {
            let mut select = tx.prepare_cached(
                "SELECT id, thread_id, blob_key FROM messages WHERE folder_id = ?1",
            )?;
            for folder in &gone {
                let rows = select.query_map([folder.0], |row| {
                    Ok((
                        MessageId(row.get(0)?),
                        ThreadId(row.get(1)?),
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                })?;
                for row in rows {
                    let (id, thread, blob) = row?;
                    removed.push(id);
                    threads.insert(thread);
                    blobs.extend(blob.and_then(|key| BlobKey::from_slice(&key)));
                }
            }
        }
        // In rowid order, as an account's removal does: FTS5's tombstones stay on few pages.
        removed.sort_unstable();
        {
            let mut unindex = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
            for id in &removed {
                unindex.execute([id.0])?;
            }
            // The folder's messages and their parts follow by cascade.
            let mut delete = tx.prepare_cached("DELETE FROM folders WHERE id = ?1")?;
            for folder in &gone {
                delete.execute([folder.0])?;
            }
        }
        for thread in &threads {
            recompute_thread(&tx, *thread)?;
        }
        let mut orphaned_blobs = Vec::new();
        {
            let mut used = tx.prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM messages INDEXED BY messages_blob WHERE blob_key = ?1)",
            )?;
            for key in blobs {
                if !used.query_row([key.0.as_slice()], |row| row.get::<_, bool>(0))? {
                    orphaned_blobs.push(key);
                }
            }
        }
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let mut views = Vec::new();
        for (view, spec) in &open.open {
            let affected = match spec.scope {
                ViewScope::Folder(folder) => gone.contains(&folder),
                ViewScope::UnifiedInbox => true,
            };
            if affected {
                let folders = views::folders(&tx, spec.scope)?;
                views.push(ViewChange {
                    view: *view,
                    diff: ViewDiff::Reload {
                        total: views::count(&tx, &folders, spec.filter)?,
                    },
                });
            }
        }
        let event = ChangeEvent {
            removed: removed.clone(),
            folders: folder_counts(&tx, &counted)?,
            folders_changed: true,
            threads: thread_counts(&tx, &threads)?,
            views,
            ..ChangeEvent::default()
        };
        let version = self.state.commit(tx, event)?;
        drop(open);
        for (id, folder) in ids.iter().zip(folders) {
            self.state.accounts.insert(*id, folder.account);
        }
        for folder in &gone {
            self.state.accounts.remove(folder);
        }
        Ok(FoldersReplaced {
            version,
            ids,
            removed_folders: count(gone.len()),
            removed_messages: count(removed.len()),
            orphaned_blobs,
        })
    }
}

fn unread(flags: u32) -> i64 {
    i64::from(flags & Flags::SEEN.bits() == 0)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
