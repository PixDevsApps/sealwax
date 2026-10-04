//! The folder sidebar (DESIGN.md "Components"): the compose button; "All Inboxes"; each account
//! with its status and its role folders (Inbox, Drafts, Sent, Archive, Junk, Trash) with their
//! counts, and its other folders under a collapsible "More"; and a footer with the accounts'
//! state. Between 900 and 1280 px it becomes a rail of icons and counts (DESIGN.md "Layout").
//!
//! It shows a plain-data [`SidebarModel`], which the store fills (ML-023). Picking a folder
//! activates `nav.folder` with the folder's key; the window answers with
//! [`Sidebar::set_selected`].

use super::panes::{PaneBar, classed, icon, icon_button, named};
use super::{COMPOSE_ACTION, OPEN_CONFIG_ACTION};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

/// `nav.folder`'s key for the unified inbox.
pub const ALL_INBOXES: &str = "all-inboxes";

const ALL_INBOXES_ICONS: &[&str] = &["mail-unread-symbolic"];
const FOLDER_ICONS: &[&str] = &["folder-symbolic"];
pub(super) const COMPOSE_ICONS: &[&str] = &["mail-message-new-symbolic", "document-edit-symbolic"];
const CONFIG_ICONS: &[&str] = &["preferences-other-symbolic", "emblem-system-symbolic"];
const MENU_ICONS: &[&str] = &["open-menu-symbolic"];

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
            Self::Inbox => "sealwax-role-inbox",
            Self::Drafts => "sealwax-role-drafts",
            Self::Sent => "sealwax-role-sent",
            Self::Archive => "sealwax-role-archive",
            Self::Junk => "sealwax-role-junk",
            Self::Trash => "sealwax-role-trash",
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
}

