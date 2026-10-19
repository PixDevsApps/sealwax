//! The folder sidebar (DESIGN.md "Components"): the compose button; "All Inboxes"; each account
//! with its status and its role folders (Inbox, Drafts, Sent, Archive, Junk, Trash) with their
//! counts, and its other folders under a collapsible "More"; and a footer with the accounts'
//! state and `config.toml`. Folders under a folder the sidebar shows are indented under it, a
//! level a step, and a folder with subfolders folds them away (M9, ADR-020), its count adding
//! their unread mail while they are folded. Between 900 and 1280 px it becomes a rail of icons
//! with unread badges and the other accounts (DESIGN.md "Layout", the mock's medium window).
//! The main menu is in the list's bar.
//!
//! It shows a plain-data [`SidebarModel`], which the store fills (ML-023). Picking a folder
//! activates `nav.folder` with the folder's key; the window answers with
//! [`Sidebar::set_selected`]. For the self-test, rows are named `folder:<key>` (their counts
//! `count:<key>`, their fold arrows `folder-arrow:<key>`), account headings `heading:<account>`
//! (their dots `status:<account>`, their names `account-name:<account>`, their words
//! `status-text:<account>`), rail buttons `rail:<key>` (their badges `badge:<key>`) and
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
    /// The folder's own name: its last level under a folder the sidebar shows, else its whole
    /// name; a role folder shows its role's name instead.
    pub name: String,
    pub role: Option<FolderRole>,
    /// Unread messages; for Drafts, the drafts.
    pub count: u32,
    /// The folders under it, shown indented under it in this order.
    pub children: Vec<SidebarFolder>,
    /// Its subfolders are folded away (the window's remembered layout).
    pub folded: bool,
}

impl SidebarFolder {
    /// It and every folder under it, depth first, each with its level under it (0 for itself).
    pub fn walk(&self) -> Vec<(&SidebarFolder, usize)> {
        let mut out = vec![(self, 0)];
        for child in &self.children {
            out.extend(
                child
                    .walk()
                    .into_iter()
                    .map(|(folder, level)| (folder, level + 1)),
            );
        }
        out
    }

    /// The folder `key`: itself or one under it.
    fn find(&self, key: &str) -> Option<&SidebarFolder> {
        if self.key == key {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(key))
    }

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

    /// Unread messages in the folders under it, at every level (they have no role, so their
    /// counts are unread mail).
    pub fn unread_below(&self) -> u32 {
        self.children.iter().fold(0, |sum, child| {
            sum.saturating_add(child.count)
                .saturating_add(child.unread_below())
        })
    }

    /// The count its row shows: its own, and while its subfolders are folded away, their unread
    /// too, as Thunderbird counts. Drafts counts drafts, so it keeps its own.
    pub fn shown_count(&self, folded: bool) -> u32 {
        if folded && self.role != Some(FolderRole::Drafts) {
            self.count.saturating_add(self.unread_below())
        } else {
            self.count
        }
    }

    /// Unread mail in the Inbox or in the user's own folders stands out; the other counts don't.
    fn count_stands_out(&self) -> bool {
        matches!(self.role, None | Some(FolderRole::Inbox))
    }

    /// "Kunder, 3 unread"; folded with unread mail under it, "Kunder, 5 unread with its
    /// subfolders".
    fn accessible_label(&self, folded: bool) -> String {
        let shown = self.shown_count(folded);
        if shown > self.count {
            return format!("{}, {shown} unread with its subfolders", self.title());
        }
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
    /// Its folders are folded away under its heading (the window's remembered layout).
    pub collapsed: bool,
    /// Whether its "More" was left open (the remembered layout); `None` for an account the
    /// layout can't name (no key), whose "More" keeps its state only while Sealwax runs.
    pub more_open: Option<bool>,
    pub status: AccountStatus,
    /// Its role folders, each with the folders under it; shown in [`FolderRole`] order.
    pub folders: Vec<SidebarFolder>,
    /// Every other folder under no folder the sidebar shows, with the folders under it, under
    /// "More".
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
        self.find(key).is_some()
    }

    /// The folder `key`, at any level.
    fn find(&self, key: &str) -> Option<&SidebarFolder> {
        self.folders
            .iter()
            .chain(&self.more)
            .find_map(|folder| folder.find(key))
    }

    /// Whether the folder `key` is under "More".
    fn in_more(&self, key: &str) -> bool {
        self.more.iter().any(|folder| folder.find(key).is_some())
    }
}

