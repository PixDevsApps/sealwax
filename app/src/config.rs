use std::time::Duration;

/// Working ids until the name is decided in M0 (ADR-010).
pub const APP_ID: &str = if cfg!(debug_assertions) {
    "io.github.pixdevsapps.Mail.Devel"
} else {
    "io.github.pixdevsapps.Mail"
};

pub const EXIT_AFTER_ENV: &str = "MAIL_EXIT_AFTER_MS";

/// `MAIL_EXIT_AFTER_MS`, for headless smoke runs.
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
