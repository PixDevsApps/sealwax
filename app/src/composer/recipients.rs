//! The To, Cc and Bcc rows (ML-067): each address a chip with its initials on the palette's
//! tint, the name, and × to remove it; an entry for the next one. A comma, a semicolon, Enter
//! or leaving the field turns what was typed or pasted into chips; Backspace in the empty
//! entry removes the last chip; an address that isn't valid stays as an error chip until it is
//! removed. While typing, a popover suggests addresses the user has written to (Up, Down,
//! Enter, Escape, or the pointer).

use super::backend::{Local, Suggestion};
use crate::window::panes::{classed, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use sealwax_domain::model::Address;
use sealwax_domain::theme::{avatar_class, initials};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// The mock's label column and suggestions popover, in px.
pub const LABEL_WIDTH: i32 = 56;
pub const SUGGESTIONS_WIDTH: i32 = 380;
/// Suggestions shown at most.
const MAX_SUGGESTIONS: usize = 6;

/// One recipient as typed: its address and whether it is valid.
pub type Parsed = (Address, bool);

/// Splits a typed or pasted list at commas, semicolons and line breaks outside quotes and
/// angle brackets, and parses each item (`Name <address>`, `"Name" <address>` or `address`).
pub fn parse_list(text: &str) -> Vec<Parsed> {
    let (mut items, rest) = split(text);
    items.push(rest);
    items
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(parse_one)
        .collect()
}

/// The complete items of `text` (each followed by a separator) and what follows the last
/// separator.
pub fn split(text: &str) -> (Vec<String>, String) {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut angle = false;
    for ch in text.chars() {
        match ch {
            '"' if !angle => {
                quoted = !quoted;
                current.push(ch);
            }
            '<' if !quoted => {
                angle = true;
                current.push(ch);
            }
            '>' if !quoted => {
                angle = false;
                current.push(ch);
            }
            ',' | ';' | '\n' | '\r' if !quoted && !angle => {
                items.push(std::mem::take(&mut current));
            }
            ch => current.push(ch),
        }
    }
    (items, current)
}

fn parse_one(item: &str) -> Parsed {
    let (name, email) = match (item.rfind('<'), item.ends_with('>')) {
        (Some(open), true) => (
            item[..open].trim().trim_matches('"').trim().to_owned(),
            item[open + 1..item.len() - 1].trim().to_owned(),
        ),
        _ => (
            String::new(),
            item.trim_matches(['<', '>']).trim().to_owned(),
        ),
    };
    let valid = valid_address(&email);
    (Address::new(name, email), valid)
}

/// A plausible address: one `@`, a local part without spaces or brackets, and a domain of at
/// least two dot-separated labels (letters, digits, hyphens or any non-ASCII letter).
pub fn valid_address(email: &str) -> bool {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !"<>()[]\\,;:@\"".contains(c));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && domain.len() <= 253
        && labels.iter().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || !c.is_ascii())
        });
    local_ok && domain_ok
}

type Query = Rc<dyn Fn(String) -> Local<Vec<Suggestion>>>;
type Listener = Rc<dyn Fn()>;

struct Chip {
    address: Address,
    valid: bool,
    widget: gtk::Box,
}

/// One row of recipients: its label, the chips and the entry.
pub struct RecipientField {
    root: gtk::Box,
    wrap: adw::WrapBox,
    entry: gtk::Entry,
    chips: RefCell<Vec<Chip>>,
    popover: gtk::Popover,
    list: gtk::ListBox,
    suggestions: RefCell<Vec<Suggestion>>,
    query: RefCell<Option<Query>>,
    serial: Cell<u64>,
    /// Chips made so far, for their widget names.
    made: Cell<u64>,
    /// Text being replaced by the field itself, which `changed` ignores.
    updating: Cell<bool>,
    listener: RefCell<Option<Listener>>,
    name: String,
}

