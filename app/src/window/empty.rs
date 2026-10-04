//! Empty states (DESIGN.md "Feedback"): no account yet, no message selected, an empty folder.
//! They use libadwaita's compact status page; the classes name them for the stylesheet.

use super::panes::classed;
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;

/// "No accounts yet", with an "Add an account" button that stays insensitive until the setup
/// assistant exists (M2).
pub fn no_account() -> (adw::StatusPage, gtk::Button) {
    let add = classed(
        gtk::Button::builder()
            .label("Add an account")
            .halign(gtk::Align::Center)
            .sensitive(false)
            .tooltip_text("Account setup is not available yet")
            .build(),
        &["pill", "suggested-action", "sealwax-add-account"],
    );
    let hint = gtk::Label::builder()
        .label("or press F1 / Ctrl+Shift+P for every command")
        .css_classes(["caption", "dim-label"])
        .build();
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    actions.append(&add);
    actions.append(&hint);
    let page = status_page(
        "mail-unread-symbolic",
        "No accounts yet",
        "Add your first account. An address and a password, or Google or Microsoft sign-in, \
         is all it takes.",
        "sealwax-no-account",
    );
    page.set_child(Some(&actions));
    (page, add)
}

/// The reader before a message is picked.
pub fn no_message() -> adw::StatusPage {
    status_page(
        "mail-read-symbolic",
        "No message selected",
        "Pick a message in the list to read it here.",
        "sealwax-no-message",
    )
}

/// A folder without messages, such as "Nothing archived yet".
pub fn folder(title: &str, description: &str) -> adw::StatusPage {
    status_page(
        "folder-symbolic",
        title,
        description,
        "sealwax-empty-folder",
    )
}

fn status_page(icon: &str, title: &str, description: &str, class: &str) -> adw::StatusPage {
    classed(
        adw::StatusPage::builder()
            .icon_name(icon)
            .title(title)
            .description(description)
            .vexpand(true)
            .build(),
        &["compact", "sealwax-empty-state", class],
    )
}