impl AccountStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Syncing => "syncing…",
            Self::Offline => "offline",
            Self::NeedsSignIn => "needs sign-in",
        }
    }

    fn classes(self) -> [&'static str; 3] {
        match self {
            Self::Synced => ["sealwax-status-dot", "sealwax-status-synced", "accent"],
            Self::Syncing => ["sealwax-status-dot", "sealwax-status-syncing", "dim-label"],
            Self::Offline => ["sealwax-status-dot", "sealwax-status-offline", "dim-label"],
            Self::NeedsSignIn => [
                "sealwax-status-dot",
                "sealwax-status-needs-sign-in",
                "error",
            ],
        }
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
        let state = if has(AccountStatus::NeedsSignIn) {
            "sign-in needed"
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
    Header,
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
    /// The main menu beside the compose button; hidden in the rail, which has its own.
    pub menu: gtk::MenuButton,
    rail_menu: gtk::MenuButton,
    model: RefCell<SidebarModel>,
    selected: RefCell<String>,
    expanded: RefCell<HashSet<String>>,
}

impl Sidebar {
    pub fn new(tiling: bool) -> Rc<Self> {
        let compose_label = gtk::Label::new(Some("New message"));
        let compose_content = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        compose_content.append(&icon(COMPOSE_ICONS));
        compose_content.append(&compose_label);
        let compose = named(
            classed(
                gtk::Button::builder()
                    .child(&compose_content)
                    .hexpand(true)
                    .action_name(COMPOSE_ACTION)
                    .tooltip_text("New message")
                    .build(),
                &["suggested-action", "sealwax-compose"],
            ),
            "compose",
        );
        compose.update_property(&[gtk::accessible::Property::Label("New message")]);
        let menu = menu_button();
        let bar = PaneBar::new(tiling, None);
        bar.content.append(&compose);
        bar.content.append(&menu);
        bar.show_controls(true, false);

        let list = named(
            classed(
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::Single)
                    .build(),
                &["navigation-sidebar", "sealwax-folder-list"],
            ),
            "folder-list",
        );
        list.update_property(&[gtk::accessible::Property::Label("Folders")]);
        let summary = named(
            gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["caption", "dim-label"])
                .build(),
            "sidebar-summary",
        );
        let footer = classed(
            gtk::Box::builder().spacing(4).build(),
            &["toolbar", "sealwax-sidebar-footer"],
        );
        footer.append(&summary);
        footer.append(&config_button());
        let full = gtk::Box::new(gtk::Orientation::Vertical, 0);
        full.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build(),
        );
        full.append(&footer);

        let rail_items = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .halign(gtk::Align::Center)
            .margin_top(6)
            .build();
        let rail_footer = classed(
            gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .halign(gtk::Align::Center)
                .margin_bottom(6)
                .build(),
            &["sealwax-rail-footer"],
        );
        let rail_menu = menu_button();
        rail_footer.append(&config_button());
        rail_footer.append(&rail_menu);
        let rail = named(
            classed(
                gtk::Box::new(gtk::Orientation::Vertical, 0),
                &["sealwax-rail"],
            ),
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
                &["sealwax-sidebar", "sealwax-chrome"],
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
            menu,
            rail_menu,
            model: RefCell::new(SidebarModel::default()),
            selected: RefCell::new(String::new()),
            expanded: RefCell::new(HashSet::new()),
        });
        sidebar.list.connect_row_activated(glib::clone!(
            #[weak]
            sidebar,
            move |_, row| sidebar.activated(row)
        ));
        sidebar.rebuild();
        sidebar
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

    pub fn set_menu_model(&self, menu: Option<&gio::MenuModel>) {
        for button in [&self.menu, &self.rail_menu] {
            button.set_menu_model(menu);
        }
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
        self.list.remove_all();
        let model = self.model.borrow();
        let expanded = self.expanded.borrow();
        let mut rows = Vec::new();
        if !model.accounts.is_empty() {
            let all = SidebarFolder {
                key: ALL_INBOXES.to_owned(),
                name: "All Inboxes".to_owned(),
                role: None,
                count: model.all_inboxes_unread(),
            };
            rows.push(folder_row(&all, ALL_INBOXES_ICONS, "sealwax-all-inboxes"));
        }
        for account in &model.accounts {
            rows.push(header_row(account));
            for folder in account.role_folders() {
                let (icons, class) = folder
                    .role
                    .map_or((FOLDER_ICONS, "sealwax-role-other"), |role| {
                        (role.icons(), role.class())
                    });
                rows.push(folder_row(folder, icons, class));
            }
            if account.more.is_empty() {
                continue;
            }
            let open = expanded.contains(&account.key);
            rows.push(more_row(account, open));
            for folder in &account.more {
                let mut row = folder_row(folder, FOLDER_ICONS, "sealwax-role-other");
                row.widget.set_visible(open);
                row.more_of = Some(account.key.clone());
                rows.push(row);
            }
        }
        for row in &rows {
            self.list.append(&row.widget);
        }
        drop(expanded);
        drop(model);
        self.rows.replace(rows);
        self.sync_selection();
    }

    fn sync_selection(&self) {
        let selected = self.selected.borrow();
        let rows = self.rows.borrow();
        match rows
            .iter()
            .find(|row| row.kind == RowKind::Folder(selected.clone()))
        {
            Some(row) => self.list.select_row(Some(&row.widget)),
            None => self.list.unselect_all(),
        }
    }

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
            ALL_INBOXES_ICONS,
            "All Inboxes",
            model.all_inboxes_unread(),
            ALL_INBOXES,
            true,
        ));
        self.rail_items.append(&rail_separator());
        for folder in current.role_folders() {
            let icons = folder.role.map_or(FOLDER_ICONS, FolderRole::icons);
            self.rail_items.append(&rail_folder(
                icons,
                &format!("{} · {}", folder.title(), current.name),
                folder.count,
                &folder.key,
                folder.count_stands_out(),
            ));
        }
        self.rail_items.append(&rail_separator());
        for account in &model.accounts {
            self.rail_items
                .append(&rail_account(account, account.key == current.key));
        }
    }
}

pub(super) fn menu_button() -> gtk::MenuButton {
    let button = classed(
        gtk::MenuButton::builder()
            .icon_name(MENU_ICONS[0])
            .tooltip_text("Main menu")
            .build(),
        &["flat", "sealwax-menu"],
    );
    button.update_property(&[gtk::accessible::Property::Label("Main menu")]);
    button
}

fn config_button() -> gtk::Button {
    let button = icon_button(CONFIG_ICONS, "Open config.toml");
    button.set_action_name(Some(OPEN_CONFIG_ACTION));
    button.add_css_class("sealwax-config");
    button
}

fn count_label(count: u32, stands_out: bool) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(count.to_string())
        .css_classes(["numeric", "sealwax-count"])
        .build();
    label.add_css_class(if stands_out { "accent" } else { "dim-label" });
    if stands_out {
        label.add_css_class("sealwax-unread");
    }
    label
}

