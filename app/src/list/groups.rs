//! The message list grouped by sender or by domain (issue #24, ADR-023): one row per sender, or
//! per domain, newest first, with its name, how many messages and how many unread, and the
//! newest subject. Opening a row (Enter or a click) shows its mail; the list does that with a
//! `sender:` or `domain:` search, so Escape comes back here. A selected row takes the mail
//! commands, which act on all of its mail in the view (issue #25). A pinned group shows a pin,
//! and in the Inboxes the store puts it first (issue #26).

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib, pango};
use sealwax_infrastructure::store::SenderGroup;
use std::cell::Cell;
use std::rc::Rc;

pub(super) struct Groups {
    scroller: gtk::ScrolledWindow,
    view: gtk::ListView,
    items: gio::ListStore,
    selection: gtk::SingleSelection,
    /// Set while `set` replaces the rows, whose passing selections aren't the user's.
    quiet: Rc<Cell<bool>>,
}

impl Groups {
    pub(super) fn new() -> Self {
        let items = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&items)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, object| {
            if let Some(list_item) = object.downcast_ref::<gtk::ListItem>() {
                list_item.set_child(Some(&row()));
            }
        });
        factory.connect_bind(|_, object| {
            let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if let (Some(row), Some(item)) = (
                list_item.child().and_downcast::<gtk::Box>(),
                list_item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) {
                bind(list_item, &row, &item.borrow::<SenderGroup>());
            }
        });
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .single_click_activate(false)
            .tab_behavior(gtk::ListTabBehavior::Item)
            .build();
        view.add_css_class("sw-message-list");
        view.set_widget_name("sender-groups");
        view.update_property(&[gtk::accessible::Property::Label("Senders")]);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        Self {
            scroller,
            view,
            items,
            selection,
            quiet: Rc::default(),
        }
    }

    pub(super) fn widget(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    pub(super) fn view(&self) -> &gtk::ListView {
        &self.view
    }

    pub(super) fn len(&self) -> u32 {
        self.items.n_items()
    }

    /// Shows `groups`, selecting the row of the first of `wanted` (groups' keys) that is there.
    pub(super) fn set(&self, groups: Vec<SenderGroup>, wanted: &[&str]) {
        let keep = wanted
            .iter()
            .find_map(|key| groups.iter().position(|group| group.key == *key));
        let objects: Vec<glib::BoxedAnyObject> =
            groups.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.quiet.set(true);
        self.items.splice(0, self.items.n_items(), &objects);
        match keep {
            Some(position) => {
                let position = position as u32;
                self.selection.set_selected(position);
                self.view
                    .scroll_to(position, gtk::ListScrollFlags::NONE, None);
            }
            None => self.selection.set_selected(gtk::INVALID_LIST_POSITION),
        }
        self.quiet.set(false);
    }

    /// The selected group.
    pub(super) fn selected(&self) -> Option<SenderGroup> {
        self.selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|item| item.borrow::<SenderGroup>().clone())
    }

    /// The key of the group a removed selected row hands the selection to: the next one, or
    /// the one before when it is the last.
    pub(super) fn neighbour(&self) -> Option<String> {
        let position = self.selection.selected();
        if position == gtk::INVALID_LIST_POSITION {
            return None;
        }
        [position + 1, position.wrapping_sub(1)]
            .into_iter()
            .find_map(|candidate| self.items.item(candidate))
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|item| item.borrow::<SenderGroup>().key.clone())
    }

    /// Selects the group at `position` (the self-test's click).
    #[cfg(feature = "test-hooks")]
    pub(super) fn select(&self, position: u32) {
        self.selection.set_selected(position);
    }

    /// Runs `handler` when the user's selection changes; `set` reports its own once.
    pub(super) fn connect_selected(&self, handler: impl Fn() + 'static) {
        let quiet = self.quiet.clone();
        self.selection.connect_selection_changed(move |_, _, _| {
            if !quiet.get() {
                handler();
            }
        });
    }

    /// The groups shown, for the self-test.
    #[cfg(feature = "test-hooks")]
    pub(super) fn rows(&self) -> Vec<SenderGroup> {
        (0..self.items.n_items())
            .filter_map(|position| self.items.item(position))
            .filter_map(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<SenderGroup>().clone())
            .collect()
    }

    /// Whether the shown row of the group `key` shows its pin; `None` when no row shows it.
    #[cfg(feature = "test-hooks")]
    pub(super) fn shows_pin(&self, key: &str) -> Option<bool> {
        let mut stack = vec![self.view.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if let Some(row) = widget.downcast_ref::<gtk::Box>()
                && row.has_css_class("sw-sender-group")
            {
                if let Some((shown, pin)) = shown(row)
                    && shown == key
                {
                    return Some(pin);
                }
                continue;
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                child = next.next_sibling();
                stack.push(next);
            }
        }
        None
    }

    /// Opens the group at `position`, as Enter on it does.
    #[cfg(feature = "test-hooks")]
    pub(super) fn activate(&self, position: u32) {
        self.view.emit_by_name::<()>("activate", &[&position]);
    }

    /// Runs `handler` with a group's key when it is opened.
    pub(super) fn connect_open(&self, handler: impl Fn(String) + 'static) {
        let items = self.items.clone();
        self.view.connect_activate(move |_, position| {
            if let Some(item) = items.item(position).and_downcast::<glib::BoxedAnyObject>() {
                let key = item.borrow::<SenderGroup>().key.clone();
                handler(key);
            }
        });
    }
}

