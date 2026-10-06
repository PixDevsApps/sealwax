//! A stand-in `org.freedesktop.Notifications` for headless runs (ML-083), ported from S6's
//! `s6-bench/fake.rs`: it records every `Notify` call (app_name, summary, body, actions, hints)
//! and every `CloseNotification`, and "clicks" on request: `ActivationToken`, `ActionInvoked`
//! with `default` or a button's action, then `NotificationClosed` as a dismissing server does.
//!
//! It runs only on `tools/headless.sh`'s private session bus: the display must be Broadway with
//! no Wayland display, the bus must be one that can start nothing (the wrapper's configuration
//! has no service folders; a desktop session's bus lists its activatable services), and it asks
//! for the name with DO_NOT_QUEUE, so it never waits in line behind the session's own
//! notification server. It has its own connection and thread, as a separate process would.

use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub const BUS_NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

/// What omarchy-shell 0.3.1 advertises (S6).
pub const CAPABILITIES: [&str; 6] = [
    "persistence",
    "body",
    "body-markup",
    "body-hyperlinks",
    "actions",
    "icon-static",
];

const XML: &str = r#"<node>
  <interface name="org.freedesktop.Notifications">
    <method name="GetCapabilities"><arg type="as" direction="out"/></method>
    <method name="Notify">
      <arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
      <arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/>
      <arg type="a{sv}" direction="in"/><arg type="i" direction="in"/>
      <arg type="u" direction="out"/>
    </method>
    <method name="CloseNotification"><arg type="u" direction="in"/></method>
    <method name="GetServerInformation">
      <arg type="s" direction="out"/><arg type="s" direction="out"/>
      <arg type="s" direction="out"/><arg type="s" direction="out"/>
    </method>
    <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
    <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
    <signal name="ActivationToken"><arg type="u"/><arg type="s"/></signal>
  </interface>
</node>"#;

/// One `Notify` call. It holds the notification's text, so its `Debug` prints lengths.
#[derive(Clone)]
pub struct Received {
    pub id: u32,
    pub at: Instant,
    /// The caller's unique bus name, where the signals go.
    pub sender: String,
    pub app_name: String,
    pub replaces: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    /// `(key, label)` pairs.
    pub actions: Vec<(String, String)>,
    /// Each hint's key and its value as GVariant text.
    pub hints: Vec<(String, String)>,
    pub expire_timeout: i32,
    /// Why it closed: 1 expired, 2 dismissed, 3 `CloseNotification`.
    pub closed: Option<u32>,
}

impl std::fmt::Debug for Received {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Received")
            .field("id", &self.id)
            .field("summary_chars", &self.summary.chars().count())
            .field("body_chars", &self.body.chars().count())
            .field("actions", &self.actions.len())
            .field("hints", &self.hints.len())
            .field("closed", &self.closed)
            .finish()
    }
}

