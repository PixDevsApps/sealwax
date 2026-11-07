//! The message list grouped by sender or by domain (issue #24, ADR-023): one row per sender, or
//! per domain, newest first, with its name, how many messages and how many unread, the newest
//! message's time and its subject. Opening a row (Enter or a click) shows its mail; the list does
//! that with a `sender:` or `domain:` search, so Escape comes back here. A selected row takes the
//! mail commands, which act on all of its mail in the view (issue #25). A pinned group shows a
//! pin, and in the Inboxes the store puts it first (issue #26). The rows are in the plain list's
//! sections (issue #26): Pinned for the groups the store put first, then Today, Yesterday, This
//! week, This month and Older by each group's newest message; they regroup when the day changes.

use super::place;
use super::row::time_text;
use super::sections::{Bounds, LABELS, PINNED, Thresholds};
use super::splice::{runs, splice};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gio, glib, pango};
use sealwax_infrastructure::store::SenderGroup;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The day the sections and times are for: its section starts, and its year.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Day {
    pub thresholds: Thresholds,
    pub year: i32,
}

impl Day {
    fn unknown() -> Self {
        Self {
            thresholds: Thresholds::new(0, 0, 0, 0),
            year: 0,
        }
    }
}

/// The section of `group` on `day`: Pinned when the store put it first, else its newest
/// message's date section.
fn section_of(group: &SenderGroup, thresholds: &Thresholds) -> usize {
    if group.leads {
        PINNED
    } else {
        thresholds.section_of(group.latest)
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GroupModel {
        pub(super) items: RefCell<Vec<glib::BoxedAnyObject>>,
        pub(super) bounds: Cell<Bounds>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GroupModel {
        const NAME: &'static str = "SealwaxGroupModel";
        type Type = super::GroupModel;
        type Interfaces = (gio::ListModel, gtk::SectionModel);
    }

    impl ObjectImpl for GroupModel {}

    impl ListModelImpl for GroupModel {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.items.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.items
                .borrow()
                .get(position as usize)
                .map(|item| item.clone().upcast())
        }
    }

    impl SectionModelImpl for GroupModel {
        fn section(&self, position: u32) -> (u32, u32) {
            let (_, start, end) = self.bounds.get().section_at(position);
            (start, end)
        }
    }
}

glib::wrapper! {
    /// The groups in their sections.
    pub struct GroupModel(ObjectSubclass<imp::GroupModel>)
        @implements gio::ListModel, gtk::SectionModel;
}

impl GroupModel {
    fn new() -> Self {
        glib::Object::new()
    }

    /// Shows `groups` in the sections of `thresholds`, changing only the rows that changed
    /// (issue #29): a group that stays in its section keeps its row, so the view keeps its
    /// place, its selection and its keyboard focus. Returns the rows that stayed with new
    /// contents, to bind again.
    fn update(
        &self,
        groups: Vec<SenderGroup>,
        thresholds: &Thresholds,
    ) -> Vec<glib::BoxedAnyObject> {
        let imp = self.imp();
        let (old_keys, old_sections): (Vec<String>, Vec<usize>) = {
            let items = imp.items.borrow();
            let bounds = imp.bounds.get();
            items
                .iter()
                .enumerate()
                .map(|(position, item)| {
                    (
                        item.borrow::<SenderGroup>().key.clone(),
                        bounds.section_at(position as u32).0,
                    )
                })
                .unzip()
        };
        let new_sections: Vec<usize> = groups
            .iter()
            .map(|group| section_of(group, thresholds))
            .collect();
        let new_keys: Vec<&str> = groups.iter().map(|group| group.key.as_str()).collect();
        let old_keys: Vec<&str> = old_keys.iter().map(String::as_str).collect();
        let plan = splice(&old_keys, &new_keys, |from, to| {
            old_sections[from] == new_sections[to]
        });
        let mut changed = Vec::new();
        {
            let items = imp.items.borrow();
            for &(from, to) in &plan.kept {
                let item = &items[from as usize];
                let group = &groups[to as usize];
                if *item.borrow::<SenderGroup>() != *group {
                    *item.borrow_mut::<SenderGroup>() = group.clone();
                    changed.push(item.clone());
                }
            }
        }
        for (first, count) in runs(&plan.removed) {
            {
                let mut items = imp.items.borrow_mut();
                let mut bounds = imp.bounds.get();
                for position in (first..first + count).rev() {
                    bounds.remove(bounds.section_at(position).0);
                    items.remove(position as usize);
                }
                imp.bounds.set(bounds);
            }
            self.items_changed(first, count, 0);
        }
        let mut fresh: Vec<Option<SenderGroup>> = groups.into_iter().map(Some).collect();
        for (first, count) in runs(&plan.inserted) {
            let added = {
                let mut items = imp.items.borrow_mut();
                let mut bounds = imp.bounds.get();
                let mut added = 0;
                for position in first..first + count {
                    if let Some(group) = fresh[position as usize].take() {
                        bounds.insert(new_sections[position as usize]);
                        items.insert(position as usize, glib::BoxedAnyObject::new(group));
                        added += 1;
                    }
                }
                imp.bounds.set(bounds);
                added
            };
            self.items_changed(first, 0, added);
        }
        let wanted = Bounds::from_sections(new_sections);
        if imp.bounds.get() != wanted {
            tracing::warn!("the groups' sections differ after an update; setting them again");
            imp.bounds.set(wanted);
            self.sections_changed(0, self.n_items());
        }
        changed
    }

    /// The section of the row at `position`.
    fn section_at(&self, position: u32) -> usize {
        self.imp().bounds.get().section_at(position).0
    }

    #[cfg(feature = "test-hooks")]
    fn bounds(&self) -> Bounds {
        self.imp().bounds.get()
    }
}

pub(super) struct Groups {
    scroller: gtk::ScrolledWindow,
    view: gtk::ListView,
    items: GroupModel,
    selection: gtk::SingleSelection,
    /// Set while `set` changes the rows, whose passing selections aren't the user's.
    quiet: Rc<Cell<bool>>,
    day: Rc<Cell<Day>>,
    /// The list items bound to a group now, to bind again when a group's contents change.
    bound: Rc<RefCell<Vec<gtk::ListItem>>>,
}

impl Groups {
    pub(super) fn new() -> Self {
        let items = GroupModel::new();
        let day = Rc::new(Cell::new(Day::unknown()));
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
        let bind_day = day.clone();
        let bound: Rc<RefCell<Vec<gtk::ListItem>>> = Rc::default();
        let bind_bound = bound.clone();
        factory.connect_bind(move |_, object| {
            let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            bind_bound.borrow_mut().push(list_item.clone());
            if let (Some(row), Some(item)) = (
                list_item.child().and_downcast::<gtk::Box>(),
                list_item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) {
                bind(
                    list_item,
                    &row,
                    &item.borrow::<SenderGroup>(),
                    bind_day.get(),
                );
            }
        });
        let unbind_bound = bound.clone();
        factory.connect_unbind(move |_, object| {
            unbind_bound
                .borrow_mut()
                .retain(|list_item| list_item.upcast_ref::<glib::Object>() != object);
        });
        let headers = gtk::SignalListItemFactory::new();
        headers.connect_setup(|_, object| {
            if let Some(header) = object.downcast_ref::<gtk::ListHeader>() {
                let label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
                label.add_css_class("sw-section-header");
                header.set_child(Some(&label));
            }
        });
        let header_items = items.downgrade();
        headers.connect_bind(move |_, object| {
            let Some(header) = object.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            if let (Some(label), Some(items)) = (
                header.child().and_downcast::<gtk::Label>(),
                header_items.upgrade(),
            ) {
                label.set_text(&LABELS[items.section_at(header.start())].to_uppercase());
            }
        });
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .header_factory(&headers)
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
            day,
            bound,
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

    /// Shows `groups` in their sections on `day`, selecting the row of the first of `wanted`
    /// (groups' keys) that is there. Only the rows that changed change, and the selected row
    /// keeps its place in the view, or a row taking the place of a selected one that left takes
    /// its place too (issue #29).
    pub(super) fn set(&self, groups: Vec<SenderGroup>, wanted: &[&str], day: Day) {
        let new_day = self.day.replace(day) != day;
        let keep = wanted
            .iter()
            .find_map(|key| groups.iter().position(|group| group.key == *key));
        let before = self.selected().map(|group| group.key);
        let top = before
            .as_deref()
            .and_then(|key| top_of(&self.bound, &self.scroller, key));
        self.quiet.set(true);
        let changed = self.items.update(groups, &day.thresholds);
        self.bind_again(|item| new_day || changed.contains(item));
        match keep {
            Some(position) => {
                let position = position as u32;
                if self.selection.selected() != position {
                    self.selection.set_selected(position);
                }
                self.hold(position, top, before.as_deref());
            }
            None => self.selection.set_selected(gtk::INVALID_LIST_POSITION),
        }
        self.quiet.set(false);
    }

    /// Binds again the rows showing a group that `again` names, after its contents changed.
    fn bind_again(&self, again: impl Fn(&glib::BoxedAnyObject) -> bool) {
        let day = self.day.get();
        let bound = self.bound.borrow().clone();
        for list_item in bound {
            if let (Some(row), Some(item)) = (
                list_item.child().and_downcast::<gtk::Box>(),
                list_item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) && again(&item)
            {
                bind(&list_item, &row, &item.borrow::<SenderGroup>(), day);
            }
        }
    }

    /// Holds the selected row at `position` at `top` through the change; scrolls to it only
    /// when it is another group than `before`, the one selected before.
    fn hold(&self, position: u32, top: Option<f32>, before: Option<&str>) {
        let Some(key) = self.selected().map(|group| group.key) else {
            return;
        };
        let reveal = before != Some(key.as_str());
        let (bound, scroller) = (self.bound.clone(), self.scroller.clone());
        let row_key = key.clone();
        let selection = self.selection.clone();
        place::hold(
            place::Hold {
                view: self.view.clone(),
                scroller: self.scroller.clone(),
                position,
                top,
                reveal,
            },
            move || top_of(&bound, &scroller, &row_key),
            move || {
                selection.selected() == position
                    && selection
                        .selected_item()
                        .and_downcast::<glib::BoxedAnyObject>()
                        .is_some_and(|item| item.borrow::<SenderGroup>().key == key)
            },
        );
    }

    /// The day changed: the same groups in that day's sections, with its times.
    pub(super) fn set_day(&self, day: Day) {
        if self.day.get() == day {
            return;
        }
        let selected = self.selected().map(|group| group.key);
        let wanted: Vec<&str> = selected.as_deref().into_iter().collect();
        self.set(self.groups(), &wanted, day);
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

    /// The groups shown.
    fn groups(&self) -> Vec<SenderGroup> {
        (0..self.items.n_items())
            .filter_map(|position| self.items.item(position))
            .filter_map(|item| item.downcast::<glib::BoxedAnyObject>().ok())
            .map(|item| item.borrow::<SenderGroup>().clone())
            .collect()
    }

    /// The groups shown, for the self-test.
    #[cfg(feature = "test-hooks")]
    pub(super) fn rows(&self) -> Vec<SenderGroup> {
        self.groups()
    }

    /// Groups in each section (an index into `sections::LABELS`), for the self-test.
    #[cfg(feature = "test-hooks")]
    pub(super) fn section_counts(&self) -> [u32; super::sections::SECTIONS] {
        self.items.bounds().counts()
    }

    /// The section headers on screen, top to bottom (the self-test).
    #[cfg(feature = "test-hooks")]
    pub(super) fn headers(&self) -> Vec<String> {
        let height = self.scroller.height() as f32;
        let mut headers: Vec<(f32, String)> = self
            .widgets()
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .filter(|label| label.has_css_class("sw-section-header") && label.is_mapped())
            .filter_map(|label| {
                let bounds = label.compute_bounds(&self.scroller)?;
                let visible = bounds.y() >= 0.0 && bounds.y() + bounds.height() <= height;
                visible.then(|| (bounds.y(), label.text().to_string()))
            })
            .collect();
        headers.sort_by(|a, b| a.0.total_cmp(&b.0));
        headers.into_iter().map(|(_, text)| text).collect()
    }

    /// What the shown row of the group `key` shows: whether its pin shows, and its time; `None`
    /// when no row shows it.
    #[cfg(feature = "test-hooks")]
    pub(super) fn shown(&self, key: &str) -> Option<(bool, String)> {
        self.widgets()
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Box>().ok())
            .filter(|row| row.has_css_class("sw-sender-group"))
            .filter_map(|row| parts(&row))
            .find(|parts| parts.detail.text() == key)
            .map(|parts| (parts.pin.is_visible(), parts.time.text().to_string()))
    }

    /// How far below the top of the view the selected group's row is, for the self-test.
    #[cfg(feature = "test-hooks")]
    pub(super) fn selected_top(&self) -> Option<f32> {
        top_of(&self.bound, &self.scroller, &self.selected()?.key)
    }

    /// The position of the group whose row is nearest the middle of the view, for the self-test.
    #[cfg(feature = "test-hooks")]
    pub(super) fn middle_position(&self) -> Option<u32> {
        let middle = self.scroller.height() as f32 / 2.0;
        let keys: Vec<String> = self.groups().into_iter().map(|group| group.key).collect();
        self.widgets()
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Box>().ok())
            .filter(|row| row.has_css_class("sw-sender-group") && row.is_mapped())
            .filter_map(|row| {
                let bounds = row.compute_bounds(&self.scroller)?;
                let key = parts(&row)?.detail.text();
                let position = keys.iter().position(|shown| *shown == key)?;
                Some((
                    (bounds.y() + bounds.height() / 2.0 - middle).abs(),
                    position,
                ))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, position)| position as u32)
    }

    /// Every widget under the list.
    #[cfg(feature = "test-hooks")]
    fn widgets(&self) -> Vec<gtk::Widget> {
        let mut found = Vec::new();
        let mut stack = vec![self.view.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            let mut child = widget.first_child();
            while let Some(next) = child {
                child = next.next_sibling();
                stack.push(next);
            }
            found.push(widget);
        }
        found
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
    let time = gtk::Label::builder()
        .xalign(1.0)
        .css_classes(["sw-time"])
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
    top.append(&time);
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

/// How far down `scroller`'s view the row of the group `key` is, among the `bound` rows; `None`
/// when no row on screen shows it.
fn top_of(
    bound: &RefCell<Vec<gtk::ListItem>>,
    scroller: &gtk::ScrolledWindow,
    key: &str,
) -> Option<f32> {
    bound
        .borrow()
        .iter()
        .filter(|list_item| {
            list_item
                .item()
                .and_downcast::<glib::BoxedAnyObject>()
                .is_some_and(|item| item.borrow::<SenderGroup>().key == key)
        })
        .filter_map(|list_item| list_item.child())
        .filter(|row| row.is_mapped())
        .find_map(|row| row.compute_bounds(scroller))
        .map(|bounds| bounds.y())
}

/// A group row's widgets, as `row` makes them.
struct Parts {
    name: gtk::Label,
    pin: gtk::Image,
    count: gtk::Label,
    time: gtk::Label,
    detail: gtk::Label,
    subject: gtk::Label,
}

fn parts(row: &gtk::Box) -> Option<Parts> {
    let top = row.first_child()?;
    let name = top.first_child()?;
    let pin = name.next_sibling()?;
    let count = pin.next_sibling()?;
    let time = count.next_sibling()?;
    let detail = top.next_sibling()?;
    let subject = detail.next_sibling()?;
    Some(Parts {
        name: name.downcast().ok()?,
        pin: pin.downcast().ok()?,
        count: count.downcast().ok()?,
        time: time.downcast().ok()?,
        detail: detail.downcast().ok()?,
        subject: subject.downcast().ok()?,
    })
}

fn bind(list_item: &gtk::ListItem, row: &gtk::Box, group: &SenderGroup, day: Day) {
    let Some(parts) = parts(row) else {
        return;
    };
    parts.pin.set_visible(group.pinned);
    let count_text = count_text(group.count, group.unread);
    parts.name.set_text(&group.name);
    let attributes = pango::AttrList::new();
    if group.unread > 0 {
        attributes.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
    }
    parts.name.set_attributes(Some(&attributes));
    parts.count.set_text(&count_text);
    // As the plain list's Pinned rows: the time as the message's date section shows it.
    let time = time_text(
        group.latest,
        day.thresholds.section_of(group.latest),
        day.year,
    );
    parts.time.set_text(&time);
    parts.detail.set_text(&group.key);
    parts
        .detail
        .set_visible(!group.key.eq_ignore_ascii_case(&group.name));
    parts.subject.set_text(&group.subject);
    list_item.set_accessible_label(&accessible_text(group, &count_text, &time));
}

/// "Anna Berg, anna@example.test, 2 messages, 1 unread, newest 10:32, pinned".
fn accessible_text(group: &SenderGroup, count_text: &str, time: &str) -> String {
    let pinned = if group.pinned { ", pinned" } else { "" };
    format!(
        "{}, {}, {count_text}, newest {time}{pinned}",
        group.name, group.key
    )
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

    fn group(latest: i64, leads: bool) -> SenderGroup {
        SenderGroup {
            key: "anna@example.test".into(),
            name: "Anna Berg".into(),
            count: 2,
            unread: 1,
            latest,
            subject: "Middag på fredag?".into(),
            pinned: leads,
            leads,
        }
    }

    #[test]
    fn a_group_goes_under_its_newest_messages_section_or_pinned_when_it_leads() {
        const DAY: i64 = 86_400;
        let today = 100 * DAY;
        let thresholds = Thresholds::new(today, today - DAY, today - 2 * DAY, today - 9 * DAY);
        assert_eq!(section_of(&group(today + 60, false), &thresholds), 1);
        assert_eq!(section_of(&group(today - 60, false), &thresholds), 2);
        assert_eq!(
            section_of(&group(today - 40 * DAY, false), &thresholds),
            super::super::sections::OLDER
        );
        assert_eq!(
            section_of(&group(today - 40 * DAY, true), &thresholds),
            PINNED
        );
    }

    #[test]
    fn a_group_row_says_its_newest_time() {
        assert_eq!(
            accessible_text(&group(0, true), "2 messages, 1 unread", "10:32"),
            "Anna Berg, anna@example.test, 2 messages, 1 unread, newest 10:32, pinned"
        );
    }
}
