//! IMAP (ADR-006): async-imap 0.12 for framing, parsing and login, with every command going
//! through the raw layer that checks completions. M2 built the connection, the folder list and
//! the first header sync; M3 adds the state-driven re-sync, the first-sync window and older
//! mail (`sync`), with IDLE and polling on the same layer. M4 adds the op queue's changes on the
//! server (`write`).

pub mod bodies;
mod connection;
mod error;
mod fetch;
mod first_sync;
mod folders;
#[cfg(any(test, feature = "fuzz"))]
pub(crate) mod fuzz;
pub mod hidden;
mod idle;
pub mod manage;
pub mod mutf7;
mod raw;
pub mod roles;
mod sync;
pub mod write;

pub(crate) use connection::xoauth2_response;
pub use connection::{
    CONNECT_TIMEOUT, Caps, ConnectOptions, Connection, Credentials, Mechanism, connect,
    connect_with,
};
pub use error::{ImapError, ServerText, SyncError};
pub use fetch::PREVIEW_BYTES;
pub use first_sync::{BATCH, FirstSyncReport, Progress, first_sync, first_sync_with};
pub use folders::{FolderStatus, ListedFolder, SyncedFolders, list, sync_folders};
pub use idle::{IDLE_COMMAND_TIMEOUT, Idled, Stop, Wake};
pub use raw::READ_TIMEOUT;
pub use sync::{
    Arrived, Elsewhere, FolderLocks, Older, Stats, SyncOptions, SyncReport, Unreadable, load_older,
    sync_account, sync_folder,
};

pub(crate) type Stream = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;
