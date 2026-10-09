//! The op queue's writes (ML-051, ML-053, ADR-015): triage queued as one op per message with its
//! overlay, in one transaction; Undo; and what the account's op runner takes and records. Local
//! accounts (no key) and POP3 accounts have no server holding their folders, so their ops are
//! done in the transaction that queues them.

use super::Writer;
use super::overlay::{
    After, Pending, Refusals, Row, RowPlan, chain, change_from_code, defer_blob_deletions,
    rehome_chain, row, row_at, shown, shown_at,
};
use crate::store::StoreError;
use crate::store::api::StoreVersion;
use crate::store::ops_api::{
    BatchId, DueOps, Failure, Lost, NotApplied, OpAction, OpGroup, OpId, OpMessage, OpsDone,
    OpsFailed, OpsRefusal, QueueTriage, Queued, Recorded, RefusedChange, RuleChange, RuleChanges,
    Undone,
};
use crate::store::overlay::{
    self, Action, BATCH_FOLDER_READ, BATCH_RULES, BATCH_TRIAGE, BATCH_UNDO, COPY, Change, EXPUNGE,
    FLAGS, HIDDEN, MOVE, READ_BELOW,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sealwax_domain::model::{AccountId, Flags, FolderId, FolderRole, MessageId};
use sealwax_domain::triage::{self, FolderInfo, Plan, Unavailable};
use std::collections::{BTreeMap, BTreeSet, HashMap};

type Sender<T> = async_channel::Sender<Result<T, StoreError>>;

/// Batches whose done ops are kept for Undo, besides those with ops still pending.
const KEEP_BATCHES: i64 = 50;

pub(crate) enum OpsJob {
    Queue {
        request: QueueTriage,
        reply: Sender<Queued>,
    },
    FolderRead {
        folder: FolderId,
        not_before: i64,
        reply: Sender<Queued>,
    },
    Rules {
        messages: Vec<RuleChanges>,
        not_before: i64,
        reply: Sender<Queued>,
    },
    Undo {
        batch: BatchId,
        reply: Sender<Undone>,
    },
    Due {
        account: AccountId,
        now: i64,
        reply: Sender<DueOps>,
    },
    Start {
        ops: Vec<OpId>,
        target_uidnext: Option<u32>,
        reply: Sender<()>,
    },
    Done {
        done: OpsDone,
        reply: Sender<Recorded>,
    },
    Failed {
        ops: Vec<OpId>,
        failure: Failure,
        reply: Sender<OpsFailed>,
    },
}

/// A new op for one message.
struct NewOp {
    change: Change,
    /// Where the user saw the message, and its flags then: what Undo puts back.
    prior_folder: FolderId,
    prior_flags: u32,
    not_before: i64,
}

/// Ops written in this transaction and the rows whose overlay they change.
#[derive(Default)]
struct Touched {
    /// Rows of server accounts: their overlay is recomputed.
    server: BTreeMap<MessageId, Row>,
    /// Ops of local accounts, done at once, oldest first.
    local: Vec<(OpId, Row, Change)>,
    accounts: BTreeSet<AccountId>,
}

impl Writer {
    pub(crate) fn ops_job(&mut self, job: OpsJob) {
        match job {
            OpsJob::Queue { request, reply } => {
                let _ = reply.try_send(self.queue(&request));
            }
            OpsJob::FolderRead {
                folder,
                not_before,
                reply,
            } => {
                let _ = reply.try_send(self.folder_read(folder, not_before));
            }
            OpsJob::Rules {
                messages,
                not_before,
                reply,
            } => {
                let _ = reply.try_send(self.rules(&messages, not_before));
            }
            OpsJob::Undo { batch, reply } => {
                let _ = reply.try_send(self.undo(batch));
            }
            OpsJob::Due {
                account,
                now,
                reply,
            } => {
                let _ = reply.try_send(self.due(account, now));
            }
            OpsJob::Start {
                ops,
                target_uidnext,
                reply,
            } => {
                let _ = reply.try_send(self.start(&ops, target_uidnext));
            }
            OpsJob::Done { done, reply } => {
                let _ = reply.try_send(self.done(&done));
            }
            OpsJob::Failed {
                ops,
                failure,
                reply,
            } => {
                let _ = reply.try_send(self.failed(&ops, failure));
            }
        }
    }

    fn queue(&mut self, request: &QueueTriage) -> Result<Queued, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut ids = request.messages.clone();
        ids.sort_unstable();
        ids.dedup();
        let mut not_applied: BTreeMap<NotApplied, u32> = BTreeMap::new();
        // By account and by the folder each message is in, or on its way to.
        let mut groups: BTreeMap<(AccountId, FolderId), Vec<Row>> = BTreeMap::new();
        for id in ids {
            let Some(found) = row(&tx, id)? else {
                *not_applied.entry(NotApplied::Gone).or_default() += 1;
                continue;
            };
            let folded = overlay::fold(found.folder, chain(&tx, id)?.iter().map(|op| op.change));
            if folded.expunge {
                *not_applied.entry(NotApplied::Gone).or_default() += 1;
                continue;
            }
            groups
                .entry((found.account, folded.at))
                .or_default()
                .push(found);
        }
        let mut batch = None;
        let mut touched = Touched::default();
        let mut applied = 0u32;
        let mut folders_of: HashMap<AccountId, Vec<FolderInfo>> = HashMap::new();
        for ((account, folder), members) in groups {
            let folders = match folders_of.get(&account) {
                Some(folders) => folders,
                None => folders_of
                    .entry(account)
                    .or_insert(account_folders(&tx, account)?),
            };
            let planned = folders
                .iter()
                .find(|info| info.id == folder)
                .ok_or(Unavailable::NoTarget)
                .and_then(|source| triage::plan(request.action, *source, folders));
            let plan = match planned {
                Ok(plan) => plan,
                Err(reason) => {
                    let reason = match reason {
                        Unavailable::NoTarget => NotApplied::NoTarget,
                        Unavailable::AlreadyThere => NotApplied::AlreadyThere,
                    };
                    *not_applied.entry(reason).or_default() += count(members.len());
                    continue;
                }
            };
            let local = is_local(&tx, account)?;
            for found in members {
                applied += 1;
                let prior = found.flags & !HIDDEN;
                let change = match plan {
                    Plan::Flags { add, remove } => {
                        if (prior | add.bits()) & !remove.bits() == prior {
                            continue;
                        }
                        Change::Flags {
                            add: add.bits(),
                            remove: remove.bits(),
                        }
                    }
                    Plan::Move { to, add, remove } => Change::Move {
                        to,
                        add: add.bits(),
                        remove: remove.bits(),
                    },
                    Plan::Expunge => Change::Expunge,
                };
                let batch = match batch {
                    Some(batch) => batch,
                    None => *batch.insert(new_batch(&tx, BATCH_TRIAGE)?),
                };
                let op = NewOp {
                    change,
                    prior_folder: folder,
                    prior_flags: prior,
                    not_before: request.not_before,
                };
                touched.add(&tx, batch, found, op, local)?;
            }
        }
        let accounts = touched.accounts.iter().copied().collect();
        let version = match batch {
            Some(_) => {
                let plans = touched.plans(&tx)?;
                prune(&tx)?;
                defer_blob_deletions(&tx, &plans)?;
                self.state.commit_rows(tx, &plans, BTreeSet::new())?.version
            }
            None => StoreVersion(self.state.version),
        };
        Ok(Queued {
            version,
            batch: batch.map(BatchId),
            applied,
            not_applied: not_applied.into_iter().collect(),
            accounts,
        })
    }

    fn folder_read(&mut self, folder: FolderId, not_before: i64) -> Result<Queued, StoreError> {
        let account = self.state.account(folder)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let local = is_local(&tx, account)?;
        let (uidvalidity, floor, below): (u32, u32, u32) = tx.query_row(
            "SELECT coalesce(uidvalidity, 0), coalesce(sync_floor, 1), below_unread \
             FROM folders WHERE id = ?1",
            [folder.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let ids: Vec<MessageId> = tx
            .prepare_cached(
                "SELECT id FROM messages INDEXED BY messages_unread \
                 WHERE folder_id = ?1 AND flags & 265 = 0",
            )?
            .query_map([folder.0], |row| row.get(0).map(MessageId))?
            .collect::<rusqlite::Result<_>>()?;
        let below = if local || floor <= 1 { 0 } else { below };
        if ids.is_empty() && below == 0 {
            tx.commit()?;
            return Ok(Queued {
                version: StoreVersion(self.state.version),
                ..Queued::default()
            });
        }
        let batch = new_batch(&tx, BATCH_FOLDER_READ)?;
        let mut touched = Touched::default();
        for id in &ids {
            let Some(found) = row(&tx, *id)? else {
                continue;
            };
            let op = NewOp {
                change: Change::Flags {
                    add: Flags::SEEN.bits(),
                    remove: 0,
                },
                prior_folder: folder,
                prior_flags: found.flags & !HIDDEN,
                not_before,
            };
            touched.add(&tx, batch, found, op, local)?;
        }
        let mut folders = BTreeSet::new();
        if below > 0 {
            tx.prepare_cached(
                "INSERT INTO ops(batch, account_id, kind, folder_id, uidvalidity, uid, \
                   prior_folder, below, not_before) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?4, ?7, ?8)",
            )?
            .execute(params![
                batch,
                account.0,
                READ_BELOW,
                folder.0,
                uidvalidity,
                floor,
                below,
                not_before
            ])?;
            touched.accounts.insert(account);
            folders.insert(folder);
        }
        let accounts = touched.accounts.iter().copied().collect();
        let plans = touched.plans(&tx)?;
        prune(&tx)?;
        let version = self.state.commit_rows(tx, &plans, folders)?.version;
        Ok(Queued {
            version,
            batch: Some(BatchId(batch)),
            applied: count(ids.len()) + below,
            not_applied: Vec::new(),
            accounts,
        })
    }

    fn rules(&mut self, messages: &[RuleChanges], not_before: i64) -> Result<Queued, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut not_applied: BTreeMap<NotApplied, u32> = BTreeMap::new();
        let mut batch = None;
        let mut touched = Touched::default();
        let mut applied = 0u32;
        let mut seen = BTreeSet::new();
        for planned in messages {
            if !seen.insert(planned.message) {
                continue;
            }
            let Some(found) = row(&tx, planned.message)? else {
                *not_applied.entry(NotApplied::Gone).or_default() += 1;
                continue;
            };
            let folded = overlay::fold(
                found.folder,
                chain(&tx, found.id)?.iter().map(|op| op.change),
            );
            if folded.leaves() {
                *not_applied.entry(NotApplied::Gone).or_default() += 1;
                continue;
            }
            let local = is_local(&tx, found.account)?;
            let mut changes = Vec::new();
            for change in &planned.changes {
                let change = match *change {
                    RuleChange::Flags { add, remove } => Change::Flags {
                        add: add.bits(),
                        remove: remove.bits(),
                    },
                    RuleChange::Move { to, add, remove } => Change::Move {
                        to,
                        add: add.bits(),
                        remove: remove.bits(),
                    },
                    RuleChange::Copy { to } => Change::Copy { to },
                };
                let target = match change {
                    Change::Move { to, .. } | Change::Copy { to } => Some(to),
                    _ => None,
                };
                if let Some(to) = target
                    && (to == found.folder || !takes_messages(&tx, found.account, to)?)
                {
                    *not_applied.entry(NotApplied::NoTarget).or_default() += 1;
                    continue;
                }
                if local && matches!(change, Change::Copy { .. }) {
                    *not_applied.entry(NotApplied::NoTarget).or_default() += 1;
                    continue;
                }
                changes.push(change);
            }
            let prior = found.flags & !HIDDEN;
            changes.retain(|change| match change {
                Change::Flags { add, remove } => (prior | add) & !remove != prior,
                _ => true,
            });
            if changes.is_empty() {
                continue;
            }
            applied += 1;
            let batch = match batch {
                Some(batch) => batch,
                None => *batch.insert(new_batch(&tx, BATCH_RULES)?),
            };
            let mut at = found.folder;
            for change in changes {
                let op = NewOp {
                    change,
                    prior_folder: at,
                    prior_flags: prior,
                    not_before,
                };
                if let Change::Move { to, .. } = change {
                    at = to;
                }
                touched.add(&tx, batch, found.clone(), op, local)?;
            }
        }
        let accounts = touched.accounts.iter().copied().collect();
        let version = match batch {
            Some(_) => {
                let plans = touched.plans(&tx)?;
                prune(&tx)?;
                defer_blob_deletions(&tx, &plans)?;
                self.state.commit_rows(tx, &plans, BTreeSet::new())?.version
            }
            None => {
                tx.commit()?;
                StoreVersion(self.state.version)
            }
        };
        Ok(Queued {
            version,
            batch: batch.map(BatchId),
            applied,
            not_applied: not_applied.into_iter().collect(),
            accounts,
        })
    }

    fn undo(&mut self, batch: BatchId) -> Result<Undone, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let undone: Option<bool> = tx
            .query_row(
                "SELECT undone FROM op_batches WHERE id = ?1",
                [batch.0],
                |row| row.get(0),
            )
            .optional()?;
        if undone != Some(false) {
            tx.commit()?;
            return Ok(Undone {
                version: StoreVersion(self.state.version),
                ..Undone::default()
            });
        }
        tx.execute("UPDATE op_batches SET undone = 1 WHERE id = ?1", [batch.0])?;
        let ops = batch_ops(&tx, batch)?;
        let mut lost: BTreeMap<Lost, u32> = BTreeMap::new();
        let (mut restored, mut reverted) = (0u32, 0u32);
        let mut touched = Touched::default();
        let mut folders = BTreeSet::new();
        let mut inverse_batch = None;
        for op in ops {
            if op.kind == READ_BELOW {
                if !op.done && op.attempts == 0 {
                    tx.execute("DELETE FROM ops WHERE id = ?1", [op.id.0])?;
                    folders.insert(op.folder);
                    restored += op.below;
                } else {
                    *lost.entry(Lost::ReadBelow).or_default() += op.below;
                }
                continue;
            }
            if !op.done && op.attempts == 0 {
                tx.execute("DELETE FROM ops WHERE id = ?1", [op.id.0])?;
                if let Some(found) = op.message.map(|id| row(&tx, id)).transpose()?.flatten() {
                    touched.server.insert(found.id, found);
                }
                restored += 1;
                continue;
            }
            if op.kind == COPY {
                // A copy made stays; the message it was made of is where it was.
                continue;
            }
            let (found, inverse) = match undo_target(&tx, &op)? {
                Ok(target) => target,
                Err(reason) => {
                    *lost.entry(reason).or_default() += 1;
                    continue;
                }
            };
            reverted += 1;
            if matches!(inverse, Change::Flags { add: 0, remove: 0 }) {
                continue;
            }
            let at = overlay::fold(
                found.folder,
                chain(&tx, found.id)?.iter().map(|op| op.change),
            )
            .at;
            let local = is_local(&tx, found.account)?;
            let inverse_batch = match inverse_batch {
                Some(batch) => batch,
                None => *inverse_batch.insert(new_batch(&tx, BATCH_UNDO)?),
            };
            let new = NewOp {
                change: inverse,
                prior_folder: at,
                prior_flags: found.flags & !HIDDEN,
                not_before: 0,
            };
            touched.add(&tx, inverse_batch, found, new, local)?;
        }
        let accounts = touched.accounts.iter().copied().collect();
        let plans = touched.plans(&tx)?;
        let version = self.state.commit_rows(tx, &plans, folders)?.version;
        Ok(Undone {
            version,
            restored,
            reverted,
            lost: lost.into_iter().collect(),
            accounts,
        })
    }

    fn due(&mut self, account: AccountId, now: i64) -> Result<DueOps, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let chains = pending_chains(&tx, account)?;
        type Key = (FolderId, Option<Action>, bool, Option<u32>);
        let mut groups: BTreeMap<Key, OpGroup> = BTreeMap::new();
        let mut trivial = Vec::new();
        for (message, chain) in chains {
            let retry = chain[0].0.attempts > 0;
            let run: Vec<&(Pending, Option<u32>)> = if retry {
                chain.iter().take_while(|(op, _)| op.attempts > 0).collect()
            } else {
                chain
                    .iter()
                    .take_while(|(op, _)| op.attempts == 0 && op.not_before <= now)
                    .collect()
            };
            if run.is_empty() || run.iter().any(|(op, _)| op.not_before > now) {
                continue;
            }
            // A copy runs on its own: the changes before it first, then it, then the rest.
            let run = match run
                .iter()
                .position(|(op, _)| matches!(op.change, Change::Copy { .. }))
            {
                Some(0) => run[..1].to_vec(),
                Some(at) => run[..at].to_vec(),
                None => run,
            };
            let Some(found) = row(&tx, message)? else {
                continue;
            };
            let folded = overlay::fold(found.folder, run.iter().map(|(op, _)| op.change));
            let ops: Vec<OpId> = run.iter().map(|(op, _)| op.id).collect();
            let action = match run[0].0.change {
                Change::Copy { to } => Some(Action::Copy { to }),
                _ => folded.action(),
            };
            let Some(action) = action else {
                trivial.push((found, ops));
                continue;
            };
            let uidnext = run.iter().filter_map(|(_, uidnext)| *uidnext).min();
            let group = groups
                .entry((found.folder, Some(action), retry, None))
                .or_insert_with(|| empty_group(account, found.folder, retry));
            group.ops.extend(&ops);
            group.attempts = group
                .attempts
                .max(run.iter().map(|(op, _)| op.attempts).max().unwrap_or(0));
            group.target_uidnext = match (group.target_uidnext, uidnext) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            group.action = match action {
                Action::Flags { add, remove } => OpAction::Flags {
                    add: Flags::from_bits(add),
                    remove: Flags::from_bits(remove),
                },
                Action::Move { to, add, remove } => OpAction::Move {
                    to,
                    to_name: String::new(),
                    to_uidvalidity: None,
                    add: Flags::from_bits(add),
                    remove: Flags::from_bits(remove),
                },
                Action::Expunge => OpAction::Expunge,
                Action::Copy { to } => OpAction::Copy {
                    to,
                    to_name: String::new(),
                    to_uidvalidity: None,
                },
            };
            group.messages.push(OpMessage {
                message,
                uid: found.uid,
                message_id: found.message_id.clone(),
                ops,
            });
        }
        for read in reads_due(&tx, account)? {
            if read.not_before > now {
                continue;
            }
            let retry = read.attempts > 0;
            let mut group = empty_group(account, read.folder, retry);
            group.action = OpAction::ReadBelow { floor: read.floor };
            group.ops.push(read.id);
            group.attempts = read.attempts;
            groups.insert((read.folder, None, retry, Some(read.floor)), group);
        }
        let mut groups: Vec<OpGroup> = groups.into_values().collect();
        for group in &mut groups {
            let (name, uidvalidity) = folder_wire(&tx, group.folder)?;
            group.folder_name = name;
            group.uidvalidity = uidvalidity;
            if let OpAction::Move {
                to,
                to_name,
                to_uidvalidity,
                ..
            }
            | OpAction::Copy {
                to,
                to_name,
                to_uidvalidity,
            } = &mut group.action
            {
                (*to_name, *to_uidvalidity) = folder_wire(&tx, *to)?;
            }
            group.messages.sort_by_key(|message| message.uid);
        }
        let next: Option<i64> = tx
            .prepare_cached(
                "SELECT min(not_before) FROM ops WHERE account_id = ?1 AND done = 0 \
                   AND not_before > ?2 AND (message IS NOT NULL OR kind = 3)",
            )?
            .query_row(params![account.0, now], |row| row.get(0))?;
        let refused = take_refusals(&tx, account)?;
        let mut plans = Vec::new();
        {
            let mut done = tx.prepare_cached("UPDATE ops SET done = 1 WHERE id = ?1")?;
            for (found, ops) in trivial {
                for op in &ops {
                    done.execute([op.0])?;
                }
                let flags = shown(&tx, &found, found.server)?;
                plans.push(RowPlan {
                    after: After::Stay {
                        flags,
                        server: found.server,
                    },
                    row: found,
                });
            }
        }
        self.state.commit_rows(tx, &plans, BTreeSet::new())?;
        Ok(DueOps {
            groups,
            next,
            refused,
        })
    }

    fn start(&mut self, ops: &[OpId], target_uidnext: Option<u32>) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut start = tx.prepare_cached(
                "UPDATE ops SET attempts = attempts + 1, \
                   target_uidnext = coalesce(target_uidnext, ?2) WHERE id = ?1 AND done = 0",
            )?;
            for op in ops {
                start.execute(params![op.0, target_uidnext])?;
            }
        }
        tx.commit()?;
        self.state.after_commit();
        Ok(())
    }

    fn done(&mut self, done: &OpsDone) -> Result<Recorded, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (by_message, reads) = given(&tx, &done.ops)?;
        let mut plans = Vec::new();
        let mut folders = BTreeSet::new();
        let mut unplaced = BTreeSet::new();
        let mut refusals = Refusals::default();
        for (message, ids) in by_message {
            let full = chain(&tx, message)?;
            let (run, rest): (Vec<Pending>, Vec<Pending>) =
                full.into_iter().partition(|op| ids.contains(&op.id));
            mark_done(&tx, &run)?;
            let Some(found) = row(&tx, message)? else {
                continue;
            };
            let folded = overlay::fold(found.folder, run.iter().map(|op| op.change));
            let server = folded.apply(found.server);
            if folded.expunge {
                mark_done(&tx, &rest)?;
                let after = if done.flagged_only {
                    let server = server | Flags::DELETED.bits();
                    After::Stay {
                        flags: shown(&tx, &found, server)?,
                        server,
                    }
                } else {
                    After::Deleted
                };
                plans.push(RowPlan { row: found, after });
                continue;
            }
            if folded.at == found.folder {
                plans.push(RowPlan {
                    after: After::Stay {
                        flags: shown(&tx, &found, server)?,
                        server,
                    },
                    row: found,
                });
                continue;
            }
            let target = folded.at;
            let uidvalidity = target_uidvalidity(&tx, target)?
                .filter(|stored| done.target_uidvalidity.is_none_or(|given| given == *stored));
            let new_uid = done
                .new_uids
                .iter()
                .find(|(old, _)| *old == found.uid)
                .map(|(_, new)| *new);
            let (Some(uidvalidity), Some(uid)) = (uidvalidity, new_uid) else {
                // Where it went is unknown: the target's sync fetches it.
                unplaced.insert(target);
                refusals.refuse(&tx, found.account, target, &rest)?;
                plans.push(RowPlan {
                    row: found,
                    after: After::Deleted,
                });
                continue;
            };
            {
                let mut record =
                    tx.prepare_cached("UPDATE ops SET new_uid = ?2 WHERE id = ?1 AND kind = 1")?;
                for op in &run {
                    record.execute(params![op.id.0, uid])?;
                }
            }
            match row_at(&tx, target, uid)? {
                Some(copy) => {
                    // The target's sync stored it first.
                    rehome_chain(&tx, message, copy.id, target, uid, uidvalidity)?;
                    let mut point =
                        tx.prepare_cached("UPDATE ops SET message = ?2 WHERE id = ?1")?;
                    for op in &run {
                        point.execute(params![op.id.0, copy.id.0])?;
                    }
                    let flags = shown(&tx, &copy, copy.server)?;
                    plans.push(RowPlan {
                        row: found,
                        after: After::Deleted,
                    });
                    plans.push(RowPlan {
                        after: After::Stay {
                            flags,
                            server: copy.server,
                        },
                        row: copy,
                    });
                }
                None => {
                    rehome_chain(&tx, message, message, target, uid, uidvalidity)?;
                    let flags = shown_at(&tx, message, target, uidvalidity, uid, server)?;
                    plans.push(RowPlan {
                        row: found,
                        after: After::Moved {
                            folder: target,
                            uid,
                            flags,
                            server,
                        },
                    });
                }
            }
        }
        for read in reads {
            let (folder, floor) = (read.folder, read.floor);
            tx.execute("UPDATE ops SET done = 1 WHERE id = ?1", [read.id.0])?;
            let changed = tx.execute(
                "UPDATE folders SET below_unread = 0 WHERE id = ?1 AND uidvalidity = ?2",
                params![folder.0, read.uidvalidity],
            )?;
            if changed > 0 {
                folders.insert(folder);
                // Rows loaded from below the floor since are read on the server now too.
                let below: Vec<MessageId> = tx
                    .prepare_cached("SELECT id FROM messages WHERE folder_id = ?1 AND uid < ?2")?
                    .query_map(params![folder.0, floor], |row| row.get(0).map(MessageId))?
                    .collect::<rusqlite::Result<_>>()?;
                for id in below {
                    if plans.iter().any(|plan: &RowPlan| plan.row.id == id) {
                        continue;
                    }
                    if let Some(found) = row(&tx, id)? {
                        let server = found.server | Flags::SEEN.bits();
                        plans.push(RowPlan {
                            after: After::Stay {
                                flags: shown(&tx, &found, server)?,
                                server,
                            },
                            row: found,
                        });
                    }
                }
            }
        }
        refusals.record(&tx)?;
        let committed = self.state.commit_rows(tx, &plans, folders)?;
        Ok(Recorded {
            version: committed.version,
            orphaned_blobs: committed.orphaned_blobs,
            unplaced: unplaced.into_iter().collect(),
        })
    }

    fn failed(&mut self, ops: &[OpId], failure: Failure) -> Result<OpsFailed, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Failure::Transient { retry_at } = failure {
            if let Some(at) = retry_at {
                let mut later = tx.prepare_cached(
                    "UPDATE ops SET not_before = max(not_before, ?2) WHERE id = ?1 AND done = 0",
                )?;
                for op in ops {
                    later.execute(params![op.0, at])?;
                }
            }
            tx.commit()?;
            self.state.after_commit();
            return Ok(OpsFailed {
                version: StoreVersion(self.state.version),
                refused: Vec::new(),
            });
        }
        let (by_message, reads) = given(&tx, ops)?;
        let mut refusals = Refusals::default();
        let mut plans = Vec::new();
        let mut folders = BTreeSet::new();
        for (message, ids) in by_message {
            let Some(found) = row(&tx, message)? else {
                continue;
            };
            let run: Vec<Pending> = chain(&tx, message)?
                .into_iter()
                .filter(|op| ids.contains(&op.id))
                .collect();
            refusals.refuse(&tx, found.account, found.folder, &run)?;
            plans.push(RowPlan {
                after: After::Stay {
                    flags: shown(&tx, &found, found.server)?,
                    server: found.server,
                },
                row: found,
            });
        }
        for read in reads {
            tx.execute("DELETE FROM ops WHERE id = ?1", [read.id.0])?;
            refusals.add(read.account, RefusedChange::FolderRead, read.below);
            folders.insert(read.folder);
        }
        let version = self.state.commit_rows(tx, &plans, folders)?.version;
        Ok(OpsFailed {
            version,
            refused: refusals.into_vec(),
        })
    }
}

