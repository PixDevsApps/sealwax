//! Pinned senders (issue #26, ADR-024): Pin Sender, Pin Domain and Unpin Sender act on the
//! selected messages, or on the group selected in Group by. A new pin keeps its sender's mail at
//! the top of the Inboxes until it is read; the toast's Keep Pinned, or Pinned Senders… in the
//! main menu (every pin, each with how long its mail stays first), make it stay while the mail
//! is in the Inbox. The pins are kept here too, for the commands and that dialog, and read again
//! after every change. Addresses never reach a log.

use crate::actions::ActionSet;
use crate::application::AppState;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use libadwaita as adw;
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::MessageId;
use sealwax_domain::senders::{GroupBy, domain_key, sender_key};
use sealwax_infrastructure::store::{Pin, PinKind, PinMode, PinTarget};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const COMMANDS: [ActionId; 3] = [
    ActionId::PinSender,
    ActionId::PinDomain,
    ActionId::UnpinSender,
];

/// How long a pin's toast shows.
const TOAST_SECONDS: u32 = 8;

thread_local! {
    /// Every pin, as last read from the store.
    static PINS: RefCell<Vec<Pin>> = const { RefCell::new(Vec::new()) };
    static LOADED: Cell<bool> = const { Cell::new(false) };
    /// Run when the pins were read again (the Pinned Senders dialog); one that gives `false` is
    /// dropped.
    static LISTENERS: RefCell<Vec<Rc<dyn Fn() -> bool>>> = const { RefCell::new(Vec::new()) };
}

/// Every pin, as last read.
pub fn pins() -> Vec<Pin> {
    PINS.with(|pins| pins.borrow().clone())
}

/// Whether the pins have been read since Sealwax started.
pub fn loaded() -> bool {
    LOADED.with(Cell::get)
}

/// Runs `listener` each time the pins were read again, until it gives `false`.
pub fn connect_changed(listener: impl Fn() -> bool + 'static) {
    LISTENERS.with(|listeners| listeners.borrow_mut().push(Rc::new(listener)));
}

pub fn install_window(state: &Rc<AppState>, actions: &ActionSet) {
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::PinnedSenders, move || {
        if let Some(state) = weak.upgrade() {
            present(&state);
        }
    });
    for id in COMMANDS {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade() {
                run(&state, id);
            }
        });
    }
    reload(state);
}

/// Reads the pins again, then tells the listeners and refreshes the commands.
pub fn reload(state: &Rc<AppState>) {
    let Some(reply) = state.mailbox().store().map(|store| store.pins()) else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let Ok(read) = reply.recv().await else {
            return;
        };
        PINS.with(|pins| pins.replace(read));
        LOADED.with(|loaded| loaded.set(true));
        let listeners = LISTENERS.with(|listeners| listeners.borrow().clone());
        let gone: Vec<Rc<dyn Fn() -> bool>> = listeners
            .into_iter()
            .filter(|listener| !listener())
            .collect();
        LISTENERS.with(|listeners| {
            listeners
                .borrow_mut()
                .retain(|listener| !gone.iter().any(|gone| Rc::ptr_eq(gone, listener)));
        });
        if let Some(state) = weak.upgrade() {
            state.refresh_commands();
        }
    });
}

/// What the commands act on now.
enum Picked {
    /// A group of Group by: its kind and key.
    Group(PinKind, String),
    /// The selected messages: each id with its sender's address.
    Messages(Vec<(MessageId, String)>),
}

fn picked(state: &AppState) -> Result<Picked, &'static str> {
    let list = state.list().ok_or("Select a message")?;
    if let Some(group) = list.selected_group() {
        return match list.group_by() {
            GroupBy::Sender => Ok(Picked::Group(PinKind::Sender, group.key)),
            GroupBy::Domain => Ok(Picked::Group(PinKind::Domain, group.key)),
            GroupBy::None => Err("Select a message"),
        };
    }
    let positions = list.selected_positions();
    if positions.is_empty() {
        return Err("Select a message");
    }
    let rows: Vec<(MessageId, String)> = positions
        .into_iter()
        .filter_map(|position| list.model().row_at(position))
        .map(|row| (row.id, row.sender_email))
        .collect();
    if rows.is_empty() {
        return Err("The selected messages haven't loaded");
    }
    Ok(Picked::Messages(rows))
}

