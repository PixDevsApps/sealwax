//! Drafts, the outbox and Drafts-folder copies (M6: ML-074, ML-076, ML-077; ADR-015). Every
//! step of a send is a compare-and-set on the outbox row's state, so no path sends a message
//! twice without the user: only a queued entry starts, the start is committed with a full sync
//! before SMTP DATA, and an entry left sending becomes uncertain, which only the user moves on.

use super::ops::queue_in;
use super::overlay::{RowPlan, row};
use super::{State, Writer};
use crate::store::api::{ChangeEvent, StoreVersion};
use crate::store::compose::{
    self, COPY_PENDING, COPY_STARTED, QUEUED, SENDING, SENT, UNCERTAIN, failure_code, kind_code,
    state_from,
};
use crate::store::compose_api::{
    CopyFolder, DRAFT_QUIET_MS, DeletionId, DeletionTarget, Discarded, DraftDeletion, DraftSaved,
    DraftUpload, DueDrafts, DueSend, DueSends, DueSentCopy, OutboxChange, Prepared, Resolution,
    SaveDraft, SendFailed, SendFailure, SendQueued, SendRecorded, SendStarted, SendState,
    SendUndone, SentCopyDone, ServerCopy, UploadRecorded,
};
use crate::store::ops_api::Failure;
use crate::store::overlay::{BATCH_COMPOSE, Change, EXPUNGE};
use crate::store::{StoreError, codec};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::model::{AccountId, BlobKey, DraftId, Flags, FolderId, MessageId};
use std::collections::BTreeSet;

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

pub(crate) enum ComposeJob {
    Save {
        save: Box<SaveDraft>,
        reply: Sender<DraftSaved>,
    },
    SetOpen {
        id: DraftId,
        open: bool,
        reply: Sender<()>,
    },
    Discard {
        id: DraftId,
        reply: Sender<Discarded>,
    },
    Queue {
        id: DraftId,
        not_before: i64,
        reply: Sender<SendQueued>,
    },
    Undo {
        id: DraftId,
        reply: Sender<SendUndone>,
    },
    Recover {
        reply: Sender<Vec<OutboxChange>>,
    },
    DueSends {
        account: AccountId,
        now: i64,
        reply: Sender<DueSends>,
    },
    Started {
        draft: DraftId,
        prepared: Box<Prepared>,
        now: i64,
        reply: Sender<SendStarted>,
    },
    Done {
        draft: DraftId,
        now: i64,
        reply: Sender<SendRecorded>,
    },
    Failed {
        draft: DraftId,
        failure: SendFailed,
        reply: Sender<SendRecorded>,
    },
    Uncertain {
        draft: DraftId,
        reply: Sender<SendRecorded>,
    },
    Resolve {
        draft: DraftId,
        resolution: Resolution,
        now: i64,
        reply: Sender<SendRecorded>,
    },
    CopyStarted {
        draft: DraftId,
        folder: FolderId,
        uidvalidity: u32,
        uidnext: u32,
        reply: Sender<SendRecorded>,
    },
    CopyDone {
        draft: DraftId,
        done: SentCopyDone,
        reply: Sender<SendRecorded>,
    },
    CopyFailed {
        draft: DraftId,
        retry_at: i64,
        reply: Sender<SendRecorded>,
    },
    DueDrafts {
        account: AccountId,
        now: i64,
        reply: Sender<DueDrafts>,
    },
    UploadStarted {
        draft: DraftId,
        revision: u64,
        folder: FolderId,
        uidvalidity: u32,
        uidnext: u32,
        reply: Sender<UploadRecorded>,
    },
    UploadDone {
        draft: DraftId,
        revision: u64,
        uid: Option<u32>,
        reply: Sender<UploadRecorded>,
    },
    UploadFailed {
        draft: DraftId,
        failure: Failure,
        reply: Sender<UploadRecorded>,
    },
    DeletionsDone {
        ids: Vec<DeletionId>,
        reply: Sender<()>,
    },
    DeletionFailed {
        id: DeletionId,
        failure: Failure,
        reply: Sender<()>,
    },
}

/// A draft's row as the outbox and its copies read it.
struct DraftRow {
    id: DraftId,
    account: AccountId,
    local: bool,
    message_id: String,
    revision: u64,
    state: Option<i64>,
    copy: Option<ServerCopy>,
    /// An upload under way: its folder, UIDVALIDITY and mark.
    upload: Option<(FolderId, u32, u32)>,
}

fn draft_row(tx: &Connection, id: DraftId) -> rusqlite::Result<Option<DraftRow>> {
    tx.prepare_cached(
        "SELECT d.account_id, a.key IS NULL, d.message_id, d.revision, o.state, d.copy_folder, \
           d.copy_uidvalidity, d.copy_uid, d.copy_mark, d.copy_revision, d.upload_folder, \
           d.upload_uidvalidity, d.upload_mark \
         FROM drafts d JOIN accounts a ON a.id = d.account_id \
         LEFT JOIN outbox o ON o.draft = d.id WHERE d.id = ?1",
    )?
    .query_row([id.0], |row| {
        let copy = match (
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<u32>>(6)?,
            row.get::<_, Option<u32>>(8)?,
        ) {
            (Some(folder), Some(uidvalidity), Some(mark)) => Some(ServerCopy {
                folder: FolderId(folder),
                uidvalidity,
                uid: row.get(7)?,
                mark,
                revision: row.get::<_, i64>(9)? as u64,
            }),
            _ => None,
        };
        let upload = match (
            row.get::<_, Option<i64>>(10)?,
            row.get::<_, Option<u32>>(11)?,
            row.get::<_, Option<u32>>(12)?,
        ) {
            (Some(folder), Some(uidvalidity), Some(mark)) => {
                Some((FolderId(folder), uidvalidity, mark))
            }
            _ => None,
        };
        Ok(DraftRow {
            id,
            account: AccountId(row.get(0)?),
            local: row.get(1)?,
            message_id: row.get(2)?,
            revision: row.get::<_, i64>(3)? as u64,
            state: row.get(4)?,
            copy,
            upload,
        })
    })
    .optional()
}

/// The account's folder with `role` that takes messages, the lowest id first.
fn role_folder(
    tx: &Connection,
    account: AccountId,
    role: &str,
) -> rusqlite::Result<Option<CopyFolder>> {
    tx.prepare_cached(
        "SELECT id, name_raw, uidvalidity FROM folders \
         WHERE account_id = ?1 AND role = ?2 AND selectable = 1 ORDER BY id LIMIT 1",
    )?
    .query_row(params![account.0, role], |row| {
        Ok(CopyFolder {
            folder: FolderId(row.get(0)?),
            name_raw: row.get(1)?,
            uidvalidity: row.get(2)?,
        })
    })
    .optional()
}

