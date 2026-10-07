//! The search field's suggestions (ML-082): a popover under the field with the operators that
//! start with the word typed, the values of `has:` and `is:`, days after `before:` and `after:`
//! (`domain::search::hints`), and people after `from:` and `to:` from the store's contacts,
//! asked off the GTK thread. It opens as the text changes while the field has the keyboard
//! focus, follows the cursor, and closes when the focus leaves; Down with it closed lists every
//! operator that starts with the word at the cursor (all of them in an empty field). The field
//! keeps the focus: Down and Up move in the list, Tab takes the row marked (the first until one
//! is chosen), Enter takes a chosen row and otherwise goes to the results, Escape closes the
//! list. A click takes a row too.

use crate::composer::recipients::highlight;
use crate::mailbox::Mailbox;
use crate::window::panes::{classed, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::model::AccountId;
use sealwax_domain::search::Date;
use sealwax_domain::search::hints::{self, At, Hints, Kind, Suggestion};
use sealwax_domain::theme::{avatar_class, initials};
use sealwax_infrastructure::store::Contact;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

/// People suggested at most, as the composer's suggestions.
pub const MAX_PEOPLE: usize = 6;
/// The popover's padding and border on each side (style.css), so it is as wide as the field.
const POPOVER_INSET: i32 = 5;

/// What the suggestions need from the list: today (the list's clock) and the accounts whose
/// people are suggested.
type Context = Rc<dyn Fn() -> Option<(Date, Vec<AccountId>)>>;

/// A row as the self-test sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub title: String,
    pub detail: String,
    /// It can be taken (a day's format can't).
    pub takeable: bool,
}

#[derive(Clone)]
pub struct Suggestions {
    inner: Rc<Inner>,
}

struct Inner {
    entry: gtk::SearchEntry,
    popover: gtk::Popover,
    list: gtk::ListBox,
    mailbox: Rc<Mailbox>,
    /// The rows shown and what each inserts.
    rows: RefCell<Vec<(Shown, Option<String>)>>,
    /// The word the rows replace, in characters.
    word: RefCell<Range<usize>>,
    /// Bumped by every question, so an older answer about people is dropped.
    serial: Cell<u64>,
    context: RefCell<Option<Context>>,
    /// An update waits for the main loop to turn, so the cursor has its place: `Some(true)`
    /// when the text changed.
    queued: Cell<Option<bool>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.popover.unparent();
    }
}

