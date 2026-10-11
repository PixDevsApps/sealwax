//! Drafts, the outbox and contacts (M6: ML-074, ML-076, ML-077, ML-071). The composer saves
//! its draft here at most 5 s behind the typing (PRD C5) and the store reopens the drafts whose
//! window was open when Sealwax stopped. A draft queued to send waits out the undo window in
//! the outbox; the account's actor takes the sends that are due, records each step before it
//! happens and what the server did, so a message is sent at most once (ADR-015): a send that
//! may have reached the server when Sealwax stopped becomes uncertain and is never sent again
//! without the user. After a send the store tracks the Sent copy and queues the source
//! message's `\Answered` or `$Forwarded` through the op queue. Each draft's copy in the
//! server's Drafts folder is replaced when it changed and went quiet, or was closed. Contacts are
//! the addresses already seen, ranked for the composer's suggestions.
//!
//! Every write is served by the writer at interactive priority, ahead of bulk sync batches.
//! Draft saves bump no store version and send no change event (the composer that saved knows);
//! outbox changes send one ([`ChangeEvent::outbox`](super::ChangeEvent::outbox)). Times are
//! Unix ms. Blobs a draft or an outbox entry references (parts, the built message, its Sent
//! copy) are never evicted by the disk cap nor reported orphaned by removals; replies that end
//! a draft list the blobs nothing references any more, for the caller to delete.

use super::ops_api::Failure;
use super::reader::ReadJob;
use super::reader::compose::ComposeRead;
use super::writer::WriteJob;
use super::writer::compose::ComposeJob;
use super::{Reply, Store, StoreVersion};
use sealwax_domain::compose::Draft;
use sealwax_domain::model::{AccountId, BlobKey, DraftId, FolderId, MessageId};

/// How long a draft that changed must stay unchanged before its Drafts-folder copy is replaced
/// while its window is open (accepted at the M6 gate).
pub const DRAFT_QUIET_MS: i64 = 30_000;
/// The same for a draft with no window open: a closed composer's draft goes up shortly after
/// its last save, and the quick reply, which keeps its draft closed while the user types, goes
/// up once the typing pauses rather than after every save.
pub const CLOSED_DRAFT_QUIET_MS: i64 = 10_000;

/// Saves a draft.
#[derive(Clone, Debug)]
pub struct SaveDraft {
    /// `None` creates a draft.
    pub id: Option<DraftId>,
    /// Its parts' bytes are in the blob store already.
    pub draft: Draft,
    /// A composer window shows it: it reopens after a crash or a restart until it is closed.
    pub open: bool,
    pub now: i64,
}

/// The reply to a save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftSaved {
    /// Saved as `revision`, which every save bumps.
    Saved { id: DraftId, revision: u64 },
    /// The draft is in the outbox and can't change until Undo send, or a failed or uncertain
    /// send puts it back.
    NotEditable { id: DraftId, state: SendState },
    /// No draft with that id: discarded, or sent.
    Gone,
}

/// Where a draft stands in the outbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SendState {
    /// Waiting for the undo window to end, or for a retry after a transient failure.
    Queued,
    /// Its data may be on its way to the server: from just before SMTP DATA until the server's
    /// answer is recorded.
    Sending,
    /// Sealwax stopped, or the connection broke, while it was sending: it may have been sent.
    /// Never sent again without the user ([`Store::resolve_uncertain`]).
    Uncertain,
    /// The server took it; its Sent copy may still be pending.
    Sent,
}

/// Why a send failed for good, kept on the draft until it is queued again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SendFailure {
    RecipientsRefused,
    TooLarge,
    /// The server refused the message or its sender.
    Refused,
    NeedsSignIn,
    Other,
    /// Sealwax stopped while it built the message, which may be what stopped it: the message
    /// isn't built again until the user sends it again ([`Store::send_building`]).
    Interrupted,
}

/// A draft as the store keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDraft {
    pub id: DraftId,
    pub draft: Draft,
    pub revision: u64,
    pub created: i64,
    pub saved: i64,
    /// A composer window had it open when it was last saved or marked.
    pub open: bool,
    /// Why its last send failed, if it came back from the outbox that way.
    pub failure: Option<SendFailure>,
    /// `None` while it is an editable draft.
    pub send: Option<SendState>,
}

