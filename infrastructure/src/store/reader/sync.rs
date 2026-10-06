//! The reads the sync engine compares the server with (ML-041).

use super::{FOLDER_SYNC, Reader, Sender, folder_sync_row};
use crate::store::api::FolderSync;
use crate::store::sync_api::StoredMessage;
use crate::store::{StoreError, codec};
use sealwax_domain::model::{AccountId, Flags, FolderId, MessageId};

pub(crate) enum SyncRead {
    FolderMessages {
        folder: FolderId,
        reply: Sender<Vec<StoredMessage>>,
    },
    AccountFolders {
        account: AccountId,
        reply: Sender<Vec<FolderSync>>,
    },
}

impl SyncRead {
    pub(super) fn cancelled(&self) -> bool {
        match self {
            SyncRead::FolderMessages { reply, .. } => reply.is_closed(),
            SyncRead::AccountFolders { reply, .. } => reply.is_closed(),
        }
    }
}

impl Reader {
    pub(super) fn sync_read(&self, read: SyncRead) {
        match read {
            SyncRead::FolderMessages { folder, reply } => {
                let _ = reply.try_send(self.folder_messages(folder));
            }
            SyncRead::AccountFolders { account, reply } => {
                let _ = reply.try_send(self.account_folders(account));
            }
        }
    }

    /// The server's flags, whatever pending ops show.
    fn folder_messages(&self, folder: FolderId) -> Result<Vec<StoredMessage>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT id, uid, coalesce(server_flags, flags), keywords FROM messages \
                 WHERE folder_id = ?1 ORDER BY uid",
            )?
            .query_map([folder.0], |row| {
                Ok(StoredMessage {
                    id: MessageId(row.get(0)?),
                    uid: row.get(1)?,
                    flags: Flags::from_bits(row.get(2)?),
                    keywords: codec::decode_words(&row.get::<_, String>(3)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn account_folders(&self, account: AccountId) -> Result<Vec<FolderSync>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT {FOLDER_SYNC} FROM folders f WHERE account_id = ?1 ORDER BY id"
            ))?
            .query_map([account.0], folder_sync_row)?
            .collect::<rusqlite::Result<_>>()?)
    }
}