/// Server copies of a draft to delete: rows the store holds go to the op queue as expunges
/// (hidden at once), the rest wait in `draft_deletions` for the account's actor.
#[derive(Default)]
struct Deletions {
    expunge: Vec<(super::overlay::Row, Change)>,
    queued: bool,
}

impl Deletions {
    /// Rows of `folder` holding `message_id` at UIDs in `from..below`, but not `keep`.
    fn rows(
        &mut self,
        tx: &Connection,
        folder: FolderId,
        message_id: &str,
        (from, below): (u32, Option<u32>),
        keep: Option<u32>,
    ) -> rusqlite::Result<()> {
        let ids: Vec<MessageId> = tx
            .prepare_cached(
                "SELECT id FROM messages WHERE message_id = ?1 AND folder_id = ?2 AND uid >= ?3 \
                   AND (?4 IS NULL OR uid < ?4) AND (?5 IS NULL OR uid <> ?5)",
            )?
            .query_map(params![message_id, folder.0, from, below, keep], |row| {
                row.get(0).map(MessageId)
            })?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            if let Some(found) = row(tx, id)? {
                self.expunge.push((found, Change::Expunge));
            }
        }
        Ok(())
    }

    fn entry(
        &mut self,
        tx: &Connection,
        account: AccountId,
        folder: FolderId,
        uidvalidity: u32,
        message_id: &str,
        target: (Option<u32>, u32, Option<u32>),
    ) -> rusqlite::Result<()> {
        let (uid, from, below) = target;
        let exists: bool = tx
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1)")?
            .query_row([folder.0], |row| row.get(0))?;
        if !exists {
            return Ok(());
        }
        tx.prepare_cached(
            "INSERT INTO draft_deletions(account_id, folder_id, uidvalidity, uid, message_id, \
               from_uid, below_uid) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            account.0,
            folder.0,
            uidvalidity,
            uid,
            message_id,
            from,
            below
        ])?;
        self.queued = true;
        Ok(())
    }

    /// Every copy of a draft that is discarded, sent or moved to another account: the rows of
    /// its folders with its Message-ID, its recorded copy and an upload under way.
    fn all(&mut self, tx: &Connection, draft: &DraftRow) -> rusqlite::Result<()> {
        if draft.local {
            return Ok(());
        }
        let mut folders = BTreeSet::new();
        if let Some(copy) = draft.copy {
            folders.insert(copy.folder);
            match copy.uid {
                Some(uid) => {
                    if !held_at(tx, copy.folder, copy.uidvalidity, uid)? {
                        self.entry(
                            tx,
                            draft.account,
                            copy.folder,
                            copy.uidvalidity,
                            &draft.message_id,
                            (Some(uid), 1, None),
                        )?;
                    }
                }
                None => self.entry(
                    tx,
                    draft.account,
                    copy.folder,
                    copy.uidvalidity,
                    &draft.message_id,
                    (None, copy.mark, None),
                )?,
            }
        }
        if let Some((folder, uidvalidity, mark)) = draft.upload {
            folders.insert(folder);
            self.entry(
                tx,
                draft.account,
                folder,
                uidvalidity,
                &draft.message_id,
                (None, mark, None),
            )?;
        }
        for folder in folders {
            self.rows(tx, folder, &draft.message_id, (1, None), None)?;
        }
        Ok(())
    }

    /// The copy `old` that a new one, uploaded to `folder` above `mark` as `uid`, replaces.
    fn replaced(
        &mut self,
        tx: &Connection,
        draft: &DraftRow,
        old: Option<ServerCopy>,
        (folder, mark, uid): (FolderId, u32, Option<u32>),
    ) -> rusqlite::Result<()> {
        // Older copies the store holds in the folder, a server draft edited here among them.
        self.rows(tx, folder, &draft.message_id, (1, Some(mark)), uid)?;
        let Some(old) = old else {
            return Ok(());
        };
        let below = (old.folder == folder).then_some(mark);
        if old.folder != folder {
            self.rows(tx, old.folder, &draft.message_id, (1, None), None)?;
        }
        match old.uid {
            Some(old_uid) if old.folder == folder && Some(old_uid) == uid => {}
            Some(old_uid) => {
                if !held_at(tx, old.folder, old.uidvalidity, old_uid)? {
                    self.entry(
                        tx,
                        draft.account,
                        old.folder,
                        old.uidvalidity,
                        &draft.message_id,
                        (Some(old_uid), 1, None),
                    )?;
                }
            }
            None => self.entry(
                tx,
                draft.account,
                old.folder,
                old.uidvalidity,
                &draft.message_id,
                (None, old.mark, below),
            )?,
        }
        Ok(())
    }
}

/// Whether the store holds a row at `uid` of `folder` under `uidvalidity` (its deletion then
/// goes through the op queue).
fn held_at(
    tx: &Connection,
    folder: FolderId,
    uidvalidity: u32,
    uid: u32,
) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id \
         WHERE m.folder_id = ?1 AND m.uid = ?2 AND f.uidvalidity = ?3)",
    )?
    .query_row(params![folder.0, uid, uidvalidity], |row| row.get(0))
}

