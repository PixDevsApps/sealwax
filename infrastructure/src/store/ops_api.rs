//! The op queue's requests (ML-051, ML-053, ADR-015): the app queues triage with its overlay and
//! undoes it; the account's op runner reads the ops that are due, marks them started and records
//! what the server did. Every write is one transaction with one exact change event, served by the
//! writer at interactive priority.

use super::reader::ReadJob;
use super::writer::WriteJob;
use super::writer::ops::OpsJob;
use super::{Reply, Store, StoreVersion};
use sealwax_domain::model::{AccountId, BlobKey, Flags, FolderId, MessageId};
use sealwax_domain::triage::Triage;

/// One change to one message, or to the server's unread below a folder's floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(pub i64);

/// The ops one user action made, for Undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchId(pub i64);

/// A user's action on messages, which may span folders and accounts (All Inboxes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueTriage {
    pub action: Triage,
    pub messages: Vec<MessageId>,
    /// When the undo window ends (Unix ms); the ops don't reach the server before it. Flag
    /// changes, which need no window, pass the current time or 0.
    pub not_before: i64,
}

/// Why an action left messages alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotApplied {
    /// The account has no folder for it, or the folder named takes no messages.
    NoTarget,
    /// The messages are already where the action would put them.
    AlreadyThere,
    /// Not in the store any more, or about to be deleted for good.
    Gone,
}

/// The reply to a queued action.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Queued {
    /// The commit's version, or the current one when nothing changed.
    pub version: StoreVersion,
    /// What Undo takes; `None` when no message changed.
    pub batch: Option<BatchId>,
    /// Messages the action applies to, those already as it asks included.
    pub applied: u32,
    /// Messages left alone, by reason.
    pub not_applied: Vec<(NotApplied, u32)>,
    /// Server accounts with new ops: the app sends `Command::OpsQueued` for each.
    pub accounts: Vec<AccountId>,
}

/// Why Undo couldn't put messages back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lost {
    /// Deleted for good.
    Deleted,
    /// The store no longer knows where the message is: gone from the store, its folder gone,
    /// or moved where the server reported no UID for it.
    Unknown,
    /// The server's unread messages below a folder's floor were marked read; which ones isn't
    /// known.
    ReadBelow,
}

/// The reply to an Undo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Undone {
    pub version: StoreVersion,
    /// Messages put back at once: their ops hadn't started, so nothing reached the server.
    pub restored: u32,
    /// Messages whose change reached the server, or may have: their inverse ops are queued
    /// without a window (applied at once in local accounts).
    pub reverted: u32,
    /// Messages that can't come back, by reason. A folder marked read counts the server's
    /// unread messages below its floor.
    pub lost: Vec<(Lost, u32)>,
    /// Server accounts with new ops: the app sends `Command::OpsQueued` for each.
    pub accounts: Vec<AccountId>,
}

/// What the server must do for a group of messages that share a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpAction {
    /// `UID STORE +FLAGS add` and `-FLAGS remove`.
    Flags { add: Flags, remove: Flags },
    /// `+FLAGS add` and `-FLAGS remove`, then a move to `to`: `UID MOVE`, or COPY, `\Deleted`
    /// and `UID EXPUNGE` after S1's UIDNEXT check.
    Move {
        to: FolderId,
        /// The target's wire name.
        to_name: String,
        to_uidvalidity: Option<u32>,
        add: Flags,
        remove: Flags,
    },
    /// `\Deleted`, then `UID EXPUNGE` where the server has UIDPLUS.
    Expunge,
    /// A folder marked read: `UID STORE 1:floor-1 +FLAGS (\Seen)`, the messages below the floor
    /// that the store doesn't hold.
    ReadBelow { floor: u32 },
}

/// One message of a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpMessage {
    pub message: MessageId,
    pub uid: u32,
    /// The RFC 5322 Message-ID, for S1's check of the target above the recorded UIDNEXT.
    pub message_id: Option<String>,
    /// The ops the action carries out for this message; record them together.
    pub ops: Vec<OpId>,
}

/// Ops due now that one IMAP action carries out: one folder, one action, a UID set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpGroup {
    pub account: AccountId,
    pub folder: FolderId,
    /// The folder's wire name.
    pub folder_name: String,
    /// The UIDVALIDITY the UIDs belong to; the runner checks it after SELECT.
    pub uidvalidity: Option<u32>,
    pub action: OpAction,
    /// In UID order; empty for `ReadBelow`.
    pub messages: Vec<OpMessage>,
    /// Every op of the group.
    pub ops: Vec<OpId>,
    /// Started before and not recorded (a crash, a lost connection): the action may have run.
    pub retry: bool,
    /// Attempts so far, the most of any op in the group.
    pub attempts: u32,
    /// The target's UIDNEXT recorded before the first attempt (moves); the lowest of the group.
    pub target_uidnext: Option<u32>,
}

