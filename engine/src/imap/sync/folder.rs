//! The folder pass: S1's `sync_folder`, `incremental`, `fetch_new` and `uid_diff` on the
//! production store, within the folder's window. Local rows are expected only from the floor
//! up: the count check subtracts the messages the server holds below it, the UID diff searches
//! from it, and flag FETCHes cover the floor's range only. The rows a step stores at or above
//! the UIDNEXT the store held when the pass began are new mail ([`Arrived`]).

use super::view::{self, View};
use super::{Arrived, FolderLocks, SyncOptions, SyncReport, count, load_state};
use crate::imap::connection::Connection;
use crate::imap::error::SyncError;
use crate::imap::first_sync::{self, BATCH, Progress};
use crate::imap::raw;
use sealwax_domain::model::{Flags, FolderId, MessageId};
use sealwax_infrastructure::store::{
    FlagUpdate, FolderChanges, FolderSync, NewMessage, Store, SyncState,
};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// One pass over one folder, holding its lock: EXAMINE; a UIDVALIDITY change resets the folder;
/// an unfinished first sync continues within the window (`progress` follows its batches); then
/// new mail, flag changes, removals and the count check. Also the entry point for a folder the
/// user opened, whatever the policy says. A folder that vanished since it was listed fails
/// with the server's NO.
pub async fn sync_folder(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    options: &SyncOptions,
    locks: &FolderLocks,
    mut progress: impl FnMut(Progress),
) -> Result<SyncReport, SyncError> {
    let _held = locks.lock(folder).await;
    let started = Instant::now();
    let mut report = SyncReport::default();
    pass(conn, store, folder, options, &mut progress, &mut report).await?;
    tracing::debug!(
        %folder,
        fast = report.stats.fast_path,
        stored = report.stats.stored,
        flags = report.stats.flag_changes,
        removed = report.stats.removed,
        diffs = report.stats.uid_diffs,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "folder pass done"
    );
    Ok(report)
}

/// The pass itself; the caller holds the folder's lock.
pub(super) async fn pass(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    options: &SyncOptions,
    progress: &mut impl FnMut(Progress),
    report: &mut SyncReport,
) -> Result<(), SyncError> {
    let mut state = load_state(store, folder).await?;
    if !state.selectable {
        return Err(SyncError::NotSelectable);
    }
    let mut view = view::examine(conn, &state.name_raw).await?;
    view.unreadable = options.unreadable.clone();
    report.stats.passes += 1;
    if state
        .uidvalidity
        .is_some_and(|known| known != view.uidvalidity)
    {
        let removed = store.reset_folder(folder, view.uidvalidity).recv().await?;
        report.stats.resets += 1;
        report.orphaned_blobs.extend(removed.orphaned_blobs);
        state = load_state(store, folder).await?;
    }
    let fresh = state.state != SyncState::Ready;
    if fresh {
        let first = first_sync::run(conn, store, &state, &mut view, options, progress).await?;
        report.stats.first_sync_batches += first.batches;
        report.stats.stored += first.fetched;
        state = load_state(store, folder).await?;
    }
    let arrivals = Arrivals {
        from: (!fresh).then_some(state.uidnext),
        sink: options.arrivals.as_ref(),
    };
    incremental(conn, store, &state, &mut view, arrivals, report).await
}

/// Where a pass reports new mail: the rows it stores at or above `from`, the UIDNEXT the store
/// held when it began. `None` once the pass ran a first sync: those rows are nobody's new mail.
#[derive(Clone, Copy)]
struct Arrivals<'a> {
    from: Option<u32>,
    sink: Option<&'a async_channel::Sender<Arrived>>,
}

impl Arrivals<'_> {
    /// The rows of a committed step that are new mail: `uids` are the step's new rows in order,
    /// `inserted` the rows the store added for them (it skips the UIDs it holds already).
    fn add(
        &self,
        folder: FolderId,
        uids: &[u32],
        inserted: Vec<MessageId>,
        report: &mut SyncReport,
    ) {
        let Some(from) = self.from else {
            return;
        };
        let messages: Vec<MessageId> = if uids.iter().all(|uid| *uid >= from) {
            inserted
        } else if uids.len() == inserted.len() {
            uids.iter()
                .zip(inserted)
                .filter(|(uid, _)| **uid >= from)
                .map(|(_, id)| id)
                .collect()
        } else {
            tracing::debug!(%folder, "new rows not matched to their UIDs; no new mail reported");
            return;
        };
        if messages.is_empty() {
            return;
        }
        let arrived = Arrived { folder, messages };
        if let Some(sink) = self.sink {
            let _ = sink.try_send(arrived.clone());
        }
        report.arrived.push(arrived);
    }
}