impl Touched {
    /// Writes an op for `found`; a local account's op is done at once (in `plans`).
    fn add(
        &mut self,
        tx: &Connection,
        batch: i64,
        found: Row,
        op: NewOp,
        local: bool,
    ) -> rusqlite::Result<()> {
        let (kind, target, add, remove) = match op.change {
            Change::Flags { add, remove } => (FLAGS, None, add, remove),
            Change::Move { to, add, remove } => (MOVE, Some(to.0), add, remove),
            Change::Expunge => (EXPUNGE, None, 0, 0),
            Change::Copy { to } => (COPY, Some(to.0), 0, 0),
        };
        let id = OpId(
            tx.prepare_cached(
                "INSERT INTO ops(batch, account_id, kind, message, folder_id, uidvalidity, uid, \
                   message_id, add_flags, remove_flags, target, prior_folder, prior_flags, \
                   not_before) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) RETURNING id",
            )?
            .query_row(
                params![
                    batch,
                    found.account.0,
                    kind,
                    found.id.0,
                    found.folder.0,
                    found.uidvalidity,
                    found.uid,
                    found.message_id,
                    add,
                    remove,
                    target,
                    op.prior_folder.0,
                    op.prior_flags,
                    op.not_before,
                ],
                |row| row.get(0),
            )?,
        );
        if local {
            self.local.push((id, found, op.change));
        } else {
            self.accounts.insert(found.account);
            self.server.insert(found.id, found);
        }
        Ok(())
    }

