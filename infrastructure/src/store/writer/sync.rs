//! The writer's sync jobs (ML-041, ML-043): a folder's changes by UID, the folder list made to
//! match the server's, and what a windowed folder leaves below its floor. Each is one
//! transaction whose change event is exact row by row, as for `set_flags`, `remove` and
//! `ingest`; new rows get their full text in a second transaction right after, as an ingest
//! batch does.

use super::{
    RowChange, Writer, below_counts, folder_counts, free_uid, fts_doc, insert_message, overlay,
    overlay_new, recompute_thread, removed_positions, resolve_thread, thread_counts, upsert_rows,
    view_changes,
};
use crate::store::api::ViewScope;
use crate::store::api::{
    ChangeEvent, FolderSpec, StoreVersion, ThreadMerge, Versioned, ViewChange, ViewDiff,
};
use crate::store::overlay::{Change, HIDDEN};
use crate::store::sync_api::{
    FlagUpdate, FolderChanges, FolderSynced, FoldersReplaced, ServerStatus,
};
use crate::store::{RefusedChange, StoreError, codec, views};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{AccountId, BlobKey, Flags, FolderId, MessageId, ThreadId};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
    CountBelow {
        folder: FolderId,
        status: ServerStatus,
        reply: Sender<Versioned<bool>>,
    },
}

