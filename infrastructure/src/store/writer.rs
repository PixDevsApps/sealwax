//! The writer thread, owner of the only read-write connection. It serves interactive requests
//! first, then pending full-text work, then bulk ingest, one transaction at a time. Each commit
//! that changes what lists show bumps `store_version` and publishes one change event, whose view
//! diffs are computed inside the transaction.

use super::api::{
    BodyUpgrade, ChangeEvent, Evicted, FirstSyncStep, FolderCounts, FolderSpec, Ingested,
    NewAccount, NewMessage, Positioned, StoreVersion, ThreadCount, ThreadMerge, Upgraded,
    Versioned, ViewChange, ViewDiff, ViewFilter, ViewId, ViewScope, ViewSpec,
};
use super::maintenance::Maintenance;
use super::{
    RefusedChange, Removed, RemovedAccount, Shared, StoreError, codec, contacts, fts, views,
};
use rusqlite::hooks::Wal;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sealwax_domain::account::AccountKey;
use sealwax_domain::message::{self, base_subject};
use sealwax_domain::model::{AccountId, Address, BlobKey, Flags, FolderId, MessageId, ThreadId};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::c_int;
use std::sync::Arc;
use std::time::Instant;

mod accounts;
mod bodies;
pub(crate) mod compose;
mod index;
pub(crate) mod ops;
pub(crate) mod overlay;
mod remote;
pub(crate) mod sync;

/// Messages per header transaction (S3: 500 held the writer for up to 258 ms with text).
pub(crate) const MAX_BATCH: usize = 200;
/// Beyond this many rows changing in one view, its event asks for a reload instead.
const MAX_SPLICE: usize = 64;
/// New rows after which the maintenance thread refreshes the planner's statistics.
const OPTIMIZE_AFTER: u64 = 50_000;
/// The start of a `Thread-Index` conversation's alias. The space keeps it apart from every
/// Message-ID, which holds no whitespace once normalized.
pub(crate) const THREAD_INDEX_ALIAS: &str = "thread-index ";

thread_local! {
    /// Frames in the WAL after this thread's last commit, set by the writer's WAL hook.
    static WAL_FRAMES: Cell<c_int> = const { Cell::new(0) };
}

fn record_wal_frames(_wal: &Wal, frames: c_int) -> rusqlite::Result<()> {
    WAL_FRAMES.set(frames);
    Ok(())
}

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

pub(crate) enum WriteJob {
    AddAccount {
        account: NewAccount,
        reply: Sender<Versioned<AccountId>>,
    },
    EnsureAccount {
        key: AccountKey,
        account: NewAccount,
        reply: Sender<Versioned<AccountId>>,
    },
    ReconcileAccounts {
        accounts: Vec<(AccountKey, NewAccount)>,
        reply: Sender<Versioned<Vec<AccountId>>>,
    },
    RemoveAccount {
        account: AccountId,
        reply: Sender<RemovedAccount>,
    },
    UpsertFolder {
        folder: FolderSpec,
        reply: Sender<Versioned<FolderId>>,
    },
    Ingest {
        messages: Vec<NewMessage>,
        reply: Sender<Ingested>,
    },
    SetFlags {
        ids: Vec<MessageId>,
        add: Flags,
        remove: Flags,
        reply: Sender<StoreVersion>,
    },
    Remove {
        ids: Vec<MessageId>,
        reply: Sender<Removed>,
    },
    UpsertFolders {
        folders: Vec<FolderSpec>,
        reply: Sender<Versioned<Vec<FolderId>>>,
    },
    FirstSync {
        folder: FolderId,
        step: FirstSyncStep,
        reply: Sender<StoreVersion>,
    },
    ResetFolder {
        folder: FolderId,
        uidvalidity: u32,
        reply: Sender<Removed>,
    },
    Sync(sync::SyncJob),
    Ops(ops::OpsJob),
    Compose(compose::ComposeJob),
    UpgradeBody {
        upgrade: Box<BodyUpgrade>,
        reply: Sender<Upgraded>,
    },
    TouchBody {
        message: MessageId,
        at: i64,
        reply: Sender<()>,
    },
    EvictBodies {
        limit: u64,
        keep: Vec<MessageId>,
        reply: Sender<Evicted>,
    },
    AllowRemoteSender {
        address: String,
        at: i64,
        reply: Sender<()>,
    },
    ForgetRemoteSender {
        address: String,
        reply: Sender<()>,
    },
    RebuildIndex {
        reply: Sender<super::IndexRebuilt>,
    },
}

/// An ingest still running: the messages left and the result so far.
struct Bulk {
    remaining: VecDeque<NewMessage>,
    result: Ingested,
    reply: Sender<Ingested>,
}

/// The full-text documents of a committed header batch.
struct Index {
    docs: Vec<FtsDoc>,
    bulk: Bulk,
}

struct FtsDoc {
    id: MessageId,
    subject: String,
    sender: String,
    recipients: String,
    body: String,
}

/// What a commit changed about one row, for the views.
#[derive(Clone, Copy)]
struct RowChange {
    id: MessageId,
    folder: FolderId,
    date: i64,
    before: Option<(u32, bool)>,
    after: Option<(u32, bool)>,
}

pub(crate) struct Writer {
    conn: Connection,
    state: State,
    /// Rebuild Search Index, while it runs.
    rebuild: Option<index::Rebuild>,
}

/// Everything but the connection, so a method can hold a transaction and still commit.
struct State {
    shared: Arc<Shared>,
    accounts: HashMap<FolderId, AccountId>,
    version: u64,
    maintenance: async_channel::Sender<Maintenance>,
    checkpoint_frames: c_int,
    inserted_since_optimize: u64,
    /// Outbox changes of the transaction under way, published with its commit.
    outbox: Vec<super::OutboxChange>,
}

