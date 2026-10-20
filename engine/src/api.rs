//! What the app sends the engine and what it hears back: plain `Send` values over async-channel
//! (ADR-003). M2 covers account setup and the first sync; M3 adds the sync engine's commands:
//! folders opened, older mail, bodies, reconnecting, a new password, and the account's status.
//! M4 adds the op queue's: the app queues triage in the store and wakes the account, and hears
//! when the server refuses a change for good. M5 adds downloads the reader asks for: one part of
//! a large message, or a message whole as the server has it. M6 adds sending: the app queues a
//! draft in the store's outbox and wakes the account, which sends it at most once, saves the
//! Sent copy and keeps the Drafts folder's copy of every draft current. M9 adds folder
//! management (ADR-020): a folder created, renamed or deleted on the server, or at once among
//! a POP3 account's local folders. Check for Updates asks GitHub for the latest release, only when
//! the user runs it.

use crate::imap::ServerText;
use sealwax_domain::account::{
    AccountColor, AccountKey, AccountSettings, CertificatePin, DetectSource, Detection, Endpoint,
    LoginError, LoginFailure, OAuthProvider, Protocol,
};
use sealwax_domain::config::Storage;
use sealwax_domain::folders::NameProblem;
use sealwax_domain::model::{AccountId, DraftId, FolderId, MessageId};
use sealwax_domain::updates::Version;
use sealwax_infrastructure::store::BatchId;
use std::fmt;
use zeroize::Zeroizing;

/// Pairs a reply with the command that asked for it.
pub type RequestId = u64;

/// A password typed into the assistant. It never prints and is wiped when dropped.
#[derive(Clone)]
pub struct Password(Zeroizing<String>);

impl Password {
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(..)")
    }
}

