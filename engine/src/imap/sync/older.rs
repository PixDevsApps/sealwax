//! Older mail (ML-043): the next messages below a folder's floor, found with a UID SEARCH under
//! the floor that widens until it has enough, fetched in one FETCH and stored with the lower
//! floor in one transaction.

use super::folder::pass;
use super::view;
use super::{FolderLocks, SyncOptions, SyncReport, count, load_state};
use crate::imap::connection::Connection;
use crate::imap::error::SyncError;
use crate::imap::raw;
use sealwax_domain::model::FolderId;
use sealwax_infrastructure::store::{FolderChanges, Store, SyncState};
use std::collections::HashSet;

/// What [`load_older`] did (`Event::OlderLoaded`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Older {
    /// Older messages stored.
    pub fetched: u32,
    /// Nothing older is left on the server.
    pub complete: bool,
    pub report: SyncReport,
}

/// Fetches the newest `options.older` messages below the folder's floor and lowers the floor,
/// holding the folder's lock. A folder whose first sync isn't done, or whose UIDVALIDITY
/// changed, gets a pass instead, which reports what its first sync stored.
pub async fn load_older(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    options: &SyncOptions,
    locks: &FolderLocks,
) -> Result<Older, SyncError> {
    let _held = locks.lock(folder).await;
    let mut report = SyncReport::default();
    let state = load_state(store, folder).await?;
    if !state.selectable {
        return Err(SyncError::NotSelectable);
    }
    if state.state != SyncState::Ready {
        pass(conn, store, folder, options, &mut |_| {}, &mut report).await?;
        let complete = load_state(store, folder).await?.complete();
        return Ok(Older {
            fetched: report.stats.stored,
            complete,
            report,
        });
    }
    let floor = state.floor.unwrap_or(1);
    if floor <= 1 {
        return Ok(Older {
            fetched: 0,
            complete: true,
            report,
        });
    }
    let mut view = view::examine(conn, &state.name_raw).await?;
    view.unreadable = options.unreadable.clone();
    if state.uidvalidity != Some(view.uidvalidity) {
        pass(conn, store, folder, options, &mut |_| {}, &mut report).await?;
        let complete = load_state(store, folder).await?.complete();
        return Ok(Older {
            fetched: 0,
            complete,
            report,
        });
    }
    report.stats.older_loads += 1;
    let want = options.older.max(1) as usize;
    let mut span = options.older.max(1).saturating_mul(4).max(1_000);
    let (found, lowest) = loop {
        let lowest = floor.saturating_sub(span).max(1);
        let mut found =
            view::uid_search(conn, &format!("UID {lowest}:{}", floor - 1), &mut view).await?;
        found.retain(|&uid| uid >= lowest && uid < floor);
        if found.len() >= want || lowest == 1 {
            break (found, lowest);
        }
        span = span.saturating_mul(8);
    };
    let take = &found[found.len().saturating_sub(want)..];
    let complete = lowest == 1 && found.len() <= want;
    let mut changes = FolderChanges {
        floor: Some(if complete {
            1
        } else {
            take.first().copied().unwrap_or(floor)
        }),
        below: Some(if complete {
            0
        } else if lowest == 1 {
            count(found.len() - take.len())
        } else {
            state.below.saturating_sub(count(take.len()))
        }),
        ..FolderChanges::default()
    };
    if !take.is_empty() {
        let wanted: HashSet<u32> = take.iter().copied().collect();
        let set = raw::uid_set(take);
        changes.new =
            view::fetch_rows(conn, folder, &set, |uid| wanted.contains(&uid), &mut view).await?;
    }
    let synced = store.sync_changes(folder, changes).recv().await?;
    report.stats.stored += synced.inserted;
    report.orphaned_blobs.extend(synced.orphaned_blobs);
    tracing::debug!(%folder, fetched = synced.inserted, complete, "older mail loaded");
    Ok(Older {
        fetched: synced.inserted,
        complete,
        report,
    })
}