impl RecipientField {
    /// `label` ("To"), `name` the widget names' prefix (`composer-to`).
    pub fn new(label: &str, name: &str) -> Rc<Self> {
        let caption = classed(
            gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .valign(gtk::Align::Center)
                .width_request(LABEL_WIDTH)
                .build(),
            &["sw-composer-field-label"],
        );
        let entry = named(
            classed(
                gtk::Entry::builder()
                    .hexpand(true)
                    .width_chars(12)
                    .has_frame(false)
                    .build(),
                &["sw-composer-recipient-entry"],
            ),
            &format!("{name}-entry"),
        );
        entry.update_property(&[gtk::accessible::Property::Label(&format!(
            "{label} recipients"
        ))]);
        let wrap = adw::WrapBox::builder()
            .child_spacing(4)
            .line_spacing(4)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        wrap.append(&entry);
        let root = named(
            classed(
                gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .build(),
                &["sw-composer-row"],
            ),
            name,
        );
        root.append(&caption);
        root.append(&wrap);

        let list = classed(
            gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::Single)
                .activate_on_single_click(true)
                .can_focus(false)
                .width_request(SUGGESTIONS_WIDTH)
                .build(),
            &["sw-composer-suggestions-list"],
        );
        let popover = named(
            classed(
                gtk::Popover::builder()
                    .child(&list)
                    .autohide(false)
                    .has_arrow(false)
                    .can_focus(false)
                    .position(gtk::PositionType::Bottom)
                    .halign(gtk::Align::Start)
                    .build(),
                &["sw-composer-suggestions"],
            ),
            &format!("{name}-suggestions"),
        );
        popover.set_parent(&entry);

        let field = Rc::new(Self {
            root,
            wrap,
            entry,
            chips: RefCell::new(Vec::new()),
            popover,
            list,
            suggestions: RefCell::new(Vec::new()),
            query: RefCell::new(None),
            serial: Cell::new(0),
            made: Cell::new(0),
            updating: Cell::new(false),
            listener: RefCell::new(None),
            name: name.to_owned(),
        });
        field.connect();
        field
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn entry(&self) -> &gtk::Entry {
        &self.entry
    }

    /// Something at the row's end, such as "Cc · Bcc".
    pub fn append_end(&self, widget: &impl IsA<gtk::Widget>) {
        self.root.append(widget);
    }

    /// Runs `listener` whenever the recipients change.
    pub fn connect_changed(&self, listener: impl Fn() + 'static) {
        self.listener.replace(Some(Rc::new(listener)));
    }

    /// Where suggestions come from.
    pub fn set_query(&self, query: impl Fn(String) -> Local<Vec<Suggestion>> + 'static) {
        self.query.replace(Some(Rc::new(query)));
    }

    /// Every recipient, valid or not, with the text still in the entry.
    pub fn all(&self) -> Vec<Parsed> {
        let mut all: Vec<Parsed> = self
            .chips
            .borrow()
            .iter()
            .map(|chip| (chip.address.clone(), chip.valid))
            .collect();
        all.extend(parse_list(&self.entry.text()));
        all
    }

    /// The valid recipients, the entry's text included.
    pub fn addresses(&self) -> Vec<Address> {
        self.all()
            .into_iter()
            .filter_map(|(address, valid)| valid.then_some(address))
            .collect()
    }

    /// The valid chips' addresses, without the entry's text.
    pub fn chip_addresses(&self) -> Vec<Address> {
        self.chips
            .borrow()
            .iter()
            .filter(|chip| chip.valid)
            .map(|chip| chip.address.clone())
            .collect()
    }

    pub fn invalid(&self) -> usize {
        self.all().iter().filter(|(_, valid)| !valid).count()
    }

    pub fn is_empty(&self) -> bool {
        self.chips.borrow().is_empty() && self.entry.text().trim().is_empty()
    }

    /// Replaces the chips with `addresses` (a draft being opened).
    pub fn set_addresses(self: &Rc<Self>, addresses: &[Address]) {
        for chip in self.chips.take() {
            self.wrap.remove(&chip.widget);
        }
        for address in addresses {
            let valid = valid_address(&address.email);
            self.add_chip(address.clone(), valid);
        }
    }

    /// Turns the entry's text into chips.
    pub fn commit(self: &Rc<Self>) {
        let text = self.entry.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.set_entry_text("");
        let mut added = false;
        for (address, valid) in parse_list(&text) {
            self.add_chip(address, valid);
            added = true;
        }
        self.close_suggestions();
        if added {
            self.changed();
        }
    }

    /// Removes the chip at `index`.
    pub fn remove(self: &Rc<Self>, index: usize) {
        let removed = {
            let mut chips = self.chips.borrow_mut();
            (index < chips.len()).then(|| chips.remove(index))
        };
        if let Some(chip) = removed {
            self.wrap.remove(&chip.widget);
            self.changed();
        }
    }

