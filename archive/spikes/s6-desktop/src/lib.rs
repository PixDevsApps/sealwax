//! Spike S6: desktop integration (M0, ML-014) — startup with a cached INBOX, single instance
//! and `mailto:` forwarding, notifications on omarchy-shell, background mode with IDLE, resume
//! and GDK_SCALE. Measurement code only; production crates never depend on it. See
//! docs/spikes/S6-desktop.md.

pub mod cache;
pub mod clock;
pub mod events;
pub mod evict;
pub mod mailto;
pub mod paths;
pub mod relay;
pub mod stats;

/// A spike-only application id, so nothing here collides with the real app.
pub const APP_ID: &str = "io.github.pixdevsapps.Mail.SpikeS6";
pub const APP_PATH: &str = "/io/github/pixdevsapps/Mail/SpikeS6";

/// Environment variables shared by the app and the bench.
pub mod env {
    /// `CLOCK_MONOTONIC` nanoseconds taken by the launcher right before it spawned the process.
    pub const EXEC_NS: &str = "MAIL_S6_EXEC_NS";
    pub const MODE: &str = "MAIL_S6_MODE";
    pub const DB: &str = "MAIL_S6_DB";
    pub const EXIT_AFTER_MS: &str = "MAIL_EXIT_AFTER_MS";
    pub const LINGER_MS: &str = "MAIL_S6_LINGER_MS";
    pub const FIRST_ROWS: &str = "MAIL_S6_FIRST_ROWS";
    pub const SNAPSHOT_ROWS: &str = "MAIL_S6_SNAPSHOT_ROWS";
    pub const WINDOWED: &str = "MAIL_S6_WINDOWED";
    pub const MAPS_OUT: &str = "MAIL_S6_MAPS_OUT";
    pub const IMAP_PORT: &str = "MAIL_S6_IMAP_PORT";
    pub const IMAP_SERVER: &str = "MAIL_S6_IMAP_SERVER";
    pub const IMAP_USER: &str = "MAIL_S6_IMAP_USER";
    pub const RUN_DIR: &str = "MAIL_S6_RUN_DIR";
    pub const BACKGROUND_CLOSE: &str = "MAIL_S6_CLOSE_WHEN_IDLING";
}
