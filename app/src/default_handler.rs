//! Make Sealwax the Default Mail App… (PRD C8, ML-078; INTEGRATIONS.md "Default mail handler"):
//! the one change Sealwax makes outside its own folders, and only when the user says yes. A
//! dialog says what changes and how to undo it; on yes, GIO makes Sealwax's desktop entry the
//! handler for `x-scheme-handler/mailto`, writing `$XDG_CONFIG_HOME/mimeapps.list` as
//! `xdg-mime default` does. When Sealwax already is the default it says so and changes nothing;
//! a build whose desktop entry isn't installed (a development build) says why it can't. GIO's
//! reads and the write run off the GTK thread.

use crate::window::Window;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::path::{Path, PathBuf};

/// The content type of `mailto:` links.
pub const MAILTO_TYPE: &str = "x-scheme-handler/mailto";

/// The desktop entry an app-id is installed as (`DBusActivatable` needs the two to match).
pub fn desktop_id(app_id: &str) -> String {
    format!("{app_id}.desktop")
}

/// An application that handles `mailto:` links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handler {
    /// Its desktop entry, `bluemail.desktop`.
    pub id: String,
    pub name: String,
}

impl Handler {
    fn describe(&self) -> String {
        if self.name.is_empty() || self.name == self.id {
            self.id.clone()
        } else {
            format!("{}, {}", self.name, self.id)
        }
    }
}

/// What the desktop looks like before the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Sealwax's desktop entry for this build's app-id.
    pub ours: String,
    /// Whether the desktop knows that entry (it is installed).
    pub installed: bool,
    /// The handler of `mailto:` links now.
    pub current: Option<Handler>,
    /// The file the change is written to.
    pub list: PathBuf,
    /// A development build (`.Devel`), which packaging never installs.
    pub development: bool,
}

/// What the command does with a [`Probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Sealwax already handles `mailto:` links: nothing to change.
    Already,
    /// No desktop entry to point the handler at.
    Missing,
    /// Ask, then change.
    Ask,
}

pub fn decide(probe: &Probe) -> Decision {
    if probe
        .current
        .as_ref()
        .is_some_and(|handler| handler.id == probe.ours)
    {
        Decision::Already
    } else if !probe.installed {
        Decision::Missing
    } else {
        Decision::Ask
    }
}

/// Looks the desktop up; blocking (GIO reads the desktop entries and the mimeapps lists).
pub fn probe(app_id: &str) -> Probe {
    let ours = desktop_id(app_id);
    Probe {
        installed: installed(&ours).is_some(),
        current: current(),
        ours,
        list: glib::user_config_dir().join("mimeapps.list"),
        development: app_id.ends_with(".Devel"),
    }
}

/// The handler of `mailto:` links now; blocking.
pub fn current() -> Option<Handler> {
    let info = gio::AppInfo::default_for_type(MAILTO_TYPE, false)?;
    Some(Handler {
        id: info.id()?.to_string(),
        name: info.display_name().to_string(),
    })
}

/// The installed application whose desktop entry is `id`; blocking. GIO lists every entry,
/// those hidden from menus too.
fn installed(id: &str) -> Option<gio::AppInfo> {
    gio::AppInfo::all()
        .into_iter()
        .find(|info| info.id().is_some_and(|candidate| candidate == id))
}

/// Makes desktop entry `ours` the handler of `mailto:` links and returns the handler GIO
/// reports afterwards; blocking.
pub fn apply(ours: &str) -> Result<Option<Handler>, String> {
    let info = installed(ours).ok_or_else(|| format!("{ours} isn't installed any more"))?;
    info.set_as_default_for_type(MAILTO_TYPE)
        .map_err(|error| error.to_string())?;
    Ok(current())
}

