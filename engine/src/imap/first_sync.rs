//! A folder's first header sync (ML-037), resumable as S1 built it, within the first-sync window
//! (ML-043): EXAMINE, then newest-first batches of UIDs from the top down to the window's floor,
//! each FETCHed, ingested in one store transaction and then recorded as the new floor, so an
//! interrupted sync continues below the last committed batch. The next batch is fetched while
//! the store commits the previous one. Older mail loads on request (`sync::load_older`).

use super::connection::Connection;
use super::error::{ImapError, SyncError};
use super::fetch;
use super::raw;
use super::sync::view::{self, View};
use super::sync::{SyncOptions, load_state};
use sealwax_domain::model::{BlobKey, FolderId};
use sealwax_infrastructure::store::{
    FirstSyncStep, FolderSync, Ingested, Reply, Store, StoreError, SyncState,
};
use std::collections::HashSet;
use std::time::Instant;

/// UIDs per FETCH and per store transaction (the writer's own limit is 200).
pub const BATCH: usize = 200;

/// Headers stored of all the folder holds, newest first; for `Event::SyncProgress`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub fetched: u32,
    pub total: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirstSyncReport {
    /// Messages stored by this run.
    pub fetched: u32,
    /// Messages the window holds, as far as this run knows.
    pub total: u32,
    pub batches: u32,
    /// The run continued an interrupted first sync.
    pub resumed: bool,
    /// The folder's UIDVALIDITY changed, so its old messages were removed first.
    pub reset: bool,
    /// Nothing older than the window is left on the server.
    pub complete: bool,
    /// Raw messages the reset left without a row, for the caller to delete from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// A batch handed to the store, not yet known to be committed.
struct Pending {
    reply: Reply<Ingested>,
    floor: u32,
}

/// Waits for the batch's commit, then records its floor; returns the messages it stored.
async fn commit(store: &Store, folder: FolderId, pending: Pending) -> Result<u32, StoreError> {
    let ingested = pending.reply.recv().await?;
    store
        .first_sync_step(folder, FirstSyncStep::Floor(pending.floor))
        .recv()
        .await?;
    Ok(ingested.ids.iter().flatten().count() as u32)
}

/// Syncs the headers of `folder` into the store within the default window (90 days, at most
/// 5,000 and at least 200 messages), newest first, calling `progress` after every committed
/// batch. A folder whose first sync is done is left to the re-sync; an interrupted one
/// continues where it stopped.
pub async fn first_sync(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    progress: impl FnMut(Progress),
) -> Result<FirstSyncReport, SyncError> {
    first_sync_with(conn, store, folder, &SyncOptions::default(), progress).await
}

/// [`first_sync`] with the window and clock of `options`.
pub async fn first_sync_with(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    options: &SyncOptions,
    mut progress: impl FnMut(Progress),
) -> Result<FirstSyncReport, SyncError> {
    let mut state = load_state(store, folder).await?;
    if !state.selectable {
        return Err(SyncError::NotSelectable);
    }
    let mut view = view::examine(conn, &state.name_raw).await?;
    view.unreadable = options.unreadable.clone();
    let mut reset = None;
    if state
        .uidvalidity
        .is_some_and(|known| known != view.uidvalidity)
    {
        reset = Some(store.reset_folder(folder, view.uidvalidity).recv().await?);
        state = load_state(store, folder).await?;
    }
    let mut report = run(conn, store, &state, &mut view, options, &mut progress).await?;
    if let Some(removed) = reset {
        report.reset = true;
        report.orphaned_blobs = removed.orphaned_blobs;
    }
    Ok(report)
}

/// The window's floor among `uids` (ascending, all below `top`): asks the server which arrived
/// within the window's days, among the newest `max` only, unless the window keeps them all.
async fn window_floor(
    conn: &mut Connection,
    options: &SyncOptions,
    uids: &[u32],
    top: u32,
    view: &mut View,
) -> Result<u32, ImapError> {
    let window = options.window;
    let Some(&lowest) = uids.first() else {
        return Ok(top);
    };
    if window.keeps_all(uids.len()) {
        return Ok(lowest);
    }
    let cap = window.cap(uids).unwrap_or(lowest);
    let criteria = format!(
        "UID {cap}:{} SINCE {}",
        top - 1,
        view::imap_date(options.since())
    );
    let first = view::uid_search(conn, &criteria, view)
        .await?
        .into_iter()
        .find(|&uid| uid >= cap && uid < top);
    Ok(window.floor(uids, first, top))
}

/// Runs or continues the first sync of `state`'s folder, which `view` has examined.
pub(crate) async fn run(
    conn: &mut Connection,
    store: &Store,
    state: &FolderSync,
    view: &mut View,
    options: &SyncOptions,
    progress: &mut impl FnMut(Progress),
) -> Result<FirstSyncReport, SyncError> {
    let started = Instant::now();
    let folder = state.folder;
    let mut report = FirstSyncReport::default();
    let (top, mut floor) = match state.state {
        SyncState::Ready => {
            report.complete = state.complete();
            return Ok(report);
        }
        SyncState::Initial { top, floor, .. } => {
            report.resumed = true;
            (top, floor)
        }
        SyncState::New => {
            let top = match view.uidnext {
                Some(next) => next,
                None => view::uid_search(conn, "ALL", view)
                    .await?
                    .last()
                    .map_or(1, |uid| uid + 1),
            };
            let start = FirstSyncStep::Start {
                uidvalidity: view.uidvalidity,
                top,
                modseq: view.highestmodseq.unwrap_or(0),
            };
            store.first_sync_step(folder, start).recv().await?;
            (top, top)
        }
    };
    let mut all = if top > 1 {
        view::uid_search(conn, &format!("UID 1:{}", top - 1), view).await?
    } else {
        Vec::new()
    };
    all.retain(|&uid| uid < top);
    let target = window_floor(conn, options, &all, top, view).await?;
    let mut uids: Vec<u32> = all
        .iter()
        .copied()
        .filter(|&uid| uid >= target && uid < floor)
        .collect();
    uids.reverse();
    let mut stored = state.messages;
    report.total = stored + uids.len() as u32;
    progress(Progress {
        fetched: stored,
        total: report.total,
    });

    let mut pending: Option<Pending> = None;
    for batch in uids.chunks(BATCH) {
        let previous = async {
            match pending.take() {
                Some(pending) => commit(store, folder, pending).await.map(Some),
                None => Ok(None),
            }
        };
        let wanted: HashSet<u32> = batch.iter().copied().collect();
        let set = raw::uid_set(batch);
        let fetch = view::fetch_headers(conn, folder, &set, |uid| wanted.contains(&uid), view);
        let (fetched, committed) = tokio::join!(fetch, previous);
        if let Some(count) = committed? {
            stored += count;
            report.fetched += count;
            progress(Progress {
                fetched: stored,
                total: report.total,
            });
        }
        let fetched = fetched?;
        let messages = tokio::task::spawn_blocking(move || {
            fetched
                .into_iter()
                .map(|found| fetch::message(folder, found))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|_| SyncError::Parse)?;
        report.batches += 1;
        let batch_floor = batch.last().copied().unwrap_or(floor);
        floor = floor.min(batch_floor);
        pending = Some(Pending {
            reply: store.ingest(messages),
            floor: batch_floor,
        });
    }
    if let Some(pending) = pending {
        let count = commit(store, folder, pending).await?;
        stored += count;
        report.fetched += count;
        progress(Progress {
            fetched: stored,
            total: report.total,
        });
    }
    let below = all.partition_point(|&uid| uid < floor) as u32;
    store
        .first_sync_step(folder, FirstSyncStep::Finish { below })
        .recv()
        .await?;
    report.complete = below == 0;
    tracing::info!(
        %folder,
        fetched = report.fetched,
        batches = report.batches,
        resumed = report.resumed,
        below,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "first sync done"
    );
    Ok(report)
}