#[derive(Debug)]
pub enum Command {
    /// Look up the settings for an address (ML-030).
    Detect { request: RequestId, address: String },
    /// Log in to IMAP, then SMTP, without saving anything (PRD A3). When the first username is
    /// refused, `second_username` is the one more attempt (≤ 2 login attempts).
    Verify {
        request: RequestId,
        settings: AccountSettings,
        second_username: Option<String>,
        password: Password,
    },
    /// Read the certificate a server on this computer shows (issue #9), for the user to trust
    /// after a check failed on it: TCP, the plaintext half of STARTTLS if `endpoint` uses it, and
    /// the handshake up to the certificate. No login; answered with `CertificateRead`.
    ReadCertificate {
        request: RequestId,
        endpoint: Endpoint,
        protocol: Protocol,
    },
    /// Save a verified account: the password (or the sign-in's refresh token) to the keyring,
    /// the settings to `accounts.toml`, the account to the store; then list its folders and
    /// sync its INBOX headers. An OAuth account names the `SignIn` request whose token it keeps
    /// in `sign_in` and sends an empty password.
    AddAccount {
        request: RequestId,
        settings: AccountSettings,
        password: Password,
        sign_in: Option<RequestId>,
    },
    /// A browser sign-in (ML-032): the engine binds its loopback redirect and sends
    /// `SignInOpen` with the page to open, waits for the browser, takes the username from the
    /// sign-in (the identity the token belongs to, which SMTP insists on: S5), checks IMAP and
    /// SMTP with the token, and answers `SignedIn`. With `account`, it signs an existing account
    /// in again: the new refresh token replaces the stored one and the account syncs again.
    SignIn {
        request: RequestId,
        provider: OAuthProvider,
        /// The address typed into the assistant, for the IMAP and SMTP endpoints' sake; never
        /// put into the sign-in URL.
        address: String,
        account: Option<AccountKey>,
    },
    /// Stops waiting for the browser; nothing is answered.
    CancelSignIn { request: RequestId },
    /// The Microsoft client ID in use (ADR-008: `config.toml`, else the build's), after every
    /// read of `config.toml` that changes it; `None` makes sign-in unavailable.
    MicrosoftClient { client_id: Option<String> },
    /// Delete the account's settings, secrets, rows and blobs (ML-038).
    RemoveAccount { request: RequestId, key: AccountKey },
    /// Set what Sealwax calls the account and its colour: its `label` and `color` lines in
    /// `accounts.toml`, `None` taking a line out (the user's UI tweak, 2026-10-08). The store's
    /// name follows with the `Accounts` the app sends once it has read the file.
    EditAccount {
        request: RequestId,
        key: AccountKey,
        label: Option<String>,
        color: Option<AccountColor>,
    },
    /// The accounts `accounts.toml` lists, after every read without mistakes: the store follows
    /// them, and every listed account gets its folder list and first INBOX sync.
    Accounts { accounts: Vec<AccountSettings> },
    /// The user opened `folder`: sync it now and keep it current while it stays open (ML-042).
    FolderOpened { folder: FolderId },
    /// The list reached the end of `folder`'s first-sync window: fetch older headers (ML-043).
    LoadOlder { folder: FolderId },
    /// Download the bodies of `messages`, most wanted first: the reader's message (ML-044).
    FetchBodies { messages: Vec<MessageId> },
    /// Download the part at mail-parser index `part` of `message` by the IMAP section the store
    /// recorded (M5, ML-061): an attachment, or an inline image, of a message stored as its text
    /// parts (`BodyState::Text`). Answered with one `PartFetched`; nothing is stored.
    FetchPart { message: MessageId, part: u32 },
    /// Download `message` whole as the server has it (M5, ML-063), for Show Original and Save
    /// as .eml of a message stored as its text parts. Answered with one `OriginalFetched`;
    /// nothing is stored.
    FetchOriginal { message: MessageId },
    /// `config.toml`'s `[storage]` (ML-044), after the engine starts and after every read that
    /// changes it. The disk cap is enforced once it arrives, and after every download.
    Storage { storage: Storage },
    /// The network came back or the machine resumed: drop every connection and reconnect now,
    /// without waiting for a socket to fail (ML-045, S6).
    Reconnect,
    /// Refresh (M8): check every account for new mail now, keeping its connections: an IMAP
    /// account runs an account pass, a POP3 account a pass. An account waiting out its backoff
    /// tries at once; one whose login was refused waits for `Reconnect` or a new password.
    CheckMail,
    /// A new password for a password account the server refused (ML-048): checked on IMAP and
    /// SMTP, stored in the keyring, and the account syncs again.
    UpdatePassword {
        request: RequestId,
        key: AccountKey,
        password: Password,
    },
    /// The app queued or undid triage of `account`'s messages in the store (ML-051): its actor
    /// runs the ops that are due now and wakes again when the next undo window ends. Queued ops
    /// also run after a start or reconnect without it; this only spares the wait.
    OpsQueued { account: AccountId },
    /// The app queued a draft to send, or undid a send, in the store (M6, ML-076): the
    /// account's actor sends what is due now and wakes again when the next undo window ends.
    /// Queued sends also go after a start or reconnect without it; this only spares the wait.
    OutboxQueued { account: AccountId },
    /// A draft of `account` was saved, closed or discarded in the store (M6, ML-074): its copy
    /// in the server's Drafts folder is brought up to date when it is due.
    DraftsChanged { account: AccountId },
    /// The submission server's size limit for `account` (EHLO SIZE, RFC 1870), for the
    /// composer's size check (ML-072). Answered with one `SubmissionLimit`.
    SubmissionLimit {
        request: RequestId,
        account: AccountId,
    },
    /// Run the account's rules on every message the store holds in `folder`, as if each had
    /// just arrived there (Apply Rules to This Folder, ADR-020); with `rule`, that Sieve script
    /// instead of the account's (a rule just saved in the Rules dialog, run on its Inbox). What
    /// they change waits `undo_window` before it reaches the server, as the user's own moves
    /// do, so Undo puts it back at once. Answered with one `RulesApplied`.
    ApplyRules {
        request: RequestId,
        folder: FolderId,
        rule: Option<String>,
        undo_window: std::time::Duration,
    },
    /// Create, rename or delete one of `account`'s folders (ADR-020). An IMAP account's change
    /// runs on its command connection, online only; a POP3 account's local folders change at
    /// once. The store follows before the answer, one `FolderChanged`.
    ChangeFolder {
        request: RequestId,
        account: AccountId,
        change: FolderChange,
    },
    /// Ask GitHub which release of Sealwax is the latest (Check for Updates, ADR-021
    /// amendment): one HTTPS request, only when the user runs the command. Answered with one
    /// `UpdateChecked`.
    CheckForUpdates { request: RequestId },
    /// Stop every connection and the runtime.
    Shutdown,
}

