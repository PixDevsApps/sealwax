//! The message row's right-click menu (M9, ADR-020): a right click on a row, or Menu or
//! Shift+F10 on the row under the list's cursor, opens it under the row. It acts on that row,
//! selected first when it wasn't, or on every selected message when the row is among them.
//!
//! Reply, Reply All, Forward; Archive, Delete, Move to…, Move to Junk (Not Junk in a Junk
//! folder); Mark as Read (Unread when all are read), Star (Unstar when all are starred); Create
//! Rule from Message…, Always Junk Messages from {address}, Never Junk Messages from {address}.
//! Each item runs its registry command, so it does what the command's key does, and is
//! insensitive where the command is, with the reason as its tooltip; its key shows at its end.
//! Items are named `message-menu-<command>` (`message-menu-junk-sender`) for the self-test.

use crate::application::AppState;
use crate::list::MessageList;
use crate::triage::{self, Target};
use crate::window::panes::{classed, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::actions::ActionId;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// The menu open now, for the self-test.
    static OPEN: RefCell<Option<gtk::Popover>> = const { RefCell::new(None) };
}

/// The menu open now, if any.
pub fn current() -> Option<gtk::Popover> {
    OPEN.with(|open| open.borrow().clone())
        .filter(|menu| menu.parent().is_some() && menu.is_visible())
}

/// The open menu's items, in order.
pub fn items() -> Vec<gtk::Button> {
    let mut buttons = Vec::new();
    let mut child = current()
        .and_then(|menu| menu.child())
        .and_then(|content| content.first_child());
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(button) = widget.downcast::<gtk::Button>() {
            buttons.push(button);
        }
    }
    buttons
}

/// Opens the menu wherever `list` asks for it.
pub fn install(state: &Rc<AppState>, list: &MessageList) {
    let weak = Rc::downgrade(state);
    let view = list.list_view().clone();
    list.connect_context_menu(move |at| {
        if let Some(state) = weak.upgrade() {
            show(&state, &view, at);
        }
    });
}

/// One item: its command, its title, and why it doesn't apply.
struct Entry {
    id: ActionId,
    title: String,
    why_not: Option<&'static str>,
}

/// The menu's sections for the messages selected now.
fn sections(state: &AppState, targets: &[Target]) -> Vec<Vec<Entry>> {
    let folders = state.mailbox().folders();
    let plain = |id: ActionId| Entry {
        id,
        title: id.title().to_owned(),
        why_not: None,
    };
    let triaged = |id: ActionId| {
        let availability = triage::availability(id, targets, &folders);
        Entry {
            id,
            title: availability.title.to_owned(),
            why_not: availability.reason,
        }
    };
    let read = if triage::availability(ActionId::MarkRead, targets, &folders).enabled() {
        triaged(ActionId::MarkRead)
    } else {
        triaged(ActionId::MarkUnread)
    };
    let sender = crate::rules::sender(state);
    let rules = |id: ActionId, title: String| Entry {
        id,
        title,
        why_not: crate::rules::applies(state, id).err(),
    };
    vec![
        vec![
            plain(ActionId::Reply),
            plain(ActionId::ReplyAll),
            plain(ActionId::Forward),
        ],
        vec![
            triaged(ActionId::Archive),
            triaged(ActionId::Delete),
            triaged(ActionId::MoveTo),
            triaged(ActionId::Junk),
        ],
        vec![read, triaged(ActionId::Star)],
        vec![
            rules(
                ActionId::NewRuleFromMessage,
                ActionId::NewRuleFromMessage.title().to_owned(),
            ),
            rules(
                ActionId::JunkSender,
                crate::rules::text::sender_title(true, sender.as_deref()),
            ),
            rules(
                ActionId::TrustSender,
                crate::rules::text::sender_title(false, sender.as_deref()),
            ),
        ],
    ]
}

fn show(state: &Rc<AppState>, view: &gtk::ListView, at: gdk::Rectangle) {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    close();
    // The commands' sensitivity and the toolbar follow the selection the row's click made.
    state.refresh_commands();
    let targets = list.selected_targets().unwrap_or_default();
    let keymap = state
        .preferences()
        .map(|prefs| prefs.keymap())
        .unwrap_or_default();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let popover = named(
        classed(
            gtk::Popover::builder()
                .child(&content)
                .has_arrow(false)
                .position(gtk::PositionType::Bottom)
                .halign(gtk::Align::Start)
                .build(),
            &["sw-folder-menu", "sw-message-menu"],
        ),
        "message-menu",
    );
    popover.update_property(&[gtk::accessible::Property::Label("Message menu")]);
    for (index, section) in sections(state, &targets).into_iter().enumerate() {
        if index > 0 {
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        for entry in section {
            let enabled = gtk::prelude::ActionGroupExt::is_action_enabled(
                window.window(),
                entry.id.name(),
            );
            let title = gtk::Label::builder()
                .label(&entry.title)
                .xalign(0.0)
                .hexpand(true)
                .build();
            let line = gtk::Box::builder().spacing(18).build();
            line.append(&title);
            let key = keymap
                .shown(entry.id)
                .first()
                .map(|binding| binding.display());
            if let Some(key) = &key {
                line.append(&classed(
                    gtk::Label::new(Some(key)),
                    &["dim-label", "sw-menu-key"],
                ));
            }
            let button = named(
                classed(
                    gtk::Button::builder()
                        .child(&line)
                        .sensitive(enabled)
                        .build(),
                    &["flat", "sw-menu-item"],
                ),
                &format!("message-menu-{}", entry.id.name()),
            );
            crate::accessibility::name_button(&button, &entry.title);
            if let Some(key) = &key {
                button.update_property(&[gtk::accessible::Property::KeyShortcuts(key)]);
            }
            if !enabled {
                button.set_tooltip_text(entry.why_not);
            }
            let (id, window) = (entry.id, window.clone());
            button.connect_clicked(glib::clone!(
                #[weak]
                popover,
                move |_| {
                    popover.popdown();
                    let window = window.clone();
                    // After the closed menu has put the focus back, so a dialog keeps it.
                    glib::idle_add_local_once(move || {
                        if gtk::prelude::WidgetExt::activate_action(
                            window.window(),
                            &id.detailed_name(),
                            None,
                        )
                        .is_err()
                        {
                            tracing::warn!(action = id.name(), "the message menu's command is gone");
                        }
                    });
                }
            ));
            content.append(&button);
        }
    }
    popover.set_parent(view);
    popover.set_pointing_to(Some(&at));
    let before = view
        .root()
        .and_then(|root| root.focus())
        .map(|widget| widget.downgrade());
    popover.connect_closed(move |popover| {
        popover.unparent();
        let before = before.clone();
        glib::idle_add_local_once(move || {
            if let Some(widget) = before.and_then(|widget| widget.upgrade())
                && widget.root().is_some()
                && !widget.has_focus()
            {
                widget.grab_focus();
            }
        });
    });
    popover.popup();
    OPEN.with(|open| open.replace(Some(popover)));
}

/// Closes the menu, if one is open.
pub fn close() {
    if let Some(menu) = OPEN.with(|open| open.take())
        && menu.parent().is_some()
    {
        menu.popdown();
        if menu.parent().is_some() {
            menu.unparent();
        }
    }
}
