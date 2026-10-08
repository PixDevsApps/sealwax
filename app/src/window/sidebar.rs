//! The folder sidebar (DESIGN.md "Components"): the compose button; "All Inboxes"; each account
//! with its status and its role folders (Inbox, Drafts, Sent, Archive, Junk, Trash) with their
//! counts, and its other folders under a collapsible "More"; and a footer with the accounts'
//! state and `config.toml`. Between 900 and 1280 px it becomes a rail of icons with unread
//! badges and the other accounts (DESIGN.md "Layout", the mock's medium window). The main
//! menu is in the list's bar.
//!
//! It shows a plain-data [`SidebarModel`], which the store fills (ML-023). Picking a folder
//! activates `nav.folder` with the folder's key; the window answers with
//! [`Sidebar::set_selected`]. For the self-test, rows are named `folder:<key>` (their counts
//! `count:<key>`), account headings `heading:<account>` (their dots `status:<account>`, their
//! names `account-name:<account>`, their words `status-text:<account>`), rail buttons `rail:<key>` (their badges `badge:<key>`) and
//! `rail-account:<account>` (their dots `rail-status:<account>`), and the `config.toml` buttons
//! `config` and `rail-config`.

use super::panes::{PaneBar, classed, icon, icon_button, named};
use super::tooltips::Tooltips;
use super::{COMPOSE_ACTION, OPEN_CONFIG_ACTION};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::account::AccountColor;
use sealwax_domain::actions::ActionId;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

/// `nav.folder`'s key for the unified inbox.
pub const ALL_INBOXES: &str = "all-inboxes";

const ALL_INBOXES_ICONS: &[&str] = &["mail-unread-symbolic"];
const FOLDER_ICONS: &[&str] = &["folder-symbolic"];
const COMPOSE_ICONS: &[&str] = &["mail-message-new-symbolic", "document-edit-symbolic"];
const CONFIG_ICONS: &[&str] = &["preferences-other-symbolic", "emblem-system-symbolic"];

/// The special folders every account shows, in display order (DESIGN.md "Components").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FolderRole {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
}

impl FolderRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Drafts => "Drafts",
            Self::Sent => "Sent",
            Self::Archive => "Archive",
            Self::Junk => "Junk",
            Self::Trash => "Trash",
        }
    }

    fn icons(self) -> &'static [&'static str] {
        match self {
            Self::Inbox => &["mail-inbox-symbolic", "mail-unread-symbolic"],
            Self::Drafts => &["mail-drafts-symbolic", "document-edit-symbolic"],
            Self::Sent => &["mail-send-symbolic"],
            Self::Archive => &["mail-archive-symbolic", "folder-documents-symbolic"],
            Self::Junk => &["mail-mark-junk-symbolic"],
            Self::Trash => &["user-trash-symbolic"],
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::Inbox => "sw-role-inbox",
            Self::Drafts => "sw-role-drafts",
            Self::Sent => "sw-role-sent",
            Self::Archive => "sw-role-archive",
            Self::Junk => "sw-role-junk",
            Self::Trash => "sw-role-trash",
        }
    }
}

/// An account's connection state, shown as a dot and in words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountStatus {
    Synced,
    Syncing,
    Offline,
    NeedsSignIn,
    /// The server's certificate no longer verifies; Sealwax stopped connecting (ML-048).
    CertificateChanged,
    /// The server says the mailbox is full; reading still syncs (ML-048).
    QuotaFull,
}

