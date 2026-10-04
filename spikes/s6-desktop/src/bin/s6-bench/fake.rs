//! A stand-in `org.freedesktop.Notifications` server for headless runs on the wrapper's private
//! bus. It records what the app sends and can "click": `ActivationToken` and `ActionInvoked`
//! for the default action of the first notification and the first button of the second. It
//! refuses to start unless it runs headless, and asks for the name with DO_NOT_QUEUE, so it can
//! never stand in line behind the live session's notification server.

use crate::runner::headless;
use anyhow::{Result, bail};
use email_spike_s6::clock::mono_ns;
use gio::glib;
use gio::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[derive(Clone, Debug)]
pub struct Received {
    pub id: u32,
    pub t: i64,
    pub app_name: String,
    pub summary: String,
    pub actions: Vec<String>,
    pub hints: Vec<String>,
}

/// Which notifications the fake server clicks, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clicks {
    None,
    /// The first notification's default action (with an activation token), then the second's
    /// first button.
    DefaultThenButton,
    /// Every notification's default action, each with an activation token.
    AllDefault,
}

#[derive(Clone, Default)]
pub struct Fake {
    pub received: Arc<Mutex<Vec<Received>>>,
    pub clicks: Arc<Mutex<Vec<(u32, String, i64)>>>,
}

impl Fake {
    pub fn received(&self) -> Vec<Received> {
        self.received
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clicks(&self) -> Vec<(u32, String, i64)> {
        self.clicks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Starts the server on its own thread and main context; returns once it owns the name.
pub fn start(policy: Clicks) -> Result<Fake> {
    if !headless() {
        bail!("the fake notification server only runs under tools/headless.sh");
    }
    let fake = Fake::default();
    let (ready, wait) = std::sync::mpsc::channel::<Result<(), String>>();
    let shared = fake.clone();
    std::thread::Builder::new()
        .name("s6-fake-notifyd".into())
        .spawn(move || {
            let context = glib::MainContext::new();
            let main_loop = glib::MainLoop::new(Some(&context), false);
            let served = context.with_thread_default(|| match serve(&shared, policy) {
                Ok(connection) => {
                    let _ = ready.send(Ok(()));
                    main_loop.run();
                    drop(connection);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            });
            if let Err(error) = served {
                let _ = ready.send(Err(error.to_string()));
            }
        })?;
    match wait.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(fake),
        Ok(Err(error)) => bail!("fake notification server: {error}"),
        Err(_) => bail!("fake notification server did not start"),
    }
}

/// Owns the name and registers the object; the returned connection must outlive the loop.
fn serve(fake: &Fake, policy: Clicks) -> Result<gio::DBusConnection, String> {
    let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(connection) => connection,
        Err(error) => return Err(error.to_string()),
    };
    let owned = connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&("org.freedesktop.Notifications", 4u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.get::<(u32,)>());
    if owned != Some((1,)) {
        return Err(format!("could not own the name: {owned:?}"));
    }
    let info = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(node) => match node.lookup_interface("org.freedesktop.Notifications") {
            Some(info) => info,
            None => return Err("no interface".into()),
        },
        Err(error) => return Err(error.to_string()),
    };
    let received = fake.received.clone();
    let clicks = fake.clicks.clone();
    let registration = connection
        .register_object("/org/freedesktop/Notifications", &info)
        .method_call(
            move |connection, sender, _, _, method, parameters, invocation| match method {
                "GetCapabilities" => invocation.return_value(Some(
                    &(vec!["actions", "body", "body-markup", "persistence"],).to_variant(),
                )),
                "GetServerInformation" => invocation.return_value(Some(
                    &("s6-fake-notifyd", "Mail spike S6", "0.1", "1.2").to_variant(),
                )),
                "Notify" => {
                    let t = mono_ns();
                    let mut list = received.lock().unwrap_or_else(|e| e.into_inner());
                    let replaces: u32 = parameters.child_value(1).get().unwrap_or(0);
                    let id = if replaces != 0 {
                        replaces
                    } else {
                        list.len() as u32 + 1
                    };
                    let actions: Vec<String> = parameters.child_value(5).get().unwrap_or_default();
                    let hints = parameters
                        .child_value(6)
                        .iter()
                        .filter_map(|entry| entry.child_value(0).str().map(str::to_owned))
                        .collect();
                    let index = list.len();
                    list.push(Received {
                        id,
                        t,
                        app_name: parameters.child_value(0).str().unwrap_or("").to_owned(),
                        summary: parameters.child_value(3).str().unwrap_or("").to_owned(),
                        actions: actions.clone(),
                        hints,
                    });
                    drop(list);
                    invocation.return_value(Some(&(id,).to_variant()));
                    let click = match (policy, index) {
                        (Clicks::AllDefault, _) | (Clicks::DefaultThenButton, 0) => {
                            Some((true, "default".to_owned()))
                        }
                        (Clicks::DefaultThenButton, 1) => {
                            actions.get(2).map(|key| (false, key.clone()))
                        }
                        _ => None,
                    };
                    if let Some((token, key)) = click {
                        let destination = sender.map(str::to_owned);
                        let connection = connection.clone();
                        let clicks = clicks.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(300));
                            let emit = |name: &str, value: glib::Variant| {
                                let _ = connection.emit_signal(
                                    destination.as_deref(),
                                    "/org/freedesktop/Notifications",
                                    "org.freedesktop.Notifications",
                                    name,
                                    Some(&value),
                                );
                            };
                            if token {
                                emit(
                                    "ActivationToken",
                                    (id, format!("s6-fake-token-{id}")).to_variant(),
                                );
                            }
                            let t = mono_ns();
                            emit("ActionInvoked", (id, key.clone()).to_variant());
                            clicks
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push((id, key, t));
                        });
                    }
                }
                "CloseNotification" => {
                    let id: u32 = parameters.child_value(0).get().unwrap_or(0);
                    invocation.return_value(None);
                    let _ = connection.emit_signal(
                        sender,
                        "/org/freedesktop/Notifications",
                        "org.freedesktop.Notifications",
                        "NotificationClosed",
                        Some(&(id, 3u32).to_variant()),
                    );
                }
                other => invocation.return_dbus_error(
                    "org.freedesktop.DBus.Error.UnknownMethod",
                    &format!("no method {other}"),
                ),
            },
        )
        .build();
    registration.map_err(|error| error.to_string())?;
    Ok(connection)
}