impl Received {
    pub fn hint(&self, key: &str) -> Option<&str> {
        self.hints
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

type Record = Arc<Mutex<Vec<Received>>>;

/// The running stand-in; dropping it releases the name and ends its thread.
pub struct StandIn {
    record: Record,
    connection: gio::DBusConnection,
    main_loop: glib::MainLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Whether this process runs under `tools/headless.sh`: Broadway, and no Wayland display.
pub fn headless() -> bool {
    std::env::var("GDK_BACKEND").is_ok_and(|backend| backend == "broadway")
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
}

fn sync_call(
    connection: &gio::DBusConnection,
    method: &str,
    args: Option<&glib::Variant>,
) -> Result<glib::Variant, String> {
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            args,
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            2_000,
            gio::Cancellable::NONE,
        )
        .map_err(|error| error.to_string())
}

/// Connects to the session bus, checks it is the wrapper's private one, owns the name and
/// serves it on the calling thread's default context.
fn serve(
    record: &Record,
    capabilities: Vec<String>,
) -> Result<(gio::DBusConnection, gio::RegistrationId), String> {
    let address = gio::dbus_address_get_for_bus_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|error| error.to_string())?;
    let connection = gio::DBusConnection::for_address_sync(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        gio::Cancellable::NONE,
    )
    .map_err(|error| error.to_string())?;
    let activatable = sync_call(&connection, "ListActivatableNames", None)?
        .get::<(Vec<String>,)>()
        .map(|(names,)| names)
        .unwrap_or_default();
    if activatable
        .iter()
        .any(|name| name != "org.freedesktop.DBus")
    {
        return Err(format!(
            "this bus can start {} services: not tools/headless.sh's private bus",
            activatable.len() - 1
        ));
    }
    // DBUS_NAME_FLAG_DO_NOT_QUEUE; 1 is DBUS_REQUEST_NAME_REPLY_PRIMARY_OWNER.
    let owned = sync_call(
        &connection,
        "RequestName",
        Some(&(BUS_NAME, 4u32).to_variant()),
    )?
    .get::<(u32,)>();
    if owned != Some((1,)) {
        return Err(format!("{BUS_NAME} has an owner on this bus already"));
    }
    let info = gio::DBusNodeInfo::for_xml(XML)
        .map_err(|error| error.to_string())?
        .lookup_interface(BUS_NAME)
        .ok_or("no interface")?;
    let record = record.clone();
    let registration = connection
        .register_object(PATH, &info)
        .method_call(
            move |connection, sender, _, _, method, parameters, invocation| match method {
                "GetCapabilities" => {
                    invocation.return_value(Some(&(capabilities.clone(),).to_variant()));
                }
                "GetServerInformation" => invocation.return_value(Some(
                    &("sealwax-stand-in", "Sealwax self-test", "0.1", "1.2").to_variant(),
                )),
                "Notify" => {
                    let id = notify(&record, sender.unwrap_or_default(), &parameters);
                    invocation.return_value(Some(&(id,).to_variant()));
                }
                "CloseNotification" => {
                    let id: u32 = parameters.child_value(0).get().unwrap_or(0);
                    invocation.return_value(None);
                    if close(&record, id, 3) {
                        let _ = connection.emit_signal(
                            sender,
                            PATH,
                            BUS_NAME,
                            "NotificationClosed",
                            Some(&(id, 3u32).to_variant()),
                        );
                    }
                }
                other => invocation.return_dbus_error(
                    "org.freedesktop.DBus.Error.UnknownMethod",
                    &format!("no method {other}"),
                ),
            },
        )
        .build()
        .map_err(|error| error.to_string())?;
    Ok((connection, registration))
}

fn lock(record: &Record) -> std::sync::MutexGuard<'_, Vec<Received>> {
    record.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Records a `Notify` call; returns its id (the replaced one's, when it replaces).
fn notify(record: &Record, sender: &str, parameters: &glib::Variant) -> u32 {
    let mut list = lock(record);
    let string = |index: usize| {
        parameters
            .child_value(index)
            .str()
            .unwrap_or_default()
            .to_owned()
    };
    let replaces: u32 = parameters.child_value(1).get().unwrap_or(0);
    let id = if replaces != 0 {
        replaces
    } else {
        list.iter().map(|received| received.id).max().unwrap_or(0) + 1
    };
    let flat: Vec<String> = parameters.child_value(5).get().unwrap_or_default();
    let actions = flat
        .chunks(2)
        .map(|pair| (pair[0].clone(), pair.get(1).cloned().unwrap_or_default()))
        .collect();
    let hints = parameters
        .child_value(6)
        .iter()
        .filter_map(|entry| {
            let key = entry.child_value(0).str()?.to_owned();
            let value = entry.child_value(1).as_variant()?;
            let value = match (value.str(), value.get::<u8>()) {
                (Some(text), _) => text.to_owned(),
                (None, Some(byte)) => byte.to_string(),
                (None, None) => value.print(false).to_string(),
            };
            Some((key, value))
        })
        .collect();
    list.push(Received {
        id,
        at: Instant::now(),
        sender: sender.to_owned(),
        app_name: string(0),
        replaces,
        app_icon: string(2),
        summary: string(3),
        body: string(4),
        actions,
        hints,
        expire_timeout: parameters.child_value(7).get().unwrap_or(-1),
        closed: None,
    });
    id
}

/// Marks the open notifications with `id` closed; false when none was open.
fn close(record: &Record, id: u32, reason: u32) -> bool {
    let mut closed = false;
    for received in lock(record)
        .iter_mut()
        .filter(|received| received.id == id && received.closed.is_none())
    {
        received.closed = Some(reason);
        closed = true;
    }
    closed
}

impl StandIn {
    /// Starts the stand-in on its own thread with `capabilities`; returns once it owns the
    /// name, or why it refused.
    pub fn start(capabilities: &[&str]) -> Result<StandIn, String> {
        if !headless() {
            return Err(
                "the stand-in notification server only runs under tools/headless.sh".into(),
            );
        }
        let record: Record = Arc::default();
        let capabilities: Vec<String> = capabilities.iter().map(|c| (*c).to_owned()).collect();
        let (ready, wait) = std::sync::mpsc::channel();
        let shared = record.clone();
        let thread = std::thread::Builder::new()
            .name("stand-in-notifyd".into())
            .spawn(move || {
                let context = glib::MainContext::new();
                let main_loop = glib::MainLoop::new(Some(&context), false);
                let served = context.with_thread_default(|| match serve(&shared, capabilities) {
                    Ok((connection, registration)) => {
                        let _ = ready.send(Ok((connection.clone(), main_loop.clone())));
                        main_loop.run();
                        let _ = connection.unregister_object(registration);
                        let _ =
                            sync_call(&connection, "ReleaseName", Some(&(BUS_NAME,).to_variant()));
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                });
                if let Err(error) = served {
                    let _ = ready.send(Err(error.to_string()));
                }
            })
            .map_err(|error| error.to_string())?;
        match wait.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((connection, main_loop))) => Ok(StandIn {
                record,
                connection,
                main_loop,
                thread: Some(thread),
            }),
            Ok(Err(error)) => Err(format!("stand-in notification server: {error}")),
            Err(_) => Err("the stand-in notification server did not start".into()),
        }
    }

    /// Every `Notify` call so far, oldest first.
    pub fn received(&self) -> Vec<Received> {
        lock(&self.record).clone()
    }

    fn emit(&self, destination: &str, name: &str, value: glib::Variant) -> Result<(), String> {
        self.connection
            .emit_signal(Some(destination), PATH, BUS_NAME, name, Some(&value))
            .map_err(|error| error.to_string())
    }

    /// A click on the open notification `id`: an activation token, `ActionInvoked` with `key`
    /// (`default`, or a button's action key), then `NotificationClosed` (dismissed). Returns when
    /// `ActionInvoked` went out.
    pub fn click(&self, id: u32, key: &str) -> Result<Instant, String> {
        let sender = lock(&self.record)
            .iter()
            .rev()
            .find(|received| received.id == id && received.closed.is_none())
            .map(|received| received.sender.clone())
            .ok_or_else(|| format!("notification {id} isn't open"))?;
        self.emit(
            &sender,
            "ActivationToken",
            (id, format!("stand-in-token-{id}")).to_variant(),
        )?;
        let invoked = Instant::now();
        self.emit(&sender, "ActionInvoked", (id, key).to_variant())?;
        if close(&self.record, id, 2) {
            self.emit(&sender, "NotificationClosed", (id, 2u32).to_variant())?;
        }
        Ok(invoked)
    }

    /// The open notification `id` expires on screen (`NotificationClosed`, reason 1).
    pub fn expire(&self, id: u32) -> Result<(), String> {
        let sender = lock(&self.record)
            .iter()
            .rev()
            .find(|received| received.id == id && received.closed.is_none())
            .map(|received| received.sender.clone())
            .ok_or_else(|| format!("notification {id} isn't open"))?;
        close(&self.record, id, 1);
        self.emit(&sender, "NotificationClosed", (id, 1u32).to_variant())
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.main_loop.quit();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_refuses_outside_the_headless_wrapper() {
        if headless() {
            return;
        }
        let refused = StandIn::start(&CAPABILITIES).err();
        assert_eq!(
            refused.as_deref(),
            Some("the stand-in notification server only runs under tools/headless.sh")
        );
    }
}
