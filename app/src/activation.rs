//! D-Bus activation for a command line. When no Sealwax runs yet, a command line — from a
//! terminal, or `xdg-open mailto:…` running the desktop file's `Exec` line — first asks the
//! session bus to start the installed service (`StartServiceByName`, served by the `.service`
//! file) and then hands its command line to that instance as a remote. So the running Sealwax is
//! always the one the bus started, as when a launcher activates it (`DBusActivatable=true`), and
//! closing the terminal or the browser that ran the command doesn't end it.
//!
//! Without a service file for the id (a development build: the `.Devel` id has none), without a
//! session bus, or when activation fails, the command line becomes the primary itself.
//!
//! Everything here runs before GTK starts, on the main thread, and blocks only this process.

use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::time::{Duration, Instant};

const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const SERVICE_UNKNOWN: &str = "org.freedesktop.DBus.Error.ServiceUnknown";

/// How long starting the service may take before this process becomes the primary itself.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How the command line reaches a primary instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A Sealwax with this id runs already; GApplication forwards the command line to it.
    Running,
    /// The bus started the service from its `.service` file.
    Activated,
    /// No session bus, no service, or activation failed: this process becomes the primary.
    Primary,
}

/// Makes sure a primary instance owns `app_id` when that is possible; see the module docs.
pub fn prepare(app_id: &str) -> Outcome {
    let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(connection) => connection,
        Err(error) => {
            tracing::info!(%error, "no session bus; this process is the primary instance");
            return Outcome::Primary;
        }
    };
    if has_owner(&connection, app_id) {
        return Outcome::Running;
    }
    let started = Instant::now();
    match start_service(&connection, app_id) {
        Ok(()) => {
            tracing::debug!(
                ms = started.elapsed().as_secs_f64() * 1e3,
                "the session bus started Sealwax"
            );
            Outcome::Activated
        }
        Err(error) if gio::DBusError::remote_error(&error).as_deref() == Some(SERVICE_UNKNOWN) => {
            Outcome::Primary
        }
        Err(error) => {
            tracing::warn!(%error, "D-Bus activation failed; this process is the primary instance");
            Outcome::Primary
        }
    }
}

fn has_owner(connection: &gio::DBusConnection, name: &str) -> bool {
    connection
        .call_sync(
            Some(BUS_NAME),
            BUS_PATH,
            BUS_NAME,
            "NameHasOwner",
            Some(&(name,).to_variant()),
            Some(glib::VariantTy::new("(b)").expect("a valid type")),
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.get::<(bool,)>())
        .is_some_and(|(owned,)| owned)
}

/// `StartServiceByName`: the bus answers once the started service owns the name.
fn start_service(connection: &gio::DBusConnection, name: &str) -> Result<(), glib::Error> {
    connection
        .call_sync(
            Some(BUS_NAME),
            BUS_PATH,
            BUS_NAME,
            "StartServiceByName",
            Some(&(name, 0u32).to_variant()),
            Some(glib::VariantTy::new("(u)").expect("a valid type")),
            gio::DBusCallFlags::NONE,
            START_TIMEOUT.as_millis() as i32,
            gio::Cancellable::NONE,
        )
        .map(|_| ())
}
