//! The writer's folder management jobs (ADR-020, M9): renames in place and deletes, each one
//! transaction with one change event.

use super::sync::drop_folders;
use super::{Writer, folder_counts, thread_counts};
use crate::store::StoreError;
use crate::store::api::{ChangeEvent, StoreVersion};
use crate::store::folders_api::{FolderRename, FoldersDeleted};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{AccountId, FolderId};
use std::collections::BTreeSet;
use std::sync::Arc;

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

pub(crate) enum FolderJob {
    Rename {
        account: AccountId,
        renames: Vec<FolderRename>,
        reply: Sender<StoreVersion>,
    },
    Delete {
        account: AccountId,
        folders: Vec<FolderId>,
        reply: Sender<FoldersDeleted>,
    },
}

/// Fails unless every folder is `account`'s.
fn owned(
    tx: &rusqlite::Connection,
    account: AccountId,
    folders: impl IntoIterator<Item = FolderId>,
) -> Result<(), StoreError> {
    let mut select = tx.prepare_cached("SELECT account_id FROM folders WHERE id = ?1")?;
    for folder in folders {
        let owner: Option<i64> = select.query_row([folder.0], |row| row.get(0)).optional()?;
        if owner != Some(account.0) {
            return Err(StoreError::UnknownFolder(folder));
        }
    }
    Ok(())
}

impl Writer {
    pub(super) fn folder_job(&mut self, job: FolderJob) {
        match job {
            FolderJob::Rename {
                account,
                renames,
                reply,
            } => {
                let _ = reply.try_send(self.rename_folders(account, &renames));
            }
            FolderJob::Delete {
                account,
                folders,
                reply,
            } => {
                let _ = reply.try_send(self.delete_folders(account, &folders));
            }
        }
    }

    fn rename_folders(
        &mut self,
        account: AccountId,
        renames: &[FolderRename],
    ) -> Result<StoreVersion, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        owned(&tx, account, renames.iter().map(|rename| rename.folder))?;
        {
            // Through names no server sends (they start with a control character), so a rename
            // whose new names are others' old ones never meets the account's unique names.
            let mut aside = tx.prepare_cached("UPDATE folders SET name_raw = ?2 WHERE id = ?1")?;
            for rename in renames {
                aside.execute(params![
                    rename.folder.0,
                    format!("\u{1}{}", rename.folder.0)
                ])?;
            }
            let mut update =
                tx.prepare_cached("UPDATE folders SET name = ?2, name_raw = ?3 WHERE id = ?1")?;
            for rename in renames {
                update.execute(params![rename.folder.0, rename.name, rename.name_raw])?;
            }
        }
        let event = ChangeEvent {
            folders_changed: true,
            ..ChangeEvent::default()
        };
        self.state.commit(tx, event)
    }

    fn delete_folders(
        &mut self,
        account: AccountId,
        folders: &[FolderId],
    ) -> Result<FoldersDeleted, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        owned(&tx, account, folders.iter().copied())?;
        let gone: Vec<FolderId> = folders
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let mut counted = BTreeSet::new();
        let dropped = drop_folders(&tx, &open.open, &gone, &mut counted)?;
        let event = ChangeEvent {
            removed: dropped.removed.clone(),
            changed: dropped.changed,
            folders: folder_counts(&tx, &counted)?,
            folders_changed: true,
            threads: thread_counts(&tx, &dropped.threads)?,
            views: dropped.views,
            ..ChangeEvent::default()
        };
        let version = self.state.commit(tx, event)?;
        drop(open);
        for folder in &gone {
            self.state.accounts.remove(folder);
        }
        Ok(FoldersDeleted {
            version,
            removed_folders: u32::try_from(gone.len()).unwrap_or(u32::MAX),
            removed_messages: u32::try_from(dropped.removed.len()).unwrap_or(u32::MAX),
            orphaned_blobs: dropped.orphaned_blobs,
        })
    }
}
