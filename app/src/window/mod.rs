//! The main window (ML-021): folders, the message list and the reader in an adaptive three-pane
//! layout (DESIGN.md "Layout"), built from two nested `adw::NavigationSplitView`s. Breakpoints
//! switch between the full sidebar (≥ 1280 px), a rail of icons (900–1280 px) and one pane at a
//! time with back navigation (< 900 px). Toggle Sidebar hides the folders pane in the wider two
//! (the owner's word, 2026-10-21) and shows the folders page in the narrow one. On Hyprland there is no title bar and there are no
//! window controls; elsewhere the pane bars carry the controls at the window's edges and drag
//! the window.
//!
//! The `nav` action group drives the navigation: `nav.folder` (the folder shown, by key),
//! `nav.pane` (`folders`, `list` or `reader`: the page shown when one pane fits), `nav.more`
//! (opens or closes an account's "More" folders) and `nav.collapse` (folds an account's folders
//! away under its heading, or opens them).

pub mod banner;
pub mod dock;
pub mod empty;
pub mod panes;
pub mod resize;
pub mod sidebar;
pub mod tooltips;

use banner::Banner;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use panes::{PaneBar, ReaderColumn, classed, icon_button, menu_button, named, title_block};
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::layout::{LIST_WIDTHS, SIDEBAR_WIDTHS};
use sidebar::{Sidebar, compose_button};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tooltips::Tooltips;

pub use banner::{Attention, BannerAction};
pub use panes::READER_COLUMN;
pub use sidebar::{
    ALL_INBOXES, AccountStatus, FolderRole, SidebarAccount, SidebarFolder, SidebarModel,
};

/// Registry actions the window's buttons are bound to (ML-022). A button whose action is not
/// installed stays insensitive: compose until the composer exists (M6).
pub const COMPOSE_ACTION: &str = "win.compose";
pub const OPEN_CONFIG_ACTION: &str = "app.preferences";
/// The welcome page's "Add an account": the setup assistant (ML-033).
pub const ADD_ACCOUNT_ACTION: &str = "win.add-account";

/// Pane widths in px (DESIGN.md "Layout").
pub const SIDEBAR_WIDTH: f64 = 220.0;
pub const RAIL_WIDTH: f64 = 56.0;
pub const LIST_WIDTH: f64 = 380.0;
/// The least room a dragged border leaves the reader, in px; more when its toolbar needs it.
pub const READER_MIN_WIDTH: f64 = 360.0;
/// Narrower windows show the rail instead of the sidebar…
pub const WIDE_WIDTH: f64 = 1280.0;
/// …and narrower still, one pane at a time.
pub const RAIL_MIN_WIDTH: f64 = 900.0;
/// One pane still fits (GNOME's smallest window, 360×294).
pub const MIN_SIZE: (i32, i32) = (360, 294);
pub const DEFAULT_SIZE: (i32, i32) = (1280, 800);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Wide,
    Rail,
    Narrow,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wide => "wide",
            Self::Rail => "rail",
            Self::Narrow => "narrow",
        }
    }
}

/// The page shown while one pane fills the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Folders,
    List,
    Reader,
}

impl Pane {
    pub fn name(self) -> &'static str {
        match self {
            Self::Folders => "folders",
            Self::List => "list",
            Self::Reader => "reader",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Folders, Self::List, Self::Reader]
            .into_iter()
            .find(|pane| pane.name() == name)
    }
}

/// Hyprland tiles windows and draws no title bars, so Sealwax shows none either (DESIGN.md
/// "Layout"); detected as in Stet.
pub fn tiling_desktop() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        || std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktop| desktop.contains("Hyprland"))
}

type FolderHandler = Box<dyn Fn(&Window, &str)>;
type WidthsHandler = Rc<dyn Fn(f64, f64)>;
type ShownHandler = Rc<dyn Fn(bool)>;

#[derive(Clone)]
pub struct Window {
    inner: Rc<Inner>,
}

