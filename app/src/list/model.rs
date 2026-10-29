//! The message list's model: a lazy `gio::ListModel` + `gtk::SectionModel` over a store view or
//! over search results (S3's model, on the M1 store).
//!
//! - **Paging.** A view's rows arrive in pages of 100 from the store's readers; until then a
//!   position holds a placeholder item, filled in place. Nothing here runs SQL or blocks.
//! - **Versions** (docs/m1/store.md): the model keeps the `store_version` it has applied. Change
//!   events are applied strictly in order; a page applies only when its version matches; a newer
//!   page waits until the events catch up, an older one is dropped (the list asks again for the
//!   rows it still shows); a gap or a `Reload` opens the view again.
//! - **Sections** come from the view's section counts and shift with every row that comes or
//!   goes; they regroup when the day changes.
//! - **First frame** (S6): after every reset GTK sees at most [`FIRST_ROWS`] rows, then the list
//!   grows by [`GROW_STEP`] rows a frame ([`MessageModel::grow`]), so no single frame lays out the
//!   ~205 rows GtkListView keeps realized.
//! - **Search**: the store's newest matches, [`SEARCH_PAGE`] at a time, more as the list nears
//!   the end; the filter chip applies to them here.

use super::item::MessageItem;
use super::sections::{Bounds, LABELS, Thresholds};
use crate::mailbox::Mailbox;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gio, glib};
use sealwax_domain::model::{FolderId, MessageId, MessageSummary, ThreadId};
use sealwax_infrastructure::store::{
    ChangeEvent, OpenView, Page, Positioned, StoreError, StoreVersion, Versioned, ViewDiff,
    ViewFilter, ViewHandle, ViewOpened, ViewSpec,
};
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Rows per page request.
pub const PAGE: u32 = 100;
/// Rows the first frame after a reset shows: more than fit on any screen (14 at 1440 px, S6).
pub const FIRST_ROWS: u32 = 40;
/// Rows the list grows by each frame after that.
pub const GROW_STEP: u32 = 40;
/// GtkListView keeps about 205 rows realized whatever the window (S3); once the list is past
/// this, the rest arrive in one step without creating row widgets.
const GROW_UNTIL: u32 = 240;
/// Loaded rows kept; past it, rows far from where GTK last asked are dropped.
const MAX_LOADED: u32 = 4_000;
const KEEP_AROUND: u32 = 1_000;
/// Search results per request, and more each time the list nears their end.
pub const SEARCH_PAGE: u32 = 50;
const SEARCH_MAX: u32 = 10_000;
/// Change events that arrive while search results are shown run the search again after this.
const SEARCH_REFRESH: Duration = Duration::from_millis(200);

/// What the model shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Content {
    #[default]
    Nothing,
    View(ViewSpec),
    Search {
        text: String,
        folder: Option<FolderId>,
        filter: ViewFilter,
    },
}

/// What changed, for the list widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// GTK is about to be told of new content: the list notes its place first.
    Resetting,
    /// New content: every row may differ, and the list grows from [`FIRST_ROWS`].
    Reset,
    /// Rows were filled, added, removed or updated.
    Rows,
    /// The store couldn't be read; the model is empty.
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelStats {
    pub opens: u64,
    pub pages_requested: u64,
    pub pages_applied: u64,
    pub pages_stale: u64,
    pub pages_stashed: u64,
    pub events_applied: u64,
    pub reopens: u64,
    pub evictions: u64,
    pub searches: u64,
}

enum Stashed {
    Page(Page),
    Sections {
        version: StoreVersion,
        thresholds: Thresholds,
        counts: Vec<u32>,
        total: u32,
    },
}

impl Stashed {
    fn version(&self) -> StoreVersion {
        match self {
            Stashed::Page(page) => page.version,
            Stashed::Sections { version, .. } => *version,
        }
    }
}

struct ViewState {
    spec: ViewSpec,
    /// `None` while the view is opening.
    handle: Option<ViewHandle>,
    /// The version of the last change applied.
    version: StoreVersion,
    /// Page index → serial of the request on its way.
    in_flight: HashMap<u32, u64>,
    stash: Vec<Stashed>,
    /// Events that arrived while the view was opening.
    queued: Vec<Arc<ChangeEvent>>,
}

struct SearchState {
    text: String,
    folder: Option<FolderId>,
    filter: ViewFilter,
    /// Matches asked for.
    limit: u32,
    /// Every match so far, newest first, before the filter.
    rows: Vec<MessageSummary>,
    exhausted: bool,
    fetching: bool,
    /// A change event came in: run the search again.
    stale: bool,
    refresh_queued: bool,
    /// The first results are shown.
    applied: bool,
}

enum Source {
    Nothing,
    View(ViewState),
    Search(SearchState),
}

struct State {
    /// Bumped by every reset; replies for an older one are dropped.
    generation: u64,
    source: Source,
    /// Rows in the view or the search results.
    total: u32,
    /// Rows GTK sees: `total`, except while the list grows after a reset.
    shown: u32,
    slots: Vec<Option<MessageItem>>,
    /// Slots holding a loaded row.
    loaded: u32,
    /// Where GTK last asked for a row.
    last_position: u32,
    bounds: Bounds,
    thresholds: Thresholds,
    serial: u64,
    stats: ModelStats,
}