    /// The row writes: server rows show their new overlay; local ops are done now, a move
    /// re-homing its row under the target's next UID and an expunge removing it.
    fn plans(self, tx: &Connection) -> rusqlite::Result<Vec<RowPlan>> {
        let mut plans = Vec::with_capacity(self.server.len() + self.local.len());
        for found in self.server.into_values() {
            let flags = shown(tx, &found, found.server)?;
            plans.push(RowPlan {
                after: After::Stay {
                    flags,
                    server: found.server,
                },
                row: found,
            });
        }
        let mut next_uid: HashMap<FolderId, u32> = HashMap::new();
        let mut done = tx.prepare_cached("UPDATE ops SET done = 1, new_uid = ?2 WHERE id = ?1")?;
        for (op, found, change) in self.local {
            let flags = found.flags & !HIDDEN;
            let after = match change {
                Change::Flags { add, remove } => {
                    let flags = (flags | add) & !remove;
                    done.execute(params![op.0, None::<u32>])?;
                    After::Stay {
                        flags,
                        server: flags,
                    }
                }
                Change::Move { to, add, remove } => {
                    let uid = match next_uid.get_mut(&to) {
                        Some(uid) => uid,
                        None => next_uid.entry(to).or_insert(
                            tx.prepare_cached(
                                "SELECT coalesce(max(uid), 0) + 1 FROM messages WHERE folder_id = ?1",
                            )?
                            .query_row([to.0], |row| row.get(0))?,
                        ),
                    };
                    let new_uid = *uid;
                    *uid += 1;
                    done.execute(params![op.0, new_uid])?;
                    let flags = (flags | add) & !remove;
                    After::Moved {
                        folder: to,
                        uid: new_uid,
                        flags,
                        server: flags,
                    }
                }
                Change::Expunge => {
                    done.execute(params![op.0, None::<u32>])?;
                    After::Deleted
                }
                // Never queued for a local account (`rules` leaves them to the caller).
                Change::Copy { .. } => {
                    done.execute(params![op.0, None::<u32>])?;
                    After::Stay {
                        flags,
                        server: flags,
                    }
                }
            };
            plans.push(RowPlan { row: found, after });
        }
        Ok(plans)
    }
}

