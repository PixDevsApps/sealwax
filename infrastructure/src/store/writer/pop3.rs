//! POP3 accounts' writes (M8: ML-087, ML-088): an account made a POP3 account with its local
//! folders, downloaded messages stored with their UIDLs in one transaction, the end of a pass, a
//! draft's copy filed into the local Drafts folder, and the raw messages ops done at once left
//! for an actor to delete.

use super::compose::drafts_folders;
use super::ops::queue_in;
use super::overlay::row;
use super::{
    RowChange, Writer, folder_counts, free_uid, fts_doc, insert_message, overlay_new,
    resolve_thread, thread_counts, upsert_rows, view_changes,
};
use crate::store::api::{ChangeEvent, FolderSpec, StoreVersion, ThreadMerge, ViewScope};
use crate::store::overlay::{BATCH_COMPOSE, Change};
use crate::store::pop3_api::{
    DraftFiled, POP3_FOLDERS, Pop3Account, Pop3Download, Pop3PassDone, Pop3Stored,
};
use crate::store::{StoreError, compose, views};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{AccountId, BlobKey, DraftId, FolderId, FolderRole, MessageId};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

pub(crate) enum Pop3Job {
    Account {
        account: AccountId,
        reply: Sender<Pop3Account>,
    },
    Store {
        account: AccountId,
        downloads: Vec<Pop3Download>,
        at: i64,
        reply: Sender<Pop3Stored>,
    },
    PassDone {
        account: AccountId,
        done: Pop3PassDone,
        reply: Sender<()>,
    },
    DraftFiled {
        account: AccountId,
        draft: DraftId,
        revision: u64,
        blob: BlobKey,
        reply: Sender<DraftFiled>,
    },
    TakeBlobDeletions {
        reply: Sender<Vec<BlobKey>>,
    },
}

/// Whether `account` is a POP3 account.
pub(crate) fn is_pop3(tx: &Connection, account: AccountId) -> rusqlite::Result<bool> {
    tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM pop3_accounts WHERE account_id = ?1)")?
        .query_row([account.0], |row| row.get(0))
}

impl Writer {
    pub(crate) fn pop3_job(&mut self, job: Pop3Job) {
        match job {
            Pop3Job::Account { account, reply } => {
                let _ = reply.try_send(self.pop3_account(account));
            }
            Pop3Job::Store {
                account,
                downloads,
                at,
                reply,
            } => {
                let _ = reply.try_send(self.pop3_store(account, &downloads, at));
            }
            Pop3Job::PassDone {
                account,
                done,
                reply,
            } => {
                let _ = reply.try_send(self.pop3_pass_done(account, &done));
            }
            Pop3Job::DraftFiled {
                account,
                draft,
                revision,
                blob,
                reply,
            } => {
                let _ = reply.try_send(self.draft_filed(account, draft, revision, blob));
            }
            Pop3Job::TakeBlobDeletions { reply } => {
                let _ = reply.try_send(self.take_blob_deletions());
            }
        }
    }

