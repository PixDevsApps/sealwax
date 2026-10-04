//! Local I/O with std only: store, blobs, configuration, Omarchy readers, logging.
//! No network, no tokio, no GTK.

pub mod logging;
pub mod omarchy;
pub mod settings_files;
pub mod xdg;