/// Commits one step and counts what it did; returns the rows it inserted.
async fn commit(
    store: &Store,
    folder: FolderId,
    changes: FolderChanges,
    report: &mut SyncReport,
) -> Result<Vec<MessageId>, SyncError> {
    let synced = store.sync_changes(folder, changes).recv().await?;
    report.stats.stored += synced.inserted;
    report.stats.flag_changes += synced.changed;
    report.stats.removed += synced.removed;
    report.orphaned_blobs.extend(synced.orphaned_blobs);
    Ok(synced.new_messages)
}

fn row_uids(rows: &[NewMessage]) -> Vec<u32> {
    rows.iter().filter_map(|row| row.uid).collect()
}

/// The stored flags and keywords by UID.
async fn stored(
    store: &Store,
    folder: FolderId,
) -> Result<HashMap<u32, (Flags, Vec<String>)>, SyncError> {
    Ok(store
        .folder_messages(folder)
        .recv()
        .await?
        .into_iter()
        .map(|message| (message.uid, (message.flags, message.keywords)))
        .collect())
}

/// The stored UIDs the VANISHED responses so far named; the view forgets them.
async fn take_vanished(
    store: &Store,
    folder: FolderId,
    view: &mut View,
) -> Result<Vec<u32>, SyncError> {
    if view.vanished.is_empty() {
        return Ok(Vec::new());
    }
    let named = store
        .folder_messages(folder)
        .recv()
        .await?
        .into_iter()
        .map(|message| message.uid)
        .filter(|uid| view.has_vanished(*uid))
        .collect();
    view.vanished.clear();
    Ok(named)
}

async fn incremental(
    conn: &mut Connection,
    store: &Store,
    state: &FolderSync,
    view: &mut View,
    arrivals: Arrivals<'_>,
    report: &mut SyncReport,
) -> Result<(), SyncError> {
    let folder = state.folder;
    let known_next = state.uidnext;
    let floor = state.floor.unwrap_or(1).max(1);
    let modseq = view.highestmodseq.filter(|_| conn.condstore());
    let skipped = count(view.unreadable.skipped(folder).len());
    if view.uidnext == Some(known_next)
        && view.exists == state.messages.saturating_add(state.below + skipped)
        && modseq == Some(state.highestmodseq)
    {
        report.stats.fast_path += 1;
        return Ok(());
    }
    if view.uidnext.is_none_or(|next| next > known_next) {
        fetch_new(conn, store, folder, known_next, view, arrivals, report).await?;
    }
    let mut changes = FolderChanges::default();
    if known_next > floor {
        let range = floor..=known_next - 1;
        let set = format!("{floor}:{}", known_next - 1);
        match modseq {
            Some(server) if server == state.highestmodseq => {}
            Some(server) if server > state.highestmodseq && state.highestmodseq > 0 => {
                let vanished = if conn.qresync() { " VANISHED" } else { "" };
                let command = format!(
                    "UID FETCH {set} (UID FLAGS) (CHANGEDSINCE {}{vanished})",
                    state.highestmodseq
                );
                changes.flags = view::fetch_flags(conn, &command, range, view).await?;
                report.stats.changedsince_fetches += 1;
            }
            _ => {
                let command = format!("UID FETCH {set} (UID FLAGS)");
                let server = view::fetch_flags(conn, &command, range.clone(), view).await?;
                report.stats.full_flag_scans += 1;
                let local = stored(store, folder).await?;
                let present: HashSet<u32> = server.iter().map(|update| update.uid).collect();
                changes.removed = local
                    .keys()
                    .copied()
                    .filter(|uid| range.contains(uid) && !present.contains(uid))
                    .collect();
                changes.flags = server
                    .into_iter()
                    .filter(|update| {
                        local.get(&update.uid).is_some_and(|(flags, keywords)| {
                            *flags != update.flags || *keywords != update.keywords
                        })
                    })
                    .collect();
            }
        }
    }
    let vanished = take_vanished(store, folder, view).await?;
    report.stats.vanished += count(vanished.len());
    changes.removed.extend(vanished);
    changes.highestmodseq = modseq;
    commit(store, folder, changes, report).await?;
    count_check(
        conn,
        store,
        folder,
        floor,
        state.below,
        view,
        arrivals,
        report,
    )
    .await
}

/// S1's count check: the stored count plus the messages below the floor (and those left out as
/// unreadable) must equal the server's EXISTS, kept current through the pass. Too few first fetches new UIDs again;
/// anything else falls back to the UID diff, which also counts the messages below the floor
/// again.
#[allow(clippy::too_many_arguments)]
async fn count_check(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    floor: u32,
    mut below: u32,
    view: &mut View,
    arrivals: Arrivals<'_>,
    report: &mut SyncReport,
) -> Result<(), SyncError> {
    for _ in 0..3 {
        let state = load_state(store, folder).await?;
        let skipped = count(view.unreadable.skipped(folder).len());
        let expected = state.messages.saturating_add(below + skipped);
        if expected == view.exists {
            return Ok(());
        }
        report.stats.count_mismatches += 1;
        if expected < view.exists {
            fetch_new(conn, store, folder, state.uidnext, view, arrivals, report).await?;
            let state = load_state(store, folder).await?;
            if state.messages.saturating_add(below + skipped) == view.exists {
                return Ok(());
            }
        }
        below = uid_diff(conn, store, folder, floor, view, arrivals, report).await?;
    }
    Ok(())
}