impl State {
    /// Commits `tx` as the next version and publishes `event`.
    fn commit(
        &mut self,
        tx: Transaction<'_>,
        mut event: ChangeEvent,
    ) -> Result<StoreVersion, StoreError> {
        event.outbox = std::mem::take(&mut self.outbox);
        let version = self.version + 1;
        tx.prepare_cached("UPDATE store_version SET version = ?1")?
            .execute([version as i64])?;
        tx.commit()?;
        self.version = version;
        event.version = StoreVersion(version);
        self.shared.publish(event);
        self.after_commit();
        Ok(StoreVersion(version))
    }

    fn after_commit(&self) {
        if WAL_FRAMES.get() >= self.checkpoint_frames {
            let _ = self.maintenance.try_send(Maintenance::Checkpoint);
        }
    }

    fn account(&self, folder: FolderId) -> Result<AccountId, StoreError> {
        self.accounts
            .get(&folder)
            .copied()
            .ok_or(StoreError::UnknownFolder(folder))
    }
}

impl Writer {
    pub(crate) fn new(
        conn: Connection,
        shared: Arc<Shared>,
        maintenance: async_channel::Sender<Maintenance>,
        checkpoint_frames: u32,
    ) -> Result<Self, StoreError> {
        conn.wal_hook(Some(record_wal_frames));
        let accounts = conn
            .prepare("SELECT id, account_id FROM folders")?
            .query_map([], |row| {
                Ok((FolderId(row.get(0)?), AccountId(row.get(1)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let version: i64 =
            conn.query_row("SELECT version FROM store_version", [], |row| row.get(0))?;
        Ok(Self {
            conn,
            state: State {
                shared,
                accounts,
                version: version as u64,
                maintenance,
                checkpoint_frames: c_int::try_from(checkpoint_frames).unwrap_or(c_int::MAX),
                inserted_since_optimize: 0,
                outbox: Vec::new(),
            },
            rebuild: None,
        })
    }

    pub(crate) fn run(mut self, queue: async_channel::Receiver<WriteJob>) {
        if let Err(error) = self.recover_index() {
            tracing::warn!(%error, "full-text recovery failed");
        }
        self.resume_rebuild();
        let mut interactive = VecDeque::new();
        let mut bulk = VecDeque::new();
        let mut index: VecDeque<Index> = VecDeque::new();
        loop {
            if interactive.is_empty()
                && bulk.is_empty()
                && index.is_empty()
                && self.rebuild.is_none()
            {
                match queue.recv_blocking() {
                    Ok(job) => self.sort(job, &mut interactive, &mut bulk),
                    Err(_) => break,
                }
            }
            while let Ok(job) = queue.try_recv() {
                self.sort(job, &mut interactive, &mut bulk);
            }
            if queue.is_closed() {
                self.stop_rebuild();
            }
            if let Some(job) = interactive.pop_front() {
                self.interactive(job);
            } else if let Some(job) = index.pop_front() {
                if let Some(next) = self.index(job) {
                    bulk.push_front(next);
                }
            } else if let Some(job) = bulk.pop_front() {
                if let Some(next) = self.ingest(job) {
                    index.push_back(next);
                }
            } else {
                self.rebuild_step();
            }
        }
        if let Err(error) = self
            .conn
            .execute_batch("PRAGMA optimize; PRAGMA wal_checkpoint(TRUNCATE);")
        {
            tracing::warn!(%error, "store maintenance at close failed");
        }
    }

    fn sort(&self, job: WriteJob, interactive: &mut VecDeque<WriteJob>, bulk: &mut VecDeque<Bulk>) {
        match job {
            WriteJob::Ingest { messages, reply } => bulk.push_back(Bulk {
                remaining: messages.into(),
                result: Ingested::default(),
                reply,
            }),
            job => interactive.push_back(job),
        }
    }

    fn interactive(&mut self, job: WriteJob) {
        match job {
            WriteJob::AddAccount { account, reply } => {
                let _ = reply.try_send(self.add_account(&account));
            }
            WriteJob::EnsureAccount {
                key,
                account,
                reply,
            } => {
                let _ = reply.try_send(self.reconcile_accounts(&[(key, account)], false).map(
                    |ids| Versioned {
                        version: ids.version,
                        value: ids.value[0],
                    },
                ));
            }
            WriteJob::ReconcileAccounts { accounts, reply } => {
                let _ = reply.try_send(self.reconcile_accounts(&accounts, true));
            }
            WriteJob::RemoveAccount { account, reply } => {
                let _ = reply.try_send(self.remove_account(account));
            }
            WriteJob::UpsertFolder { folder, reply } => {
                let _ = reply.try_send(self.upsert_folder(&folder));
            }
            WriteJob::SetFlags {
                ids,
                add,
                remove,
                reply,
            } => {
                let _ = reply.try_send(self.set_flags(&ids, add, remove));
            }
            WriteJob::Remove { ids, reply } => {
                let _ = reply.try_send(self.remove(&ids));
            }
            WriteJob::UpsertFolders { folders, reply } => {
                let _ = reply.try_send(self.upsert_folders(&folders));
            }
            WriteJob::FirstSync {
                folder,
                step,
                reply,
            } => {
                let _ = reply.try_send(self.first_sync_step(folder, step));
            }
            WriteJob::ResetFolder {
                folder,
                uidvalidity,
                reply,
            } => {
                let _ = reply.try_send(self.reset_folder(folder, uidvalidity));
            }
            WriteJob::Sync(job) => self.sync_job(job),
            WriteJob::Ops(job) => self.ops_job(job),
            WriteJob::Compose(job) => self.compose_job(job),
            WriteJob::UpgradeBody { upgrade, reply } => {
                let _ = reply.try_send(self.upgrade_body(&upgrade));
            }
            WriteJob::TouchBody { message, at, reply } => {
                let _ = reply.try_send(self.touch_body(message, at));
            }
            WriteJob::EvictBodies { limit, keep, reply } => {
                let _ = reply.try_send(self.evict_bodies(limit, &keep));
            }
            WriteJob::AllowRemoteSender { address, at, reply } => {
                let _ = reply.try_send(self.allow_remote_sender(&address, at));
            }
            WriteJob::ForgetRemoteSender { address, reply } => {
                let _ = reply.try_send(self.forget_remote_sender(&address));
            }
            WriteJob::RebuildIndex { reply } => self.start_rebuild(reply),
            WriteJob::Ingest { .. } => unreachable!("ingest jobs are bulk"),
        }
    }

    fn add_account(&mut self, account: &NewAccount) -> Result<Versioned<AccountId>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO accounts(name, address) VALUES (?1, ?2)",
            params![account.name, account.address],
        )?;
        let id = AccountId(tx.last_insert_rowid());
        let version = self.state.commit(
            tx,
            ChangeEvent {
                folders_changed: true,
                ..ChangeEvent::default()
            },
        )?;
        Ok(Versioned { version, value: id })
    }

    fn upsert_folder(&mut self, folder: &FolderSpec) -> Result<Versioned<FolderId>, StoreError> {
        let Versioned { version, value } = self.upsert_folders(std::slice::from_ref(folder))?;
        Ok(Versioned {
            version,
            value: value[0],
        })
    }

    /// Upserts folders by `(account, name_raw)` in one transaction. STATUS counts replace the
    /// counts only of folders still new and empty (see `check`).
    fn upsert_folders(
        &mut self,
        folders: &[FolderSpec],
    ) -> Result<Versioned<Vec<FolderId>>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (ids, counted) = upsert_rows(&tx, folders)?;
        let shared = Arc::clone(&self.state.shared);
        let open = shared.registry.lock();
        let mut views = Vec::new();
        for (view, spec) in &open.open {
            if spec.scope == ViewScope::UnifiedInbox {
                let folders = views::folders(&tx, spec.scope)?;
                views.push(ViewChange {
                    view: *view,
                    diff: ViewDiff::Reload {
                        total: views::count(&tx, &folders, spec.filter)?,
                    },
                });
            }
        }
        let event = ChangeEvent {
            folders: folder_counts(&tx, &counted)?,
            folders_changed: true,
            views,
            ..ChangeEvent::default()
        };
        let version = self.state.commit(tx, event)?;
        drop(open);
        for (id, folder) in ids.iter().zip(folders) {
            self.state.accounts.insert(*id, folder.account);
        }
        Ok(Versioned {
            version,
            value: ids,
        })
    }

    fn first_sync_step(
        &mut self,
        folder: FolderId,
        step: FirstSyncStep,
    ) -> Result<StoreVersion, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = match step {
            FirstSyncStep::Start {
                uidvalidity,
                top,
                modseq,
            } => tx.execute(
                "UPDATE folders SET uidvalidity = ?2, sync_state = 'initial', initial_top = ?3, \
                   initial_modseq = ?4, sync_floor = ?3, below_floor = 0, below_unread = 0, \
                   total_count = (SELECT count(*) FROM messages \
                     WHERE folder_id = ?1 AND flags & 264 = 0), \
                   unread_count = (SELECT count(*) FROM messages \
                     WHERE folder_id = ?1 AND flags & 265 = 0) \
                 WHERE id = ?1 AND sync_state = 'new'",
                params![folder.0, uidvalidity, top, modseq as i64],
            )?,
            FirstSyncStep::Floor(floor) => tx.execute(
                "UPDATE folders SET sync_floor = ?2 WHERE id = ?1 AND sync_state = 'initial'",
                params![folder.0, floor],
            )?,
            FirstSyncStep::Finish { below } => tx.execute(
                "UPDATE folders SET sync_state = 'ready', uidnext = initial_top, \
                   highestmodseq = initial_modseq, initial_top = NULL, initial_modseq = NULL, \
                   below_floor = ?2, sync_floor = CASE WHEN ?2 = 0 THEN 1 ELSE sync_floor END \
                 WHERE id = ?1 AND sync_state = 'initial'",
                params![folder.0, below],
            )?,
        };
        if changed == 0 {
            return Err(StoreError::SyncState(folder));
        }
        if matches!(step, FirstSyncStep::Finish { .. }) {
            // Ops whose messages left with a UIDVALIDITY change and didn't come back.
            let mut refusals = overlay::Refusals::default();
            for (account, ops) in overlay::waiting(&tx, folder)? {
                refusals.refuse(&tx, account, folder, &ops)?;
            }
            refusals.record(&tx)?;
        }
        // Messages left below the floor join the counts the sidebar shows.
        let counted = match step {
            FirstSyncStep::Start { .. } => true,
            FirstSyncStep::Floor(_) => false,
            FirstSyncStep::Finish { below } => below > 0,
        };
        if !counted {
            tx.commit()?;
            self.state.after_commit();
            return Ok(StoreVersion(self.state.version));
        }
        let event = ChangeEvent {
            folders: folder_counts(&tx, &BTreeSet::from([folder]))?,
            ..ChangeEvent::default()
        };
        self.state.commit(tx, event)
    }

    /// The folder's pending ops survive: they wait for its next sync to bring their messages
    /// back, found by Message-ID (`overlay::attach_waiting`), and are refused at the end of its
    /// first sync if they don't come. A pending read below the floor is refused at once.
    fn reset_folder(&mut self, folder: FolderId, uidvalidity: u32) -> Result<Removed, StoreError> {
        self.state.account(folder)?;
        let ids = self
            .conn
            .prepare_cached("SELECT id FROM messages WHERE folder_id = ?1")?
            .query_map([folder.0], |row| Ok(MessageId(row.get(0)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let removed = self.remove_rows(&ids, true)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut refusals = overlay::Refusals::default();
        for (op, account, below) in overlay::reads_below(&tx, folder)? {
            tx.execute("DELETE FROM ops WHERE id = ?1", [op.0])?;
            refusals.add(account, RefusedChange::FolderRead, below);
        }
        refusals.record(&tx)?;
        let below = below_counts(&tx, folder)?;
        tx.execute(
            "UPDATE folders SET uidvalidity = ?2, sync_state = 'new', uidnext = 1, \
               highestmodseq = 0, initial_top = NULL, initial_modseq = NULL, sync_floor = NULL, \
               below_floor = 0, below_unread = 0 \
             WHERE id = ?1",
            params![folder.0, uidvalidity],
        )?;
        if below == (0, 0) {
            tx.commit()?;
            self.state.after_commit();
            return Ok(removed);
        }
        let event = ChangeEvent {
            folders: folder_counts(&tx, &BTreeSet::from([folder]))?,
            ..ChangeEvent::default()
        };
        let version = self.state.commit(tx, event)?;
        Ok(Removed { version, ..removed })
    }

    /// Commits the next batch of headers; returns the batch's full-text work.
    fn ingest(&mut self, mut bulk: Bulk) -> Option<Index> {
        if bulk.remaining.is_empty() {
            bulk.result.version = StoreVersion(self.state.version);
            let _ = bulk.reply.try_send(Ok(bulk.result));
            return None;
        }
        if bulk.result.batches == 0
            && let Some(unknown) = bulk
                .remaining
                .iter()
                .find(|message| self.state.account(message.folder).is_err())
        {
            let _ = bulk
                .reply
                .try_send(Err(StoreError::UnknownFolder(unknown.folder)));
            return None;
        }
        let count = bulk.remaining.len().min(MAX_BATCH);
        let batch: Vec<NewMessage> = bulk.remaining.drain(..count).collect();
        let started = Instant::now();
        match self.insert_batch(&batch) {
            Ok((version, ids, docs)) => {
                bulk.result.header_time += started.elapsed();
                bulk.result.batches += 1;
                bulk.result.version = version;
                bulk.result.ids.extend(ids);
                self.state.inserted_since_optimize += docs.len() as u64;
                if self.state.inserted_since_optimize >= OPTIMIZE_AFTER {
                    self.state.inserted_since_optimize = 0;
                    let _ = self.state.maintenance.try_send(Maintenance::Optimize);
                }
                tracing::debug!(
                    messages = batch.len(),
                    ms = started.elapsed().as_secs_f64() * 1000.0,
                    "header batch committed"
                );
                Some(Index { docs, bulk })
            }
            Err(error) => {
                tracing::warn!(%error, "header batch failed");
                let _ = bulk.reply.try_send(Err(error));
                None
            }
        }
    }

    /// Indexes a committed batch; returns the ingest if messages remain, else replies.
    fn index(&mut self, Index { docs, mut bulk }: Index) -> Option<Bulk> {
        let started = Instant::now();
        if let Err(error) = self.index_docs(&docs) {
            tracing::warn!(%error, "full-text batch failed");
            let _ = bulk.reply.try_send(Err(error));
            return None;
        }
        bulk.result.index_time += started.elapsed();
        if bulk.remaining.is_empty() {
            let _ = bulk.reply.try_send(Ok(bulk.result));
            None
        } else {
            Some(bulk)
        }
    }

    /// Indexes `docs` in one transaction and records the highest id indexed, so a crash between
    /// a header commit and its full-text commit is repaired at the next start.
    fn index_docs(&mut self, docs: &[FtsDoc]) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            // A body downloaded between a batch's header commit and this one has indexed the
            // whole message already.
            let mut insert = tx.prepare_cached(
                "INSERT INTO message_fts(rowid, subject, sender, recipients, body) \
                 SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM messages WHERE id = ?1) \
                   AND NOT EXISTS (SELECT 1 FROM message_fts WHERE rowid = ?1)",
            )?;
            for doc in docs {
                insert.execute(params![
                    doc.id.0,
                    fts::index_text(&doc.subject),
                    fts::index_text(&doc.sender),
                    fts::index_text(&doc.recipients),
                    fts::index_text(&doc.body),
                ])?;
            }
        }
        if let Some(last) = docs.iter().map(|doc| doc.id.0).max() {
            tx.prepare_cached(
                "INSERT INTO meta(key, value) VALUES ('fts_through', ?1) \
                 ON CONFLICT(key) DO UPDATE SET value = max(value, excluded.value)",
            )?
            .execute([last])?;
        }
        tx.commit()?;
        self.state.after_commit();
        Ok(())
    }

    /// Indexes messages a crash left without full text: those past the recorded high-water mark
    /// with no index row. The body text comes from the blob store when the store has one.
    fn recover_index(&mut self) -> Result<(), StoreError> {
        let through: i64 = self.conn.query_row(
            "SELECT coalesce((SELECT value FROM meta WHERE key = 'fts_through'), 0)",
            [],
            |row| row.get(0),
        )?;
        let missing = self
            .conn
            .prepare(
                "SELECT id, subject, from_name, from_addr, to_addrs, cc_addrs, snippet, blob_key \
                 FROM messages m WHERE id > ?1 \
                   AND NOT EXISTS (SELECT 1 FROM message_fts WHERE rowid = m.id) ORDER BY id",
            )?
            .query_map([through], |row| {
                Ok((
                    MessageId(row.get(0)?),
                    row.get::<_, String>(1)?,
                    Address::new(row.get::<_, String>(2)?, row.get::<_, String>(3)?),
                    [
                        codec::decode_addresses(&row.get::<_, String>(4)?),
                        codec::decode_addresses(&row.get::<_, String>(5)?),
                    ]
                    .concat(),
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if missing.is_empty() {
            return Ok(());
        }
        let blobs = self.state.shared.blobs.clone();
        for chunk in missing.chunks(MAX_BATCH) {
            let docs: Vec<FtsDoc> = chunk
                .iter()
                .map(|(id, subject, sender, recipients, snippet, blob)| {
                    let body = blobs
                        .as_ref()
                        .zip(blob.as_deref().and_then(BlobKey::from_slice))
                        .and_then(|(blobs, key)| blobs.get(&key).ok())
                        .map(|raw| {
                            let parsed = message::parse(&raw);
                            fts::body_text(&parsed.body_text, &parsed.attachments).into_owned()
                        })
                        .unwrap_or_else(|| snippet.clone());
                    FtsDoc {
                        id: *id,
                        subject: subject.clone(),
                        sender: people(std::slice::from_ref(sender)),
                        recipients: people(recipients),
                        body,
                    }
                })
                .collect();
            self.index_docs(&docs)?;
        }
        tracing::info!(
            messages = missing.len(),
            "full-text index repaired after a crash"
        );
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn insert_batch(
        &mut self,
        batch: &[NewMessage],
    ) -> Result<(StoreVersion, Vec<Option<MessageId>>, Vec<FtsDoc>), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut ids = Vec::with_capacity(batch.len());
        let mut docs = Vec::with_capacity(batch.len());
        let mut rows = Vec::with_capacity(batch.len());
        let mut threads = BTreeSet::new();
        let mut merged_threads = Vec::new();
        let mut folders = BTreeSet::new();
        let mut waiting = HashMap::new();
        for message in batch {
            let account = self.state.account(message.folder)?;
            let Some(uid) = free_uid(&tx, message)? else {
                ids.push(None);
                continue;
            };
            let (thread, merged) = resolve_thread(&tx, account, message)?;
            merged_threads.extend(merged.iter().map(|from| ThreadMerge {
                from: *from,
                into: thread,
            }));
            let id = insert_message(&tx, thread, uid, message)?;
            let flags = overlay_new(&tx, &mut waiting, message, id, uid)?;
            ids.push(Some(id));
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
        }
        if rows.is_empty() {
            tx.commit()?;
            return Ok((StoreVersion(self.state.version), ids, docs));
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
        Ok((version, ids, docs))
    }

    /// The server's flags changed: `add` and `remove` apply to its flags, and what is shown
    /// keeps any pending op.
    fn set_flags(
        &mut self,
        ids: &[MessageId],
        add: Flags,
        remove: Flags,
    ) -> Result<StoreVersion, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut plans = Vec::new();
        let mut seen = BTreeSet::new();
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            let Some(row) = overlay::row(&tx, *id)? else {
                continue;
            };
            let server = Flags::from_bits(row.server)
                .union(add)
                .difference(remove)
                .bits();
            if server == row.server {
                continue;
            }
            let flags = overlay::shown(&tx, &row, server)?;
            plans.push(overlay::RowPlan {
                after: overlay::After::Stay { flags, server },
                row,
            });
        }
        if plans.is_empty() {
            return Ok(StoreVersion(self.state.version));
        }
        Ok(self.state.commit_rows(tx, &plans, BTreeSet::new())?.version)
    }

    fn remove(&mut self, ids: &[MessageId]) -> Result<Removed, StoreError> {
        self.remove_rows(ids, false)
    }

    /// Removes rows the server expunged, whose pending ops end as `overlay::vanished` says; or,
    /// with `retarget`, rows a UIDVALIDITY change took, whose pending ops wait for their messages
    /// to come back (`overlay::detach`).
    fn remove_rows(&mut self, ids: &[MessageId], retarget: bool) -> Result<Removed, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut plans = Vec::new();
        let mut seen = BTreeSet::new();
        let mut refusals = overlay::Refusals::default();
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            let Some(row) = overlay::row(&tx, *id)? else {
                continue;
            };
            if retarget {
                overlay::detach(&tx, &row, &mut refusals)?;
            } else {
                overlay::vanished(&tx, row.id, row.account, row.folder, &mut refusals)?;
            }
            plans.push(overlay::RowPlan {
                row,
                after: overlay::After::Deleted,
            });
        }
        if plans.is_empty() {
            return Ok(Removed {
                version: StoreVersion(self.state.version),
                ..Removed::default()
            });
        }
        refusals.record(&tx)?;
        let removed = plans.iter().map(|plan| plan.row.id).collect();
        let committed = self.state.commit_rows(tx, &plans, BTreeSet::new())?;
        Ok(Removed {
            version: committed.version,
            removed,
            orphaned_blobs: committed.orphaned_blobs,
        })
    }
}

/// Gives a new row the ops waiting in its folder for its Message-ID, the folder's pending read
/// below the floor, and an expunge when it is a Drafts copy queued for deletion
/// (`compose::take_deletion`), when the folder has any (`waiting` caches that per batch);
/// returns the flags the row shows.
pub(crate) fn overlay_new(
    tx: &Connection,
    waiting: &mut HashMap<FolderId, bool>,
    message: &NewMessage,
    id: MessageId,
    uid: i64,
) -> rusqlite::Result<u32> {
    let flags = message.flags.bits();
    let folder = message.folder;
    let has = match waiting.get(&folder) {
        Some(has) => *has,
        None => *waiting
            .entry(folder)
            .or_insert(overlay::has_waiting(tx, folder)? || compose::has_deletions(tx, folder)?),
    };
    if !has {
        return Ok(flags);
    }
    let uid = uid as u32;
    let uidvalidity: u32 = tx
        .prepare_cached("SELECT coalesce(uidvalidity, 0) FROM folders WHERE id = ?1")?
        .query_row([folder.0], |row| row.get(0))?;
    let message_id = message.headers.message_id.as_deref();
    overlay::attach_waiting(tx, folder, id, message_id, uid, uidvalidity)?;
    compose::take_deletion(tx, folder, id, uid, uidvalidity, message_id)?;
    let shown = overlay::shown_at(tx, id, folder, uidvalidity, uid, flags)?;
    if shown != flags {
        tx.prepare_cached("UPDATE messages SET flags = ?2, server_flags = ?3 WHERE id = ?1")?
            .execute(params![id.0, shown, overlay::server_column(shown, flags)])?;
        overlay::count_delta(tx, folder, Some(flags), Some(shown))?;
    }
    Ok(shown)
}

/// Upserts folders by `(account, name_raw)`; returns their ids in input order and the folders
/// whose counts became the server's STATUS counts (those still new and empty).
fn upsert_rows(
    tx: &Connection,
    folders: &[FolderSpec],
) -> rusqlite::Result<(Vec<FolderId>, BTreeSet<FolderId>)> {
    let mut ids = Vec::with_capacity(folders.len());
    let mut counted = BTreeSet::new();
    let mut upsert = tx.prepare_cached(
        "INSERT INTO folders(account_id, name, name_raw, delimiter, role, attributes, \
           selectable) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(account_id, name_raw) DO UPDATE SET \
           name = excluded.name, delimiter = excluded.delimiter, role = excluded.role, \
           attributes = excluded.attributes, selectable = excluded.selectable \
         RETURNING id",
    )?;
    let mut counts = tx.prepare_cached(
        "UPDATE folders SET total_count = ?2, unread_count = ?3 \
         WHERE id = ?1 AND sync_state = 'new' \
           AND NOT EXISTS (SELECT 1 FROM messages WHERE folder_id = ?1)",
    )?;
    for folder in folders {
        let id = FolderId(upsert.query_row(
            params![
                folder.account.0,
                folder.name,
                folder.name_raw,
                folder.delimiter.map(String::from),
                folder.role.as_str(),
                folder.attributes.join(" "),
                folder.selectable,
            ],
            |row| row.get(0),
        )?);
        if let Some(server) = folder.server_counts.filter(|_| folder.selectable)
            && counts.execute(params![id.0, server.total, server.unread])? > 0
        {
            counted.insert(id);
        }
        ids.push(id);
    }
    Ok((ids, counted))
}

/// The UID a new message gets, or `None` when the folder already holds it: the same UID, or,
/// for local folders without UIDs, the same raw content.
fn free_uid(tx: &Connection, message: &NewMessage) -> rusqlite::Result<Option<i64>> {
    let folder = message.folder.0;
    if let Some(uid) = message.uid {
        let taken: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE folder_id = ?1 AND uid = ?2)",
            )?
            .query_row(params![folder, uid], |row| row.get(0))?;
        return Ok((!taken).then_some(i64::from(uid)));
    }
    if let Some(blob) = &message.blob {
        let held: bool = tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM messages INDEXED BY messages_blob \
                 WHERE blob_key = ?1 AND folder_id = ?2)",
            )?
            .query_row(params![blob.0.as_slice(), folder], |row| row.get(0))?;
        if held {
            return Ok(None);
        }
    }
    tx.prepare_cached("SELECT coalesce(max(uid), 0) + 1 FROM messages WHERE folder_id = ?1")?
        .query_row([folder], |row| row.get(0))
        .map(Some)
}