    /// The chips' widgets, for the self-tests.
    pub fn chip_widgets(&self) -> Vec<gtk::Box> {
        self.chips
            .borrow()
            .iter()
            .map(|chip| chip.widget.clone())
            .collect()
    }

    pub fn suggestions_open(&self) -> bool {
        self.popover.is_visible()
    }

    pub fn suggestions(&self) -> Vec<Suggestion> {
        self.suggestions.borrow().clone()
    }

    pub fn suggestion_rows(&self) -> Vec<gtk::ListBoxRow> {
        let mut rows = Vec::new();
        let mut child = self.list.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                rows.push(row.clone());
            }
            child = widget.next_sibling();
        }
        rows
    }

    pub fn selected_suggestion(&self) -> Option<usize> {
        self.list
            .selected_row()
            .and_then(|row| usize::try_from(row.index()).ok())
    }

    pub fn popover(&self) -> &gtk::Popover {
        &self.popover
    }

    /// Takes suggestion `index` as a chip and clears the entry.
    pub fn take_suggestion(self: &Rc<Self>, index: usize) {
        let Some(suggestion) = self.suggestions.borrow().get(index).cloned() else {
            return;
        };
        self.set_entry_text("");
        self.add_chip(suggestion.address, true);
        self.close_suggestions();
        self.entry.grab_focus_without_selecting();
        self.changed();
    }

    /// Whether the keyboard focus is in the entry (its text widget has it).
    pub fn focused(&self) -> bool {
        self.entry
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus == self.entry || focus.is_ancestor(&self.entry))
    }

    fn changed(&self) {
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener();
        }
    }

    fn set_entry_text(&self, text: &str) {
        self.updating.set(true);
        self.entry.set_text(text);
        self.entry.set_position(-1);
        self.updating.set(false);
    }

    fn add_chip(self: &Rc<Self>, address: Address, valid: bool) {
        let made = self.made.get();
        self.made.set(made + 1);
        let widget = chip(&address, valid, &format!("{}-chip-{made}", self.name));
        let remove = widget
            .last_child()
            .and_downcast::<gtk::Button>()
            .expect("a chip ends with its remove button");
        let weak = Rc::downgrade(self);
        let target = widget.downgrade();
        remove.connect_clicked(move |_| {
            if let (Some(field), Some(target)) = (weak.upgrade(), target.upgrade()) {
                let index = field
                    .chips
                    .borrow()
                    .iter()
                    .position(|chip| chip.widget == target);
                if let Some(index) = index {
                    field.remove(index);
                    field.entry.grab_focus_without_selecting();
                }
            }
        });
        self.wrap.insert_child_after(
            &widget,
            self.chips
                .borrow()
                .last()
                .map(|chip| chip.widget.upcast_ref::<gtk::Widget>()),
        );
        self.chips.borrow_mut().push(Chip {
            address,
            valid,
            widget,
        });
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.entry.connect_changed(move |entry| {
            let Some(field) = weak.upgrade() else {
                return;
            };
            if field.updating.get() {
                return;
            }
            let text = entry.text().to_string();
            let (items, rest) = split(&text);
            if !items.is_empty() {
                let complete: Vec<Parsed> = items
                    .iter()
                    .map(|item| item.trim())
                    .filter(|item| !item.is_empty())
                    .map(parse_one)
                    .collect();
                field.set_entry_text(rest.trim_start());
                let added = !complete.is_empty();
                for (address, valid) in complete {
                    field.add_chip(address, valid);
                }
                if added {
                    field.changed();
                }
            }
            field.suggest();
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |_| {
            if let Some(field) = weak.upgrade() {
                field.commit();
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(field) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            field.key(key, state)
        });
        self.entry.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(field) = weak.upgrade()
                    && !field.focused()
                {
                    field.commit();
                    field.close_suggestions();
                }
            });
        });
        self.entry.add_controller(focus);

        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, row| {
            if let (Some(field), Ok(index)) = (weak.upgrade(), usize::try_from(row.index())) {
                field.take_suggestion(index);
            }
        });
        let weak: Weak<Self> = Rc::downgrade(self);
        self.list.connect_selected_rows_changed(move |list| {
            let Some(selected) = list.selected_row() else {
                return;
            };
            if let Some(field) = weak.upgrade() {
                for row in field.suggestion_rows() {
                    if let Some(hint) = row.child().and_then(|child| child.last_child()) {
                        hint.set_visible(row == selected);
                    }
                }
            }
        });
    }

    fn key(self: &Rc<Self>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let plain = !state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SHIFT_MASK,
        );
        let open = self.suggestions_open() && !self.suggestions.borrow().is_empty();
        match key {
            gdk::Key::BackSpace if plain && self.entry.text().is_empty() => {
                let last = self.chips.borrow().len().checked_sub(1);
                if let Some(last) = last {
                    self.remove(last);
                }
                glib::Propagation::Stop
            }
            gdk::Key::Down | gdk::Key::Up if plain && open => {
                let count = self.suggestions.borrow().len();
                let current = self.selected_suggestion().unwrap_or(0);
                let next = if key == gdk::Key::Down {
                    (current + 1).min(count - 1)
                } else {
                    current.saturating_sub(1)
                };
                self.select(next);
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter if plain && open => {
                if let Some(index) = self.selected_suggestion() {
                    self.take_suggestion(index);
                } else {
                    self.commit();
                }
                glib::Propagation::Stop
            }
            gdk::Key::Escape if open => {
                self.close_suggestions();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    pub fn select(&self, index: usize) {
        if let Some(row) = self.list.row_at_index(index as i32) {
            self.list.select_row(Some(&row));
        }
    }

    pub fn close_suggestions(&self) {
        self.serial.set(self.serial.get() + 1);
        self.suggestions.borrow_mut().clear();
        if self.popover.is_visible() {
            self.popover.popdown();
        }
    }

    /// Asks for suggestions for the entry's text; a newer question drops an older answer.
    fn suggest(self: &Rc<Self>) {
        let query = self.entry.text().trim().to_owned();
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        let source = self.query.borrow().clone();
        let (false, Some(source)) = (query.is_empty(), source) else {
            self.close_suggestions();
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let found = source(query.clone()).await;
            let Some(field) = weak.upgrade() else {
                return;
            };
            if field.serial.get() != serial || !field.focused() {
                return;
            }
            let taken: Vec<String> = field
                .chips
                .borrow()
                .iter()
                .map(|chip| chip.address.email.to_lowercase())
                .collect();
            let found: Vec<Suggestion> = found
                .into_iter()
                .filter(|suggestion| !taken.contains(&suggestion.address.email.to_lowercase()))
                .take(MAX_SUGGESTIONS)
                .collect();
            field.show_suggestions(&query, found);
        });
    }

    fn show_suggestions(&self, query: &str, found: Vec<Suggestion>) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        if found.is_empty() {
            self.suggestions.borrow_mut().clear();
            if self.popover.is_visible() {
                self.popover.popdown();
            }
            return;
        }
        for (index, suggestion) in found.iter().enumerate() {
            let row = gtk::ListBoxRow::builder()
                .child(&suggestion_row(suggestion, query))
                .can_focus(false)
                .build();
            row.set_widget_name(&format!("{}-suggestion-{index}", self.name));
            self.list.append(&row);
        }
        self.suggestions.replace(found);
        self.select(0);
        if self.popover.is_visible() {
            self.popover.present();
        } else {
            self.popover.popup();
        }
        self.entry.grab_focus_without_selecting();
    }
}