/// The flags of fetched rows as flag updates. A row the store already holds at one of these
/// UIDs (a moved message the op runner re-homed under its new UID, M4) is skipped as new, and
/// the flag scan doesn't reach UIDs above the stored UIDNEXT: without these, a flag another
/// client changed just before the move would never reach it.
fn refreshed(rows: &[NewMessage]) -> Vec<FlagUpdate> {
    rows.iter()
        .filter_map(|row| {
            Some(FlagUpdate {
                uid: row.uid?,
                flags: row.flags,
                keywords: row.keywords.clone(),
            })
        })
        .collect()
}

/// New messages from UID `from` up, ascending, each batch committed with its UIDNEXT: one FETCH
/// of `from:*` for a small gap, else a SEARCH and batches.
async fn fetch_new(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    from: u32,
    view: &mut View,
    arrivals: Arrivals<'_>,
    report: &mut SyncReport,
) -> Result<(), SyncError> {
    let gap = view.uidnext.map(|next| next.saturating_sub(from) as usize);
    if gap.is_some_and(|gap| gap <= BATCH) {
        let rows =
            view::fetch_rows(conn, folder, &format!("{from}:*"), |uid| uid >= from, view).await?;
        report.stats.new_mail_fetches += 1;
        let top = rows
            .iter()
            .filter_map(|row| row.uid)
            .max()
            .map_or(from, |uid| uid + 1);
        let fetched = row_uids(&rows);
        let changes = FolderChanges {
            flags: refreshed(&rows),
            new: rows,
            uidnext: Some(top.max(view.uidnext.unwrap_or(from))),
            ..FolderChanges::default()
        };
        let inserted = commit(store, folder, changes, report).await?;
        arrivals.add(folder, &fetched, inserted, report);
        return Ok(());
    }
    let mut uids = view::uid_search(conn, &format!("UID {from}:*"), view).await?;
    uids.retain(|&uid| uid >= from);
    for chunk in uids.chunks(BATCH) {
        let wanted: HashSet<u32> = chunk.iter().copied().collect();
        let set = raw::uid_set(chunk);
        let rows = view::fetch_rows(conn, folder, &set, |uid| wanted.contains(&uid), view).await?;
        report.stats.new_mail_fetches += 1;
        let fetched = row_uids(&rows);
        let changes = FolderChanges {
            flags: refreshed(&rows),
            new: rows,
            uidnext: chunk.last().map(|uid| uid + 1),
            ..FolderChanges::default()
        };
        let inserted = commit(store, folder, changes, report).await?;
        arrivals.add(folder, &fetched, inserted, report);
    }
    if let Some(next) = view.uidnext.filter(|&next| next > from) {
        let changes = FolderChanges {
            uidnext: Some(next),
            ..FolderChanges::default()
        };
        commit(store, folder, changes, report).await?;
    }
    Ok(())
}

/// The fallback that needs nothing but IMAP4rev1: the server's UIDs from the floor up against
/// the stored ones. Returns the messages the server holds below the floor.
async fn uid_diff(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    floor: u32,
    view: &mut View,
    arrivals: Arrivals<'_>,
    report: &mut SyncReport,
) -> Result<u32, SyncError> {
    report.stats.uid_diffs += 1;
    let mut server = view::uid_search(conn, &format!("UID {floor}:*"), view).await?;
    server.retain(|&uid| uid >= floor);
    view.vanished.clear();
    let local: HashSet<u32> = stored(store, folder).await?.into_keys().collect();
    let on_server: HashSet<u32> = server.iter().copied().collect();
    view.unreadable
        .retain(folder, |uid| on_server.contains(&uid));
    let missing: Vec<u32> = server
        .iter()
        .copied()
        .filter(|uid| !local.contains(uid))
        .collect();
    let below = if floor <= 1 {
        0
    } else {
        view.exists.saturating_sub(count(server.len()))
    };
    let mut changes = FolderChanges {
        removed: local
            .into_iter()
            .filter(|uid| !on_server.contains(uid))
            .collect(),
        ..FolderChanges::default()
    };
    for chunk in missing.chunks(BATCH) {
        let wanted: HashSet<u32> = chunk.iter().copied().collect();
        let set = raw::uid_set(chunk);
        changes.new =
            view::fetch_rows(conn, folder, &set, |uid| wanted.contains(&uid), view).await?;
        let fetched = row_uids(&changes.new);
        let inserted = commit(store, folder, std::mem::take(&mut changes), report).await?;
        arrivals.add(folder, &fetched, inserted, report);
    }
    changes.uidnext = server.last().map(|uid| uid + 1);
    changes.below = Some(below);
    commit(store, folder, changes, report).await?;
    view.exists = count(server.len()).saturating_add(below);
    Ok(below)
}
