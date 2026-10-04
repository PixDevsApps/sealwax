//! The reader's toolbar (DESIGN.md "Reader"): archive, delete, junk, move, mark unread and star,
//! bound to the registry's window actions (they stay insensitive until their handlers exist,
//! M4), then the position "n of N" with previous and next.

use crate::window::panes::icon_button;
use gtk4 as gtk;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The message commands on the toolbar, in the mock's order; a separator follows Move to.
const ACTIONS: [(ActionId, &[&str]); 6] = [
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
}

impl Toolbar {
    pub fn new(keymap: &Keymap) -> Rc<Self> {
        let root = gtk::Box::builder()
            .spacing(2)
            .hexpand(true)
            .css_classes(["sealwax-reader-toolbar"])
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
        let with_keys = |id: ActionId, title: &str| {
            let keys = keymap.display(id);
            if keys.is_empty() {
                title.to_owned()
            } else {
                format!("{title} ({keys})")
            }
        };
        for (id, button) in &self.buttons {
            button.set_tooltip_text(Some(&with_keys(*id, id.title())));
        }
        self.previous.set_tooltip_text(Some(&with_keys(
            ActionId::PreviousMessage,
            "Previous message",
        )));
        self.next
            .set_tooltip_text(Some(&with_keys(ActionId::NextMessage, "Next message")));
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
