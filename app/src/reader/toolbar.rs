//! The reader's toolbar (DESIGN.md "Reader"): archive, delete, junk, move, mark unread and star,
//! bound to the registry's window actions, which are sensitive where they apply to the selected
//! messages (M4), then the position "n of N" with previous and next. Each tooltip names the
//! command, its keys, and why it doesn't apply when it doesn't; in a Junk folder Move to Junk
//! is Not Junk. M5 adds Print and a More menu (the approved mock's): Show Original, Save as
//! .eml, Expand All and Collapse All Messages, Show Images and Always Show Images from This
//! Sender; each insensitive until its command is built.

use crate::window::panes::{icon, icon_button};
use gtk4 as gtk;
use gtk4::gio;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Not Junk's icon, with the junk icon as the fallback.
const NOT_JUNK_ICONS: &[&str] = &["mail-mark-notjunk-symbolic", "mail-mark-junk-symbolic"];

/// The message commands on the toolbar, in the mock's order; a separator follows Move to.
const ACTIONS: [(ActionId, &[&str]); 7] = [
    (
        ActionId::Archive,
        &["mail-archive-symbolic", "folder-documents-symbolic"],
    ),
    (ActionId::Delete, &["user-trash-symbolic"]),
    (
        ActionId::Junk,
        &["mail-mark-junk-symbolic", "dialog-warning-symbolic"],
    ),
    (ActionId::MoveTo, &["mail-move-symbolic", "folder-symbolic"]),
    (ActionId::MarkUnread, &["mail-unread-symbolic"]),
    (ActionId::Star, &["starred-symbolic"]),
    (
        ActionId::Print,
        &["printer-symbolic", "document-print-symbolic"],
    ),
];

/// The More menu's sections (the approved mock's "Show original, Save as .eml", then M5's
/// conversation and remote-content commands).
const MORE: [&[ActionId]; 3] = [
    &[ActionId::ShowOriginal, ActionId::SaveMessage],
    &[ActionId::ExpandAll, ActionId::CollapseAll],
    &[ActionId::ShowImages, ActionId::AlwaysShowImages],
];

/// Where the shown message is in its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 0 for the first message.
    pub index: u32,
    pub total: u32,
}

type Callbacks = Rc<RefCell<Vec<Box<dyn Fn()>>>>;

pub struct Toolbar {
    root: gtk::Box,
    buttons: Vec<(ActionId, gtk::Button)>,
    position: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    at: Cell<Option<Position>>,
    on_previous: Callbacks,
    on_next: Callbacks,
    keymap: RefCell<Keymap>,
    /// Each command's title here and why it doesn't apply, when it doesn't.
    commands: RefCell<HashMap<ActionId, (String, Option<String>)>>,
}