impl Default for State {
    fn default() -> Self {
        Self {
            generation: 0,
            source: Source::Nothing,
            total: 0,
            shown: 0,
            slots: Vec::new(),
            loaded: 0,
            last_position: 0,
            bounds: Bounds::default(),
            thresholds: Thresholds::new(0, 0, 0, 0),
            serial: 0,
            stats: ModelStats::default(),
        }
    }
}

impl State {
    fn section_at(&self, position: u32) -> usize {
        self.bounds.section_at(position).0
    }

    fn view_mut(&mut self) -> Option<&mut ViewState> {
        match &mut self.source {
            Source::View(view) => Some(view),
            _ => None,
        }
    }

    fn is_loaded(&self, position: u32) -> bool {
        self.slots
            .get(position as usize)
            .is_some_and(|slot| slot.as_ref().is_some_and(MessageItem::is_loaded))
    }

    /// Fills the rows of a page whose version matches the model's.
    fn fill(&mut self, offset: u32, rows: Vec<MessageSummary>) -> Fills {
        let mut fills = Fills::default();
        for (index, row) in rows.into_iter().enumerate() {
            let position = offset as usize + index;
            if position >= self.slots.len() {
                break;
            }
            let section = self.section_at(position as u32);
            match &self.slots[position] {
                Some(item) if !item.is_loaded() => {
                    self.loaded += 1;
                    fills.rows.push((item.clone(), row));
                }
                Some(item) if item.id() == Some(row.id) => fills.rows.push((item.clone(), row)),
                Some(_) => {
                    tracing::debug!(position, "list row replaced by a matching page");
                    self.slots[position] = Some(MessageItem::new(section, Some(row)));
                    if (position as u32) < self.shown {
                        fills.replaced.push(position as u32);
                    }
                }
                None => {
                    self.loaded += 1;
                    self.slots[position] = Some(MessageItem::new(section, Some(row)));
                }
            }
        }
        fills
    }

    /// Drops loaded rows far from where GTK last asked, once there are too many.
    fn evict(&mut self) {
        if self.loaded <= MAX_LOADED {
            return;
        }
        let keep = self.last_position.saturating_sub(KEEP_AROUND)
            ..self.last_position.saturating_add(KEEP_AROUND);
        let mut loaded = 0;
        for (position, slot) in self.slots.iter_mut().enumerate() {
            let Some(item) = slot else { continue };
            if !item.is_loaded() {
                continue;
            }
            if keep.contains(&(position as u32)) {
                loaded += 1;
            } else {
                *slot = None;
            }
        }
        self.loaded = loaded;
        self.stats.evictions += 1;
    }

    /// Whether some row of page `page` is still missing.
    fn page_missing(&self, page: u32) -> bool {
        let start = page * PAGE;
        if start >= self.total {
            return false;
        }
        let end = (start + PAGE).min(self.total) - 1;
        !self.is_loaded(start) || !self.is_loaded(end)
    }
}

#[derive(Default)]
struct Fills {
    rows: Vec<(MessageItem, MessageSummary)>,
    replaced: Vec<u32>,
}

/// A change to apply to the view, decided while the state is borrowed.
enum Plan {
    Ignore,
    Reopen,
    Refresh,
    Apply(Option<ViewDiff>),
}

/// Runs of positions removed one by one in descending order, as `(first, count)`.
fn removal_runs(removed: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &position in removed {
        match runs.last_mut() {
            Some((first, count)) if position + 1 == *first => {
                *first = position;
                *count += 1;
            }
            _ => runs.push((position, 1)),
        }
    }
    runs
}

/// Runs of rows inserted one by one in ascending order.
fn insertion_runs(inserted: Vec<Positioned>) -> Vec<(u32, Vec<MessageSummary>)> {
    let mut runs: Vec<(u32, Vec<MessageSummary>)> = Vec::new();
    for Positioned { position, row } in inserted {
        match runs.last_mut() {
            Some((first, rows)) if *first + rows.len() as u32 == position => rows.push(row),
            _ => runs.push((position, vec![row])),
        }
    }
    runs
}

pub fn passes(filter: ViewFilter, row: &MessageSummary) -> bool {
    match filter {
        ViewFilter::All => true,
        ViewFilter::Unread => !row.flags.is_seen(),
        ViewFilter::Starred => row.flags.is_flagged(),
        ViewFilter::Attachments => row.has_attachments,
    }
}

type ChangeListener = Rc<dyn Fn(Change)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageModel {
        pub(super) state: RefCell<State>,
        pub(super) mailbox: OnceCell<Rc<Mailbox>>,
        pub(super) listener: RefCell<Option<ChangeListener>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageModel {
        const NAME: &'static str = "SealwaxMessageModel";
        type Type = super::MessageModel;
        type Interfaces = (gio::ListModel, gtk::SectionModel);
    }

    impl ObjectImpl for MessageModel {}

    impl ListModelImpl for MessageModel {
        fn item_type(&self) -> glib::Type {
            MessageItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.state.borrow().shown
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.obj().item_at(position).map(Cast::upcast)
        }
    }

    impl SectionModelImpl for MessageModel {
        fn section(&self, position: u32) -> (u32, u32) {
            let state = self.state.borrow();
            if position >= state.shown {
                return (state.shown, u32::MAX);
            }
            let (_, start, end) = state.bounds.section_at(position);
            (start, end.min(state.shown))
        }
    }
}