fn pinned(kind: PinKind, key: &str) -> bool {
    PINS.with(|pins| {
        pins.borrow()
            .iter()
            .any(|pin| pin.kind == kind && pin.key == key)
    })
}

/// Whether a pin names the sender `address` or its domain.
fn names(address: &str) -> bool {
    let domain = domain_key(address);
    pinned(PinKind::Sender, &sender_key(address))
        || (!domain.is_empty() && pinned(PinKind::Domain, &domain))
}

/// Whether `id`, one of [`COMMANDS`], applies to the selection, or why not.
pub fn applies(state: &AppState, id: ActionId) -> Result<(), &'static str> {
    match (id, picked(state)?) {
        (ActionId::PinSender, Picked::Group(kind, key)) => match kind {
            PinKind::Sender if pinned(kind, &key) => Err("This sender is pinned"),
            PinKind::Sender => Ok(()),
            PinKind::Domain => Err("Group by Sender to pin one sender"),
        },
        (ActionId::PinDomain, Picked::Group(kind, key)) => match kind {
            PinKind::Domain if pinned(kind, &key) => Err("This domain is pinned"),
            PinKind::Domain => Ok(()),
            PinKind::Sender => Err("Group by Domain to pin a domain"),
        },
        (ActionId::UnpinSender, Picked::Group(kind, key)) if pinned(kind, &key) => Ok(()),
        (ActionId::UnpinSender, Picked::Group(..)) => Err("This group isn't pinned"),
        (ActionId::PinSender, Picked::Messages(rows)) => {
            if rows
                .iter()
                .all(|(_, address)| pinned(PinKind::Sender, &sender_key(address)))
            {
                Err("The sender is pinned")
            } else {
                Ok(())
            }
        }
        (ActionId::PinDomain, Picked::Messages(rows)) => {
            let domains: Vec<String> = rows
                .iter()
                .map(|(_, address)| domain_key(address))
                .filter(|domain| !domain.is_empty())
                .collect();
            if domains.is_empty() {
                Err("The sender has no domain")
            } else if domains.iter().all(|domain| pinned(PinKind::Domain, domain)) {
                Err("The domain is pinned")
            } else {
                Ok(())
            }
        }
        (ActionId::UnpinSender, Picked::Messages(rows)) => {
            if rows.iter().any(|(_, address)| names(address)) {
                Ok(())
            } else {
                Err("The sender isn't pinned")
            }
        }
        _ => Err("Not a pin command"),
    }
}

/// Whether each command applies to the selection.
pub fn refresh(state: &AppState, actions: &ActionSet) {
    for id in COMMANDS {
        actions.set_applies(id, applies(state, id).is_ok());
    }
}

fn now() -> i64 {
    glib::DateTime::now_utc()
        .map(|now| now.to_unix())
        .unwrap_or_default()
}

fn run(state: &Rc<AppState>, id: ActionId) {
    if applies(state, id).is_err() {
        return;
    }
    let Ok(picked) = picked(state) else {
        return;
    };
    match id {
        ActionId::PinSender | ActionId::PinDomain => {
            let kind = if id == ActionId::PinSender {
                PinKind::Sender
            } else {
                PinKind::Domain
            };
            let targets: Vec<PinTarget> = match picked {
                Picked::Group(_, key) => vec![PinTarget::Key(key)],
                Picked::Messages(rows) => rows
                    .into_iter()
                    .map(|(id, _)| PinTarget::Message(id))
                    .collect(),
            };
            pin(state, targets, kind);
        }
        ActionId::UnpinSender => match picked {
            Picked::Group(kind, key) => {
                let removed: Vec<Pin> = pins()
                    .into_iter()
                    .filter(|pin| pin.kind == kind && pin.key == key)
                    .collect();
                if let Some(store) = state.mailbox().store() {
                    let reply = store.remove_pin(kind, key);
                    drop(store);
                    let weak = Rc::downgrade(state);
                    glib::spawn_future_local(async move {
                        let _ = reply.recv().await;
                        if let Some(state) = weak.upgrade() {
                            unpinned(&state, removed);
                        }
                    });
                }
            }
            Picked::Messages(rows) => {
                let ids: Vec<MessageId> = rows
                    .into_iter()
                    .filter(|(_, address)| names(address))
                    .map(|(id, _)| id)
                    .collect();
                unpin(state, ids);
            }
        },
        _ => {}
    }
}