struct Inner {
    window: adw::ApplicationWindow,
    /// Folders | mail.
    panes: adw::NavigationSplitView,
    /// List | reader.
    mail: adw::NavigationSplitView,
    rail: adw::Breakpoint,
    narrow: adw::Breakpoint,
    sidebar: Rc<Sidebar>,
    /// `welcome` (no account) or `mail`.
    content: gtk::Stack,
    list_pane: adw::ToolbarView,
    list_title: gtk::Label,
    list_subtitle: gtk::Label,
    list_slot: adw::Bin,
    list_menu: gtk::MenuButton,
    /// "New" in the list's bar, while the sidebar's isn't shown.
    list_compose: gtk::Button,
    welcome_menu: gtk::MenuButton,
    reader_pane: adw::ToolbarView,
    reader_toolbar: adw::Bin,
    /// `empty`, `message` or `batch`.
    reader: gtk::Stack,
    column: ReaderColumn,
    /// The reader while several messages are selected.
    batch: empty::BatchPage,
    welcome: adw::ToolbarView,
    /// "or press Ctrl+Shift+P / F1 for every command", with the keys in use.
    welcome_hint: gtk::Label,
    /// The most important account or desktop state (ML-032, ML-048), over every pane.
    banner: Banner,
    /// The composer's cards over the panes.
    dock: dock::Dock,
    toasts: adw::ToastOverlay,
    folder: gio::SimpleAction,
    pane: gio::SimpleAction,
    tooltips: Rc<Tooltips>,
    /// Widgets the self-test finds by name.
    named: Vec<gtk::Widget>,
    folder_handlers: RefCell<Vec<FolderHandler>>,
    /// The widths the sidebar and the message list were dragged to (DESIGN.md "Layout").
    sidebar_width: Cell<f64>,
    list_width: Cell<f64>,
    /// The rail shows: the folders pane is the rail's width, whatever was dragged.
    rail_on: Cell<bool>,
    /// One pane at a time.
    narrow_on: Cell<bool>,
    /// Toggle Sidebar hid the folders pane (in the wider layouts).
    sidebar_hidden: Cell<bool>,
    /// Told whether the folders show whenever that may have changed.
    sidebar_shown_changed: RefCell<Option<ShownHandler>>,
    /// The drag handles on the left edges of the mail area and the reader.
    sidebar_handle: gtk::Box,
    list_handle: gtk::Box,
    /// Told the widths when a drag ends or a double click resets one.
    widths_changed: RefCell<Option<WidthsHandler>>,
}

