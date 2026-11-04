//! The S6 app: a single-instance GTK window over a cached 10k INBOX. A second invocation with a
//! `mailto:` URI forwards it to the running instance and exits. Modes (`MAIL_S6_MODE`) cover
//! startup timing, forwarding, notifications, background mode and the user's manual checks.
//! Every run ends by itself (`MAIL_EXIT_AFTER_MS`).

mod app;
mod config;
mod desktop;
mod engine;
mod model;
mod notify;
mod prewarm;
mod probe;
mod row;
mod store;
mod theme;
mod window;

use email_spike_s6::clock::mono_ns;
use email_spike_s6::env;
use email_spike_s6::events::emit;
use gtk4::{gio, glib};
use libadwaita::prelude::*;
use serde_json::json;

fn main() -> glib::ExitCode {
    let t_main = mono_ns();
    let exec_ns = std::env::var(env::EXEC_NS)
        .ok()
        .and_then(|value| value.parse::<i64>().ok());
    let config = match config::Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("email-spike-s6: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = app::S6App::new(config, t_main, exec_ns);
    let t_app = mono_ns();
    if let Err(error) = app.register(gio::Cancellable::NONE) {
        eprintln!("email-spike-s6: could not register: {error}");
        return glib::ExitCode::FAILURE;
    }
    let t_registered = mono_ns();
    let remote = app.is_remote();
    if remote {
        emit(
            "remote",
            json!({
                "exec": exec_ns,
                "main": t_main,
                "app": t_app,
                "dbus_register": app.state().stamp_value("dbus-register"),
                "registered": t_registered,
            }),
        );
    }
    let arguments: Vec<String> = std::env::args().collect();
    let code = app.run_with_args(&arguments);
    if remote {
        emit("remote-done", json!({ "code": code.get() }));
    }
    code
}
