//! State-driven re-sync (ML-041) and the first-sync window (ML-043), ported from S1's sync model
//! onto M2's connection, first sync and store. Every pass starts from the server's state
//! (EXAMINE, or the account's STATUS) and the stored sync state; server notices only decide
//! when a pass runs, never what it concludes (ADR-006).
//!
//! - The folder pass ([`sync_folder`]): EXAMINE; a UIDVALIDITY change resets the folder; an
//!   unfinished first sync continues; then new UIDs, flag changes (CHANGEDSINCE with VANISHED,
//!   CHANGEDSINCE, or a full flag scan), and a count check against EXISTS with a UID diff as
//!   the fallback. Every step commits on its own, so a cut pass loses nothing.
//! - The account pass ([`sync_account`]): LIST with STATUS in one round trip, folders gone from
//!   the server removed, then a folder pass for each folder the policy syncs whose STATUS
//!   changed; a windowed folder's counts below its floor follow its STATUS.
//! - Older mail ([`load_older`]): the next batch below a folder's floor.
//!
//! Passes over one folder are serialized by [`FolderLocks`], so two connections of an account
//! (the IDLE connection and the polling one, phase B) can share its folders.
//!
//! New mail ([`Arrived`], ML-083): the rows a folder pass stores at or above the UIDNEXT the
//! store held when it began, step by step as they commit, into the report and to
//! [`SyncOptions::arrivals`]. A pass that runs a first sync (a UIDVALIDITY reset's included)
//! reports none, and neither does older mail; a row the store holds already (one the op runner
//! re-homed after a move) is never inserted again.
//!
//! A header FETCH whose reply async-imap can't read (issue #20) breaks the connection; the
//! account's [`Unreadable`] remembers the folder, so the passes after it ask for less until the
//! one message that can't be read at all is left out.

mod account;
mod folder;
mod older;
pub(crate) mod view;

pub use account::sync_account;
pub use folder::sync_folder;
pub use older::{Older, load_older};

use super::error::SyncError;
use sealwax_domain::model::{BlobKey, FolderId, MessageId};
use sealwax_domain::sync::{OLDER_BATCH, Window};
use sealwax_infrastructure::store::{FolderSync, Store, StoreError};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// How passes run.
#[derive(Clone, Debug)]
pub struct SyncOptions {
    /// How far back a first sync reaches.
    pub window: Window,
    /// Older messages per [`load_older`].
    pub older: u32,
    /// A folder another connection keeps current (INBOX on the IDLE connection).
    pub elsewhere: Option<Elsewhere>,
    /// The time the window and the body prefetch count back from (Unix seconds); the clock
    /// when `None`.
    pub now: Option<i64>,
    /// Where each step's new mail goes the moment it is committed, besides the report.
    pub arrivals: Option<async_channel::Sender<Arrived>>,
    /// The account's header FETCHes that couldn't be read, kept across its reconnects.
    pub unreadable: Unreadable,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            window: Window::DEFAULT,
            older: OLDER_BATCH,
            elsewhere: None,
            now: None,
            arrivals: None,
            unreadable: Unreadable::default(),
        }
    }
}

/// Mail that arrived in a folder after its first sync: rows one step stored above the UIDNEXT
/// the store held when the pass began, in the step's order (ML-083).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrived {
    pub folder: FolderId,
    pub messages: Vec<MessageId>,
}

impl SyncOptions {
    /// Where the first-sync window starts.
    pub(crate) fn since(&self) -> i64 {
        self.window.since(self.clock())
    }

    /// `now`, or the clock.
    pub(crate) fn clock(&self) -> i64 {
        self.now.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs() as i64)
        })
    }
}

/// A folder the account pass leaves to another connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elsewhere {
    pub folder: FolderId,
    /// Still compare its STATUS in the account pass and sync it when STATUS shows a change: the
    /// backstop for notices a server never sends (S1: Stalwart misses changes made just before
    /// IDLE).
    pub backstop: bool,
}

/// What passes did: counters only, no names, subjects or addresses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Folder passes (one EXAMINE each).
    pub passes: u32,
    /// Passes that found nothing changed in one round trip.
    pub fast_path: u32,
    /// Folders the account pass skipped because STATUS matched the store.
    pub status_skips: u32,
    /// Folders whose UIDVALIDITY changed.
    pub resets: u32,
    pub first_sync_batches: u32,
    /// Messages stored: first sync, new mail, UID diffs and older mail.
    pub stored: u32,
    pub new_mail_fetches: u32,
    pub changedsince_fetches: u32,
    pub full_flag_scans: u32,
    /// Stored messages whose flags or keywords changed.
    pub flag_changes: u32,
    /// Stored messages a VANISHED named.
    pub vanished: u32,
    /// Stored messages removed (VANISHED, flag scans and UID diffs).
    pub removed: u32,
    pub count_mismatches: u32,
    pub uid_diffs: u32,
    /// Folders gone from the server, removed from the store.
    pub folders_removed: u32,
    /// Folders that vanished between LIST and EXAMINE.
    pub skipped_missing: u32,
    pub older_loads: u32,
    /// Windowed folders whose server counts below the floor changed with STATUS.
    pub below_counts: u32,
}

