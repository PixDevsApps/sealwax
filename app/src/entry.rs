//! The program's start, shared by `sealwax` and `sealwax-selftest` (`uitest`), so a self-test's
//! second instance (`current_exe()`) is a whole Sealwax too.

use crate::application::{self, AppConfig};
use crate::{activation, cli, config, webview};
use gtk4::glib;
use std::time::Instant;

/// Runs Sealwax with the process's command line and environment.
pub fn main() -> glib::ExitCode {
    let started = Instant::now();
    let sandbox_off = std::env::var_os(config::SANDBOX_OFF_ENV).is_some();
    if sandbox_off {
        // SAFETY: Sealwax has one thread here (GTK, WebKit, the logger and the engine start
        // theirs later), so nothing else reads or writes the environment meanwhile.
        unsafe { std::env::remove_var(config::SANDBOX_OFF_ENV) };
    }
    // GLib sends the process name as a notification's app_name (S6).
    glib::set_prgname(Some("sealwax"));
    glib::set_application_name("Sealwax");
    if let Err(error) = sealwax_infrastructure::logging::init() {
        eprintln!("Could not initialize logging: {error}");
        return glib::ExitCode::FAILURE;
    }
    if sandbox_off {
        tracing::warn!(
            "{} was set; Sealwax cleared it, so WebKit's sandbox stays on",
            config::SANDBOX_OFF_ENV
        );
    }
    // Parsed here first, so a typo never reaches a running Sealwax.
    let parsed = match cli::parse(std::env::args_os()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let _ = error.print();
            return glib::ExitCode::from(error.exit_code() as u8);
        }
    };
    let exit_after = match config::exit_after_from_env() {
        Ok(exit_after) => exit_after,
        Err(message) => {
            tracing::error!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };
    // In a Flatpak WebKit gives each web process a sandbox of its own through the Flatpak
    // portal, and starts them without one when it can't. Its check runs beside the start; mail
    // waits for it (SECURITY.md, layer 8).
    webview::start_flatpak_check();
    #[cfg(feature = "test-hooks")]
    if let Some(script) = parsed.self_test {
        return crate::selftest::run(&script, started);
    }
    let config = AppConfig::from_env(exit_after);
    // In a Flatpak the bus could start another Sealwax with the same id outside the sandbox
    // (an installed package's service file), so the command line becomes the primary itself.
    if !parsed.service && !sealwax_infrastructure::sandbox::is_flatpak() {
        let outcome = activation::prepare(&config.app_id);
        tracing::debug!(?outcome, "command line");
    }
    application::run(config, started)
}
