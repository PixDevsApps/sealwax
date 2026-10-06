//! The overlay's writes (ADR-015): a row shows its server flags with its pending ops applied,
//! only shown rows count, and a transaction's row changes go out as one exact change event.
//! Sync writes the server's state into `server_flags` (or `flags` where nothing is pending) and
//! never drops an overlay whose op is pending.

use super::{
    RowChange, State, folder_counts, recompute_thread, removed_positions, thread_counts,
    view_changes,
};
use crate::store::StoreError;
use crate::store::api::{ChangeEvent, StoreVersion};
use crate::store::ops_api::{OpId, OpsRefusal, RefusedChange};
use crate::store::overlay::{self, BATCH_FOLDER_READ, Change, Folded, HIDDEN, READ_BELOW};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sealwax_domain::model::{AccountId, BlobKey, FolderId, MessageId, ThreadId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// A stored row as the op queue reads it.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) id: MessageId,
    pub(crate) account: AccountId,
    pub(crate) folder: FolderId,
    pub(crate) uid: u32,
    /// The folder's UIDVALIDITY; 0 for local folders.
    pub(crate) uidvalidity: u32,
    pub(crate) thread: ThreadId,
    pub(crate) date: i64,
    /// What is shown.
    pub(crate) flags: u32,
    /// The server's.
    pub(crate) server: u32,
    pub(crate) has_attachments: bool,
    pub(crate) blob: Option<BlobKey>,
    pub(crate) message_id: Option<String>,
}

const ROW_COLUMNS: &str = "m.id, f.account_id, m.folder_id, m.uid, coalesce(f.uidvalidity, 0), \
    m.thread_id, m.date, m.flags, coalesce(m.server_flags, m.flags), m.has_attachments, \
    m.blob_key, m.message_id";

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        id: MessageId(row.get(0)?),
        account: AccountId(row.get(1)?),
        folder: FolderId(row.get(2)?),
        uid: row.get(3)?,
        uidvalidity: row.get(4)?,
        thread: ThreadId(row.get(5)?),
        date: row.get(6)?,
        flags: row.get(7)?,
        server: row.get::<_, u32>(8)? & !HIDDEN,
        has_attachments: row.get(9)?,
        blob: row
            .get::<_, Option<Vec<u8>>>(10)?
            .and_then(|key| BlobKey::from_slice(&key)),
        message_id: row.get(11)?,
    })
}

pub(crate) fn row(tx: &Connection, id: MessageId) -> rusqlite::Result<Option<Row>> {
    tx.prepare_cached(&format!(
        "SELECT {ROW_COLUMNS} FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.id = ?1"
    ))?
    .query_row([id.0], row_from)
    .optional()
}

/// The row of `folder` with `uid`.
pub(crate) fn row_at(tx: &Connection, folder: FolderId, uid: u32) -> rusqlite::Result<Option<Row>> {
    tx.prepare_cached(&format!(
        "SELECT {ROW_COLUMNS} FROM messages m JOIN folders f ON f.id = m.folder_id \
         WHERE m.folder_id = ?1 AND m.uid = ?2"
    ))?
    .query_row(params![folder.0, uid], row_from)
    .optional()
}

/// A pending op of a message.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pending {
    pub(crate) id: OpId,
    pub(crate) change: Change,
    pub(crate) batch: i64,
    pub(crate) attempts: u32,
    pub(crate) not_before: i64,
}

/// The pending ops of a message, oldest first.
pub(crate) fn chain(tx: &Connection, message: MessageId) -> rusqlite::Result<Vec<Pending>> {
    let mut select = tx.prepare_cached(
        "SELECT id, kind, target, add_flags, remove_flags, batch, attempts, not_before \
         FROM ops WHERE message = ?1 AND done = 0 ORDER BY id",
    )?;
    let rows = select.query_map([message.0], |row| {
        Ok((
            OpId(row.get(0)?),
            Change::from_row(row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
        ))
    })?;
    let mut chain = Vec::new();
    for row in rows {
        let (id, change, batch, attempts, not_before) = row?;
        if let Some(change) = change {
            chain.push(Pending {
                id,
                change,
                batch,
                attempts,
                not_before,
            });
        }
    }
    Ok(chain)
}

/// Whether a folder marked read, pending, covers a row with `uid` (below its floor).
pub(crate) fn read_below(
    tx: &Connection,
    folder: FolderId,
    uidvalidity: u32,
    uid: u32,
) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM ops WHERE folder_id = ?1 AND done = 0 AND message IS NULL \
           AND kind = 3 AND uidvalidity = ?2 AND uid > ?3)",
    )?
    .query_row(params![folder.0, uidvalidity, uid], |row| row.get(0))
}