impl Drop for RecipientField {
    fn drop(&mut self) {
        self.popover.unparent();
    }
}

/// A recipient chip: the initials on the address's tint, the name, and ×.
fn chip(address: &Address, valid: bool, name: &str) -> gtk::Box {
    let shown = address.display_name().to_owned();
    let avatar = classed(
        gtk::Label::builder()
            .label(if valid {
                initials(&address.name, &address.email)
            } else {
                "!".to_owned()
            })
            .valign(gtk::Align::Center)
            .build(),
        &[
            "sw-avatar",
            "sw-composer-chip-avatar",
            &avatar_class(&address.email),
        ],
    );
    let label = classed(
        gtk::Label::builder()
            .label(&shown)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(32)
            .build(),
        &["sw-composer-chip-name"],
    );
    let remove = named(
        classed(
            gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .tooltip_text(format!("Remove {shown}"))
                .valign(gtk::Align::Center)
                .can_focus(false)
                .build(),
            &["sw-composer-chip-remove"],
        ),
        &format!("{name}-remove"),
    );
    remove.update_property(&[gtk::accessible::Property::Label(&format!("Remove {shown}"))]);
    let chip = named(
        classed(
            gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .valign(gtk::Align::Center)
                .build(),
            &["sw-composer-chip"],
        ),
        name,
    );
    if !valid {
        chip.add_css_class("sw-invalid");
    }
    let full = if address.name.is_empty() {
        address.email.clone()
    } else {
        format!("{} <{}>", address.name, address.email)
    };
    chip.set_tooltip_text(Some(&if valid {
        full
    } else {
        format!("{full}: not a valid address")
    }));
    chip.append(&avatar);
    chip.append(&label);
    chip.append(&remove);
    chip
}

