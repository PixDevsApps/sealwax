//! A lazy `gio::ListModel` + `gtk::SectionModel` over a store view. Rows arrive in pages from
//! the reader threads; until then a position holds a placeholder item that is filled in place
//! (`notify::loaded`), so selection and scroll position never see a reload. Pages carry the
//! store version their snapshot saw and are only applied when it matches the version of the
//! last change the model applied.

use crate::worker::ReadRequest;
use email_spike_s3::sections::{Bounds, Thresholds};
use email_spike_s3::store::{InsertDiff, ListRow, Page, Snapshot, View};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{gio, glib::Properties};
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;

pub const PAGE: u32 = 100;
const MAX_PAGES: usize = 40;

mod item_imp {
    use super::*;

    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::MessageItem)]
    pub struct MessageItem {
        pub row: RefCell<Option<ListRow>>,
        #[property(get = Self::is_loaded)]
        loaded: std::marker::PhantomData<bool>,
        pub section: Cell<u8>,
    }

    impl MessageItem {
        fn is_loaded(&self) -> bool {
            self.row.borrow().is_some()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageItem {
        const NAME: &'static str = "S3MessageItem";
        type Type = super::MessageItem;
    }

    #[glib::derived_properties]
    impl ObjectImpl for MessageItem {}
}

glib::wrapper! {
    pub struct MessageItem(ObjectSubclass<item_imp::MessageItem>);
}

impl MessageItem {
    pub fn new(section: u8, row: Option<ListRow>) -> Self {
        let item: Self = glib::Object::new();
        item.imp().section.set(section);
        *item.imp().row.borrow_mut() = row;
        item
    }

    pub fn row(&self) -> Ref<'_, Option<ListRow>> {
        self.imp().row.borrow()
    }

    pub fn section(&self) -> u8 {
        self.imp().section.get()
    }

    pub fn id(&self) -> Option<i64> {
        self.imp().row.borrow().as_ref().map(|row| row.id)
    }

    fn fill(&self, row: ListRow) {
        *self.imp().row.borrow_mut() = Some(row);
        self.notify_loaded();
    }
}

#[derive(Clone, Copy, Debug)]
enum PageState {
    Loading,
    Loaded { used: u64 },
}

#[derive(Default, Clone, Copy, Debug)]
pub struct ModelStats {
    pub requested: u64,
    pub applied: u64,
    pub stale: u64,
    pub stashed: u64,
    pub evicted: u64,
    pub items_created: u64,
}

pub struct State {
    view: View,
    version: i64,
    n_items: u32,
    bounds: Bounds,
    thresholds: Thresholds,
    slots: Vec<Option<MessageItem>>,
    pages: HashMap<u32, PageState>,
    stash: Vec<Page>,
    clock: u64,
    stats: ModelStats,
}

impl State {
    fn empty(view: View, thresholds: Thresholds) -> Self {
        Self {
            view,
            version: -1,
            n_items: 0,
            bounds: Bounds::from_counts([0; 4], 0),
            thresholds,
            slots: Vec::new(),
            pages: HashMap::new(),
            stash: Vec::new(),
            clock: 0,
            stats: ModelStats::default(),
        }
    }

    fn section_at(&self, position: u32) -> u8 {
        self.bounds.section_at(position).0
    }

    fn page_range(&self, page: u32) -> std::ops::Range<usize> {
        let start = (page * PAGE) as usize;
        start..((page + 1) * PAGE).min(self.n_items) as usize
    }

    fn wants(&mut self, page: u32) -> bool {
        if page * PAGE >= self.n_items || self.pages.contains_key(&page) {
            return false;
        }
        self.pages.insert(page, PageState::Loading);
        self.stats.requested += 1;
        true
    }

    fn evict(&mut self) {
        if self.pages.len() <= MAX_PAGES {
            return;
        }
        let mut loaded: Vec<(u64, u32)> = self
            .pages
            .iter()
            .filter_map(|(page, state)| match state {
                PageState::Loaded { used } => Some((*used, *page)),
                PageState::Loading => None,
            })
            .collect();
        loaded.sort_unstable();
        let excess = self.pages.len() - MAX_PAGES;
        for (_, page) in loaded.into_iter().take(excess) {
            self.pages.remove(&page);
            let range = self.page_range(page);
            for slot in &mut self.slots[range] {
                *slot = None;
            }
            self.stats.evicted += 1;
        }
    }