/// What a message at `folder` (with `uidvalidity`) under `uid` shows with server flags
/// `server` and its pending ops.
pub(crate) fn shown_at(
    tx: &Connection,
    id: MessageId,
    folder: FolderId,
    uidvalidity: u32,
    uid: u32,
    server: u32,
) -> rusqlite::Result<u32> {
    let chain = chain(tx, id)?;
    let read = read_below(tx, folder, uidvalidity, uid)?;
    Ok(overlay::fold(folder, chain.iter().map(|op| op.change)).shown(server, read))
}

/// What `row` shows with new server flags where it is.
pub(crate) fn shown(tx: &Connection, row: &Row, server: u32) -> rusqlite::Result<u32> {
    shown_at(tx, row.id, row.folder, row.uidvalidity, row.uid, server)
}

/// Whether an account has ops not yet done; without any, sync writes shown flags directly.
pub(crate) fn has_pending(tx: &Connection, account: AccountId) -> rusqlite::Result<bool> {
    tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM ops WHERE account_id = ?1 AND done = 0)")?
        .query_row([account.0], |row| row.get(0))
}

/// Whether a folder has ops waiting for their messages (after a UIDVALIDITY change) or a
/// pending read below its floor, which new rows must see.
pub(crate) fn has_waiting(tx: &Connection, folder: FolderId) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM ops WHERE folder_id = ?1 AND done = 0 AND message IS NULL)",
    )?
    .query_row([folder.0], |row| row.get(0))
}

/// Gives a new row of `folder` the ops that wait there for a message with its Message-ID: those
/// of the oldest message that left with a UIDVALIDITY change and has none yet.
pub(crate) fn attach_waiting(
    tx: &Connection,
    folder: FolderId,
    id: MessageId,
    message_id: Option<&str>,
    uid: u32,
    uidvalidity: u32,
) -> rusqlite::Result<bool> {
    let Some(message_id) = message_id else {
        return Ok(false);
    };
    let old: Option<(u32, u32)> = tx
        .prepare_cached(
            "SELECT uidvalidity, uid FROM ops WHERE folder_id = ?1 AND done = 0 \
               AND message IS NULL AND kind <> 3 AND message_id = ?2 ORDER BY id LIMIT 1",
        )?
        .query_row(params![folder.0, message_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let Some((old_uidvalidity, old_uid)) = old else {
        return Ok(false);
    };
    tx.prepare_cached(
        "UPDATE ops SET message = ?3, uid = ?4, uidvalidity = ?5 WHERE folder_id = ?1 \
           AND done = 0 AND message IS NULL AND kind <> 3 AND message_id = ?2 \
           AND uidvalidity = ?6 AND uid = ?7",
    )?
    .execute(params![
        folder.0,
        message_id,
        id.0,
        uid,
        uidvalidity,
        old_uidvalidity,
        old_uid
    ])?;
    Ok(true)
}

/// Moves the pending ops of `from` to the row `to` at its place: re-homed after a move, or the
/// copy the target's sync stored first.
pub(crate) fn rehome_chain(
    tx: &Connection,
    from: MessageId,
    to: MessageId,
    folder: FolderId,
    uid: u32,
    uidvalidity: u32,
) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE ops SET message = ?2, folder_id = ?3, uid = ?4, uidvalidity = ?5 \
         WHERE message = ?1 AND done = 0",
    )?
    .execute(params![from.0, to.0, folder.0, uid, uidvalidity])?;
    Ok(())
}

/// The `server_flags` column for a row showing `flags`: NULL while they are the server's.
pub(crate) fn server_column(flags: u32, server: u32) -> Option<u32> {
    (flags != server).then_some(server)
}

/// Adjusts a folder's counts for a row going from `before` to `after` (None: not in the
/// folder); replies whether they changed.
pub(crate) fn count_delta(
    tx: &Connection,
    folder: FolderId,
    before: Option<u32>,
    after: Option<u32>,
) -> rusqlite::Result<bool> {
    let mut deltas = Deltas::default();
    deltas.add(folder, before, after);
    deltas.apply(tx).map(|changed| !changed.is_empty())
}

/// Count changes of many rows, written once per folder.
#[derive(Default)]
pub(crate) struct Deltas(BTreeMap<FolderId, (i64, i64)>);

