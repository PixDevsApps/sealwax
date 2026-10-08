//! Edit Account (the user's UI tweak, 2026-10-08): what Sealwax calls an account and its
//! colour, for an account already set up. A dialog asks which account (when there are
//! several), its "Account name" (the address when left empty) and its "Colour", then sends
//! `Command::EditAccount`; the engine writes the account's `label` and `color` lines in
//! `accounts.toml`, and the sidebar follows as the app reads the file. A toast reports the
//! result. The sender name is the setup's and stays.

use super::colors::ColorChoice;
use super::view::{entry, field};
use crate::engine_link::{EngineLink, Subscription};
use crate::window::Window;
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::{AccountColor, AccountKey};
use sealwax_engine::api::{Command, Event};
use std::cell::RefCell;
use std::rc::Rc;

/// An account whose name and colour can be changed: one `accounts.toml` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editable {
    pub key: AccountKey,
    pub address: String,
    pub label: Option<String>,
    pub color: Option<AccountColor>,
}

impl Editable {
    /// What Sealwax calls it now.
    fn title(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.address)
    }

    /// The account's line in the choice: its name and its address.
    fn choice(&self) -> String {
        match &self.label {
            Some(label) => format!("{label} — {}", self.address),
            None => self.address.clone(),
        }
    }

    /// The label to write for `typed`: none for an empty one or the address typed again.
    fn label_for(&self, typed: &str) -> Option<String> {
        let typed = typed.trim();
        (!typed.is_empty() && !typed.eq_ignore_ascii_case(&self.address)).then(|| typed.to_owned())
    }
}

const BODY: &str = "Sealwax shows the account by this name and colour in the sidebar. The name \
                    people see on your mail stays as it is.";

/// The dialog's fields, filled from the account chosen.
struct Fields {
    label: gtk::Entry,
    colors: Rc<ColorChoice>,
}

impl Fields {
    fn show(&self, account: &Editable) {
        self.label
            .set_text(account.label.as_deref().unwrap_or_default());
        self.label.set_placeholder_text(Some(&account.address));
        self.colors.choose(account.color, false);
    }
}

/// Asks for the name and colour of one of `accounts`, `chosen` first, then sends the command.
pub fn open(window: &Window, engine: &EngineLink, accounts: Vec<Editable>, chosen: usize) {
    let chosen = chosen.min(accounts.len().saturating_sub(1));
    let Some(first) = accounts.get(chosen) else {
        return;
    };
    let dialog = adw::AlertDialog::builder()
        .heading(if accounts.len() == 1 {
            format!("Edit {}", first.title())
        } else {
            "Edit an account".to_owned()
        })
        .body(BODY)
        .close_response("cancel")
        .default_response("save")
        .build();
    dialog.set_widget_name("edit-account");
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("save", "Save");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("sw-edit-account");
    let choice = (accounts.len() > 1).then(|| {
        let lines: Vec<String> = accounts.iter().map(Editable::choice).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let choice = gtk::DropDown::from_strings(&lines);
        choice.set_widget_name("edit-account-choice");
        choice.set_selected(chosen as u32);
        content.append(&field("Account", &choice, "edit-account-choice-field"));
        choice
    });
    let label = entry("edit-account-label", "", &[]);
    label.set_activates_default(true);
    content.append(&field("Account name", &label, "edit-account-label-field"));
    let colors = ColorChoice::new("edit-account");
    content.append(&field("Colour", &colors.root, "edit-account-color-field"));
    dialog.set_extra_child(Some(&content));
    dialog.set_property("focus-widget", &label);

    let fields = Rc::new(Fields { label, colors });
    fields.show(first);
    let accounts = Rc::new(accounts);
    if let Some(choice) = &choice {
        let (fields, accounts) = (fields.clone(), accounts.clone());
        choice.connect_selected_notify(move |choice| {
            if let Some(account) = accounts.get(choice.selected() as usize) {
                fields.show(account);
            }
        });
    }
    // Enter on a colour saves, as Enter in the name does.
    let shown = dialog.downgrade();
    fields.colors.connect_enter(move || {
        if let Some(dialog) = shown.upgrade() {
            dialog.emit_by_name::<()>("response", &[&"save"]);
            dialog.force_close();
        }
    });
    let engine = engine.clone();
    let toasts = window.toasts();
    dialog.connect_response(None, move |_, response| {
        if response != "save" {
            return;
        }
        let index = choice
            .as_ref()
            .map_or(chosen, |choice| choice.selected() as usize);
        let Some(account) = accounts.get(index) else {
            return;
        };
        let label = account.label_for(&fields.label.text());
        let color = fields.colors.chosen();
        if label == account.label && color == account.color {
            return;
        }
        send(&engine, &toasts, account, label, color);
    });
    dialog.present(Some(window.window()));
}

fn send(
    engine: &EngineLink,
    toasts: &adw::ToastOverlay,
    account: &Editable,
    label: Option<String>,
    color: Option<AccountColor>,
) {
    let title = label.clone().unwrap_or_else(|| account.address.clone());
    let request = engine.next_request();
    let command = Command::EditAccount {
        request,
        key: account.key.clone(),
        label,
        color,
    };
    if engine.send(command).is_err() {
        toasts.add_toast(adw::Toast::new("Not connected to the mail engine"));
        return;
    }
    // The subscription lives until the engine answers this request.
    let held: Rc<RefCell<Option<Subscription>>> = Rc::default();
    let toasts = toasts.clone();
    let release = held.clone();
    let subscription = engine.subscribe(move |event| {
        let Event::AccountEdited {
            request: answered,
            result,
        } = event
        else {
            return;
        };
        if *answered != request {
            return;
        }
        let text = match result {
            Ok(()) => format!("Saved {title}"),
            Err(error) => format!("Couldn't save {title}: {error}"),
        };
        toasts.add_toast(adw::Toast::new(&text));
        release.take();
    });
    held.replace(Some(subscription));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(label: Option<&str>) -> Editable {
        Editable {
            key: AccountKey("work".to_owned()),
            address: "kari@example.test".to_owned(),
            label: label.map(str::to_owned),
            color: None,
        }
    }

    #[test]
    fn an_empty_name_or_the_address_shows_the_address() {
        let plain = account(None);
        assert_eq!(plain.label_for("  "), None);
        assert_eq!(plain.label_for(" Kari@Example.test "), None);
        assert_eq!(plain.label_for(" Work "), Some("Work".to_owned()));
        assert_eq!(plain.title(), "kari@example.test");
        assert_eq!(plain.choice(), "kari@example.test");
        let named = account(Some("Work"));
        assert_eq!(named.title(), "Work");
        assert_eq!(named.choice(), "Work — kari@example.test");
    }
}
