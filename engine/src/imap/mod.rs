//! IMAP (ADR-006): async-imap 0.12 for framing, parsing and login, with every command going
//! through the raw layer that checks completions. M2 builds the connection, the folder list
//! and the first header sync; M3 builds re-sync, IDLE and polling on the same layer.

mod connection;
mod error;
mod fetch;
mod first_sync;
mod folders;
pub mod mutf7;
mod raw;
pub mod roles;

pub use connection::{CONNECT_TIMEOUT, Caps, Connection, Credentials, Mechanism, connect};
pub use error::{ImapError, ServerText, SyncError};
pub use fetch::PREVIEW_BYTES;
pub use first_sync::{BATCH, FirstSyncReport, Progress, first_sync};
pub use folders::{ListedFolder, SyncedFolders, list, sync_folders};
pub use raw::READ_TIMEOUT;

pub(crate) type Stream = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;
