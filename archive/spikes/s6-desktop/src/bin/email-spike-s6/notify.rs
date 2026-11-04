//! Notifications through `gio::Notification`, and a watch on this process's own session-bus
//! connection: a GDBus filter sees GLib's outgoing `Notify` call with its arguments, the
//! server's reply (the notification id, i.e. whether it was accepted) and the signals the
//! server sends back (`ActionInvoked`, `ActivationToken`, `NotificationClosed`). Nothing else on
//! the bus is observed.

use email_spike_s6::clock::mono_ns;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

pub const BUS_NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

#[derive(Clone, Debug)]
pub enum BusEvent {
    Notify {
        serial: u32,
        t: i64,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        actions: Vec<String>,
        hints: Vec<String>,
        desktop_entry: Option<String>,
        urgency: Option<u8>,
        image_path: Option<String>,
        expire_timeout: i32,
    },
    Close {
        serial: u32,
        t: i64,
        id: u32,
    },
    Reply {
        serial: u32,
        t: i64,
        id: Option<u32>,
        error: Option<String>,
    },
    ActionInvoked {
        t: i64,
        id: u32,
        key: String,
    },
    ActivationToken {
        t: i64,
        id: u32,
        token_len: usize,
        token: String,
    },
    Closed {
        t: i64,
        id: u32,
        reason: u32,
    },
}

fn string(value: &glib::Variant) -> String {
    value.str().unwrap_or_default().to_owned()
}

fn notify_call(message: &gio::DBusMessage, t: i64) -> Option<BusEvent> {
    let body = message.body()?;
    if body.n_children() < 8 {
        return None;
    }
    let hints = body.child_value(6);
    let dict = glib::VariantDict::new(Some(&hints));
    let keys = hints
        .iter()
        .filter_map(|entry| entry.child_value(0).str().map(str::to_owned))
        .collect();
    Some(BusEvent::Notify {
        serial: message.serial(),
        t,
        app_name: string(&body.child_value(0)),
        replaces_id: body.child_value(1).get().unwrap_or(0),
        app_icon: string(&body.child_value(2)),
        actions: body.child_value(5).get().unwrap_or_default(),
        hints: keys,
        desktop_entry: dict.lookup("desktop-entry").ok().flatten(),
        urgency: dict.lookup("urgency").ok().flatten(),
        image_path: dict.lookup("image-path").ok().flatten(),
        expire_timeout: body.child_value(7).get().unwrap_or(-1),
    })
}

fn signal(message: &gio::DBusMessage, t: i64) -> Option<BusEvent> {
    let body = message.body()?;
    match message.member()?.as_str() {
        "ActionInvoked" => Some(BusEvent::ActionInvoked {
            t,
            id: body.child_value(0).get()?,
            key: string(&body.child_value(1)),
        }),
        "ActivationToken" => {
            let token = string(&body.child_value(1));
            Some(BusEvent::ActivationToken {
                t,
                id: body.child_value(0).get()?,
                token_len: token.len(),
                token,
            })
        }
        "NotificationClosed" => Some(BusEvent::Closed {
            t,
            id: body.child_value(0).get()?,
            reason: body.child_value(1).get()?,
        }),
        _ => None,
    }
}

/// Installs the filter. Runs on GDBus's worker thread, so it only touches thread-safe things.
pub fn watch(connection: &gio::DBusConnection) -> async_channel::Receiver<BusEvent> {
    let (sender, receiver) = async_channel::unbounded();
    let pending: Arc<Mutex<HashSet<u32>>> = Arc::default();
    connection.add_filter(move |_, message, incoming| {
        let t = mono_ns();
        let event = match (incoming, message.message_type()) {
            (false, gio::DBusMessageType::MethodCall)
                if message.interface().as_deref() == Some(BUS_NAME) =>
            {
                let member = message.member();
                match member.as_deref() {
                    Some("Notify") => notify_call(message, t),
                    Some("CloseNotification") => message
                        .body()
                        .and_then(|body| body.child_value(0).get::<u32>())
                        .map(|id| BusEvent::Close {
                            serial: message.serial(),
                            t,
                            id,
                        }),
                    _ => None,
                }
                .inspect(|event| {
                    if let BusEvent::Notify { serial, .. } | BusEvent::Close { serial, .. } = event
                    {
                        pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(*serial);
                    }
                })
            }
            (true, gio::DBusMessageType::MethodReturn | gio::DBusMessageType::Error)
                if pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&message.reply_serial()) =>
            {
                Some(BusEvent::Reply {
                    serial: message.reply_serial(),
                    t,
                    id: message
                        .body()
                        .filter(|body| body.n_children() == 1)
                        .and_then(|body| body.child_value(0).get::<u32>()),
                    error: message.error_name().map(|name| name.to_string()),
                })
            }
            (true, gio::DBusMessageType::Signal)
                if message.interface().as_deref() == Some(BUS_NAME) =>
            {
                signal(message, t)
            }
            _ => None,
        };
        if let Some(event) = event {
            let _ = sender.try_send(event);
        }
        Some(message.clone())
    });
    receiver
}

#[derive(Clone, Debug, Default)]
pub struct ServerInfo {
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub spec: String,
    pub capabilities: Vec<String>,
    pub error: Option<String>,
}

pub async fn server_info(connection: &gio::DBusConnection) -> ServerInfo {
    let call = |method: &'static str| {
        connection.call_future(
            Some(BUS_NAME),
            PATH,
            BUS_NAME,
            method,
            None,
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            2000,
        )
    };
    let mut info = ServerInfo::default();
    match call("GetServerInformation").await {
        Ok(reply) => {
            if let Some((name, vendor, version, spec)) =
                reply.get::<(String, String, String, String)>()
            {
                info.name = name;
                info.vendor = vendor;
                info.version = version;
                info.spec = spec;
            }
        }
        Err(error) => info.error = Some(error.to_string()),
    }
    match call("GetCapabilities").await {
        Ok(reply) => {
            info.capabilities = reply
                .get::<(Vec<String>,)>()
                .map(|(caps,)| caps)
                .unwrap_or_default()
        }
        Err(error) => info.error = Some(error.to_string()),
    }
    info
}

/// A new-mail notification: the default action opens the message, two buttons act on it.
pub fn new_mail(title: &str, body: &str, target: &str) -> gio::Notification {
    let notification = gio::Notification::new(title);
    notification.set_body(Some(body));
    notification.set_priority(gio::NotificationPriority::Low);
    notification.set_category(Some("email.arrived"));
    notification.set_icon(&gio::ThemedIcon::new("mail-unread-symbolic"));
    notification
        .set_default_action_and_target_value("app.open-message", Some(&target.to_variant()));
    notification.add_button_with_target_value(
        "Archive",
        "app.archive-message",
        Some(&target.to_variant()),
    );
    notification.add_button_with_target_value(
        "Mark read",
        "app.mark-read",
        Some(&target.to_variant()),
    );
    notification
}