impl Toolbar {
    pub fn new(keymap: &Keymap) -> Rc<Self> {
        let root = gtk::Box::builder()
            .spacing(2)
            .hexpand(true)
            .css_classes(["sw-reader-toolbar"])
            .build();
        root.set_widget_name("reader-actions");
        let mut buttons = Vec::new();
        for (index, (id, icons)) in ACTIONS.into_iter().enumerate() {
            let button = icon_button(icons, id.title());
            button.add_css_class("sw-icon-button");
            button.set_action_name(Some(&id.detailed_name()));
            button.set_widget_name(&format!("reader-{}", id.name()));
            root.append(&button);
            buttons.push((id, button));
            if index == 3 {
                root.append(
                    &gtk::Separator::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .margin_start(6)
                        .margin_end(6)
                        .margin_top(10)
                        .margin_bottom(10)
                        .build(),
                );
            }
        }
        root.append(&more_button());
        let position = gtk::Label::builder()
            .hexpand(true)
            .xalign(1.0)
            .margin_end(6)
            .css_classes(["sw-secondary", "numeric"])
            .visible(false)
            .build();
        position.set_widget_name("reader-position");
        root.append(&position);
        let previous = icon_button(&["go-up-symbolic"], "Previous message");
        let next = icon_button(&["go-down-symbolic"], "Next message");
        previous.set_widget_name("reader-previous");
        next.set_widget_name("reader-next");
        for button in [&previous, &next] {
            button.add_css_class("sw-icon-button");
            button.set_sensitive(false);
            root.append(button);
        }
        let toolbar = Rc::new(Self {
            root,
            buttons,
            position,
            previous,
            next,
            at: Cell::new(None),
            on_previous: Callbacks::default(),
            on_next: Callbacks::default(),
            keymap: RefCell::new(keymap.clone()),
            commands: RefCell::new(HashMap::new()),
        });
        for (button, callbacks) in [
            (&toolbar.previous, toolbar.on_previous.clone()),
            (&toolbar.next, toolbar.on_next.clone()),
        ] {
            button.connect_clicked(move |_| {
                for callback in callbacks.borrow().iter() {
                    callback();
                }
            });
        }
        toolbar.update_keys(keymap);
        toolbar
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Tooltips name each command's keys (DESIGN.md "Accessibility"); call again when
    /// `keys.toml` changes.
    pub fn update_keys(&self, keymap: &Keymap) {
        self.keymap.replace(keymap.clone());
        for (id, _) in &self.buttons {
            self.refresh_command(*id);
        }
        self.previous.set_tooltip_text(Some(&tooltip(
            keymap,
            ActionId::PreviousMessage,
            "Previous message",
            None,
        )));
        self.next.set_tooltip_text(Some(&tooltip(
            keymap,
            ActionId::NextMessage,
            "Next message",
            None,
        )));
    }

    /// A command's title for the selected messages (Not Junk, Unstar) and why it doesn't apply.
    pub fn set_command(&self, id: ActionId, title: &str, reason: Option<&str>) {
        let new = (title.to_owned(), reason.map(str::to_owned));
        let old = self.commands.borrow_mut().insert(id, new.clone());
        if old.as_ref() != Some(&new) {
            if id == ActionId::Junk && old.as_ref().map(|(title, _)| title) != Some(&new.0) {
                self.set_junk_icon(new.0 == "Not Junk");
            }
            self.refresh_command(id);
        }
    }

    fn set_junk_icon(&self, not_junk: bool) {
        if let Some((_, button)) = self.buttons.iter().find(|(id, _)| *id == ActionId::Junk) {
            let icons = if not_junk {
                NOT_JUNK_ICONS
            } else {
                ACTIONS[2].1
            };
            button.set_child(Some(&icon(icons)));
        }
    }

    fn refresh_command(&self, id: ActionId) {
        let Some((_, button)) = self.buttons.iter().find(|(candidate, _)| *candidate == id) else {
            return;
        };
        let commands = self.commands.borrow();
        let (title, reason) = commands
            .get(&id)
            .map_or((id.title(), None), |(title, reason)| {
                (title.as_str(), reason.as_deref())
            });
        let text = tooltip(&self.keymap.borrow(), id, title, reason);
        button.set_tooltip_text(Some(&text));
        button.update_property(&[gtk::accessible::Property::Label(title)]);
    }

    /// Shows "n of N", or nothing; previous and next follow it.
    pub fn set_position(&self, position: Option<Position>) {
        self.at.set(position);
        match position {
            Some(Position { index, total }) => {
                self.position.set_text(&format!("{} of {total}", index + 1));
                self.position.set_visible(true);
            }
            None => self.position.set_visible(false),
        }
        self.refresh();
    }

    pub fn connect_previous(&self, callback: impl Fn() + 'static) {
        self.on_previous.borrow_mut().push(Box::new(callback));
        self.refresh();
    }

    pub fn connect_next(&self, callback: impl Fn() + 'static) {
        self.on_next.borrow_mut().push(Box::new(callback));
        self.refresh();
    }

    fn refresh(&self) {
        let (has_previous, has_next) = match self.at.get() {
            Some(Position { index, total }) => (index > 0, index + 1 < total),
            None => (true, true),
        };
        self.previous
            .set_sensitive(has_previous && !self.on_previous.borrow().is_empty());
        self.next
            .set_sensitive(has_next && !self.on_next.borrow().is_empty());
    }
}

/// The More menu, bound to the registry's window actions.
fn more_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    for section in MORE {
        let items = gio::Menu::new();
        for id in section {
            items.append(Some(id.title()), Some(&id.detailed_name()));
        }
        menu.append_section(None, &items);
    }
    let button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("More")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "sw-icon-button"])
        .build();
    button.set_widget_name("reader-more");
    button.update_property(&[gtk::accessible::Property::Label("More")]);
    button
}

/// "Archive (e, y)", and why it doesn't apply: "Archive (e, y) — already archived".
pub fn tooltip(keymap: &Keymap, id: ActionId, title: &str, reason: Option<&str>) -> String {
    let keys = keymap.display(id);
    let named = if keys.is_empty() {
        title.to_owned()
    } else {
        format!("{title} ({keys})")
    };
    match reason {
        Some(reason) => format!("{named} — {reason}"),
        None => named,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltips_name_the_keys_and_the_reason() {
        let keymap = Keymap::default();
        assert_eq!(
            tooltip(&keymap, ActionId::Archive, "Archive", None),
            "Archive (e, y)"
        );
        assert_eq!(
            tooltip(&keymap, ActionId::Junk, "Not Junk", None),
            "Not Junk (!, Ctrl+J)"
        );
        assert_eq!(
            tooltip(
                &keymap,
                ActionId::Archive,
                "Archive",
                Some("already archived")
            ),
            "Archive (e, y) — already archived"
        );
        assert_eq!(
            tooltip(
                &keymap,
                ActionId::MarkFolderRead,
                "Mark Folder as Read",
                None
            ),
            "Mark Folder as Read"
        );
    }
}