impl Suggestions {
    pub fn new(entry: &gtk::SearchEntry, mailbox: Rc<Mailbox>) -> Self {
        let list = named(
            classed(
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::Single)
                    .activate_on_single_click(true)
                    .can_focus(false)
                    .build(),
                &["sw-search-suggestions-list"],
            ),
            "list-search-suggestions-list",
        );
        list.update_property(&[gtk::accessible::Property::Label("Search suggestions")]);
        let popover = named(
            classed(
                gtk::Popover::builder()
                    .child(&list)
                    .autohide(false)
                    .has_arrow(false)
                    .can_focus(false)
                    .position(gtk::PositionType::Bottom)
                    .halign(gtk::Align::Center)
                    .build(),
                &["sw-search-suggestions"],
            ),
            "list-search-suggestions",
        );
        popover.set_parent(entry);
        let suggestions = Self {
            inner: Rc::new(Inner {
                entry: entry.clone(),
                popover,
                list,
                mailbox,
                rows: RefCell::new(Vec::new()),
                word: RefCell::new(0..0),
                serial: Cell::new(0),
                context: RefCell::new(None),
                queued: Cell::new(None),
            }),
        };
        suggestions.connect();
        suggestions
    }

    /// Where today and the accounts come from.
    pub fn set_context(&self, context: impl Fn() -> Option<(Date, Vec<AccountId>)> + 'static) {
        self.inner.context.replace(Some(Rc::new(context)));
    }

    fn connect(&self) {
        let entry = &self.inner.entry;
        let weak = Rc::downgrade(&self.inner);
        entry.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                Suggestions { inner }.queue_update(true);
            }
        });
        let weak = Rc::downgrade(&self.inner);
        entry.connect_cursor_position_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                Suggestions { inner }.queue_update(false);
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&self.inner);
        keys.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
            Some(inner) => Suggestions { inner }.key(key, state),
            None => glib::Propagation::Proceed,
        });
        entry.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&self.inner);
        focus.connect_leave(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade()
                    && !focused(&inner.entry)
                {
                    Suggestions { inner }.close();
                }
            });
        });
        entry.add_controller(focus);
        let weak = Rc::downgrade(&self.inner);
        self.inner.list.connect_row_activated(move |_, row| {
            if let (Some(inner), Ok(index)) = (weak.upgrade(), usize::try_from(row.index())) {
                Suggestions { inner }.take(index);
            }
        });
        let weak = Rc::downgrade(&self.inner);
        self.inner.list.connect_selected_rows_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                Suggestions { inner }.mark();
            }
        });
    }

    pub fn popover(&self) -> &gtk::Popover {
        &self.inner.popover
    }

    pub fn is_open(&self) -> bool {
        self.inner.popover.is_visible()
    }

    /// The rows shown now (none while closed).
    pub fn shown(&self) -> Vec<Shown> {
        if !self.is_open() {
            return Vec::new();
        }
        self.inner
            .rows
            .borrow()
            .iter()
            .map(|(shown, _)| shown.clone())
            .collect()
    }

    /// The row chosen with Down or Up.
    pub fn selected(&self) -> Option<usize> {
        self.inner
            .list
            .selected_row()
            .and_then(|row| usize::try_from(row.index()).ok())
    }

    pub fn close(&self) {
        let inner = &self.inner;
        inner.serial.set(inner.serial.get() + 1);
        if inner.popover.is_visible() {
            inner.popover.popdown();
        }
    }

    fn queue_update(&self, opening: bool) {
        let inner = &self.inner;
        let queued = inner
            .queued
            .replace(Some(opening || inner.queued.get() == Some(true)));
        if queued.is_some() {
            return;
        }
        let weak = Rc::downgrade(inner);
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade()
                && let Some(opening) = inner.queued.take()
            {
                Suggestions { inner }.update(opening);
            }
        });
    }

    /// Looks again at the word at the field's cursor: `opening` (the text changed) shows the
    /// rows; otherwise (the cursor moved) only a list on screen follows. Nothing opens while
    /// the field hasn't got the keyboard focus (the list set its text).
    pub fn update(&self, opening: bool) {
        let inner = &self.inner;
        if (!opening && !self.is_open()) || !focused(&inner.entry) {
            self.close();
            return;
        }
        let context = inner.context.borrow().clone();
        let Some((today, accounts)) = context.and_then(|context| context()) else {
            self.close();
            return;
        };
        let serial = inner.serial.get() + 1;
        inner.serial.set(serial);
        let text = inner.entry.text();
        let cursor = usize::try_from(inner.entry.position()).unwrap_or_default();
        // An open list of operators (Down) narrows by every letter typed.
        let found = hints::at(&text, cursor, today).or_else(|| {
            self.is_open()
                .then(|| hints::operators_at(&text, cursor))
                .flatten()
                .filter(|at| !at.word.is_empty())
        });
        let Some(At { word, hints }) = found else {
            self.close();
            return;
        };
        match hints {
            Hints::Fixed(found) => self.show(word, found.into_iter().map(fixed_row).collect()),
            Hints::People { operator, prefix } => {
                let Some(store) = inner.mailbox.shared_store() else {
                    self.close();
                    return;
                };
                let replies: Vec<_> = accounts
                    .into_iter()
                    .map(|account| store.recipients(account, &prefix, MAX_PEOPLE as u32))
                    .collect();
                let weak = Rc::downgrade(&self.inner);
                glib::spawn_future_local(async move {
                    let mut people: Vec<Contact> = Vec::new();
                    for reply in replies {
                        people.extend(reply.recv().await.unwrap_or_default());
                    }
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    if inner.serial.get() != serial || !focused(&inner.entry) {
                        return;
                    }
                    let rows = merge(people)
                        .iter()
                        .map(|contact| {
                            (
                                person_row(contact, &prefix),
                                Some(hints::person(operator, &contact.address)),
                            )
                        })
                        .collect();
                    Suggestions { inner }.show(word, rows);
                });
            }
        }
    }

    fn show(&self, word: Range<usize>, rows: Vec<((Shown, gtk::Box), Option<String>)>) {
        let inner = &self.inner;
        while let Some(row) = inner.list.row_at_index(0) {
            inner.list.remove(&row);
        }
        if rows.is_empty() {
            inner.rows.borrow_mut().clear();
            self.close();
            return;
        }
        let mut kept = Vec::with_capacity(rows.len());
        for (index, ((shown, child), insert)) in rows.into_iter().enumerate() {
            let row = gtk::ListBoxRow::builder()
                .child(&child)
                .can_focus(false)
                .activatable(insert.is_some())
                .selectable(insert.is_some())
                .build();
            row.set_widget_name(&format!("list-search-suggestion-{index}"));
            if insert.is_none() {
                row.add_css_class("sw-search-suggestion-hint");
            }
            inner.list.append(&row);
            kept.push((shown, insert));
        }
        inner.rows.replace(kept);
        inner.word.replace(word);
        inner.list.unselect_all();
        self.mark();
        // As wide as the field's border box (`width()` is its content box).
        let width = inner
            .entry
            .compute_bounds(&inner.entry)
            .map_or(0.0, |bounds| bounds.width());
        inner
            .list
            .set_size_request((width as i32 - 2 * POPOVER_INSET).max(1), -1);
        // An open popover is presented again at its new size, as the composer's is: left to
        // resize on its own it sometimes vanished (headless, when its rows went from 1 to 4).
        if inner.popover.is_visible() {
            inner.popover.present();
        } else {
            inner.popover.popup();
        }
    }

    /// The row Tab or Enter takes: the one chosen, else the first that can be taken.
    fn target(&self) -> Option<usize> {
        self.selected().or_else(|| {
            self.inner
                .rows
                .borrow()
                .iter()
                .position(|(_, insert)| insert.is_some())
        })
    }

    /// Shows the key on the row it takes: "Enter" on a chosen row, "Tab" on the first.
    fn mark(&self) {
        let target = self.target();
        let key = if self.selected().is_some() {
            "Enter"
        } else {
            "Tab"
        };
        let mut child = self.inner.list.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
                && let Some(hint) = row
                    .child()
                    .and_then(|child| child.last_child())
                    .and_downcast::<gtk::Label>()
            {
                let marked = usize::try_from(row.index()).ok() == target;
                hint.set_label(key);
                hint.set_visible(marked);
            }
            child = widget.next_sibling();
        }
    }

    /// Down, Up, Tab, Enter and Escape while the list is open; any other key goes to the field.
    pub fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let plain = !state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SHIFT_MASK,
        );
        if !self.is_open() {
            return if key == gdk::Key::Down && plain && self.browse() {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            };
        }
        match key {
            gdk::Key::Down | gdk::Key::Up if plain => {
                self.step(if key == gdk::Key::Down { 1 } else { -1 });
                glib::Propagation::Stop
            }
            gdk::Key::Tab | gdk::Key::KP_Tab if plain => match self.target() {
                Some(index) => {
                    self.take(index);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            },
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter if plain => {
                match self.selected() {
                    Some(index) => {
                        self.take(index);
                        glib::Propagation::Stop
                    }
                    None => {
                        self.close();
                        glib::Propagation::Proceed
                    }
                }
            }
            gdk::Key::Escape => {
                self.close();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    /// Down with the list closed: every operator that starts with the word at the cursor (all of
    /// them in an empty field). Whether there were any.
    fn browse(&self) -> bool {
        let inner = &self.inner;
        let cursor = usize::try_from(inner.entry.position()).unwrap_or_default();
        let Some(At {
            word,
            hints: Hints::Fixed(found),
        }) = hints::operators_at(&inner.entry.text(), cursor)
        else {
            return false;
        };
        inner.serial.set(inner.serial.get() + 1);
        self.show(word, found.into_iter().map(fixed_row).collect());
        true
    }

    /// Chooses the next or previous row that can be taken; Up from the first leaves the list
    /// with nothing chosen.
    fn step(&self, delta: i64) {
        let takeable: Vec<usize> = self
            .inner
            .rows
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, (_, insert))| insert.is_some())
            .map(|(index, _)| index)
            .collect();
        let at = self
            .selected()
            .and_then(|selected| takeable.iter().position(|index| *index == selected));
        let next = match (at, delta > 0) {
            (None, true) => takeable.first().copied(),
            (None, false) => None,
            (Some(at), true) => takeable.get(at + 1).or(takeable.last()).copied(),
            (Some(0), false) => None,
            (Some(at), false) => takeable.get(at - 1).copied(),
        };
        match next.and_then(|index| self.inner.list.row_at_index(index as i32)) {
            Some(row) => {
                self.inner.list.select_row(Some(&row));
                // The field keeps the focus, so a screen reader hears the row this way.
                crate::accessibility::announce(
                    &self.inner.entry,
                    &crate::accessibility::shown_text(&row),
                );
            }
            None => self.inner.list.unselect_all(),
        }
    }

    /// Takes row `index`: its text replaces the word, and the cursor goes after it.
    pub fn take(&self, index: usize) {
        let inner = &self.inner;
        let Some(insert) = inner
            .rows
            .borrow()
            .get(index)
            .and_then(|(_, insert)| insert.clone())
        else {
            return;
        };
        let word = inner.word.borrow().clone();
        let (text, cursor) = hints::accept(&inner.entry.text(), word, &insert);
        self.close();
        inner.entry.set_text(&text);
        inner
            .entry
            .set_position(i32::try_from(cursor).unwrap_or(i32::MAX));
    }
}