impl Window {
    /// `tiling`: no title bar and no window controls (Hyprland, [`tiling_desktop`]).
    pub fn new(app: &adw::Application, tiling: bool) -> Self {
        let tooltips = Rc::new(Tooltips::default());
        let sidebar = Sidebar::new(tiling, &tooltips);

        let list_bar = PaneBar::new(
            tiling,
            Some((
                named(
                    icon_button(&["sidebar-show-symbolic"], "Folders"),
                    "list-back",
                ),
                Pane::Folders.name(),
            )),
            &[],
        );
        let list_title = named(gtk::Label::new(None), "list-title");
        let list_subtitle = named(gtk::Label::new(None), "list-subtitle");
        list_bar
            .content
            .append(&title_block(&list_title, &list_subtitle));
        let (list_compose, _) = compose_button("New");
        let list_compose = named(list_compose, "list-compose");
        list_compose.set_valign(gtk::Align::Center);
        list_compose.set_visible(false);
        tooltips.add(&list_compose, "New message", ActionId::Compose);
        let list_menu = named(menu_button(), "list-menu");
        list_bar.content.append(&list_compose);
        list_bar.content.append(&list_menu);
        let list_slot = named(
            adw::Bin::builder()
                .vexpand(true)
                .child(
                    empty::folder("No messages", "Messages in this folder appear here.").widget(),
                )
                .build(),
            "list-slot",
        );
        let list_pane = named(
            classed(
                adw::ToolbarView::builder().content(&list_slot).build(),
                &["sw-list-pane"],
            ),
            "list-pane",
        );
        list_pane.add_top_bar(&list_bar.root);

        let reader_back = named(
            icon_button(&["go-previous-symbolic"], "Back to the list"),
            "reader-back",
        );
        tooltips.add(&reader_back, "Back to the list", ActionId::BackToList);
        let reader_bar = PaneBar::new(
            tiling,
            Some((reader_back, Pane::List.name())),
            &["sw-toolbar"],
        );
        reader_bar.show_controls(false, true);
        let reader_toolbar = named(adw::Bin::builder().hexpand(true).build(), "reader-toolbar");
        reader_bar.content.append(&reader_toolbar);
        let column = ReaderColumn::new();
        column.slot.set_widget_name("reader-column");
        let reader_empty = named(empty::no_message(), "reader-empty");
        let batch = empty::BatchPage::new();
        batch.widget.set_widget_name("reader-batch");
        let reader = gtk::Stack::builder().vexpand(true).build();
        reader.add_named(&reader_empty, Some("empty"));
        reader.add_named(&column.root, Some("message"));
        reader.add_named(&batch.widget, Some("batch"));
        reader.set_visible_child_name("empty");
        let reader_pane = named(
            classed(
                adw::ToolbarView::builder().content(&reader).build(),
                &["sw-reader"],
            ),
            "reader-pane",
        );
        reader_pane.add_top_bar(&reader_bar.root);
        let list_handle = resize::handle("list-handle");
        let reader_area = gtk::Overlay::builder().child(&reader_pane).build();
        reader_area.add_overlay(&list_handle);

        let welcome_bar = PaneBar::new(
            tiling,
            Some((
                named(
                    icon_button(&["sidebar-show-symbolic"], "Folders"),
                    "welcome-back",
                ),
                Pane::Folders.name(),
            )),
            &[],
        );
        welcome_bar.show_controls(false, true);
        let welcome_menu = named(menu_button(), "welcome-menu");
        welcome_menu.set_hexpand(true);
        welcome_menu.set_halign(gtk::Align::End);
        welcome_bar.content.append(&welcome_menu);
        let no_account = empty::no_account();
        let add_account = named(no_account.add, "add-account");
        let welcome_hint = named(no_account.hint, "welcome-hint");
        let welcome = named(
            classed(
                adw::ToolbarView::builder()
                    .content(&no_account.page)
                    .build(),
                &["sw-welcome"],
            ),
            "welcome",
        );
        welcome.add_top_bar(&welcome_bar.root);

        let mail = adw::NavigationSplitView::builder()
            .sidebar(&page("Messages", "list", &list_pane))
            .content(&page("Message", "reader", &reader_area))
            .sidebar_width_unit(adw::LengthUnit::Px)
            .min_sidebar_width(LIST_WIDTH)
            .max_sidebar_width(LIST_WIDTH)
            .build();
        let content = gtk::Stack::new();
        content.add_named(&welcome, Some("welcome"));
        content.add_named(&mail, Some("mail"));
        content.set_visible_child_name("welcome");
        let sidebar_handle = resize::handle("sidebar-handle");
        let mail_area = gtk::Overlay::builder().child(&content).build();
        mail_area.add_overlay(&sidebar_handle);
        let panes = adw::NavigationSplitView::builder()
            .sidebar(&page("Folders", "folders", &sidebar.root))
            .content(&page("Mail", "mail", &mail_area))
            .sidebar_width_unit(adw::LengthUnit::Px)
            .min_sidebar_width(SIDEBAR_WIDTH)
            .max_sidebar_width(SIDEBAR_WIDTH)
            .show_content(true)
            .build();
        let banner = Banner::new();
        panes.set_vexpand(true);
        let dock = dock::Dock::new(&panes);
        let stacked = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stacked.append(&banner.root);
        stacked.append(dock.widget());
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stacked));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Sealwax")
            .default_width(DEFAULT_SIZE.0)
            .default_height(DEFAULT_SIZE.1)
            .width_request(MIN_SIZE.0)
            .height_request(MIN_SIZE.1)
            .content(&toasts)
            .build();
        window.add_css_class("sw-window");
        crate::accessibility::forget_stale_focus(&window);

        let shown = true.to_value();
        let hidden = false.to_value();
        // The rail's width is set as it applies (`connect_actions`): a setter would put back
        // the width from before the rail, not the one dragged since.
        let rail = breakpoint(WIDE_WIDTH);
        rail.add_setter(&sidebar_handle, "visible", Some(&hidden));
        rail.add_setter(
            &sidebar.stack,
            "visible-child-name",
            Some(&"rail".to_value()),
        );
        rail.add_setter(&sidebar.compose_label, "visible", Some(&hidden));
        if let Some(controls) = &sidebar.bar.start_controls {
            rail.add_setter(controls, "visible", Some(&hidden));
        }
        let narrow = breakpoint(RAIL_MIN_WIDTH);
        narrow.add_setter(&panes, "collapsed", Some(&shown));
        narrow.add_setter(&mail, "collapsed", Some(&shown));
        narrow.add_setter(&sidebar_handle, "visible", Some(&hidden));
        narrow.add_setter(&list_handle, "visible", Some(&hidden));
        for bar in [&sidebar.bar, &list_bar, &reader_bar, &welcome_bar] {
            bar.narrow_setters(&narrow);
        }
        window.add_breakpoint(rail.clone());
        window.add_breakpoint(narrow.clone());
        // One pane at a time draws no borders between panes (`style.css`).
        narrow.connect_apply(glib::clone!(
            #[weak]
            window,
            move |_| window.add_css_class("sw-one-pane")
        ));
        narrow.connect_unapply(glib::clone!(
            #[weak]
            window,
            move |_| window.remove_css_class("sw-one-pane")
        ));

        let folder = gio::SimpleAction::new_stateful(
            "folder",
            Some(glib::VariantTy::STRING),
            &"".to_variant(),
        );
        let pane = gio::SimpleAction::new_stateful(
            "pane",
            Some(glib::VariantTy::STRING),
            &Pane::List.name().to_variant(),
        );
        let more = gio::SimpleAction::new("more", Some(glib::VariantTy::STRING));
        let collapse = gio::SimpleAction::new("collapse", Some(glib::VariantTy::STRING));
        let group = gio::SimpleActionGroup::new();
        group.add_action(&folder);
        group.add_action(&pane);
        group.add_action(&more);
        group.add_action(&collapse);
        window.insert_action_group("nav", Some(&group));

        let named = [
            sidebar.root.clone().upcast::<gtk::Widget>(),
            sidebar.compose.clone().upcast(),
            list_pane.clone().upcast(),
            reader_pane.clone().upcast(),
            reader_empty.upcast(),
            column.slot.clone().upcast(),
            welcome.clone().upcast(),
            add_account.upcast(),
            welcome_hint.clone().upcast(),
            list_compose.clone().upcast(),
            list_title.clone().upcast(),
            list_subtitle.clone().upcast(),
            list_slot.clone().upcast(),
            reader_toolbar.clone().upcast(),
        ]
        .into_iter()
        .chain(
            [&list_bar, &reader_bar, &welcome_bar]
                .into_iter()
                .filter_map(|bar| bar.back.clone().map(|back| back.upcast())),
        )
        .collect();

        let this = Self {
            inner: Rc::new(Inner {
                window,
                panes,
                mail,
                rail,
                narrow,
                sidebar,
                content,
                list_pane,
                list_title,
                list_subtitle,
                list_slot,
                list_menu,
                list_compose,
                welcome_menu,
                reader_pane,
                reader_toolbar,
                reader,
                column,
                batch,
                welcome,
                welcome_hint,
                banner,
                dock,
                toasts,
                folder,
                pane,
                tooltips,
                named,
                folder_handlers: RefCell::new(Vec::new()),
                sidebar_width: Cell::new(SIDEBAR_WIDTH),
                list_width: Cell::new(LIST_WIDTH),
                rail_on: Cell::new(false),
                narrow_on: Cell::new(false),
                sidebar_hidden: Cell::new(false),
                sidebar_shown_changed: RefCell::new(None),
                sidebar_handle,
                list_handle,
                widths_changed: RefCell::new(None),
            }),
        };
        this.connect_actions(&more, &collapse);
        this.update_heading();
        this
    }

    fn connect_actions(&self, more: &gio::SimpleAction, collapse: &gio::SimpleAction) {
        let inner = &self.inner;
        inner.folder.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                if let Some(key) = parameter.and_then(|parameter| parameter.str()) {
                    Window { inner }.select_folder(key);
                }
            }
        ));
        inner.pane.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                match parameter
                    .and_then(|parameter| parameter.str())
                    .and_then(Pane::from_name)
                {
                    Some(pane) => Window { inner }.show_pane(pane),
                    None => tracing::warn!("nav.pane takes folders, list or reader"),
                }
            }
        ));
        more.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                if let Some(account) = parameter.and_then(|parameter| parameter.str()) {
                    inner.sidebar.toggle_more(account);
                }
            }
        ));
        collapse.connect_activate(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, parameter| {
                if let Some(account) = parameter.and_then(|parameter| parameter.str()) {
                    inner.sidebar.toggle_collapsed(account);
                }
            }
        ));
        for split in [&inner.panes, &inner.mail] {
            split.connect_show_content_notify(glib::clone!(
                #[weak(rename_to = inner)]
                self.inner,
                move |_| Window { inner }.sync_pane()
            ));
        }
        inner.rail.connect_apply(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| {
                inner.rail_on.set(true);
                Window { inner }.apply_widths();
            }
        ));
        inner.rail.connect_unapply(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| {
                inner.rail_on.set(false);
                Window { inner }.apply_widths();
            }
        ));
        let narrow_changed = |narrow: bool| {
            glib::clone!(
                #[weak(rename_to = inner)]
                self.inner,
                move |_: &adw::Breakpoint| {
                    inner.narrow_on.set(narrow);
                    let window = Window { inner };
                    window.apply_widths();
                    window.sidebar_shown_changed();
                }
            )
        };
        inner.narrow.connect_apply(narrow_changed(true));
        inner.narrow.connect_unapply(narrow_changed(false));
        resize::connect(
            &inner.panes,
            vec![
                (inner.sidebar_handle.clone(), self.handle_moves(true)),
                (inner.list_handle.clone(), self.handle_moves(false)),
            ],
        );
        inner.window.connect_current_breakpoint_notify(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_| tracing::debug!(layout = Window { inner }.layout().name(), "layout")
        ));
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.inner.window
    }

    /// Where the composer's cards go (the user's UI tweak, 2026-10-08).
    pub fn dock(&self) -> &dock::Dock {
        &self.inner.dock
    }

    /// The sidebar's and the message list's widths, as dragged (the defaults until then).
    pub fn pane_widths(&self) -> (f64, f64) {
        (self.inner.sidebar_width.get(), self.inner.list_width.get())
    }

    /// Sets the sidebar's and the message list's widths, each held to its range (as a
    /// remembered layout does at start).
    pub fn set_pane_widths(&self, sidebar: f64, list: f64) {
        let inner = &self.inner;
        inner.sidebar_width.set(clamp_to(sidebar, &SIDEBAR_WIDTHS));
        inner.list_width.set(clamp_to(list, &LIST_WIDTHS));
        self.apply_widths();
    }

    /// Runs `handler` with the widths when a drag ends or a double click resets one.
    pub fn connect_pane_widths(&self, handler: impl Fn(f64, f64) + 'static) {
        self.inner.widths_changed.replace(Some(Rc::new(handler)));
    }

    /// Whether the folders show: the folders pane in the wider layouts, the folders page while
    /// one pane fits.
    pub fn sidebar_shown(&self) -> bool {
        let inner = &self.inner;
        if inner.narrow_on.get() {
            self.pane() == Pane::Folders
        } else {
            !inner.sidebar_hidden.get()
        }
    }

    /// Whether Toggle Sidebar hid the folders pane, as remembered between runs.
    pub fn sidebar_hidden(&self) -> bool {
        self.inner.sidebar_hidden.get()
    }

    /// Hides the folders pane in the wider layouts or shows it again; while one pane fits, the
    /// folders page shows once the window widens.
    pub fn set_sidebar_hidden(&self, hidden: bool) {
        let inner = &self.inner;
        inner.sidebar_hidden.set(hidden);
        inner.sidebar_handle.set_can_target(!hidden);
        self.apply_widths();
        self.sidebar_shown_changed();
    }

    /// Toggle Sidebar: hides or shows the folders pane, or, while one pane fits, goes to the
    /// folders page or back to the list. True when what is remembered between runs changed.
    pub fn show_sidebar(&self, shown: bool) -> bool {
        if self.inner.narrow_on.get() {
            self.show_pane(if shown { Pane::Folders } else { Pane::List });
            return false;
        }
        let changed = self.sidebar_hidden() == shown;
        self.set_sidebar_hidden(!shown);
        changed
    }

    /// Runs `handler` with [`Self::sidebar_shown`] whenever it may have changed.
    pub fn connect_sidebar_shown(&self, handler: impl Fn(bool) + 'static) {
        self.inner
            .sidebar_shown_changed
            .replace(Some(Rc::new(handler)));
        self.sidebar_shown_changed();
    }

    fn sidebar_shown_changed(&self) {
        let handler = self.inner.sidebar_shown_changed.borrow().clone();
        if let Some(handler) = handler {
            handler(self.sidebar_shown());
        }
    }

    /// The widths on the split views: the folders pane is the rail's while the rail shows, and
    /// none while Toggle Sidebar hides it.
    fn apply_widths(&self) {
        let inner = &self.inner;
        let hidden = inner.sidebar_hidden.get() && !inner.narrow_on.get();
        inner.sidebar.root.set_visible(!hidden);
        inner
            .list_compose
            .set_visible(hidden || inner.narrow_on.get());
        let folders = if hidden {
            0.0
        } else if inner.rail_on.get() {
            RAIL_WIDTH
        } else {
            inner.sidebar_width.get()
        };
        inner.panes.set_min_sidebar_width(folders);
        inner.panes.set_max_sidebar_width(folders);
        inner.mail.set_min_sidebar_width(inner.list_width.get());
        inner.mail.set_max_sidebar_width(inner.list_width.get());
    }

    /// What the handle of the sidebar's border (`sidebar`) or the message list's does: drags
    /// it within its range, short of leaving the reader less than [`READER_MIN_WIDTH`] or its
    /// own minimum.
    fn handle_moves(&self, sidebar: bool) -> resize::Moves {
        let inner = &self.inner;
        let width = move |inner: &Inner| {
            if sidebar {
                inner.sidebar_width.get()
            } else {
                inner.list_width.get()
            }
        };
        let weak = Rc::downgrade(inner);
        let start = move || weak.upgrade().map_or(0.0, |inner| width(&inner));
        let weak = Rc::downgrade(inner);
        let drag = move |wanted: f64| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let window = f64::from(inner.window.width());
            let (range, others) = if sidebar {
                (&SIDEBAR_WIDTHS, inner.list_width.get())
            } else {
                let folders = if inner.rail_on.get() {
                    RAIL_WIDTH
                } else {
                    inner.sidebar_width.get()
                };
                (&LIST_WIDTHS, folders)
            };
            let (reader, ..) = inner.reader_pane.measure(gtk::Orientation::Horizontal, -1);
            let room = window - others - READER_MIN_WIDTH.max(f64::from(reader));
            // No narrower than the pane's content allows (the list's chips), so the width kept
            // is the width shown.
            let pane: &gtk::Widget = if sidebar {
                inner.sidebar.root.upcast_ref()
            } else {
                inner.list_pane.upcast_ref()
            };
            let (minimum, ..) = pane.measure(gtk::Orientation::Horizontal, -1);
            let width = clamp_to(wanted.min(room), range).max(f64::from(minimum));
            if sidebar {
                inner.sidebar_width.set(width);
            } else {
                inner.list_width.set(width);
            }
            Window { inner }.apply_widths();
        };
        let weak = Rc::downgrade(inner);
        let done = move || {
            if let Some(inner) = weak.upgrade() {
                Window { inner }.widths_changed();
            }
        };
        let weak = Rc::downgrade(inner);
        let reset = move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if sidebar {
                inner.sidebar_width.set(SIDEBAR_WIDTH);
            } else {
                inner.list_width.set(LIST_WIDTH);
            }
            let window = Window { inner };
            window.apply_widths();
            window.widths_changed();
        };
        resize::Moves {
            start: Box::new(start),
            drag: Box::new(drag),
            done: Box::new(done),
            reset: Box::new(reset),
        }
    }

    fn widths_changed(&self) {
        let handler = self.inner.widths_changed.borrow().clone();
        if let Some(handler) = handler {
            let (sidebar, list) = self.pane_widths();
            handler(sidebar, list);
        }
    }

    pub fn present(&self) {
        self.inner.window.present();
    }

    /// The message list (ML-026) goes here, under the list pane's bar.
    pub fn set_list(&self, list: &impl IsA<gtk::Widget>) {
        self.inner.list_slot.set_child(Some(list));
    }

    /// Shows a message (ML-027) in the reader's centred column, scrolled to its top. In one-pane
    /// layouts this doesn't navigate; `show_pane(Pane::Reader)` does.
    pub fn set_reader(&self, reader: &impl IsA<gtk::Widget>) {
        let inner = &self.inner;
        inner.column.slot.set_child(Some(reader));
        inner.column.scroller.vadjustment().set_value(0.0);
        inner.reader.set_visible_child_name("message");
    }

    /// Back to "No message selected".
    pub fn clear_reader(&self) {
        let inner = &self.inner;
        inner.reader.set_visible_child_name("empty");
        inner.column.slot.set_child(None::<&gtk::Widget>);
    }

    /// The reader's page for the select mode: "N messages selected" with the batch actions
    /// (ML-054). The message shown before goes; `set_reader` brings one back.
    pub fn show_batch(&self, count: u32) {
        let inner = &self.inner;
        inner.batch.set_count(count);
        inner.reader.set_visible_child_name("batch");
        inner.column.slot.set_child(None::<&gtk::Widget>);
    }

    /// The batch page, for its commands' titles and tooltips.
    pub fn batch_page(&self) -> &empty::BatchPage {
        &self.inner.batch
    }

    /// What the reader pane shows: `empty`, `message` or `batch`.
    pub fn reader_page(&self) -> String {
        self.inner
            .reader
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_default()
    }

    /// The reader's actions (archive, delete, … "n of N"), in its bar; the bar stretches.
    pub fn set_reader_toolbar(&self, toolbar: &impl IsA<gtk::Widget>) {
        self.inner.reader_toolbar.set_child(Some(toolbar));
    }

    /// Shows `model` in the sidebar. The folder shown stays when the model still has it, else
    /// the window picks its account's Inbox (a deleted folder's), else the first account's;
    /// with no account it shows "No accounts yet".
    pub fn set_sidebar(&self, model: SidebarModel) {
        let inner = &self.inner;
        let current = self.selected_folder();
        let selected = if model.contains(&current) {
            current.clone()
        } else {
            inner
                .sidebar
                .account_of(&current)
                .and_then(|account| model.accounts.iter().find(|a| a.key == account))
                .and_then(|account| account.inbox())
                .map(|inbox| inbox.key.clone())
                .or_else(|| model.default_selection())
                .unwrap_or_default()
        };
        inner
            .content
            .set_visible_child_name(if model.accounts.is_empty() {
                "welcome"
            } else {
                "mail"
            });
        inner.sidebar.set_model(model, &selected);
        inner.folder.set_state(&selected.to_variant());
        self.update_heading();
        if selected != current && !selected.is_empty() {
            self.emit_folder(&selected);
        }
    }

    /// The key of the folder shown, or "" before there is one.
    pub fn selected_folder(&self) -> String {
        self.inner
            .folder
            .state()
            .and_then(|state| state.str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Shows the folder `key` (as picking it in the sidebar does), and the list when one pane
    /// fits. Unknown keys are ignored.
    pub fn select_folder(&self, key: &str) {
        let inner = &self.inner;
        if !inner.sidebar.contains(key) {
            tracing::warn!("asked for a folder the sidebar doesn't have");
            return;
        }
        let changed = self.selected_folder() != key;
        inner.folder.set_state(&key.to_variant());
        inner.sidebar.set_selected(key);
        self.update_heading();
        if changed {
            self.emit_folder(key);
        }
        self.show_pane(Pane::List);
    }

    /// Runs `handler` with the key of every folder the window starts showing.
    pub fn connect_folder_selected(&self, handler: impl Fn(&Window, &str) + 'static) {
        self.inner
            .folder_handlers
            .borrow_mut()
            .push(Box::new(handler));
    }

    fn emit_folder(&self, key: &str) {
        for handler in self.inner.folder_handlers.borrow().iter() {
            handler(self, key);
        }
    }

    /// Overrides the list pane's title, as for search results.
    pub fn set_list_heading(&self, title: &str, subtitle: &str) {
        let inner = &self.inner;
        inner.list_title.set_text(title);
        inner.list_subtitle.set_text(subtitle);
        inner.list_subtitle.set_visible(!subtitle.is_empty());
    }

    fn update_heading(&self) {
        let (title, subtitle) = self
            .inner
            .sidebar
            .heading(&self.selected_folder())
            .unwrap_or_default();
        self.set_list_heading(&title, &subtitle);
    }

    /// The page shown while one pane fits; it is remembered in wider layouts.
    pub fn show_pane(&self, pane: Pane) {
        let inner = &self.inner;
        inner.panes.set_show_content(pane != Pane::Folders);
        if pane != Pane::Folders {
            inner.mail.set_show_content(pane == Pane::Reader);
        }
        self.sync_pane();
    }

    pub fn pane(&self) -> Pane {
        let inner = &self.inner;
        if !inner.panes.shows_content() {
            Pane::Folders
        } else if inner.mail.shows_content() {
            Pane::Reader
        } else {
            Pane::List
        }
    }

    fn sync_pane(&self) {
        let pane = self.pane().name();
        let current = self.inner.pane.state();
        if current.as_ref().and_then(|state| state.str()) != Some(pane) {
            self.inner.pane.set_state(&pane.to_variant());
            if self.inner.narrow_on.get() {
                self.sidebar_shown_changed();
            }
        }
    }

    pub fn layout(&self) -> Layout {
        let inner = &self.inner;
        match inner.window.current_breakpoint() {
            Some(current) if current == inner.narrow => Layout::Narrow,
            Some(current) if current == inner.rail => Layout::Rail,
            _ => Layout::Wide,
        }
    }

    /// The main menu (ML-022), at the end of the list pane's bar, or of the welcome page's while
    /// there is no account.
    pub fn set_menu_model(&self, menu: Option<&gio::MenuModel>) {
        self.inner.list_menu.set_menu_model(menu);
        self.inner.welcome_menu.set_menu_model(menu);
    }

    /// Shows the keys of `keymap` (the registry with `keys.toml` over it) in every tooltip and
    /// hint that names a command.
    pub fn set_keymap(&self, keymap: &Keymap) {
        self.inner.tooltips.set_keymap(keymap);
        self.inner
            .welcome_hint
            .set_text(&empty::palette_hint(keymap));
    }

    /// Gives `widget`, an icon-only button, the tooltip `label (keys)` with the keys of `id`,
    /// kept in step with `keys.toml` (DESIGN.md "Accessibility"): for the reader's toolbar
    /// (ML-027) and the list's own buttons (ML-026).
    pub fn add_shortcut_tooltip(&self, widget: &impl IsA<gtk::Widget>, label: &str, id: ActionId) {
        self.inner.tooltips.add(widget, label, id);
    }

    /// Shows the first of `queue` in the banner and counts the others, or hides the banner when
    /// `queue` is empty (ML-048).
    pub fn show_banner(&self, queue: &[Attention]) {
        self.inner.banner.show(queue);
    }

    pub fn shows_banner(&self) -> bool {
        self.inner.banner.is_shown()
    }

    /// Runs `handler` with what each banner button clicked asks for.
    pub fn connect_banner(&self, handler: impl Fn(BannerAction) + 'static) {
        self.inner.banner.connect(handler);
    }

    pub fn add_toast(&self, toast: adw::Toast) {
        self.inner.toasts.add_toast(toast);
    }

    /// Where toasts appear, for a dialog that reports its outcome after it closes.
    pub fn toasts(&self) -> adw::ToastOverlay {
        self.inner.toasts.clone()
    }

    pub fn sidebar(&self) -> &Sidebar {
        &self.inner.sidebar
    }

    /// A pane's root widget: `sidebar`, `list`, `reader` or `welcome`.
    pub fn pane_widget(&self, name: &str) -> Option<gtk::Widget> {
        let inner = &self.inner;
        Some(match name {
            "sidebar" => inner.sidebar.root.clone().upcast(),
            "list" => inner.list_pane.clone().upcast(),
            "reader" => inner.reader_pane.clone().upcast(),
            "welcome" => inner.welcome.clone().upcast(),
            _ => return None,
        })
    }

    /// A widget by its name (`gtk::Widget::widget_name`): the window's named parts, then
    /// anything in the window's tree.
    pub fn widget(&self, name: &str) -> Option<gtk::Widget> {
        if let Some(widget) = self.pane_widget(name) {
            return Some(widget);
        }
        self.inner
            .named
            .iter()
            .find(|widget| widget.widget_name() == name)
            .cloned()
            .or_else(|| find_named(self.inner.window.upcast_ref(), name))
    }
}

/// `width` held to `range`, in px.
fn clamp_to(width: f64, range: &std::ops::RangeInclusive<u32>) -> f64 {
    width
        .round()
        .clamp(f64::from(*range.start()), f64::from(*range.end()))
}

fn page(title: &str, tag: &str, child: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(child)
        .build()
}

/// Applies while the window is narrower than `width`.
fn breakpoint(width: f64) -> adw::Breakpoint {
    adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        width - 1.0,
        adw::LengthUnit::Px,
    ))
}

fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_named(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::actions::ActionId;

    #[test]
    fn buttons_are_bound_to_the_registry_names() {
        assert_eq!(COMPOSE_ACTION, ActionId::Compose.detailed_name());
        assert_eq!(OPEN_CONFIG_ACTION, ActionId::Preferences.detailed_name());
        assert_eq!(ADD_ACCOUNT_ACTION, ActionId::AddAccount.detailed_name());
    }
}