/// What the sidebar shows: plain data, filled by the store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarModel {
    pub accounts: Vec<SidebarAccount>,
    /// The view a new window opens on when the sidebar still has it: the one the last window
    /// showed (issue #5).
    pub preferred: Option<String>,
}

impl SidebarModel {
    /// Unread messages in every account's Inbox: "All Inboxes".
    pub fn all_inboxes_unread(&self) -> u32 {
        self.accounts.iter().map(SidebarAccount::unread).sum()
    }

    pub fn find(&self, key: &str) -> Option<(&SidebarAccount, &SidebarFolder)> {
        self.accounts
            .iter()
            .find_map(|account| account.find(key).map(|folder| (account, folder)))
    }

    /// Whether `key` names something to show: a folder, or All Inboxes once an account exists.
    pub fn contains(&self, key: &str) -> bool {
        (key == ALL_INBOXES && !self.accounts.is_empty()) || self.find(key).is_some()
    }

    /// What a new window shows: the view the last one showed, else the first account's Inbox.
    pub fn default_selection(&self) -> Option<String> {
        if let Some(preferred) = self.preferred.as_ref().filter(|key| self.contains(key)) {
            return Some(preferred.clone());
        }
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
    /// The "More" row's, a heading's or a parent folder's arrow.
    arrow: Option<gtk::Image>,
    /// The folders above this one in the tree, the nearest first.
    ancestors: Vec<String>,
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
    /// Whether New Subfolder, Rename Folder and Delete Folder apply to a folder key (why not),
    /// and what they do.
    folder_commands: RefCell<Option<FolderCommands>>,
    /// What an account heading's Edit Account does, given the account's key.
    edit_account: RefCell<Option<EditAccount>>,
    /// What an account heading's New Folder does, given the account's key.
    new_folder: RefCell<Option<EditAccount>>,
    /// What an account heading's Rules… does, given the account's key.
    edit_rules: RefCell<Option<EditAccount>>,
    /// Whether Apply Rules to This Folder applies to a folder key (why not), and what it does.
    apply_rules: RefCell<Option<ApplyRules>>,
    /// The accounts whose folders are folded away, by key.
    collapsed: RefCell<HashSet<String>>,
    /// Told when an account is folded away (true) or opened again.
    collapse_changed: RefCell<Option<CollapseChanged>>,
    /// The folders whose subfolders are folded away, by key.
    folded: RefCell<HashSet<String>>,
    /// Told when a folder's subfolders are folded away (true) or opened again.
    fold_changed: RefCell<Option<CollapseChanged>>,
    more_changed: RefCell<Option<CollapseChanged>>,
    weak: RefCell<std::rc::Weak<Sidebar>>,
}

type MarkRead = (Rc<dyn Fn(&str) -> bool>, Rc<dyn Fn(&str)>);
type FolderCommands = (
    Rc<dyn Fn(ActionId, &str) -> Result<(), &'static str>>,
    Rc<dyn Fn(ActionId, &str)>,
);
type EditAccount = Rc<dyn Fn(&str)>;
type ApplyRules = (
    Rc<dyn Fn(&str) -> Result<(), &'static str>>,
    Rc<dyn Fn(&str)>,
);
type CollapseChanged = Rc<dyn Fn(&str, bool)>;

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
            folder_commands: RefCell::new(None),
            edit_account: RefCell::new(None),
            new_folder: RefCell::new(None),
            edit_rules: RefCell::new(None),
            apply_rules: RefCell::new(None),
            collapsed: RefCell::new(HashSet::new()),
            collapse_changed: RefCell::new(None),
            folded: RefCell::new(HashSet::new()),
            fold_changed: RefCell::new(None),
            more_changed: RefCell::new(None),
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

