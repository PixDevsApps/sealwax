//! The command palette (F1, Ctrl+Shift+P): a search entry over the commands the window can run
//! now, each with its keys, ranked by Stet's matcher. Typing filters, Up and Down move, Enter
//! runs the selected command and Escape closes; a click runs a row.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::palette;
use std::cell::RefCell;
use std::rc::Rc;

/// One command the palette offers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    id: ActionId,
    title: String,
    category: &'static str,
    keys: String,
}

impl Item {
    fn search_text(&self) -> String {
        format!("{} {}", self.title, self.category)
    }

    /// What a screen reader says for the row: the command, its category, its keys.
    fn spoken(&self) -> String {
        [self.title.as_str(), self.category, self.keys.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// What the signal handlers share. It holds the widgets weakly, so the dialog never keeps
/// itself alive through its own handlers.
struct Inner {
    dialog: glib::WeakRef<adw::Dialog>,
    entry: glib::WeakRef<gtk::SearchEntry>,
    list: glib::WeakRef<gtk::ListBox>,
    scrolled: glib::WeakRef<gtk::ScrolledWindow>,
    window: glib::WeakRef<gtk::ApplicationWindow>,
    items: Vec<Item>,
    shown: RefCell<Vec<Item>>,
}

/// An open palette; [`present`] makes one. Each opening builds a new palette, so it always
/// shows the commands enabled at that moment and the current keys.
#[derive(Clone)]
pub struct Palette {
    dialog: adw::Dialog,
    inner: Rc<Inner>,
}

/// The commands of `window` the palette lists: in the palette and enabled now.
fn items(window: &gtk::ApplicationWindow, keymap: &Keymap, titles: &Titles) -> Vec<Item> {
    ActionId::ALL
        .into_iter()
        .filter(|id| id.spec().palette && is_enabled(window, *id))
        .map(|id| Item {
            id,
            title: titles(id).unwrap_or_else(|| id.title().to_owned()),
            category: id.category().label(),
            keys: keymap.display(id),
        })
        .collect()
}

fn is_enabled(window: &gtk::ApplicationWindow, id: ActionId) -> bool {
    match id.scope() {
        Scope::App => window
            .application()
            .is_some_and(|app| app.is_action_enabled(id.name())),
        _ => window.is_action_enabled(id.name()),
    }
}

/// A command's title where it differs from the registry's: Move to Junk is Not Junk in a
/// Junk folder.
pub type Titles = dyn Fn(ActionId) -> Option<String>;

impl Palette {
    pub fn new(window: &impl IsA<gtk::ApplicationWindow>, keymap: &Keymap) -> Self {
        Self::titled(window, keymap, &|_| None)
    }

    /// A palette whose commands take their titles from `titles` where it has one.
    pub fn titled(
        window: &impl IsA<gtk::ApplicationWindow>,
        keymap: &Keymap,
        titles: &Titles,
    ) -> Self {
        let window = window.as_ref();
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Type a command")
            .search_delay(0)
            .hexpand(true)
            .css_classes(["sw-palette-search"])
            .build();
        entry.set_widget_name("palette-search");
        entry.update_property(&[gtk::accessible::Property::Label("Command palette")]);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .css_classes(["sw-palette-list"])
            .build();
        list.set_widget_name("palette-list");
        list.update_property(&[gtk::accessible::Property::Label("Commands")]);
        let empty = gtk::Label::builder()
            .label("No matching command")
            .css_classes(["sw-palette-empty", "sw-secondary"])
            .build();
        list.set_placeholder(Some(&empty));
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let header = adw::HeaderBar::builder().title_widget(&entry).build();
        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.set_content(Some(&scrolled));
        let dialog = adw::Dialog::builder()
            .title("Command Palette")
            .content_width(560)
            .content_height(460)
            .child(&content)
            .focus_widget(&entry)
            .css_classes(["sw-palette"])
            .build();
        let inner = Rc::new(Inner {
            dialog: dialog.downgrade(),
            entry: entry.downgrade(),
            list: list.downgrade(),
            scrolled: scrolled.downgrade(),
            window: window.downgrade(),
            items: items(window, keymap, titles),
            shown: RefCell::new(Vec::new()),
        });
        connect(&inner, &entry, &list);
        inner.refresh();
        Self { dialog, inner }
    }

    /// Opens over the window with the cursor in the search entry.
    pub fn present(&self) {
        let Some(window) = self.inner.window.upgrade() else {
            return;
        };
        self.dialog.present(Some(&window));
        if let Some(entry) = self.inner.entry.upgrade() {
            entry.grab_focus();
        }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    /// Types `query` into the search entry, as a user would.
    pub fn set_query(&self, query: &str) {
        if let Some(entry) = self.inner.entry.upgrade() {
            entry.set_text(query);
        }
        self.inner.refresh();
    }

    /// The commands shown, best match first.
    pub fn shown(&self) -> Vec<ActionId> {
        self.inner
            .shown
            .borrow()
            .iter()
            .map(|item| item.id)
            .collect()
    }

    /// The search entry: typing, Enter (`activate`) and Escape (`stop-search`) arrive there.
    pub fn search_entry(&self) -> Option<gtk::SearchEntry> {
        self.inner.entry.upgrade()
    }

    /// The command Enter would run.
    pub fn selected(&self) -> Option<ActionId> {
        self.inner.selected()
    }

    /// Runs the selected command, as Enter does, and returns it.
    pub fn activate_selected(&self) -> Option<ActionId> {
        let id = self.inner.selected()?;
        self.inner.run(id);
        Some(id)
    }

    /// Moves the selection as Down (positive) or Up (negative) does.
    pub fn move_selection(&self, step: i32) {
        self.inner.move_selection(step);
    }
}

/// Builds a palette for `window` and opens it.
pub fn present(window: &impl IsA<gtk::ApplicationWindow>, keymap: &Keymap) -> Palette {
    present_titled(window, keymap, &|_| None)
}

/// Builds a palette for `window` whose commands take their titles from `titles`, and opens it.
pub fn present_titled(
    window: &impl IsA<gtk::ApplicationWindow>,
    keymap: &Keymap,
    titles: &Titles,
) -> Palette {
    let palette = Palette::titled(window, keymap, titles);
    palette.present();
    palette
}

/// The handlers keep `inner` alive while the dialog exists; it holds the widgets weakly, so
/// nothing keeps the dialog alive once it is closed and dropped.
fn connect(inner: &Rc<Inner>, entry: &gtk::SearchEntry, list: &gtk::ListBox) {
    entry.connect_search_changed(glib::clone!(
        #[strong]
        inner,
        move |_| inner.refresh()
    ));
    entry.connect_activate(glib::clone!(
        #[strong]
        inner,
        move |_| {
            if let Some(id) = inner.selected() {
                inner.run(id);
            }
        }
    ));
    entry.connect_stop_search(glib::clone!(
        #[strong]
        inner,
        move |_| {
            if let Some(dialog) = inner.dialog.upgrade() {
                dialog.close();
            }
        }
    ));
    list.connect_row_activated(glib::clone!(
        #[strong]
        inner,
        move |_, row| {
            let id = usize::try_from(row.index())
                .ok()
                .and_then(|index| inner.shown.borrow().get(index).map(|item| item.id));
            if let Some(id) = id {
                inner.run(id);
            }
        }
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[strong]
        inner,
        move |_, key, _, _| {
            let step = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                gdk::Key::Page_Down => 8,
                gdk::Key::Page_Up => -8,
                _ => return glib::Propagation::Proceed,
            };
            inner.move_selection(step);
            glib::Propagation::Stop
        }
    ));
    entry.add_controller(keys);
}

impl Inner {
    fn refresh(&self) {
        let (Some(entry), Some(list)) = (self.entry.upgrade(), self.list.upgrade()) else {
            return;
        };
        let query = entry.text();
        let texts: Vec<String> = self.items.iter().map(Item::search_text).collect();
        let shown: Vec<Item> = palette::rank(&query, texts.iter().map(String::as_str))
            .into_iter()
            .map(|index| self.items[index].clone())
            .collect();
        list.remove_all();
        for item in &shown {
            list.append(&row(item));
        }
        if let Some(first) = list.row_at_index(0) {
            list.select_row(Some(&first));
        }
        self.shown.replace(shown);
    }

    fn selected(&self) -> Option<ActionId> {
        let row = self.list.upgrade()?.selected_row()?;
        let index = usize::try_from(row.index()).ok()?;
        self.shown.borrow().get(index).map(|item| item.id)
    }

    fn move_selection(&self, step: i32) {
        let Some(list) = self.list.upgrade() else {
            return;
        };
        let count = i32::try_from(self.shown.borrow().len()).unwrap_or(i32::MAX);
        if count == 0 {
            return;
        }
        let current = list.selected_row().map_or(-1, |row| row.index());
        let next = (current + step).clamp(0, count - 1);
        if let Some(row) = list.row_at_index(next) {
            list.select_row(Some(&row));
            self.scroll_to(&list, &row);
            // The focus stays in the entry, so a screen reader hears the row this way.
            if let (Some(entry), Some(item)) = (
                self.entry.upgrade(),
                usize::try_from(next)
                    .ok()
                    .and_then(|index| self.shown.borrow().get(index).cloned()),
            ) {
                crate::accessibility::announce(&entry, &item.spoken());
            }
        }
    }

    /// Scrolls just enough to show `row`, leaving the keyboard focus in the entry.
    fn scroll_to(&self, list: &gtk::ListBox, row: &gtk::ListBoxRow) {
        let Some(scrolled) = self.scrolled.upgrade() else {
            return;
        };
        let Some(bounds) = row.compute_bounds(list) else {
            return;
        };
        let adjustment = scrolled.vadjustment();
        let (top, bottom) = (
            f64::from(bounds.y()),
            f64::from(bounds.y() + bounds.height()),
        );
        if top < adjustment.value() {
            adjustment.set_value(top);
        } else if bottom > adjustment.value() + adjustment.page_size() {
            adjustment.set_value(bottom - adjustment.page_size());
        }
    }

    /// Closes the palette, then runs `id` on the window, so a command that opens a dialog
    /// opens it over the window.
    fn run(&self, id: ActionId) {
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.close();
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        if let Err(error) = WidgetExt::activate_action(&window, &id.detailed_name(), None) {
            tracing::warn!(action = id.name(), %error, "the palette could not run a command");
        }
    }
}

fn row(item: &Item) -> gtk::ListBoxRow {
    let title = gtk::Label::builder()
        .label(&item.title)
        .xalign(0.0)
        .css_classes(["sw-palette-title"])
        .build();
    let category = gtk::Label::builder()
        .label(item.category)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["sw-palette-category", "sw-secondary"])
        .build();
    let keys = gtk::Label::builder()
        .label(&item.keys)
        .xalign(1.0)
        .css_classes(["sw-palette-keys", "sw-secondary"])
        .build();
    // DESIGN.md's control padding, 10 × 6 px; the theme's CSS can change it.
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(10)
        .margin_end(10)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    content.append(&title);
    content.append(&category);
    content.append(&keys);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .css_classes(["sw-palette-row"])
        .build();
    // The focus stays in the search entry, where Up and Down move the chosen row and a screen
    // reader hears it: Tab doesn't walk every row.
    row.set_focusable(false);
    // A screen reader reads the row as one.
    row.update_property(&[gtk::accessible::Property::Label(&item.spoken())]);
    row
}
