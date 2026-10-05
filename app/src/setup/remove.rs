//! Remove Account (ML-038): a confirmation that names what goes, then `Command::RemoveAccount`.
//! The engine deletes the account's `accounts.toml` entry, its local mail and its keyring
//! secret; the mail on the server stays. A toast reports the result.

use crate::engine_link::{EngineLink, Subscription};
use crate::window::Window;
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::AccountKey;
use sealwax_engine::api::{Command, Event};
use std::cell::RefCell;
use std::rc::Rc;

/// An account that can be removed: one `accounts.toml` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removable {
    pub key: AccountKey,
    pub name: String,
    pub address: String,
}

impl Removable {
    fn label(&self) -> String {
        if self.name == self.address {
            self.address.clone()
        } else {
            format!("{} — {}", self.name, self.address)
        }
    }
}

const BODY: &str = "Sealwax deletes the account's mail stored on this computer, its password in \
                    the keyring and its entry in accounts.toml. The mail on the server stays.";

/// Asks which account to remove, and whether, then sends the command.
pub fn confirm(window: &Window, engine: &EngineLink, accounts: Vec<Removable>) {
    let Some(first) = accounts.first() else {
        return;
    };
    let dialog = adw::AlertDialog::builder()
        .heading(if accounts.len() == 1 {
            format!("Remove {}?", first.label())
        } else {
            "Remove an account?".to_owned()
        })
        .body(BODY)
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.set_widget_name("remove-account");
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let choice = (accounts.len() > 1).then(|| {
        let labels: Vec<String> = accounts.iter().map(Removable::label).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let choice = gtk::DropDown::from_strings(&labels);
        choice.set_widget_name("remove-account-choice");
        choice.update_property(&[gtk::accessible::Property::Label("Account to remove")]);
        dialog.set_extra_child(Some(&choice));
        choice
    });
    let engine = engine.clone();
    let toasts = window.toasts();
    dialog.connect_response(None, move |_, response| {
        if response != "remove" {
            return;
        }
        let index = choice
            .as_ref()
            .map_or(0, |choice| choice.selected() as usize);
        if let Some(account) = accounts.get(index) {
            send(&engine, &toasts, account.clone());
        }
    });
    dialog.present(Some(window.window()));
}

fn send(engine: &EngineLink, toasts: &adw::ToastOverlay, account: Removable) {
    let request = engine.next_request();
    let command = Command::RemoveAccount {
        request,
        key: account.key.clone(),
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
        let Event::AccountRemoved {
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
            Ok(()) => format!("Removed {}", account.address),
            Err(error) => format!("Couldn't remove {}: {error}", account.address),
        };
        toasts.add_toast(adw::Toast::new(&text));
        release.take();
    });
    held.replace(Some(subscription));
}