/// Queues `changes` for their rows in `tx` as one batch of `kind`, due at once: what the outbox
/// queues for a sent message's source (`\Answered`, `$Forwarded`) and for Drafts-folder copies
/// to expunge. A row whose pending ops already take it away, and a flag change that changes
/// nothing shown, get no op. Returns the row writes for `commit_rows` and the server accounts
/// whose op runner has work.
pub(crate) fn queue_in(
    tx: &Connection,
    kind: i64,
    changes: Vec<(Row, Change)>,
) -> rusqlite::Result<(Vec<RowPlan>, BTreeSet<AccountId>)> {
    let mut touched = Touched::default();
    let mut batch = None;
    let mut seen = BTreeSet::new();
    for (found, change) in changes {
        if !seen.insert(found.id) {
            continue;
        }
        let folded = overlay::fold(
            found.folder,
            chain(tx, found.id)?.iter().map(|op| op.change),
        );
        if folded.expunge {
            continue;
        }
        let prior = found.flags & !HIDDEN;
        if let Change::Flags { add, remove } = change
            && (prior | add) & !remove == prior
        {
            continue;
        }
        let batch = match batch {
            Some(batch) => batch,
            None => *batch.insert(new_batch(tx, kind)?),
        };
        let local = is_local(tx, found.account)?;
        let op = NewOp {
            change,
            prior_folder: folded.at,
            prior_flags: prior,
            not_before: 0,
        };
        touched.add(tx, batch, found, op, local)?;
    }
    let accounts = touched.accounts.clone();
    let plans = touched.plans(tx)?;
    Ok((plans, accounts))
}