    /// Where the keyboard focus enters the folder list: the folder shown, else the nearest
    /// folder above it that shows (its subfolders folded), else the heading of its folded
    /// account, else the first row.
    fn entry_row(&self) -> Option<gtk::ListBoxRow> {
        if !self.list.is_mapped() {
            return None;
        }
        let rows = self.rows.borrow();
        let usable = |row: &&Row| row.widget.is_mapped() && row.widget.is_sensitive();
        let folded = self
            .model
            .borrow()
            .accounts
            .iter()
            .find(|account| account.contains(&self.selected.borrow()))
            .map(|account| RowKind::Header(account.key.clone()));
        let above: Vec<RowKind> = rows
            .iter()
            .find(|row| row.widget.has_css_class("sw-current"))
            .map(|row| {
                row.ancestors
                    .iter()
                    .map(|key| RowKind::Folder(key.clone()))
                    .collect()
            })
            .unwrap_or_default();
        rows.iter()
            .filter(usable)
            .find(|row| row.widget.has_css_class("sw-current"))
            .or_else(|| {
                above
                    .iter()
                    .find_map(|kind| rows.iter().filter(usable).find(|row| row.kind == *kind))
            })
            .or_else(|| {
                rows.iter()
                    .filter(usable)
                    .find(|row| Some(&row.kind) == folded.as_ref())
            })
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
        {
            let mut expanded = self.expanded.borrow_mut();
            expanded.retain(|account| model.accounts.iter().any(|a| &a.key == account));
            for account in &model.accounts {
                match account.more_open {
                    Some(true) => {
                        expanded.insert(account.key.clone());
                    }
                    Some(false) => {
                        expanded.remove(&account.key);
                    }
                    None => {}
                }
            }
        }
        self.collapsed.replace(
            model
                .accounts
                .iter()
                .filter(|account| account.collapsed)
                .map(|account| account.key.clone())
                .collect(),
        );
        self.folded.replace(
            model
                .accounts
                .iter()
                .flat_map(|account| account.folders.iter().chain(&account.more))
                .flat_map(SidebarFolder::walk)
                .filter(|(folder, _)| folder.folded && !folder.children.is_empty())
                .map(|(folder, _)| folder.key.clone())
                .collect(),
        );
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
        let changed = *self.selected.borrow() != key;
        self.selected.replace(key.to_owned());
        let unfolded = changed && self.unfold_above(key);
        if self.reveal_selected() || unfolded {
            self.rebuild_list();
        } else {
            self.sync_selection();
        }
        if account_changed {
            self.rebuild_rail();
        }
    }

    /// Opens the folded folders above the folder `key`, so it shows; true when one was folded.
    fn unfold_above(&self, key: &str) -> bool {
        let above = ancestors_of(&self.model.borrow(), key);
        let opened: Vec<String> = {
            let mut folded = self.folded.borrow_mut();
            above
                .into_iter()
                .filter(|folder| folded.remove(folder))
                .collect()
        };
        let handler = self.fold_changed.borrow().clone();
        if let Some(handler) = handler {
            for folder in &opened {
                handler(folder, false);
            }
        }
        !opened.is_empty()
    }

    /// The key of the account whose folders hold `key`.
    pub fn account_of(&self, key: &str) -> Option<String> {
        self.model
            .borrow()
            .accounts
            .iter()
            .find(|account| account.contains(key))
            .map(|account| account.key.clone())
    }

    /// Whether the folder `key`'s subfolders are folded away.
    pub fn is_folded(&self, key: &str) -> bool {
        self.folded.borrow().contains(key)
    }

    /// Folds the subfolders of the folder `key` away, or opens them again (a click on its
    /// arrow; Left folds, Right opens), its count adding their unread while folded. The folder
    /// shown stays shown; a row that had the keyboard focus keeps it.
    pub fn toggle_folded(&self, key: &str) {
        let has_children = self
            .model
            .borrow()
            .find(key)
            .is_some_and(|(_, folder)| !folder.children.is_empty());
        if !has_children {
            return;
        }
        let folded = {
            let mut set = self.folded.borrow_mut();
            if set.remove(key) {
                false
            } else {
                set.insert(key.to_owned());
                true
            }
        };
        self.rebuild_list();
        let handler = self.fold_changed.borrow().clone();
        if let Some(handler) = handler {
            handler(key, folded);
        }
    }

    /// Runs `handler` with a folder's key when its subfolders are folded away (true) or opened
    /// again.
    pub fn connect_folded(&self, handler: impl Fn(&str, bool) + 'static) {
        self.fold_changed.replace(Some(Rc::new(handler)));
    }

    /// Shows the rows whose "More" is open and whose folders above them aren't folded.
    fn apply_visibility(&self) {
        let expanded = self.expanded.borrow();
        let folded = self.folded.borrow();
        for row in self.rows.borrow().iter() {
            let more_open = row
                .more_of
                .as_ref()
                .is_none_or(|account| expanded.contains(account));
            let unfolded = row.ancestors.iter().all(|key| !folded.contains(key));
            row.widget.set_visible(more_open && unfolded);
        }
    }

    pub fn is_expanded(&self, account: &str) -> bool {
        self.expanded.borrow().contains(account)
    }

    /// Whether `account`'s folders are folded away under its heading.
    pub fn is_collapsed(&self, account: &str) -> bool {
        self.collapsed.borrow().contains(account)
    }