impl Deltas {
    pub(crate) fn add(&mut self, folder: FolderId, before: Option<u32>, after: Option<u32>) {
        let (total_before, unread_before) = before.map_or((0, 0), overlay::counted);
        let (total_after, unread_after) = after.map_or((0, 0), overlay::counted);
        let delta = self.0.entry(folder).or_default();
        delta.0 += total_after - total_before;
        delta.1 += unread_after - unread_before;
    }

    /// Writes them; replies with the folders whose counts changed.
    pub(crate) fn apply(self, tx: &Connection) -> rusqlite::Result<BTreeSet<FolderId>> {
        let mut update = tx.prepare_cached(
            "UPDATE folders SET total_count = total_count + ?2, unread_count = unread_count + ?3 \
             WHERE id = ?1",
        )?;
        let mut changed = BTreeSet::new();
        for (folder, (total, unread)) in self.0 {
            if (total, unread) != (0, 0) {
                update.execute(params![folder.0, total, unread])?;
                changed.insert(folder);
            }
        }
        Ok(changed)
    }
}

/// Whether any of `batches` marked a folder read.
pub(crate) fn folder_read(tx: &Connection, batches: &[i64]) -> rusqlite::Result<bool> {
    let mut select = tx.prepare_cached("SELECT kind FROM op_batches WHERE id = ?1")?;
    for batch in batches {
        let kind: Option<i64> = select.query_row([batch], |row| row.get(0)).optional()?;
        if kind == Some(BATCH_FOLDER_READ) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The kind of change a run of refused ops makes.
pub(crate) fn refused_change(folded: &Folded, folder_read: bool) -> RefusedChange {
    if folded.expunge {
        RefusedChange::Expunge
    } else if folded.at != folded.from {
        RefusedChange::Move
    } else if folder_read {
        RefusedChange::FolderRead
    } else {
        RefusedChange::Flags
    }
}

/// Refused changes by account and kind.
#[derive(Default)]
pub(crate) struct Refusals(BTreeMap<(AccountId, RefusedChange), u32>);

impl Refusals {
    pub(crate) fn add(&mut self, account: AccountId, change: RefusedChange, messages: u32) {
        *self.0.entry((account, change)).or_default() += messages;
    }

    /// Drops `ops` (one message's run, oldest first, which started at `from`) as refused.
    pub(crate) fn refuse(
        &mut self,
        tx: &Connection,
        account: AccountId,
        from: FolderId,
        ops: &[Pending],
    ) -> rusqlite::Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let folded = overlay::fold(from, ops.iter().map(|op| op.change));
        let batches: Vec<i64> = ops.iter().map(|op| op.batch).collect();
        self.add(
            account,
            refused_change(&folded, folder_read(tx, &batches)?),
            1,
        );
        let mut delete = tx.prepare_cached("DELETE FROM ops WHERE id = ?1")?;
        for op in ops {
            delete.execute([op.id.0])?;
        }
        Ok(())
    }

    pub(crate) fn into_vec(self) -> Vec<OpsRefusal> {
        self.0
            .into_iter()
            .map(|((account, change), messages)| OpsRefusal {
                account,
                change,
                messages,
            })
            .collect()
    }

    /// Keeps them for the account's op runner, which takes them with its due ops.
    pub(crate) fn record(self, tx: &Connection) -> rusqlite::Result<()> {
        let mut insert = tx.prepare_cached(
            "INSERT INTO op_refusals(account_id, change, messages) VALUES (?1, ?2, ?3)",
        )?;
        for ((account, change), messages) in self.0 {
            insert.execute(params![account.0, change_code(change), messages])?;
        }
        Ok(())
    }
}

pub(crate) fn change_code(change: RefusedChange) -> i64 {
    match change {
        RefusedChange::Flags => 0,
        RefusedChange::Move => 1,
        RefusedChange::Expunge => 2,
        RefusedChange::FolderRead => 3,
    }
}

pub(crate) fn change_from_code(code: i64) -> RefusedChange {
    match code {
        1 => RefusedChange::Move,
        2 => RefusedChange::Expunge,
        3 => RefusedChange::FolderRead,
        _ => RefusedChange::Flags,
    }
}

/// What sync's sight of a row leaving its folder means for the row's pending ops (S1): a move
/// or expunge among them counts as done, with the flag changes before it; ops after a move would
/// apply where the message went, which the store doesn't know, so they are refused; flag
/// changes of a message deleted elsewhere are moot. Call before the row is deleted.
pub(crate) fn vanished(
    tx: &Connection,
    id: MessageId,
    account: AccountId,
    folder: FolderId,
    refusals: &mut Refusals,
) -> rusqlite::Result<()> {
    let chain = chain(tx, id)?;
    if chain.is_empty() {
        return Ok(());
    }
    let mut end = chain.len();
    let mut moved = false;
    for n in 0..chain.len() {
        let folded = overlay::fold(folder, chain[..=n].iter().map(|op| op.change));
        if folded.leaves() {
            end = n + 1;
            moved = !folded.expunge;
            break;
        }
    }
    let mut done = tx.prepare_cached("UPDATE ops SET done = 1 WHERE id = ?1")?;
    for op in &chain[..end] {
        done.execute([op.id.0])?;
    }
    if moved {
        let to = overlay::fold(folder, chain[..end].iter().map(|op| op.change)).at;
        refusals.refuse(tx, account, to, &chain[end..])?;
    } else {
        for op in &chain[end..] {
            done.execute([op.id.0])?;
        }
    }
    Ok(())
}

/// The pending ops of a row a UIDVALIDITY change takes: they wait in its folder for the next
/// sync to bring the message back, found by its Message-ID; without one they are refused. Call
/// before the row is deleted.
pub(crate) fn detach(tx: &Connection, row: &Row, refusals: &mut Refusals) -> rusqlite::Result<()> {
    let chain = chain(tx, row.id)?;
    if chain.is_empty() {
        return Ok(());
    }
    if row.message_id.is_none() {
        return refusals.refuse(tx, row.account, row.folder, &chain);
    }
    tx.prepare_cached("UPDATE ops SET message = NULL WHERE message = ?1 AND done = 0")?
        .execute([row.id.0])?;
    Ok(())
}

/// What a transaction does to one row.
#[derive(Clone, Debug)]
pub(crate) enum After {
    /// Stays where it is with new shown and server flags.
    Stay {
        flags: u32,
        server: u32,
    },
    /// Re-homed in `folder` under `uid` (body, parts and full text kept).
    Moved {
        folder: FolderId,
        uid: u32,
        flags: u32,
        server: u32,
    },
    Deleted,
}

#[derive(Clone, Debug)]
pub(crate) struct RowPlan {
    /// The row as it was.
    pub(crate) row: Row,
    pub(crate) after: After,
}

/// What [`State::commit_rows`] did.
pub(crate) struct Committed {
    pub(crate) version: StoreVersion,
    pub(crate) orphaned_blobs: Vec<BlobKey>,
}

impl State {
    /// Applies `plans` and commits `tx` with one exact change event for them and for the counts
    /// of `folders` (changed otherwise); commits silently when nothing shown changed.
    pub(crate) fn commit_rows(
        &mut self,
        tx: Transaction<'_>,
        plans: &[RowPlan],
        mut folders: BTreeSet<FolderId>,
    ) -> Result<Committed, StoreError> {
        let mut rows = Vec::new();
        for plan in plans {
            let row = &plan.row;
            let before = Some((row.flags, row.has_attachments));
            let change = |folder, before, after| RowChange {
                id: row.id,
                folder,
                date: row.date,
                before,
                after,
            };
            match plan.after {
                After::Stay { flags, .. } if flags != row.flags => {
                    rows.push(change(
                        row.folder,
                        before,
                        Some((flags, row.has_attachments)),
                    ));
                }
                After::Stay { .. } => {}
                After::Moved { folder, flags, .. } => {
                    rows.push(change(row.folder, before, None));
                    rows.push(change(folder, None, Some((flags, row.has_attachments))));
                }
                After::Deleted => rows.push(change(row.folder, before, None)),
            }
        }
        let shared = Arc::clone(&self.shared);
        let open = shared.registry.lock();
        let positions = removed_positions(&tx, &open.open, &rows)?;
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        let mut threads = BTreeSet::new();
        let mut blobs = BTreeSet::new();
        let mut deltas = Deltas::default();
        {
            let mut update = tx.prepare_cached(
                "UPDATE messages SET flags = ?2, server_flags = ?3 WHERE id = ?1",
            )?;
            let mut rehome = tx.prepare_cached(
                "UPDATE messages SET folder_id = ?2, uid = ?3, flags = ?4, server_flags = ?5 \
                 WHERE id = ?1",
            )?;
            let mut delete = tx.prepare_cached("DELETE FROM messages WHERE id = ?1")?;
            let mut unindex = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
            for plan in plans {
                let row = &plan.row;
                match plan.after {
                    After::Stay { flags, server } => {
                        if flags == row.flags && server == row.server {
                            continue;
                        }
                        update.execute(params![row.id.0, flags, server_column(flags, server)])?;
                        deltas.add(row.folder, Some(row.flags), Some(flags));
                        if flags != row.flags {
                            changed.push(row.id);
                        }
                    }
                    After::Moved {
                        folder,
                        uid,
                        flags,
                        server,
                    } => {
                        rehome.execute(params![
                            row.id.0,
                            folder.0,
                            uid,
                            flags,
                            server_column(flags, server)
                        ])?;
                        deltas.add(row.folder, Some(row.flags), None);
                        deltas.add(folder, None, Some(flags));
                        changed.push(row.id);
                    }
                    After::Deleted => {
                        delete.execute([row.id.0])?;
                        unindex.execute([row.id.0])?;
                        deltas.add(row.folder, Some(row.flags), None);
                        threads.insert(row.thread);
                        blobs.extend(row.blob);
                        removed.push(row.id);
                    }
                }
            }
        }
        folders.extend(deltas.apply(&tx)?);
        for thread in &threads {
            recompute_thread(&tx, *thread)?;
        }
        let orphaned_blobs = orphaned(&tx, blobs)?;
        if rows.is_empty() && folders.is_empty() {
            tx.commit()?;
            drop(open);
            self.after_commit();
            return Ok(Committed {
                version: StoreVersion(self.version),
                orphaned_blobs,
            });
        }
        let views = view_changes(&tx, &open.open, &rows, &positions)?;
        let event = ChangeEvent {
            removed,
            changed,
            folders: folder_counts(&tx, &folders)?,
            threads: thread_counts(&tx, &threads)?,
            views,
            ..ChangeEvent::default()
        };
        let version = self.commit(tx, event)?;
        drop(open);
        Ok(Committed {
            version,
            orphaned_blobs,
        })
    }
}

/// Of `blobs`, those no row refers to any more.
pub(crate) fn orphaned(
    tx: &Connection,
    blobs: BTreeSet<BlobKey>,
) -> rusqlite::Result<Vec<BlobKey>> {
    let mut used = tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM messages INDEXED BY messages_blob WHERE blob_key = ?1)",
    )?;
    let mut out = Vec::new();
    for key in blobs {
        if !used.query_row([key.0.as_slice()], |row| row.get::<_, bool>(0))? {
            out.push(key);
        }
    }
    Ok(out)
}