/// An op of a batch, as Undo reads it.
struct BatchOp {
    id: OpId,
    kind: i64,
    done: bool,
    message: Option<MessageId>,
    folder: FolderId,
    message_id: Option<String>,
    add: u32,
    remove: u32,
    target: Option<i64>,
    prior_folder: FolderId,
    prior_flags: u32,
    below: u32,
    attempts: u32,
    target_uidnext: Option<u32>,
}

fn batch_ops(tx: &Connection, batch: BatchId) -> rusqlite::Result<Vec<BatchOp>> {
    tx.prepare_cached(
        "SELECT id, kind, done, message, folder_id, message_id, add_flags, remove_flags, target, \
           prior_folder, prior_flags, below, attempts, target_uidnext \
         FROM ops WHERE batch = ?1 ORDER BY id",
    )?
    .query_map([batch.0], |row| {
        Ok(BatchOp {
            id: OpId(row.get(0)?),
            kind: row.get(1)?,
            done: row.get(2)?,
            message: row.get::<_, Option<i64>>(3)?.map(MessageId),
            folder: FolderId(row.get(4)?),
            message_id: row.get(5)?,
            add: row.get(6)?,
            remove: row.get(7)?,
            target: row.get(8)?,
            prior_folder: FolderId(row.get(9)?),
            prior_flags: row.get(10)?,
            below: row.get(11)?,
            attempts: row.get(12)?,
            target_uidnext: row.get(13)?,
        })
    })?
    .collect()
}

