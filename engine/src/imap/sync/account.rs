//! The account pass: S1's `sync_account` on M2's folder list. LIST with SPECIAL-USE and STATUS
//! in one round trip, pipelined behind an UNSELECT so no STATUS comes from the connection's own
//! selected mailbox (Dovecot answers that from a stale view, S1); the store's folder list made
//! the server's, so folders gone from it leave with their messages; then a folder pass for
//! every folder the policy syncs whose STATUS differs from the store, and for a folder that
//! keeps only its newest mail, the server's counts below its floor from that STATUS (ML-043).

use super::folder::sync_folder;
use super::{FolderLocks, SyncOptions, SyncReport};
use crate::imap::connection::Connection;
use crate::imap::error::{ImapError, SyncError};
use crate::imap::first_sync::Progress;
use crate::imap::folders::{self, FolderStatus, ListedFolder};
use sealwax_domain::model::{AccountId, FolderId};
use sealwax_domain::sync::{Facts, Plan, plan};
use sealwax_infrastructure::store::{FolderSync, Store, SyncState};
use std::collections::HashMap;
use std::time::Instant;

/// One account pass. `progress` follows the batches of first syncs. A folder that vanished
/// between LIST and its pass (the server's NO) is skipped; the next pass removes it.
pub async fn sync_account(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
    options: &SyncOptions,
    locks: &FolderLocks,
    mut progress: impl FnMut(FolderId, Progress),
) -> Result<SyncReport, SyncError> {
    let started = Instant::now();
    let listed = folders::list(conn).await?;
    if listed.is_empty() {
        return Err(ImapError::Protocol("an empty folder list").into());
    }
    let specs = listed
        .iter()
        .map(|folder| folders::spec(account, folder))
        .collect();
    let replaced = store.replace_folders(account, specs).recv().await?;
    let mut report = SyncReport::default();
    report.stats.folders_removed = replaced.removed_folders;
    report.orphaned_blobs = replaced.orphaned_blobs;
    let states: HashMap<FolderId, FolderSync> = store
        .account_folders(account)
        .recv()
        .await?
        .into_iter()
        .map(|state| (state.folder, state))
        .collect();
    for (id, folder) in replaced.ids.iter().copied().zip(&listed) {
        let Some(state) = states.get(&id) else {
            continue;
        };
        let mut passed = false;
        if wanted(id, folder, state, options) {
            if folder.status.is_some_and(|status| {
                unchanged(state, &status, options.unreadable.skipped(id).len())
            }) {
                report.stats.status_skips += 1;
            } else {
                match sync_folder(conn, store, id, options, locks, |p| progress(id, p)).await {
                    Ok(done) => {
                        report.add(done);
                        passed = true;
                    }
                    Err(SyncError::Imap(ImapError::No(_))) => report.stats.skipped_missing += 1,
                    Err(error) => return Err(error),
                }
            }
        }
        // Whether or not it synced, a windowed folder's counts follow the server's.
        let windowed = state.state == SyncState::Ready && !state.complete();
        if let Some(status) = folder.status.and_then(|status| status.server())
            && (passed || windowed)
            && store.count_below(id, status).recv().await?.value
        {
            report.stats.below_counts += 1;
        }
    }
    tracing::debug!(
        %account,
        folders = listed.len(),
        passes = report.stats.passes,
        status_skips = report.stats.status_skips,
        below_counts = report.stats.below_counts,
        removed_folders = report.stats.folders_removed,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "account pass done"
    );
    Ok(report)
}

/// The policy syncs the folder here, not on another connection (unless STATUS is the backstop).
fn wanted(id: FolderId, folder: &ListedFolder, state: &FolderSync, options: &SyncOptions) -> bool {
    let facts = Facts {
        role: folder.role,
        attributes: &folder.attributes,
        selectable: folder.selectable,
        synced_before: state.uidvalidity.is_some(),
    };
    plan(facts) == Plan::Sync
        && options
            .elsewhere
            .filter(|elsewhere| elsewhere.folder == id)
            .is_none_or(|elsewhere| elsewhere.backstop && folder.status.is_some())
}

/// STATUS shows nothing the store doesn't hold, besides the `skipped` messages left out as
/// unreadable. Without HIGHESTMODSEQ a flag change can't show, so such a folder always gets its
/// pass.
fn unchanged(state: &FolderSync, status: &FolderStatus, skipped: usize) -> bool {
    state.state == SyncState::Ready
        && status.uidvalidity.is_some()
        && status.uidvalidity == state.uidvalidity
        && status.uidnext == Some(state.uidnext)
        && status.highestmodseq == Some(state.highestmodseq)
        && status.messages
            == Some(
                state
                    .messages
                    .saturating_add(state.below)
                    .saturating_add(super::count(skipped)),
            )
}
