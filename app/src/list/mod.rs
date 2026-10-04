//! The message list (ML-026, DESIGN.md "Message list"): a search field and the filter chips
//! (All, Unread, Starred, Attachments) over a `gtk::ListView` of the store's view of a folder or
//! the unified inbox, in date sections, or of search results.
//!
//! - The model ([`model`]) pages lazily and applies the store's change events in version order.
//! - New mail keeps the list at its top while the first row is in view (S3).
//! - Rows show their placeholder look while the list moves faster than ~30,000 px/s (S3).
//! - After every reset the list shows [`FIRST_ROWS`] rows, then grows by [`GROW_STEP`] a frame
//!   (S6's first-frame trim).
//! - Sections regroup when the day changes.
//! - One message is selected at a time (batch selection is M4); the List scope's keys move the
//!   selection (`j`, `k`; GtkListView's Down and Up) and open it (Enter, `o`).

pub mod item;
pub mod model;
pub mod row;
pub mod sections;

pub use model::{Change, Content, FIRST_ROWS, GROW_STEP, MessageModel, ModelStats, SEARCH_PAGE};

use crate::actions::{self, ScopedShortcuts};
use crate::mailbox::{Latch, Mailbox};
use crate::window::empty;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use item::MessageItem;
use row::{MessageRow, RowContext, SectionHeader};
use sealwax_domain::actions::Scope;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{FolderId, MessageId};
use sealwax_infrastructure::store::{ChangeEvent, ViewFilter, ViewScope, ViewSpec};
use sections::{Thresholds, until_next_day};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

/// Faster than this (px/s), rows skip their text (S3: a scrollbar drag through 100k rows then
/// holds 144 Hz).
pub const SKELETON_SPEED: f64 = 30_000.0;
/// Frames slower than the skeleton speed before rows show their text again.
const SLOW_FRAMES: u32 = 2;
/// Frames without movement before the speed watch stops.
const STILL_FRAMES: u32 = 8;
/// Within this many pixels of the top, new mail keeps the list at its top (S3).
const PIN_SLACK: f64 = 48.0;
/// The day is checked at least this often, so sections regroup soon after a suspend over
/// midnight too.
const DAY_CHECK: Duration = Duration::from_secs(600);

pub const FILTERS: [(ViewFilter, &str); 4] = [
    (ViewFilter::All, "All"),
    (ViewFilter::Unread, "Unread"),
    (ViewFilter::Starred, "Starred"),
    (ViewFilter::Attachments, "Attachments"),
];

pub fn filter_name(filter: ViewFilter) -> &'static str {
    FILTERS
        .iter()
        .find(|(candidate, _)| *candidate == filter)
        .map_or("All", |(_, name)| name)
}

/// The list's title while it shows search results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHeading {
    pub results: u32,
    /// More results can be fetched.
    pub more: bool,
    pub searching: bool,
}

impl SearchHeading {
    pub fn subtitle(&self) -> String {
        match (self.searching && self.results == 0, self.results, self.more) {
            (true, _, _) => "Searching…".to_owned(),
            (_, 0, _) => "No results".to_owned(),
            (_, 1, false) => "1 result".to_owned(),
            (_, count, false) => format!("{count} results"),
            (_, count, true) => format!("{count}+ results"),
        }
    }
}

/// A bound row as the self-test and the perf probe see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundRow {
    pub position: Option<u32>,
    pub id: Option<MessageId>,
    pub placeholder: bool,
}

type IdHandler = Rc<dyn Fn(MessageId)>;
type Handler = Rc<dyn Fn()>;
type SearchHandler = Rc<dyn Fn(Option<&SearchHeading>)>;

#[derive(Default)]
struct Handlers {
    selected: RefCell<Vec<IdHandler>>,
    opened: RefCell<Vec<IdHandler>>,
    cleared: RefCell<Vec<Handler>>,
    search: RefCell<Vec<SearchHandler>>,
}

#[derive(Clone)]
pub struct MessageList {
    inner: Rc<Inner>,
}

