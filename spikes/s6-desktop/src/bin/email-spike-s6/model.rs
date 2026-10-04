//! The lazy, sectioned list model from S3's `s3-list` (pages from the reader thread, placeholder
//! items filled in place), trimmed to what startup needs, plus a first-frame limit: the model can
//! report only the first rows until the first frame is painted, so GTK builds fewer row widgets
//! in that frame.

use crate::store::ReadRequest;
use email_spike_s3::sections::Bounds;
use email_spike_s3::store::{ListRow, Page, Snapshot, View};
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
        const NAME: &'static str = "S6MessageItem";
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

pub struct State {
    view: View,
    version: i64,
    n_items: u32,
    total: u32,
    bounds: Bounds,
    slots: Vec<Option<MessageItem>>,
    pages: HashMap<u32, PageState>,
    clock: u64,
}

impl State {
    fn empty() -> Self {
        Self {
            view: View::All,
            version: -1,
            n_items: 0,
            total: 0,
            bounds: Bounds::from_counts([0; 4], 0),
            slots: Vec::new(),
            pages: HashMap::new(),
            clock: 0,
        }
    }

    fn section_at(&self, position: u32) -> u8 {
        self.bounds.section_at(position).0
    }

    fn page_range(&self, page: u32) -> std::ops::Range<usize> {
        let start = (page * PAGE) as usize;
        start..((page + 1) * PAGE).min(self.total) as usize
    }

    fn wants(&mut self, page: u32) -> bool {
        if page * PAGE >= self.total || self.pages.contains_key(&page) {
            return false;
        }
        self.pages.insert(page, PageState::Loading);
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
                state: RefCell::new(State::empty()),
                requests: OnceCell::new(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageList {
        const NAME: &'static str = "S6MessageList";
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
            let state = self.state.borrow();
            if position >= state.n_items {
                return (state.n_items, u32::MAX);
            }
            let (_, start, end) = state.bounds.section_at(position);
            (start, end.min(state.n_items))
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

    /// Replaces the view with a snapshot. With `limit`, only that many rows are visible until
    /// `expand` is called.
    pub fn reset(&self, view: View, snapshot: Snapshot, limit: Option<u32>) {
        let (removed, added) = {
            let mut state = self.imp().state.borrow_mut();
            let removed = state.n_items;
            *state = State::empty();
            state.view = view;
            state.version = snapshot.version;
            state.total = snapshot.total;
            state.n_items = limit.map_or(snapshot.total, |limit| limit.min(snapshot.total));
            state.bounds = snapshot.bounds;
            state.slots = vec![None; snapshot.total as usize];
            let loaded = snapshot.rows.len() as u32;
            for (index, row) in snapshot.rows.into_iter().enumerate() {
                let section = state.section_at(index as u32);
                state.slots[index] = Some(MessageItem::new(section, Some(row)));
            }
            for page in 0..loaded / PAGE {
                state.pages.insert(page, PageState::Loaded { used: 0 });
            }
            (removed, state.n_items)
        };
        self.items_changed(0, removed, added);
    }

    /// Shows every row after a limited first frame.
    pub fn expand(&self) {
        let (shown, total) = {
            let mut state = self.imp().state.borrow_mut();
            let shown = state.n_items;
            state.n_items = state.total;
            (shown, state.total)
        };
        if total > shown {
            self.items_changed(shown, 0, total - shown);
        }
    }

    pub fn apply_page(&self, view: View, page: Page) {
        let mut fills = Vec::new();
        {
            let mut state = self.imp().state.borrow_mut();
            if view != state.view || page.version != state.version {
                return;
            }
            let index = page.offset / PAGE;
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
                    }
                }
            }
            let used = state.clock;
            state.pages.insert(index, PageState::Loaded { used });
        }
        for (item, row) in fills {
            item.fill(row);
        }
    }

    /// The position of a loaded message, if its row is in memory.
    pub fn position_of(&self, id: i64) -> Option<u32> {
        let state = self.imp().state.borrow();
        state
            .slots
            .iter()
            .position(|slot| {
                slot.as_ref()
                    .is_some_and(|item| item.row().as_ref().is_some_and(|row| row.id == id))
            })
            .map(|position| position as u32)
    }

    pub fn total(&self) -> u32 {
        self.imp().state.borrow().total
    }
}
