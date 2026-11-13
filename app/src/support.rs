//! Help for everyone, with or without a GitHub account (ADR-028): User Guide opens the online
//! guide, Contact Support… shows the support address with the details a message to support
//! needs, and About Sealwax is libadwaita's About dialog with the same links. Nothing is sent
//! by itself: Write to Support opens a composer the user reads and sends, and the details say
//! nothing about the user's mail, accounts or files (`domain::support`).

use crate::accessibility;
use crate::actions::ActionSet;
use crate::application::{AppState, SealwaxApplication};
use crate::composer::{self, ComposeRequest};
use crate::test_hooks::Log;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::support::{self, Details, Install};
use std::cell::Cell;
use std::rc::Rc;

/// The Contact Support dialog's widget name, for the self-tests.
pub(crate) const DIALOG: &str = "contact-support";
/// The About dialog's widget name.
pub(crate) const ABOUT: &str = "about-sealwax";
/// The support address's label and the button that copies it.
pub(crate) const ADDRESS: &str = "support-address";
pub(crate) const COPY_ADDRESS: &str = "support-copy-address";
/// The details' label and the button that copies them.
pub(crate) const DETAILS: &str = "support-details";
pub(crate) const COPY_DETAILS: &str = "support-copy-details";

/// The dialog's responses.
pub(crate) const CLOSE: &str = "close";
pub(crate) const GUIDE: &str = "guide";
pub(crate) const WRITE: &str = "write";

pub(crate) const HEADING: &str = "Contact Support";
const BODY: &str = "Write to Sealwax's support and say what happened and what you expected. \
     The details below say which Sealwax you have and where it runs, and nothing about your mail \
     or accounts; they help find the cause. Many answers are in the user guide.";
const BODY_NO_ACCOUNT: &str = "Write to Sealwax's support from your webmail or another mail app, \
     and say what happened and what you expected. Copy the address and the details below into \
     your message: they say which Sealwax you have and where it runs, and nothing about your mail. \
     Many answers are in the user guide.";

thread_local! {
    static OPEN: Cell<bool> = const { Cell::new(false) };
    /// The links opened while a self-test records, in place of the browser.
    static OPENED: Log<String> = const { Log::new() };
}

/// Installs User Guide, Contact Support… and About Sealwax, once at startup.
pub(crate) fn install(state: &Rc<AppState>, actions: &ActionSet, app_id: &str) {
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::UserGuide, move || {
        if let Some(state) = weak.upgrade() {
            open_link(&state, support::GUIDE_URL);
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::ContactSupport, move || {
        if let Some(state) = weak.upgrade() {
            contact(&state);
        }
    });
    let weak = Rc::downgrade(state);
    let app_id = app_id.to_owned();
    actions.connect(ActionId::About, move || {
        if let Some(state) = weak.upgrade() {
            about(&state, &app_id);
        }
    });
}

/// Opens `uri` in the user's browser (or, for `mailto:`, their mail app).
fn open_link(state: &AppState, uri: &str) {
    if OPENED.with(|opened| opened.push(uri.to_owned())).is_some() {
        return;
    }
    let window = state.window().map(|window| window.window().clone());
    gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |result| {
        if result.is_err() {
            tracing::info!("a help link didn't open");
        }
    });
}

/// The links opened while a self-test recorded, oldest first.
#[cfg(feature = "test-hooks")]
pub(crate) fn opened() -> Vec<String> {
    OPENED.with(Log::entries)
}

/// The details of this Sealwax; `os_release` is the system's os-release file, read off the GTK
/// thread.
fn details(state: &AppState, os_release: Option<&str>) -> Details {
    let under_usr = std::env::current_exe().is_ok_and(|path| path.starts_with("/usr/"));
    let version = |major: u32, minor: u32, micro: u32| format!("{major}.{minor}.{micro}");
    Details {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        install: Install::new(
            sealwax_infrastructure::sandbox::is_flatpak(),
            cfg!(debug_assertions),
            under_usr,
        ),
        system: os_release.and_then(support::os_name),
        architecture: std::env::consts::ARCH.to_owned(),
        desktop: std::env::var("XDG_CURRENT_DESKTOP").ok(),
        session: std::env::var("XDG_SESSION_TYPE").ok(),
        gtk: version(
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
        ),
        libadwaita: version(
            adw::major_version(),
            adw::minor_version(),
            adw::micro_version(),
        ),
        webkit: version(
            webkit6::functions::major_version(),
            webkit6::functions::minor_version(),
            webkit6::functions::micro_version(),
        ),
        accounts: state.mailbox().accounts().len(),
    }
}