struct Inner {
    mailbox: Rc<Mailbox>,
    root: gtk::Box,
    search: gtk::SearchEntry,
    chips: Vec<(ViewFilter, gtk::ToggleButton)>,
    stack: gtk::Stack,
    empty: empty::EmptyState,
    scroller: gtk::ScrolledWindow,
    view: gtk::ListView,
    model: MessageModel,
    selection: gtk::SingleSelection,
    context: Rc<RowContext>,
    scope: Cell<Option<ViewScope>>,
    filter: Cell<ViewFilter>,
    query: RefCell<String>,
    /// Typing into the search field while the list changes it itself.
    quiet: Cell<bool>,
    /// A fixed "now" (Unix seconds) for the self-test; the real clock otherwise.
    clock: Cell<Option<i64>>,
    content: RefCell<Content>,
    growth: RefCell<Option<gtk::TickCallbackId>>,
    growth_frames: RefCell<Vec<u32>>,
    speed: RefCell<Option<gtk::TickCallbackId>>,
    /// The message the list last reported as selected.
    selected: Cell<Option<MessageId>>,
    /// A message to select again once its row loads, after a reload.
    wanted: Cell<Option<MessageId>>,
    /// Where the list was just before its last reset, and whether it had the keyboard focus.
    place: Cell<f64>,
    refocus: Cell<bool>,
    settle_queued: Cell<bool>,
    waiting: RefCell<Option<(MessageItem, glib::SignalHandlerId)>>,
    sweep_queued: Cell<bool>,
    handlers: Handlers,
    keys: RefCell<Option<ScopedShortcuts>>,
    first_rows: RefCell<Option<Rc<Latch>>>,
    day_check: RefCell<Option<glib::SourceId>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(source) = self.day_check.take() {
            source.remove();
        }
    }
}