/// What the op runner of an account does next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DueOps {
    pub groups: Vec<OpGroup>,
    /// When the next op becomes due (Unix ms), for the runner's timer.
    pub next: Option<i64>,
    /// Changes the store gave up while syncing since the last call (a folder gone, messages of
    /// a folder whose UIDVALIDITY changed that didn't come back): one `Event::OpsRefused` each.
    pub refused: Vec<OpsRefusal>,
}

/// What the server did for ops it carried out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpsDone {
    pub ops: Vec<OpId>,
    /// A move's new UIDs in its target by the UID in the source: from COPYUID, or S1's
    /// Message-ID check above the recorded UIDNEXT. A moved message without one leaves the
    /// source for the target's sync to fetch.
    pub new_uids: Vec<(u32, u32)>,
    /// The target's UIDVALIDITY of `new_uids` (COPYUID's); `None` trusts the stored one.
    pub target_uidvalidity: Option<u32>,
    /// The server lacks UIDPLUS: expunged messages were only flagged `\Deleted` and stay, hidden.
    pub flagged_only: bool,
}

/// The reply to recorded outcomes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    pub version: StoreVersion,
    /// Raw messages no remaining row refers to; the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// How the server refused ops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Try again: the ops and their overlays stay, from `retry_at` (Unix ms) when given.
    Transient { retry_at: Option<i64> },
    /// For good: the ops are dropped and the messages shown as the server has them.
    Permanent,
}

/// What kind of change was refused, as `engine::api::Refused` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefusedChange {
    Flags,
    Move,
    Expunge,
    FolderRead,
}

/// Changes of one kind refused for an account: what `Event::OpsRefused` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpsRefusal {
    pub account: AccountId,
    pub change: RefusedChange,
    /// Messages put back; for a folder marked read, the server's unread below its floor.
    pub messages: u32,
}

/// The reply to recorded failures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpsFailed {
    pub version: StoreVersion,
    pub refused: Vec<OpsRefusal>,
}

impl Store {
    /// Queues `request.action` on its messages: groups them by the folder each is in (where a
    /// pending move takes it, if any), plans each group with `domain::triage::plan` and the
    /// account's folders, and writes the ops and their overlays in one transaction. Local
    /// accounts (no key) have no server: their ops apply at once.
    pub fn queue_triage(&self, request: QueueTriage) -> Reply<Queued> {
        self.write(|reply| WriteJob::Ops(OpsJob::Queue { request, reply }))
    }

    /// Marks every message of `folder` read, the server's unread below a windowed folder's floor
    /// included: the counts drop at once.
    pub fn mark_folder_read(&self, folder: FolderId, not_before: i64) -> Reply<Queued> {
        self.write(|reply| {
            WriteJob::Ops(OpsJob::FolderRead {
                folder,
                not_before,
                reply,
            })
        })
    }

    /// Undoes a batch: ops that haven't started are dropped and their messages come back as
    /// they were; for ops that have, inverse ops computed from each message's state at queue
    /// time are queued without a window. A batch undone before does nothing.
    pub fn undo(&self, batch: BatchId) -> Reply<Undone> {
        self.write(|reply| WriteJob::Ops(OpsJob::Undo { batch, reply }))
    }

    /// The account's ops due at `now` (Unix ms), grouped into IMAP actions, with the next time
    /// one becomes due and the refusals the sync paths made.
    pub fn due_ops(&self, account: AccountId, now: i64) -> Reply<DueOps> {
        self.write(|reply| {
            WriteJob::Ops(OpsJob::Due {
                account,
                now,
                reply,
            })
        })
    }

    /// Marks ops started, before their IMAP action, with the target's UIDNEXT for moves (kept
    /// from the first attempt).
    pub fn start_ops(&self, ops: Vec<OpId>, target_uidnext: Option<u32>) -> Reply<()> {
        self.write(|reply| {
            WriteJob::Ops(OpsJob::Start {
                ops,
                target_uidnext,
                reply,
            })
        })
    }

    /// Records ops the server carried out: the overlay becomes the server's state in the same
    /// transaction. Ops recorded before, or gone, are skipped.
    pub fn ops_done(&self, done: OpsDone) -> Reply<Recorded> {
        self.write(|reply| WriteJob::Ops(OpsJob::Done { done, reply }))
    }

    /// Records ops the server refused.
    pub fn ops_failed(&self, ops: Vec<OpId>, failure: Failure) -> Reply<OpsFailed> {
        self.write(|reply| {
            WriteJob::Ops(OpsJob::Failed {
                ops,
                failure,
                reply,
            })
        })
    }

    /// The account's ops not yet done, for its status.
    pub fn pending_ops(&self, account: AccountId) -> Reply<u32> {
        self.read(|reply| ReadJob::PendingOps { account, reply })
    }
}