impl AccountStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Syncing => "syncing…",
            Self::Offline => "offline",
            Self::NeedsSignIn => "needs sign-in",
            Self::CertificateChanged => "bad certificate",
            Self::QuotaFull => "mailbox full",
        }
    }

    /// The status dot's colour (`style.css`): accent when synced, plain while syncing, warning
    /// while Sealwax keeps trying or still syncs (offline, mailbox full), error when it stopped
    /// until the user acts (sign-in, certificate).
    fn class(self) -> Option<&'static str> {
        match self {
            Self::Synced => Some("sw-synced"),
            Self::Syncing => None,
            Self::Offline => Some("sw-offline"),
            Self::NeedsSignIn => Some("sw-attention"),
            Self::CertificateChanged => Some("sw-certificate"),
            Self::QuotaFull => Some("sw-quota"),
        }
    }

    /// The states the rail marks on an account: the mock's rail shows none for synced or
    /// syncing accounts.
    fn needs_attention(self) -> bool {
        !matches!(self, Self::Synced | Self::Syncing)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarFolder {
    /// Unique among every account's folders: what `nav.folder` carries.
    pub key: String,
    /// The folder's own name; a role folder shows its role's name instead.
    pub name: String,
    pub role: Option<FolderRole>,
    /// Unread messages; for Drafts, the drafts.
    pub count: u32,
}

impl SidebarFolder {
    /// The name shown: the role's ("Sent" for a server's "Sendt"), else the folder's own.
    pub fn title(&self) -> &str {
        match self.role {
            Some(role) => role.label(),
            None => &self.name,
        }
    }

    /// The count in words, for accessible labels and the list's subtitle.
    pub fn count_text(&self) -> Option<String> {
        match (self.count, self.role) {
            (0, _) => None,
            (count, Some(FolderRole::Drafts)) => Some(format!("{count} drafts")),
            (count, _) => Some(format!("{count} unread")),
        }
    }

    /// Unread mail in the Inbox or in the user's own folders stands out; the other counts don't.
    fn count_stands_out(&self) -> bool {
        matches!(self.role, None | Some(FolderRole::Inbox))
    }

    fn accessible_label(&self) -> String {
        match self.count_text() {
            Some(count) => format!("{}, {count}", self.title()),
            None => self.title().to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarAccount {
    pub key: String,
    pub name: String,
    /// Its colour (`accounts.toml`): the heading's name and synced dot, the rail's initial.
    pub color: Option<AccountColor>,
    pub status: AccountStatus,
    /// Its role folders; shown in [`FolderRole`] order.
    pub folders: Vec<SidebarFolder>,
    /// Every other folder, under "More".
    pub more: Vec<SidebarFolder>,
}

impl SidebarAccount {
    pub fn inbox(&self) -> Option<&SidebarFolder> {
        self.folders
            .iter()
            .find(|folder| folder.role == Some(FolderRole::Inbox))
    }

    /// The role folders in display order; folders without a role come last.
    pub fn role_folders(&self) -> Vec<&SidebarFolder> {
        let mut folders: Vec<&SidebarFolder> = self.folders.iter().collect();
        folders.sort_by_key(|folder| folder.role.map_or(usize::MAX, |role| role as usize));
        folders
    }

    fn unread(&self) -> u32 {
        self.inbox().map_or(0, |inbox| inbox.count)
    }

    fn contains(&self, key: &str) -> bool {
        self.folders.iter().chain(&self.more).any(|f| f.key == key)
    }
}

/// What the sidebar shows: plain data, filled by the store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarModel {
    pub accounts: Vec<SidebarAccount>,
}

impl SidebarModel {
    /// Unread messages in every account's Inbox: "All Inboxes".
    pub fn all_inboxes_unread(&self) -> u32 {
        self.accounts.iter().map(SidebarAccount::unread).sum()
    }

    pub fn find(&self, key: &str) -> Option<(&SidebarAccount, &SidebarFolder)> {
        self.accounts.iter().find_map(|account| {
            account
                .folders
                .iter()
                .chain(&account.more)
                .find(|folder| folder.key == key)
                .map(|folder| (account, folder))
        })
    }

    /// Whether `key` names something to show: a folder, or All Inboxes once an account exists.
    pub fn contains(&self, key: &str) -> bool {
        (key == ALL_INBOXES && !self.accounts.is_empty()) || self.find(key).is_some()
    }

    /// What a new window shows: the first account's Inbox.
    pub fn default_selection(&self) -> Option<String> {
        let first = self.accounts.first()?;
        Some(
            first
                .inbox()
                .or_else(|| first.folders.first())
                .map_or_else(|| ALL_INBOXES.to_owned(), |folder| folder.key.clone()),
        )
    }

    /// The account whose folders the rail shows: the selected folder's, else the first.
    fn current_account(&self, key: &str) -> Option<&SidebarAccount> {
        self.accounts
            .iter()
            .find(|account| account.contains(key))
            .or_else(|| self.accounts.first())
    }

    /// The footer: "2 accounts · all synced".
    pub fn summary(&self) -> String {
        let accounts = match self.accounts.len() {
            0 => return "No accounts".to_owned(),
            1 => "1 account".to_owned(),
            count => format!("{count} accounts"),
        };
        let has = |status| self.accounts.iter().any(|account| account.status == status);
        let state = if has(AccountStatus::CertificateChanged) {
            "bad certificate"
        } else if has(AccountStatus::NeedsSignIn) {
            "sign-in needed"
        } else if has(AccountStatus::QuotaFull) {
            "mailbox full"
        } else if has(AccountStatus::Offline) {
            "offline"
        } else if has(AccountStatus::Syncing) {
            "syncing…"
        } else if self.accounts.len() == 1 {
            "synced"
        } else {
            "all synced"
        };
        format!("{accounts} · {state}")
    }

    /// The list pane's title and subtitle for `key`: "Inbox", "12 unread · Personal".
    pub fn heading(&self, key: &str) -> Option<(String, String)> {
        if key == ALL_INBOXES {
            if self.accounts.is_empty() {
                return None;
            }
            let accounts = match self.accounts.len() {
                1 => "1 account".to_owned(),
                count => format!("{count} accounts"),
            };
            let subtitle = match self.all_inboxes_unread() {
                0 => accounts,
                unread => format!("{unread} unread · {accounts}"),
            };
            return Some(("All Inboxes".to_owned(), subtitle));
        }
        let (account, folder) = self.find(key)?;
        let subtitle = match folder.count_text() {
            Some(count) => format!("{count} · {}", account.name),
            None => account.name.clone(),
        };
        Some((folder.title().to_owned(), subtitle))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowKind {
    Folder(String),
    /// An account's heading, by the account's key.
    Header(String),
    More(String),
}

struct Row {
    widget: gtk::ListBoxRow,
    kind: RowKind,
    /// The account whose "More" holds this folder.
    more_of: Option<String>,
    /// The "More" row's arrow.
    arrow: Option<gtk::Image>,
}

pub struct Sidebar {
    /// The folders page's content.
    pub root: adw::ToolbarView,
    pub bar: PaneBar,
    pub compose: gtk::Button,
    /// "New message", hidden in the rail.
    pub compose_label: gtk::Label,
    /// `full` or `rail`.
    pub stack: gtk::Stack,
    list: gtk::ListBox,
    rows: RefCell<Vec<Row>>,
    rail_items: gtk::Box,
    summary: gtk::Label,
    model: RefCell<SidebarModel>,
    selected: RefCell<String>,
    expanded: RefCell<HashSet<String>>,
    /// A folder row's or an account heading's context menu, while it is open.
    menu: RefCell<Option<gtk::Popover>>,
    /// Whether Mark Folder as Read applies to a folder key, and what it does.
    mark_read: RefCell<Option<MarkRead>>,
    /// What an account heading's Edit Account does, given the account's key.
    edit_account: RefCell<Option<EditAccount>>,
    weak: RefCell<std::rc::Weak<Sidebar>>,
}

type MarkRead = (Rc<dyn Fn(&str) -> bool>, Rc<dyn Fn(&str)>);
type EditAccount = Rc<dyn Fn(&str)>;

impl Sidebar {
    /// `tooltips` names the keys of the buttons that run a registry command.
    pub fn new(tiling: bool, tooltips: &Tooltips) -> Rc<Self> {
        let (compose, compose_label) = compose_button("New message");
        let compose = named(compose, "compose");
        compose.set_hexpand(true);
        tooltips.add(&compose, "New message", ActionId::Compose);
        let bar = PaneBar::new(tiling, None, &["sw-sidebar-bar"]);
        bar.content.append(&compose);
        bar.show_controls(true, false);

        let list = named(
            classed(
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .build(),
                &["sw-folder-list"],
            ),
            "folder-list",
        );
        list.update_property(&[gtk::accessible::Property::Label("Folders")]);
        let summary = named(
            classed(
                gtk::Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build(),
                &["sw-sidebar-summary"],
            ),
            "sidebar-summary",
        );
        let footer = classed(
            gtk::Box::new(gtk::Orientation::Horizontal, 0),
            &["sw-sidebar-footer"],
        );
        footer.append(&summary);
        let config = named(config_button(tooltips, "sw-icon-button"), "config");
        footer.append(&config);
        let full = classed(
            gtk::Box::new(gtk::Orientation::Vertical, 0),
            &["sw-sidebar-body"],
        );
        full.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build(),
        );
        full.append(&footer);

        let rail_items = classed(
            gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .halign(gtk::Align::Center)
                .build(),
            &["sw-rail-items"],
        );
        let rail_footer = classed(
            gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .halign(gtk::Align::Center)
                .build(),
            &["sw-rail-footer"],
        );
        rail_footer.append(&named(
            config_button(tooltips, "sw-rail-button"),
            "rail-config",
        ));
        let rail = named(
            classed(gtk::Box::new(gtk::Orientation::Vertical, 0), &["sw-rail"]),
            "rail",
        );
        rail.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&rail_items)
                .build(),
        );
        rail.append(&rail_footer);

        let stack = gtk::Stack::builder()
            .hhomogeneous(false)
            .vexpand(true)
            .build();
        stack.add_named(&full, Some("full"));
        stack.add_named(&rail, Some("rail"));
        stack.set_visible_child_name("full");

        let root = named(
            classed(
                adw::ToolbarView::builder().content(&stack).build(),
                &["sw-sidebar"],
            ),
            "sidebar",
        );
        root.add_top_bar(&bar.root);

        let sidebar = Rc::new(Self {
            root,
            bar,
            compose,
            compose_label,
            stack,
            list,
            rows: RefCell::new(Vec::new()),
            rail_items,
            summary,
            model: RefCell::new(SidebarModel::default()),
            selected: RefCell::new(String::new()),
            expanded: RefCell::new(HashSet::new()),
            menu: RefCell::new(None),
            mark_read: RefCell::new(None),
            edit_account: RefCell::new(None),
            weak: RefCell::new(std::rc::Weak::new()),
        });
        sidebar.weak.replace(Rc::downgrade(&sidebar));
        sidebar.keep_one_tab_stop(&config);
        sidebar.list.connect_row_activated(glib::clone!(
            #[weak]
            sidebar,
            move |_, row| sidebar.activated(row)
        ));
        sidebar.rebuild();
        sidebar
    }

    /// The folder list is one Tab stop, as the message list is: Tab and Shift+Tab leave it
    /// from any row (Up and Down move inside it), and coming in from the compose button or
    /// the config button they land on the folder shown. GtkListBox's own Tab goes row by row
    /// and stops at the first account heading, so it differed by direction.
    fn keep_one_tab_stop(&self, config: &gtk::Button) {
        let leave = gtk::EventControllerKey::new();
        leave.set_propagation_phase(gtk::PropagationPhase::Capture);
        let compose = self.compose.downgrade();
        let after = config.downgrade();
        leave.connect_key_pressed(move |_, key, _, state| {
            let target = match tab_direction(key, state) {
                Some(Tab::Forward) => after.upgrade(),
                Some(Tab::Backward) => compose.upgrade(),
                None => None,
            };
            match target {
                Some(target) if target.is_sensitive() && target.grab_focus() => {
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.list.add_controller(leave);
        for (neighbour, entering) in [
            (self.compose.clone(), Tab::Forward),
            (config.clone(), Tab::Backward),
        ] {
            let enter = gtk::EventControllerKey::new();
            enter.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = self.weak.borrow().clone();
            enter.connect_key_pressed(move |_, key, _, state| {
                let row = weak.upgrade().and_then(|sidebar| sidebar.entry_row());
                match row {
                    Some(row)
                        if tab_direction(key, state) == Some(entering) && row.grab_focus() =>
                    {
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
            neighbour.add_controller(enter);
        }
    }

    /// Where the keyboard focus enters the folder list: the folder shown, else the first row.
    fn entry_row(&self) -> Option<gtk::ListBoxRow> {
        if !self.list.is_mapped() {
            return None;
        }
        let rows = self.rows.borrow();
        let usable = |row: &&Row| row.widget.is_mapped() && row.widget.is_sensitive();
        rows.iter()
            .filter(usable)
            .find(|row| row.widget.has_css_class("sw-current"))
            .or_else(|| {
                rows.iter()
                    .filter(usable)
                    .find(|row| row.widget.is_activatable())
            })
            .map(|row| row.widget.clone())
    }

    pub fn contains(&self, key: &str) -> bool {
        self.model.borrow().contains(key)
    }

    pub fn heading(&self, key: &str) -> Option<(String, String)> {
        self.model.borrow().heading(key)
    }

    /// Shows `model` with `selected` picked.
    pub fn set_model(&self, model: SidebarModel, selected: &str) {
        self.expanded
            .borrow_mut()
            .retain(|account| model.accounts.iter().any(|a| &a.key == account));
        self.model.replace(model);
        self.selected.replace(selected.to_owned());
        self.reveal_selected();
        self.rebuild();
    }

    /// Marks `key` as the folder shown; a folder under a closed "More" opens it.
    pub fn set_selected(&self, key: &str) {
        let account_changed = {
            let model = self.model.borrow();
            let before = model
                .current_account(&self.selected.borrow())
                .map(|a| a.key.clone());
            before != model.current_account(key).map(|a| a.key.clone())
        };
        self.selected.replace(key.to_owned());
        if self.reveal_selected() {
            self.rebuild_list();
        } else {
            self.sync_selection();
        }
        if account_changed {
            self.rebuild_rail();
        }
    }

    pub fn is_expanded(&self, account: &str) -> bool {
        self.expanded.borrow().contains(account)
    }

    /// Gives every folder row a context menu with Mark Folder as Read (ML-052): a right click,
    /// or Menu or Shift+F10 on a focused row, opens it. `applies` says whether the folder has
    /// anything unread; `activate` marks it read.
    pub fn connect_mark_read(
        &self,
        applies: impl Fn(&str) -> bool + 'static,
        activate: impl Fn(&str) + 'static,
    ) {
        self.mark_read
            .replace(Some((Rc::new(applies), Rc::new(activate))));
    }

    /// Gives every account heading a context menu with Edit Account (the user's UI tweak,
    /// 2026-10-08), opened by a right click; `activate` gets the account's key.
    pub fn connect_edit_account(&self, activate: impl Fn(&str) + 'static) {
        self.edit_account.replace(Some(Rc::new(activate)));
    }

    /// Opens the context menu of the folder row `key`, as a right click does.
    pub fn open_menu(&self, key: &str) -> bool {
        let row = self
            .rows
            .borrow()
            .iter()
            .find(|row| row.kind == RowKind::Folder(key.to_owned()))
            .map(|row| row.widget.clone());
        match row {
            Some(row) => {
                self.show_menu(&row, key);
                true
            }
            None => false,
        }
    }

    /// Opens the context menu of the account `key`'s heading, as a right click does.
    pub fn open_account_menu(&self, key: &str) -> bool {
        let row = self
            .rows
            .borrow()
            .iter()
            .find(|row| row.kind == RowKind::Header(key.to_owned()))
            .map(|row| row.widget.clone());
        match row {
            Some(row) => {
                self.show_account_menu(&row, key);
                true
            }
            None => false,
        }
    }

    /// The open context menu, if any.
    pub fn menu(&self) -> Option<gtk::Popover> {
        self.menu
            .borrow()
            .clone()
            .filter(|menu| menu.parent().is_some())
    }

    fn show_menu(&self, row: &gtk::ListBoxRow, key: &str) {
        self.close_menu();
        let Some((applies, activate)) = self.mark_read.borrow().clone() else {
            return;
        };
        let enabled = applies(key);
        let button = named(
            classed(
                gtk::Button::builder()
                    .label(ActionId::MarkFolderRead.title())
                    .sensitive(enabled)
                    .build(),
                &["flat", "sw-menu-item"],
            ),
            "folder-menu-mark-read",
        );
        if !enabled {
            button.set_tooltip_text(Some("Nothing here is unread"));
        }
        let key = key.to_owned();
        self.popup(row, &button, "folder-menu", move || activate(&key));
    }

    /// An account heading's menu: Edit Account for that account.
    fn show_account_menu(&self, row: &gtk::ListBoxRow, key: &str) {
        self.close_menu();
        let Some(activate) = self.edit_account.borrow().clone() else {
            return;
        };
        let button = named(
            classed(
                gtk::Button::builder()
                    .label(ActionId::EditAccount.title())
                    .build(),
                &["flat", "sw-menu-item"],
            ),
            "account-menu-edit",
        );
        let key = key.to_owned();
        // After the closed menu has put the focus back, so the dialog keeps it.
        self.popup(row, &button, "account-menu", move || {
            let (activate, key) = (activate.clone(), key.clone());
            glib::idle_add_local_once(move || activate(&key));
        });
    }

    /// Shows `button` in a context menu named `name` under `row`; a click closes the menu and
    /// runs `activate`.
    fn popup(
        &self,
        row: &gtk::ListBoxRow,
        button: &gtk::Button,
        name: &str,
        activate: impl Fn() + 'static,
    ) {
        let popover = named(
            classed(
                gtk::Popover::builder()
                    .child(button)
                    .has_arrow(false)
                    .position(gtk::PositionType::Bottom)
                    .build(),
                &["sw-folder-menu"],
            ),
            name,
        );
        popover.set_parent(row);
        // The focus goes back where it was (the list, or the row for Menu and Shift+F10), once
        // the sidebar has been rebuilt for the new counts.
        let before = row
            .root()
            .and_then(|root| root.focus())
            .map(|widget| widget.downgrade());
        popover.connect_closed(move |popover| {
            popover.unparent();
            let before = before.clone();
            glib::idle_add_local_once(move || {
                if let Some(widget) = before.and_then(|widget| widget.upgrade())
                    && widget.root().is_some()
                {
                    widget.grab_focus();
                }
            });
        });
        button.connect_clicked(glib::clone!(
            #[weak]
            popover,
            move |_| {
                popover.popdown();
                activate();
            }
        ));
        popover.popup();
        self.menu.replace(Some(popover));
    }

    fn close_menu(&self) {
        if let Some(menu) = self.menu.take()
            && menu.parent().is_some()
        {
            menu.popdown();
            if menu.parent().is_some() {
                menu.unparent();
            }
        }
    }

    /// A right click, Menu or Shift+F10 on a folder row opens its context menu.
    fn add_menu_triggers(&self, row: &gtk::ListBoxRow, key: &str) {
        let click = gtk::GestureClick::builder().button(3).build();
        let weak = self.weak.borrow().clone();
        let target = key.to_owned();
        click.connect_pressed(move |gesture, _, _, _| {
            if let (Some(sidebar), Some(row)) = (
                weak.upgrade(),
                gesture.widget().and_downcast::<gtk::ListBoxRow>(),
            ) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                sidebar.show_menu(&row, &target);
            }
        });
        row.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak.borrow().clone();
        let target = key.to_owned();
        keys.connect_key_pressed(move |controller, key, _, state| {
            let menu_key = key == gtk::gdk::Key::Menu
                || (key == gtk::gdk::Key::F10
                    && state.contains(gtk::gdk::ModifierType::SHIFT_MASK));
            match (
                menu_key,
                weak.upgrade(),
                controller.widget().and_downcast::<gtk::ListBoxRow>(),
            ) {
                (true, Some(sidebar), Some(row)) => {
                    sidebar.show_menu(&row, &target);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        row.add_controller(keys);
    }

    /// A right click on an account heading opens its menu. The heading takes no keyboard
    /// focus; Edit Account is in the palette and the main menu.
    fn add_account_menu_trigger(&self, row: &gtk::ListBoxRow, key: &str) {
        let click = gtk::GestureClick::builder().button(3).build();
        let weak = self.weak.borrow().clone();
        let target = key.to_owned();
        click.connect_pressed(move |gesture, _, _, _| {
            if let (Some(sidebar), Some(row)) = (
                weak.upgrade(),
                gesture.widget().and_downcast::<gtk::ListBoxRow>(),
            ) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                sidebar.show_account_menu(&row, &target);
            }
        });
        row.add_controller(click);
    }

    /// Opens or closes an account's "More" folders.
    pub fn toggle_more(&self, account: &str) {
        let expanded = {
            let mut set = self.expanded.borrow_mut();
            if !set.remove(account) {
                set.insert(account.to_owned());
                true
            } else {
                false
            }
        };
        for row in self.rows.borrow().iter() {
            if row.more_of.as_deref() == Some(account) {
                row.widget.set_visible(expanded);
            }
            if row.kind == RowKind::More(account.to_owned()) {
                set_expanded(row, expanded);
            }
        }
    }

    /// The key of the folder row marked as the one shown.
    pub fn marked_folder(&self) -> Option<String> {
        self.rows
            .borrow()
            .iter()
            .find(|row| row.widget.has_css_class("sw-current"))
            .and_then(|row| match &row.kind {
                RowKind::Folder(key) => Some(key.clone()),
                _ => None,
            })
    }

    /// The keys of the folder rows the user can see in the full sidebar, in order.
    pub fn visible_keys(&self) -> Vec<String> {
        self.rows
            .borrow()
            .iter()
            .filter(|row| row.widget.get_visible())
            .filter_map(|row| match &row.kind {
                RowKind::Folder(key) => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    /// Opens the "More" that holds the selected folder; true when it was closed.
    fn reveal_selected(&self) -> bool {
        let model = self.model.borrow();
        let selected = self.selected.borrow();
        let holder = model
            .accounts
            .iter()
            .find(|account| account.more.iter().any(|folder| folder.key == *selected));
        match holder {
            Some(account) => self.expanded.borrow_mut().insert(account.key.clone()),
            None => false,
        }
    }

    fn activated(&self, row: &gtk::ListBoxRow) {
        let kind = self
            .rows
            .borrow()
            .iter()
            .find(|candidate| &candidate.widget == row)
            .map(|row| row.kind.clone());
        let (action, target) = match kind {
            Some(RowKind::Folder(key)) => ("nav.folder", key),
            Some(RowKind::More(account)) => ("nav.more", account),
            _ => return,
        };
        if let Err(error) = self
            .list
            .activate_action(action, Some(&target.to_variant()))
        {
            tracing::warn!(%error, action, "sidebar row without its window action");
        }
    }

    fn rebuild(&self) {
        self.rebuild_list();
        self.rebuild_rail();
        self.summary.set_text(&self.model.borrow().summary());
    }

    fn rebuild_list(&self) {
        self.close_menu();
        // New counts rebuild the rows; a row that had the keyboard focus keeps it.
        let focused = self.focused_kind();
        self.list.remove_all();
        let model = self.model.borrow();
        let expanded = self.expanded.borrow();
        let mut rows = Vec::new();
        if !model.accounts.is_empty() {
            rows.push(folder_row(
                &all_inboxes(&model),
                ALL_INBOXES_ICONS,
                "sw-all-inboxes",
                false,
            ));
        }
        for account in &model.accounts {
            rows.push(header_row(account));
            for folder in account.role_folders() {
                let (icons, class) = folder.role.map_or((FOLDER_ICONS, "sw-role-other"), |role| {
                    (role.icons(), role.class())
                });
                rows.push(folder_row(folder, icons, class, folder.count_stands_out()));
            }
            if account.more.is_empty() {
                continue;
            }
            let open = expanded.contains(&account.key);
            rows.push(more_row(account, open));
            for folder in &account.more {
                let mut row = folder_row(
                    folder,
                    FOLDER_ICONS,
                    "sw-role-other",
                    folder.count_stands_out(),
                );
                row.widget.set_visible(open);
                row.more_of = Some(account.key.clone());
                rows.push(row);
            }
        }
        for row in &rows {
            match &row.kind {
                RowKind::Folder(key) => self.add_menu_triggers(&row.widget, key),
                RowKind::Header(key) => self.add_account_menu_trigger(&row.widget, key),
                RowKind::More(_) => {}
            }
            self.list.append(&row.widget);
        }
        drop(expanded);
        drop(model);
        if let Some(kind) = focused
            && let Some(row) = rows.iter().find(|row| row.kind == kind)
        {
            row.widget.grab_focus();
        }
        self.rows.replace(rows);
        self.sync_selection();
    }

    /// The kind of the row that has the keyboard focus, if one has.
    fn focused_kind(&self) -> Option<RowKind> {
        let focus = self.list.root()?.focus()?;
        self.rows
            .borrow()
            .iter()
            .find(|row| {
                focus == *row.widget.upcast_ref::<gtk::Widget>() || focus.is_ancestor(&row.widget)
            })
            .map(|row| row.kind.clone())
    }

    /// Marks the row of the folder shown. The list has no selection of its own: GTK's follows
    /// the focus and the cursor, and then left another row marked than the folder shown.
    fn sync_selection(&self) {
        let selected = self.selected.borrow();
        for row in self.rows.borrow().iter() {
            let current = row.kind == RowKind::Folder(selected.clone());
            if current {
                row.widget.add_css_class("sw-current");
            } else {
                row.widget.remove_css_class("sw-current");
            }
            if matches!(row.kind, RowKind::Folder(_)) {
                row.widget
                    .update_state(&[gtk::accessible::State::Selected(Some(current))]);
            }
        }
    }

    /// All Inboxes, the shown account's role folders with unread badges, and the other accounts
    /// (the mock's medium window).
    fn rebuild_rail(&self) {
        while let Some(child) = self.rail_items.first_child() {
            self.rail_items.remove(&child);
        }
        let model = self.model.borrow();
        let selected = self.selected.borrow();
        let Some(current) = model.current_account(&selected) else {
            return;
        };
        self.rail_items.append(&rail_folder(
            &all_inboxes(&model),
            ALL_INBOXES_ICONS,
            "All Inboxes",
            false,
        ));
        self.rail_items.append(&rail_separator());
        for folder in current.role_folders() {
            self.rail_items.append(&rail_folder(
                folder,
                folder.role.map_or(FOLDER_ICONS, FolderRole::icons),
                &format!("{} · {}", folder.title(), current.name),
                folder.count_stands_out(),
            ));
        }
        let others: Vec<&SidebarAccount> = model
            .accounts
            .iter()
            .filter(|account| account.key != current.key)
            .collect();
        if !others.is_empty() {
            self.rail_items.append(&rail_separator());
        }
        for account in others {
            self.rail_items.append(&rail_account(account));
        }
    }
}

/// The compose button: the accent fill with an icon and `label`, bound to the registry's
/// Compose (insensitive until the composer exists, M6). The label is hidden in the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Forward,
    Backward,
}

/// Which way a key moves the focus as Tab does: Tab, or Shift+Tab (GDK's ISO_Left_Tab);
/// Ctrl+Tab and Alt+Tab are left alone.
fn tab_direction(key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> Option<Tab> {
    use gtk::gdk::{Key, ModifierType};
    if state.intersects(ModifierType::CONTROL_MASK | ModifierType::ALT_MASK) {
        return None;
    }
    match key {
        Key::ISO_Left_Tab => Some(Tab::Backward),
        Key::Tab | Key::KP_Tab if state.contains(ModifierType::SHIFT_MASK) => Some(Tab::Backward),
        Key::Tab | Key::KP_Tab => Some(Tab::Forward),
        _ => None,
    }
}

pub(super) fn compose_button(label: &str) -> (gtk::Button, gtk::Label) {
    let text = gtk::Label::new(Some(label));
    let content = gtk::Box::builder().halign(gtk::Align::Center).build();
    content.append(&icon(COMPOSE_ICONS));
    content.append(&text);
    let button = classed(
        gtk::Button::builder()
            .child(&content)
            .action_name(COMPOSE_ACTION)
            .build(),
        &["sw-compose"],
    );
    button.update_property(&[gtk::accessible::Property::Label("New message")]);
    (button, text)
}

fn config_button(tooltips: &Tooltips, class: &str) -> gtk::Button {
    let button = icon_button(CONFIG_ICONS, "Open config.toml");
    button.remove_css_class("sw-icon-button");
    button.add_css_class(class);
    button.set_action_name(Some(OPEN_CONFIG_ACTION));
    tooltips.add(&button, "Open config.toml", ActionId::Preferences);
    button
}

/// The unified inbox as a folder of its own; its count is the accounts' Inboxes, so it stays
/// quiet beside them (the mock).
fn all_inboxes(model: &SidebarModel) -> SidebarFolder {
    SidebarFolder {
        key: ALL_INBOXES.to_owned(),
        name: "All Inboxes".to_owned(),
        role: None,
        count: model.all_inboxes_unread(),
    }
}

fn count_label(count: u32, stands_out: bool) -> gtk::Label {
    let label = classed(gtk::Label::new(Some(&count.to_string())), &["sw-count"]);
    if stands_out {
        label.add_css_class("sw-unread");
    }
    label
}

fn folder_row(folder: &SidebarFolder, icons: &[&str], class: &str, stands_out: bool) -> Row {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&icon(icons));
    content.append(
        &gtk::Label::builder()
            .label(folder.title())
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    if folder.count > 0 {
        content.append(&named(
            count_label(folder.count, stands_out),
            &format!("count:{}", folder.key),
        ));
    }
    let widget = named(
        classed(
            gtk::ListBoxRow::builder().child(&content).build(),
            &["sw-sidebar-row", class],
        ),
        &format!("folder:{}", folder.key),
    );
    widget.update_property(&[gtk::accessible::Property::Label(&folder.accessible_label())]);
    Row {
        widget,
        kind: RowKind::Folder(folder.key.clone()),
        more_of: None,
        arrow: None,
    }
}

/// The account's state as a dot (DESIGN.md "Components"); the words beside it say the same.
fn status_dot(status: AccountStatus) -> gtk::Box {
    let dot = classed(
        gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
        &["sw-status-dot"],
    );
    if let Some(class) = status.class() {
        dot.add_css_class(class);
    }
    dot
}

/// Marks `widget` with the account's colour, which `style.css` gives the parts inside it.
fn color_account(widget: &impl IsA<gtk::Widget>, color: Option<AccountColor>) {
    if let Some(color) = color {
        widget.add_css_class("sw-account-colored");
        widget.add_css_class(&format!("sw-account-{}", color.name()));
    }
}

/// The account's heading: its status dot, its name (upper-cased by `style.css`) and its
/// status in words, the dot and the name in the account's colour.
fn header_row(account: &SidebarAccount) -> Row {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    color_account(&content, account.color);
    content.append(&named(
        status_dot(account.status),
        &format!("status:{}", account.key),
    ));
    content.append(&named(
        classed(
            gtk::Label::builder()
                .label(&account.name)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
            &["sw-account-name"],
        ),
        &format!("account-name:{}", account.key),
    ));
    content.append(&named(
        classed(
            gtk::Label::new(Some(account.status.label())),
            &["sw-account-status"],
        ),
        &format!("status-text:{}", account.key),
    ));
    let widget = named(
        classed(
            gtk::ListBoxRow::builder()
                .child(&content)
                .activatable(false)
                .selectable(false)
                .focusable(false)
                .build(),
            &["sw-sidebar-heading"],
        ),
        &format!("heading:{}", account.key),
    );
    widget.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}, {}",
        account.name,
        account.status.label()
    ))]);
    Row {
        widget,
        kind: RowKind::Header(account.key.clone()),
        more_of: None,
        arrow: None,
    }
}

fn more_row(account: &SidebarAccount, open: bool) -> Row {
    let arrow = gtk::Image::new();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&arrow);
    content.append(
        &gtk::Label::builder()
            .label("More folders")
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    let widget = classed(
        gtk::ListBoxRow::builder()
            .child(&content)
            .selectable(false)
            .build(),
        &["sw-sidebar-row", "sw-more-row"],
    );
    widget.update_property(&[gtk::accessible::Property::Label(&format!(
        "More folders in {}",
        account.name
    ))]);
    let row = Row {
        widget,
        kind: RowKind::More(account.key.clone()),
        more_of: None,
        arrow: Some(arrow),
    };
    set_expanded(&row, open);
    row
}

fn set_expanded(row: &Row, open: bool) {
    if let Some(arrow) = &row.arrow {
        arrow.set_icon_name(Some(if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
    }
    row.widget
        .update_state(&[gtk::accessible::State::Expanded(Some(open))]);
}

/// A rail button for a folder: its icon, and an accent badge at the top-right corner when its
/// count stands out (the mock badges only unread mail; the tooltip has every count).
fn rail_folder(
    folder: &SidebarFolder,
    icons: &[&str],
    name: &str,
    stands_out: bool,
) -> gtk::ToggleButton {
    let overlay = gtk::Overlay::builder().child(&icon(icons)).build();
    if folder.count > 0 && stands_out {
        overlay.add_overlay(&named(
            classed(
                gtk::Label::builder()
                    .label(folder.count.to_string())
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Start)
                    .build(),
                &["sw-rail-badge"],
            ),
            &format!("badge:{}", folder.key),
        ));
    }
    let label = match folder.count_text() {
        Some(count) => format!("{name}, {count}"),
        None => name.to_owned(),
    };
    let button = named(
        classed(
            gtk::ToggleButton::builder()
                .child(&overlay)
                .action_name("nav.folder")
                .action_target(&folder.key.to_variant())
                .tooltip_text(&label)
                .build(),
            &["sw-rail-button"],
        ),
        &format!("rail:{}", folder.key),
    );
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button
}

/// Another account: its initial and its unread count in the accent, as in the mock ("W4"), and
/// a status dot only when it needs the user (offline or signed out).
fn rail_account(account: &SidebarAccount) -> gtk::Button {
    let initial: String = account
        .name
        .chars()
        .next()
        .map(|first| first.to_uppercase().collect())
        .unwrap_or_default();
    let content = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    color_account(&content, account.color);
    content.append(&classed(
        gtk::Label::new(Some(&initial)),
        &["sw-rail-initial"],
    ));
    let unread = account.unread();
    if unread > 0 {
        content.append(&count_label(unread, true));
    }
    let overlay = gtk::Overlay::builder().child(&content).build();
    if account.status.needs_attention() {
        let dot = named(
            status_dot(account.status),
            &format!("rail-status:{}", account.key),
        );
        dot.set_halign(gtk::Align::End);
        dot.set_valign(gtk::Align::End);
        overlay.add_overlay(&dot);
    }
    let mut label = format!("{}, {}", account.name, account.status.label());
    if unread > 0 {
        label.push_str(&format!(", {unread} unread"));
    }
    let target = account
        .inbox()
        .or_else(|| account.folders.first())
        .map_or_else(|| ALL_INBOXES.to_owned(), |folder| folder.key.clone());
    let button = named(
        classed(
            gtk::Button::builder()
                .child(&overlay)
                .action_name("nav.folder")
                .action_target(&target.to_variant())
                .tooltip_text(&label)
                .build(),
            &["sw-rail-button", "sw-rail-account"],
        ),
        &format!("rail-account:{}", account.key),
    );
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button
}

fn rail_separator() -> gtk::Separator {
    classed(
        gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .halign(gtk::Align::Center)
            .build(),
        &["sw-rail-separator"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(key: &str, name: &str, role: Option<FolderRole>, count: u32) -> SidebarFolder {
        SidebarFolder {
            key: key.to_owned(),
            name: name.to_owned(),
            role,
            count,
        }
    }

    fn model() -> SidebarModel {
        SidebarModel {
            accounts: vec![
                SidebarAccount {
                    key: "a".to_owned(),
                    name: "Personal".to_owned(),
                    color: None,
                    status: AccountStatus::Synced,
                    folders: vec![
                        folder("a/trash", "Papirkurv", Some(FolderRole::Trash), 0),
                        folder("a/inbox", "INBOX", Some(FolderRole::Inbox), 12),
                        folder("a/drafts", "Drafts", Some(FolderRole::Drafts), 2),
                        folder("a/sent", "Sendt", Some(FolderRole::Sent), 0),
                    ],
                    more: vec![folder("a/receipts", "Receipts", None, 3)],
                },
                SidebarAccount {
                    key: "b".to_owned(),
                    name: "Work".to_owned(),
                    color: None,
                    status: AccountStatus::Syncing,
                    folders: vec![folder("b/inbox", "INBOX", Some(FolderRole::Inbox), 4)],
                    more: vec![],
                },
            ],
        }
    }

    #[test]
    fn role_folders_show_in_role_order_with_their_role_names() {
        let model = model();
        let titles: Vec<&str> = model.accounts[0]
            .role_folders()
            .into_iter()
            .map(SidebarFolder::title)
            .collect();
        assert_eq!(titles, ["Inbox", "Drafts", "Sent", "Trash"]);
        assert_eq!(model.find("a/receipts").unwrap().1.title(), "Receipts");
    }

    #[test]
    fn counts_and_headings() {
        let model = model();
        assert_eq!(model.all_inboxes_unread(), 16);
        assert_eq!(
            model.heading(ALL_INBOXES),
            Some(("All Inboxes".into(), "16 unread · 2 accounts".into()))
        );
        assert_eq!(
            model.heading("a/inbox"),
            Some(("Inbox".into(), "12 unread · Personal".into()))
        );
        assert_eq!(
            model.heading("a/drafts"),
            Some(("Drafts".into(), "2 drafts · Personal".into()))
        );
        assert_eq!(
            model.heading("a/sent"),
            Some(("Sent".into(), "Personal".into()))
        );
        assert_eq!(model.heading("nope"), None);
        assert_eq!(SidebarModel::default().heading(ALL_INBOXES), None);
    }

    #[test]
    fn selection_defaults_and_validity() {
        let model = model();
        assert_eq!(model.default_selection().as_deref(), Some("a/inbox"));
        assert!(model.contains(ALL_INBOXES) && model.contains("a/receipts"));
        assert!(!model.contains("c/inbox"));
        let empty = SidebarModel::default();
        assert_eq!(empty.default_selection(), None);
        assert!(!empty.contains(ALL_INBOXES));
        assert_eq!(model.current_account("b/inbox").unwrap().name, "Work");
        assert_eq!(model.current_account(ALL_INBOXES).unwrap().name, "Personal");
    }

    #[test]
    fn summaries() {
        let mut model = model();
        assert_eq!(model.summary(), "2 accounts · syncing…");
        model.accounts[1].status = AccountStatus::Synced;
        assert_eq!(model.summary(), "2 accounts · all synced");
        model.accounts[0].status = AccountStatus::NeedsSignIn;
        model.accounts[1].status = AccountStatus::Offline;
        assert_eq!(model.summary(), "2 accounts · sign-in needed");
        model.accounts[1].status = AccountStatus::CertificateChanged;
        assert_eq!(model.summary(), "2 accounts · bad certificate");
        model.accounts[0].status = AccountStatus::Offline;
        model.accounts[1].status = AccountStatus::QuotaFull;
        assert_eq!(model.summary(), "2 accounts · mailbox full");
        model.accounts.truncate(1);
        model.accounts[0].status = AccountStatus::Synced;
        assert_eq!(model.summary(), "1 account · synced");
        assert_eq!(SidebarModel::default().summary(), "No accounts");
    }

    #[test]
    fn every_state_has_words_and_the_rail_marks_those_that_need_the_user() {
        let states = [
            (AccountStatus::Synced, "synced", Some("sw-synced"), false),
            (AccountStatus::Syncing, "syncing…", None, false),
            (AccountStatus::Offline, "offline", Some("sw-offline"), true),
            (
                AccountStatus::NeedsSignIn,
                "needs sign-in",
                Some("sw-attention"),
                true,
            ),
            (
                AccountStatus::CertificateChanged,
                "bad certificate",
                Some("sw-certificate"),
                true,
            ),
            (
                AccountStatus::QuotaFull,
                "mailbox full",
                Some("sw-quota"),
                true,
            ),
        ];
        for (status, words, class, marked) in states {
            assert_eq!(status.label(), words);
            assert_eq!(status.class(), class);
            assert_eq!(status.needs_attention(), marked, "{words}");
        }
    }

    #[test]
    fn accessible_labels_say_what_the_counts_mean() {
        let model = model();
        assert_eq!(
            model.find("a/inbox").unwrap().1.accessible_label(),
            "Inbox, 12 unread"
        );
        assert_eq!(
            model.find("a/drafts").unwrap().1.accessible_label(),
            "Drafts, 2 drafts"
        );
        assert_eq!(model.find("a/sent").unwrap().1.accessible_label(), "Sent");
    }
}
