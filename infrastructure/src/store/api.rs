//! The store's requests, replies and change events: plain `Send` values.

use super::views::ViewHandle;
use sealwax_domain::model::{
    AccountId, Attachment, BlobKey, BodyKind, Flags, FolderId, FolderRole, Headers, MessageId,
    MessageSummary, ThreadId,
};
use std::time::Duration;

/// Bumped by every commit that changes what a list, the sidebar or the reader shows. Replies
/// carry the version their snapshot saw; change events carry the version they produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreVersion(pub u64);

/// Which messages a view lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewScope {
    Folder(FolderId),
    /// Every folder with the Inbox role, across accounts.
    UnifiedInbox,
}

/// The list's filter chips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ViewFilter {
    #[default]
    All,
    Unread,
    Starred,
    Attachments,
}

/// A message list: a scope and a filter, newest first (date, then id, descending).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewSpec {
    pub scope: ViewScope,
    pub filter: ViewFilter,
}

impl ViewSpec {
    pub fn folder(folder: FolderId) -> Self {
        Self {
            scope: ViewScope::Folder(folder),
            filter: ViewFilter::All,
        }
    }

    pub fn unified_inbox() -> Self {
        Self {
            scope: ViewScope::UnifiedInbox,
            filter: ViewFilter::All,
        }
    }

    pub fn with_filter(self, filter: ViewFilter) -> Self {
        Self { filter, ..self }
    }
}

/// Identifies an open view in change events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(pub u64);

/// Opens a view: its row count, the first page and the section bounds, from one snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenView {
    pub spec: ViewSpec,
    /// Rows of the first page to return with the count (0 for none).
    pub first_page: u32,
    /// Section start times (Unix seconds, newest first); the reply counts the rows at or after
    /// each, so the list knows where Today, Yesterday… end.
    pub section_starts: Vec<i64>,
}

#[derive(Debug)]
pub struct ViewOpened {
    pub version: StoreVersion,
    /// Keeps the view registered for change events; dropping it closes the view.
    pub view: ViewHandle,
    pub total: u32,
    /// For each section start, the number of rows dated at or after it.
    pub section_counts: Vec<u32>,
    pub rows: Vec<MessageSummary>,
}

/// Rows `offset..offset + rows.len()` of a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub version: StoreVersion,
    pub offset: u32,
    pub rows: Vec<MessageSummary>,
}

/// A reply value with the version of the snapshot or commit it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Versioned<T> {
    pub version: StoreVersion,
    pub value: T,
}

/// A new account row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAccount {
    pub name: String,
    pub address: String,
}

/// A folder as LIST reports it; inserted or updated by `(account, name_raw)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSpec {
    pub account: AccountId,
    /// Decoded display name.
    pub name: String,
    /// The name on the wire (modified UTF-7).
    pub name_raw: String,
    pub delimiter: Option<char>,
    pub role: FolderRole,
    /// LIST attributes as the server sent them (`\HasNoChildren`, `\Sent`…).
    pub attributes: Vec<String>,
    /// False for `\Noselect` and `\NonExistent` folders, which hold no messages.
    pub selectable: bool,
    /// The server's STATUS counts. The sidebar shows them while the folder is new and empty,
    /// until its first sync starts; after that the store counts what it holds, plus what a
    /// windowed folder leaves below its floor ([`Store::count_below`](super::Store::count_below)).
    pub server_counts: Option<ServerCounts>,
}

impl FolderSpec {
    /// A local folder whose wire name is its display name.
    pub fn local(account: AccountId, name: &str, role: FolderRole) -> Self {
        Self {
            account,
            name: name.to_owned(),
            name_raw: name.to_owned(),
            delimiter: Some('/'),
            role,
            attributes: Vec::new(),
            selectable: true,
            server_counts: None,
        }
    }
}

/// MESSAGES and UNSEEN from STATUS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerCounts {
    pub total: u32,
    pub unread: u32,
}

/// Where a folder's sync stands (S1's resumable first sync).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Listed; nothing fetched yet.
    New,
    /// The first sync is under way: every UID from `floor` up to `top` (the UIDNEXT when it
    /// started, exclusive) is stored, so an interrupted sync continues below `floor`.
    Initial { top: u32, modseq: u64, floor: u32 },
    /// The first sync is done: every UID from the floor up to `uidnext` (exclusive) that the
    /// server holds is stored.
    Ready,
}

