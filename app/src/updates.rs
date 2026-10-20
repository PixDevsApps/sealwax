//! Check for Updates (ADR-021 amendment, 2026-10-20): only when the user runs it, the engine asks
//! GitHub which release is the latest, and a dialog says whether this Sealwax is it. When a newer
//! one is out, the dialog shows the command that installs it, with a button that copies it;
//! Sealwax never installs anything itself.

use crate::accessibility;
use crate::application::AppState;
use crate::window::Window;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::updates::{self, Outcome, Version};
use sealwax_engine::api::{Command, Event, UpdateError};
use std::cell::Cell;
use std::rc::Rc;

/// The dialog's widget name, for the self-tests.
pub const DIALOG: &str = "check-for-updates";
/// The command's label.
pub const COMMAND: &str = "update-command";
/// The button that copies the command.
pub const COPY: &str = "update-copy";

pub const CHECKING: &str = "Checking for updates…";

thread_local! {
    static OPEN: Cell<bool> = const { Cell::new(false) };
}

/// The running Sealwax's version.
pub fn running() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_default()
}

/// What the dialog says once the engine answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub heading: String,
    pub body: String,
    /// The command that installs the newer release.
    pub command: Option<String>,
}

pub fn answer(running: Version, result: Result<Version, UpdateError>, keymap: &Keymap) -> Answer {
    let latest = match result {
        Ok(latest) => latest,
        Err(error) => {
            return Answer {
                heading: "Couldn't check for updates".to_owned(),
                body: failure(error).to_owned(),
                command: None,
            };
        }
    };
    match updates::compare(running, latest) {
        Outcome::Latest => Answer {
            heading: "You have the latest version".to_owned(),
            body: format!("Sealwax {running} is the newest release."),
            command: None,
        },
        Outcome::Ahead(latest) => Answer {
            heading: "You have the latest version".to_owned(),
            body: format!("This Sealwax, {running}, is newer than the latest release, {latest}."),
            command: None,
        },
        Outcome::Newer(latest) => {
            let quit = match keymap.shown(ActionId::Quit).first() {
                Some(key) => format!("press {} in Sealwax", key.display()),
                None => "choose Quit in Sealwax's menu".to_owned(),
            };
            Answer {
                heading: format!("Sealwax {latest} is available"),
                body: format!(
                    "You have {running}. To update, copy this command, paste it into a terminal \
                     and press Enter; it asks for your computer's password. Then {quit} and \
                     start Sealwax again. Your mail, accounts and settings stay as they are."
                ),
                command: Some(updates::update_command(latest)),
            }
        }
    }
}

fn failure(error: UpdateError) -> &'static str {
    match error {
        UpdateError::Offline => {
            "GitHub, where Sealwax's releases are, couldn't be reached. Check your internet \
             connection and try again."
        }
        UpdateError::Timeout => "GitHub didn't answer in time. Try again later.",
        UpdateError::Certificate => {
            "The connection to GitHub couldn't be verified, so its answer wasn't used. A network \
             that intercepts secure connections can cause this."
        }
        UpdateError::Busy => {
            "GitHub is limiting requests from your network right now. Try again in an hour."
        }
        UpdateError::NoRelease => "GitHub has no Sealwax release to compare with.",
        UpdateError::Unreadable => "GitHub's answer couldn't be read. Try again later.",
    }
}

/// Installs Check for Updates, once at startup.
pub fn install(state: &Rc<AppState>) {
    let Some(actions) = state.app_actions() else {
        return;
    };
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::CheckForUpdates, move || {
        if let Some(state) = weak.upgrade()
            && let Some(window) = state.window()
        {
            check(&state, &window);
        }
    });
}