/// The message's conversation through the alias table (ADR-016): every Message-ID,
/// In-Reply-To and References entry, and the conversation of an Outlook `Thread-Index`, maps
/// to a thread, so a parent that arrives after its reply still joins it; a message that links
/// several threads merges them into the largest.
fn resolve_thread(
    tx: &Connection,
    account: AccountId,
    message: &NewMessage,
) -> rusqlite::Result<(ThreadId, Vec<ThreadId>)> {
    let headers = &message.headers;
    let thread_index = headers
        .thread_index
        .map(|index| format!("{THREAD_INDEX_ALIAS}{}", index.to_hex()));
    let mut keys: Vec<&str> = headers
        .message_id
        .iter()
        .chain(&headers.in_reply_to)
        .chain(&headers.references)
        .chain(&thread_index)
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let mut found: Vec<i64> = Vec::new();
    {
        let mut lookup = tx.prepare_cached(
            "SELECT thread_id FROM thread_aliases WHERE account_id = ?1 AND message_id = ?2",
        )?;
        for key in &keys {
            if let Some(thread) = lookup
                .query_row(params![account.0, key], |row| row.get::<_, i64>(0))
                .optional()?
                && !found.contains(&thread)
            {
                found.push(thread);
            }
        }
    }
    let (thread, merged) = match found.as_slice() {
        [] => {
            let thread = tx
                .prepare_cached(
                    "INSERT INTO threads(account_id, subject, first_date, last_date) \
                     VALUES (?1, ?2, ?3, ?3) RETURNING id",
                )?
                .query_row(
                    params![account.0, base_subject(&headers.subject), message.date],
                    |row| row.get(0),
                )?;
            (thread, Vec::new())
        }
        [thread] => (*thread, Vec::new()),
        _ => merge_threads(tx, &found)?,
    };
    let mut alias = tx.prepare_cached(
        "INSERT OR IGNORE INTO thread_aliases(account_id, message_id, thread_id) VALUES (?1, ?2, ?3)",
    )?;
    for key in &keys {
        alias.execute(params![account.0, key, thread])?;
    }
    Ok((ThreadId(thread), merged))
}