    /// Folds `account`'s folders away under its heading, or opens them again (a click, Enter
    /// or Space on the heading; Left folds, Right opens). The folder shown stays shown.
    pub fn toggle_collapsed(&self, account: &str) {
        if !self
            .model
            .borrow()
            .accounts
            .iter()
            .any(|a| a.key == account)
        {
            return;
        }
        let collapsed = {
            let mut set = self.collapsed.borrow_mut();
            if set.remove(account) {
                false
            } else {
                set.insert(account.to_owned());
                true
            }
        };
        self.rebuild_list();
        let handler = self.collapse_changed.borrow().clone();
        if let Some(handler) = handler {
            handler(account, collapsed);
        }
    }

    /// Runs `handler` with the account's key when it is folded away (true) or opened again.
    pub fn connect_collapsed(&self, handler: impl Fn(&str, bool) + 'static) {
        self.collapse_changed.replace(Some(Rc::new(handler)));
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

    /// Adds New Subfolder…, Rename Folder… and Delete Folder… to every folder row's context
    /// menu (M9, ADR-020): `applies` says whether the command applies to the folder's key, or
    /// why not (the item is then insensitive, its tooltip the reason); `activate` runs it.
    pub fn connect_folder_commands(
        &self,
        applies: impl Fn(ActionId, &str) -> Result<(), &'static str> + 'static,
        activate: impl Fn(ActionId, &str) + 'static,
    ) {
        self.folder_commands
            .replace(Some((Rc::new(applies), Rc::new(activate))));
    }

    /// Gives every account heading a context menu with Edit Account (the user's UI tweak,
    /// 2026-10-08), opened by a right click; `activate` gets the account's key.
    pub fn connect_edit_account(&self, activate: impl Fn(&str) + 'static) {
        self.edit_account.replace(Some(Rc::new(activate)));
    }

    /// Adds New Folder… to every account heading's menu (M9): `activate` gets the account's key.
    pub fn connect_new_folder(&self, activate: impl Fn(&str) + 'static) {
        self.new_folder.replace(Some(Rc::new(activate)));
    }

    /// Adds Rules… to every account heading's menu (M9, ADR-020): `activate` gets the account's
    /// key.
    pub fn connect_edit_rules(&self, activate: impl Fn(&str) + 'static) {
        self.edit_rules.replace(Some(Rc::new(activate)));
    }

    /// Adds Apply Rules to This Folder to every folder row's menu (M9, ADR-020): `applies` says
    /// whether it applies to the folder's key, or why not; `activate` runs it.
    pub fn connect_apply_rules(
        &self,
        applies: impl Fn(&str) -> Result<(), &'static str> + 'static,
        activate: impl Fn(&str) + 'static,
    ) {
        self.apply_rules
            .replace(Some((Rc::new(applies), Rc::new(activate))));
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

    /// A folder row's menu: Mark Folder as Read, then (not on All Inboxes) New Subfolder…,
    /// Rename Folder… and Delete Folder…, each insensitive with the reason as its tooltip where
    /// it doesn't apply.
    fn show_menu(&self, row: &gtk::ListBoxRow, key: &str) {
        self.close_menu();
        let mut items: Vec<MenuItem> = Vec::new();
        if let Some((applies, activate)) = self.mark_read.borrow().clone() {
            let enabled = applies(key);
            let key = key.to_owned();
            items.push(MenuItem {
                name: "folder-menu-mark-read",
                label: ActionId::MarkFolderRead.title(),
                why_not: (!enabled).then_some("Nothing here is unread"),
                activate: Rc::new(move || activate(&key)),
            });
        }
        if key != ALL_INBOXES
            && let Some((applies, activate)) = self.folder_commands.borrow().clone()
        {
            for (id, name, label) in [
                (ActionId::NewFolder, "folder-menu-new", "New Subfolder…"),
                (
                    ActionId::RenameFolder,
                    "folder-menu-rename",
                    ActionId::RenameFolder.title(),
                ),
                (
                    ActionId::DeleteFolder,
                    "folder-menu-delete",
                    ActionId::DeleteFolder.title(),
                ),
            ] {
                let (activate, key) = (activate.clone(), key.to_owned());
                items.push(MenuItem {
                    name,
                    label,
                    why_not: applies(id, &key).err(),
                    // After the closed menu has put the focus back, so the dialog keeps it.
                    activate: Rc::new(move || {
                        let (activate, key) = (activate.clone(), key.clone());
                        glib::idle_add_local_once(move || activate(id, &key));
                    }),
                });
            }
        }
        if key != ALL_INBOXES
            && let Some((applies, activate)) = self.apply_rules.borrow().clone()
        {
            let key = key.to_owned();
            items.push(MenuItem {
                name: "folder-menu-apply-rules",
                label: ActionId::ApplyRules.title(),
                why_not: applies(&key).err(),
                activate: Rc::new(move || activate(&key)),
            });
        }
        if !items.is_empty() {
            self.popup(row, items, "folder-menu");
        }
    }

    /// An account heading's menu: Edit Account, Rules… and New Folder… for that account.
    fn show_account_menu(&self, row: &gtk::ListBoxRow, key: &str) {
        self.close_menu();
        let mut items: Vec<MenuItem> = Vec::new();
        for (callback, name, label) in [
            (
                self.edit_account.borrow().clone(),
                "account-menu-edit",
                ActionId::EditAccount.title(),
            ),
            (
                self.edit_rules.borrow().clone(),
                "account-menu-rules",
                ActionId::EditRules.title(),
            ),
            (
                self.new_folder.borrow().clone(),
                "account-menu-new-folder",
                ActionId::NewFolder.title(),
            ),
        ] {
            let Some(callback) = callback else {
                continue;
            };
            let key = key.to_owned();
            items.push(MenuItem {
                name,
                label,
                why_not: None,
                // After the closed menu has put the focus back, so the dialog keeps it.
                activate: Rc::new(move || {
                    let (callback, key) = (callback.clone(), key.clone());
                    glib::idle_add_local_once(move || callback(&key));
                }),
            });
        }
        if !items.is_empty() {
            self.popup(row, items, "account-menu");
        }
    }

    /// Shows `items` in a context menu named `name` under `row`; a click on one closes the menu
    /// and runs it.
    fn popup(&self, row: &gtk::ListBoxRow, items: Vec<MenuItem>, name: &str) {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let popover = named(
            classed(
                gtk::Popover::builder()
                    .child(&content)
                    .has_arrow(false)
                    .position(gtk::PositionType::Bottom)
                    .build(),
                &["sw-folder-menu"],
            ),
            name,
        );
        for item in items {
            let label = gtk::Label::builder().label(item.label).xalign(0.0).build();
            let button = named(
                classed(
                    gtk::Button::builder()
                        .child(&label)
                        .sensitive(item.why_not.is_none())
                        .build(),
                    &["flat", "sw-menu-item"],
                ),
                item.name,
            );
            button.set_tooltip_text(item.why_not);
            let activate = item.activate;
            button.connect_clicked(glib::clone!(
                #[weak]
                popover,
                move |_| {
                    popover.popdown();
                    activate();
                }
            ));
            content.append(&button);
        }
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
        popover.popup();
        self.menu.replace(Some(popover));
    }

    /// The open menu's items, in order.
    pub fn menu_items(&self) -> Vec<gtk::Button> {
        let mut buttons = Vec::new();
        let mut child = self
            .menu()
            .and_then(|menu| menu.child())
            .and_then(|content| content.first_child());
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(button) = widget.downcast::<gtk::Button>() {
                buttons.push(button);
            }
        }
        buttons
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

    /// A right click, Menu or Shift+F10 on a folder row opens its context menu; on a folder
    /// with subfolders, Left folds them away and Right opens them.
    fn add_menu_triggers(&self, row: &gtk::ListBoxRow, key: &str, parent: bool) {
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
            let (Some(sidebar), Some(row)) = (
                weak.upgrade(),
                controller.widget().and_downcast::<gtk::ListBoxRow>(),
            ) else {
                return glib::Propagation::Proceed;
            };
            let menu = key == gtk::gdk::Key::Menu
                || (key == gtk::gdk::Key::F10
                    && state.contains(gtk::gdk::ModifierType::SHIFT_MASK));
            let folded = sidebar.is_folded(&target);
            let fold = parent
                && match key {
                    gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => !folded,
                    gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => folded,
                    _ => false,
                };
            if menu {
                sidebar.show_menu(&row, &target);
                glib::Propagation::Stop
            } else if fold {
                sidebar.toggle_folded(&target);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        row.add_controller(keys);
    }

    /// A click on a parent folder's arrow folds its subfolders away or opens them; the row
    /// isn't picked.
    fn add_fold_trigger(&self, arrow: &gtk::Image, key: &str) {
        let click = gtk::GestureClick::builder().button(1).build();
        let weak = self.weak.borrow().clone();
        let target = key.to_owned();
        click.connect_pressed(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(sidebar) = weak.upgrade() {
                sidebar.toggle_folded(&target);
            }
        });
        arrow.add_controller(click);
    }

    /// A right click, Menu or Shift+F10 on an account heading opens its menu; Left folds the
    /// account's folders away and Right opens them (a click, Enter or Space toggles).
    fn add_heading_triggers(&self, row: &gtk::ListBoxRow, key: &str) {
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak.borrow().clone();
        let target = key.to_owned();
        keys.connect_key_pressed(move |controller, key, _, state| {
            let (Some(sidebar), Some(row)) = (
                weak.upgrade(),
                controller.widget().and_downcast::<gtk::ListBoxRow>(),
            ) else {
                return glib::Propagation::Proceed;
            };
            let menu = key == gtk::gdk::Key::Menu
                || (key == gtk::gdk::Key::F10
                    && state.contains(gtk::gdk::ModifierType::SHIFT_MASK));
            let collapsed = sidebar.is_collapsed(&target);
            let fold = match key {
                gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => !collapsed,
                gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => collapsed,
                _ => false,
            };
            if menu {
                sidebar.show_account_menu(&row, &target);
                glib::Propagation::Stop
            } else if fold {
                sidebar.toggle_collapsed(&target);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        row.add_controller(keys);
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
            if row.kind == RowKind::More(account.to_owned()) {
                set_expanded(row, expanded);
            }
        }
        self.apply_visibility();
        self.more_changed(account, expanded);
    }

    /// Runs `handler` with the account's key when its "More" opens (true) or closes.
    pub fn connect_more(&self, handler: impl Fn(&str, bool) + 'static) {
        self.more_changed.replace(Some(Rc::new(handler)));
    }

    fn more_changed(&self, account: &str, open: bool) {
        let handler = self.more_changed.borrow().clone();
        if let Some(handler) = handler {
            handler(account, open);
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

    /// The folder rows of the account `account` the user can see, in order, by the names they
    /// show and their levels in the tree.
    pub fn shown_tree(&self, account: &str) -> Vec<(String, usize)> {
        let model = self.model.borrow();
        let mut within = false;
        let mut shown = Vec::new();
        for row in self.rows.borrow().iter() {
            match &row.kind {
                RowKind::Header(key) => within = key == account,
                RowKind::Folder(key) if within && row.widget.get_visible() => {
                    if let Some((_, folder)) = model.find(key) {
                        shown.push((folder.title().to_owned(), row.ancestors.len()));
                    }
                }
                _ => {}
            }
        }
        shown
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
            .find(|account| account.in_more(&selected));
        let opened = holder
            .filter(|account| self.expanded.borrow_mut().insert(account.key.clone()))
            .map(|account| account.key.clone());
        drop(selected);
        drop(model);
        match opened {
            Some(account) => {
                self.more_changed(&account, true);
                true
            }
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
            Some(RowKind::Header(account)) => ("nav.collapse", account),
            None => return,
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
        let mut rows = Vec::new();
        if !model.accounts.is_empty() {
            rows.push(folder_row(
                &all_inboxes(&model),
                ALL_INBOXES_ICONS,
                "sw-all-inboxes",
                false,
                None,
            ));
        }
        let collapsed = self.collapsed.borrow();
        let expanded = self.expanded.borrow();
        let folded = self.folded.borrow();
        for account in &model.accounts {
            let away = collapsed.contains(&account.key);
            rows.push(header_row(account, away));
            if away {
                continue;
            }
            for folder in account.role_folders() {
                tree_rows(&mut rows, folder, None, &[], &folded);
            }
            if account.more.is_empty() {
                continue;
            }
            rows.push(more_row(account, expanded.contains(&account.key)));
            for folder in &account.more {
                tree_rows(&mut rows, folder, Some(&account.key), &[], &folded);
            }
        }
        drop(folded);
        drop(expanded);
        drop(collapsed);
        drop(model);
        for row in &rows {
            match &row.kind {
                RowKind::Folder(key) => {
                    self.add_menu_triggers(&row.widget, key, row.arrow.is_some());
                    if let Some(arrow) = &row.arrow {
                        self.add_fold_trigger(arrow, key);
                    }
                }
                RowKind::Header(key) => self.add_heading_triggers(&row.widget, key),
                RowKind::More(_) => {}
            }
            self.list.append(&row.widget);
        }
        self.rows.replace(rows);
        self.apply_visibility();
        if let Some(kind) = focused
            && let Some(row) = self
                .rows
                .borrow()
                .iter()
                .find(|row| row.kind == kind && row.widget.is_visible())
        {
            row.widget.grab_focus();
        }
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
        children: Vec::new(),
        folded: false,
    }
}

fn count_label(count: u32, stands_out: bool) -> gtk::Label {
    let label = classed(gtk::Label::new(Some(&count.to_string())), &["sw-count"]);
    if stands_out {
        label.add_css_class("sw-unread");
    }
    label
}

/// How far each level of the folder tree is indented, in px.
const INDENT: i32 = 16;

/// One item of a folder row's or an account heading's menu.
struct MenuItem {
    name: &'static str,
    label: &'static str,
    /// Why it doesn't apply, its tooltip; the item is insensitive then.
    why_not: Option<&'static str>,
    activate: Rc<dyn Fn()>,
}

/// Where a folder row sits in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Place {
    /// 0 at the top of its section.
    level: usize,
    /// It has subfolders, folded away when `folded`.
    parent: bool,
    folded: bool,
}

/// `folder`'s row and the rows of the folders under it, depth first: role folders take their
/// role's icon, the others the folder icon; `more_of` is the account whose "More" holds them.
fn tree_rows(
    rows: &mut Vec<Row>,
    folder: &SidebarFolder,
    more_of: Option<&str>,
    ancestors: &[String],
    folded: &HashSet<String>,
) {
    let (icons, class) = folder.role.map_or((FOLDER_ICONS, "sw-role-other"), |role| {
        (role.icons(), role.class())
    });
    let place = Place {
        level: ancestors.len(),
        parent: !folder.children.is_empty(),
        folded: folded.contains(&folder.key),
    };
    let mut row = folder_row(folder, icons, class, folder.count_stands_out(), Some(place));
    row.more_of = more_of.map(str::to_owned);
    row.ancestors = ancestors.to_vec();
    rows.push(row);
    let mut below = vec![folder.key.clone()];
    below.extend_from_slice(ancestors);
    for child in &folder.children {
        tree_rows(rows, child, more_of, &below, folded);
    }
}

/// The keys of the folders above `key` in the tree, the nearest first.
fn ancestors_of(model: &SidebarModel, key: &str) -> Vec<String> {
    fn path(folder: &SidebarFolder, key: &str, above: &mut Vec<String>) -> bool {
        if folder.key == key {
            return true;
        }
        above.push(folder.key.clone());
        if folder.children.iter().any(|child| path(child, key, above)) {
            return true;
        }
        above.pop();
        false
    }
    for account in &model.accounts {
        for folder in account.folders.iter().chain(&account.more) {
            let mut above = Vec::new();
            if path(folder, key, &mut above) {
                above.reverse();
                return above;
            }
        }
    }
    Vec::new()
}

fn folder_row(
    folder: &SidebarFolder,
    icons: &[&str],
    class: &str,
    stands_out: bool,
    place: Option<Place>,
) -> Row {
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
    let folded = place.is_some_and(|place| place.parent && place.folded);
    let count = folder.shown_count(folded);
    if count > 0 {
        content.append(&named(
            count_label(count, stands_out),
            &format!("count:{}", folder.key),
        ));
    }
    let arrow = place.filter(|place| place.parent).map(|_| {
        let arrow = named(
            classed(gtk::Image::new(), &["sw-folder-arrow"]),
            &format!("folder-arrow:{}", folder.key),
        );
        content.append(&arrow);
        arrow
    });
    let level = place.map_or(0, |place| place.level);
    content.set_margin_start(INDENT * i32::try_from(level).unwrap_or(0));
    let widget = named(
        classed(
            gtk::ListBoxRow::builder().child(&content).build(),
            &["sw-sidebar-row", class],
        ),
        &format!("folder:{}", folder.key),
    );
    widget.update_property(&[
        gtk::accessible::Property::Label(&folder.accessible_label(folded)),
        gtk::accessible::Property::Level(i32::try_from(level + 1).unwrap_or(1)),
    ]);
    let row = Row {
        widget,
        kind: RowKind::Folder(folder.key.clone()),
        more_of: None,
        arrow,
        ancestors: Vec::new(),
    };
    if let Some(place) = place.filter(|place| place.parent) {
        set_expanded(&row, !place.folded);
    }
    row
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

/// The account's heading: its status dot, its name (upper-cased by `style.css`), its unread
/// count while its folders are folded away, its status in words and the arrow that folds and
/// opens it, the dot and the name in the account's colour.
fn header_row(account: &SidebarAccount, collapsed: bool) -> Row {
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
    let unread = account.unread();
    if collapsed && unread > 0 {
        content.append(&named(
            count_label(unread, true),
            &format!("heading-count:{}", account.key),
        ));
    }
    content.append(&named(
        classed(
            gtk::Label::new(Some(account.status.label())),
            &["sw-account-status"],
        ),
        &format!("status-text:{}", account.key),
    ));
    let arrow = named(
        classed(gtk::Image::new(), &["sw-heading-arrow"]),
        &format!("heading-arrow:{}", account.key),
    );
    content.append(&arrow);
    let widget = named(
        classed(
            gtk::ListBoxRow::builder()
                .child(&content)
                .selectable(false)
                .build(),
            &["sw-sidebar-heading"],
        ),
        &format!("heading:{}", account.key),
    );
    let mut label = format!("{}, {}", account.name, account.status.label());
    if collapsed && unread > 0 {
        label.push_str(&format!(", {unread} unread"));
    }
    widget.update_property(&[gtk::accessible::Property::Label(&label)]);
    let row = Row {
        widget,
        kind: RowKind::Header(account.key.clone()),
        more_of: None,
        arrow: Some(arrow),
        ancestors: Vec::new(),
    };
    set_expanded(&row, !collapsed);
    row
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
        ancestors: Vec::new(),
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
            children: Vec::new(),
            folded: false,
        }
    }

    fn model() -> SidebarModel {
        SidebarModel {
            preferred: None,
            accounts: vec![
                SidebarAccount {
                    key: "a".to_owned(),
                    name: "Personal".to_owned(),
                    color: None,
                    collapsed: false,
                    more_open: None,
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
                    collapsed: false,
                    more_open: None,
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
            model.find("a/inbox").unwrap().1.accessible_label(false),
            "Inbox, 12 unread"
        );
        assert_eq!(
            model.find("a/drafts").unwrap().1.accessible_label(false),
            "Drafts, 2 drafts"
        );
        assert_eq!(
            model.find("a/sent").unwrap().1.accessible_label(false),
            "Sent"
        );
    }

    /// Kunder (1 unread) holds Acme (2) and Fjord (0), which holds 2026 (4).
    fn customers() -> SidebarFolder {
        let mut fjord = folder("fjord", "Fjord", None, 0);
        fjord.children = vec![folder("2026", "2026", None, 4)];
        let mut kunder = folder("kunder", "Kunder", None, 1);
        kunder.children = vec![folder("acme", "Acme", None, 2), fjord];
        kunder
    }

    #[test]
    fn a_folded_folder_counts_its_subfolders_unread_at_every_level() {
        let kunder = customers();
        assert_eq!(kunder.unread_below(), 6);
        assert_eq!(kunder.shown_count(true), 7);
        assert_eq!(kunder.shown_count(false), 1);
        let fjord = kunder.find("fjord").unwrap();
        assert_eq!(fjord.shown_count(true), 4);
        assert_eq!(fjord.shown_count(false), 0);
        let acme = kunder.find("acme").unwrap();
        assert_eq!(acme.shown_count(true), 2);
        assert_eq!(
            kunder.accessible_label(true),
            "Kunder, 7 unread with its subfolders"
        );
        assert_eq!(kunder.accessible_label(false), "Kunder, 1 unread");
        assert_eq!(
            fjord.accessible_label(true),
            "Fjord, 4 unread with its subfolders"
        );
        assert_eq!(fjord.accessible_label(false), "Fjord");
    }

    #[test]
    fn a_folded_folder_without_unread_below_keeps_its_own_count_and_words() {
        let mut inbox = folder("inbox", "INBOX", Some(FolderRole::Inbox), 3);
        inbox.children = vec![folder("receipts", "Kvitteringer", None, 0)];
        assert_eq!(inbox.shown_count(true), 3);
        assert_eq!(inbox.accessible_label(true), "Inbox, 3 unread");
        let mut drafts = folder("drafts", "Drafts", Some(FolderRole::Drafts), 2);
        drafts.children = vec![folder("old", "Old", None, 5)];
        assert_eq!(drafts.shown_count(true), 2);
        assert_eq!(drafts.accessible_label(true), "Drafts, 2 drafts");
        let mut huge = folder("huge", "Huge", None, u32::MAX);
        huge.children = vec![folder("more", "More", None, 1)];
        assert_eq!(huge.shown_count(true), u32::MAX);
    }

    #[test]
    fn folding_leaves_all_inboxes_and_the_headings_alone() {
        let mut model = model();
        model.accounts[0].folders[1].children =
            vec![folder("a/receipts-2", "Kvitteringer", None, 5)];
        model.accounts[0].folders[1].folded = true;
        assert_eq!(model.all_inboxes_unread(), 16);
        assert_eq!(model.accounts[0].unread(), 12);
        assert_eq!(
            model.heading("a/inbox"),
            Some(("Inbox".into(), "12 unread · Personal".into()))
        );
    }
}