fn check(state: &AppState, window: &Window) {
    if OPEN.with(|open| open.replace(true)) {
        return;
    }
    let keymap = state
        .preferences()
        .map(|prefs| prefs.keymap())
        .unwrap_or_default();
    let engine = state.engine();
    let request = engine.next_request();
    let (sender, answers) = async_channel::bounded(1);
    let subscription = engine.subscribe(move |event| {
        if let Event::UpdateChecked {
            request: answered,
            result,
        } = event
            && *answered == request
        {
            let _ = sender.try_send(*result);
        }
    });
    let dialog = adw::AlertDialog::builder()
        .heading(CHECKING)
        .body("Asking GitHub for the latest release of Sealwax.")
        .close_response("close")
        .default_response("close")
        .build();
    dialog.set_widget_name(DIALOG);
    dialog.add_response("close", "Cancel");
    dialog.set_extra_child(Some(&adw::Spinner::new()));
    let closed = Rc::new(Cell::new(false));
    dialog.connect_closed({
        let closed = closed.clone();
        move |_| {
            closed.set(true);
            OPEN.with(|open| open.set(false));
        }
    });
    dialog.present(Some(window.window()));
    let sent = engine.send(Command::CheckForUpdates { request });
    glib::spawn_future_local(async move {
        let _subscription = subscription;
        let result = match sent {
            Ok(()) => match answers.recv().await {
                Ok(result) => result,
                Err(_) => return,
            },
            Err(_) => {
                tracing::warn!("Check for Updates has no engine to ask");
                Err(UpdateError::Offline)
            }
        };
        if closed.get() {
            return;
        }
        let answer = answer(running(), result, &keymap);
        dialog.set_heading(Some(&answer.heading));
        dialog.set_body(&answer.body);
        dialog.set_response_label("close", "Close");
        match &answer.command {
            Some(command) => dialog.set_extra_child(Some(&command_box(command))),
            None => dialog.set_extra_child(gtk::Widget::NONE),
        }
        accessibility::announce(
            dialog.upcast_ref::<gtk::Widget>(),
            &format!("{}. {}", answer.heading, answer.body),
        );
    });
}

/// The command, selectable in the terminal's font, and a button that copies it.
fn command_box(command: &str) -> gtk::Box {
    let label = gtk::Label::builder()
        .label(command)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::Char)
        .xalign(0.0)
        .css_classes(["card", "sw-update-command"])
        .build();
    label.set_widget_name(COMMAND);
    // A break in the URL gets no hyphen, which would read as part of it.
    let attributes = gtk::pango::AttrList::new();
    attributes.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
    label.set_attributes(Some(&attributes));
    let copy = gtk::Button::builder()
        .label("Copy Command")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    copy.set_widget_name(COPY);
    let text = command.to_owned();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&text);
        button.set_label("Copied");
        accessibility::announce(button, "The update command is copied");
    });
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.append(&label);
    column.append(&copy);
    column
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn says_whether_this_is_the_latest() {
        let keymap = Keymap::default();
        let same = answer(v("0.5.0"), Ok(v("0.5.0")), &keymap);
        assert_eq!(same.heading, "You have the latest version");
        assert_eq!(same.body, "Sealwax 0.5.0 is the newest release.");
        assert_eq!(same.command, None);
        let ahead = answer(v("0.5.1"), Ok(v("0.5.0")), &keymap);
        assert_eq!(ahead.heading, "You have the latest version");
        assert!(
            ahead
                .body
                .contains("0.5.1, is newer than the latest release, 0.5.0")
        );
        assert_eq!(ahead.command, None);
    }

    #[test]
    fn a_newer_release_comes_with_its_command() {
        let newer = answer(v("0.5.0"), Ok(v("0.6.0")), &Keymap::default());
        assert_eq!(newer.heading, "Sealwax 0.6.0 is available");
        assert!(newer.body.starts_with("You have 0.5.0."), "{}", newer.body);
        assert!(
            newer.body.contains("press Ctrl+Q in Sealwax"),
            "{}",
            newer.body
        );
        assert_eq!(newer.command, Some(updates::update_command(v("0.6.0"))));
        let unbound = Keymap::with_overrides([(ActionId::Quit, Vec::new())]);
        let newer = answer(v("0.5.0"), Ok(v("0.6.0")), &unbound);
        assert!(
            newer.body.contains("choose Quit in Sealwax's menu"),
            "{}",
            newer.body
        );
    }

    #[test]
    fn failures_say_what_to_do() {
        for error in [
            UpdateError::Offline,
            UpdateError::Timeout,
            UpdateError::Certificate,
            UpdateError::Busy,
            UpdateError::NoRelease,
            UpdateError::Unreadable,
        ] {
            let failed = answer(v("0.5.0"), Err(error), &Keymap::default());
            assert_eq!(failed.heading, "Couldn't check for updates");
            assert!(!failed.body.is_empty());
            assert_eq!(failed.command, None);
        }
    }

    #[test]
    fn the_running_version_reads() {
        assert_eq!(running().to_string(), env!("CARGO_PKG_VERSION"));
    }
}
