//! A folder's first header sync (ML-037), resumable as S1 built it: EXAMINE, then newest-first
//! batches of UIDs from the top down, each FETCHed, ingested in one store transaction and then
//! recorded as the new floor, so an interrupted sync continues below the last committed batch.
//! The next batch is fetched while the store commits the previous one. Re-sync, IDLE and the
//! first-sync window are M3's.

use super::connection::Connection;
use super::error::ImapError;
use super::error::SyncError;
use super::fetch::{self, Fetched};
use super::raw::{self, quote};
use async_imap::imap_proto::{MailboxDatum, Response, ResponseCode, Status};
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
    /// Messages the folder holds, as far as this run knows.
    pub total: u32,
    pub batches: u32,
    /// The run continued an interrupted first sync.
    pub resumed: bool,
    /// The folder's UIDVALIDITY changed, so its old messages were removed first.
    pub reset: bool,
    /// Raw messages the reset left without a row, for the caller to delete from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// What EXAMINE reported.
#[derive(Clone, Copy, Debug, Default)]
struct View {
    uidvalidity: Option<u32>,
    uidnext: Option<u32>,
    highestmodseq: Option<u64>,
}

async fn examine(conn: &mut Connection, name_raw: &str) -> Result<View, ImapError> {
    let mut command = format!("EXAMINE {}", quote(name_raw)?);
    if conn.caps().has("CONDSTORE") && !conn.qresync() {
        command.push_str(" (CONDSTORE)");
    }
    conn.selected = None;
    let mut view = View::default();
    raw::run(&mut conn.session, &command, |response| {
        if let Response::Data {
            status: Status::Ok,
            outcome,
        } = response
        {
            match outcome.code {
                Some(ResponseCode::UidValidity(value)) => view.uidvalidity = Some(value),
                Some(ResponseCode::UidNext(value)) => view.uidnext = Some(value),
                Some(ResponseCode::HighestModSeq(value)) => view.highestmodseq = Some(value),
                _ => {}
            }
        }
    })
    .await?;
    conn.selected = Some(name_raw.to_owned());
    Ok(view)
}

async fn uid_search(conn: &mut Connection, criteria: &str) -> Result<Vec<u32>, ImapError> {
    let mut uids = Vec::new();
    raw::run(
        &mut conn.session,
        &format!("UID SEARCH {criteria}"),
        |response| {
            if let Response::MailboxData(MailboxDatum::Search(found)) = response {
                uids.extend_from_slice(found);
            }
        },
    )
    .await?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

/// The headers of `uids`. FETCH responses for other UIDs, or without the header section, are
/// unsolicited hints and are dropped.
async fn fetch_batch(conn: &mut Connection, uids: &[u32]) -> Result<Vec<Fetched>, ImapError> {
    let wanted: HashSet<u32> = uids.iter().copied().collect();
    let mut out = Vec::with_capacity(uids.len());
    let command = format!("UID FETCH {} {}", raw::uid_set(uids), fetch::items());
    raw::run(&mut conn.session, &command, |response| {
        if let Response::Fetch(_, attributes) = response
            && let Some(found) = fetch::fetched(attributes)
            && wanted.contains(&found.uid)
        {
            out.push(found);
        }
    })
    .await?;
    Ok(out)
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

async fn load_state(store: &Store, folder: FolderId) -> Result<FolderSync, StoreError> {
    store
        .folder_sync(folder)
        .recv()
        .await?
        .ok_or(StoreError::UnknownFolder(folder))
}

/// Syncs the headers of `folder` (M2 runs it for INBOX) into the store, newest first, calling
/// `progress` after every committed batch. A folder whose first sync is done is left to M3's
/// re-sync; an interrupted one continues where it stopped.
pub async fn first_sync(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    mut progress: impl FnMut(Progress),
) -> Result<FirstSyncReport, SyncError> {
    let started = Instant::now();
    let mut state = load_state(store, folder).await?;
    if !state.selectable {
        return Err(SyncError::NotSelectable);
    }
    let view = examine(conn, &state.name_raw).await?;
    let uidvalidity = view
        .uidvalidity
        .ok_or(ImapError::Protocol("EXAMINE without UIDVALIDITY"))?;
    let mut report = FirstSyncReport::default();
    if state.uidvalidity.is_some_and(|known| known != uidvalidity) {
        let removed = store.reset_folder(folder, uidvalidity).recv().await?;
        report.reset = true;
        report.orphaned_blobs = removed.orphaned_blobs;
        state = load_state(store, folder).await?;
    }
    let floor = match state.state {
        SyncState::Ready => return Ok(report),
        SyncState::Initial { floor, .. } => {
            report.resumed = true;
            floor
        }
        SyncState::New => {
            let top = match view.uidnext {
                Some(next) => next,
                None => uid_search(conn, "ALL")
                    .await?
                    .last()
                    .map_or(1, |uid| uid + 1),
            };
            let start = FirstSyncStep::Start {
                uidvalidity,
                top,
                modseq: view.highestmodseq.unwrap_or(0),
            };
            store.first_sync_step(folder, start).recv().await?;
            top
        }
    };
    let mut uids = if floor > 1 {
        uid_search(conn, &format!("UID 1:{}", floor - 1)).await?
    } else {
        Vec::new()
    };
    uids.retain(|&uid| uid < floor);
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
        let (fetched, committed) = tokio::join!(fetch_batch(conn, batch), previous);
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
        pending = Some(Pending {
            reply: store.ingest(messages),
            floor: batch.last().copied().unwrap_or(floor),
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
    store
        .first_sync_step(folder, FirstSyncStep::Finish)
        .recv()
        .await?;
    tracing::info!(
        %folder,
        fetched = report.fetched,
        batches = report.batches,
        resumed = report.resumed,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "first sync done"
    );
    Ok(report)
}