glib::wrapper! {
    pub struct MessageModel(ObjectSubclass<imp::MessageModel>)
        @implements gio::ListModel, gtk::SectionModel;
}

impl MessageModel {
    pub fn new(mailbox: Rc<Mailbox>) -> Self {
        let model: Self = glib::Object::new();
        let _ = model.imp().mailbox.set(mailbox);
        model
    }

    /// Runs `listener` after every reset and every change to the rows.
    pub fn set_listener(&self, listener: impl Fn(Change) + 'static) {
        self.imp().listener.replace(Some(Rc::new(listener)));
    }

    fn mailbox(&self) -> &Rc<Mailbox> {
        self.imp()
            .mailbox
            .get()
            .expect("the model is built with a mailbox")
    }

    fn notify(&self, change: Change) {
        let listener = self.imp().listener.borrow().clone();
        if let Some(listener) = listener {
            listener(change);
        }
    }

    pub fn content(&self) -> Content {
        match &self.imp().state.borrow().source {
            Source::Nothing => Content::Nothing,
            Source::View(view) => Content::View(view.spec),
            Source::Search(search) => Content::Search {
                text: search.text.clone(),
                folder: search.folder,
                filter: search.filter,
            },
        }
    }

    /// Rows in the view or the results, also those GTK doesn't see yet while the list grows.
    pub fn total(&self) -> u32 {
        self.imp().state.borrow().total
    }

    /// Whether new content is on its way (the view is opening, or the first search runs).
    pub fn is_pending(&self) -> bool {
        match &self.imp().state.borrow().source {
            Source::Nothing => false,
            Source::View(view) => view.handle.is_none(),
            Source::Search(search) => !search.applied,
        }
    }

    /// Whether search results are on their way, or a change asked for them again.
    pub fn is_searching(&self) -> bool {
        matches!(
            &self.imp().state.borrow().source,
            Source::Search(search) if search.fetching || search.refresh_queued || search.stale
        )
    }

    /// Whether more search results can be fetched.
    pub fn has_more_results(&self) -> bool {
        matches!(&self.imp().state.borrow().source, Source::Search(search) if !search.exhausted)
    }

    pub fn is_growing(&self) -> bool {
        let state = self.imp().state.borrow();
        state.shown < state.total
    }

    /// The version the view's rows are at.
    pub fn version(&self) -> Option<StoreVersion> {
        match &self.imp().state.borrow().source {
            Source::View(view) if view.handle.is_some() => Some(view.version),
            _ => None,
        }
    }

    pub fn thresholds(&self) -> Thresholds {
        self.imp().state.borrow().thresholds
    }

    pub fn stats(&self) -> ModelStats {
        self.imp().state.borrow().stats
    }

    /// Rows in each section (Today, Yesterday, This week, This month, Older).
    pub fn section_counts(&self) -> [u32; 5] {
        self.imp().state.borrow().bounds.counts()
    }

