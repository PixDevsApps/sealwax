//! The writer's account operations (ML-034, ML-038): accounts named by their `accounts.toml`
//! key, kept in step with the file, and the removal of an account with everything it owns.

use super::Writer;
use crate::store::api::{
    ChangeEvent, NewAccount, StoreVersion, Versioned, ViewChange, ViewDiff, ViewId, ViewScope,
    ViewSpec,
};
use crate::store::{RemovedAccount, StoreError, views};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sealwax_domain::account::AccountKey;
use sealwax_domain::model::{AccountId, BlobKey, FolderId, MessageId};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

/// Every open view `affected` picks, reopened at its new size.
fn reloads(
    tx: &Connection,
    open: &BTreeMap<ViewId, ViewSpec>,
    affected: impl Fn(ViewScope) -> bool,
) -> rusqlite::Result<Vec<ViewChange>> {
    let mut changes = Vec::new();
    for (view, spec) in open {
        if affected(spec.scope) {
            let folders = views::folders(tx, spec.scope)?;
            changes.push(ViewChange {
                view: *view,
                diff: ViewDiff::Reload {
                    total: views::count(tx, &folders, spec.filter)?,
                },
            });
        }
    }
    Ok(changes)
}

impl Writer {
    /// Inserts each keyed account, or updates its name and address and attaches it again; with
    /// `detach_others`, every other keyed account is detached. Commits only when something
    /// changed; the unified inbox reopens, since its folders may have changed.
    pub(super) fn reconcile_accounts(
        &mut self,
        accounts: &[(AccountKey, NewAccount)],
        detach_others: bool,
    ) -> Result<Versioned<Vec<AccountId>>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut ids = Vec::with_capacity(accounts.len());
        let mut changed = false;
        {
            let mut select = tx.prepare_cached(
                "SELECT id, name, address, detached FROM accounts WHERE key = ?1",
            )?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO accounts(key, name, address) VALUES (?1, ?2, ?3) RETURNING id",
            )?;
            let mut update = tx.prepare_cached(
                "UPDATE accounts SET name = ?2, address = ?3, detached = 0 WHERE id = ?1",
            )?;
            for (key, account) in accounts {
                let found = select
                    .query_row([&key.0], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    })
                    .optional()?;
                let id = match found {
                    Some((id, name, address, detached)) => {
                        if name != account.name || address != account.address || detached {
                            update.execute(params![id, account.name, account.address])?;
                            changed = true;
                        }
                        id
                    }
                    None => {
                        changed = true;
                        insert.query_row(params![key.0, account.name, account.address], |row| {
                            row.get(0)
                        })?
                    }
                };
                ids.push(AccountId(id));
            }
        }
        if detach_others {
            let listed: HashSet<&str> = accounts.iter().map(|(key, _)| key.0.as_str()).collect();
            let attached: Vec<(i64, String)> = tx
                .prepare_cached(
                    "SELECT id, key FROM accounts WHERE key IS NOT NULL AND detached = 0",
                )?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut detach = tx.prepare_cached("UPDATE accounts SET detached = 1 WHERE id = ?1")?;
            for (id, key) in attached {
                if !listed.contains(key.as_str()) {
                    detach.execute([id])?;
                    changed = true;
                }
            }
        }
        if !changed {
            return Ok(Versioned {
                version: StoreVersion(self.state.version),
                value: ids,
            });
        }
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let views = reloads(&tx, &open.open, |scope| scope == ViewScope::UnifiedInbox)?;
        let version = self.state.commit(
            tx,
            ChangeEvent {
                folders_changed: true,
                views,
                ..ChangeEvent::default()
            },
        )?;
        drop(open);
        Ok(Versioned {
            version,
            value: ids,
        })
    }

    /// Deletes an account with its folders, messages, parts, conversations, aliases and
    /// full-text rows in one transaction, and lists the raw messages no other account refers
    /// to. An account that doesn't exist is no error: removal can be resumed.
    pub(super) fn remove_account(
        &mut self,
        account: AccountId,
    ) -> Result<RemovedAccount, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id = ?1)",
            [account.0],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(RemovedAccount {
                version: StoreVersion(self.state.version),
                ..RemovedAccount::default()
            });
        }
        let folders: Vec<FolderId> = tx
            .prepare_cached("SELECT id FROM folders WHERE account_id = ?1")?
            .query_map([account.0], |row| row.get(0).map(FolderId))?
            .collect::<rusqlite::Result<_>>()?;
        let had_inbox: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE account_id = ?1 AND role = 'inbox')",
            [account.0],
            |row| row.get(0),
        )?;
        let mut removed = Vec::new();
        {
            let mut select = tx.prepare_cached("SELECT id FROM messages WHERE folder_id = ?1")?;
            for folder in &folders {
                for id in select.query_map([folder.0], |row| row.get(0).map(MessageId))? {
                    removed.push(id?);
                }
            }
        }
        let orphaned_blobs: Vec<BlobKey> = tx
            .prepare_cached(
                "SELECT DISTINCT m.blob_key FROM messages m \
                 WHERE m.folder_id IN (SELECT id FROM folders WHERE account_id = ?1) \
                   AND m.blob_key IS NOT NULL \
                   AND NOT EXISTS (SELECT 1 FROM messages o INDEXED BY messages_blob \
                     WHERE o.blob_key = m.blob_key \
                       AND o.folder_id NOT IN (SELECT id FROM folders WHERE account_id = ?1))",
            )?
            .query_map([account.0], |row| row.get::<_, Vec<u8>>(0))?
            .filter_map(|key| key.map(|key| BlobKey::from_slice(&key)).transpose())
            .collect::<rusqlite::Result<_>>()?;
        // In rowid order: FTS5's tombstones then stay on the same pages. In date order, 100k
        // deletes took 17.2 s here, sorted 0.23 s.
        removed.sort_unstable();
        {
            let mut unindex = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
            for id in &removed {
                unindex.execute([id.0])?;
            }
        }
        // Folders (with their messages and parts), conversations and aliases follow by cascade.
        tx.execute("DELETE FROM accounts WHERE id = ?1", [account.0])?;
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let views = reloads(&tx, &open.open, |scope| match scope {
            ViewScope::Folder(folder) => folders.contains(&folder),
            ViewScope::UnifiedInbox => had_inbox,
        })?;
        let messages = u32::try_from(removed.len()).unwrap_or(u32::MAX);
        let version = self.state.commit(
            tx,
            ChangeEvent {
                removed,
                folders_changed: true,
                views,
                ..ChangeEvent::default()
            },
        )?;
        drop(open);
        self.state.accounts.retain(|_, owner| *owner != account);
        Ok(RemovedAccount {
            version,
            found: true,
            folders: u32::try_from(folders.len()).unwrap_or(u32::MAX),
            messages,
            orphaned_blobs,
        })
    }
}