/// `path` with the home folder as `~`.
fn shown(path: &Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The question: what changes, where, and how to undo it.
pub fn question(probe: &Probe) -> (String, String) {
    let now = probe
        .current
        .as_ref()
        .map_or_else(|| "no app".to_owned(), Handler::describe);
    let undo = match &probe.current {
        Some(handler) => format!("xdg-mime default {} {MAILTO_TYPE}", handler.id),
        None => format!(
            "remove the {MAILTO_TYPE} line under [Default Applications] in {}",
            shown(&probe.list)
        ),
    };
    (
        "Make Sealwax the default mail app?".to_owned(),
        format!(
            "Links that start with mailto:, in your browser and other apps, will open a new \
             message in Sealwax.\n\nSealwax sets the desktop's handler for {MAILTO_TYPE} to {} \
             in {} (now: {now}). Nothing else changes.\n\nTo undo: {undo}",
            probe.ours,
            shown(&probe.list),
        ),
    )
}

/// What the dialog says when the command can't or needn't change anything.
pub fn explanation(probe: &Probe) -> (String, String) {
    match decide(probe) {
        Decision::Already => (
            "Sealwax is already the default mail app".to_owned(),
            format!(
                "The desktop opens mailto: links with {}. Nothing was changed.",
                probe.ours
            ),
        ),
        _ if probe.development => (
            "A development build can't be the default mail app".to_owned(),
            format!(
                "This is a development build, which has no desktop entry ({} isn't installed), \
                 so the desktop couldn't start it for a mailto: link. Install Sealwax's package \
                 and use this command in the installed Sealwax. Nothing was changed.",
                probe.ours
            ),
        ),
        _ => (
            "Sealwax can't be the default mail app yet".to_owned(),
            format!(
                "Sealwax's desktop entry, {}, isn't installed (Sealwax's package installs it), \
                 so the desktop couldn't start Sealwax for a mailto: link. Nothing was changed.",
                probe.ours
            ),
        ),
    }
}

/// The command: looks the desktop up, asks, changes, and says how it went.
pub fn run(window: &Window, app_id: &str) {
    let window = window.clone();
    let app_id = app_id.to_owned();
    glib::spawn_future_local(async move {
        let Ok(probe) = gio::spawn_blocking(move || probe(&app_id)).await else {
            window.add_toast(adw::Toast::new("Couldn't look up the default mail app"));
            return;
        };
        if decide(&probe) != Decision::Ask {
            let (heading, body) = explanation(&probe);
            tell(&window, &heading, &body).await;
            return;
        }
        let (heading, body) = question(&probe);
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body(body)
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.set_widget_name("default-mail-app");
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("make-default", "Make Default");
        dialog.set_response_appearance("make-default", adw::ResponseAppearance::Suggested);
        let answer = dialog.choose_future(Some(window.window())).await;
        if answer != "make-default" {
            return;
        }
        let ours = probe.ours.clone();
        match gio::spawn_blocking(move || apply(&ours)).await {
            Ok(Ok(Some(after))) if after.id == probe.ours => {
                tracing::info!("Sealwax is now the desktop's mailto: handler");
                window.add_toast(adw::Toast::new("Sealwax is now the default mail app"));
            }
            Ok(Ok(after)) => {
                let still = after.map_or_else(|| "no app".to_owned(), |after| after.describe());
                tell(
                    &window,
                    "The desktop still opens mailto: links elsewhere",
                    &format!(
                        "Sealwax was written to {} as the handler for {MAILTO_TYPE}, but the \
                         desktop still reports {still}: a desktop-specific list (such as \
                         ~/.config/hyprland-mimeapps.list) may name another app first.",
                        shown(&probe.list)
                    ),
                )
                .await;
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "the mailto: handler couldn't be set");
                tell(
                    &window,
                    "Sealwax couldn't be made the default mail app",
                    &error,
                )
                .await;
            }
            Err(_) => window.add_toast(adw::Toast::new("Setting the default mail app failed")),
        }
    });
}

async fn tell(window: &Window, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("close")
        .default_response("close")
        .build();
    dialog.set_widget_name("default-mail-app");
    dialog.add_response("close", "Close");
    dialog.choose_future(Some(window.window())).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(current: Option<&str>, installed: bool) -> Probe {
        Probe {
            ours: "io.github.pixdevsapps.Sealwax.desktop".into(),
            installed,
            current: current.map(|id| Handler {
                id: id.into(),
                name: "Mail".into(),
            }),
            list: PathBuf::from("/nowhere/.config/mimeapps.list"),
            development: false,
        }
    }

    #[test]
    fn asks_only_when_there_is_something_to_change() {
        assert_eq!(
            decide(&probe(Some("io.github.pixdevsapps.Sealwax.desktop"), true)),
            Decision::Already
        );
        assert_eq!(
            decide(&probe(Some("bluemail.desktop"), false)),
            Decision::Missing
        );
        assert_eq!(decide(&probe(None, false)), Decision::Missing);
        assert_eq!(decide(&probe(Some("bluemail.desktop"), true)), Decision::Ask);
        assert_eq!(decide(&probe(None, true)), Decision::Ask);
        assert_eq!(
            desktop_id("io.github.pixdevsapps.Sealwax"),
            "io.github.pixdevsapps.Sealwax.desktop"
        );
    }

    #[test]
    fn the_question_says_what_changes_and_how_to_undo_it() {
        let (heading, body) = question(&probe(Some("bluemail.desktop"), true));
        assert_eq!(heading, "Make Sealwax the default mail app?");
        assert!(body.contains("io.github.pixdevsapps.Sealwax.desktop"), "{body}");
        assert!(body.contains("now: Mail, bluemail.desktop"), "{body}");
        assert!(
            body.contains("To undo: xdg-mime default bluemail.desktop x-scheme-handler/mailto"),
            "{body}"
        );
        let (_, body) = question(&probe(None, true));
        assert!(body.contains("now: no app"), "{body}");
        assert!(body.contains("To undo: remove the x-scheme-handler/mailto line"));
    }

    #[test]
    fn development_builds_say_why_they_cant() {
        let mut missing = probe(Some("bluemail.desktop"), false);
        missing.development = true;
        let (heading, body) = explanation(&missing);
        assert_eq!(
            heading,
            "A development build can't be the default mail app"
        );
        assert!(body.contains("Install Sealwax's package"), "{body}");
        missing.development = false;
        assert_eq!(
            explanation(&missing).0,
            "Sealwax can't be the default mail app yet"
        );
    }
}