    pub fn section_label(&self, position: u32) -> &'static str {
        LABELS[self.imp().state.borrow().section_at(position)]
    }

    /// The loaded row at `position`.
    pub fn row_at(&self, position: u32) -> Option<MessageSummary> {
        let state = self.imp().state.borrow();
        state
            .slots
            .get(position as usize)
            .and_then(|slot| slot.as_ref())
            .and_then(|item| item.row().clone())
    }

    /// The position of a message whose row is loaded.
    pub fn position_of(&self, id: MessageId) -> Option<u32> {
        let state = self.imp().state.borrow();
        state
            .slots
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|item| item.id() == Some(id)))
            .map(|position| position as u32)
    }

    /// How many of `ids` the model holds as loaded rows.
    pub fn held(&self, ids: &HashSet<MessageId>) -> usize {
        let state = self.imp().state.borrow();
        state
            .slots
            .iter()
            .flatten()
            .filter(|item| item.id().is_some_and(|id| ids.contains(&id)))
            .count()
    }

    fn item_at(&self, position: u32) -> Option<MessageItem> {
        let (item, pages) = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            if position >= state.shown {
                return None;
            }
            state.last_position = position;
            let item = match &state.slots[position as usize] {
                Some(item) => item.clone(),
                None => {
                    let item = MessageItem::new(state.section_at(position), None);
                    state.slots[position as usize] = Some(item.clone());
                    item
                }
            };
            let mut pages = Vec::new();
            if let Source::View(view) = &state.source
                && view.handle.is_some()
            {
                let page = position / PAGE;
                if !item.is_loaded() {
                    pages.push(page);
                }
                let within = position % PAGE;
                let neighbour = if within >= PAGE * 7 / 10 {
                    Some(page + 1)
                } else if within < PAGE * 3 / 10 && page > 0 {
                    Some(page - 1)
                } else {
                    None
                };
                if let Some(neighbour) = neighbour
                    && state.page_missing(neighbour)
                {
                    pages.push(neighbour);
                }
            }
            (item, pages)
        };
        for page in pages {
            self.request_page(page);
        }
        Some(item)
    }

    /// Asks for the page holding `position` when its row is missing (the list calls this for
    /// placeholders it shows, after rows moved under a page on its way).
    pub fn request_position(&self, position: u32) {
        let wanted = {
            let state = self.imp().state.borrow();
            position < state.total
                && !state.is_loaded(position)
                && matches!(&state.source, Source::View(view) if view.handle.is_some())
        };
        if wanted {
            self.request_page(position / PAGE);
        }
    }

    fn request_page(&self, page: u32) {
        let request = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            let generation = state.generation;
            let total = state.total;
            state.serial += 1;
            let serial = state.serial;
            match &mut state.source {
                Source::View(view)
                    if view.handle.is_some()
                        && page * PAGE < total
                        && !view.in_flight.contains_key(&page) =>
                {
                    view.in_flight.insert(page, serial);
                    state.stats.pages_requested += 1;
                    Some((view.spec, generation, serial))
                }
                _ => None,
            }
        };
        let Some((spec, generation, serial)) = request else {
            return;
        };
        let Some(reply) = self
            .mailbox()
            .store()
            .map(|store| store.page(spec, page * PAGE, PAGE))
        else {
            return;
        };
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let result = reply.recv().await;
            if let Some(model) = weak.upgrade() {
                model.page_arrived(generation, page, serial, result);
            }
        });
    }

    fn page_arrived(
        &self,
        generation: u64,
        index: u32,
        serial: u64,
        result: Result<Page, StoreError>,
    ) {
        let fills = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            if state.generation != generation {
                return;
            }
            let Some(view) = state.view_mut() else {
                return;
            };
            if view.in_flight.get(&index) == Some(&serial) {
                view.in_flight.remove(&index);
            }
            let page = match result {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!("a page of the message list could not be read: {error}");
                    return;
                }
            };
            if page.version > view.version {
                view.stash.push(Stashed::Page(page));
                state.stats.pages_stashed += 1;
                return;
            }
            if page.version < view.version {
                state.stats.pages_stale += 1;
                return;
            }
            state.stats.pages_applied += 1;
            let fills = state.fill(page.offset, page.rows);
            state.evict();
            fills
        };
        self.finish(fills);
        self.notify(Change::Rows);
    }

    fn finish(&self, fills: Fills) {
        for (item, row) in fills.rows {
            item.set_row(row);
        }
        for position in fills.replaced {
            self.items_changed(position, 1, 1);
        }
    }

    /// Shows `spec` from a view already opened with `thresholds` (the mailbox reads the first
    /// window's view ahead).
    pub fn adopt(&self, spec: ViewSpec, thresholds: Thresholds, opened: ViewOpened) {
        let generation = self.begin_view(spec);
        self.opened(generation, thresholds, Ok(opened));
    }

    fn begin_view(&self, spec: ViewSpec) -> u64 {
        let mut state = self.imp().state.borrow_mut();
        state.generation += 1;
        state.stats.opens += 1;
        state.source = Source::View(ViewState {
            spec,
            handle: None,
            version: StoreVersion::default(),
            in_flight: HashMap::new(),
            stash: Vec::new(),
            queued: Vec::new(),
        });
        state.generation
    }

    /// Shows `spec`. The rows shown so far stay until the view's first page is in.
    pub fn open(&self, spec: ViewSpec, thresholds: Thresholds) {
        let generation = self.begin_view(spec);
        let request = OpenView {
            spec,
            first_page: PAGE,
            section_starts: thresholds.starts(),
        };
        let Some(reply) = self.mailbox().store().map(|store| store.open_view(request)) else {
            self.failed(generation, "the store is not open".to_owned());
            return;
        };
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let result = reply.recv().await;
            if let Some(model) = weak.upgrade() {
                model.opened(generation, thresholds, result);
            }
        });
    }

    fn opened(
        &self,
        generation: u64,
        thresholds: Thresholds,
        result: Result<ViewOpened, StoreError>,
    ) {
        let opened = match result {
            Ok(opened) => opened,
            Err(error) => {
                self.failed(generation, error.to_string());
                return;
            }
        };
        let queued = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            if state.generation != generation {
                return;
            }
            let Source::View(view) = &mut state.source else {
                return;
            };
            view.version = opened.version;
            view.handle = Some(opened.view);
            let queued = std::mem::take(&mut view.queued);
            state.thresholds = thresholds;
            state.total = opened.total;
            state.bounds = Bounds::from_counts(&opened.section_counts, opened.total);
            state.slots = vec![None; opened.total as usize];
            state.loaded = 0;
            state.last_position = 0;
            for (position, row) in opened.rows.into_iter().enumerate() {
                let section = state.section_at(position as u32);
                state.slots[position] = Some(MessageItem::new(section, Some(row)));
                state.loaded += 1;
            }
            queued
        };
        for event in queued {
            self.apply(&event, false);
        }
        self.reset_items();
    }

    fn failed(&self, generation: u64, message: String) {
        {
            let mut state = self.imp().state.borrow_mut();
            if state.generation != generation {
                return;
            }
            tracing::warn!("the message list could not be read: {message}");
            state.source = Source::Nothing;
            state.total = 0;
            state.slots.clear();
            state.loaded = 0;
            state.bounds = Bounds::default();
        }
        self.reset_items();
        self.notify(Change::Failed);
    }

    /// Shows nothing.
    pub fn clear(&self) {
        {
            let mut state = self.imp().state.borrow_mut();
            state.generation += 1;
            state.source = Source::Nothing;
            state.total = 0;
            state.slots.clear();
            state.loaded = 0;
            state.bounds = Bounds::default();
        }
        self.reset_items();
    }

    /// Opens the shown view again, as after a `Reload` or a gap in the events.
    fn reopen(&self) {
        let spec = match &self.imp().state.borrow().source {
            Source::View(view) => view.spec,
            _ => return,
        };
        let thresholds = self.thresholds();
        self.imp().state.borrow_mut().stats.reopens += 1;
        self.open(spec, thresholds);
    }

    /// Tells GTK the content changed; it sees at most [`FIRST_ROWS`] rows until the list grows.
    fn reset_items(&self) {
        self.notify(Change::Resetting);
        let (removed, added) = {
            let mut state = self.imp().state.borrow_mut();
            let removed = state.shown;
            state.shown = state.total.min(FIRST_ROWS);
            (removed, state.shown)
        };
        self.items_changed(0, removed, added);
        self.notify(Change::Reset);
    }

    /// Shows [`GROW_STEP`] more rows, or every row once past [`GROW_UNTIL`]; false when every
    /// row is shown.
    pub fn grow(&self) -> bool {
        let (start, added, more) = {
            let mut state = self.imp().state.borrow_mut();
            if state.shown >= state.total {
                return false;
            }
            let start = state.shown;
            let next = if start + GROW_STEP >= GROW_UNTIL {
                state.total
            } else {
                (start + GROW_STEP).min(state.total)
            };
            state.shown = next;
            (start, next - start, next < state.total)
        };
        self.items_changed(start, 0, added);
        more
    }

    /// Shows every row now.
    pub fn complete_growth(&self) {
        let (start, added) = {
            let mut state = self.imp().state.borrow_mut();
            if state.shown >= state.total {
                return;
            }
            let start = state.shown;
            state.shown = state.total;
            (start, state.total - start)
        };
        self.items_changed(start, 0, added);
    }

    /// Applies a change event; true when rows were inserted (the list then keeps its top in
    /// view).
    pub fn apply_event(&self, event: &Arc<ChangeEvent>) -> bool {
        self.apply(event, true)
    }

    /// With `emit` false the rows change silently, for events that arrived while the view was
    /// opening: the reset that follows tells GTK.
    fn apply(&self, event: &Arc<ChangeEvent>, emit: bool) -> bool {
        let plan = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            match &mut state.source {
                Source::Nothing => Plan::Ignore,
                Source::Search(search) => {
                    let touched = !event.inserted.is_empty()
                        || !event.removed.is_empty()
                        || !event.changed.is_empty();
                    if touched {
                        search.stale = true;
                        Plan::Refresh
                    } else {
                        Plan::Ignore
                    }
                }
                Source::View(view) if view.handle.is_none() => {
                    view.queued.push(event.clone());
                    Plan::Ignore
                }
                Source::View(view) if event.version <= view.version => Plan::Ignore,
                Source::View(view) if event.version.0 != view.version.0 + 1 => {
                    tracing::debug!("a change event is missing; opening the list again");
                    Plan::Reopen
                }
                Source::View(view) => {
                    view.version = event.version;
                    state.stats.events_applied += 1;
                    let id = view.handle.as_ref().map(ViewHandle::id);
                    let diff = event
                        .views
                        .iter()
                        .find(|change| Some(change.view) == id)
                        .map(|change| change.diff.clone());
                    match diff {
                        Some(ViewDiff::Reload { .. }) => Plan::Reopen,
                        diff => Plan::Apply(diff),
                    }
                }
            }
        };
        match plan {
            Plan::Ignore => false,
            Plan::Reopen => {
                self.reopen();
                false
            }
            Plan::Refresh => {
                self.queue_search_refresh();
                false
            }
            Plan::Apply(diff) => {
                let inserted = match diff {
                    Some(ViewDiff::Splice {
                        removed,
                        inserted,
                        changed,
                        total,
                    }) => self.splice(&removed, inserted, changed, total, emit),
                    _ => false,
                };
                self.apply_threads(event);
                self.apply_stash();
                if emit {
                    self.notify(Change::Rows);
                }
                inserted
            }
        }
    }

    fn splice(
        &self,
        removed: &[u32],
        inserted: Vec<Positioned>,
        changed: Vec<Positioned>,
        total: u32,
        emit: bool,
    ) -> bool {
        if emit {
            self.complete_growth();
        }
        let any_inserted = !inserted.is_empty();
        for (first, count) in removal_runs(removed) {
            {
                let mut guard = self.imp().state.borrow_mut();
                let state = &mut *guard;
                if first + count > state.total {
                    drop(guard);
                    self.reopen();
                    return false;
                }
                for position in (first..first + count).rev() {
                    let section = state.section_at(position);
                    state.bounds.remove(section);
                    if let Some(item) = state.slots.remove(position as usize)
                        && item.is_loaded()
                    {
                        state.loaded -= 1;
                    }
                }
                state.total -= count;
                if emit {
                    state.shown = state.total;
                }
            }
            if emit {
                self.items_changed(first, count, 0);
            }
        }
        for (first, rows) in insertion_runs(inserted) {
            let count = rows.len() as u32;
            {
                let mut guard = self.imp().state.borrow_mut();
                let state = &mut *guard;
                if first > state.total {
                    drop(guard);
                    self.reopen();
                    return false;
                }
                for (offset, row) in rows.into_iter().enumerate() {
                    let section = state.thresholds.section_of(row.date);
                    state.bounds.insert(section);
                    state.slots.insert(
                        first as usize + offset,
                        Some(MessageItem::new(section, Some(row))),
                    );
                    state.loaded += 1;
                }
                state.total += count;
                if emit {
                    state.shown = state.total;
                }
            }
            if emit {
                self.items_changed(first, 0, count);
            }
        }
        let fills = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            if state.total != total {
                tracing::debug!("the list's row count differs from the store's; opening again");
                drop(guard);
                self.reopen();
                return false;
            }
            let mut fills = Fills::default();
            for Positioned { position, row } in changed {
                let Some(Some(item)) = state.slots.get(position as usize) else {
                    continue;
                };
                if item.id() == Some(row.id) {
                    fills.rows.push((item.clone(), row));
                } else if !item.is_loaded() {
                    state.loaded += 1;
                    fills.rows.push((item.clone(), row));
                } else {
                    let section = state.section_at(position);
                    state.slots[position as usize] = Some(MessageItem::new(section, Some(row)));
                    if emit && position < state.shown {
                        fills.replaced.push(position);
                    }
                }
            }
            fills
        };
        self.finish(fills);
        any_inserted
    }

    /// Conversations merged and their sizes, on the rows already loaded.
    fn apply_threads(&self, event: &ChangeEvent) {
        if event.merged_threads.is_empty() && event.threads.is_empty() {
            return;
        }
        let merged: HashMap<ThreadId, ThreadId> = event
            .merged_threads
            .iter()
            .map(|merge| (merge.from, merge.into))
            .collect();
        let counts: HashMap<ThreadId, u32> = event
            .threads
            .iter()
            .map(|count| (count.thread, count.count))
            .collect();
        let touched: HashSet<ThreadId> = merged.keys().chain(counts.keys()).copied().collect();
        let items: Vec<MessageItem> = {
            let state = self.imp().state.borrow();
            state
                .slots
                .iter()
                .flatten()
                .filter(|item| {
                    item.row()
                        .as_ref()
                        .is_some_and(|row| touched.contains(&row.thread))
                })
                .cloned()
                .collect()
        };
        for item in items {
            item.edit(|row| {
                let mut changed = false;
                if let Some(into) = merged.get(&row.thread) {
                    row.thread = *into;
                    changed = true;
                }
                if let Some(count) = counts.get(&row.thread)
                    && row.thread_count != *count
                {
                    row.thread_count = *count;
                    changed = true;
                }
                changed
            });
        }
    }

    /// Applies pages and section counts that waited for the events to catch up.
    fn apply_stash(&self) {
        let ready = {
            let mut guard = self.imp().state.borrow_mut();
            let Some(view) = guard.view_mut() else {
                return;
            };
            let version = view.version;
            let (ready, later): (Vec<Stashed>, Vec<Stashed>) = std::mem::take(&mut view.stash)
                .into_iter()
                .filter(|stashed| stashed.version() >= version)
                .partition(|stashed| stashed.version() == version);
            view.stash = later;
            ready
        };
        for stashed in ready {
            match stashed {
                Stashed::Page(page) => {
                    let fills = {
                        let mut state = self.imp().state.borrow_mut();
                        state.stats.pages_applied += 1;
                        let fills = state.fill(page.offset, page.rows);
                        state.evict();
                        fills
                    };
                    self.finish(fills);
                }
                Stashed::Sections {
                    thresholds,
                    counts,
                    total,
                    ..
                } => self.set_sections(thresholds, Bounds::from_counts(&counts, total)),
            }
        }
    }

    /// Regroups the sections with `thresholds`, as when the day changes.
    pub fn regroup(&self, thresholds: Thresholds) {
        enum Regroup {
            Set,
            Reopen,
            Search(Bounds),
            Count(ViewSpec, u64),
        }
        let plan = {
            let state = self.imp().state.borrow();
            match &state.source {
                Source::Nothing => Regroup::Set,
                Source::View(view) if view.handle.is_none() => Regroup::Reopen,
                Source::View(view) => Regroup::Count(view.spec, state.generation),
                Source::Search(_) => {
                    let dates = state
                        .slots
                        .iter()
                        .flatten()
                        .filter_map(|item| item.row().as_ref().map(|row| row.date))
                        .collect::<Vec<_>>();
                    Regroup::Search(Bounds::from_dates(dates, &thresholds))
                }
            }
        };
        match plan {
            Regroup::Set => self.imp().state.borrow_mut().thresholds = thresholds,
            Regroup::Reopen => {
                let spec = match &self.imp().state.borrow().source {
                    Source::View(view) => view.spec,
                    _ => return,
                };
                self.open(spec, thresholds);
            }
            Regroup::Search(bounds) => self.set_sections(thresholds, bounds),
            Regroup::Count(spec, generation) => {
                let request = OpenView {
                    spec,
                    first_page: 0,
                    section_starts: thresholds.starts(),
                };
                let Some(reply) = self.mailbox().store().map(|store| store.open_view(request))
                else {
                    return;
                };
                let weak = self.downgrade();
                glib::spawn_future_local(async move {
                    let result = reply.recv().await;
                    if let Some(model) = weak.upgrade() {
                        model.counted(generation, thresholds, result);
                    }
                });
            }
        }
    }

    fn counted(
        &self,
        generation: u64,
        thresholds: Thresholds,
        result: Result<ViewOpened, StoreError>,
    ) {
        let Ok(counted) = result else {
            return;
        };
        let apply = {
            let mut guard = self.imp().state.borrow_mut();
            if guard.generation != generation {
                return;
            }
            let Some(view) = guard.view_mut() else {
                return;
            };
            if counted.version > view.version {
                view.stash.push(Stashed::Sections {
                    version: counted.version,
                    thresholds,
                    counts: counted.section_counts,
                    total: counted.total,
                });
                None
            } else if counted.version < view.version {
                Some(None)
            } else {
                Some(Some(Bounds::from_counts(
                    &counted.section_counts,
                    counted.total,
                )))
            }
        };
        match apply {
            None => {}
            Some(None) => self.regroup(thresholds),
            Some(Some(bounds)) => self.set_sections(thresholds, bounds),
        }
    }

    fn set_sections(&self, thresholds: Thresholds, bounds: Bounds) {
        let (changes, shown) = {
            let mut state = self.imp().state.borrow_mut();
            state.thresholds = thresholds;
            state.bounds = bounds;
            let changes: Vec<(MessageItem, usize)> = state
                .slots
                .iter()
                .enumerate()
                .filter_map(|(position, slot)| {
                    slot.as_ref()
                        .map(|item| (item.clone(), bounds.section_at(position as u32).0))
                })
                .collect();
            (changes, state.shown)
        };
        for (item, section) in changes {
            item.set_section(section);
        }
        if shown > 0 {
            self.sections_changed(0, shown);
        }
        self.notify(Change::Rows);
    }

    /// Shows the newest matches of `text` in `folder` (everywhere for `None`), filtered by
    /// `filter`.
    pub fn search(
        &self,
        text: String,
        folder: Option<FolderId>,
        filter: ViewFilter,
        thresholds: Thresholds,
    ) {
        let generation = {
            let mut state = self.imp().state.borrow_mut();
            state.generation += 1;
            state.thresholds = thresholds;
            state.source = Source::Search(SearchState {
                text,
                folder,
                filter,
                limit: SEARCH_PAGE,
                rows: Vec::new(),
                exhausted: false,
                fetching: false,
                stale: false,
                refresh_queued: false,
                applied: false,
            });
            state.generation
        };
        self.fetch(generation, SEARCH_PAGE);
    }

    fn fetch(&self, generation: u64, limit: u32) {
        let query = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            if state.generation != generation {
                return;
            }
            let Source::Search(search) = &mut state.source else {
                return;
            };
            search.fetching = true;
            search.stale = false;
            search.limit = limit;
            state.stats.searches += 1;
            (search.text.clone(), search.folder)
        };
        let Some(reply) = self
            .mailbox()
            .store()
            .map(|store| store.search(&query.0, query.1, limit))
        else {
            self.failed(generation, "the store is not open".to_owned());
            return;
        };
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let result = reply.recv().await;
            if let Some(model) = weak.upgrade() {
                model.found(generation, result);
            }
        });
    }

    /// Fetches more search results, when there are more and none are on their way; the list
    /// asks as its end comes into view.
    pub fn search_more(&self) {
        let next = {
            let state = self.imp().state.borrow();
            match &state.source {
                Source::Search(search) if !search.fetching && !search.exhausted => {
                    let limit = if search.filter == ViewFilter::All {
                        search.limit + SEARCH_PAGE
                    } else {
                        search.limit * 2
                    };
                    Some((state.generation, limit.min(SEARCH_MAX)))
                }
                _ => None,
            }
        };
        if let Some((generation, limit)) = next {
            self.fetch(generation, limit);
        }
    }

    /// Runs the search shown again, as a change event does.
    pub fn search_again(&self) {
        self.queue_search_refresh();
    }

    fn queue_search_refresh(&self) {
        let generation = {
            let mut state = self.imp().state.borrow_mut();
            let generation = state.generation;
            let Source::Search(search) = &mut state.source else {
                return;
            };
            if search.refresh_queued || search.fetching {
                return;
            }
            search.refresh_queued = true;
            generation
        };
        let weak = self.downgrade();
        glib::timeout_add_local_once(SEARCH_REFRESH, move || {
            let Some(model) = weak.upgrade() else {
                return;
            };
            let limit = {
                let mut state = model.imp().state.borrow_mut();
                if state.generation != generation {
                    return;
                }
                let Source::Search(search) = &mut state.source else {
                    return;
                };
                search.refresh_queued = false;
                search.limit
            };
            model.fetch(generation, limit);
        });
    }

    fn found(&self, generation: u64, result: Result<Versioned<Vec<MessageSummary>>, StoreError>) {
        enum Update {
            Reset,
            Append(u32),
            InPlace(Vec<(MessageItem, MessageSummary)>),
        }
        let (update, more, again) = {
            let mut guard = self.imp().state.borrow_mut();
            let state = &mut *guard;
            if state.generation != generation {
                return;
            }
            let Source::Search(search) = &mut state.source else {
                return;
            };
            search.fetching = false;
            let rows = match result {
                Ok(found) => found.value,
                Err(_) => {
                    // The error could quote the full-text expression, which holds what the user
                    // typed: it isn't logged.
                    tracing::warn!("the search could not be run");
                    Vec::new()
                }
            };
            search.exhausted = (rows.len() as u32) < search.limit || search.limit >= SEARCH_MAX;
            search.rows = rows;
            let visible: Vec<MessageSummary> = search
                .rows
                .iter()
                .filter(|row| passes(search.filter, row))
                .cloned()
                .collect();
            let more = !search.exhausted && (visible.len() as u32) < SEARCH_PAGE;
            let again = search.stale;
            let first = !std::mem::replace(&mut search.applied, true);
            let old: Vec<Option<MessageId>> = state
                .slots
                .iter()
                .map(|slot| slot.as_ref().and_then(MessageItem::id))
                .collect();
            let prefix = !first
                && old.len() <= visible.len()
                && old
                    .iter()
                    .zip(&visible)
                    .all(|(id, row)| *id == Some(row.id));
            let update = if prefix && old.len() == visible.len() {
                Update::InPlace(
                    state
                        .slots
                        .iter()
                        .flatten()
                        .cloned()
                        .zip(visible.iter().cloned())
                        .collect(),
                )
            } else if prefix && state.shown == state.total {
                let start = state.total;
                for row in &visible[start as usize..] {
                    let section = state.thresholds.section_of(row.date);
                    state
                        .slots
                        .push(Some(MessageItem::new(section, Some(row.clone()))));
                }
                Update::Append(start)
            } else {
                state.slots = visible
                    .iter()
                    .map(|row| {
                        let section = state.thresholds.section_of(row.date);
                        Some(MessageItem::new(section, Some(row.clone())))
                    })
                    .collect();
                Update::Reset
            };
            state.total = visible.len() as u32;
            state.loaded = state.total;
            state.bounds =
                Bounds::from_dates(visible.iter().map(|row| row.date), &state.thresholds);
            (update, more, again)
        };
        match update {
            Update::Reset => self.reset_items(),
            Update::Append(start) => {
                let (added, shown) = {
                    let mut state = self.imp().state.borrow_mut();
                    state.shown = state.total;
                    (state.total - start, state.shown)
                };
                if added > 0 {
                    self.items_changed(start, 0, added);
                    self.sections_changed(0, shown);
                }
                self.notify(Change::Rows);
            }
            Update::InPlace(rows) => {
                for (item, row) in rows {
                    item.set_row(row);
                }
                self.notify(Change::Rows);
            }
        }
        if again {
            self.queue_search_refresh();
        } else if more {
            self.search_more();
        }
    }

    /// Forgets the loaded rows, so the next ones come from the store's readers (the perf
    /// script's cold jumps).
    pub fn drop_cache(&self) {
        let mut state = self.imp().state.borrow_mut();
        if !matches!(state.source, Source::View(_)) {
            return;
        }
        for slot in &mut state.slots {
            if slot.as_ref().is_some_and(MessageItem::is_loaded) {
                *slot = None;
            }
        }
        state.loaded = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId, MessageId};

    fn row(id: i64) -> MessageSummary {
        MessageSummary {
            id: MessageId(id),
            folder: FolderId(1),
            thread: ThreadId(id),
            sender_name: String::new(),
            sender_email: String::new(),
            subject: String::new(),
            snippet: String::new(),
            date: id,
            flags: Flags::NONE,
            has_attachments: false,
            thread_count: 1,
            subscription: false,
        }
    }

    #[test]
    fn removals_and_insertions_coalesce_into_runs() {
        assert_eq!(removal_runs(&[9, 8, 7, 3, 1, 0]), [(7, 3), (3, 1), (0, 2)]);
        assert_eq!(removal_runs(&[]), []);
        let inserted = [0, 1, 2, 10, 12]
            .into_iter()
            .map(|position| Positioned {
                position,
                row: row(i64::from(position)),
            })
            .collect();
        let runs: Vec<(u32, usize)> = insertion_runs(inserted)
            .into_iter()
            .map(|(first, rows)| (first, rows.len()))
            .collect();
        assert_eq!(runs, [(0, 3), (10, 1), (12, 1)]);
    }

    #[test]
    fn filters_pass_the_rows_their_chip_names() {
        let mut unread = row(1);
        unread.has_attachments = true;
        let mut starred = row(2);
        starred.flags = Flags::SEEN | Flags::FLAGGED;
        assert!(passes(ViewFilter::Unread, &unread));
        assert!(!passes(ViewFilter::Unread, &starred));
        assert!(passes(ViewFilter::Starred, &starred));
        assert!(passes(ViewFilter::Attachments, &unread));
        assert!(!passes(ViewFilter::Attachments, &starred));
        assert!(passes(ViewFilter::All, &starred));
    }
}