/// Whether the keyboard focus is in `entry` (its text, in fact).
fn focused(entry: &gtk::SearchEntry) -> bool {
    entry
        .root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| {
            &focus == entry.upcast_ref::<gtk::Widget>() || focus.is_ancestor(entry)
        })
}

/// The contacts of several accounts as one list: each address once, ranked as the store ranks
/// them (written to most, then seen most, then most recently).
fn merge(mut people: Vec<Contact>) -> Vec<Contact> {
    people.sort_by(|a, b| {
        (b.written, b.seen, b.last)
            .cmp(&(a.written, a.seen, a.last))
            .then_with(|| a.address.cmp(&b.address))
    });
    let mut seen = std::collections::HashSet::new();
    people.retain(|contact| seen.insert(contact.address.clone()));
    people.truncate(MAX_PEOPLE);
    people
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    classed(
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
        classes,
    )
}

fn key_hint() -> gtk::Label {
    classed(
        gtk::Label::builder()
            .label("Tab")
            .valign(gtk::Align::Center)
            .visible(false)
            .build(),
        &["sw-search-key-hint"],
    )
}

fn row_box(start: Option<&gtk::Widget>, text: &gtk::Box, end: Option<&gtk::Label>) -> gtk::Box {
    let row = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build(),
        &["sw-search-suggestion"],
    );
    if let Some(start) = start {
        row.append(start);
    }
    row.append(text);
    if let Some(end) = end {
        row.append(end);
    }
    row.append(&key_hint());
    row
}