/// The details, with the system's name read off the GTK thread.
async fn read_details(state: &AppState) -> Details {
    let os_release = gio::spawn_blocking(sealwax_infrastructure::sandbox::os_release)
        .await
        .ok()
        .flatten();
    details(state, os_release.as_deref())
}

/// Shows Contact Support over the main window, shown first if it isn't.
fn contact(state: &Rc<AppState>) {
    if OPEN.with(|open| open.replace(true)) {
        return;
    }
    let state = state.clone();
    glib::spawn_future_local(async move {
        let details = read_details(&state).await;
        let Some(window) = state.window() else {
            OPEN.with(|open| open.set(false));
            return;
        };
        let dialog = contact_dialog(&details);
        dialog.connect_closed(|_| OPEN.with(|open| open.set(false)));
        let weak = Rc::downgrade(&state);
        let app = window
            .window()
            .application()
            .and_downcast::<SealwaxApplication>();
        dialog.connect_response(None, move |_, response| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            match response {
                GUIDE => open_link(&state, support::GUIDE_URL),
                WRITE => {
                    if let Some(app) = &app {
                        composer::open(app, ComposeRequest::Mailto(support::message(&details)));
                    }
                }
                _ => {}
            }
        });
        dialog.present(Some(window.window()));
    });
}

/// The dialog: the address and the details, each with a Copy button; Write to Support while
/// there is an account to send from.
fn contact_dialog(details: &Details) -> adw::AlertDialog {
    let can_write = details.accounts > 0;
    let dialog = adw::AlertDialog::builder()
        .heading(HEADING)
        .body(if can_write { BODY } else { BODY_NO_ACCOUNT })
        .close_response(CLOSE)
        .build();
    dialog.set_widget_name(DIALOG);
    dialog.add_response(CLOSE, "Close");
    dialog.add_response(GUIDE, "User Guide");
    if can_write {
        dialog.add_response(WRITE, "Write to Support");
        dialog.set_response_appearance(WRITE, adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some(WRITE));
    } else {
        dialog.set_default_response(Some(CLOSE));
    }
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.append(&copyable(
        support::SUPPORT_ADDRESS,
        ADDRESS,
        &["card", "sw-support-address"],
        "Copy Address",
        COPY_ADDRESS,
        "The support address is copied",
    ));
    column.append(&copyable(
        &details.text(),
        DETAILS,
        &["card", "sw-support-details"],
        "Copy Details",
        COPY_DETAILS,
        "The details are copied",
    ));
    dialog.set_extra_child(Some(&column));
    dialog
}

/// A selectable text and a button that copies it.
fn copyable(
    text: &str,
    name: &str,
    classes: &[&str],
    copy_label: &str,
    copy_name: &str,
    copied: &'static str,
) -> gtk::Box {
    let label = gtk::Label::builder()
        .label(text)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .css_classes(classes.to_vec())
        .build();
    label.set_widget_name(name);
    let copy = gtk::Button::builder()
        .label(copy_label)
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    copy.set_widget_name(copy_name);
    let text = text.to_owned();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&text);
        button.set_label("Copied");
        accessibility::announce(button, copied);
    });
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&label);
    column.append(&copy);
    column
}

/// libadwaita's About dialog: the version, the guide as the website, support by mail (which
/// opens Contact Support, so the details go with it), GitHub's issues and the details under
/// Troubleshooting.
fn about(state: &Rc<AppState>, app_id: &str) {
    let state = state.clone();
    let app_id = app_id.to_owned();
    glib::spawn_future_local(async move {
        let details = read_details(&state).await;
        let Some(window) = state.window() else {
            return;
        };
        let dialog = adw::AboutDialog::builder()
            .application_name("Sealwax")
            .application_icon(app_id.as_str())
            .developer_name("PixDevs")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Fast, keyboard-first email client")
            .website(support::GUIDE_URL)
            .support_url(format!("mailto:{}", support::SUPPORT_ADDRESS))
            .issue_url(support::ISSUES_URL)
            .debug_info(details.text())
            .debug_info_filename("sealwax-details.txt")
            .license_type(gtk::License::MitX11)
            .copyright("© 2026 Sealwax contributors")
            .build();
        dialog.set_widget_name(ABOUT);
        let weak = Rc::downgrade(&state);
        dialog.connect_activate_link(move |dialog, uri| {
            let Some(state) = weak.upgrade() else {
                return false;
            };
            if uri.starts_with("mailto:") {
                dialog.close();
                contact(&state);
            } else {
                open_link(&state, uri);
            }
            true
        });
        dialog.present(Some(window.window()));
    });
}
