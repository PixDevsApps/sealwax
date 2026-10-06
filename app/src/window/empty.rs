//! Empty states (DESIGN.md "Feedback"): no account yet, no message selected, an empty folder.
//! As in the mock: a quiet icon, a title, a line of help and, for "No accounts yet", the way
//! forward; centred, and scrolled when the window is too small for them.

use super::panes::{classed, icon};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::{Binding, Keymap};

/// "No accounts yet", with "Add an account" (the setup assistant, `win.add-account`) and a
/// hint with the command palette's keys.
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
            .action_name(super::ADD_ACCOUNT_ACTION)
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

/// The batch actions the reader's page offers while several messages are selected (ML-054),
/// in the toolbar's order.
pub const BATCH_ACTIONS: [ActionId; 7] = [
    ActionId::Archive,
    ActionId::Delete,
    ActionId::Junk,
    ActionId::MoveTo,
    ActionId::Star,
    ActionId::MarkRead,
    ActionId::MarkUnread,
];

/// The reader in the select mode: "3 messages selected", what applies to them, and the batch
/// actions as buttons bound to the registry's commands.
pub struct BatchPage {
    pub widget: gtk::Widget,
    title: gtk::Label,
    buttons: Vec<(ActionId, gtk::Button)>,
}

impl BatchPage {
    pub fn new() -> Self {
        let (state, title, _) = empty_state_parts(
            &["object-select-symbolic", "checkbox-checked-symbolic"],
            "No messages selected",
            "Every action here applies to all of them. Escape leaves the select mode.",
            "sw-batch-page",
        );
        let row = classed(
            gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .halign(gtk::Align::Center)
                .max_children_per_line(4)
                .homogeneous(false)
                .build(),
            &["sw-batch-actions"],
        );
        let mut buttons = Vec::new();
        for id in BATCH_ACTIONS {
            let button = classed(
                gtk::Button::builder()
                    .label(id.title())
                    .action_name(id.detailed_name())
                    .build(),
                &["sw-batch-action"],
            );
            button.set_widget_name(&format!("batch-{}", id.name()));
            row.insert(&button, -1);
            buttons.push((id, button));
        }
        state.append(&row);
        title.set_widget_name("batch-title");
        Self {
            widget: centred(&state),
            title,
            buttons,
        }
    }

    /// "1 message selected", "3 messages selected".
    pub fn set_count(&self, count: u32) {
        self.title.set_label(&match count {
            1 => "1 message selected".to_owned(),
            count => format!("{count} messages selected"),
        });
    }

    /// A command's title here (Not Junk, Unstar) and its tooltip.
    pub fn set_command(&self, id: ActionId, title: &str, tooltip: &str) {
        if let Some((_, button)) = self.buttons.iter().find(|(candidate, _)| *candidate == id) {
            button.set_label(title);
            button.set_tooltip_text(Some(tooltip));
        }
    }
}

impl Default for BatchPage {
    fn default() -> Self {
        Self::new()
    }
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
