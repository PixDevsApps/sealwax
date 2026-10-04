//! Local I/O with std only: store, blobs, configuration, Omarchy readers, logging.
//! No network, no tokio, no GTK.

pub mod logging;
pub mod xdg;

pub mod blobs;
pub mod ingest;
pub mod store;