fn text_column(first: &gtk::Label, second: &gtk::Label) -> gtk::Box {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    column.append(first);
    column.append(second);
    column
}

/// An operator, a value or a day: the text it inserts over what it finds, an operator's example
/// at the end.
fn fixed_row(suggestion: Suggestion) -> ((Shown, gtk::Box), Option<String>) {
    let title = label(&suggestion.title, &["sw-search-suggestion-term"]);
    let detail = label(&suggestion.detail, &["sw-search-suggestion-detail"]);
    let example = (suggestion.kind == Kind::Operator).then(|| {
        classed(
            gtk::Label::builder()
                .label(&suggestion.example)
                .valign(gtk::Align::Center)
                .build(),
            &["sw-search-suggestion-example"],
        )
    });
    let row = row_box(None, &text_column(&title, &detail), example.as_ref());
    let shown = Shown {
        title: suggestion.title,
        detail: suggestion.detail,
        takeable: suggestion.insert.is_some(),
    };
    ((shown, row), suggestion.insert)
}

/// A person: initials on the address's tint, the name with the typed part in bold, the address
/// and how many messages it was seen in.
fn person_row(contact: &Contact, prefix: &str) -> (Shown, gtk::Box) {
    let avatar = classed(
        gtk::Label::builder()
            .label(initials(&contact.name, &contact.address))
            .valign(gtk::Align::Center)
            .build(),
        &[
            "sw-avatar",
            "sw-avatar-small",
            &avatar_class(&contact.address),
        ],
    );
    let shown_name = if contact.name.is_empty() {
        contact.address.as_str()
    } else {
        contact.name.as_str()
    };
    let name = label("", &["sw-search-suggestion-name"]);
    name.set_markup(&highlight(shown_name, prefix));
    let messages = match contact.seen {
        1 => "1 message".to_owned(),
        count => format!("{count} messages"),
    };
    let detail_text = format!("{} · {messages}", contact.address);
    let detail = label(&detail_text, &["sw-search-suggestion-detail"]);
    let row = row_box(
        Some(avatar.upcast_ref()),
        &text_column(&name, &detail),
        None,
    );
    let shown = Shown {
        title: shown_name.to_owned(),
        detail: detail_text,
        takeable: true,
    };
    (shown, row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(address: &str, written: u32, seen: u32, last: i64) -> Contact {
        Contact {
            address: address.to_owned(),
            name: String::new(),
            seen,
            written,
            last,
        }
    }

    #[test]
    fn several_accounts_people_merge_by_rank() {
        let merged = merge(vec![
            contact("b@example.com", 0, 9, 5),
            contact("a@example.org", 2, 1, 1),
            contact("c@example.net", 0, 9, 7),
            contact("a@example.org", 0, 3, 9),
        ]);
        let order: Vec<&str> = merged.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(order, ["a@example.org", "c@example.net", "b@example.com"]);
        let many = (0..10)
            .map(|n| contact(&format!("p{n}@example.com"), 0, n, 0))
            .collect();
        assert_eq!(merge(many).len(), MAX_PEOPLE);
    }
}