/// The pending ops waiting in `folder` for their messages, by the message they left with
/// (UIDVALIDITY and UID), each run oldest first.
pub(crate) fn waiting(
    tx: &Connection,
    folder: FolderId,
) -> rusqlite::Result<Vec<(AccountId, Vec<Pending>)>> {
    let mut select = tx.prepare_cached(
        "SELECT account_id, uidvalidity, uid, id, kind, target, add_flags, remove_flags, batch, \
           attempts, not_before FROM ops \
         WHERE folder_id = ?1 AND done = 0 AND message IS NULL AND kind <> 3 \
         ORDER BY uidvalidity, uid, id",
    )?;
    let rows = select.query_map([folder.0], |row| {
        Ok((
            AccountId(row.get(0)?),
            (row.get::<_, u32>(1)?, row.get::<_, u32>(2)?),
            OpId(row.get(3)?),
            Change::from_row(row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?),
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
        ))
    })?;
    let mut out: Vec<(AccountId, (u32, u32), Vec<Pending>)> = Vec::new();
    for row in rows {
        let (account, key, id, change, batch, attempts, not_before) = row?;
        let Some(change) = change else {
            continue;
        };
        let op = Pending {
            id,
            change,
            batch,
            attempts,
            not_before,
        };
        match out.last_mut() {
            Some((_, last, ops)) if *last == key => ops.push(op),
            _ => out.push((account, key, vec![op])),
        }
    }
    Ok(out
        .into_iter()
        .map(|(account, _, ops)| (account, ops))
        .collect())
}

/// Pending folder-read ops of `folder`: their ids, accounts and the unread they covered.
pub(crate) fn reads_below(
    tx: &Connection,
    folder: FolderId,
) -> rusqlite::Result<Vec<(OpId, AccountId, u32)>> {
    tx.prepare_cached(
        "SELECT id, account_id, below FROM ops \
         WHERE folder_id = ?1 AND done = 0 AND message IS NULL AND kind = ?2",
    )?
    .query_map(params![folder.0, READ_BELOW], |row| {
        Ok((OpId(row.get(0)?), AccountId(row.get(1)?), row.get(2)?))
    })?
    .collect()
}