/// The row an op that started or is done applies to now, and the change that puts it back as
/// it was when the op was queued; or why it can't come back.
fn undo_target(tx: &Connection, op: &BatchOp) -> rusqlite::Result<Result<(Row, Change), Lost>> {
    let Some(change) = Change::from_row(op.kind, op.target, op.add, op.remove) else {
        return Ok(Err(Lost::Unknown));
    };
    let found = op.message.map(|id| row(tx, id)).transpose()?.flatten();
    let Some(inverse) = overlay::inverse(change, op.prior_flags, op.prior_folder) else {
        // Without UIDPLUS an expunge only flagged the message `\Deleted`, and it is still here.
        return Ok(match found {
            Some(found) if op.done && found.server & Flags::DELETED.bits() != 0 => Ok((
                found,
                Change::Flags {
                    add: 0,
                    remove: Flags::DELETED.bits(),
                },
            )),
            _ => Err(Lost::Deleted),
        });
    };
    let found = match (found, change, &op.message_id) {
        (Some(found), ..) => Some(found),
        (None, Change::Move { to, .. }, Some(message_id)) if op.done => {
            copy_in(tx, to, message_id, op.target_uidnext)?
        }
        _ => None,
    };
    let back = match inverse {
        Change::Move { to, .. } => folder_exists(tx, to)?,
        _ => true,
    };
    Ok(match found {
        Some(found) if back => Ok((found, inverse)),
        _ => Err(Lost::Unknown),
    })
}