/// A folder as the sync engine reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderSync {
    pub folder: FolderId,
    pub account: AccountId,
    pub name_raw: String,
    pub delimiter: Option<char>,
    pub role: FolderRole,
    pub selectable: bool,
    pub uidvalidity: Option<u32>,
    pub uidnext: u32,
    pub highestmodseq: u64,
    pub state: SyncState,
    /// The lowest UID the first sync reached, once it started: every UID the server holds from
    /// it up is stored. 1 once nothing older is left on the server.
    pub floor: Option<u32>,
    /// Messages the server held below the floor when last counted: a hint, since other clients
    /// can delete them unseen.
    pub below: u32,
    /// Messages the store holds for the folder.
    pub messages: u32,
}

impl FolderSync {
    /// Nothing older than the floor is left on the server (ML-043).
    pub fn complete(&self) -> bool {
        self.floor.is_some_and(|floor| floor <= 1)
    }
}

/// One step of a folder's first sync, each committed on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstSyncStep {
    /// Starts the first sync of a new folder with the server's UIDVALIDITY, UIDNEXT and
    /// HIGHESTMODSEQ (0 without CONDSTORE) at that moment. The folder's counts become the
    /// store's own again.
    Start {
        uidvalidity: u32,
        top: u32,
        modseq: u64,
    },
    /// Every UID from `floor` up to the start's UIDNEXT is stored. Commit it only after the
    /// messages' ingest has replied.
    Floor(u32),
    /// Done: the folder becomes `Ready` with the UIDNEXT and HIGHESTMODSEQ from the start, so
    /// the next re-sync catches whatever changed while the first sync ran. `below` messages are
    /// left under the floor (ML-043's window); with none, the floor drops to 1.
    Finish { below: u32 },
}

/// How much of a message the store holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BodyState {
    /// Headers only; no blob.
    #[default]
    Headers,
    /// Headers and the start of the text (for the snippet); no blob.
    Partial,
    /// The whole raw message is in the blob store.
    Full,
    /// A message too large to download whole (ML-044): the blob store holds its headers and
    /// text parts in the message's own MIME structure, every attachment left empty and recorded
    /// by its IMAP section for fetching on demand.
    Text,
}

impl BodyState {
    pub(crate) fn code(self) -> i64 {
        match self {
            BodyState::Headers => 0,
            BodyState::Partial => 1,
            BodyState::Full => 2,
            BodyState::Text => 3,
        }
    }

    pub(crate) fn from_code(code: i64) -> BodyState {
        match code {
            1 => BodyState::Partial,
            2 => BodyState::Full,
            3 => BodyState::Text,
            _ => BodyState::Headers,
        }
    }
}

/// One message for the writer, as `ingest_raw` builds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewMessage {
    pub folder: FolderId,
    /// The IMAP UID; `None` gives the next free UID of a local folder and skips content the
    /// folder already holds.
    pub uid: Option<u32>,
    pub flags: Flags,
    pub keywords: Vec<String>,
    /// The sort key: INTERNALDATE, else the Date header (Unix seconds).
    pub date: i64,
    pub headers: Headers,
    pub snippet: String,
    pub body_kind: BodyKind,
    pub text_parts: Vec<u32>,
    pub html_parts: Vec<u32>,
    pub attachments: Vec<Attachment>,
    pub has_attachments: bool,
    pub size: u64,
    pub body_state: BodyState,
    pub blob: Option<BlobKey>,
    /// The body as plain text, for the search index.
    pub search_text: String,
}

/// The writer's reply to an ingest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ingested {
    /// The version after the last header commit.
    pub version: StoreVersion,
    /// The stored id for each message, in input order; `None` for one the folder already held.
    pub ids: Vec<Option<MessageId>>,
    /// Time inside header transactions, until each COMMIT returned.
    pub header_time: Duration,
    /// Time inside full-text transactions.
    pub index_time: Duration,
    /// Header transactions (at most 200 messages each).
    pub batches: u32,
}

/// A folder's counts as the sidebar shows them: the store's own, plus, for a folder that keeps
/// only its newest mail (ML-043), the server's messages and unread messages below its floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderCounts {
    pub folder: FolderId,
    pub total: u32,
    pub unread: u32,
}

/// A conversation's new size; 0 when its last message is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadCount {
    pub thread: ThreadId,
    pub count: u32,
}

