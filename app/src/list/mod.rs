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
//! - The List scope's keys move the selection (`j`, `k`; GtkListView's Down and Up) and open it
//!   (Enter, `o`). Several messages can be selected for batch actions (ML-054): Ctrl+click
//!   toggles a row, Shift+click and Shift with the arrows or `j`/`k` extend, and Select (`x`,
//!   Space) enters a select mode whose rows show checkboxes, where `j`/`k` and the arrows move a
//!   cursor and Escape leaves.

pub mod item;
pub mod model;
pub mod row;
pub mod sections;

pub use model::{Change, Content, FIRST_ROWS, GROW_STEP, MessageModel, ModelStats, SEARCH_PAGE};

use crate::actions::{self, ScopedShortcuts};
use crate::mailbox::{Latch, Mailbox};
use crate::triage::Target;
use crate::window::empty;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use item::MessageItem;
pub use row::Quick;
use row::{MessageRow, RowContext, SectionHeader};
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{FolderId, MessageId};
use sealwax_infrastructure::store::{ChangeEvent, ViewFilter, ViewScope, ViewSpec};
use sections::{Thresholds, until_next_day};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
/// How long the list waits for the messages of an action to leave it.
const LEAVING_WAIT: Duration = Duration::from_secs(10);
/// How long an action waits for selected rows that aren't loaded yet.
const TARGETS_WAIT: Duration = Duration::from_secs(3);

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
type CountHandler = Rc<dyn Fn(u32)>;
type QuickHandler = Rc<dyn Fn(ActionId, Target)>;

#[derive(Default)]
struct Handlers {
    selected: RefCell<Vec<IdHandler>>,
    opened: RefCell<Vec<IdHandler>>,
    cleared: RefCell<Vec<Handler>>,
    batch: RefCell<Vec<CountHandler>>,
    search: RefCell<Vec<SearchHandler>>,
    older: RefCell<Vec<FolderHandler>>,
    quick: RefCell<Vec<QuickHandler>>,
}

type FolderHandler = Rc<dyn Fn(FolderId)>;

/// What the list last reported about its selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selected {
    Nothing,
    /// One message, outside the select mode: the reader shows it.
    One(MessageId),
    /// The select mode, with this many messages selected.
    Batch(u32),
}

/// Messages an action is taking out of the list. Once one is gone and nothing is selected, the
/// row that took the first one's place is selected: the next message, as Gmail does.
struct Leaving {
    ids: HashSet<MessageId>,
    position: u32,
    since: Instant,
}