impl Stats {
    pub fn add(&mut self, other: &Stats) {
        self.passes += other.passes;
        self.fast_path += other.fast_path;
        self.status_skips += other.status_skips;
        self.resets += other.resets;
        self.first_sync_batches += other.first_sync_batches;
        self.stored += other.stored;
        self.new_mail_fetches += other.new_mail_fetches;
        self.changedsince_fetches += other.changedsince_fetches;
        self.full_flag_scans += other.full_flag_scans;
        self.flag_changes += other.flag_changes;
        self.vanished += other.vanished;
        self.removed += other.removed;
        self.count_mismatches += other.count_mismatches;
        self.uid_diffs += other.uid_diffs;
        self.folders_removed += other.folders_removed;
        self.skipped_missing += other.skipped_missing;
        self.older_loads += other.older_loads;
        self.below_counts += other.below_counts;
    }
}

/// What a pass did, and what it leaves for the caller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub stats: Stats,
    /// Raw messages no row refers to any more (folders removed, UIDVALIDITY resets,
    /// expunges); the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
    /// New mail, one entry per committed step that stored some.
    pub arrived: Vec<Arrived>,
}

impl SyncReport {
    pub fn add(&mut self, other: SyncReport) {
        self.stats.add(&other.stats);
        self.orphaned_blobs.extend(other.orphaned_blobs);
        self.arrived.extend(other.arrived);
    }
}

/// One async lock per folder, shared by an account's connections, so passes over a folder never
/// overlap (S1's per-mailbox lock). Clones share the locks.
#[derive(Clone, Debug, Default)]
pub struct FolderLocks(Arc<Mutex<HashMap<FolderId, Arc<tokio::sync::Mutex<()>>>>>);

impl FolderLocks {
    /// Waits until no other pass holds `folder`.
    pub(crate) async fn lock(&self, folder: FolderId) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(folder)
            .or_default()
            .clone();
        lock.lock_owned().await
    }
}

/// How a folder's header FETCHes ask, after replies async-imap couldn't read (issue #20).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Careful {
    /// Everything [`super::fetch::items`] names.
    #[default]
    Full,
    /// The same without BODYSTRUCTURE, the most involved part of the grammar.
    Lean,
    /// Lean, one message per FETCH, so the one that can't be read is found and left out.
    Single,
}

#[derive(Debug, Default)]
struct Folder {
    careful: Careful,
    skipped: BTreeSet<u32>,
}

/// An account's folders whose header FETCH broke the connection with a reply async-imap
/// couldn't parse, kept by the account across reconnects so the next pass asks for less: each
/// failure goes one step down from [`Careful::Full`], a FETCH that gets through goes back to
/// it, and a message that fails on its own is left out until Sealwax starts again. Nothing of
/// the reply is kept. Clones share it.
#[derive(Clone, Debug, Default)]
pub struct Unreadable(Arc<Mutex<HashMap<FolderId, Folder>>>);

impl Unreadable {
    fn with<R>(&self, folder: FolderId, f: impl FnOnce(&mut Folder) -> R) -> R {
        let mut folders = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = folders.entry(folder).or_default();
        let out = f(entry);
        if entry.careful == Careful::Full && entry.skipped.is_empty() {
            folders.remove(&folder);
        }
        out
    }

    pub(crate) fn careful(&self, folder: FolderId) -> Careful {
        self.with(folder, |entry| entry.careful)
    }

    /// The messages left out of `folder`, ascending.
    pub fn skipped(&self, folder: FolderId) -> Vec<u32> {
        self.with(folder, |entry| entry.skipped.iter().copied().collect())
    }

    /// A FETCH in `folder` couldn't be read: the next one asks for less.
    pub(crate) fn failed(&self, folder: FolderId) -> Careful {
        self.with(folder, |entry| {
            entry.careful = match entry.careful {
                Careful::Full => Careful::Lean,
                Careful::Lean | Careful::Single => Careful::Single,
            };
            entry.careful
        })
    }

    /// `uid` couldn't be read on its own: it is left out.
    pub(crate) fn skip(&self, folder: FolderId, uid: u32) {
        self.with(folder, |entry| entry.skipped.insert(uid));
    }

    /// A careful FETCH in `folder` got through: the next one asks for everything again.
    pub(crate) fn passed(&self, folder: FolderId) {
        self.with(folder, |entry| entry.careful = Careful::Full);
    }

    /// Forgets the left-out messages of `folder` the server no longer holds.
    pub(crate) fn retain(&self, folder: FolderId, on_server: impl Fn(u32) -> bool) {
        self.with(folder, |entry| entry.skipped.retain(|&uid| on_server(uid)));
    }
}

pub(crate) async fn load_state(store: &Store, folder: FolderId) -> Result<FolderSync, SyncError> {
    Ok(store
        .folder_sync(folder)
        .recv()
        .await?
        .ok_or(StoreError::UnknownFolder(folder))?)
}

pub(crate) fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