fn pin(state: &Rc<AppState>, targets: Vec<PinTarget>, kind: PinKind) {
    let Some(replies) = state.mailbox().store().map(|store| {
        let at = now();
        targets
            .into_iter()
            .map(|target| store.pin(target, kind, at))
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let mut made: Vec<Pin> = Vec::new();
        for reply in replies {
            if let Ok(Some(pin)) = reply.recv().await
                && !made.contains(&pin)
            {
                made.push(pin);
            }
        }
        let Some(state) = weak.upgrade() else {
            return;
        };
        reload(&state);
        let Some(first) = made.first() else {
            return;
        };
        let who = if made.len() == 1 {
            first.key.clone()
        } else {
            format!("{} senders", made.len())
        };
        let until_read: Vec<Pin> = made
            .iter()
            .filter(|pin| pin.mode == PinMode::UntilRead)
            .cloned()
            .collect();
        let toast = adw::Toast::builder()
            .use_markup(false)
            .timeout(TOAST_SECONDS)
            .build();
        if until_read.is_empty() {
            toast.set_title(&format!("{who} is pinned"));
        } else {
            toast.set_title(&format!("{who} pinned until read"));
            toast.set_button_label(Some("Keep Pinned"));
            let weak = Rc::downgrade(&state);
            toast.connect_button_clicked(move |_| {
                if let Some(state) = weak.upgrade() {
                    let kept = until_read
                        .iter()
                        .cloned()
                        .map(|pin| Pin {
                            mode: PinMode::Keep,
                            ..pin
                        })
                        .collect();
                    restore(&state, kept);
                }
            });
        }
        state.show_toast(&toast, false);
    });
}

fn unpin(state: &Rc<AppState>, ids: Vec<MessageId>) {
    let Some(replies) = state.mailbox().store().map(|store| {
        ids.into_iter()
            .map(|id| store.unpin(id))
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let mut removed: Vec<Pin> = Vec::new();
        for reply in replies {
            if let Ok(pins) = reply.recv().await {
                for pin in pins {
                    if !removed.contains(&pin) {
                        removed.push(pin);
                    }
                }
            }
        }
        if let Some(state) = weak.upgrade() {
            unpinned(&state, removed);
        }
    });
}

/// After an unpin: the pins read again, and a toast whose Undo puts `removed` back.
fn unpinned(state: &Rc<AppState>, removed: Vec<Pin>) {
    reload(state);
    let Some(first) = removed.first() else {
        return;
    };
    let title = if removed.len() == 1 {
        format!("{} unpinned", first.key)
    } else {
        format!("{} pins removed", removed.len())
    };
    let toast = adw::Toast::builder()
        .title(title)
        .use_markup(false)
        .button_label("Undo")
        .timeout(TOAST_SECONDS)
        .build();
    let weak = Rc::downgrade(state);
    toast.connect_button_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            restore(&state, removed.clone());
        }
    });
    state.show_toast(&toast, false);
}

/// Puts `pins` back as they are given, or sets their mode.
pub fn restore(state: &Rc<AppState>, pins: Vec<Pin>) {
    let Some(reply) = state
        .mailbox()
        .store()
        .map(|store| store.restore_pins(pins))
    else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let _ = reply.recv().await;
        if let Some(state) = weak.upgrade() {
            reload(&state);
        }
    });
}