/// The copy of a moved message the target's sync stored, found by its Message-ID at or above
/// the target's UIDNEXT recorded before the move.
fn copy_in(
    tx: &Connection,
    folder: FolderId,
    message_id: &str,
    from_uid: Option<u32>,
) -> rusqlite::Result<Option<Row>> {
    let id: Option<i64> = tx
        .prepare_cached(
            "SELECT id FROM messages WHERE message_id = ?1 AND folder_id = ?2 AND uid >= ?3 \
             ORDER BY uid LIMIT 1",
        )?
        .query_row(
            params![message_id, folder.0, from_uid.unwrap_or(0)],
            |row| row.get(0),
        )
        .optional()?;
    id.map(|id| row(tx, MessageId(id)))
        .transpose()
        .map(Option::flatten)
}

/// A message's pending ops, oldest first, each with the target UIDNEXT recorded for it.
type Chain = Vec<(Pending, Option<u32>)>;

/// The pending ops of an account's messages by message.
fn pending_chains(
    tx: &Connection,
    account: AccountId,
) -> rusqlite::Result<Vec<(MessageId, Chain)>> {
    let mut select = tx.prepare_cached(
        "SELECT message, id, kind, target, add_flags, remove_flags, batch, attempts, not_before, \
           target_uidnext FROM ops \
         WHERE account_id = ?1 AND done = 0 AND message IS NOT NULL ORDER BY message, id",
    )?;
    let rows = select.query_map([account.0], |row| {
        Ok((
            MessageId(row.get(0)?),
            OpId(row.get(1)?),
            Change::from_row(row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?),
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
        ))
    })?;
    let mut chains: Vec<(MessageId, Chain)> = Vec::new();
    for row in rows {
        let (message, id, change, batch, attempts, not_before, uidnext) = row?;
        let Some(change) = change else {
            continue;
        };
        let op = Pending {
            id,
            change,
            batch,
            attempts,
            not_before,
        };
        match chains.last_mut() {
            Some((last, ops)) if *last == message => ops.push((op, uidnext)),
            _ => chains.push((message, vec![(op, uidnext)])),
        }
    }
    Ok(chains)
}

/// A pending read below a folder's floor (a folder marked read).
struct ReadOp {
    id: OpId,
    account: AccountId,
    folder: FolderId,
    uidvalidity: u32,
    floor: u32,
    /// The server's unread below the floor when it was queued.
    below: u32,
    attempts: u32,
    not_before: i64,
}

const READ_COLUMNS: &str =
    "id, account_id, folder_id, uidvalidity, uid, below, attempts, not_before";

