//! What the app sends the engine and what it hears back: plain `Send` values over async-channel
//! (ADR-003). M2 covers account setup and the first sync; M3 grows it into the sync engine's API.

use sealwax_domain::account::{
    AccountKey, AccountSettings, DetectSource, Detection, LoginError, LoginFailure,
};
use sealwax_domain::model::{AccountId, FolderId};
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
    /// Save a verified account: the password to the keyring, the settings to `accounts.toml`,
    /// the account to the store; then list its folders and sync its INBOX headers.
    AddAccount {
        request: RequestId,
        settings: AccountSettings,
        password: Password,
    },
    /// Delete the account's settings, secrets, rows and blobs (ML-038).
    RemoveAccount { request: RequestId, key: AccountKey },
    /// The accounts `accounts.toml` lists, after every read without mistakes: the store follows
    /// them, and every listed account gets its folder list and first INBOX sync.
    Accounts { accounts: Vec<AccountSettings> },
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
    /// The account's folder list and first INBOX sync are done.
    SyncFinished { account: AccountId },
    /// An account's connection failed; the sidebar shows its state.
    SyncFailed {
        account: AccountId,
        failure: LoginFailure,
    },
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
}