/// Takes the pin `(kind, key)` off (Preferences' remove button).
pub fn remove(state: &Rc<AppState>, kind: PinKind, key: String) {
    let Some(reply) = state
        .mailbox()
        .store()
        .map(|store| store.remove_pin(kind, key))
    else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let _ = reply.recv().await;
        if let Some(state) = weak.upgrade() {
            reload(&state);
        }
    });
}

/// The Pinned Senders dialog's name, for the self-test.
pub const DIALOG: &str = "pinned-senders";

/// Pinned Senders…: every pin, with how long its mail stays first and a button that unpins it.
pub fn present(state: &Rc<AppState>) {
    let Some(window) = state.window() else {
        return;
    };
    let group = adw::PreferencesGroup::builder()
        .title("Pinned Senders")
        .description(
            "Their mail comes first in each Inbox and in All Inboxes. Pin a sender or a domain \
             from a message's right-click menu.",
        )
        .build();
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let content = adw::ToolbarView::new();
    content.add_top_bar(&adw::HeaderBar::new());
    content.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title("Pinned Senders")
        .content_width(520)
        .content_height(460)
        .child(&content)
        .build();
    dialog.set_widget_name(DIALOG);
    let rows: Rc<RefCell<Vec<gtk::Widget>>> = Rc::default();
    let fill = {
        let (state, group, rows) = (Rc::downgrade(state), group.downgrade(), rows.clone());
        move || {
            let (Some(state), Some(group)) = (state.upgrade(), group.upgrade()) else {
                return false;
            };
            for row in rows.borrow_mut().drain(..) {
                group.remove(&row);
            }
            let pins = pins();
            if pins.is_empty() {
                let empty = adw::ActionRow::builder()
                    .title("Nothing is pinned")
                    .subtitle("Choose Pin Sender or Pin Domain on a message")
                    .build();
                empty.set_widget_name("pin-none");
                group.add(&empty);
                rows.borrow_mut().push(empty.upcast());
            }
            for (index, pin) in pins.into_iter().enumerate() {
                let row = pin_row(&state, index, pin);
                group.add(&row);
                rows.borrow_mut().push(row.upcast());
            }
            true
        }
    };
    fill();
    connect_changed(fill);
    dialog.present(Some(window.window()));
}

/// One pin's row: its key, what it names, how long its mail leads, and Unpin.
fn pin_row(state: &Rc<AppState>, index: usize, pin: Pin) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(pin.key.as_str())
        .title_selectable(true)
        .subtitle(match pin.kind {
            PinKind::Sender => "Sender",
            PinKind::Domain => "Domain",
        })
        .use_markup(false)
        .build();
    row.set_widget_name(&format!("pin-{index}"));
    let mode = gtk::DropDown::from_strings(&["Until read", "Always"]);
    mode.set_valign(gtk::Align::Center);
    mode.set_widget_name(&format!("pin-{index}-mode"));
    mode.set_tooltip_text(Some(
        "How long their mail comes first: until you read it, or as long as it is in the Inbox",
    ));
    mode.update_property(&[gtk::accessible::Property::Label("Keep first")]);
    mode.set_selected(u32::from(pin.mode == PinMode::Keep));
    let weak = Rc::downgrade(state);
    let changed = pin.clone();
    mode.connect_selected_notify(move |mode| {
        let wanted = if mode.selected() == 1 {
            PinMode::Keep
        } else {
            PinMode::UntilRead
        };
        if let Some(state) = weak.upgrade()
            && wanted != changed.mode
        {
            restore(
                &state,
                vec![Pin {
                    mode: wanted,
                    ..changed.clone()
                }],
            );
        }
    });
    let unpin = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Unpin")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    unpin.set_widget_name(&format!("pin-{index}-remove"));
    unpin.update_property(&[gtk::accessible::Property::Label("Unpin")]);
    let weak = Rc::downgrade(state);
    unpin.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            remove(&state, pin.kind, pin.key.clone());
        }
    });
    row.add_suffix(&mode);
    row.add_suffix(&unpin);
    row
}