/// The reply to a discard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discarded {
    pub version: StoreVersion,
    /// Whether the draft existed and was discarded.
    pub found: bool,
    /// It is being sent or was sent: nothing changed.
    pub refused: Option<SendState>,
    /// Its account. When `server_copy`, the app sends `Command::DraftsChanged` for it.
    pub account: Option<AccountId>,
    /// Its Drafts-folder copy is queued for deletion on the server.
    pub server_copy: bool,
    /// Blobs nothing references any more; the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// The reply to queueing a send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendQueued {
    /// In the outbox until `not_before`; the app sends `Command::OutboxQueued` for `account`.
    Queued {
        account: AccountId,
        revision: u64,
        version: StoreVersion,
    },
    /// The account has no server (a local account): nothing can be sent from it.
    LocalAccount,
    /// Already in the outbox.
    NotEditable(SendState),
    Gone,
}

/// The reply to Undo send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendUndone {
    /// Back to an editable draft, marked open; nothing reached the server.
    Undone {
        account: AccountId,
        version: StoreVersion,
        /// A message built for an earlier attempt that nothing references any more.
        orphaned_blobs: Vec<BlobKey>,
    },
    /// Too late: it is being sent, may have been sent, or was sent.
    TooLate(SendState),
    /// Not in the outbox.
    NotQueued,
}

/// One entry of the outbox, for the window.
#[derive(Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub draft: DraftId,
    pub account: AccountId,
    pub state: SendState,
    /// When it goes, or is tried again.
    pub not_before: i64,
    pub attempts: u32,
    /// The draft's subject, for the window to name it (issue #2); `Debug` leaves it out.
    pub subject: String,
    /// When its send started (Unix ms), once it has.
    pub started: Option<i64>,
}

impl std::fmt::Debug for OutboxEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxEntry")
            .field("draft", &self.draft)
            .field("account", &self.account)
            .field("state", &self.state)
            .field("not_before", &self.not_before)
            .field("attempts", &self.attempts)
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

/// What one commit changed in the outbox, in [`ChangeEvent::outbox`](super::ChangeEvent::outbox).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutboxChange {
    pub account: AccountId,
    pub draft: DraftId,
    /// Where it stands now; `None` once it left the outbox (undone, failed for good, sent with
    /// its Sent copy done, back to a draft).
    pub state: Option<SendState>,
    /// Set when it left the outbox because the send failed for good: the draft says why.
    pub failure: Option<SendFailure>,
}

/// The built message, recorded when a send starts so every retry sends the same bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    /// The transmitted copy (no Bcc header), in the blob store.
    pub message_blob: BlobKey,
    /// The Sent copy (the transmitted bytes plus one `Bcc:` field) when it differs.
    pub sent_copy_blob: Option<BlobKey>,
    pub size: u64,
    /// MAIL FROM.
    pub envelope_from: String,
    /// RCPT TO: To, Cc and Bcc.
    pub envelope_to: Vec<String>,
}

/// A send that is due.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueSend {
    pub draft: DraftId,
    pub account: AccountId,
    pub revision: u64,
    /// What to build the message from, when `prepared` is `None`.
    pub message: Draft,
    /// Starts so far.
    pub attempts: u32,
    /// Recorded by an earlier start whose data the server refused with a transient reply, or
    /// that the user sends again after it was uncertain: send exactly these bytes.
    pub prepared: Option<Prepared>,
}

/// The Sent folder a copy goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyFolder {
    pub folder: FolderId,
    pub name_raw: String,
    pub uidvalidity: Option<u32>,
}

/// A sent message whose Sent copy is due.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueSentCopy {
    pub draft: DraftId,
    pub account: AccountId,
    /// The Sent copy's bytes: `Prepared::sent_copy_blob`, else the transmitted copy.
    pub blob: BlobKey,
    pub message_id: String,
    /// The account's Sent folder, `None` when it has none.
    pub folder: Option<CopyFolder>,
    /// The folder's UIDNEXT recorded before the first APPEND: an attempt may have appended it,
    /// so look for the Message-ID at or above it first.
    pub retry_from: Option<u32>,
    pub attempts: u32,
}