/// A stored row a step touches: `row` carries what is shown, `server` the server's flags.
struct Stored {
    row: RowChange,
    keywords: String,
    keywords_changed: bool,
    thread: ThreadId,
    blob: Option<BlobKey>,
    server: u32,
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
            SyncJob::CountBelow {
                folder,
                status,
                reply,
            } => {
                let _ = reply.try_send(self.count_below(folder, &status));
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
        let state: Option<(String, Option<u32>, u32)> = tx
            .query_row(
                "SELECT sync_state, sync_floor, coalesce(uidvalidity, 0) FROM folders WHERE id = ?1",
                [folder.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (old_floor, uidvalidity) = match state {
            None => return Err(StoreError::UnknownFolder(folder)),
            Some((state, floor, uidvalidity)) if state == "ready" => {
                (floor.unwrap_or(1), uidvalidity)
            }
            Some(_) => return Err(StoreError::SyncState(folder)),
        };
        let pending = overlay::has_pending(&tx, account)?;
        let below_before = below_counts(&tx, folder)?;
        let gone: BTreeSet<u32> = changes.removed.iter().copied().collect();
        let updates: BTreeMap<u32, &FlagUpdate> = changes
            .flags
            .iter()
            .filter(|update| !gone.contains(&update.uid))
            .map(|update| (update.uid, update))
            .collect();
        let mut removed = Vec::new();
        let mut flagged = Vec::new();
        let mut changed_on_server = 0;
        {
            let mut select = tx.prepare_cached(
                "SELECT id, date, flags, has_attachments, keywords, thread_id, blob_key, \
                   coalesce(server_flags, flags) FROM messages WHERE folder_id = ?1 AND uid = ?2",
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
                            server: row.get::<_, u32>(7)? & !HIDDEN,
                            keywords_changed: false,
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
                let server = update.flags.bits();
                if found.server == server && found.keywords == keywords {
                    continue;
                }
                changed_on_server += 1;
                let shown = if pending {
                    overlay::shown_at(&tx, found.row.id, folder, uidvalidity, *uid, server)?
                } else {
                    server
                };
                found.row.after = Some((shown, has_attachments));
                found.keywords_changed = found.keywords != keywords;
                found.keywords = keywords;
                found.server = server;
                flagged.push((found, flags != shown));
            }
        }
        let mut refusals = overlay::Refusals::default();
        if pending {
            for stored in &removed {
                overlay::vanished(&tx, stored.row.id, account, folder, &mut refusals)?;
            }
        }
        let mut rows: Vec<RowChange> = removed
            .iter()
            .map(|stored| stored.row)
            .chain(
                flagged
                    .iter()
                    .filter(|(stored, shown)| *shown || stored.keywords_changed)
                    .map(|(stored, _)| stored.row),
            )
            .collect();
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let positions = removed_positions(&tx, &open.open, &rows)?;
        let mut threads = BTreeSet::new();
        let mut blobs = BTreeSet::new();
        {
            let mut delete = tx.prepare_cached("DELETE FROM messages WHERE id = ?1")?;
            let mut unindex = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
            for stored in &removed {
                let (flags, _) = stored.row.before.expect("stored rows have a before");
                delete.execute([stored.row.id.0])?;
                unindex.execute([stored.row.id.0])?;
                overlay::count_delta(&tx, folder, Some(flags), None)?;
                threads.insert(stored.thread);
                blobs.extend(stored.blob);
            }
            let mut update = tx.prepare_cached(
                "UPDATE messages SET flags = ?2, server_flags = ?3, keywords = ?4 WHERE id = ?1",
            )?;
            for (stored, _) in &flagged {
                let (before, _) = stored.row.before.expect("stored rows have a before");
                let (after, _) = stored.row.after.expect("flag changes have an after");
                update.execute(params![
                    stored.row.id.0,
                    after,
                    overlay::server_column(after, stored.server),
                    stored.keywords
                ])?;
                overlay::count_delta(&tx, folder, Some(before), Some(after))?;
            }
        }
        for thread in &threads {
            recompute_thread(&tx, *thread)?;
        }
        let orphaned_blobs = overlay::orphaned(&tx, blobs)?;
        let mut inserted = Vec::new();
        let mut docs = Vec::new();
        let mut merged_threads = Vec::new();
        let mut older_unread = 0;
        let mut waiting = HashMap::new();
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
            let flags = overlay_new(&tx, &mut waiting, message, id, uid)?;
            if uid < i64::from(old_floor) {
                older_unread += unread(message.flags.bits());
            }
            threads.insert(thread);
            rows.push(RowChange {
                id,
                folder,
                date: message.date,
                before: None,
                after: Some((flags, message.has_attachments)),
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
        if changes.below.is_some() || older_unread > 0 {
            tx.prepare_cached(
                "UPDATE folders SET below_unread = max(0, min(below_unread - ?2, below_floor)) \
                 WHERE id = ?1",
            )?
            .execute(params![folder.0, older_unread])?;
        }
        refusals.record(&tx)?;
        let mut synced = FolderSynced {
            version: StoreVersion(self.state.version),
            inserted: count(inserted.len()),
            changed: changed_on_server,
            removed: count(removed.len()),
            orphaned_blobs,
        };
        if rows.is_empty() && below_counts(&tx, folder)? == below_before {
            tx.commit()?;
            drop(open);
            self.state.after_commit();
            return Ok(synced);
        }
        let views = view_changes(&tx, &open.open, &rows, &positions)?;
        let event = ChangeEvent {
            inserted,
            removed: removed.iter().map(|stored| stored.row.id).collect(),
            changed: flagged
                .iter()
                .filter(|(stored, shown)| *shown || stored.keywords_changed)
                .map(|(stored, _)| stored.row.id)
                .collect(),
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

    /// Folders gone from the server take their ops with them, refused, and moves into them are
    /// refused too: those messages show again where they are.
    fn replace_folders(
        &mut self,
        account: AccountId,
        folders: &[FolderSpec],
    ) -> Result<FoldersReplaced, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(ids) = unchanged(&tx, account, folders)? {
            // The usual account pass: the same list. Only a new folder's STATUS counts may
            // have moved, and nothing else is written or published.
            let counted = server_counts(&tx, &ids, folders)?;
            if counted.is_empty() {
                tx.commit()?;
                return Ok(FoldersReplaced {
                    version: StoreVersion(self.state.version),
                    ids,
                    ..FoldersReplaced::default()
                });
            }
            let event = ChangeEvent {
                folders: folder_counts(&tx, &counted)?,
                ..ChangeEvent::default()
            };
            let version = self.state.commit(tx, event)?;
            return Ok(FoldersReplaced {
                version,
                ids,
                ..FoldersReplaced::default()
            });
        }
        let (ids, mut counted) = upsert_rows(&tx, folders)?;
        let listed: HashSet<FolderId> = ids.iter().copied().collect();
        let gone: Vec<FolderId> = tx
            .prepare_cached("SELECT id FROM folders WHERE account_id = ?1 ORDER BY id")?
            .query_map([account.0], |row| row.get(0).map(FolderId))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|folder| !listed.contains(folder))
            .collect();
        let (refusals, back) = refuse_gone(&tx, &gone)?;
        refusals.record(&tx)?;
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
        let orphaned_blobs = overlay::orphaned(&tx, blobs)?;
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        // Messages whose move into a folder now gone was refused show again where they are.
        let rows: Vec<RowChange> = back
            .iter()
            .filter_map(|plan| match plan.after {
                overlay::After::Stay { flags, .. } if flags != plan.row.flags => Some(RowChange {
                    id: plan.row.id,
                    folder: plan.row.folder,
                    date: plan.row.date,
                    before: Some((plan.row.flags, plan.row.has_attachments)),
                    after: Some((flags, plan.row.has_attachments)),
                }),
                _ => None,
            })
            .collect();
        let positions = removed_positions(&tx, &open.open, &rows)?;
        let mut changed = Vec::new();
        {
            let mut update = tx.prepare_cached(
                "UPDATE messages SET flags = ?2, server_flags = ?3 WHERE id = ?1",
            )?;
            for plan in &back {
                if let overlay::After::Stay { flags, server } = plan.after
                    && (flags, server) != (plan.row.flags, plan.row.server)
                {
                    update.execute(params![
                        plan.row.id.0,
                        flags,
                        overlay::server_column(flags, server)
                    ])?;
                    if overlay::count_delta(
                        &tx,
                        plan.row.folder,
                        Some(plan.row.flags),
                        Some(flags),
                    )? {
                        counted.insert(plan.row.folder);
                    }
                    if flags != plan.row.flags {
                        changed.push(plan.row.id);
                    }
                }
            }
        }
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
        let reloaded: BTreeSet<_> = views.iter().map(|change| change.view).collect();
        views.extend(
            view_changes(&tx, &open.open, &rows, &positions)?
                .into_iter()
                .filter(|change| !reloaded.contains(&change.view)),
        );
        let event = ChangeEvent {
            removed: removed.clone(),
            changed,
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

    fn count_below(
        &mut self,
        folder: FolderId,
        status: &ServerStatus,
    ) -> Result<Versioned<bool>, StoreError> {
        self.state.account(folder)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let unchanged = Versioned {
            version: StoreVersion(self.state.version),
            value: false,
        };
        // A windowed folder, and a STATUS no older than what the store holds: one taken before
        // mail or flags the store has since stored would count them twice, or not at all.
        let own: Option<(u32, u32)> = tx
            .query_row(
                "SELECT total_count, unread_count FROM folders \
                 WHERE id = ?1 AND sync_state = 'ready' AND sync_floor > 1 \
                   AND uidvalidity = ?2 AND uidnext <= ?3 \
                   AND (?4 IS NULL OR highestmodseq <= ?4)",
                params![
                    folder.0,
                    status.uidvalidity,
                    status.uidnext,
                    status.highestmodseq.map(|modseq| modseq as i64)
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((shown_total, shown_unread)) = own else {
            return Ok(unchanged);
        };
        // STATUS counts what the server holds: rows hidden by a pending op or `\Deleted`, and
        // the server's own flags.
        let (hidden, unread_off): (i64, i64) = tx
            .prepare_cached(
                "SELECT count(*) FILTER (WHERE flags & 264 <> 0), \
                   coalesce(sum((coalesce(server_flags, flags) & 1 = 0) - (flags & 265 = 0)), 0) \
                 FROM messages INDEXED BY messages_special \
                 WHERE folder_id = ?1 AND (server_flags IS NOT NULL OR flags & 8 <> 0)",
            )?
            .query_row([folder.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let total = (i64::from(shown_total) + hidden).max(0) as u32;
        let unread = (i64::from(shown_unread) + unread_off).max(0) as u32;
        let below_floor = status.counts.total.saturating_sub(total);
        let below_unread = status.counts.unread.saturating_sub(unread).min(below_floor);
        if below_counts(&tx, folder)? == (below_floor, below_unread) {
            return Ok(unchanged);
        }
        tx.prepare_cached("UPDATE folders SET below_floor = ?2, below_unread = ?3 WHERE id = ?1")?
            .execute(params![folder.0, below_floor, below_unread])?;
        let event = ChangeEvent {
            folders: folder_counts(&tx, &BTreeSet::from([folder]))?,
            ..ChangeEvent::default()
        };
        Ok(Versioned {
            version: self.state.commit(tx, event)?,
            value: true,
        })
    }
}

/// Refuses the pending ops of folders gone from the server: those of their messages, those
/// waiting there for their messages, their reads below the floor, and moves into them. The
/// messages whose move was refused show again where they are (the returned plans).
fn refuse_gone(
    tx: &rusqlite::Connection,
    gone: &[FolderId],
) -> rusqlite::Result<(overlay::Refusals, Vec<overlay::RowPlan>)> {
    let mut refusals = overlay::Refusals::default();
    let mut moved = BTreeSet::new();
    let gone_set: HashSet<FolderId> = gone.iter().copied().collect();
    for folder in gone {
        let held: Vec<MessageId> = tx
            .prepare_cached(
                "SELECT DISTINCT message FROM ops \
                 WHERE folder_id = ?1 AND done = 0 AND message IS NOT NULL",
            )?
            .query_map([folder.0], |row| row.get(0).map(MessageId))?
            .collect::<rusqlite::Result<_>>()?;
        for id in held {
            if let Some(row) = overlay::row(tx, id)? {
                let chain = overlay::chain(tx, id)?;
                refusals.refuse(tx, row.account, row.folder, &chain)?;
            }
        }
        for (account, ops) in overlay::waiting(tx, *folder)? {
            refusals.refuse(tx, account, *folder, &ops)?;
        }
        for (op, account, below) in overlay::reads_below(tx, *folder)? {
            tx.execute("DELETE FROM ops WHERE id = ?1", [op.0])?;
            refusals.add(account, RefusedChange::FolderRead, below);
        }
        let moving: Vec<MessageId> = tx
            .prepare_cached(
                "SELECT DISTINCT message FROM ops \
                 WHERE target = ?1 AND done = 0 AND message IS NOT NULL",
            )?
            .query_map([folder.0], |row| row.get(0).map(MessageId))?
            .collect::<rusqlite::Result<_>>()?;
        for id in moving {
            let Some(row) = overlay::row(tx, id)? else {
                continue;
            };
            if gone_set.contains(&row.folder) {
                continue;
            }
            let into: Vec<overlay::Pending> = overlay::chain(tx, id)?
                .into_iter()
                .filter(|op| matches!(op.change, Change::Move { to, .. } if to == *folder))
                .collect();
            refusals.refuse(tx, row.account, row.folder, &into)?;
            moved.insert(id);
        }
    }
    let mut back = Vec::new();
    for id in moved {
        if let Some(row) = overlay::row(tx, id)? {
            let flags = overlay::shown(tx, &row, row.server)?;
            back.push(overlay::RowPlan {
                after: overlay::After::Stay {
                    flags,
                    server: row.server,
                },
                row,
            });
        }
    }
    Ok((refusals, back))
}

/// The listed folders' ids in input order when `folders` is exactly what the store holds for
/// `account` (names, delimiters, roles, attributes, selectability, and no other folder).
fn unchanged(
    tx: &rusqlite::Connection,
    account: AccountId,
    folders: &[FolderSpec],
) -> rusqlite::Result<Option<Vec<FolderId>>> {
    type Row = (FolderId, String, Option<String>, String, String, bool);
    let stored: HashMap<String, Row> = tx
        .prepare_cached(
            "SELECT name_raw, id, name, delimiter, role, attributes, selectable \
             FROM folders WHERE account_id = ?1",
        )?
        .query_map([account.0], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    FolderId(row.get(1)?),
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if stored.len() != folders.len() {
        return Ok(None);
    }
    let mut ids = Vec::with_capacity(folders.len());
    for folder in folders {
        let Some((id, name, delimiter, role, attributes, selectable)) =
            stored.get(&folder.name_raw)
        else {
            return Ok(None);
        };
        let same = folder.account == account
            && *name == folder.name
            && *delimiter == folder.delimiter.map(String::from)
            && role == folder.role.as_str()
            && *attributes == folder.attributes.join(" ")
            && *selectable == folder.selectable;
        if !same {
            return Ok(None);
        }
        ids.push(*id);
    }
    Ok(Some(ids))
}

/// Writes the STATUS counts of folders still new and empty where they changed; returns those.
fn server_counts(
    tx: &rusqlite::Connection,
    ids: &[FolderId],
    folders: &[FolderSpec],
) -> rusqlite::Result<BTreeSet<FolderId>> {
    let mut counted = BTreeSet::new();
    let mut update = tx.prepare_cached(
        "UPDATE folders SET total_count = ?2, unread_count = ?3 \
         WHERE id = ?1 AND sync_state = 'new' \
           AND (total_count <> ?2 OR unread_count <> ?3) \
           AND NOT EXISTS (SELECT 1 FROM messages WHERE folder_id = ?1)",
    )?;
    for (id, folder) in ids.iter().zip(folders) {
        if let Some(server) = folder.server_counts.filter(|_| folder.selectable)
            && update.execute(params![id.0, server.total, server.unread])? > 0
        {
            counted.insert(*id);
        }
    }
    Ok(counted)
}

fn unread(flags: u32) -> i64 {
    i64::from(flags & Flags::SEEN.bits() == 0)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