/// Two conversations became one: rows of `from` now belong to `into`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadMerge {
    pub from: ThreadId,
    pub into: ThreadId,
}

/// A row and its position in a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Positioned {
    pub position: u32,
    pub row: MessageSummary,
}

/// How one open view changed in one commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewDiff {
    /// Apply `removed` (positions before the commit, descending) one by one, then `inserted`
    /// (positions after the commit, ascending), then update `changed` in place (positions after
    /// the commit). `total` is the row count afterwards.
    Splice {
        removed: Vec<u32>,
        inserted: Vec<Positioned>,
        changed: Vec<Positioned>,
        total: u32,
    },
    /// Too much changed to describe row by row: open the view again.
    Reload { total: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewChange {
    pub view: ViewId,
    pub diff: ViewDiff,
}

/// What one commit changed. Views that a commit doesn't touch get no entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeEvent {
    pub version: StoreVersion,
    pub inserted: Vec<MessageId>,
    pub removed: Vec<MessageId>,
    /// Messages whose flags changed, or whose body was downloaded.
    pub changed: Vec<MessageId>,
    /// New counts of the folders this commit touched.
    pub folders: Vec<FolderCounts>,
    /// An account or folder was added or changed: read the folder list again.
    pub folders_changed: bool,
    /// New sizes of the conversations this commit touched; rows of these threads show them.
    pub threads: Vec<ThreadCount>,
    pub merged_threads: Vec<ThreadMerge>,
    pub views: Vec<ViewChange>,
    /// Sends that entered, moved within or left the outbox (M6): the window's outbox count,
    /// a send failed for good (`state` `None` with a `failure`) or uncertain.
    pub outbox: Vec<super::OutboxChange>,
}

/// A body downloaded for a message the store holds as headers (ML-044). Its raw message, or for
/// a large one its text parts ([`BodyState::Text`]), is in the blob store already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyUpgrade {
    pub message: MessageId,
    /// The UID it was fetched by; a row with another UID is left alone.
    pub uid: u32,
    pub blob: BlobKey,
    /// The blob's length.
    pub blob_size: u64,
    /// [`BodyState::Full`] or [`BodyState::Text`].
    pub body_state: BodyState,
    pub body_kind: BodyKind,
    pub has_attachments: bool,
    /// Replaces the preview's snippet unless empty.
    pub snippet: String,
    pub text_parts: Vec<u32>,
    pub html_parts: Vec<u32>,
    pub attachments: Vec<Attachment>,
    /// IMAP sections of the attachments by mail-parser part index, for fetching one alone.
    pub sections: Vec<(u32, String)>,
    /// The body as plain text, for the search index.
    pub search_text: String,
    /// When it was fetched (Unix seconds): the disk cap evicts the bodies used longest ago.
    pub used: i64,
}

/// The writer's reply to an upgrade.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Upgraded {
    pub version: StoreVersion,
    /// Whether the message was still there with its UID.
    pub found: bool,
    /// Raw messages no row refers to any more (the new one when the message was gone, or the
    /// one it replaced); the caller deletes them from the blob store.
    pub orphaned_blobs: Vec<BlobKey>,
}

/// What the engine needs to download a message's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyTarget {
    pub message: MessageId,
    pub account: AccountId,
    pub folder: FolderId,
    pub uid: u32,
    /// RFC822.SIZE.
    pub size: u64,
    pub body_state: BodyState,
    pub blob: Option<BlobKey>,
}

/// Where one attachment of a stored message is on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartSource {
    pub message: MessageId,
    pub folder: FolderId,
    pub uid: u32,
    pub section: String,
    pub mime_type: String,
}

/// A message without a body in an Inbox of a server account, for the background prefetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyCandidate {
    pub message: MessageId,
    pub account: AccountId,
    pub folder: FolderId,
    pub role: FolderRole,
    pub date: i64,
    pub size: u64,
}

/// What the downloaded bodies take on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyUsage {
    pub bodies: u64,
    pub bytes: u64,
}

/// The writer's reply to an eviction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evicted {
    /// Messages back to their headers and preview.
    pub messages: Vec<MessageId>,
    /// Raw messages to delete from the blob store.
    pub blobs: Vec<BlobKey>,
    /// What the downloaded bodies take afterwards.
    pub bytes: u64,
    /// Still over the cap after a full batch: evict again.
    pub more: bool,
}