impl MessageList {
    pub fn new(mailbox: Rc<Mailbox>) -> Self {
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search mail")
            .hexpand(true)
            .build();
        search.add_css_class("sw-search");
        search.update_property(&[gtk::accessible::Property::Label("Search mail")]);
        let chip_row = gtk::Box::builder().spacing(6).build();
        chip_row.add_css_class("sw-chips");
        let mut chips: Vec<(ViewFilter, gtk::ToggleButton)> = Vec::new();
        for (filter, name) in FILTERS {
            let chip = gtk::ToggleButton::builder()
                .label(name)
                .active(filter == ViewFilter::All)
                .build();
            chip.add_css_class("sw-chip");
            chip.set_widget_name(&format!("chip-{}", name.to_lowercase()));
            if let Some((_, first)) = chips.first() {
                chip.set_group(Some(first));
            }
            chip_row.append(&chip);
            chips.push((filter, chip));
        }
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        head.add_css_class("sw-list-head");
        head.append(&search);
        head.append(&chip_row);

        let model = MessageModel::new(mailbox.clone());
        let selection = gtk::SingleSelection::builder()
            .model(&model)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let context = RowContext::new();
        let factory = gtk::SignalListItemFactory::new();
        let row_context = context.clone();
        factory.connect_setup(move |_, object| {
            if let Some(list_item) = object.downcast_ref::<gtk::ListItem>() {
                list_item.set_child(Some(&MessageRow::new(row_context.clone())));
            }
        });
        factory.connect_bind(|_, object| {
            let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            if let (Some(row), Some(item)) = (
                list_item.child().and_downcast::<MessageRow>(),
                list_item.item().and_downcast::<MessageItem>(),
            ) {
                row.bind(list_item, &item);
            }
        });
        let unbind = |_: &gtk::SignalListItemFactory, object: &glib::Object| {
            if let Some(row) = object
                .downcast_ref::<gtk::ListItem>()
                .and_then(|list_item| list_item.child().and_downcast::<MessageRow>())
            {
                row.unbind();
            }
        };
        factory.connect_unbind(unbind);
        factory.connect_teardown(unbind);
        let headers = gtk::SignalListItemFactory::new();
        headers.connect_setup(|_, object| {
            if let Some(header) = object.downcast_ref::<gtk::ListHeader>() {
                header.set_child(Some(&SectionHeader::new()));
            }
        });
        let header_context = context.clone();
        headers.connect_bind(move |_, object| {
            let Some(header) = object.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            if let (Some(widget), Some(item)) = (
                header.child().and_downcast::<SectionHeader>(),
                header.item().and_downcast::<MessageItem>(),
            ) {
                widget.bind(&item, &header_context);
            }
        });
        let header_context = context.clone();
        headers.connect_unbind(move |_, object| {
            if let Some(widget) = object
                .downcast_ref::<gtk::ListHeader>()
                .and_then(|header| header.child().and_downcast::<SectionHeader>())
            {
                widget.unbind(&header_context);
            }
        });
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .header_factory(&headers)
            .build();
        view.add_css_class("sw-message-list");
        view.set_widget_name("message-list");
        view.update_property(&[gtk::accessible::Property::Label("Messages")]);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let empty = empty::folder("No messages", "Messages in this folder appear here.");
        empty.widget().set_widget_name("list-empty");
        let stack = gtk::Stack::builder().vexpand(true).build();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(empty.widget(), Some("empty"));
        stack.set_visible_child_name("list");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("sw-message-list");
        root.add_css_class("sw-message-list-pane");
        root.set_widget_name("message-list-pane");
        root.append(&head);
        root.append(&stack);

        let list = Self {
            inner: Rc::new(Inner {
                mailbox,
                root,
                search,
                chips,
                stack,
                empty,
                scroller,
                view,
                model,
                selection,
                context,
                scope: Cell::new(None),
                filter: Cell::new(ViewFilter::All),
                query: RefCell::new(String::new()),
                quiet: Cell::new(false),
                clock: Cell::new(None),
                content: RefCell::new(Content::Nothing),
                growth: RefCell::new(None),
                growth_frames: RefCell::new(Vec::new()),
                speed: RefCell::new(None),
                selected: Cell::new(None),
                wanted: Cell::new(None),
                place: Cell::new(0.0),
                refocus: Cell::new(false),
                settle_queued: Cell::new(false),
                waiting: RefCell::new(None),
                sweep_queued: Cell::new(false),
                handlers: Handlers::default(),
                keys: RefCell::new(None),
                first_rows: RefCell::new(None),
                day_check: RefCell::new(None),
            }),
        };
        list.connect();
        list.schedule_day_check();
        list
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    fn with(weak: &Weak<Inner>, f: impl FnOnce(MessageList)) {
        if let Some(inner) = weak.upgrade() {
            f(MessageList { inner });
        }
    }

    fn connect(&self) {
        let inner = &self.inner;
        let weak = self.downgrade();
        inner.model.set_listener(move |change| {
            Self::with(&weak, |list| list.model_changed(change));
        });
        let weak = self.downgrade();
        inner.selection.connect_selected_notify(move |_| {
            Self::with(&weak, |list| list.queue_settle());
        });
        let weak = self.downgrade();
        inner.view.connect_activate(move |_, position| {
            Self::with(&weak, |list| {
                list.inner.selection.set_selected(position);
                list.settle();
                list.open_selected();
            });
        });
        let weak = self.downgrade();
        inner.search.connect_search_changed(move |entry| {
            let text = entry.text().trim().to_owned();
            Self::with(&weak, |list| {
                if !list.inner.quiet.get() {
                    list.set_query(text);
                }
            });
        });
        let weak = self.downgrade();
        inner.search.connect_stop_search(move |_| {
            Self::with(&weak, |list| {
                list.clear_search();
                list.inner.view.grab_focus();
            });
        });
        let weak = self.downgrade();
        inner.search.connect_activate(move |_| {
            Self::with(&weak, |list| {
                if list.inner.selection.selected() == gtk::INVALID_LIST_POSITION
                    && list.inner.model.total() > 0
                {
                    list.select_position(0);
                }
                list.inner.view.grab_focus();
            });
        });
        for (filter, chip) in &inner.chips {
            let filter = *filter;
            let weak = self.downgrade();
            chip.connect_toggled(move |chip| {
                if chip.is_active() {
                    Self::with(&weak, |list| list.set_filter(filter));
                }
            });
        }
        let weak = self.downgrade();
        inner
            .scroller
            .vadjustment()
            .connect_value_changed(move |_| Self::with(&weak, |list| list.scrolled()));
        let weak = self.downgrade();
        inner
            .scroller
            .hadjustment()
            .connect_page_size_notify(move |adjustment| {
                let width = adjustment.page_size();
                Self::with(&weak, |list| list.width_changed(width));
            });
        let weak = self.downgrade();
        inner.mailbox.connect_event(move |event| {
            Self::with(&weak, |list| list.on_event(event));
        });
    }

    /// The list pane's content: the search field, the chips and the list.
    pub fn widget(&self) -> &gtk::Box {
        &self.inner.root
    }

    pub fn list_view(&self) -> &gtk::ListView {
        &self.inner.view
    }

    pub fn search_entry(&self) -> &gtk::SearchEntry {
        &self.inner.search
    }

    pub fn scroller(&self) -> &gtk::ScrolledWindow {
        &self.inner.scroller
    }

    pub fn model(&self) -> &MessageModel {
        &self.inner.model
    }

    pub fn selection(&self) -> &gtk::SingleSelection {
        &self.inner.selection
    }

    pub fn chip(&self, filter: ViewFilter) -> Option<&gtk::ToggleButton> {
        self.inner
            .chips
            .iter()
            .find(|(candidate, _)| *candidate == filter)
            .map(|(_, chip)| chip)
    }

    pub fn empty_state(&self) -> Option<(String, String)> {
        (self.inner.stack.visible_child_name().as_deref() == Some("empty")).then(|| {
            (
                self.inner.empty.title().to_string(),
                self.inner
                    .empty
                    .description()
                    .map(|text| text.to_string())
                    .unwrap_or_default(),
            )
        })
    }

    /// Opens `latch` once the list has the first rows of what it shows (the first window waits
    /// for it, S6).
    pub fn set_first_rows(&self, latch: Rc<Latch>) {
        self.inner.first_rows.replace(Some(latch));
    }

    /// Shows a folder or the unified inbox (`None`: nothing). A search in progress ends.
    pub fn show(&self, scope: Option<ViewScope>) {
        self.inner.scope.set(scope);
        if !self.inner.query.borrow().is_empty() {
            self.inner.query.borrow_mut().clear();
            self.inner.quiet.set(true);
            self.inner.search.set_text("");
            self.inner.quiet.set(false);
            self.emit_search();
        }
        self.reload();
    }

    pub fn scope(&self) -> Option<ViewScope> {
        self.inner.scope.get()
    }

    pub fn filter(&self) -> ViewFilter {
        self.inner.filter.get()
    }

    /// Filters by a chip, as clicking it does.
    pub fn set_filter(&self, filter: ViewFilter) {
        if let Some(chip) = self.chip(filter)
            && !chip.is_active()
        {
            chip.set_active(true);
            return;
        }
        if self.inner.filter.replace(filter) != filter {
            self.reload();
        }
    }

    pub fn query(&self) -> String {
        self.inner.query.borrow().clone()
    }

    /// Searches for `text` in the folder shown (every folder from the unified inbox); empty
    /// text shows the folder again.
    pub fn set_query(&self, text: String) {
        if *self.inner.query.borrow() == text {
            return;
        }
        if self.inner.search.text().trim() != text {
            self.inner.quiet.set(true);
            self.inner.search.set_text(&text);
            self.inner.quiet.set(false);
        }
        self.inner.query.replace(text);
        self.reload();
    }

    /// Ends the search, as Escape in the search field does.
    pub fn clear_search(&self) {
        self.set_query(String::new());
    }

    /// Puts the keyboard focus in the search field (Search: `/`, Ctrl+F).
    pub fn focus_search(&self) {
        self.inner.search.grab_focus();
    }

    fn reload(&self) {
        let inner = &self.inner;
        let thresholds = self.thresholds_now();
        let query = inner.query.borrow().clone();
        match inner.scope.get() {
            None => inner.model.clear(),
            Some(scope) if query.is_empty() => {
                let spec = ViewSpec {
                    scope,
                    filter: inner.filter.get(),
                };
                match inner.mailbox.take_first_view(spec, thresholds) {
                    Some(opened) => inner.model.adopt(spec, thresholds, opened),
                    None => inner.model.open(spec, thresholds),
                }
            }
            Some(scope) => {
                let folder = match scope {
                    ViewScope::Folder(folder) => Some(folder),
                    ViewScope::UnifiedInbox => None,
                };
                inner
                    .model
                    .search(query, folder, inner.filter.get(), thresholds);
            }
        }
        self.emit_search();
    }

    fn model_changed(&self, change: Change) {
        match change {
            Change::Resetting => {
                self.inner
                    .place
                    .set(self.inner.scroller.vadjustment().value());
                // A reload of the same view destroys the row that has the keyboard focus, and
                // GTK would then scroll its next focus row, the first, into view: the focus
                // leaves the list until the rows are back at their place.
                let reload = *self.inner.content.borrow() == self.inner.model.content();
                let refocus = reload && self.has_focus_within();
                self.inner.refocus.set(refocus);
                if refocus && let Some(root) = self.inner.view.root() {
                    root.set_focus(None::<&gtk::Widget>);
                }
                return;
            }
            Change::Reset => {
                self.start_growth();
                let content = self.inner.model.content();
                let same = *self.inner.content.borrow() == content;
                self.inner.content.replace(content);
                if same {
                    // The same view read again (a reload): it keeps its place, its selection
                    // and the keyboard focus. GTK lost the row that had the focus with the reset
                    // and would scroll its next focus row, the first, into view.
                    self.inner.model.complete_growth();
                    self.scroll_after_layout(self.inner.place.get());
                    if self.inner.refocus.get() {
                        self.focus_after_layout();
                    }
                    self.restore_selection();
                } else {
                    self.inner.wanted.set(None);
                    self.scroll_after_layout(0.0);
                }
                if let Some(latch) = self.inner.first_rows.borrow().as_ref() {
                    latch.open();
                }
            }
            Change::Rows => self.select_wanted(),
            Change::Failed => {
                if let Some(latch) = self.inner.first_rows.borrow().as_ref() {
                    latch.open();
                }
            }
        }
        self.update_empty();
        self.emit_search();
        self.queue_sweep();
    }

    fn update_empty(&self) {
        let inner = &self.inner;
        let model = &inner.model;
        if model.is_pending() {
            return;
        }
        let empty = model.total() == 0;
        if empty {
            let (title, description) = match model.content() {
                Content::Nothing => ("No folder", "Pick a folder to see its messages."),
                Content::Search { .. } => ("No results", "No messages match the search."),
                Content::View(spec) => match spec.filter {
                    ViewFilter::All => ("No messages", "Messages in this folder appear here."),
                    ViewFilter::Unread => ("No unread messages", "Everything here is read."),
                    ViewFilter::Starred => ("No starred messages", "Starred messages appear here."),
                    ViewFilter::Attachments => {
                        ("No attachments", "Messages with attachments appear here.")
                    }
                },
            };
            inner.empty.set_title(title);
            inner.empty.set_description(Some(description));
        }
        inner
            .stack
            .set_visible_child_name(if empty { "empty" } else { "list" });
    }

    /// Tells the search listeners again what the list shows (the window resets the list's title
    /// when it rebuilds its sidebar).
    pub fn refresh_heading(&self) {
        self.emit_search();
    }

    fn emit_search(&self) {
        let heading =
            matches!(self.inner.model.content(), Content::Search { .. }).then(|| SearchHeading {
                results: self.inner.model.total(),
                more: self.inner.model.has_more_results(),
                searching: self.inner.model.is_searching(),
            });
        let handlers = self.inner.handlers.search.borrow().clone();
        for handler in handlers {
            handler(heading.as_ref());
        }
    }

    /// After a reset GTK sees [`FIRST_ROWS`] rows; each later frame shows [`GROW_STEP`] more until
    /// every row is there (S6). The rows bound after each frame are kept for the self-test.
    fn start_growth(&self) {
        if let Some(id) = self.inner.growth.take() {
            id.remove();
        }
        self.inner.growth_frames.borrow_mut().clear();
        let weak = self.downgrade();
        let ticks = Cell::new(0u32);
        let done = Cell::new(false);
        let id = self.inner.view.add_tick_callback(move |_, _| {
            let Some(inner) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let tick = ticks.get();
            ticks.set(tick + 1);
            if tick > 0 {
                let bound = inner.context.bound.borrow().len() as u32;
                inner.growth_frames.borrow_mut().push(bound);
            }
            if tick == 0 {
                return glib::ControlFlow::Continue;
            }
            if done.get() {
                inner.growth.take();
                return glib::ControlFlow::Break;
            }
            if !inner.model.grow() {
                done.set(true);
            }
            glib::ControlFlow::Continue
        });
        self.inner.growth.replace(Some(id));
    }

    /// Rows bound after each frame since the last reset, while the list grew: the first is what
    /// the first frame laid out.
    pub fn growth_frames(&self) -> Vec<u32> {
        self.inner.growth_frames.borrow().clone()
    }

    pub fn is_growing(&self) -> bool {
        self.inner.growth.borrow().is_some()
    }

    fn on_event(&self, event: &Arc<ChangeEvent>) {
        let at_top = self.inner.scroller.vadjustment().value() < PIN_SLACK;
        let inserted = self.inner.model.apply_event(event);
        if inserted && at_top {
            self.scroll_after_layout(0.0);
        }
    }

    /// Scrolls to `value` once GTK has laid out the rows that just changed. GTK keeps the row it
    /// showed first anchored, so new mail would land above the view; after a reset it anchors the
    /// first row's top, leaving its section header above the view (S3), and loses the place a
    /// reload should keep. Moving the adjustment in the layout phase, after GTK's own layout,
    /// lays the list out again within the same frame.
    fn scroll_after_layout(&self, value: f64) {
        let view = &self.inner.view;
        match view.frame_clock() {
            Some(clock) => self.scroll_on(&clock, value),
            None => {
                let weak = self.downgrade();
                let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
                let once = handler.clone();
                let id = view.connect_map(move |view| {
                    if let Some(id) = once.take() {
                        view.disconnect(id);
                    }
                    if let Some(clock) = view.frame_clock() {
                        Self::with(&weak, |list| list.scroll_on(&clock, value));
                    }
                });
                handler.replace(Some(id));
            }
        }
    }

    fn scroll_on(&self, clock: &gtk::gdk::FrameClock, value: f64) {
        let weak = self.downgrade();
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let once = handler.clone();
        let id = clock.connect_layout(move |clock| {
            if let Some(id) = once.take() {
                clock.disconnect(id);
            }
            Self::with(&weak, |list| {
                list.inner.scroller.vadjustment().set_value(value)
            });
        });
        handler.replace(Some(id));
        self.inner.view.queue_resize();
    }

    /// Gives the keyboard focus to the selected row, or else the first row in view, once the
    /// list is laid out at its place: the scroll lands in one frame's layout phase and GTK lays
    /// the rows out there in the next frame at the latest.
    fn focus_after_layout(&self) {
        let Some(clock) = self.inner.view.frame_clock() else {
            return;
        };
        let weak = self.downgrade();
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
        let once = handler.clone();
        let painted = Cell::new(0u32);
        let id = clock.connect_after_paint(move |clock| {
            painted.set(painted.get() + 1);
            if painted.get() < 2 {
                return;
            }
            if let Some(id) = once.take() {
                clock.disconnect(id);
            }
            Self::with(&weak, |list| list.focus_row_in_view());
        });
        handler.replace(Some(id));
        self.inner.view.queue_draw();
    }

    fn focus_row_in_view(&self) {
        let scroller = self.scroller();
        let height = scroller.height() as f32;
        let in_view = |row: &MessageRow| {
            row.is_mapped()
                && row.compute_bounds(scroller).is_some_and(|bounds| {
                    bounds.y() >= 0.0 && bounds.y() + bounds.height() <= height
                })
        };
        let position = self
            .selected_position()
            .filter(|position| self.row_widget(*position).is_some_and(|row| in_view(&row)))
            .or_else(|| {
                self.inner
                    .context
                    .bound
                    .borrow()
                    .iter()
                    .filter(|row| in_view(row))
                    .filter_map(MessageRow::position)
                    .min()
            });
        if let Some(position) = position {
            let view = &self.inner.view;
            view.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
            view.grab_focus();
        }
    }

    /// After rows moved under a page on its way, asks again for the rows still shown as
    /// placeholders.
    fn queue_sweep(&self) {
        if self.inner.sweep_queued.replace(true) {
            return;
        }
        let weak = self.downgrade();
        glib::idle_add_local_once(move || {
            Self::with(&weak, |list| {
                list.inner.sweep_queued.set(false);
                let positions: Vec<u32> = list
                    .inner
                    .context
                    .bound
                    .borrow()
                    .iter()
                    .filter(|row| row.item().is_some_and(|item| !item.is_loaded()))
                    .filter_map(MessageRow::position)
                    .collect();
                for position in positions {
                    list.inner.model.request_position(position);
                }
            });
        });
    }

    fn scrolled(&self) {
        self.fetch_more_results();
        if self.inner.speed.borrow().is_some() {
            return;
        }
        let weak = self.downgrade();
        let last: Cell<Option<(f64, i64)>> = Cell::new(None);
        let slow = Cell::new(0u32);
        let still = Cell::new(0u32);
        let id = self.inner.view.add_tick_callback(move |_, clock| {
            let Some(inner) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let list = MessageList { inner };
            let value = list.inner.scroller.vadjustment().value();
            let now = clock.frame_time();
            let (speed, moved) = match last.replace(Some((value, now))) {
                Some((before, then)) if now > then => (
                    (value - before).abs() / ((now - then) as f64 / 1e6),
                    value != before,
                ),
                _ => (0.0, true),
            };
            if speed > SKELETON_SPEED {
                slow.set(0);
                list.inner.context.skeleton.set(true);
            } else {
                slow.set(slow.get() + 1);
                if slow.get() >= SLOW_FRAMES {
                    list.show_text();
                }
            }
            still.set(if moved { 0 } else { still.get() + 1 });
            if still.get() >= STILL_FRAMES {
                list.show_text();
                list.inner.speed.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        self.inner.speed.replace(Some(id));
    }

    /// Search results come [`SEARCH_PAGE`] at a time; the next ones are fetched once the last
    /// screenful is in view. GtkListView realizes rows far beyond the view, so the rows it asks
    /// for can't tell.
    fn fetch_more_results(&self) {
        if !matches!(self.inner.model.content(), Content::Search { .. }) {
            return;
        }
        let adjustment = self.inner.scroller.vadjustment();
        let page = adjustment.page_size();
        if page > 0.0 && adjustment.value() + 2.0 * page >= adjustment.upper() {
            self.inner.model.search_more();
        }
    }

    /// Rows show their text again after a fast scroll.
    fn show_text(&self) {
        if !self.inner.context.skeleton.replace(false) {
            return;
        }
        let rows: Vec<MessageRow> = self
            .inner
            .context
            .bound
            .borrow()
            .iter()
            .filter(|row| row.shows_skeleton())
            .cloned()
            .collect();
        for row in rows {
            row.refresh();
        }
    }

    /// Whether rows skip their text now because the list moves fast.
    pub fn is_skeleton(&self) -> bool {
        self.inner.context.skeleton.get()
    }

    fn width_changed(&self, width: f64) {
        let scale = ((width / 400.0) * 2.0).floor().max(2.0) as f32 / 2.0;
        if self.inner.context.scale.replace(scale) == scale {
            return;
        }
        let rows = self.inner.context.bound.borrow().clone();
        for row in rows {
            row.refresh();
        }
    }

    fn queue_settle(&self) {
        if self.inner.settle_queued.replace(true) {
            return;
        }
        let weak = self.downgrade();
        glib::idle_add_local_once(move || {
            Self::with(&weak, |list| list.settle());
        });
    }

    /// Reports the selection once it has settled: a reset that keeps the same message selected
    /// reports nothing, and a placeholder is reported once its row is in.
    fn settle(&self) {
        let inner = &self.inner;
        inner.settle_queued.set(false);
        if let Some((item, handler)) = inner.waiting.take() {
            item.disconnect(handler);
        }
        let item = inner
            .selection
            .selected_item()
            .and_downcast::<MessageItem>();
        if item.is_some() {
            inner.wanted.set(None);
        }
        match item {
            Some(item) if item.is_loaded() => {
                let id = item.id();
                if inner.selected.replace(id) != id
                    && let Some(id) = id
                {
                    let handlers = inner.handlers.selected.borrow().clone();
                    for handler in handlers {
                        handler(id);
                    }
                }
            }
            Some(item) => {
                let weak = self.downgrade();
                let handler = item.connect_changed(move |_| {
                    Self::with(&weak, |list| list.queue_settle());
                });
                inner.waiting.replace(Some((item, handler)));
            }
            None => {
                if inner.selected.take().is_some() {
                    let handlers = inner.handlers.cleared.borrow().clone();
                    for handler in handlers {
                        handler();
                    }
                }
            }
        }
    }

    /// After the same content was read again (a reload), selects the message that was selected,
    /// now or once its row loads.
    fn restore_selection(&self) {
        if self.selected_position().is_none()
            && let Some(id) = self.inner.selected.get()
        {
            self.inner.wanted.set(Some(id));
        }
        self.select_wanted();
    }

    fn select_wanted(&self) {
        let Some(wanted) = self.inner.wanted.get() else {
            return;
        };
        if let Some(position) = self.inner.model.position_of(wanted) {
            self.inner.wanted.set(None);
            if position >= self.inner.model.n_items() {
                self.inner.model.complete_growth();
            }
            self.inner.selection.set_selected(position);
        }
    }

    /// The selected message.
    pub fn selected(&self) -> Option<MessageId> {
        self.inner
            .selection
            .selected_item()
            .and_downcast::<MessageItem>()
            .and_then(|item| item.id())
    }

    /// The selected position.
    pub fn selected_position(&self) -> Option<u32> {
        Some(self.inner.selection.selected()).filter(|p| *p != gtk::INVALID_LIST_POSITION)
    }

    /// Where a message is in the list, 1-based, and how many the list holds: the reader's
    /// "n of N". `None` when its row isn't loaded.
    pub fn position(&self, id: MessageId) -> Option<(u32, u32)> {
        let total = self.inner.model.total();
        if let Some(selected) = self.selected_position()
            && self
                .inner
                .model
                .row_at(selected)
                .is_some_and(|row| row.id == id)
        {
            return Some((selected + 1, total));
        }
        self.inner
            .model
            .position_of(id)
            .map(|position| (position + 1, total))
    }

    /// Selects the next message (Next Message: `j`, and GtkListView's own Down).
    pub fn select_next(&self) {
        self.step(1);
    }

    /// Selects the previous message (Previous Message: `k`, and GtkListView's own Up).
    pub fn select_previous(&self) {
        self.step(-1);
    }

    fn step(&self, delta: i64) {
        let total = self.inner.model.total();
        if total == 0 {
            return;
        }
        let target = match self.selected_position() {
            None => 0,
            Some(current) => (i64::from(current) + delta).clamp(0, i64::from(total) - 1) as u32,
        };
        self.select_position(target);
    }

    /// Selects the message at `position` and scrolls it into view; the keyboard focus moves
    /// with it only when it was in the list.
    pub fn select_position(&self, position: u32) {
        let inner = &self.inner;
        if position >= inner.model.total() {
            return;
        }
        if position >= inner.model.n_items() {
            inner.model.complete_growth();
        }
        let focus = self.has_focus_within();
        inner.selection.set_selected(position);
        let flags = if focus {
            gtk::ListScrollFlags::FOCUS
        } else {
            gtk::ListScrollFlags::NONE
        };
        inner.view.scroll_to(position, flags, None);
    }

    fn has_focus_within(&self) -> bool {
        let view = &self.inner.view;
        view.root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| {
                &focus == view.upcast_ref::<gtk::Widget>() || focus.is_ancestor(view)
            })
    }

    /// Opens the selected message (Open Message: Enter, `o`, or a double click).
    pub fn open_selected(&self) {
        if let Some(id) = self.selected() {
            let handlers = self.inner.handlers.opened.borrow().clone();
            for handler in handlers {
                handler(id);
            }
        }
    }

    /// Runs `handler` with every message the user selects: clicks, keys, next and previous.
    pub fn connect_message_selected(&self, handler: impl Fn(MessageId) + 'static) {
        self.inner
            .handlers
            .selected
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when the user opens the selected message (Enter, `o`, double click).
    pub fn connect_message_opened(&self, handler: impl Fn(MessageId) + 'static) {
        self.inner
            .handlers
            .opened
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when no message is selected any more (another folder, the message gone).
    pub fn connect_selection_cleared(&self, handler: impl Fn() + 'static) {
        self.inner
            .handlers
            .cleared
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when search results come or go (`None`: the folder shows again).
    pub fn connect_search_changed(&self, handler: impl Fn(Option<&SearchHeading>) + 'static) {
        self.inner
            .handlers
            .search
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// The List scope's keys on the pane (after its search field) and on the list view itself
    /// (before GtkListView's own Enter and Space), as actions.md describes.
    pub fn install_keys(&self, keymap: &Keymap) {
        let keys = actions::install_scoped(&self.inner.root, Scope::List, keymap);
        keys.attach(&self.inner.view, gtk::PropagationPhase::Capture);
        self.inner.keys.replace(Some(keys));
    }

    /// `keys.toml` changed.
    pub fn update_keys(&self, keymap: &Keymap) {
        if let Some(keys) = self.inner.keys.borrow().as_ref() {
            keys.update(keymap);
        }
    }

    pub fn keys(&self) -> Option<std::cell::Ref<'_, ScopedShortcuts>> {
        std::cell::Ref::filter_map(self.inner.keys.borrow(), Option::as_ref).ok()
    }

    fn now(&self) -> glib::DateTime {
        self.inner
            .clock
            .get()
            .and_then(|unix| glib::DateTime::from_unix_local(unix).ok())
            .or_else(|| glib::DateTime::now_local().ok())
            .unwrap_or_else(|| glib::DateTime::from_unix_utc(0).expect("the epoch is a valid date"))
    }

    /// "Now" as the list sees it (Unix seconds).
    pub fn now_unix(&self) -> i64 {
        self.now().to_unix()
    }

    /// The sections for today.
    pub fn thresholds_now(&self) -> Thresholds {
        let now = self.now();
        self.inner.context.year.set(now.year());
        Thresholds::at(&now).unwrap_or_else(|_| Thresholds::new(0, 0, 0, 0))
    }

    /// Fixes "now" (Unix seconds; `None`: the real clock) and regroups, as midnight does (the
    /// self-test).
    pub fn set_clock(&self, unix: Option<i64>) {
        self.inner.clock.set(unix);
        self.regroup_if_needed();
    }

    fn regroup_if_needed(&self) {
        let wanted = self.thresholds_now();
        if wanted != self.inner.model.thresholds() {
            tracing::debug!("the day changed; regrouping the list");
            self.inner.model.regroup(wanted);
        }
    }

    fn schedule_day_check(&self) {
        let delay = until_next_day(&self.now()).min(DAY_CHECK) + Duration::from_secs(1);
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(delay, move || {
            Self::with(&weak, |list| {
                list.inner.day_check.take();
                list.regroup_if_needed();
                list.schedule_day_check();
            });
        });
        if let Some(previous) = self.inner.day_check.replace(Some(source)) {
            previous.remove();
        }
    }

    /// The rows GtkListView has bound now.
    pub fn bound_rows(&self) -> Vec<BoundRow> {
        self.inner
            .context
            .bound
            .borrow()
            .iter()
            .map(|row| BoundRow {
                position: row.position(),
                id: row.id(),
                placeholder: row.is_placeholder(),
            })
            .collect()
    }

    /// The bound row at `position`.
    pub fn row_widget(&self, position: u32) -> Option<MessageRow> {
        self.inner
            .context
            .bound
            .borrow()
            .iter()
            .find(|row| row.position() == Some(position))
            .cloned()
    }

    /// The section headers in view now, top to bottom.
    pub fn headers(&self) -> Vec<String> {
        let scroller = &self.inner.scroller;
        let height = scroller.height() as f32;
        let mut headers: Vec<(f32, String)> = self
            .inner
            .context
            .headers
            .borrow()
            .iter()
            .filter(|header| header.is_mapped())
            .filter_map(|header| {
                let bounds = header.compute_bounds(scroller)?;
                let visible = bounds.y() >= 0.0 && bounds.y() + bounds.height() <= height;
                visible.then(|| (bounds.y(), header.text()))
            })
            .collect();
        headers.sort_by(|a, b| a.0.total_cmp(&b.0));
        headers.into_iter().map(|(_, text)| text).collect()
    }

    /// Rows bound so far.
    pub fn binds(&self) -> u64 {
        self.inner.context.binds.get()
    }

    /// Row widgets created so far.
    pub fn rows_created(&self) -> u64 {
        self.inner.context.rows_created.get()
    }

    pub fn folder_of(scope: ViewScope) -> Option<FolderId> {
        match scope {
            ViewScope::Folder(folder) => Some(folder),
            ViewScope::UnifiedInbox => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_headings_count_their_results() {
        let heading = |results, more, searching| SearchHeading {
            results,
            more,
            searching,
        };
        assert_eq!(heading(0, false, true).subtitle(), "Searching…");
        assert_eq!(heading(0, false, false).subtitle(), "No results");
        assert_eq!(heading(1, false, false).subtitle(), "1 result");
        assert_eq!(heading(12, false, true).subtitle(), "12 results");
        assert_eq!(heading(50, true, false).subtitle(), "50+ results");
    }

    #[test]
    fn chips_name_the_store_filters() {
        assert_eq!(filter_name(ViewFilter::Unread), "Unread");
        assert_eq!(FILTERS.len(), 4);
    }
}
