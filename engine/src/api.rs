//! What the app sends the engine and what it hears back: plain `Send` values over async-channel
//! (ADR-003). M2 covers account setup and the first sync; M3 adds the sync engine's commands:
//! folders opened, older mail, bodies, reconnecting, a new password, and the account's status.
//! M4 adds the op queue's: the app queues triage in the store and wakes the account, and hears
//! when the server refuses a change for good. M5 adds downloads the reader asks for: one part of
//! a large message, or a message whole as the server has it.

use sealwax_domain::account::{
    AccountKey, AccountSettings, DetectSource, Detection, LoginError, LoginFailure, OAuthProvider,
};
use sealwax_domain::config::Storage;
use sealwax_domain::model::{AccountId, FolderId, MessageId};
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
    /// Stop every connection and the runtime.
    Shutdown,
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