/// What the outbox of an account has to do now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DueSends {
    pub sends: Vec<DueSend>,
    pub sent_copies: Vec<DueSentCopy>,
    /// When the next send or Sent copy becomes due, for the actor's timer.
    pub next: Option<i64>,
}

/// The reply to [`Store::send_started`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendStarted {
    /// Recorded: send DATA now.
    Started,
    /// Not queued any more (undone, or another state): **don't send**.
    NotQueued(Option<SendState>),
    /// An earlier start recorded other bytes: send the recorded ones (`DueSend::prepared`).
    Mismatch,
}

/// How the server answered a send that didn't go through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendFailed {
    /// Nothing was delivered and it may go later: before DATA, or a 4xx reply to the data.
    Transient { retry_at: i64 },
    /// Refused for good: back to an editable draft with the reason.
    Permanent(SendFailure),
}

/// The user's answer to an uncertain send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Send it again (the same bytes, so recipients' clients can tell the copies apart by
    /// Message-ID) from `not_before`.
    SendAgain { not_before: i64 },
    /// It was sent: record it so, with its Sent copy and the source's flag.
    WasSent,
    /// Back to an editable draft.
    BackToDraft,
}

/// The reply to recording a send's progress.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendRecorded {
    pub version: StoreVersion,
    /// Whether the entry was in a state that allowed the step; nothing changed otherwise.
    pub applied: bool,
    /// Where it stands now; `None` once it left the outbox.
    pub state: Option<SendState>,
    /// The op queue got the source message's `\Answered` or `$Forwarded`: run the op runner
    /// (`Command::OpsQueued`).
    pub ops_queued: bool,
    /// Drafts-folder copies were queued for deletion: run the draft uploads
    /// (`Command::DraftsChanged`).
    pub copies_to_delete: bool,
    /// Blobs nothing references any more; the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// How a Sent copy ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentCopyDone {
    /// APPENDed (or found at or above the mark), with its UID when known.
    Appended { uid: Option<u32> },
    /// The server saves its own copy of what it sends: none is appended.
    ServerSaves,
    /// Refused for good (no Sent folder, the server refuses it): the message stays sent.
    GaveUp,
    /// Filed into the account's local Sent folder (a POP3 account, M8).
    Filed,
}

/// A copy in the server's Drafts folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerCopy {
    pub folder: FolderId,
    pub uidvalidity: u32,
    /// `None` when the server reported no UID and the search found none.
    pub uid: Option<u32>,
    /// The folder's UIDNEXT before the APPEND that made it.
    pub mark: u32,
    pub revision: u64,
}

/// A draft whose Drafts-folder copy is due to be replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftUpload {
    pub draft: DraftId,
    pub account: AccountId,
    pub revision: u64,
    pub message: Draft,
    /// The account's Drafts folder.
    pub folder: CopyFolder,
    /// The folder's UIDNEXT recorded before an earlier attempt of this upload: look for the
    /// Message-ID at or above it before appending again.
    pub retry_from: Option<u32>,
    pub attempts: u32,
    /// The copy it replaces (deleted once this one is recorded).
    pub replaces: Option<ServerCopy>,
}

/// Identifies a queued deletion of a Drafts-folder copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeletionId(pub i64);

/// Which copy to delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeletionTarget {
    Uid(u32),
    /// Every message with this Message-ID at UIDs from `from_uid` up to `below_uid`
    /// (exclusive; `None` for no bound): copies whose UID was never learned.
    Search {
        message_id: String,
        from_uid: u32,
        below_uid: Option<u32>,
    },
}

/// A copy to delete that the store holds no row for (those it holds go through the op queue as
/// expunges, hidden at once).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftDeletion {
    pub id: DeletionId,
    pub account: AccountId,
    pub folder: FolderId,
    pub folder_name: String,
    /// The UIDVALIDITY the target belongs to: if the folder's changed, record it done.
    pub uidvalidity: u32,
    pub target: DeletionTarget,
    pub attempts: u32,
}

/// What an account's Drafts folder has to do now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DueDrafts {
    pub uploads: Vec<DraftUpload>,
    pub deletions: Vec<DraftDeletion>,
    /// When the next upload or deletion becomes due.
    pub next: Option<i64>,
}

