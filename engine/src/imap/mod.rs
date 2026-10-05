//! IMAP (ADR-006): async-imap 0.12 for framing, parsing and login, with every command going
//! through the raw layer that checks completions. M2 built the connection, the folder list and
//! the first header sync; M3 adds the state-driven re-sync, the first-sync window and older
//! mail (`sync`), with IDLE and polling on the same layer.

pub mod bodies;
mod connection;
mod error;
mod fetch;
mod first_sync;
mod folders;
pub mod mutf7;
mod raw;
pub mod roles;
mod sync;

pub use connection::{
    CONNECT_TIMEOUT, Caps, ConnectOptions, Connection, Credentials, Mechanism, connect,
    connect_with,
};
pub use error::{ImapError, ServerText, SyncError};
pub use fetch::PREVIEW_BYTES;
pub use first_sync::{BATCH, FirstSyncReport, Progress, first_sync, first_sync_with};
pub use folders::{FolderStatus, ListedFolder, SyncedFolders, list, sync_folders};
pub use raw::READ_TIMEOUT;
pub use sync::{
    Elsewhere, FolderLocks, Older, Stats, SyncOptions, SyncReport, load_older, sync_account,
    sync_folder,
};

pub(crate) type Stream = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;