/// A change to an account's folders. Names are the user's: `Debug` leaves them out.
#[derive(Clone, PartialEq, Eq)]
pub enum FolderChange {
    /// A folder whose last level is `name`, under `parent`, or at the top of the account (under
    /// the server's personal prefix, where its folders share one).
    Create {
        parent: Option<FolderId>,
        name: String,
    },
    /// `folder`'s last level becomes `name`; the folders under it follow.
    Rename { folder: FolderId, name: String },
    /// `folder` and every folder under it, with their messages, on the server for good.
    Delete { folder: FolderId },
}

impl fmt::Debug for FolderChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FolderChange::Create { parent, .. } => f
                .debug_struct("Create")
                .field("parent", parent)
                .finish_non_exhaustive(),
            FolderChange::Rename { folder, .. } => f
                .debug_struct("Rename")
                .field("folder", folder)
                .finish_non_exhaustive(),
            FolderChange::Delete { folder } => {
                f.debug_struct("Delete").field("folder", folder).finish()
            }
        }
    }
}

#[derive(Debug)]
pub enum Event {
    /// Autodetect started asking `source`.
    DetectProgress {
        request: RequestId,
        source: DetectSource,
    },
    Detected {
        request: RequestId,
        detection: Detection,
    },
    /// `Ok` carries the username that logged in.
    Verified {
        request: RequestId,
        result: Result<String, LoginError>,
    },
    /// The SHA-256 of the certificate the server showed, or why none was read.
    CertificateRead {
        request: RequestId,
        result: Result<CertificatePin, LoginFailure>,
    },
    /// Open `url` in the browser for the sign-in; it can be opened again while the engine waits.
    SignInOpen { request: RequestId, url: String },
    /// `Ok` carries the username the sign-in belongs to, which IMAP and SMTP accepted. A failure
    /// before the IMAP check (the browser, the token endpoint, the ID token) is reported as
    /// IMAP's; a re-sign-in as another user than the account's is `WrongPassword`.
    SignedIn {
        request: RequestId,
        result: Result<String, LoginError>,
    },
    AccountAdded {
        request: RequestId,
        result: Result<AccountId, AccountError>,
    },
    AccountRemoved {
        request: RequestId,
        result: Result<(), AccountError>,
    },
    AccountEdited {
        request: RequestId,
        result: Result<(), AccountError>,
    },
    /// The folder list of an account was read and stored.
    FoldersSynced { account: AccountId, folders: u32 },
    /// Headers of `folder` arrived: `fetched` of `total` so far, newest first.
    SyncProgress {
        account: AccountId,
        folder: FolderId,
        fetched: u32,
        total: u32,
    },
    /// Where the account's sync stands; the sidebar and the banners show it (ML-048).
    AccountStatus {
        account: AccountId,
        status: SyncStatus,
    },
    /// A body download for `FetchBodies` finished; the store's change event shows the message.
    BodyFetched {
        message: MessageId,
        result: Result<(), BodyError>,
    },
    /// The answer to `FetchPart`.
    PartFetched {
        message: MessageId,
        part: u32,
        result: Result<FetchedPart, BodyError>,
    },
    /// The answer to `FetchOriginal`: the message's bytes as the server sent them.
    OriginalFetched {
        message: MessageId,
        result: Result<Original, BodyError>,
    },
    /// `LoadOlder` stored `fetched` older headers; `complete` when the folder has none left.
    OlderLoaded {
        folder: FolderId,
        fetched: u32,
        complete: bool,
    },
    PasswordUpdated {
        request: RequestId,
        result: Result<(), LoginError>,
    },
    /// The keyring didn't answer: secrets stay in memory until Sealwax quits (ADR-008).
    KeyringUnavailable,
    /// The server refused queued changes for good (ML-051), such as a move into a folder that
    /// is gone: the store has dropped the ops and put `messages` back as the server has them.
    OpsRefused {
        account: AccountId,
        change: Refused,
        messages: u32,
    },
    /// The answer to `SubmissionLimit`: the largest message in bytes the server takes, `None`
    /// when it names no limit.
    SubmissionLimit {
        request: RequestId,
        account: AccountId,
        result: Result<Option<u64>, SendError>,
    },
    /// The server took `draft` for delivery (ML-075); the store records it as sent.
    Sent { account: AccountId, draft: DraftId },
    /// `draft` can't be sent as it is, or the server refused it for good: the store has put it
    /// back as a draft whose send failed, for the user to change and send again.
    SendFailed {
        account: AccountId,
        draft: DraftId,
        error: SendError,
    },
    /// `draft`'s data may have reached the server, and `reason` says why Sealwax can't tell:
    /// it may have been sent. It is never sent again without the user (ADR-015).
    SendUncertain {
        account: AccountId,
        draft: DraftId,
        reason: UncertainReason,
    },
    /// Mail that arrived in `folder` after its first sync, above the UIDNEXT the store held,
    /// sent once the rows are committed (ML-083). Never for a first sync, older mail loaded
    /// below the floor, or a message Sealwax's own op moved there. Any folder and any flags:
    /// the app decides what notifies.
    NewMail {
        account: AccountId,
        folder: FolderId,
        messages: Vec<MessageId>,
    },
    /// `ApplyRules` ran: `Ok` counts the messages a rule moved, copied or flagged; `batch` is
    /// what Undo takes back, `None` when nothing was queued.
    RulesApplied {
        request: RequestId,
        folder: FolderId,
        result: Result<u32, RulesError>,
        batch: Option<BatchId>,
    },
    /// The account's rules file can't be run, so no rule runs until it changes (ADR-020). Sent
    /// once per change of the file.
    RulesFailed {
        account: AccountId,
        error: RulesError,
    },
    /// The answer to `ChangeFolder`, once the store holds the change.
    FolderChanged {
        request: RequestId,
        account: AccountId,
        result: Result<(), FolderError>,
    },
    /// The answer to `CheckForUpdates`: the latest release's version.
    UpdateChecked {
        request: RequestId,
        result: Result<Version, UpdateError>,
    },
}