/// Joins `threads` into the one with the most messages; returns it and the ones merged away.
fn merge_threads(tx: &Connection, threads: &[i64]) -> rusqlite::Result<(i64, Vec<ThreadId>)> {
    let mut sized = Vec::with_capacity(threads.len());
    for thread in threads {
        let size: i64 = tx
            .prepare_cached("SELECT message_count FROM threads WHERE id = ?1")?
            .query_row([thread], |row| row.get(0))?;
        sized.push((size, -thread));
    }
    sized.sort_unstable();
    let target = -sized.last().expect("at least two threads").1;
    let mut merged = Vec::new();
    for other in threads.iter().copied().filter(|thread| *thread != target) {
        tx.prepare_cached("UPDATE messages SET thread_id = ?1 WHERE thread_id = ?2")?
            .execute([target, other])?;
        tx.prepare_cached("UPDATE thread_aliases SET thread_id = ?1 WHERE thread_id = ?2")?
            .execute([target, other])?;
        tx.prepare_cached(
            "UPDATE threads SET \
               message_count = threads.message_count + o.message_count, \
               last_date = max(threads.last_date, o.last_date), \
               subject = CASE WHEN o.first_date < threads.first_date THEN o.subject \
                 ELSE threads.subject END, \
               first_date = min(threads.first_date, o.first_date) \
             FROM (SELECT * FROM threads WHERE id = ?2) AS o WHERE threads.id = ?1",
        )?
        .execute([target, other])?;
        tx.prepare_cached("DELETE FROM threads WHERE id = ?1")?
            .execute([other])?;
        merged.push(ThreadId(other));
    }
    Ok((target, merged))
}