fn folder_row(folder: &SidebarFolder, icons: &[&str], class: &str) -> Row {
    let content = gtk::Box::builder().spacing(10).build();
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
        content.append(&count_label(folder.count, folder.count_stands_out()));
    }
    let widget = classed(
        gtk::ListBoxRow::builder().child(&content).build(),
        &["sealwax-folder-row", class],
    );
    widget.update_property(&[gtk::accessible::Property::Label(&folder.accessible_label())]);
    Row {
        widget,
        kind: RowKind::Folder(folder.key.clone()),
        more_of: None,
        arrow: None,
    }
}

fn status_dot(status: AccountStatus) -> gtk::Label {
    gtk::Label::builder()
        .label("●")
        .css_classes(status.classes())
        .build()
}

fn header_row(account: &SidebarAccount) -> Row {
    let content = gtk::Box::builder().spacing(8).margin_top(10).build();
    content.append(&status_dot(account.status));
    content.append(
        &gtk::Label::builder()
            .label(&account.name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption-heading", "dim-label"])
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label(account.status.label())
            .css_classes(["caption", "dim-label", "sealwax-account-status"])
            .build(),
    );
    let widget = classed(
        gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(false)
            .selectable(false)
            .focusable(false)
            .build(),
        &["sealwax-account-header"],
    );
    widget.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}, {}",
        account.name,
        account.status.label()
    ))]);
    Row {
        widget,
        kind: RowKind::Header,
        more_of: None,
        arrow: None,
    }
}

fn more_row(account: &SidebarAccount, open: bool) -> Row {
    let arrow = gtk::Image::new();
    let content = gtk::Box::builder().spacing(10).build();
    content.append(&arrow);
    content.append(
        &gtk::Label::builder()
            .label("More folders")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["dim-label"])
            .build(),
    );
    let widget = classed(
        gtk::ListBoxRow::builder()
            .child(&content)
            .selectable(false)
            .build(),
        &["sealwax-more-row"],
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

fn badge(count: u32, stands_out: bool) -> gtk::Label {
    let label = count_label(count, stands_out);
    label.add_css_class("caption");
    label.add_css_class("sealwax-badge");
    label.set_halign(gtk::Align::End);
    label.set_valign(gtk::Align::Start);
    label
}

fn rail_folder(
    icons: &[&str],
    name: &str,
    count: u32,
    key: &str,
    stands_out: bool,
) -> gtk::ToggleButton {
    let overlay = gtk::Overlay::builder()
        .child(&icon(icons))
        .width_request(36)
        .height_request(28)
        .build();
    let label = match count {
        0 => name.to_owned(),
        count => {
            overlay.add_overlay(&badge(count, stands_out));
            format!("{name}, {count}")
        }
    };
    let button = classed(
        gtk::ToggleButton::builder()
            .child(&overlay)
            .action_name("nav.folder")
            .action_target(&key.to_variant())
            .tooltip_text(&label)
            .build(),
        &["flat", "sealwax-rail-button"],
    );
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button
}

fn rail_account(account: &SidebarAccount, current: bool) -> gtk::Button {
    let initial: String = account
        .name
        .chars()
        .next()
        .map(|first| first.to_uppercase().collect())
        .unwrap_or_default();
    let letter = gtk::Label::builder()
        .label(initial)
        .css_classes(["heading"])
        .build();
    let overlay = gtk::Overlay::builder()
        .child(&letter)
        .width_request(36)
        .height_request(28)
        .build();
    let dot = status_dot(account.status);
    dot.set_halign(gtk::Align::End);
    dot.set_valign(gtk::Align::End);
    overlay.add_overlay(&dot);
    let unread = account.unread();
    if unread > 0 {
        overlay.add_overlay(&badge(unread, true));
    }
    let mut label = format!("{}, {}", account.name, account.status.label());
    if unread > 0 {
        label.push_str(&format!(", {unread} unread"));
    }
    let target = account
        .inbox()
        .or_else(|| account.folders.first())
        .map_or_else(|| ALL_INBOXES.to_owned(), |folder| folder.key.clone());
    let button = classed(
        gtk::Button::builder()
            .child(&overlay)
            .action_name("nav.folder")
            .action_target(&target.to_variant())
            .tooltip_text(&label)
            .build(),
        &["flat", "sealwax-rail-account"],
    );
    if current {
        button.add_css_class("sealwax-current");
    }
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
    button
}

fn rail_separator() -> gtk::Separator {
    gtk::Separator::builder()
        .orientation(gtk::Orientation::Horizontal)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(8)
        .margin_end(8)
        .build()
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
        model.accounts.truncate(1);
        model.accounts[0].status = AccountStatus::Synced;
        assert_eq!(model.summary(), "1 account · synced");
        assert_eq!(SidebarModel::default().summary(), "No accounts");
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