    /// After positions shift, a page is loaded again only if all its slots hold loaded rows.
    fn rebuild_pages(&mut self) {
        let clock = self.clock;
        let pages = self.n_items.div_ceil(PAGE);
        self.pages
            .retain(|_, state| matches!(state, PageState::Loading));
        for page in 0..pages {
            if self.pages.contains_key(&page) {
                continue;
            }
            let range = self.page_range(page);
            let full = self.slots[range].iter().all(|slot| {
                slot.as_ref()
                    .is_some_and(|item| item.imp().row.borrow().is_some())
            });
            if full {
                self.pages.insert(page, PageState::Loaded { used: clock });
            }
        }
    }
}

mod list_imp {
    use super::*;
    use std::cell::OnceCell;

    pub struct MessageList {
        pub state: RefCell<State>,
        pub requests: OnceCell<async_channel::Sender<ReadRequest>>,
    }

    impl Default for MessageList {
        fn default() -> Self {
            Self {
                state: RefCell::new(State::empty(View::All, Thresholds::new(0, 0, 0, 0))),
                requests: OnceCell::new(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageList {
        const NAME: &'static str = "S3MessageList";
        type Type = super::MessageList;
        type Interfaces = (gio::ListModel, gtk4::SectionModel);
    }

    impl ObjectImpl for MessageList {}

    impl ListModelImpl for MessageList {
        fn item_type(&self) -> glib::Type {
            MessageItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.state.borrow().n_items
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.obj().item_at(position).map(Cast::upcast)
        }
    }

    impl SectionModelImpl for MessageList {
        fn section(&self, position: u32) -> (u32, u32) {
            let (_, start, end) = self.state.borrow().bounds.section_at(position);
            (start, end)
        }
    }
}

glib::wrapper! {
    pub struct MessageList(ObjectSubclass<list_imp::MessageList>)
        @implements gio::ListModel, gtk4::SectionModel;
}

impl MessageList {
    pub fn new(requests: async_channel::Sender<ReadRequest>) -> Self {
        let list: Self = glib::Object::new();
        let _ = list.imp().requests.set(requests);
        list
    }

    fn request_pages(&self, view: View, pages: &[u32]) {
        let Some(requests) = self.imp().requests.get() else {
            return;
        };
        for page in pages {
            let _ = requests.try_send(ReadRequest::Page {
                view,
                offset: page * PAGE,
                limit: PAGE,
            });
        }
    }

    fn item_at(&self, position: u32) -> Option<MessageItem> {
        let (item, view, pages) = {
            let mut state = self.imp().state.borrow_mut();
            if position >= state.n_items {
                return None;
            }
            state.clock += 1;
            let clock = state.clock;
            let page = position / PAGE;
            let mut pages = Vec::new();
            if state.slots[position as usize].is_none() {
                let section = state.section_at(position);
                state.slots[position as usize] = Some(MessageItem::new(section, None));
                state.stats.items_created += 1;
            }
            match state.pages.get_mut(&page) {
                Some(PageState::Loaded { used }) => *used = clock,
                Some(PageState::Loading) => {}
                None => {
                    if state.wants(page) {
                        pages.push(page);
                    }
                }
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
                && state.wants(neighbour)
            {
                pages.push(neighbour);
            }
            state.evict();
            (state.slots[position as usize].clone(), state.view, pages)
        };
        self.request_pages(view, &pages);
        item
    }

    /// Replaces the whole view with a fresh snapshot.
    pub fn reset(&self, view: View, thresholds: Thresholds, snapshot: Snapshot) {
        let (removed, added) = {
            let mut state = self.imp().state.borrow_mut();
            let removed = state.n_items;
            let stats = state.stats;
            *state = State::empty(view, thresholds);
            state.stats = stats;
            state.version = snapshot.version;
            state.n_items = snapshot.total;
            state.bounds = snapshot.bounds;
            state.slots = vec![None; snapshot.total as usize];
            for (index, row) in snapshot.rows.into_iter().enumerate() {
                let section = state.section_at(index as u32);
                state.slots[index] = Some(MessageItem::new(section, Some(row)));
                state.stats.items_created += 1;
            }
            if snapshot.total > 0 {
                state.pages.insert(0, PageState::Loaded { used: 0 });
            }
            (removed, snapshot.total)
        };
        self.items_changed(0, removed, added);
    }

    pub fn apply_page(&self, view: View, page: Page) {
        let mut fills = Vec::new();
        let mut retry = Vec::new();
        {
            let mut state = self.imp().state.borrow_mut();
            if view != state.view {
                return;
            }
            let index = page.offset / PAGE;
            if page.version > state.version {
                state.stats.stashed += 1;
                state.stash.push(page);
                return;
            }
            if page.version < state.version {
                state.stats.stale += 1;
                state.pages.remove(&index);
                let range = state.page_range(index);
                let missing = state.slots[range].iter().any(|slot| {
                    slot.as_ref()
                        .is_some_and(|item| item.imp().row.borrow().is_none())
                });
                if missing && state.wants(index) {
                    retry.push(index);
                }
            } else {
                state.stats.applied += 1;
                for (offset, row) in page.rows.into_iter().enumerate() {
                    let position = page.offset as usize + offset;
                    if position >= state.slots.len() {
                        break;
                    }
                    match &state.slots[position] {
                        Some(item) if item.imp().row.borrow().is_none() => {
                            fills.push((item.clone(), row));
                        }
                        Some(_) => {}
                        None => {
                            let section = state.section_at(position as u32);
                            state.slots[position] = Some(MessageItem::new(section, Some(row)));
                            state.stats.items_created += 1;
                        }
                    }
                }
                let used = state.clock;
                state.pages.insert(index, PageState::Loaded { used });
            }
        }
        for (item, row) in fills {
            item.fill(row);
        }
        let view = self.view();
        self.request_pages(view, &retry);
    }

    /// Applies an insert diff; returns its position in this view, if it belongs here.
    pub fn apply_insert(&self, diff: InsertDiff) -> Option<u32> {
        let position = {
            let mut state = self.imp().state.borrow_mut();
            if diff.version != state.version + 1 {
                eprintln!(
                    "s3-list: version gap (model {}, diff {}); the view needs a reload",
                    state.version, diff.version
                );
            }
            state.version = diff.version;
            let view = state.view;
            let position = diff
                .positions
                .iter()
                .find_map(|(v, position)| (*v == view).then_some(*position))?;
            let section = state.thresholds.section_of(diff.row.date);
            state.bounds.insert(section);
            state.n_items += 1;
            state.slots.insert(
                position as usize,
                Some(MessageItem::new(section, Some(diff.row))),
            );
            state.stats.items_created += 1;
            state.rebuild_pages();
            position
        };
        self.items_changed(position, 0, 1);
        let stashed: Vec<Page> = {
            let mut state = self.imp().state.borrow_mut();
            std::mem::take(&mut state.stash)
        };
        let view = self.view();
        for page in stashed {
            self.apply_page(view, page);
        }
        Some(position)
    }

    /// Forgets every loaded page, so the next jump is served cold from the readers.
    pub fn drop_cache(&self) {
        let mut state = self.imp().state.borrow_mut();
        state
            .pages
            .retain(|_, page| matches!(page, PageState::Loading));
        let loading: Vec<u32> = state.pages.keys().copied().collect();
        for (index, slot) in state.slots.iter_mut().enumerate() {
            if !loading.contains(&(index as u32 / PAGE)) {
                *slot = None;
            }
        }
    }

    pub fn view(&self) -> View {
        self.imp().state.borrow().view
    }

    pub fn len(&self) -> u32 {
        self.imp().state.borrow().n_items
    }

    pub fn stats(&self) -> ModelStats {
        self.imp().state.borrow().stats
    }

    pub fn cached_pages(&self) -> usize {
        self.imp().state.borrow().pages.len()
    }

    pub fn thresholds(&self) -> Thresholds {
        self.imp().state.borrow().thresholds
    }
}