fn row() -> gtk::Box {
    let name = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["sw-row-sender"])
        .build();
    let count = gtk::Label::builder()
        .xalign(1.0)
        .css_classes(["sw-small", "sw-secondary"])
        .build();
    let pin = gtk::Image::builder()
        .icon_name("view-pin-symbolic")
        .tooltip_text("Pinned")
        .visible(false)
        .build();
    pin.add_css_class("sw-pin");
    pin.add_css_class("sw-indicator");
    let top = gtk::Box::builder().spacing(8).build();
    top.append(&name);
    top.append(&pin);
    top.append(&count);
    let detail = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["sw-small", "sw-secondary"])
        .build();
    let subject = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .css_classes(["sw-small"])
        .build();
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    row.add_css_class("sw-sender-group");
    row.append(&top);
    row.append(&detail);
    row.append(&subject);
    row
}

fn bind(list_item: &gtk::ListItem, row: &gtk::Box, group: &SenderGroup) {
    let labels: Vec<gtk::Label> = {
        let top = row.first_child().and_downcast::<gtk::Box>();
        let mut labels: Vec<gtk::Label> = top
            .iter()
            .flat_map(|top| [top.first_child(), top.last_child()])
            .flatten()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .collect();
        let mut next = row.first_child().and_then(|top| top.next_sibling());
        while let Some(widget) = next {
            next = widget.next_sibling();
            if let Ok(label) = widget.downcast::<gtk::Label>() {
                labels.push(label);
            }
        }
        labels
    };
    let [name, count, detail, subject] = labels.as_slice() else {
        return;
    };
    if let Some(pin) = pin_of(row) {
        pin.set_visible(group.pinned);
    }
    let count_text = count_text(group.count, group.unread);
    name.set_text(&group.name);
    let attributes = pango::AttrList::new();
    if group.unread > 0 {
        attributes.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
    }
    name.set_attributes(Some(&attributes));
    count.set_text(&count_text);
    detail.set_text(&group.key);
    detail.set_visible(!group.key.eq_ignore_ascii_case(&group.name));
    subject.set_text(&group.subject);
    let pinned = if group.pinned { ", pinned" } else { "" };
    list_item.set_accessible_label(&format!(
        "{}, {}, {}{pinned}",
        group.name, group.key, count_text
    ));
}

/// The pin of a group's row.
fn pin_of(row: &gtk::Box) -> Option<gtk::Image> {
    row.first_child()?
        .first_child()?
        .next_sibling()
        .and_downcast::<gtk::Image>()
}

/// The key a group's row shows, and whether its pin shows.
#[cfg(feature = "test-hooks")]
fn shown(row: &gtk::Box) -> Option<(String, bool)> {
    let detail = row
        .first_child()?
        .next_sibling()
        .and_downcast::<gtk::Label>()?;
    Some((detail.text().to_string(), pin_of(row)?.is_visible()))
}

/// "12 messages, 3 unread".
pub(super) fn count_text(count: u32, unread: u32) -> String {
    let messages = if count == 1 {
        "1 message".to_owned()
    } else {
        format!("{count} messages")
    };
    if unread == 0 {
        messages
    } else {
        format!("{messages}, {unread} unread")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        assert_eq!(count_text(1, 0), "1 message");
        assert_eq!(count_text(12, 3), "12 messages, 3 unread");
    }
}