/// A suggestion: a 24 px avatar, the name with the typed part in bold, the address and how
/// many messages, and "Enter" on the selected row.
fn suggestion_row(suggestion: &Suggestion, query: &str) -> gtk::Box {
    let address = &suggestion.address;
    let avatar = classed(
        gtk::Label::builder()
            .label(initials(&address.name, &address.email))
            .valign(gtk::Align::Center)
            .build(),
        &[
            "sw-avatar",
            "sw-avatar-small",
            &avatar_class(&address.email),
        ],
    );
    let name = classed(
        gtk::Label::builder()
            .use_markup(true)
            .label(highlight(address.display_name(), query))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
        &["sw-composer-suggestion-name"],
    );
    let messages = match suggestion.messages {
        1 => "1 message".to_owned(),
        count => format!("{count} messages"),
    };
    let detail = classed(
        gtk::Label::builder()
            .label(format!("{} · {messages}", address.email))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
        &["sw-composer-suggestion-detail"],
    );
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(&name);
    text.append(&detail);
    let hint = classed(
        gtk::Label::builder()
            .label("Enter")
            .valign(gtk::Align::Center)
            .visible(false)
            .build(),
        &["sw-composer-key-hint"],
    );
    let row = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build(),
        &["sw-composer-suggestion"],
    );
    row.append(&avatar);
    row.append(&text);
    row.append(&hint);
    row
}

/// `text` as Pango markup with the first case-insensitive match of `query` in bold.
pub fn highlight(text: &str, query: &str) -> String {
    let lower = text.to_lowercase();
    let wanted = query.trim().to_lowercase();
    let found = (!wanted.is_empty())
        .then(|| lower.find(&wanted))
        .flatten()
        .filter(|at| lower.len() == text.len() && text.is_char_boundary(*at));
    match found {
        Some(at) if text.is_char_boundary(at + wanted.len()) => format!(
            "{}<b>{}</b>{}",
            glib::markup_escape_text(&text[..at]),
            glib::markup_escape_text(&text[at..at + wanted.len()]),
            glib::markup_escape_text(&text[at + wanted.len()..])
        ),
        _ => glib::markup_escape_text(text).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_split_outside_quotes_and_brackets() {
        let parsed = parse_list(
            "\"Nordmann, Kari\" <kari@example.com>; ola@example.org,\nBjørn Ærø <bjorn@example.net>",
        );
        assert_eq!(
            parsed,
            vec![
                (Address::new("Nordmann, Kari", "kari@example.com"), true),
                (Address::new("", "ola@example.org"), true),
                (Address::new("Bjørn Ærø", "bjorn@example.net"), true),
            ]
        );
        let (items, rest) = split("a@example.test, b@exa");
        assert_eq!(items, ["a@example.test"]);
        assert_eq!(rest, " b@exa");
    }

    #[test]
    fn invalid_addresses_are_kept_and_marked() {
        for bad in [
            "bob",
            "bob@",
            "@example.com",
            "bob@example",
            "a b@example.com",
            "x@-a.com",
        ] {
            assert!(!valid_address(bad), "{bad}");
        }
        for good in [
            "bob@example.com",
            "o.ly+tag@sub.example.test",
            "ola@blåbær.example",
        ] {
            assert!(valid_address(good), "{good}");
        }
        assert_eq!(
            parse_list("not an address"),
            vec![(Address::new("", "not an address"), false)]
        );
    }

    #[test]
    fn the_typed_part_is_bold() {
        assert_eq!(highlight("Ada Lovelace", "love"), "Ada <b>Love</b>lace");
        assert_eq!(highlight("Øystein", "øy"), "<b>Øy</b>stein");
        assert_eq!(highlight("A & B", "zz"), "A &amp; B");
    }
}