/// Why an account's rules didn't run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RulesError {
    /// The script doesn't parse, or uses what Sealwax doesn't run. Its `Debug` leaves out the
    /// message; the app may show it.
    Script(sealwax_domain::sieve::Error),
    /// The file can't be read: over 1 MiB, not UTF-8, or not readable.
    File,
    /// The store couldn't be read or written.
    Store,
}

/// A part downloaded for `FetchPart`, its transfer encoding decoded. Message content: its
/// `Debug` prints only the type and the length.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchedPart {
    /// Lower case, `type/subtype`, from the part's MIME header.
    pub mime_type: String,
    pub data: Vec<u8>,
}

impl fmt::Debug for FetchedPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchedPart")
            .field("mime_type", &self.mime_type)
            .field("len", &self.data.len())
            .finish()
    }
}

/// A message downloaded whole for `FetchOriginal`. Its `Debug` prints only the length.
#[derive(Clone, PartialEq, Eq)]
pub struct Original(pub Vec<u8>);

impl fmt::Debug for Original {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Original({} bytes)", self.0.len())
    }
}

/// What kind of change the server refused, for the app's message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Read, unread, star or unstar.
    Flags,
    /// Archive, junk, not junk, move, or delete into the Trash.
    Move,
    /// Deleting for good.
    Expunge,
    /// Marking a whole folder read.
    FolderRead,
    /// A rule's copy into another folder.
    Copy,
}

/// Where an account's sync stands (ML-048).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// Logging in, or a pass is running.
    Syncing,
    /// The last pass finished: the account matches the server and waits for changes.
    Synced,
    /// The server can't be reached; the engine tries again by itself.
    Offline(LoginFailure),
    /// The server refused the account's password or token: the user types a password again or
    /// signs in again.
    NeedsSignIn(LoginFailure),
    /// The server's certificate no longer verifies.
    CertificateChanged,
    /// The server says the mailbox is full.
    QuotaFull,
}

impl SyncStatus {
    /// The state a failed login or sync leaves an account in.
    pub fn from_failure(failure: LoginFailure) -> Self {
        match failure {
            LoginFailure::WrongPassword
            | LoginFailure::AppPasswordRequired
            | LoginFailure::ImapDisabled
            | LoginFailure::OAuthRequired
            | LoginFailure::SignInDeclined => SyncStatus::NeedsSignIn(failure),
            LoginFailure::Certificate => SyncStatus::CertificateChanged,
            LoginFailure::NoTls
            | LoginFailure::Unreachable
            | LoginFailure::Timeout
            | LoginFailure::NoUidl
            | LoginFailure::Busy
            | LoginFailure::Other => SyncStatus::Offline(failure),
        }
    }

    /// The failure behind a failed state.
    pub fn failure(self) -> Option<LoginFailure> {
        match self {
            SyncStatus::Offline(failure) | SyncStatus::NeedsSignIn(failure) => Some(failure),
            SyncStatus::CertificateChanged => Some(LoginFailure::Certificate),
            SyncStatus::Syncing | SyncStatus::Synced | SyncStatus::QuotaFull => None,
        }
    }
}

