use std::time::Duration;

/// The app-id (ADR-010); debug builds use `.Devel`, so they run beside an installed build.
pub const APP_ID: &str = if cfg!(debug_assertions) {
    "io.github.pixdevsapps.Sealwax.Devel"
} else {
    "io.github.pixdevsapps.Sealwax"
};

/// The name of Sealwax's folders in each XDG base directory: debug builds keep their own, so a
/// `cargo run` never opens the installed app's store, accounts or rules.
pub const DIR_NAME: &str = if cfg!(debug_assertions) {
    sealwax_infrastructure::xdg::DEVEL_DIR
} else {
    sealwax_infrastructure::xdg::APP_DIR
};

pub const EXIT_AFTER_ENV: &str = "SEALWAX_EXIT_AFTER_MS";

/// Test hooks for the startup measurement: the launcher's `CLOCK_MONOTONIC` time in ns, taken
/// just before it starts Sealwax, so `first-frame-from-exec` counts from the process start as
/// S6 did; and a fullscreen first window, so live runs don't re-tile the desktop.
pub const EXEC_NS_ENV: &str = "SEALWAX_EXEC_NS";
pub const START_FULLSCREEN_ENV: &str = "SEALWAX_START_FULLSCREEN";

/// WebKit starts its web processes outside their sandbox when this is set; `main` clears it in
/// every build before any thread starts, so the sandbox stays on (SECURITY.md, layer 8; M9).
pub const SANDBOX_OFF_ENV: &str = "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS";

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_development_build_keeps_its_own_folders() {
        // Tests are debug builds, as `cargo run` is.
        assert_eq!(APP_ID, "io.github.pixdevsapps.Sealwax.Devel");
        assert_eq!(DIR_NAME, "sealwax-devel");
    }
}