/// The reply to recording an upload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UploadRecorded {
    pub version: StoreVersion,
    /// The draft still exists and the upload was recorded.
    pub applied: bool,
    /// The copy it replaced went to the op queue: run the op runner.
    pub ops_queued: bool,
}

/// A suggestion for a recipient field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    /// Lower case.
    pub address: String,
    /// The name it was last seen with; empty for none.
    pub name: String,
    /// Messages it was seen in ("14 messages").
    pub seen: u32,
    /// Messages the user wrote to it.
    pub written: u32,
    /// When it was last seen (Unix seconds).
    pub last: i64,
}

impl Store {
    /// Saves a draft (a new one when `id` is `None`) and bumps its revision. A draft in the
    /// outbox isn't changed. Draft saves send no change event.
    pub fn save_draft(&self, save: SaveDraft) -> Reply<DraftSaved> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::Save {
                save: Box::new(save),
                reply,
            })
        })
    }

    /// A draft, with its revision and its place in the outbox.
    pub fn draft(&self, id: DraftId) -> Reply<Option<StoredDraft>> {
        self.read(|reply| ReadJob::Compose(ComposeRead::Draft { id, reply }))
    }

    /// The editable drafts whose composer window was open when Sealwax stopped, oldest first.
    pub fn open_drafts(&self) -> Reply<Vec<DraftId>> {
        self.read(|reply| ReadJob::Compose(ComposeRead::OpenDrafts { reply }))
    }

    /// Marks a draft's window open or closed. Closing keeps the draft (and makes its
    /// Drafts-folder copy due at once: send `Command::DraftsChanged`).
    pub fn set_draft_open(&self, id: DraftId, open: bool) -> Reply<()> {
        self.write(|reply| WriteJob::Compose(ComposeJob::SetOpen { id, open, reply }))
    }

    /// Discards a draft (an Undo-able send in the window included): its server copy is queued
    /// for deletion and the reply lists the blobs nothing references any more. A draft being
    /// sent, uncertain or sent is refused.
    pub fn discard_draft(&self, id: DraftId) -> Reply<Discarded> {
        self.write(|reply| WriteJob::Compose(ComposeJob::Discard { id, reply }))
    }

    /// The local draft whose Drafts-folder copy `message` is (same account, same Message-ID,
    /// in a folder with the Drafts role): Edit Draft opens that one, and the list can tell the
    /// two are one draft.
    pub fn draft_for_message(&self, message: MessageId) -> Reply<Option<DraftId>> {
        self.read(|reply| ReadJob::Compose(ComposeRead::DraftForMessage { message, reply }))
    }

    /// The newest closed, editable draft that replies to `message` with Reply (M6 phase B: the
    /// quick reply's text kept as a draft, found again after a restart).
    pub fn reply_draft(&self, message: MessageId) -> Reply<Option<StoredDraft>> {
        self.read(|reply| ReadJob::Compose(ComposeRead::ReplyDraft { message, reply }))
    }

    /// Queues a draft to send from `not_before` (the end of the undo window). Refused for local
    /// accounts, which have no server; a POP3 account sends through its SMTP server.
    pub fn queue_send(&self, id: DraftId, not_before: i64) -> Reply<SendQueued> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::Queue {
                id,
                not_before,
                reply,
            })
        })
    }

    /// Undo send: a queued draft that hasn't started goes back to an editable draft, and
    /// nothing reaches the server.
    pub fn undo_send(&self, id: DraftId) -> Reply<SendUndone> {
        self.write(|reply| WriteJob::Compose(ComposeJob::Undo { id, reply }))
    }

    /// The outbox: every queued, sending, uncertain or sent entry, oldest first; `account`
    /// narrows it to one account.
    pub fn outbox(&self, account: Option<AccountId>) -> Reply<Vec<OutboxEntry>> {
        self.read(|reply| ReadJob::Compose(ComposeRead::Outbox { account, reply }))
    }

    /// At the engine's start, before any `due_sends`: every send left `Sending` becomes
    /// `Uncertain` (its data may have reached the server) and is never sent again without the
    /// user; every send whose message was being built ([`Store::send_building`]) goes back to an
    /// editable draft that failed with [`SendFailure::Interrupted`], so a message that stops
    /// Sealwax isn't built again at every start; a Drafts-folder copy left being built
    /// ([`Store::draft_building`]) waits for the draft to change. Replies with the sends, for
    /// `Event::SendUncertain` and `Event::SendFailed`.
    pub fn recover_sends(&self) -> Reply<Vec<OutboxChange>> {
        self.write(|reply| WriteJob::Compose(ComposeJob::Recover { reply }))
    }

    /// Records, before a queued send's message is built, that it is being built; `false` when it
    /// isn't queued any more (build nothing). [`Store::send_built`] clears it once the build
    /// returns, whatever it returned; a start that finds it set fails the send instead of
    /// building it again ([`Store::recover_sends`]).
    pub fn send_building(&self, draft: DraftId, now: i64) -> Reply<bool> {
        self.write(|reply| WriteJob::Compose(ComposeJob::Building { draft, now, reply }))
    }

    /// The message's build returned.
    pub fn send_built(&self, draft: DraftId) -> Reply<()> {
        self.write(|reply| WriteJob::Compose(ComposeJob::Built { draft, reply }))
    }

    /// Records, before a draft's Drafts-folder copy is built, that `revision`'s is being built;
    /// `false` when the draft is gone or in the outbox. [`Store::draft_built`] clears it.
    pub fn draft_building(&self, draft: DraftId, revision: u64) -> Reply<bool> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::DraftBuilding {
                draft,
                revision,
                reply,
            })
        })
    }

    /// The Drafts-folder copy's build returned.
    pub fn draft_built(&self, draft: DraftId) -> Reply<()> {
        self.write(|reply| WriteJob::Compose(ComposeJob::DraftBuilt { draft, reply }))
    }

    /// The account's sends and Sent copies due at `now`, and when the next one is.
    pub fn due_sends(&self, account: AccountId, now: i64) -> Reply<DueSends> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::DueSends {
                account,
                now,
                reply,
            })
        })
    }

    /// Records a send's start, just before SMTP DATA (after MAIL FROM and RCPT TO), with the
    /// built message (already durable in the blob store) and its envelope. Only a queued entry
    /// starts: send DATA only on [`SendStarted::Started`].
    pub fn send_started(&self, draft: DraftId, prepared: Prepared, now: i64) -> Reply<SendStarted> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::Started {
                draft,
                prepared: Box::new(prepared),
                now,
                reply,
            })
        })
    }

    /// The server took the message (250 after DATA), or a `Sending` or `Uncertain` entry was
    /// sent after all. In one transaction: the entry becomes `Sent` with its Sent copy pending,
    /// the source message's `\Answered` or `$Forwarded` is queued in the op queue, and the
    /// draft's Drafts-folder copy is queued for deletion.
    pub fn send_done(&self, draft: DraftId, now: i64) -> Reply<SendRecorded> {
        self.write(|reply| WriteJob::Compose(ComposeJob::Done { draft, now, reply }))
    }

    /// The server didn't take it: a transient failure (nothing delivered: before DATA, or a 4xx
    /// reply to the data) stays queued until `retry_at` with the recorded bytes; a permanent
    /// one goes back to an editable draft with the reason.
    pub fn send_failed(&self, draft: DraftId, failure: SendFailed) -> Reply<SendRecorded> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::Failed {
                draft,
                failure,
                reply,
            })
        })
    }

    /// The connection broke after DATA started and before the server's answer: the entry
    /// becomes `Uncertain`.
    pub fn send_uncertain(&self, draft: DraftId) -> Reply<SendRecorded> {
        self.write(|reply| WriteJob::Compose(ComposeJob::Uncertain { draft, reply }))
    }

    /// The user's answer to an uncertain send.
    pub fn resolve_uncertain(
        &self,
        draft: DraftId,
        resolution: Resolution,
        now: i64,
    ) -> Reply<SendRecorded> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::Resolve {
                draft,
                resolution,
                now,
                reply,
            })
        })
    }

    /// Records the start of a Sent copy's APPEND: the folder, its UIDVALIDITY and its UIDNEXT
    /// (kept from the first attempt), before the command.
    pub fn sent_copy_started(
        &self,
        draft: DraftId,
        folder: FolderId,
        uidvalidity: u32,
        uidnext: u32,
    ) -> Reply<SendRecorded> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::CopyStarted {
                draft,
                folder,
                uidvalidity,
                uidnext,
                reply,
            })
        })
    }

    /// The Sent copy is done (or skipped, or given up): the entry leaves the outbox, the draft
    /// is gone, and the reply lists the blobs nothing references any more.
    pub fn sent_copy_done(&self, draft: DraftId, done: SentCopyDone) -> Reply<SendRecorded> {
        self.write(|reply| WriteJob::Compose(ComposeJob::CopyDone { draft, done, reply }))
    }

    /// A Sent copy's APPEND failed transiently: tried again from `retry_at`, looking above the
    /// recorded mark first.
    pub fn sent_copy_failed(&self, draft: DraftId, retry_at: i64) -> Reply<SendRecorded> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::CopyFailed {
                draft,
                retry_at,
                reply,
            })
        })
    }

    /// The account's Drafts-folder uploads and deletions due at `now`. A draft is due when it
    /// changed since its last upload and was saved at least [`DRAFT_QUIET_MS`] ago while its
    /// window is open, or [`CLOSED_DRAFT_QUIET_MS`] ago when none is; drafts in the outbox aren't
    /// uploaded. Local accounts have none; a POP3 account's are filed into its local Drafts
    /// folder ([`Store::draft_filed`]) instead of a server's.
    pub fn due_drafts(&self, account: AccountId, now: i64) -> Reply<DueDrafts> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::DueDrafts {
                account,
                now,
                reply,
            })
        })
    }

    /// Records the start of an upload, before the APPEND: the folder's UIDVALIDITY and UIDNEXT
    /// (the mark kept from the first attempt). `applied` is false when the draft is gone.
    pub fn draft_upload_started(
        &self,
        draft: DraftId,
        revision: u64,
        folder: FolderId,
        uidvalidity: u32,
        uidnext: u32,
    ) -> Reply<UploadRecorded> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::UploadStarted {
                draft,
                revision,
                folder,
                uidvalidity,
                uidnext,
                reply,
            })
        })
    }

    /// The upload is on the server (APPENDUID's UID, or the one found above the mark): the
    /// copy it replaces is queued for deletion. A draft discarded or sent meanwhile has its
    /// upload deleted already (a search above the mark).
    pub fn draft_upload_done(
        &self,
        draft: DraftId,
        revision: u64,
        uid: Option<u32>,
    ) -> Reply<UploadRecorded> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::UploadDone {
                draft,
                revision,
                uid,
                reply,
            })
        })
    }

    /// An upload failed: transient ones wait for `retry_at`; a permanent one isn't tried again
    /// until the draft changes.
    pub fn draft_upload_failed(&self, draft: DraftId, failure: Failure) -> Reply<UploadRecorded> {
        self.write(|reply| {
            WriteJob::Compose(ComposeJob::UploadFailed {
                draft,
                failure,
                reply,
            })
        })
    }

    /// Deletions the server carried out (or that no longer apply: another UIDVALIDITY, the
    /// copy gone already).
    pub fn draft_deletions_done(&self, ids: Vec<DeletionId>) -> Reply<()> {
        self.write(|reply| WriteJob::Compose(ComposeJob::DeletionsDone { ids, reply }))
    }

    /// A deletion failed: transient ones wait for `retry_at`, permanent ones are dropped.
    pub fn draft_deletion_failed(&self, id: DeletionId, failure: Failure) -> Reply<()> {
        self.write(|reply| WriteJob::Compose(ComposeJob::DeletionFailed { id, failure, reply }))
    }

    /// Up to `limit` contacts of `account` with an address, domain or name word starting with
    /// `prefix` (case- and diacritic-insensitive): those the user wrote to first, then by how
    /// often and how recently they were seen.
    pub fn recipients(&self, account: AccountId, prefix: &str, limit: u32) -> Reply<Vec<Contact>> {
        let prefix = prefix.to_owned();
        self.read(|reply| {
            ReadJob::Compose(ComposeRead::Recipients {
                account,
                prefix,
                limit,
                reply,
            })
        })
    }
}