/// Why a body download failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BodyError {
    #[error("the account is offline")]
    Offline,
    #[error("the account needs sign-in")]
    NeedsSignIn,
    #[error("the server no longer has the message")]
    Gone,
    #[error("the local store failed")]
    Store,
    #[error("the download failed")]
    Other,
}

/// Why a message couldn't be sent, or the size limit couldn't be read.
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("the account is offline")]
    Offline,
    #[error("the account needs sign-in")]
    NeedsSignIn,
    /// The server refused these recipients' addresses (RCPT TO).
    #[error("the server refused {} recipients", .0.len())]
    RecipientsRefused(Vec<String>),
    /// Larger than the server takes: its limit when it named one (EHLO SIZE), `None` when it
    /// only refused the message as too large (552).
    #[error("the message is larger than the server takes")]
    TooLarge { limit: Option<u64> },
    /// The server refused the message itself, or its sender address.
    #[error("the server refused the message")]
    Refused,
    #[error("the local store failed")]
    Store,
    #[error("sending failed")]
    Other,
    /// Sealwax stopped while it built the message (which may be what stopped it); it isn't
    /// built again until the user sends it again.
    #[error("Sealwax stopped while it built the message")]
    Interrupted,
}

/// Why a send's outcome is unknown (issue #2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UncertainReason {
    /// Sealwax stopped while the message was going out; found at the next start.
    Stopped,
    /// No reply to the message's end: the connection closed, broke or timed out.
    NoReply,
    /// The server replied, but its reply couldn't be read (such as a line longer than lettre
    /// takes), so whether it took the message is unknown.
    UnreadableReply,
}

/// Recipient addresses are the user's data: `Debug` prints only how many.
impl fmt::Debug for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::Offline => f.write_str("Offline"),
            SendError::NeedsSignIn => f.write_str("NeedsSignIn"),
            SendError::RecipientsRefused(recipients) => {
                write!(f, "RecipientsRefused({})", recipients.len())
            }
            SendError::TooLarge { limit } => write!(f, "TooLarge {{ limit: {limit:?} }}"),
            SendError::Refused => f.write_str("Refused"),
            SendError::Store => f.write_str("Store"),
            SendError::Other => f.write_str("Other"),
            SendError::Interrupted => f.write_str("Interrupted"),
        }
    }
}

/// Why a folder change didn't happen, or didn't all happen.
#[derive(Debug, thiserror::Error)]
pub enum FolderError {
    /// The account isn't connected; nothing was sent.
    #[error("the account is offline")]
    Offline,
    /// The name can't be used there; nothing was sent.
    #[error("the name can't be used here")]
    Name(NameProblem),
    /// A role folder (or a folder holding one) can't be renamed or deleted, and a folder that
    /// takes no subfolders gets none; nothing was sent.
    #[error("this folder can't change that way")]
    NotAllowed,
    /// The folder isn't in the store any more.
    #[error("the folder is gone")]
    Missing,
    /// The server refused, in its own words (shown, never logged). For a delete of several
    /// folders, the ones before the refusal are gone.
    #[error("the server refused")]
    Refused(ServerText),
    /// The connection broke before the server answered; the account pass reads the folders
    /// again.
    #[error("the connection was lost")]
    Lost,
    #[error("the local store failed")]
    Store,
}

/// Why Check for Updates has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("GitHub couldn't be reached")]
    Offline,
    #[error("GitHub didn't answer in time")]
    Timeout,
    #[error("GitHub's certificate didn't verify")]
    Certificate,
    /// GitHub limits requests without an account (60 an hour from one address).
    #[error("GitHub refused the request for now")]
    Busy,
    #[error("GitHub has no release")]
    NoRelease,
    #[error("GitHub's answer couldn't be read")]
    Unreadable,
}

/// Why saving or removing an account failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    #[error("the keyring refused the secret")]
    Keyring,
    #[error("accounts.toml could not be written")]
    SettingsFile,
    #[error("the local store failed")]
    Store,
    #[error("an account with this address exists")]
    Exists,
    #[error("no such account")]
    Missing,
    /// An OAuth account's add names no sign-in, or one that expired (after 30 minutes), was
    /// cancelled or was made for another address: sign in again.
    #[error("the sign-in expired")]
    SignInExpired,
}