/// Older mail of a folder whose first sync kept only its newest messages (ML-043).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Older {
    /// Asked for; "Loading older messages…" shows.
    Asked,
    /// Nothing came and more may be there (offline): asked again once the folder is shown again.
    Paused,
    /// The server has nothing older.
    Complete,
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
    selection: gtk::MultiSelection,
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
    /// What the list last reported as selected.
    reported: Cell<Selected>,
    /// Messages to select again once their rows load, after a reload, until `wanted_until`.
    wanted: RefCell<Vec<MessageId>>,
    wanted_until: Cell<Option<Instant>>,
    /// The selected messages and the select mode just before a reset.
    restoring: RefCell<(Vec<MessageId>, bool)>,
    /// The selected messages when the selection last settled.
    settled: RefCell<Vec<MessageId>>,
    /// Rows show checkboxes, and `j`/`k` and the arrows move the cursor, not the selection.
    select_mode: Cell<bool>,
    /// The row the select mode's cursor is on, when the list hasn't got the keyboard focus.
    cursor: Cell<Option<MessageId>>,
    /// A click without Ctrl or Shift selects one row and leaves the select mode.
    plain_click: Cell<bool>,
    leaving: RefCell<Option<Leaving>>,
    /// Next and Previous Message's one-key keys, which extend the selection with Shift.
    extend_keys: RefCell<Vec<(gdk::Key, i64)>>,
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
    /// The quiet row under the list while older mail loads.
    older_row: gtk::Label,
    older: RefCell<std::collections::HashMap<FolderId, Older>>,
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
        search.set_widget_name("list-search");
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
        let selection = gtk::MultiSelection::new(Some(model.clone()));
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
        let older_row = gtk::Label::builder()
            .label("Loading older messages…")
            .visible(false)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        older_row.add_css_class("dim-label");
        older_row.add_css_class("caption");
        older_row.add_css_class("sw-list-older");
        older_row.set_widget_name("list-older");
        root.append(&older_row);

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
                reported: Cell::new(Selected::Nothing),
                wanted: RefCell::new(Vec::new()),
                wanted_until: Cell::new(None),
                restoring: RefCell::new((Vec::new(), false)),
                settled: RefCell::new(Vec::new()),
                select_mode: Cell::new(false),
                cursor: Cell::new(None),
                plain_click: Cell::new(false),
                leaving: RefCell::new(None),
                extend_keys: RefCell::new(Vec::new()),
                place: Cell::new(0.0),
                refocus: Cell::new(false),
                settle_queued: Cell::new(false),
                waiting: RefCell::new(None),
                sweep_queued: Cell::new(false),
                handlers: Handlers::default(),
                keys: RefCell::new(None),
                first_rows: RefCell::new(None),
                day_check: RefCell::new(None),
                older_row,
                older: RefCell::default(),
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
        inner.selection.connect_selection_changed(move |_, _, _| {
            Self::with(&weak, |list| list.queue_settle());
        });
        // GtkMultiSelection drops removed rows from the selection without a signal of its own.
        let weak = self.downgrade();
        inner
            .selection
            .connect_items_changed(move |_, _, removed, _| {
                if removed > 0 {
                    Self::with(&weak, |list| list.queue_settle());
                }
            });
        let weak = self.downgrade();
        inner.view.connect_activate(move |_, position| {
            Self::with(&weak, |list| {
                list.inner.select_mode.set(false);
                list.select_position(position);
                list.settle();
                list.open_selected();
            });
        });
        self.connect_selection_input();
        let weak = self.downgrade();
        inner.context.set_quick(move |action, target| {
            Self::with(&weak, |list| {
                let handlers = list.inner.handlers.quick.borrow().clone();
                for handler in handlers {
                    handler(action, target);
                }
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
                if list.selected_positions().is_empty() && list.inner.model.total() > 0 {
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
            .vadjustment()
            .connect_changed(move |_| Self::with(&weak, |list| list.check_end()));
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

    pub fn selection(&self) -> &gtk::MultiSelection {
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
        if let Some(ViewScope::Folder(folder)) = scope {
            let mut older = self.inner.older.borrow_mut();
            if older.get(&folder) == Some(&Older::Paused) {
                older.remove(&folder);
            }
        }
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

    /// Ends the search, as Escape in the search field does; text typed within the entry's
    /// search delay goes too, so it doesn't start a search afterwards.
    pub fn clear_search(&self) {
        if !self.inner.search.text().is_empty() {
            self.inner.quiet.set(true);
            self.inner.search.set_text("");
            self.inner.quiet.set(false);
        }
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
                // The model holds the new rows already; the selection is what settled last.
                let selected = self.inner.settled.borrow().clone();
                self.inner
                    .restoring
                    .replace((selected, self.inner.select_mode.get()));
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
                    self.inner.wanted.borrow_mut().clear();
                    self.inner.leaving.take();
                    self.inner.cursor.set(None);
                    self.set_select_mode(false);
                    self.scroll_after_layout(0.0);
                }
                if let Some(latch) = self.inner.first_rows.borrow().as_ref() {
                    latch.open();
                }
            }
            Change::Rows => {
                self.select_wanted();
                self.check_leaving();
            }
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
                MessageList { inner }.check_end();
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
        self.check_end();
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

    /// Within a screenful of the end of a folder (never All Inboxes or search results), once all
    /// its rows are in: asks for older mail, unless it was asked for, the server has none, or an
    /// answer brought nothing since the folder was shown (ML-043).
    fn check_end(&self) {
        let inner = &self.inner;
        let Content::View(ViewSpec {
            scope: ViewScope::Folder(folder),
            ..
        }) = inner.model.content()
        else {
            return;
        };
        if self.is_growing()
            || inner.model.is_pending()
            || inner.older.borrow().contains_key(&folder)
        {
            return;
        }
        let adjustment = inner.scroller.vadjustment();
        let page = adjustment.page_size();
        if page <= 0.0 || adjustment.value() + 2.0 * page < adjustment.upper() {
            return;
        }
        inner.older.borrow_mut().insert(folder, Older::Asked);
        inner.older_row.set_visible(true);
        let handlers = inner.handlers.older.borrow().clone();
        for handler in handlers {
            handler(folder);
        }
    }

    /// Runs `handler` when the list wants `folder`'s older mail (`Command::LoadOlder`).
    pub fn connect_older_wanted(&self, handler: impl Fn(FolderId) + 'static) {
        self.inner
            .handlers
            .older
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// The engine's answer (`Event::OlderLoaded`): the row goes; the stored mail arrives as
    /// change events. Nothing more is asked once `complete`; after an answer with nothing, not
    /// until the folder is shown again.
    pub fn older_loaded(&self, folder: FolderId, fetched: u32, complete: bool) {
        let mut older = self.inner.older.borrow_mut();
        if complete {
            older.insert(folder, Older::Complete);
        } else if fetched > 0 {
            older.remove(&folder);
        } else {
            older.insert(folder, Older::Paused);
        }
        let asking = older.values().any(|state| *state == Older::Asked);
        drop(older);
        self.inner.older_row.set_visible(asking);
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
    /// reports nothing, and a placeholder is reported once its row is in. Two messages or more
    /// put the list in the select mode, and nothing selected, or a plain click, leaves it.
    fn settle(&self) {
        let inner = &self.inner;
        inner.settle_queued.set(false);
        if let Some((item, handler)) = inner.waiting.take() {
            item.disconnect(handler);
        }
        let positions = self.selected_positions();
        if !positions.is_empty() || inner.wanted_until.get() < Some(Instant::now()) {
            inner.wanted.borrow_mut().clear();
        }
        if !inner.wanted.borrow().is_empty() {
            // A reload is bringing the selection back as its rows load.
            return;
        }
        inner.settled.replace(self.selected_ids());
        let plain_click = inner.plain_click.replace(false);
        if positions.is_empty() || (plain_click && positions.len() == 1) {
            self.set_select_mode(false);
        } else if positions.len() > 1 {
            self.set_select_mode(true);
        }
        let selected = match positions.as_slice() {
            [] => Selected::Nothing,
            [position] if !inner.select_mode.get() => {
                let Some(item) = inner.model.item(*position).and_downcast::<MessageItem>() else {
                    return;
                };
                match item.id() {
                    Some(id) => {
                        inner.cursor.set(Some(id));
                        Selected::One(id)
                    }
                    None => {
                        let weak = self.downgrade();
                        let handler = item.connect_changed(move |_| {
                            Self::with(&weak, |list| list.queue_settle());
                        });
                        inner.waiting.replace(Some((item, handler)));
                        return;
                    }
                }
            }
            positions => Selected::Batch(positions.len() as u32),
        };
        self.report(selected);
    }

    fn report(&self, selected: Selected) {
        if self.inner.reported.replace(selected) == selected {
            return;
        }
        let handlers = &self.inner.handlers;
        match selected {
            Selected::Nothing => {
                let cleared = handlers.cleared.borrow().clone();
                for handler in cleared {
                    handler();
                }
            }
            Selected::One(id) => {
                let selected = handlers.selected.borrow().clone();
                for handler in selected {
                    handler(id);
                }
            }
            Selected::Batch(count) => {
                let batch = handlers.batch.borrow().clone();
                for handler in batch {
                    handler(count);
                }
            }
        }
    }

    /// What the list last reported: nothing, one message, or the select mode's count.
    pub fn reported(&self) -> Selected {
        self.inner.reported.get()
    }

    /// Whether the rows show checkboxes and `j`/`k` move a cursor.
    pub fn is_select_mode(&self) -> bool {
        self.inner.select_mode.get()
    }

    fn set_select_mode(&self, on: bool) {
        if self.inner.select_mode.replace(on) == on {
            return;
        }
        self.inner.context.select_mode.set(on);
        if on {
            self.inner.view.add_css_class("sw-select-mode");
        } else {
            self.inner.view.remove_css_class("sw-select-mode");
        }
        let rows = self.inner.context.bound.borrow().clone();
        for row in rows {
            row.refresh();
        }
    }

    /// After the same content was read again (a reload), selects the messages that were
    /// selected, now or once their rows load, in the select mode if it was on.
    fn restore_selection(&self) {
        let (ids, select_mode) = self.inner.restoring.take();
        if select_mode {
            self.set_select_mode(true);
        }
        if self.selected_positions().is_empty() && !ids.is_empty() {
            self.inner.wanted.replace(ids);
            self.inner
                .wanted_until
                .set(Some(Instant::now() + TARGETS_WAIT));
            let weak = self.downgrade();
            glib::timeout_add_local_once(TARGETS_WAIT, move || {
                Self::with(&weak, |list| list.queue_settle());
            });
        }
        self.select_wanted();
    }

    fn select_wanted(&self) {
        if self.inner.wanted.borrow().is_empty() {
            return;
        }
        let wanted = self.inner.wanted.take();
        let mut waiting = Vec::new();
        for id in wanted {
            match self.inner.model.position_of(id) {
                Some(position) => {
                    if position >= self.inner.model.n_items() {
                        self.inner.model.complete_growth();
                    }
                    self.inner.selection.select_item(position, false);
                }
                None => waiting.push(id),
            }
        }
        self.inner.wanted.replace(waiting);
    }

    /// The selected positions, first to last.
    pub fn selected_positions(&self) -> Vec<u32> {
        let set = self.inner.selection.selection();
        match gtk::BitsetIter::init_first(&set) {
            Some((rest, first)) => std::iter::once(first).chain(rest).collect(),
            None => Vec::new(),
        }
    }

    /// The selected messages whose rows are loaded.
    pub fn selected_ids(&self) -> Vec<MessageId> {
        self.selected_positions()
            .into_iter()
            .filter_map(|position| self.inner.model.row_at(position).map(|row| row.id))
            .collect()
    }

    /// The message selected, when it is the only one.
    pub fn selected(&self) -> Option<MessageId> {
        self.selected_position()
            .and_then(|position| self.inner.model.row_at(position))
            .map(|row| row.id)
    }

    /// The selected position, when it is the only one.
    pub fn selected_position(&self) -> Option<u32> {
        match self.selected_positions().as_slice() {
            [position] => Some(*position),
            _ => None,
        }
    }

    /// The selected messages as an action needs them, or the positions whose rows aren't
    /// loaded yet.
    pub fn selected_targets(&self) -> Result<Vec<Target>, Vec<u32>> {
        let mut targets = Vec::new();
        let mut missing = Vec::new();
        for position in self.selected_positions() {
            match self.target_at(position) {
                Some(target) => targets.push(target),
                None => missing.push(position),
            }
        }
        if missing.is_empty() {
            Ok(targets)
        } else {
            Err(missing)
        }
    }

    /// The selected messages, once the rows of those far down a range are loaded too; `None`
    /// when they don't come within [`TARGETS_WAIT`].
    pub async fn wait_targets(&self) -> Option<Vec<Target>> {
        let started = Instant::now();
        loop {
            match self.selected_targets() {
                Ok(targets) => return Some(targets),
                Err(_) if started.elapsed() > TARGETS_WAIT => return None,
                Err(missing) => {
                    for position in missing {
                        self.inner.model.request_position(position);
                    }
                    glib::timeout_future(Duration::from_millis(16)).await;
                }
            }
        }
    }

    /// The message at `position`, as an action needs it.
    pub fn target_at(&self, position: u32) -> Option<Target> {
        self.inner.model.row_at(position).map(|row| Target {
            id: row.id,
            folder: row.folder,
            flags: row.flags,
        })
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

    /// Selects the next message (Next Message: `j`, and GtkListView's own Down); in the select
    /// mode, moves the cursor.
    pub fn select_next(&self) {
        self.step(1);
    }

    /// Selects the previous message (Previous Message: `k`, and GtkListView's own Up); in the
    /// select mode, moves the cursor.
    pub fn select_previous(&self) {
        self.step(-1);
    }

    fn step(&self, delta: i64) {
        let total = self.inner.model.total();
        if total == 0 {
            return;
        }
        self.inner.wanted.borrow_mut().clear();
        if self.inner.select_mode.get() {
            let target = self
                .cursor_position()
                .map_or(0, |current| offset(current, delta, total));
            self.move_cursor(target);
            return;
        }
        let target = match self.selected_positions().as_slice() {
            [] => 0,
            [first, ..] if delta < 0 => offset(*first, delta, total),
            positions => offset(positions[positions.len() - 1], delta, total),
        };
        self.select_position(target);
    }

    /// Extends the selection from where it started to the next or previous row (Shift with the
    /// arrows or with `j`/`k`), as Shift+click does.
    pub fn extend(&self, delta: i64) {
        let total = self.inner.model.total();
        if total == 0 {
            return;
        }
        self.inner.wanted.borrow_mut().clear();
        let Some(from) = self.cursor_position() else {
            self.select_position(0);
            return;
        };
        if self.selected_positions().is_empty() {
            self.select_position(from);
            return;
        }
        let target = offset(from, delta, total);
        if target >= self.inner.model.n_items() {
            self.inner.model.complete_growth();
        }
        self.select_item(target, false, true);
        self.move_cursor(target);
    }

    /// Select (`x`, Space): enters the select mode with the message under the cursor selected,
    /// or in the select mode selects or unselects it.
    pub fn toggle_selection(&self) {
        let total = self.inner.model.total();
        if total == 0 {
            return;
        }
        self.inner.wanted.borrow_mut().clear();
        let position = self.cursor_position().unwrap_or(0).min(total - 1);
        if position >= self.inner.model.n_items() {
            self.inner.model.complete_growth();
        }
        if let Some(row) = self.inner.model.row_at(position) {
            self.inner.cursor.set(Some(row.id));
        }
        if self.inner.select_mode.get() {
            self.select_item(position, true, false);
        } else {
            self.set_select_mode(true);
            if !self.inner.selection.is_selected(position) {
                self.select_item(position, true, false);
            }
            self.queue_settle();
        }
    }

    /// Leaves the select mode (Escape) with the message under the cursor selected alone.
    pub fn leave_select_mode(&self) {
        if !self.inner.select_mode.get() {
            return;
        }
        let position = self.cursor_position();
        self.set_select_mode(false);
        match position {
            Some(position) => self.select_position(position),
            None => {
                self.inner.selection.unselect_all();
            }
        }
        self.queue_settle();
    }

    /// The messages of `ids` are about to leave the list (an action on the selection). Once
    /// one has and nothing is selected any more, the row in the first one's place is selected.
    pub fn advance_after(&self, ids: &[MessageId]) {
        let Some(position) = ids
            .iter()
            .filter_map(|id| self.inner.model.position_of(*id))
            .min()
        else {
            return;
        };
        self.inner.leaving.replace(Some(Leaving {
            ids: ids.iter().copied().collect(),
            position,
            since: Instant::now(),
        }));
    }

    fn check_leaving(&self) {
        let ready = {
            let leaving = self.inner.leaving.borrow();
            let Some(leaving) = leaving.as_ref() else {
                return;
            };
            if leaving.since.elapsed() > LEAVING_WAIT {
                None
            } else if !self.selected_positions().is_empty()
                || self.inner.model.held(&leaving.ids) == leaving.ids.len()
            {
                return;
            } else {
                Some(leaving.position)
            }
        };
        self.inner.leaving.take();
        let total = self.inner.model.total();
        if let Some(position) = ready
            && total > 0
        {
            self.set_select_mode(false);
            self.select_position(position.min(total - 1));
        }
    }

    /// Selects the message at `position` alone and scrolls it into view; the keyboard focus
    /// moves with it only when it was in the list.
    pub fn select_position(&self, position: u32) {
        let inner = &self.inner;
        if position >= inner.model.total() {
            return;
        }
        if position >= inner.model.n_items() {
            inner.model.complete_growth();
        }
        inner.wanted.borrow_mut().clear();
        let focus = self.has_focus_within();
        self.select_item(position, false, false);
        if let Some(row) = inner.model.row_at(position) {
            inner.cursor.set(Some(row.id));
        }
        let flags = if focus {
            gtk::ListScrollFlags::FOCUS
        } else {
            gtk::ListScrollFlags::NONE
        };
        inner.view.scroll_to(position, flags, None);
    }

    /// GtkListView's own selection, as a click makes it: alone, toggled (`modify`) or from the
    /// anchor (`extend`); all but extending move the anchor.
    fn select_item(&self, position: u32, modify: bool, extend: bool) {
        if self
            .inner
            .view
            .activate_action(
                "list.select-item",
                Some(&(position, modify, extend).to_variant()),
            )
            .is_err()
        {
            tracing::warn!("the message list has no list.select-item");
        }
    }

    /// Moves the keyboard cursor without changing the selection (the select mode's `j`, `k`).
    fn move_cursor(&self, position: u32) {
        let inner = &self.inner;
        if position >= inner.model.n_items() {
            inner.model.complete_growth();
        }
        if let Some(row) = inner.model.row_at(position) {
            inner.cursor.set(Some(row.id));
        }
        let flags = if self.has_focus_within() {
            gtk::ListScrollFlags::FOCUS
        } else {
            gtk::ListScrollFlags::NONE
        };
        inner.view.scroll_to(position, flags, None);
    }

    /// The row the keyboard focus is on, else the cursor's, else the first selected.
    fn cursor_position(&self) -> Option<u32> {
        if self.has_focus_within()
            && let Some(position) = self.focused_row()
        {
            return Some(position);
        }
        self.inner
            .cursor
            .get()
            .and_then(|id| self.inner.model.position_of(id))
            .or_else(|| self.selected_positions().first().copied())
    }

    fn focused_row(&self) -> Option<u32> {
        let focus = self.inner.view.root()?.focus()?;
        self.inner
            .context
            .bound
            .borrow()
            .iter()
            .find(|row| row.is_ancestor(&focus) || focus == **row || focus.is_ancestor(*row))
            .and_then(MessageRow::position)
    }

    fn has_focus_within(&self) -> bool {
        let view = &self.inner.view;
        view.root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| {
                &focus == view.upcast_ref::<gtk::Widget>() || focus.is_ancestor(view)
            })
    }

    /// Clicks and keys of the selection: a plain click leaves the select mode; Shift with the
    /// arrows or with Next and Previous Message's keys extends; in the select mode the arrows
    /// move the cursor and Escape leaves; GtkListView's own Ctrl+A is no command here.
    fn connect_selection_input(&self) {
        let view = &self.inner.view;
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        click.connect_pressed(move |gesture, _, x, y| {
            let state = gesture.current_event_state();
            let modify = state.contains(gdk::ModifierType::CONTROL_MASK);
            let extend = state.contains(gdk::ModifierType::SHIFT_MASK);
            Self::with(&weak, |list| {
                let picked = list.inner.view.pick(x, y, gtk::PickFlags::DEFAULT);
                let control = picked.as_ref().is_some_and(|widget| {
                    widget.ancestor(gtk::CheckButton::static_type()).is_some()
                        || widget.ancestor(gtk::Button::static_type()).is_some()
                });
                list.inner.plain_click.set(!modify && !extend && !control);
                // Ctrl and Shift clicks take effect here, on the press: GtkListView's own
                // Ctrl+click on a selected row selected it alone on the press and unselected it
                // on the release (seen headless, GTK 4.22).
                let position = picked
                    .and_then(|widget| widget.ancestor(MessageRow::static_type()))
                    .and_downcast::<MessageRow>()
                    .and_then(|row| row.position());
                if (modify || extend)
                    && !control
                    && let Some(position) = position
                {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    list.inner.view.grab_focus();
                    list.select_item(position, modify, extend);
                    list.move_cursor(position);
                }
            });
        });
        view.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            MessageList { inner }.selection_key(key, state)
        });
        view.add_controller(keys);
    }

    fn selection_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        if state.contains(gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            return if key.to_lower() == gdk::Key::a {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            };
        }
        let delta = match key {
            gdk::Key::Down => Some(1),
            gdk::Key::Up => Some(-1),
            _ if shift => self
                .inner
                .extend_keys
                .borrow()
                .iter()
                .find(|(bound, _)| *bound == key.to_lower())
                .map(|(_, delta)| *delta),
            _ => None,
        };
        match delta {
            Some(delta) if shift => self.extend(delta),
            Some(delta) if self.inner.select_mode.get() => self.step(delta),
            _ if key == gdk::Key::Escape && !shift && self.inner.select_mode.get() => {
                self.leave_select_mode();
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
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

    /// Runs `handler` with the count of selected messages while the list is in the select mode.
    pub fn connect_batch_selected(&self, handler: impl Fn(u32) + 'static) {
        self.inner
            .handlers
            .batch
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Runs `handler` when a row's quick action is clicked: the command and that row's message,
    /// selected or not.
    pub fn connect_quick_action(&self, handler: impl Fn(ActionId, Target) + 'static) {
        self.inner
            .handlers
            .quick
            .borrow_mut()
            .push(Rc::new(handler));
    }

    /// Shows the quick actions' keys in their tooltips (`keys.toml`).
    pub fn set_quick_tooltips(&self, tooltips: Vec<(Quick, String)>) {
        self.inner.context.set_quick_tooltips(tooltips);
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
        self.inner.extend_keys.replace(extend_keys(keymap));
    }

    /// `keys.toml` changed.
    pub fn update_keys(&self, keymap: &Keymap) {
        if let Some(keys) = self.inner.keys.borrow().as_ref() {
            keys.update(keymap);
        }
        self.inner.extend_keys.replace(extend_keys(keymap));
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

/// `current` moved by `delta`, kept inside a list of `total` rows.
fn offset(current: u32, delta: i64, total: u32) -> u32 {
    (i64::from(current) + delta).clamp(0, i64::from(total) - 1) as u32
}

/// The one-key keys of Next and Previous Message, which extend the selection with Shift.
fn extend_keys(keymap: &Keymap) -> Vec<(gdk::Key, i64)> {
    [(ActionId::NextMessage, 1), (ActionId::PreviousMessage, -1)]
        .into_iter()
        .flat_map(|(id, delta)| {
            keymap
                .keys(id)
                .into_iter()
                .filter_map(|binding| binding.single().cloned())
                .filter(|key| key.is_one_key() && !key.shift)
                .filter_map(|key| gdk::Key::from_name(&key.key))
                .map(move |key| (key.to_lower(), delta))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_stay_inside_the_list() {
        assert_eq!(offset(0, -1, 10), 0);
        assert_eq!(offset(9, 1, 10), 9);
        assert_eq!(offset(4, 1, 10), 5);
    }

    #[test]
    fn shift_extends_with_next_and_previous_messages_one_key_keys() {
        assert_eq!(
            extend_keys(&Keymap::default()),
            [(gdk::Key::j, 1), (gdk::Key::k, -1)]
        );
        let rebound = Keymap::with_overrides([(
            ActionId::NextMessage,
            vec![sealwax_domain::keymap::Binding::parse("n").unwrap()],
        )]);
        assert_eq!(extend_keys(&rebound), [(gdk::Key::n, 1), (gdk::Key::k, -1)]);
    }

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