fn read_op(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReadOp> {
    Ok(ReadOp {
        id: OpId(row.get(0)?),
        account: AccountId(row.get(1)?),
        folder: FolderId(row.get(2)?),
        uidvalidity: row.get(3)?,
        floor: row.get(4)?,
        below: row.get(5)?,
        attempts: row.get(6)?,
        not_before: row.get(7)?,
    })
}

/// The account's pending folder-read ops.
fn reads_due(tx: &Connection, account: AccountId) -> rusqlite::Result<Vec<ReadOp>> {
    tx.prepare_cached(&format!(
        "SELECT {READ_COLUMNS} FROM ops WHERE account_id = ?1 AND done = 0 AND kind = 3 \
         ORDER BY id"
    ))?
    .query_map([account.0], read_op)?
    .collect()
}

/// Ops by the message they apply to.
type ByMessage = BTreeMap<MessageId, BTreeSet<OpId>>;

/// The pending ops among `ops`: per message, and the folder-read ops. Ops waiting for their
/// message are left alone.
fn given(tx: &Connection, ops: &[OpId]) -> rusqlite::Result<(ByMessage, Vec<ReadOp>)> {
    let mut by_message = ByMessage::new();
    let mut reads = Vec::new();
    let mut message =
        tx.prepare_cached("SELECT message FROM ops WHERE id = ?1 AND done = 0 AND kind <> 3")?;
    let mut read = tx.prepare_cached(&format!(
        "SELECT {READ_COLUMNS} FROM ops WHERE id = ?1 AND done = 0 AND kind = 3"
    ))?;
    let mut seen = BTreeSet::new();
    for op in ops {
        if !seen.insert(*op) {
            continue;
        }
        let attached: Option<Option<i64>> =
            message.query_row([op.0], |row| row.get(0)).optional()?;
        match attached {
            Some(Some(id)) => {
                by_message.entry(MessageId(id)).or_default().insert(*op);
            }
            Some(None) => {}
            None => reads.extend(read.query_row([op.0], read_op).optional()?),
        }
    }
    Ok((by_message, reads))
}

fn mark_done(tx: &Connection, ops: &[Pending]) -> rusqlite::Result<()> {
    let mut done = tx.prepare_cached("UPDATE ops SET done = 1 WHERE id = ?1")?;
    for op in ops {
        done.execute([op.id.0])?;
    }
    Ok(())
}

fn take_refusals(tx: &Connection, account: AccountId) -> rusqlite::Result<Vec<OpsRefusal>> {
    let refused = tx
        .prepare_cached(
            "SELECT change, sum(messages) FROM op_refusals WHERE account_id = ?1 \
             GROUP BY change ORDER BY change",
        )?
        .query_map([account.0], |row| {
            Ok(OpsRefusal {
                account,
                change: change_from_code(row.get(0)?),
                messages: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !refused.is_empty() {
        tx.prepare_cached("DELETE FROM op_refusals WHERE account_id = ?1")?
            .execute([account.0])?;
    }
    Ok(refused)
}

fn empty_group(account: AccountId, folder: FolderId, retry: bool) -> OpGroup {
    OpGroup {
        account,
        folder,
        folder_name: String::new(),
        uidvalidity: None,
        action: OpAction::Expunge,
        messages: Vec::new(),
        ops: Vec::new(),
        retry,
        attempts: 0,
        target_uidnext: None,
    }
}

/// A folder's wire name and UIDVALIDITY.
fn folder_wire(tx: &Connection, folder: FolderId) -> rusqlite::Result<(String, Option<u32>)> {
    tx.prepare_cached("SELECT name_raw, uidvalidity FROM folders WHERE id = ?1")?
        .query_row([folder.0], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
        .map(Option::unwrap_or_default)
}

/// The UIDVALIDITY of a folder whose first sync is done; re-homing a moved row needs one.
fn target_uidvalidity(tx: &Connection, folder: FolderId) -> rusqlite::Result<Option<u32>> {
    tx.prepare_cached("SELECT uidvalidity FROM folders WHERE id = ?1 AND sync_state = 'ready'")?
        .query_row([folder.0], |row| row.get(0))
        .optional()
        .map(Option::flatten)
}

/// Whether `folder` is one of `account`'s folders that take messages.
fn takes_messages(tx: &Connection, account: AccountId, folder: FolderId) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1 AND account_id = ?2 AND selectable)",
    )?
    .query_row(params![folder.0, account.0], |row| row.get(0))
}

fn folder_exists(tx: &Connection, folder: FolderId) -> rusqlite::Result<bool> {
    tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1)")?
        .query_row([folder.0], |row| row.get(0))
}

/// An account's folders as planning needs them.
fn account_folders(tx: &Connection, account: AccountId) -> rusqlite::Result<Vec<FolderInfo>> {
    tx.prepare_cached("SELECT id, role, selectable FROM folders WHERE account_id = ?1 ORDER BY id")?
        .query_map([account.0], |row| {
            Ok(FolderInfo {
                id: FolderId(row.get(0)?),
                role: FolderRole::parse(&row.get::<_, String>(1)?),
                selectable: row.get(2)?,
            })
        })?
        .collect()
}

/// A local account has no server holding its folders: one with no key (fixtures and
/// self-tests) or a POP3 account (M8), whose folders are local.
fn is_local(tx: &Connection, account: AccountId) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT key IS NULL OR EXISTS(SELECT 1 FROM pop3_accounts WHERE account_id = accounts.id) \
         FROM accounts WHERE id = ?1",
    )?
    .query_row([account.0], |row| row.get(0))
}

fn new_batch(tx: &Connection, kind: i64) -> rusqlite::Result<i64> {
    tx.prepare_cached("INSERT INTO op_batches(kind) VALUES (?1) RETURNING id")?
        .query_row([kind], |row| row.get(0))
}

/// Drops batches with nothing pending beyond the newest [`KEEP_BATCHES`], and empty ones.
fn prune(tx: &Connection) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "DELETE FROM op_batches WHERE NOT EXISTS \
           (SELECT 1 FROM ops WHERE batch = op_batches.id AND done = 0) \
         AND (id <= coalesce((SELECT id FROM op_batches ORDER BY id DESC LIMIT 1 OFFSET ?1), 0) \
           OR NOT EXISTS (SELECT 1 FROM ops WHERE batch = op_batches.id))",
    )?
    .execute([KEEP_BATCHES])?;
    Ok(())
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