/// A new row of `folder` that a queued deletion names (its UID, or its Message-ID above a
/// search's mark) gets an expunge op at once, so a stale Drafts copy never shows; a deletion by
/// UID is then the op queue's. Called where new rows take their waiting ops.
pub(crate) fn take_deletion(
    tx: &Connection,
    folder: FolderId,
    id: MessageId,
    uid: u32,
    uidvalidity: u32,
    message_id: Option<&str>,
) -> rusqlite::Result<bool> {
    let found: Option<(i64, i64, Option<u32>)> = tx
        .prepare_cached(
            "SELECT id, account_id, uid FROM draft_deletions \
             WHERE folder_id = ?1 AND uidvalidity = ?2 AND (uid = ?3 OR (uid IS NULL \
               AND message_id = ?4 AND from_uid <= ?3 AND (below_uid IS NULL OR ?3 < below_uid))) \
             ORDER BY id LIMIT 1",
        )?
        .query_row(params![folder.0, uidvalidity, uid, message_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let Some((deletion, account, by_uid)) = found else {
        return Ok(false);
    };
    if by_uid.is_some() {
        tx.prepare_cached("DELETE FROM draft_deletions WHERE id = ?1")?
            .execute([deletion])?;
    }
    let batch: i64 = tx
        .prepare_cached("INSERT INTO op_batches(kind) VALUES (?1) RETURNING id")?
        .query_row([BATCH_COMPOSE], |row| row.get(0))?;
    let flags: u32 = tx
        .prepare_cached("SELECT flags FROM messages WHERE id = ?1")?
        .query_row([id.0], |row| row.get(0))?;
    tx.prepare_cached(
        "INSERT INTO ops(batch, account_id, kind, message, folder_id, uidvalidity, uid, \
           message_id, prior_folder, prior_flags, not_before) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?5, ?9, 0)",
    )?
    .execute(params![
        batch,
        account,
        EXPUNGE,
        id.0,
        folder.0,
        uidvalidity,
        uid,
        message_id,
        flags
    ])?;
    Ok(true)
}

/// Whether `folder` has queued deletions, which new rows must see.
pub(crate) fn has_deletions(tx: &Connection, folder: FolderId) -> rusqlite::Result<bool> {
    tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM draft_deletions WHERE folder_id = ?1)")?
        .query_row([folder.0], |row| row.get(0))
}

fn change(draft: &DraftRow, state: Option<i64>, failure: Option<SendFailure>) -> OutboxChange {
    OutboxChange {
        account: draft.account,
        draft: draft.id,
        state: state.map(state_from),
        failure,
    }
}

fn prepared_blobs(tx: &Connection, draft: DraftId) -> rusqlite::Result<BTreeSet<BlobKey>> {
    let (message, copy): (Option<Vec<u8>>, Option<Vec<u8>>) = tx
        .prepare_cached("SELECT message_blob, sent_copy_blob FROM outbox WHERE draft = ?1")?
        .query_row([draft.0], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?
        .unwrap_or_default();
    Ok([message, copy]
        .into_iter()
        .flatten()
        .filter_map(|key| BlobKey::from_slice(&key))
        .collect())
}

fn read_prepared(tx: &Connection, draft: DraftId) -> rusqlite::Result<Option<Prepared>> {
    Ok(tx
        .prepare_cached(
            "SELECT message_blob, sent_copy_blob, size, envelope_from, envelope_to FROM outbox \
             WHERE draft = ?1 AND message_blob IS NOT NULL",
        )?
        .query_row([draft.0], |row| {
            let key = |index| -> rusqlite::Result<Option<BlobKey>> {
                Ok(row
                    .get::<_, Option<Vec<u8>>>(index)?
                    .and_then(|key| BlobKey::from_slice(&key)))
            };
            let Some(message_blob) = key(0)? else {
                return Ok(None);
            };
            Ok(Some(Prepared {
                message_blob,
                sent_copy_blob: key(1)?,
                size: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                envelope_from: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                envelope_to: codec::decode_words(
                    &row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ),
            }))
        })
        .optional()?
        .flatten())
}

/// Takes a draft out of the outbox, back to an editable draft; returns the built message's
/// blobs nothing references any more.
fn leave_outbox(
    tx: &Connection,
    id: DraftId,
    failure: Option<SendFailure>,
    open: bool,
) -> rusqlite::Result<Vec<BlobKey>> {
    let blobs = prepared_blobs(tx, id)?;
    tx.prepare_cached("DELETE FROM outbox WHERE draft = ?1")?
        .execute([id.0])?;
    tx.prepare_cached("UPDATE drafts SET failure = ?2, open = open OR ?3 WHERE id = ?1")?
        .execute(params![id.0, failure.map(failure_code), open])?;
    compose::unreferenced(tx, blobs)
}

fn not_applied(version: u64, state: Option<i64>) -> SendRecorded {
    SendRecorded {
        version: StoreVersion(version),
        state: state.map(state_from),
        ..SendRecorded::default()
    }
}

impl State {
    /// Commits a transaction with outbox changes and row writes in one event; without either,
    /// commits quietly at the current version.
    fn commit_compose(
        &mut self,
        tx: Transaction<'_>,
        changes: Vec<OutboxChange>,
        plans: &[RowPlan],
    ) -> Result<StoreVersion, StoreError> {
        self.outbox = changes;
        if plans.is_empty() && self.outbox.is_empty() {
            tx.commit()?;
            self.after_commit();
            return Ok(StoreVersion(self.version));
        }
        if plans.is_empty() {
            return self.commit(tx, ChangeEvent::default());
        }
        Ok(self.commit_rows(tx, plans, BTreeSet::new())?.version)
    }
}

impl Writer {
    pub(crate) fn compose_job(&mut self, job: ComposeJob) {
        match job {
            ComposeJob::Save { save, reply } => {
                let _ = reply.try_send(self.save_draft(&save));
            }
            ComposeJob::SetOpen { id, open, reply } => {
                let _ = reply.try_send(self.set_draft_open(id, open));
            }
            ComposeJob::Discard { id, reply } => {
                let _ = reply.try_send(self.discard_draft(id));
            }
            ComposeJob::Queue {
                id,
                not_before,
                reply,
            } => {
                let _ = reply.try_send(self.queue_send(id, not_before));
            }
            ComposeJob::Undo { id, reply } => {
                let _ = reply.try_send(self.undo_send(id));
            }
            ComposeJob::Recover { reply } => {
                let _ = reply.try_send(self.recover_sends());
            }
            ComposeJob::DueSends {
                account,
                now,
                reply,
            } => {
                let _ = reply.try_send(self.due_sends(account, now));
            }
            ComposeJob::Started {
                draft,
                prepared,
                now,
                reply,
            } => {
                let _ = reply.try_send(self.send_started(draft, &prepared, now));
            }
            ComposeJob::Done { draft, now, reply } => {
                let _ = reply.try_send(self.send_done(draft, now, &[SENDING, UNCERTAIN]));
            }
            ComposeJob::Failed {
                draft,
                failure,
                reply,
            } => {
                let _ = reply.try_send(self.send_failed(draft, failure));
            }
            ComposeJob::Uncertain { draft, reply } => {
                let _ = reply.try_send(self.send_uncertain(draft));
            }
            ComposeJob::Resolve {
                draft,
                resolution,
                now,
                reply,
            } => {
                let _ = reply.try_send(self.resolve(draft, resolution, now));
            }
            ComposeJob::CopyStarted {
                draft,
                folder,
                uidvalidity,
                uidnext,
                reply,
            } => {
                let _ = reply.try_send(self.copy_started(draft, folder, uidvalidity, uidnext));
            }
            ComposeJob::CopyDone { draft, done, reply } => {
                let _ = reply.try_send(self.copy_done(draft, done));
            }
            ComposeJob::CopyFailed {
                draft,
                retry_at,
                reply,
            } => {
                let _ = reply.try_send(self.copy_failed(draft, retry_at));
            }
            ComposeJob::DueDrafts {
                account,
                now,
                reply,
            } => {
                let _ = reply.try_send(self.due_drafts(account, now));
            }
            ComposeJob::UploadStarted {
                draft,
                revision,
                folder,
                uidvalidity,
                uidnext,
                reply,
            } => {
                let _ = reply.try_send(self.upload_started(
                    draft,
                    revision,
                    folder,
                    uidvalidity,
                    uidnext,
                ));
            }
            ComposeJob::UploadDone {
                draft,
                revision,
                uid,
                reply,
            } => {
                let _ = reply.try_send(self.upload_done(draft, revision, uid));
            }
            ComposeJob::UploadFailed {
                draft,
                failure,
                reply,
            } => {
                let _ = reply.try_send(self.upload_failed(draft, failure));
            }
            ComposeJob::DeletionsDone { ids, reply } => {
                let _ = reply.try_send(self.deletions_done(&ids));
            }
            ComposeJob::DeletionFailed { id, failure, reply } => {
                let _ = reply.try_send(self.deletion_failed(id, failure));
            }
        }
        // A job that failed half-way publishes none of its outbox changes.
        self.state.outbox.clear();
    }

    fn save_draft(&mut self, save: &SaveDraft) -> Result<DraftSaved, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let draft = &save.draft;
        let source = draft.source.map(|source| source.message.0);
        let source_message_id: Option<String> = match source {
            Some(id) => tx
                .prepare_cached("SELECT message_id FROM messages WHERE id = ?1")?
                .query_row([id], |row| row.get(0))
                .optional()?
                .flatten(),
            None => None,
        };
        let mut deletions = Deletions::default();
        let id = match save.id {
            Some(id) => {
                let Some(old) = draft_row(&tx, id)? else {
                    return Ok(DraftSaved::Gone);
                };
                if let Some(state) = old.state {
                    return Ok(DraftSaved::NotEditable {
                        id,
                        state: state_from(state),
                    });
                }
                if old.account != draft.account {
                    // Its copies belong to the account it leaves.
                    deletions.all(&tx, &old)?;
                    tx.prepare_cached(
                        "UPDATE drafts SET copy_folder = NULL, copy_uidvalidity = NULL, \
                           copy_uid = NULL, copy_mark = NULL, copy_revision = 0, \
                           upload_revision = NULL, upload_folder = NULL, \
                           upload_uidvalidity = NULL, upload_mark = NULL, upload_attempts = 0, \
                           upload_not_before = 0, upload_refused = 0 WHERE id = ?1",
                    )?
                    .execute([id.0])?;
                }
                tx.prepare_cached(
                    "UPDATE drafts SET account_id = ?2, message_id = ?3, revision = revision + 1, \
                       saved = ?4, open = ?5, from_name = ?6, from_addr = ?7, to_addrs = ?8, \
                       cc_addrs = ?9, bcc_addrs = ?10, subject = ?11, html = ?12, \
                       in_reply_to = ?13, refs = ?14, \
                       source_message = (SELECT id FROM messages WHERE id = ?15), \
                       source_message_id = ?16, source_kind = ?17 WHERE id = ?1",
                )?
                .execute(params![
                    id.0,
                    draft.account.0,
                    draft.message_id,
                    save.now,
                    save.open,
                    draft.from.name,
                    draft.from.email,
                    codec::encode_addresses(&draft.to),
                    codec::encode_addresses(&draft.cc),
                    codec::encode_addresses(&draft.bcc),
                    draft.subject,
                    draft.html,
                    draft.in_reply_to,
                    codec::encode_words(&draft.references),
                    source,
                    source_message_id,
                    draft.source.map(|source| kind_code(source.kind)),
                ])?;
                tx.prepare_cached("DELETE FROM draft_parts WHERE draft = ?1")?
                    .execute([id.0])?;
                id
            }
            None => DraftId(
                tx.prepare_cached(
                    "INSERT INTO drafts(account_id, message_id, revision, created, saved, open, \
                       from_name, from_addr, to_addrs, cc_addrs, bcc_addrs, subject, html, \
                       in_reply_to, refs, source_message, source_message_id, source_kind) \
                     VALUES (?1, ?2, 1, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                       (SELECT id FROM messages WHERE id = ?14), ?15, ?16) RETURNING id",
                )?
                .query_row(
                    params![
                        draft.account.0,
                        draft.message_id,
                        save.now,
                        save.open,
                        draft.from.name,
                        draft.from.email,
                        codec::encode_addresses(&draft.to),
                        codec::encode_addresses(&draft.cc),
                        codec::encode_addresses(&draft.bcc),
                        draft.subject,
                        draft.html,
                        draft.in_reply_to,
                        codec::encode_words(&draft.references),
                        source,
                        source_message_id,
                        draft.source.map(|source| kind_code(source.kind)),
                    ],
                    |row| row.get(0),
                )?,
            ),
        };
        {
            let mut part = tx.prepare_cached(
                "INSERT INTO draft_parts(draft, position, name, mime_type, size, blob_key, \
                   content_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut hold = tx.prepare_cached(
                "INSERT OR IGNORE INTO draft_blobs(draft, blob_key) VALUES (?1, ?2)",
            )?;
            for (position, item) in draft.parts.iter().enumerate() {
                part.execute(params![
                    id.0,
                    position as i64,
                    item.name,
                    item.mime_type,
                    item.size as i64,
                    item.blob.0.as_slice(),
                    item.content_id,
                ])?;
                hold.execute(params![id.0, item.blob.0.as_slice()])?;
            }
        }
        let revision: i64 = tx
            .prepare_cached("SELECT revision FROM drafts WHERE id = ?1")?
            .query_row([id.0], |row| row.get(0))?;
        let (plans, _) = queue_in(&tx, BATCH_COMPOSE, std::mem::take(&mut deletions.expunge))?;
        self.state.commit_compose(tx, Vec::new(), &plans)?;
        Ok(DraftSaved::Saved {
            id,
            revision: revision as u64,
        })
    }

    fn set_draft_open(&mut self, id: DraftId, open: bool) -> Result<(), StoreError> {
        let changed = self
            .conn
            .prepare_cached("UPDATE drafts SET open = ?2 WHERE id = ?1 AND open <> ?2")?
            .execute(params![id.0, open])?;
        if changed > 0 {
            self.state.after_commit();
        }
        Ok(())
    }

    fn discard_draft(&mut self, id: DraftId) -> Result<Discarded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(Discarded {
                version: StoreVersion(self.state.version),
                ..Discarded::default()
            });
        };
        if let Some(state) = draft.state.filter(|state| *state != QUEUED) {
            return Ok(Discarded {
                version: StoreVersion(self.state.version),
                found: true,
                refused: Some(state_from(state)),
                account: Some(draft.account),
                ..Discarded::default()
            });
        }
        let mut changes = Vec::new();
        if draft.state.is_some() {
            changes.push(change(&draft, None, None));
        }
        let blobs = compose::blobs_of(&tx, id)?;
        let mut deletions = Deletions::default();
        deletions.all(&tx, &draft)?;
        tx.prepare_cached("DELETE FROM drafts WHERE id = ?1")?
            .execute([id.0])?;
        let orphaned_blobs = compose::unreferenced(&tx, blobs)?;
        let server_copy = deletions.queued || !deletions.expunge.is_empty();
        let (plans, _) = queue_in(&tx, BATCH_COMPOSE, deletions.expunge)?;
        let version = self.state.commit_compose(tx, changes, &plans)?;
        Ok(Discarded {
            version,
            found: true,
            refused: None,
            account: Some(draft.account),
            server_copy,
            orphaned_blobs,
        })
    }

    fn queue_send(&mut self, id: DraftId, not_before: i64) -> Result<SendQueued, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(SendQueued::Gone);
        };
        if let Some(state) = draft.state {
            return Ok(SendQueued::NotEditable(state_from(state)));
        }
        if draft.local {
            return Ok(SendQueued::LocalAccount);
        }
        tx.prepare_cached(
            "INSERT INTO outbox(draft, account_id, state, revision, queued, not_before, \
               message_id) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        )?
        .execute(params![
            id.0,
            draft.account.0,
            QUEUED,
            draft.revision as i64,
            not_before,
            draft.message_id
        ])?;
        tx.prepare_cached("UPDATE drafts SET open = 0, failure = NULL WHERE id = ?1")?
            .execute([id.0])?;
        let version =
            self.state
                .commit_compose(tx, vec![change(&draft, Some(QUEUED), None)], &[])?;
        Ok(SendQueued::Queued {
            account: draft.account,
            revision: draft.revision,
            version,
        })
    }

    fn undo_send(&mut self, id: DraftId) -> Result<SendUndone, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(SendUndone::NotQueued);
        };
        match draft.state {
            None => return Ok(SendUndone::NotQueued),
            Some(QUEUED) => {}
            Some(state) => return Ok(SendUndone::TooLate(state_from(state))),
        }
        let blobs = prepared_blobs(&tx, id)?;
        tx.prepare_cached("DELETE FROM outbox WHERE draft = ?1")?
            .execute([id.0])?;
        tx.prepare_cached("UPDATE drafts SET open = 1 WHERE id = ?1")?
            .execute([id.0])?;
        let orphaned_blobs = compose::unreferenced(&tx, blobs)?;
        let version = self
            .state
            .commit_compose(tx, vec![change(&draft, None, None)], &[])?;
        Ok(SendUndone::Undone {
            account: draft.account,
            version,
            orphaned_blobs,
        })
    }

    fn recover_sends(&mut self) -> Result<Vec<OutboxChange>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changes: Vec<OutboxChange> = tx
            .prepare_cached(
                "UPDATE outbox SET state = ?1 WHERE state = ?2 RETURNING draft, account_id",
            )?
            .query_map(params![UNCERTAIN, SENDING], |row| {
                Ok(OutboxChange {
                    draft: DraftId(row.get(0)?),
                    account: AccountId(row.get(1)?),
                    state: Some(SendState::Uncertain),
                    failure: None,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if !changes.is_empty() {
            tracing::info!(
                sends = changes.len(),
                "sends interrupted by a stop are uncertain"
            );
        }
        self.state.commit_compose(tx, changes.clone(), &[])?;
        Ok(changes)
    }

    fn due_sends(&mut self, account: AccountId, now: i64) -> Result<DueSends, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let due: Vec<(DraftId, i64, u32)> = tx
            .prepare_cached(
                "SELECT draft, revision, attempts FROM outbox \
                 WHERE account_id = ?1 AND state = ?2 AND not_before <= ?3 \
                 ORDER BY not_before, draft",
            )?
            .query_map(params![account.0, QUEUED, now], |row| {
                Ok((DraftId(row.get(0)?), row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut sends = Vec::with_capacity(due.len());
        for (draft, revision, attempts) in due {
            let Some(stored) = compose::read(&tx, draft)? else {
                continue;
            };
            sends.push(DueSend {
                draft,
                account,
                revision: revision as u64,
                message: stored.draft,
                attempts,
                prepared: read_prepared(&tx, draft)?,
            });
        }
        let folder = role_folder(&tx, account, "sent")?;
        let sent_copies: Vec<DueSentCopy> = tx
            .prepare_cached(
                "SELECT draft, coalesce(sent_copy_blob, message_blob), coalesce(message_id, ''), \
                   CASE WHEN copy_state = ?3 THEN copy_mark END, copy_attempts FROM outbox \
                 WHERE account_id = ?1 AND state = ?2 AND copy_not_before <= ?4 \
                   AND message_blob IS NOT NULL ORDER BY draft",
            )?
            .query_map(params![account.0, SENT, COPY_STARTED, now], |row| {
                let Some(blob) = BlobKey::from_slice(&row.get::<_, Vec<u8>>(1)?) else {
                    return Ok(None);
                };
                Ok(Some(DueSentCopy {
                    draft: DraftId(row.get(0)?),
                    account,
                    blob,
                    message_id: row.get(2)?,
                    folder: folder.clone(),
                    retry_from: row.get(3)?,
                    attempts: row.get(4)?,
                }))
            })?
            .filter_map(Result::transpose)
            .collect::<rusqlite::Result<_>>()?;
        let next: Option<i64> = tx
            .prepare_cached(
                "SELECT min(at) FROM (\
                   SELECT min(not_before) AS at FROM outbox \
                     WHERE account_id = ?1 AND state = ?2 AND not_before > ?4 \
                   UNION ALL SELECT min(copy_not_before) FROM outbox \
                     WHERE account_id = ?1 AND state = ?3 AND copy_not_before > ?4)",
            )?
            .query_row(params![account.0, QUEUED, SENT, now], |row| row.get(0))?;
        tx.commit()?;
        Ok(DueSends {
            sends,
            sent_copies,
            next,
        })
    }

    /// The mark before DATA: committed with a full sync, so not even a power loss can make the
    /// send look unstarted after its data went out.
    fn send_started(
        &mut self,
        id: DraftId,
        prepared: &Prepared,
        now: i64,
    ) -> Result<SendStarted, StoreError> {
        self.conn.pragma_update(None, "synchronous", "FULL")?;
        let started = self.start_send(id, prepared, now);
        let reset = self.conn.pragma_update(None, "synchronous", "NORMAL");
        let started = started?;
        reset?;
        Ok(started)
    }

    fn start_send(
        &mut self,
        id: DraftId,
        prepared: &Prepared,
        now: i64,
    ) -> Result<SendStarted, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(SendStarted::NotQueued(None));
        };
        if draft.state != Some(QUEUED) {
            return Ok(SendStarted::NotQueued(draft.state.map(state_from)));
        }
        if let Some(recorded) = read_prepared(&tx, id)?
            && recorded != *prepared
        {
            return Ok(SendStarted::Mismatch);
        }
        tx.prepare_cached(
            "UPDATE outbox SET state = ?2, attempts = attempts + 1, started = ?3, \
               message_blob = ?4, sent_copy_blob = ?5, size = ?6, envelope_from = ?7, \
               envelope_to = ?8 WHERE draft = ?1",
        )?
        .execute(params![
            id.0,
            SENDING,
            now,
            prepared.message_blob.0.as_slice(),
            prepared.sent_copy_blob.as_ref().map(|key| key.0.as_slice()),
            prepared.size as i64,
            prepared.envelope_from,
            codec::encode_words(&prepared.envelope_to),
        ])?;
        self.state
            .commit_compose(tx, vec![change(&draft, Some(SENDING), None)], &[])?;
        Ok(SendStarted::Started)
    }

    /// Records a send the server took (from one of `from`): the Sent copy becomes due, the
    /// source message gets its flag through the op queue and the draft's server copies are
    /// queued for deletion, in one transaction.
    fn send_done(
        &mut self,
        id: DraftId,
        now: i64,
        from: &[i64],
    ) -> Result<SendRecorded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(not_applied(self.state.version, None));
        };
        if !draft.state.is_some_and(|state| from.contains(&state)) {
            return Ok(not_applied(self.state.version, draft.state));
        }
        tx.prepare_cached(
            "UPDATE outbox SET state = ?2, sent = ?3, copy_state = ?4, copy_not_before = 0 \
             WHERE draft = ?1",
        )?
        .execute(params![id.0, SENT, now, COPY_PENDING])?;
        let mut changes = Vec::new();
        let source: Option<(Option<i64>, Option<i64>)> = compose::read(&tx, id)?.map(|stored| {
            (
                stored.draft.source.map(|source| source.message.0),
                stored.draft.source.map(|source| kind_code(source.kind)),
            )
        });
        if let Some((Some(message), Some(kind))) = source
            && let Some(found) = row(&tx, MessageId(message))?
            && found.account == draft.account
        {
            let flag = match compose::kind_from(kind) {
                ReplyKind::Reply | ReplyKind::ReplyAll => Flags::ANSWERED,
                ReplyKind::Forward => Flags::FORWARDED,
            };
            changes.push((
                found,
                Change::Flags {
                    add: flag.bits(),
                    remove: 0,
                },
            ));
        }
        let mut deletions = Deletions::default();
        deletions.all(&tx, &draft)?;
        tx.prepare_cached(
            "UPDATE drafts SET copy_folder = NULL, copy_uidvalidity = NULL, copy_uid = NULL, \
               copy_mark = NULL, upload_revision = NULL, upload_folder = NULL, \
               upload_uidvalidity = NULL, upload_mark = NULL WHERE id = ?1",
        )?
        .execute([id.0])?;
        let copies_to_delete = deletions.queued;
        changes.extend(deletions.expunge);
        let (plans, accounts) = queue_in(&tx, BATCH_COMPOSE, changes)?;
        let version =
            self.state
                .commit_compose(tx, vec![change(&draft, Some(SENT), None)], &plans)?;
        Ok(SendRecorded {
            version,
            applied: true,
            state: Some(SendState::Sent),
            ops_queued: !accounts.is_empty(),
            copies_to_delete,
            orphaned_blobs: Vec::new(),
        })
    }

    fn send_failed(
        &mut self,
        id: DraftId,
        failure: SendFailed,
    ) -> Result<SendRecorded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(not_applied(self.state.version, None));
        };
        if !matches!(draft.state, Some(QUEUED | SENDING)) {
            return Ok(not_applied(self.state.version, draft.state));
        }
        match failure {
            SendFailed::Transient { retry_at } => {
                tx.prepare_cached(
                    "UPDATE outbox SET state = ?2, not_before = ?3 WHERE draft = ?1",
                )?
                .execute(params![id.0, QUEUED, retry_at])?;
                let version =
                    self.state
                        .commit_compose(tx, vec![change(&draft, Some(QUEUED), None)], &[])?;
                Ok(SendRecorded {
                    version,
                    applied: true,
                    state: Some(SendState::Queued),
                    ..SendRecorded::default()
                })
            }
            SendFailed::Permanent(reason) => {
                let orphaned_blobs = leave_outbox(&tx, id, Some(reason), false)?;
                let version =
                    self.state
                        .commit_compose(tx, vec![change(&draft, None, Some(reason))], &[])?;
                Ok(SendRecorded {
                    version,
                    applied: true,
                    orphaned_blobs,
                    ..SendRecorded::default()
                })
            }
        }
    }

    fn send_uncertain(&mut self, id: DraftId) -> Result<SendRecorded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(not_applied(self.state.version, None));
        };
        if draft.state != Some(SENDING) {
            return Ok(not_applied(self.state.version, draft.state));
        }
        tx.prepare_cached("UPDATE outbox SET state = ?2 WHERE draft = ?1")?
            .execute(params![id.0, UNCERTAIN])?;
        let version =
            self.state
                .commit_compose(tx, vec![change(&draft, Some(UNCERTAIN), None)], &[])?;
        Ok(SendRecorded {
            version,
            applied: true,
            state: Some(SendState::Uncertain),
            ..SendRecorded::default()
        })
    }

    fn resolve(
        &mut self,
        id: DraftId,
        resolution: Resolution,
        now: i64,
    ) -> Result<SendRecorded, StoreError> {
        if resolution == Resolution::WasSent {
            return self.send_done(id, now, &[UNCERTAIN]);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(not_applied(self.state.version, None));
        };
        if draft.state != Some(UNCERTAIN) {
            return Ok(not_applied(self.state.version, draft.state));
        }
        match resolution {
            Resolution::SendAgain { not_before } => {
                tx.prepare_cached(
                    "UPDATE outbox SET state = ?2, not_before = ?3 WHERE draft = ?1",
                )?
                .execute(params![id.0, QUEUED, not_before])?;
                let version =
                    self.state
                        .commit_compose(tx, vec![change(&draft, Some(QUEUED), None)], &[])?;
                Ok(SendRecorded {
                    version,
                    applied: true,
                    state: Some(SendState::Queued),
                    ..SendRecorded::default()
                })
            }
            Resolution::BackToDraft | Resolution::WasSent => {
                let orphaned_blobs = leave_outbox(&tx, id, None, true)?;
                let version =
                    self.state
                        .commit_compose(tx, vec![change(&draft, None, None)], &[])?;
                Ok(SendRecorded {
                    version,
                    applied: true,
                    orphaned_blobs,
                    ..SendRecorded::default()
                })
            }
        }
    }

    fn copy_started(
        &mut self,
        id: DraftId,
        folder: FolderId,
        uidvalidity: u32,
        uidnext: u32,
    ) -> Result<SendRecorded, StoreError> {
        let changed = self
            .conn
            .prepare_cached(
                "UPDATE outbox SET copy_state = ?2, copy_attempts = copy_attempts + 1, \
                   copy_mark = CASE WHEN copy_mark IS NOT NULL AND copy_folder = ?3 \
                     AND copy_uidvalidity = ?4 THEN copy_mark ELSE ?5 END, \
                   copy_folder = ?3, copy_uidvalidity = ?4 \
                 WHERE draft = ?1 AND state = ?6",
            )?
            .execute(params![
                id.0,
                COPY_STARTED,
                folder.0,
                uidvalidity,
                uidnext,
                SENT
            ])?;
        if changed > 0 {
            self.state.after_commit();
        }
        Ok(SendRecorded {
            version: StoreVersion(self.state.version),
            applied: changed > 0,
            state: (changed > 0).then_some(SendState::Sent),
            ..SendRecorded::default()
        })
    }

    fn copy_done(&mut self, id: DraftId, done: SentCopyDone) -> Result<SendRecorded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(not_applied(self.state.version, None));
        };
        if draft.state != Some(SENT) {
            return Ok(not_applied(self.state.version, draft.state));
        }
        if done == SentCopyDone::GaveUp {
            tracing::warn!(draft = id.0, "the Sent copy was given up");
        }
        // Its server copies were queued for deletion when the send was recorded, and an upload
        // still under way is deleted when it is recorded.
        let blobs = compose::blobs_of(&tx, id)?;
        tx.prepare_cached("DELETE FROM drafts WHERE id = ?1")?
            .execute([id.0])?;
        let orphaned_blobs = compose::unreferenced(&tx, blobs)?;
        let version = self
            .state
            .commit_compose(tx, vec![change(&draft, None, None)], &[])?;
        Ok(SendRecorded {
            version,
            applied: true,
            orphaned_blobs,
            ..SendRecorded::default()
        })
    }

    fn copy_failed(&mut self, id: DraftId, retry_at: i64) -> Result<SendRecorded, StoreError> {
        let changed = self
            .conn
            .prepare_cached(
                "UPDATE outbox SET copy_not_before = ?2 WHERE draft = ?1 AND state = ?3",
            )?
            .execute(params![id.0, retry_at, SENT])?;
        if changed > 0 {
            self.state.after_commit();
        }
        Ok(SendRecorded {
            version: StoreVersion(self.state.version),
            applied: changed > 0,
            state: (changed > 0).then_some(SendState::Sent),
            ..SendRecorded::default()
        })
    }

    fn due_drafts(&mut self, account: AccountId, now: i64) -> Result<DueDrafts, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let local: Option<bool> = tx
            .prepare_cached("SELECT key IS NULL FROM accounts WHERE id = ?1")?
            .query_row([account.0], |row| row.get(0))
            .optional()?;
        if local != Some(false) {
            return Ok(DueDrafts::default());
        }
        let mut due = DueDrafts::default();
        let mut next: Option<i64> = None;
        if let Some(folder) = role_folder(&tx, account, "drafts")? {
            let waiting: Vec<(DraftId, i64)> = tx
                .prepare_cached(
                    "SELECT id, max(upload_not_before, CASE WHEN open THEN saved + ?2 ELSE 0 END) \
                     FROM drafts d WHERE account_id = ?1 AND revision > copy_revision \
                       AND revision > upload_refused \
                       AND NOT EXISTS (SELECT 1 FROM outbox WHERE draft = d.id) \
                     ORDER BY id",
                )?
                .query_map(params![account.0, DRAFT_QUIET_MS], |row| {
                    Ok((DraftId(row.get(0)?), row.get(1)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (id, at) in waiting {
                if at > now {
                    next = Some(next.map_or(at, |next| next.min(at)));
                    continue;
                }
                let (Some(stored), Some(draft)) = (compose::read(&tx, id)?, draft_row(&tx, id)?)
                else {
                    continue;
                };
                let attempts: u32 = tx
                    .prepare_cached("SELECT upload_attempts FROM drafts WHERE id = ?1")?
                    .query_row([id.0], |row| row.get(0))?;
                let retry_from = draft
                    .upload
                    .filter(|(upload, uidvalidity, _)| {
                        *upload == folder.folder && Some(*uidvalidity) == folder.uidvalidity
                    })
                    .map(|(_, _, mark)| mark);
                due.uploads.push(DraftUpload {
                    draft: id,
                    account,
                    revision: stored.revision,
                    message: stored.draft,
                    folder: folder.clone(),
                    retry_from,
                    attempts,
                    replaces: draft.copy,
                });
            }
        }
        let deletions: Vec<(DraftDeletion, i64)> = tx
            .prepare_cached(
                "SELECT d.id, d.folder_id, f.name_raw, d.uidvalidity, d.uid, d.message_id, \
                   d.from_uid, d.below_uid, d.attempts, d.not_before \
                 FROM draft_deletions d JOIN folders f ON f.id = d.folder_id \
                 WHERE d.account_id = ?1 ORDER BY d.id",
            )?
            .query_map([account.0], |row| {
                let target = match row.get::<_, Option<u32>>(4)? {
                    Some(uid) => DeletionTarget::Uid(uid),
                    None => DeletionTarget::Search {
                        message_id: row.get(5)?,
                        from_uid: row.get(6)?,
                        below_uid: row.get(7)?,
                    },
                };
                Ok((
                    DraftDeletion {
                        id: DeletionId(row.get(0)?),
                        account,
                        folder: FolderId(row.get(1)?),
                        folder_name: row.get(2)?,
                        uidvalidity: row.get(3)?,
                        target,
                        attempts: row.get(8)?,
                    },
                    row.get(9)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (deletion, at) in deletions {
            if at > now {
                next = Some(next.map_or(at, |next| next.min(at)));
            } else {
                due.deletions.push(deletion);
            }
        }
        tx.commit()?;
        due.next = next;
        Ok(due)
    }

    fn upload_started(
        &mut self,
        id: DraftId,
        revision: u64,
        folder: FolderId,
        uidvalidity: u32,
        uidnext: u32,
    ) -> Result<UploadRecorded, StoreError> {
        let changed = self
            .conn
            .prepare_cached(
                "UPDATE drafts SET upload_revision = ?2, upload_attempts = upload_attempts + 1, \
                   upload_mark = CASE WHEN upload_mark IS NOT NULL AND upload_folder = ?3 \
                     AND upload_uidvalidity = ?4 THEN upload_mark ELSE ?5 END, \
                   upload_folder = ?3, upload_uidvalidity = ?4 \
                 WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM outbox WHERE draft = ?1)",
            )?
            .execute(params![
                id.0,
                revision as i64,
                folder.0,
                uidvalidity,
                uidnext
            ])?;
        if changed > 0 {
            self.state.after_commit();
        }
        Ok(UploadRecorded {
            version: StoreVersion(self.state.version),
            applied: changed > 0,
            ops_queued: false,
        })
    }

    fn upload_done(
        &mut self,
        id: DraftId,
        revision: u64,
        uid: Option<u32>,
    ) -> Result<UploadRecorded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(draft) = draft_row(&tx, id)? else {
            return Ok(UploadRecorded {
                version: StoreVersion(self.state.version),
                ..UploadRecorded::default()
            });
        };
        let Some((folder, uidvalidity, mark)) = draft.upload else {
            return Ok(UploadRecorded {
                version: StoreVersion(self.state.version),
                ..UploadRecorded::default()
            });
        };
        let mut deletions = Deletions::default();
        if draft.state == Some(SENT) {
            // Sent while it uploaded: the new copy goes too.
            let dead = DraftRow {
                copy: Some(ServerCopy {
                    folder,
                    uidvalidity,
                    uid,
                    mark,
                    revision,
                }),
                upload: None,
                ..draft
            };
            deletions.all(&tx, &dead)?;
            tx.prepare_cached(
                "UPDATE drafts SET upload_revision = NULL, upload_folder = NULL, \
                   upload_uidvalidity = NULL, upload_mark = NULL, upload_attempts = 0 \
                 WHERE id = ?1",
            )?
            .execute([id.0])?;
        } else {
            deletions.replaced(&tx, &draft, draft.copy, (folder, mark, uid))?;
            tx.prepare_cached(
                "UPDATE drafts SET copy_folder = ?2, copy_uidvalidity = ?3, copy_uid = ?4, \
                   copy_mark = ?5, copy_revision = max(copy_revision, ?6), \
                   upload_revision = NULL, upload_folder = NULL, upload_uidvalidity = NULL, \
                   upload_mark = NULL, upload_attempts = 0, upload_not_before = 0 WHERE id = ?1",
            )?
            .execute(params![
                id.0,
                folder.0,
                uidvalidity,
                uid,
                mark,
                revision as i64
            ])?;
        }
        let (plans, accounts) = queue_in(&tx, BATCH_COMPOSE, deletions.expunge)?;
        let version = self.state.commit_compose(tx, Vec::new(), &plans)?;
        Ok(UploadRecorded {
            version,
            applied: true,
            ops_queued: !accounts.is_empty(),
        })
    }

    fn upload_failed(
        &mut self,
        id: DraftId,
        failure: Failure,
    ) -> Result<UploadRecorded, StoreError> {
        let changed = match failure {
            Failure::Transient { retry_at } => self
                .conn
                .prepare_cached("UPDATE drafts SET upload_not_before = ?2 WHERE id = ?1")?
                .execute(params![id.0, retry_at.unwrap_or(0)])?,
            Failure::Permanent => self
                .conn
                .prepare_cached(
                    "UPDATE drafts SET upload_refused = coalesce(upload_revision, revision), \
                       upload_revision = NULL, upload_folder = NULL, upload_uidvalidity = NULL, \
                       upload_mark = NULL, upload_attempts = 0 WHERE id = ?1",
                )?
                .execute([id.0])?,
        };
        if changed > 0 {
            self.state.after_commit();
        }
        Ok(UploadRecorded {
            version: StoreVersion(self.state.version),
            applied: changed > 0,
            ops_queued: false,
        })
    }

    fn deletions_done(&mut self, ids: &[DeletionId]) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut delete = tx.prepare_cached("DELETE FROM draft_deletions WHERE id = ?1")?;
            for id in ids {
                delete.execute([id.0])?;
            }
        }
        tx.commit()?;
        self.state.after_commit();
        Ok(())
    }

    fn deletion_failed(&mut self, id: DeletionId, failure: Failure) -> Result<(), StoreError> {
        match failure {
            Failure::Transient { retry_at } => self
                .conn
                .prepare_cached(
                    "UPDATE draft_deletions SET attempts = attempts + 1, not_before = ?2 \
                     WHERE id = ?1",
                )?
                .execute(params![id.0, retry_at.unwrap_or(0)])?,
            Failure::Permanent => self
                .conn
                .prepare_cached("DELETE FROM draft_deletions WHERE id = ?1")?
                .execute([id.0])?,
        };
        self.state.after_commit();
        Ok(())
    }
}
