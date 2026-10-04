use std::time::Duration;

/// The app-id (ADR-010); debug builds use `.Devel`, so they run beside an installed build.
pub const APP_ID: &str = if cfg!(debug_assertions) {
    "io.github.pixdevsapps.Sealwax.Devel"
} else {
    "io.github.pixdevsapps.Sealwax"
};

pub const EXIT_AFTER_ENV: &str = "SEALWAX_EXIT_AFTER_MS";

/// Test hooks for the startup measurement: the launcher's `CLOCK_MONOTONIC` time in ns, taken
/// just before it starts Sealwax, so `first-frame-from-exec` counts from the process start as
/// S6 did; and a fullscreen first window, so live runs don't re-tile the desktop.
pub const EXEC_NS_ENV: &str = "SEALWAX_EXEC_NS";
pub const START_FULLSCREEN_ENV: &str = "SEALWAX_START_FULLSCREEN";

/// `SEALWAX_EXIT_AFTER_MS`, for headless smoke runs.
pub fn exit_after_from_env() -> Result<Option<Duration>, String> {
    match std::env::var(EXIT_AFTER_ENV) {
        Err(_) => Ok(None),
        Ok(ms) => ms
            .parse()
            .map(|ms| Some(Duration::from_millis(ms)))
            .map_err(|error| {
                format!("{EXIT_AFTER_ENV} must be a whole number of milliseconds: {error}")
            }),
    }
}
