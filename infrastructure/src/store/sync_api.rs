//! The sync engine's requests (ML-041, ML-043): a folder's changes by UID in one transaction,
//! the folder list made to match the server's, and the reads the passes compare with.

use super::reader::{ReadJob, SyncRead};
use super::writer::WriteJob;
use super::writer::sync::SyncJob;
use super::{FolderSpec, FolderSync, NewMessage, Reply, Store, StoreVersion};
use sealwax_domain::model::{AccountId, BlobKey, Flags, FolderId, MessageId};

/// A message's flags and keywords as the server reports them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagUpdate {
    pub uid: u32,
    pub flags: Flags,
    /// Sorted and deduplicated, as `Flags::from_imap` gives them.
    pub keywords: Vec<String>,
}

/// A stored message as the sync engine compares it with the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: MessageId,
    pub uid: u32,
    pub flags: Flags,
    pub keywords: Vec<String>,
}

/// What one step of a pass changes in a folder whose first sync is done, by UID. The writer
/// applies it in one transaction with one change event: what the server says, nothing else
/// (overlays are the op queue's, M4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderChanges {
    /// New messages; a UID the folder holds already is skipped.
    pub new: Vec<NewMessage>,
    /// Flags and keywords to replace; UIDs the folder doesn't hold are ignored.
    pub flags: Vec<FlagUpdate>,
    /// UIDs gone from the server (EXPUNGE, VANISHED or a UID diff).
    pub removed: Vec<u32>,
    /// Every UID below it is stored; it only ever rises.
    pub uidnext: Option<u32>,
    pub highestmodseq: Option<u64>,
    /// The new floor: lower after older mail loaded, 1 once none is left.
    pub floor: Option<u32>,
    /// Messages the server holds below the floor, as just counted.
    pub below: Option<u32>,
}

/// The writer's reply to [`Store::sync_changes`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderSynced {
    /// The commit's version, or the current one when no row changed.
    pub version: StoreVersion,
    pub inserted: u32,
    /// Rows whose flags or keywords changed.
    pub changed: u32,
    pub removed: u32,
    /// Raw messages no remaining row refers to; the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// The writer's reply to [`Store::replace_folders`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldersReplaced {
    pub version: StoreVersion,
    /// The listed folders' ids, in input order.
    pub ids: Vec<FolderId>,
    /// Folders the server no longer lists, removed with their messages.
    pub removed_folders: u32,
    pub removed_messages: u32,
    /// Raw messages no remaining row refers to; the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

impl Store {
    /// Applies one step of a pass to a folder in sync state `Ready`; any other state is
    /// [`super::StoreError::SyncState`].
    pub fn sync_changes(&self, folder: FolderId, changes: FolderChanges) -> Reply<FolderSynced> {
        self.write(|reply| {
            WriteJob::Sync(SyncJob::Changes {
                folder,
                changes,
                reply,
            })
        })
    }

    /// Makes `account`'s folders the listed ones in one transaction: each upserted as
    /// `upsert_folders` does, and every other folder of the account removed with its messages.
    /// Every spec must name `account`.
    pub fn replace_folders(
        &self,
        account: AccountId,
        folders: Vec<FolderSpec>,
    ) -> Reply<FoldersReplaced> {
        self.write(|reply| {
            WriteJob::Sync(SyncJob::ReplaceFolders {
                account,
                folders,
                reply,
            })
        })
    }

    /// Every message of a folder with its UID, flags and keywords, in UID order.
    pub fn folder_messages(&self, folder: FolderId) -> Reply<Vec<StoredMessage>> {
        self.read(|reply| ReadJob::Sync(SyncRead::FolderMessages { folder, reply }))
    }

    /// The sync state of every folder of an account, in id order.
    pub fn account_folders(&self, account: AccountId) -> Reply<Vec<FolderSync>> {
        self.read(|reply| ReadJob::Sync(SyncRead::AccountFolders { account, reply }))
    }
}