    /// A draft's copy, stored in the account's Drafts folder as `blob`, is its copy from now
    /// on: older copies (rows there with its Message-ID) go at once. A copy of a draft that was
    /// discarded or queued to send meanwhile goes itself.
    fn draft_filed(
        &mut self,
        account: AccountId,
        draft: DraftId,
        revision: u64,
        blob: BlobKey,
    ) -> Result<DraftFiled, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !is_pop3(&tx, account)? {
            return Err(StoreError::NotPop3(account));
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT d.message_id FROM drafts d \
                 WHERE d.id = ?1 AND d.account_id = ?2 \
                   AND NOT EXISTS (SELECT 1 FROM outbox WHERE draft = d.id)",
                params![draft.0, account.0],
                |row| row.get(0),
            )
            .optional()?;
        let mut stale = Vec::new();
        for folder in drafts_folders(&tx, account)? {
            let ids: Vec<MessageId> = match &current {
                Some(message_id) => tx
                    .prepare_cached(
                        "SELECT id FROM messages WHERE folder_id = ?1 AND message_id = ?2 \
                           AND (blob_key IS NULL OR blob_key <> ?3)",
                    )?
                    .query_map(params![folder.0, message_id, blob.0.as_slice()], |row| {
                        row.get(0).map(MessageId)
                    })?
                    .collect::<rusqlite::Result<_>>()?,
                None => tx
                    .prepare_cached(
                        "SELECT id FROM messages INDEXED BY messages_blob \
                         WHERE blob_key = ?1 AND folder_id = ?2",
                    )?
                    .query_map(params![blob.0.as_slice(), folder.0], |row| {
                        row.get(0).map(MessageId)
                    })?
                    .collect::<rusqlite::Result<_>>()?,
            };
            stale.extend(ids);
        }
        if current.is_some() {
            tx.execute(
                "UPDATE drafts SET copy_revision = max(copy_revision, ?2), upload_attempts = 0 \
                 WHERE id = ?1",
                params![draft.0, revision as i64],
            )?;
        }
        let mut changes = Vec::with_capacity(stale.len());
        for id in stale {
            if let Some(found) = row(&tx, id)? {
                changes.push((found, Change::Expunge));
            }
        }
        let (plans, _) = queue_in(&tx, BATCH_COMPOSE, changes)?;
        let committed = self.state.commit_rows(tx, &plans, BTreeSet::new())?;
        Ok(DraftFiled {
            version: committed.version,
            applied: current.is_some(),
            orphaned_blobs: committed.orphaned_blobs,
        })
    }

    /// The raw messages ops done at once left that nothing refers to any more; the others are
    /// forgotten too, as something refers to them again.
    fn take_blob_deletions(&mut self) -> Result<Vec<BlobKey>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let keys: BTreeSet<BlobKey> = tx
            .prepare_cached("SELECT blob_key FROM blob_deletions")?
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .filter_map(|key| key.map(|key| BlobKey::from_slice(&key)).transpose())
            .collect::<rusqlite::Result<_>>()?;
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        tx.execute("DELETE FROM blob_deletions", [])?;
        let unreferenced = compose::unreferenced(&tx, keys)?;
        tx.commit()?;
        self.state.after_commit();
        Ok(unreferenced)
    }

    fn pop3_account(&mut self, account: AccountId) -> Result<Pop3Account, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id = ?1)",
            [account.0],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::UnknownAccount(account));
        }
        let made = tx.execute(
            "INSERT OR IGNORE INTO pop3_accounts(account_id) VALUES (?1)",
            [account.0],
        )? > 0;
        let roles: Vec<FolderRole> = tx
            .prepare_cached("SELECT role FROM folders WHERE account_id = ?1")?
            .query_map([account.0], |row| {
                row.get::<_, String>(0).map(|role| FolderRole::parse(&role))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let missing: Vec<FolderSpec> = POP3_FOLDERS
            .iter()
            .filter(|(_, role)| !roles.contains(role))
            .map(|(name, role)| FolderSpec::local(account, name, *role))
            .collect();
        let (created, _) = upsert_rows(&tx, &missing)?;
        // Nothing older is left on a server: `LoadOlder` is answered complete, actor or not.
        tx.execute(
            "UPDATE folders SET sync_floor = 1 WHERE account_id = ?1 AND sync_floor IS NULL",
            [account.0],
        )?;
        let inbox: FolderId = tx.query_row(
            "SELECT id FROM folders WHERE account_id = ?1 AND role = 'inbox' ORDER BY id LIMIT 1",
            [account.0],
            |row| row.get(0).map(FolderId),
        )?;
        let first_pass: Option<i64> = tx.query_row(
            "SELECT first_pass FROM pop3_accounts WHERE account_id = ?1",
            [account.0],
            |row| row.get(0),
        )?;
        let reply = Pop3Account {
            account,
            inbox,
            first_pass,
            created: made || !created.is_empty(),
        };
        if created.is_empty() {
            tx.commit()?;
            self.state.after_commit();
            return Ok(reply);
        }
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let mut changes = Vec::new();
        for (view, spec) in &open.open {
            if spec.scope == ViewScope::UnifiedInbox {
                let folders = views::folders(&tx, spec.scope)?;
                changes.push(crate::store::api::ViewChange {
                    view: *view,
                    diff: crate::store::api::ViewDiff::Reload {
                        total: views::count(&tx, &folders, spec.filter)?,
                    },
                });
            }
        }
        self.state.commit(
            tx,
            ChangeEvent {
                folders_changed: true,
                views: changes,
                ..ChangeEvent::default()
            },
        )?;
        drop(open);
        for id in created {
            self.state.accounts.insert(id, account);
        }
        tracing::info!(
            account = account.0,
            "a POP3 account's local folders were created"
        );
        Ok(reply)
    }

    fn pop3_store(
        &mut self,
        account: AccountId,
        downloads: &[Pop3Download],
        at: i64,
    ) -> Result<Pop3Stored, StoreError> {
        for download in downloads {
            let folder = download.message.folder;
            if self.state.account(folder)? != account {
                return Err(StoreError::UnknownFolder(folder));
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !is_pop3(&tx, account)? {
            return Err(StoreError::NotPop3(account));
        }
        let mut ids = Vec::with_capacity(downloads.len());
        let mut rows = Vec::new();
        let mut docs = Vec::new();
        let mut threads = BTreeSet::new();
        let mut merged_threads = Vec::new();
        let mut folders = BTreeSet::new();
        let mut waiting = HashMap::new();
        for download in downloads {
            let known: bool = tx
                .prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM pop3_uidls WHERE account_id = ?1 AND uidl = ?2)",
                )?
                .query_row(params![account.0, download.uidl], |row| row.get(0))?;
            if known {
                ids.push(None);
                continue;
            }
            let message = &download.message;
            let inserted = match free_uid(&tx, message)? {
                Some(uid) => {
                    let (thread, merged) = resolve_thread(&tx, account, message)?;
                    merged_threads.extend(merged.iter().map(|from| ThreadMerge {
                        from: *from,
                        into: thread,
                    }));
                    let id = insert_message(&tx, thread, uid, message)?;
                    let flags = overlay_new(&tx, &mut waiting, message, id, uid)?;
                    threads.insert(thread);
                    folders.insert(message.folder);
                    rows.push(RowChange {
                        id,
                        folder: message.folder,
                        date: message.date,
                        before: None,
                        after: Some((flags, message.has_attachments)),
                    });
                    docs.push(fts_doc(id, message));
                    Some(id)
                }
                None => None,
            };
            // A message the folder held already (the same bytes): the UIDL names that row.
            let row = match (inserted, &message.blob) {
                (Some(id), _) => Some(id),
                (None, Some(blob)) => tx
                    .prepare_cached(
                        "SELECT id FROM messages INDEXED BY messages_blob \
                         WHERE blob_key = ?1 AND folder_id = ?2 ORDER BY id LIMIT 1",
                    )?
                    .query_row(params![blob.0.as_slice(), message.folder.0], |row| {
                        row.get(0).map(MessageId)
                    })
                    .optional()?,
                (None, None) => None,
            };
            tx.prepare_cached(
                "INSERT INTO pop3_uidls(account_id, uidl, message, downloaded, whole) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                account.0,
                download.uidl,
                row.map(|id| id.0),
                at,
                download.whole
            ])?;
            ids.push(inserted);
        }
        if rows.is_empty() {
            tx.commit()?;
            self.state.after_commit();
            return Ok(Pop3Stored {
                version: StoreVersion(self.state.version),
                ids,
            });
        }
        for merge in &merged_threads {
            threads.remove(&merge.from);
        }
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let views = view_changes(&tx, &open.open, &rows, &BTreeMap::new())?;
        let event = ChangeEvent {
            inserted: rows.iter().map(|row| row.id).collect(),
            folders: folder_counts(&tx, &folders)?,
            threads: thread_counts(&tx, &threads)?,
            merged_threads,
            views,
            ..ChangeEvent::default()
        };
        let version = self.state.commit(tx, event)?;
        drop(open);
        if let Err(error) = self.index_docs(&docs) {
            tracing::warn!(%error, "full-text batch of a POP3 download failed");
        }
        Ok(Pop3Stored { version, ids })
    }

    fn pop3_pass_done(
        &mut self,
        account: AccountId,
        done: &Pop3PassDone,
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if done.complete {
            tx.execute(
                "UPDATE pop3_accounts SET first_pass = coalesce(first_pass, ?2) \
                 WHERE account_id = ?1",
                params![account.0, done.at],
            )?;
        }
        {
            let mut deleted = tx.prepare_cached(
                "UPDATE pop3_uidls SET deleted = coalesce(deleted, ?3) \
                 WHERE account_id = ?1 AND uidl = ?2",
            )?;
            for uidl in &done.deleted {
                deleted.execute(params![account.0, uidl, done.at])?;
            }
            let mut forget = tx.prepare_cached(
                "DELETE FROM pop3_uidls \
                 WHERE account_id = ?1 AND uidl = ?2 AND deleted IS NOT NULL",
            )?;
            for uidl in &done.forget {
                forget.execute(params![account.0, uidl])?;
            }
        }
        tx.commit()?;
        self.state.after_commit();
        Ok(())
    }
}
