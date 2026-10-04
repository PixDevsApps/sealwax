//! Tooltips that name a command's keys (DESIGN.md "Accessibility": icon-only buttons have
//! tooltips with their shortcut). The keys come from the keymap in use, so a `keys.toml`
//! change shows up in every tooltip registered here.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use std::cell::RefCell;

struct Entry {
    widget: glib::WeakRef<gtk::Widget>,
    label: String,
    id: ActionId,
}

/// The widgets whose tooltips name a command's keys, and the keymap they show.
#[derive(Default)]
pub struct Tooltips {
    keymap: RefCell<Keymap>,
    entries: RefCell<Vec<Entry>>,
}

impl Tooltips {
    /// Gives `widget` the tooltip `label (keys of id)` now and after every keymap change.
    pub fn add(&self, widget: &impl IsA<gtk::Widget>, label: &str, id: ActionId) {
        let widget = widget.as_ref();
        widget.set_tooltip_text(Some(&with_keys(label, &self.keymap.borrow().display(id))));
        let mut entries = self.entries.borrow_mut();
        entries.retain(|entry| entry.widget.upgrade().is_some());
        entries.push(Entry {
            widget: widget.downgrade(),
            label: label.to_owned(),
            id,
        });
    }

    pub fn set_keymap(&self, keymap: &Keymap) {
        self.keymap.replace(keymap.clone());
        self.entries
            .borrow_mut()
            .retain(|entry| match entry.widget.upgrade() {
                Some(widget) => {
                    widget.set_tooltip_text(Some(&with_keys(
                        &entry.label,
                        &keymap.display(entry.id),
                    )));
                    true
                }
                None => false,
            });
    }
}

/// `New message (c, Ctrl+N)`, or just the label when the command has no keys.
pub fn with_keys(label: &str, keys: &str) -> String {
    if keys.is_empty() {
        label.to_owned()
    } else {
        format!("{label} ({keys})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::keymap::Binding;

    #[test]
    fn tooltips_name_the_keys_in_use() {
        let keymap = Keymap::default();
        assert_eq!(
            with_keys("New message", &keymap.display(ActionId::Compose)),
            "New message (c, Ctrl+N)"
        );
        assert_eq!(
            with_keys("Open config.toml", &keymap.display(ActionId::Preferences)),
            "Open config.toml (Ctrl+,)"
        );
        let changed = Keymap::with_overrides([(
            ActionId::Compose,
            vec![Binding::parse("<Control>m").unwrap()],
        )]);
        assert_eq!(
            with_keys("New message", &changed.display(ActionId::Compose)),
            "New message (Ctrl+M)"
        );
        let unbound = Keymap::with_overrides([(ActionId::Compose, Vec::new())]);
        assert_eq!(
            with_keys("New message", &unbound.display(ActionId::Compose)),
            "New message"
        );
    }
}
