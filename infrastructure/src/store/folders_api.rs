//! Folder management (ADR-020, M9): folders renamed in place, and folders deleted with their
//! messages. The engine asks for these once the server has made the change, or at once for a
//! POP3 account's local folders; a new folder is an [`Store::upsert_folder`].

use super::writer::WriteJob;
use super::writer::folders::FolderJob;
use super::{Reply, Store, StoreVersion};
use sealwax_domain::model::{AccountId, BlobKey, FolderId};
use std::fmt;

/// A folder's names after a rename.
#[derive(Clone, PartialEq, Eq)]
pub struct FolderRename {
    pub folder: FolderId,
    /// The display name, the whole path.
    pub name: String,
    /// The name on the wire.
    pub name_raw: String,
}

/// Folder names are the user's: `Debug` shows only the folder's id.
impl fmt::Debug for FolderRename {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FolderRename")
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

/// The writer's reply to [`Store::delete_folders`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldersDeleted {
    pub version: StoreVersion,
    pub removed_folders: u32,
    pub removed_messages: u32,
    /// Raw messages no remaining row refers to; the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

impl Store {
    /// Gives folders of `account` new names in one transaction. Each keeps its id, its messages
    /// and its sync state, UIDVALIDITY included, so an open folder stays open; the next pass's
    /// UIDVALIDITY check still guards it. A folder of another account, or a name another folder
    /// of the account holds, refuses the whole rename.
    pub fn rename_folders(
        &self,
        account: AccountId,
        renames: Vec<FolderRename>,
    ) -> Reply<StoreVersion> {
        self.write(|reply| {
            WriteJob::Folders(FolderJob::Rename {
                account,
                renames,
                reply,
            })
        })
    }

    /// Deletes folders of `account` with their messages in one transaction, as when folders
    /// leave the server: their pending ops are refused, and messages whose move into one of
    /// them was waiting show again where they are. A folder of another account refuses the
    /// whole delete.
    pub fn delete_folders(
        &self,
        account: AccountId,
        folders: Vec<FolderId>,
    ) -> Reply<FoldersDeleted> {
        self.write(|reply| {
            WriteJob::Folders(FolderJob::Delete {
                account,
                folders,
                reply,
            })
        })
    }
}
