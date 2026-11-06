//! The Background portal (`org.freedesktop.portal.Background`): how a Flatpak asks the desktop
//! to let it run without a window and to start it at login (INTEGRATIONS.md "Flatpak"). GNOME
//! and KDE have it; Omarchy's portals (hyprland, gtk) don't, which [`Outcome::Unavailable`]
//! says. A request is answered on its own `org.freedesktop.portal.Request` object, whose path
//! follows from the bus name and a token Sealwax picks, so the answer is subscribed to before
//! the call. Used only in a Flatpak.

use crate::autostart::{BACKGROUND_OPTION, PROGRAM};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const BACKGROUND: &str = "org.freedesktop.portal.Background";
const REQUEST: &str = "org.freedesktop.portal.Request";

/// How long the desktop may take to answer, its question to the user included.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

/// What the desktop allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Allowed {
    pub background: bool,
    pub autostart: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Answered(Allowed),
    /// The user dismissed the desktop's question.
    Cancelled,
    /// The desktop has no Background portal.
    Unavailable,
    Failed(String),
}

/// The request's object path: the caller's unique name without `:` and with `_` for `.`.
pub(crate) fn handle_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{DESKTOP_PATH}/request/{sender}/{token}")
}

/// `RequestBackground`'s parameters: no parent window, Sealwax's token and reason, and
/// `sealwax --background` as the command a login starts.
pub(crate) fn parameters(token: &str, reason: &str, autostart: bool) -> glib::Variant {
    let options = glib::VariantDict::new(None);
    options.insert_value("handle_token", &token.to_variant());
    options.insert_value("reason", &reason.to_variant());
    options.insert_value("autostart", &autostart.to_variant());
    options.insert_value(
        "commandline",
        &vec![PROGRAM.to_owned(), BACKGROUND_OPTION.to_owned()].to_variant(),
    );
    options.insert_value("dbus-activatable", &false.to_variant());
    glib::Variant::tuple_from_iter(["".to_variant(), options.end()])
}

/// The `Response` signal's code and results.
pub(crate) fn outcome(code: u32, results: &HashMap<String, glib::Variant>) -> Outcome {
    let flag = |key: &str| {
        results
            .get(key)
            .and_then(glib::Variant::get::<bool>)
            .unwrap_or(false)
    };
    match code {
        0 => Outcome::Answered(Allowed {
            background: flag("background"),
            autostart: flag("autostart"),
        }),
        1 => Outcome::Cancelled,
        _ => Outcome::Failed("the desktop ended the request".to_owned()),
    }
}

fn token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "sealwax_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Asks the desktop to let Sealwax run in the background, and to start it at login when
/// `autostart` (false turns a login start off again). Each request says both.
pub(crate) async fn request_background(reason: &str, autostart: bool) -> Outcome {
    let connection = match gio::bus_get_future(gio::BusType::Session).await {
        Ok(connection) => connection,
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    let version = connection
        .call_future(
            Some(DESKTOP),
            DESKTOP_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(BACKGROUND, "version").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            5_000,
        )
        .await;
    if let Err(error) = version {
        tracing::info!(%error, "no Background portal");
        return Outcome::Unavailable;
    }
    let Some(unique_name) = connection.unique_name() else {
        return Outcome::Failed("the session bus gave no name".to_owned());
    };
    let token = token();
    let expected = handle_path(&unique_name, &token);
    let (sender, answers) = async_channel::bounded(1);
    let subscribe = |path: &str| {
        let sender = sender.clone();
        connection.subscribe_to_signal(
            Some(DESKTOP),
            Some(REQUEST),
            Some("Response"),
            Some(path),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if let Some((code, results)) = signal
                    .parameters
                    .get::<(u32, HashMap<String, glib::Variant>)>()
                {
                    let _ = sender.try_send(outcome(code, &results));
                }
            },
        )
    };
    let mut subscriptions = vec![subscribe(&expected)];
    let reply = connection
        .call_future(
            Some(DESKTOP),
            DESKTOP_PATH,
            BACKGROUND,
            "RequestBackground",
            Some(&parameters(&token, reason, autostart)),
            glib::VariantTy::new("(o)").ok(),
            gio::DBusCallFlags::NONE,
            30_000,
        )
        .await;
    match reply
        .as_ref()
        .ok()
        .and_then(|reply| reply.get::<(glib::variant::ObjectPath,)>())
    {
        Some((path,)) if path.as_str() != expected => {
            // A portal older than 0.9 picks its own path.
            subscriptions.push(subscribe(path.as_str()));
        }
        Some(_) => {}
        None => {
            let error = reply
                .err()
                .map_or_else(|| "an unexpected reply".to_owned(), |e| e.to_string());
            return Outcome::Failed(error);
        }
    }
    let answer = glib::future_with_timeout(ANSWER_TIMEOUT, answers.recv()).await;
    drop(subscriptions);
    match answer {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => Outcome::Failed("the answer was lost".to_owned()),
        Err(_) => Outcome::Failed("the desktop didn't answer".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_path_follows_the_bus_name() {
        assert_eq!(
            handle_path(":1.42", "sealwax_7_0"),
            "/org/freedesktop/portal/desktop/request/1_42/sealwax_7_0"
        );
        let token = token();
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "{token}"
        );
        assert_ne!(token, super::token());
    }

    #[test]
    fn the_request_names_the_background_command() {
        let parameters = parameters("t", "Mail keeps syncing", true);
        assert_eq!(parameters.type_().as_str(), "(sa{sv})");
        let (parent, options) = parameters
            .get::<(String, HashMap<String, glib::Variant>)>()
            .unwrap();
        assert_eq!(parent, "");
        assert_eq!(options["handle_token"].get::<String>().unwrap(), "t");
        assert_eq!(options["autostart"].get::<bool>(), Some(true));
        assert_eq!(options["dbus-activatable"].get::<bool>(), Some(false));
        assert_eq!(
            options["commandline"].get::<Vec<String>>().unwrap(),
            ["sealwax", "--background"]
        );
    }

    #[test]
    fn answers_are_read_with_missing_flags_as_no() {
        let mut results = HashMap::new();
        results.insert("background".to_owned(), true.to_variant());
        assert_eq!(
            outcome(0, &results),
            Outcome::Answered(Allowed {
                background: true,
                autostart: false
            })
        );
        assert_eq!(outcome(1, &results), Outcome::Cancelled);
        assert!(matches!(outcome(2, &HashMap::new()), Outcome::Failed(_)));
    }
}