fn insert_message(
    tx: &Connection,
    thread: ThreadId,
    uid: i64,
    message: &NewMessage,
) -> rusqlite::Result<MessageId> {
    let headers = &message.headers;
    let duplicate = match &headers.message_id {
        Some(message_id) => tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE message_id = ?1 AND thread_id = ?2)",
            )?
            .query_row(params![message_id, thread.0], |row| row.get::<_, bool>(0))?,
        None => false,
    };
    let (account, in_sent, own): (i64, bool, String) = tx
        .prepare_cached(
            "SELECT f.account_id, f.role = 'sent', a.address FROM folders f \
             JOIN accounts a ON a.id = f.account_id WHERE f.id = ?1",
        )?
        .query_row([message.folder.0], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let written = in_sent
        && match &headers.message_id {
            Some(message_id) => !tx
                .prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id \
                     WHERE m.message_id = ?1 AND f.account_id = ?2 AND f.role = 'sent')",
                )?
                .query_row(params![message_id, account], |row| row.get::<_, bool>(0))?,
            None => true,
        };
    contacts::add(
        tx,
        &contacts::Seen {
            account: AccountId(account),
            own: &own,
            from: headers.from.first(),
            to: &headers.to,
            cc: &headers.cc,
            date: message.date,
            first_copy: !duplicate,
            written,
        },
    )?;
    let sender = headers.from.first();
    let id = MessageId(
        tx.prepare_cached(
            "INSERT INTO messages(folder_id, uid, thread_id, message_id, in_reply_to, refs, \
               date, sent_date, from_name, from_addr, to_addrs, cc_addrs, reply_to, subject, \
               snippet, list_id, flags, keywords, has_attachments, body_kind, size, body_state, \
               blob_key, thread_index) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
               ?18, ?19, ?20, ?21, ?22, ?23, ?24) RETURNING id",
        )?
        .query_row(
            params![
                message.folder.0,
                uid,
                thread.0,
                headers.message_id,
                codec::encode_words(&headers.in_reply_to),
                codec::encode_words(&headers.references),
                message.date,
                headers.date,
                sender.map_or("", |address| address.name.as_str()),
                sender.map_or("", |address| address.email.as_str()),
                codec::encode_addresses(&headers.to),
                codec::encode_addresses(&headers.cc),
                codec::encode_addresses(&headers.reply_to),
                headers.subject,
                message.snippet,
                headers.list_id,
                message.flags.bits(),
                codec::encode_words(&message.keywords),
                message.has_attachments,
                message.body_kind.code(),
                message.size as i64,
                message.body_state.code(),
                message.blob.as_ref().map(|key| key.0.as_slice()),
                headers
                    .thread_index
                    .as_ref()
                    .map(|index| index.0.as_slice()),
            ],
            |row| row.get(0),
        )?,
    );
    {
        let mut part = tx.prepare_cached(
            "INSERT OR IGNORE INTO message_parts(message, part, role, mime_type, name, size, \
               content_id, inline) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for index in &message.text_parts {
            part.execute(params![
                id.0,
                index,
                0,
                "text/plain",
                None::<&str>,
                0,
                None::<&str>,
                false
            ])?;
        }
        for index in &message.html_parts {
            part.execute(params![
                id.0,
                index,
                1,
                "text/html",
                None::<&str>,
                0,
                None::<&str>,
                false
            ])?;
        }
        for attachment in &message.attachments {
            part.execute(params![
                id.0,
                attachment.part,
                2,
                attachment.mime_type,
                attachment.name,
                attachment.size as i64,
                attachment.content_id,
                attachment.inline,
            ])?;
        }
    }
    tx.prepare_cached(
        "UPDATE threads SET message_count = message_count + ?2, \
           last_date = max(last_date, ?3), \
           subject = CASE WHEN ?3 < first_date THEN ?4 ELSE subject END, \
           first_date = min(first_date, ?3) \
         WHERE id = ?1",
    )?
    .execute(params![
        thread.0,
        i64::from(!duplicate),
        message.date,
        base_subject(&headers.subject)
    ])?;
    overlay::count_delta(tx, message.folder, None, Some(message.flags.bits()))?;
    Ok(id)
}

