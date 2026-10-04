//! Empty states (DESIGN.md "Feedback"): no account yet, no message selected, an empty folder.
//! As in the mock: a quiet icon, a title, a line of help and, for "No accounts yet", the way
//! forward; centred, and scrolled when the window is too small for them.

use super::panes::{classed, icon};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::{Binding, Keymap};

/// "No accounts yet", with "Add an account" (insensitive until the setup assistant exists, M2)
/// and a hint with the command palette's keys.
pub struct NoAccount {
    pub page: gtk::Widget,
    pub add: gtk::Button,
    pub hint: gtk::Label,
}

pub fn no_account() -> NoAccount {
    let add = classed(
        gtk::Button::builder()
            .label("Add an account")
            .halign(gtk::Align::Center)
            .sensitive(false)
            .tooltip_text("Account setup is not available yet")
            .build(),
        &["sw-primary", "sw-add-account"],
    );
    let hint = classed(
        gtk::Label::builder()
            .label(palette_hint(&Keymap::default()))
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build(),
        &["sw-empty-hint"],
    );
    let state = empty_state(
        &["mail-unread-symbolic"],
        "No accounts yet",
        "Add your first account. An address and a password, or Microsoft sign-in, is all it \
         takes.",
        "sw-no-account",
    );
    state.append(&add);
    state.append(&hint);
    NoAccount {
        page: centred(&state),
        add,
        hint,
    }
}

/// `or press Ctrl+Shift+P / F1 for every command`, with the keys in use.
pub fn palette_hint(keymap: &Keymap) -> String {
    let keys: Vec<String> = keymap
        .shown(ActionId::CommandPalette)
        .iter()
        .map(Binding::display)
        .collect();
    if keys.is_empty() {
        "or open the command palette for every command".to_owned()
    } else {
        format!("or press {} for every command", keys.join(" / "))
    }
}

/// The reader before a message is picked.
pub fn no_message() -> gtk::Widget {
    centred(&empty_state(
        &["mail-read-symbolic"],
        "No message selected",
        "Pick a message in the list to read it here.",
        "sw-no-message",
    ))
}

/// A folder without messages, such as "Nothing archived yet"; the list changes its text for
/// filters and searches.
pub fn folder(title: &str, description: &str) -> EmptyState {
    let (state, title, description) =
        empty_state_parts(&["folder-symbolic"], title, description, "sw-empty-folder");
    EmptyState {
        widget: centred(&state),
        title,
        description,
    }
}

/// An empty state whose text can change.
#[derive(Clone)]
pub struct EmptyState {
    widget: gtk::Widget,
    title: gtk::Label,
    description: gtk::Label,
}

impl EmptyState {
    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    pub fn title(&self) -> glib::GString {
        self.title.label()
    }

    pub fn description(&self) -> Option<glib::GString> {
        let text = self.description.label();
        (!text.is_empty()).then_some(text)
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_label(title);
    }

    pub fn set_description(&self, description: Option<&str>) {
        self.description.set_label(description.unwrap_or_default());
        self.description.set_visible(description.is_some());
    }
}

fn empty_state(icons: &[&str], title: &str, description: &str, class: &str) -> gtk::Box {
    empty_state_parts(icons, title, description, class).0
}

fn empty_state_parts(
    icons: &[&str],
    title: &str,
    description: &str,
    class: &str,
) -> (gtk::Box, gtk::Label, gtk::Label) {
    let state = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
        &["sw-empty-state", class],
    );
    state.append(&classed(icon(icons), &["sw-empty-icon"]));
    let [title, description] =
        [(title, "sw-empty-title"), (description, "sw-empty-body")].map(|(text, class)| {
            classed(
                gtk::Label::builder()
                    .label(text)
                    .wrap(true)
                    .max_width_chars(48)
                    .justify(gtk::Justification::Center)
                    .build(),
                &[class],
            )
        });
    state.append(&title);
    state.append(&description);
    (state, title, description)
}

fn centred(state: &gtk::Box) -> gtk::Widget {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(state)
        .build()
        .upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_names_the_palette_keys_in_use() {
        assert_eq!(
            palette_hint(&Keymap::default()),
            "or press Ctrl+Shift+P / F1 for every command"
        );
        let unbound = Keymap::with_overrides([(ActionId::CommandPalette, Vec::new())]);
        assert_eq!(
            palette_hint(&unbound),
            "or open the command palette for every command"
        );
    }
}
