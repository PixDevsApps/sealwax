//! The keyboard shortcuts dialog (`?`, `h`, Ctrl+?): an AdwShortcutsDialog generated from the
//! registry and the keymap in use, one section per category.

use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::{ActionId, Category, Scope};
use sealwax_domain::keymap::{Binding, Keymap};

/// The dialog. `available` says whether a command exists yet; one that doesn't is listed with
/// "Not available yet", so the keymap is complete from the start.
pub fn dialog(keymap: &Keymap, available: &dyn Fn(ActionId) -> bool) -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    dialog.add_css_class("sw-shortcuts");
    let mut titles = Vec::new();
    for category in Category::ALL {
        let section = adw::ShortcutsSection::new(Some(category.label()));
        let mut empty = true;
        for id in ActionId::ALL
            .into_iter()
            .filter(|id| id.category() == category)
        {
            let label = keymap.label(id);
            if label.is_empty() {
                continue;
            }
            let item = adw::ShortcutsItem::new(id.title(), &label);
            item.set_subtitle(&subtitle(id, keymap, available(id)));
            section.add(item);
            empty = false;
        }
        if !empty {
            dialog.add(section);
            titles.push(category.label());
        }
    }
    // libadwaita leaves each section's list without a name; a screen reader entering one
    // hears the section's title instead of only "list". The lists exist once it shows, each
    // in a group titled as its section.
    dialog.connect_map(move |dialog| {
        let mut lists = Vec::new();
        collect_lists(dialog.upcast_ref(), &mut lists);
        for list in lists {
            let title = list
                .ancestor(adw::PreferencesGroup::static_type())
                .map(|group| group.property::<String>("title"))
                .filter(|title| titles.contains(&title.as_str()));
            if let Some(title) = title {
                list.update_property(&[gtk::accessible::Property::Label(&title)]);
            }
        }
    });
    dialog
}

fn collect_lists(widget: &gtk::Widget, lists: &mut Vec<gtk::Widget>) {
    if widget.is::<gtk::ListBox>() && widget.has_css_class("boxed-list") {
        lists.push(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        collect_lists(&current, lists);
        child = current.next_sibling();
    }
}

/// Builds the dialog and presents it over `parent`.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    keymap: &Keymap,
    available: &dyn Fn(ActionId) -> bool,
) -> adw::ShortcutsDialog {
    let dialog = dialog(keymap, available);
    dialog.present(Some(parent));
    dialog
}

fn place(scopes: &[Scope]) -> &'static str {
    match scopes {
        [Scope::List, Scope::Reader] => "the message list and the reader",
        [Scope::List] => "the message list",
        [Scope::Reader] => "the reader",
        _ => "the composer",
    }
}

fn keys(bindings: &[Binding]) -> String {
    bindings
        .iter()
        .map(Binding::display)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where the keys of `id` work when not anywhere in the window, and whether it exists yet:
/// `In the message list and the reader`, or `c in the message list and the reader` when the
/// command also has keys that work anywhere.
pub fn subtitle(id: ActionId, keymap: &Keymap, available: bool) -> String {
    let scoped = keymap.scoped(id);
    let list = Keymap::list_keys(id);
    let anywhere = !keymap.accels(id).is_empty();
    let mut parts = Vec::new();
    if !scoped.is_empty() {
        let place = place(id.spec().key_scopes);
        if anywhere || !list.is_empty() {
            parts.push(format!("{} in {place}", keys(&scoped)));
        } else {
            parts.push(format!("In {place}"));
        }
    }
    if !list.is_empty() {
        parts.push(format!("{} in the message list", keys(&list)));
    }
    if !available {
        parts.push("Not available yet".to_owned());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_say_where_keys_work() {
        let keymap = Keymap::default();
        assert_eq!(
            subtitle(ActionId::Archive, &keymap, true),
            "In the message list and the reader"
        );
        assert_eq!(
            subtitle(ActionId::Compose, &keymap, true),
            "c in the message list and the reader"
        );
        assert_eq!(
            subtitle(ActionId::NextMessage, &keymap, true),
            "j in the message list and the reader · Down in the message list"
        );
        assert_eq!(
            subtitle(ActionId::Select, &keymap, true),
            "In the message list"
        );
        assert_eq!(
            subtitle(ActionId::BackToList, &keymap, true),
            "In the reader"
        );
        assert_eq!(
            subtitle(ActionId::Send, &keymap, false),
            "In the composer · Not available yet"
        );
        assert_eq!(subtitle(ActionId::Quit, &keymap, true), "");
        assert_eq!(
            subtitle(ActionId::Refresh, &keymap, false),
            "Not available yet"
        );
    }
}