fn people(list: &[Address]) -> String {
    list.iter()
        .map(|address| format!("{} {}", address.name, address.email))
        .collect::<Vec<_>>()
        .join(", ")
}

fn fts_doc(id: MessageId, message: &NewMessage) -> FtsDoc {
    let headers = &message.headers;
    FtsDoc {
        id,
        subject: headers.subject.clone(),
        sender: people(&headers.from),
        recipients: people(&[headers.to.as_slice(), headers.cc.as_slice()].concat()),
        body: fts::body_text(&message.search_text, &message.attachments).into_owned(),
    }
}

/// Recomputes a conversation after removals: size (each Message-ID once), dates and subject;
/// deletes it, with its aliases, when no message is left.
fn recompute_thread(tx: &Connection, thread: ThreadId) -> rusqlite::Result<()> {
    let (count, first, last): (i64, Option<i64>, Option<i64>) = tx
        .prepare_cached(
            "SELECT count(DISTINCT message_id) + count(*) - count(message_id), min(date), \
             max(date) FROM messages WHERE thread_id = ?1",
        )?
        .query_row([thread.0], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let (Some(first), Some(last)) = (first, last) else {
        tx.prepare_cached("DELETE FROM threads WHERE id = ?1")?
            .execute([thread.0])?;
        return Ok(());
    };
    let subject: String = tx
        .prepare_cached(
            "SELECT subject FROM messages WHERE thread_id = ?1 ORDER BY date, id LIMIT 1",
        )?
        .query_row([thread.0], |row| row.get(0))?;
    tx.prepare_cached(
        "UPDATE threads SET message_count = ?2, first_date = ?3, last_date = ?4, subject = ?5 \
         WHERE id = ?1",
    )?
    .execute(params![
        thread.0,
        count,
        first,
        last,
        base_subject(&subject)
    ])?;
    Ok(())
}

/// A folder's messages and unread messages below its floor, as last counted.
fn below_counts(tx: &Connection, folder: FolderId) -> rusqlite::Result<(u32, u32)> {
    tx.prepare_cached("SELECT below_floor, below_unread FROM folders WHERE id = ?1")?
        .query_row([folder.0], |row| Ok((row.get(0)?, row.get(1)?)))
}

/// The counts the sidebar shows: the store's own plus what a windowed folder leaves below its
/// floor (ML-043), whose unread a pending "mark folder read" already counts as read.
fn folder_counts(
    tx: &Connection,
    folders: &BTreeSet<FolderId>,
) -> rusqlite::Result<Vec<FolderCounts>> {
    let mut select = tx.prepare_cached(&format!(
        "SELECT total_count + below_floor, {} FROM folders WHERE id = ?1",
        super::overlay::UNREAD_SQL
    ))?;
    folders
        .iter()
        .map(|folder| {
            select.query_row([folder.0], |row| {
                Ok(FolderCounts {
                    folder: *folder,
                    total: row.get(0)?,
                    unread: row.get(1)?,
                })
            })
        })
        .collect()
}

fn thread_counts(
    tx: &Connection,
    threads: &BTreeSet<ThreadId>,
) -> rusqlite::Result<Vec<ThreadCount>> {
    let mut select = tx.prepare_cached("SELECT message_count FROM threads WHERE id = ?1")?;
    let mut counts = Vec::new();
    for thread in threads {
        let count: Option<u32> = select.query_row([thread.0], |row| row.get(0)).optional()?;
        counts.push(ThreadCount {
            thread: *thread,
            count: count.unwrap_or(0),
        });
    }
    Ok(counts)
}

fn in_view(
    filter: ViewFilter,
    folders: &[FolderId],
    folder: FolderId,
    state: Option<(u32, bool)>,
) -> bool {
    state.is_some_and(|(flags, has_attachments)| {
        folders.contains(&folder) && views::passes(filter, flags, has_attachments)
    })
}

/// Positions, before any change of this transaction, of the rows that leave each open view.
fn removed_positions(
    tx: &Connection,
    open: &BTreeMap<ViewId, ViewSpec>,
    rows: &[RowChange],
) -> rusqlite::Result<BTreeMap<ViewId, Vec<u32>>> {
    let mut removed = BTreeMap::new();
    for (view, spec) in open {
        let folders = views::folders(tx, spec.scope)?;
        let leaving: Vec<&RowChange> = rows
            .iter()
            .filter(|row| {
                in_view(spec.filter, &folders, row.folder, row.before)
                    && !in_view(spec.filter, &folders, row.folder, row.after)
            })
            .collect();
        if leaving.is_empty() || leaving.len() > MAX_SPLICE {
            removed.insert(*view, Vec::with_capacity(leaving.len()));
            continue;
        }
        let mut positions = leaving
            .iter()
            .map(|row| views::position(tx, &folders, spec.filter, row.date, row.id, row.folder))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        positions.sort_unstable_by(|a, b| b.cmp(a));
        removed.insert(*view, positions);
    }
    Ok(removed)
}

/// Each open view's diff after this transaction's changes; views it didn't touch get none.
fn view_changes(
    tx: &Connection,
    open: &BTreeMap<ViewId, ViewSpec>,
    rows: &[RowChange],
    removed: &BTreeMap<ViewId, Vec<u32>>,
) -> rusqlite::Result<Vec<ViewChange>> {
    let mut changes = Vec::new();
    for (view, spec) in open {
        let folders = views::folders(tx, spec.scope)?;
        let mut leaving = 0;
        let mut entering = Vec::new();
        let mut staying = Vec::new();
        for row in rows {
            let before = in_view(spec.filter, &folders, row.folder, row.before);
            let after = in_view(spec.filter, &folders, row.folder, row.after);
            match (before, after) {
                (true, false) => leaving += 1,
                (false, true) => entering.push(row),
                (true, true) => staying.push(row),
                (false, false) => {}
            }
        }
        if leaving == 0 && entering.is_empty() && staying.is_empty() {
            continue;
        }
        let total = views::count(tx, &folders, spec.filter)?;
        let diff = if leaving + entering.len() + staying.len() > MAX_SPLICE {
            ViewDiff::Reload { total }
        } else {
            let positioned = |list: &[&RowChange]| -> rusqlite::Result<Vec<Positioned>> {
                let mut out = list
                    .iter()
                    .map(|row| {
                        Ok(Positioned {
                            position: views::position(
                                tx,
                                &folders,
                                spec.filter,
                                row.date,
                                row.id,
                                row.folder,
                            )?,
                            row: views::summary(tx, row.id)?,
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                out.sort_unstable_by_key(|item| item.position);
                Ok(out)
            };
            ViewDiff::Splice {
                removed: removed.get(view).cloned().unwrap_or_default(),
                inserted: positioned(&entering)?,
                changed: positioned(&staying)?,
                total,
            }
        };
        changes.push(ViewChange { view: *view, diff });
    }
    Ok(changes)
}
