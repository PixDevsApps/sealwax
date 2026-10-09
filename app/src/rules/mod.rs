//! Rules in the app (ADR-020, docs/m9/folders-rules.md, docs/m9/rules-app.md): the Rules
//! dialog and its editor, Create Rule from Message, Always and Never Junk Messages from Sender,
//! Apply Rules to This Folder, and what the engine reports about an account's rules.
//!
//! - **Files:** each account's Sieve script, `rules/<key>.sieve` in the config folder, is read
//!   and written by the app off the GTK thread ([`file`]); the engine reads it again when its
//!   modification time changes, so no command tells it: what is saved runs at the next new mail.
//! - **Always / Never Junk** put the sender's address on a list, save the file, and act on the
//!   selected messages at once (to Junk, or from Junk to the Inbox, with the junk keywords, as
//!   Move to Junk and Not Junk do), with Undo taking back both.
//! - **Events:** `RulesFailed` puts up the window's banner, "Rules in {account} can't run",
//!   with the line and column and Open File, until the file runs again (the folder is watched:
//!   a broken file shows as soon as it is saved, and a fixed one clears); `RulesApplied` is a
//!   toast. Nothing of a rule, an address or a folder reaches the log.

pub mod dialog;
pub mod draft;
pub mod editor;
pub mod file;
pub mod text;

use crate::application::AppState;
use crate::engine_link::Subscription;
use crate::monitor::{DirWatch, TriggerRule};
use crate::status::Waiting;
use crate::triage::{self, Target};
use crate::window::{Attention, Window};
use dialog::Start;
use draft::Draft;
use file::{ChangeError, Listed, Place, Unreadable};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use sealwax_domain::account::AccountKey;
use sealwax_domain::actions::ActionId;
use sealwax_domain::folders;
use sealwax_domain::model::{AccountId, Folder, FolderId, FolderRole, MessageId};
use sealwax_domain::sieve::{Action, Folders, Item, RuleSet};
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Event, RequestId, RulesError};
use sealwax_infrastructure::store::ViewScope;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::rc::{Rc, Weak};

/// The commands this module runs.
pub const COMMANDS: [ActionId; 5] = [
    ActionId::EditRules,
    ActionId::NewRuleFromMessage,
    ActionId::JunkSender,
    ActionId::TrustSender,
    ActionId::ApplyRules,
];

/// An account whose rules the app shows: one with a key, which names its file.
#[derive(Clone, Debug)]
pub struct Account {
    pub id: AccountId,
    pub key: AccountKey,
    /// What Sealwax calls it.
    pub name: String,
    /// Its folders as the store holds them.
    pub folders: Vec<Folder>,
}

/// The role folders in the sidebar's order, before the others in a folder menu.
const ROLES: [FolderRole; 7] = [
    FolderRole::Inbox,
    FolderRole::Drafts,
    FolderRole::Sent,
    FolderRole::Archive,
    FolderRole::Junk,
    FolderRole::Trash,
    FolderRole::All,
];

impl Account {
    /// The account `id`, when it has a key.
    pub fn of(state: &AppState, id: AccountId) -> Option<Account> {
        let mailbox = state.mailbox();
        let account = mailbox
            .accounts()
            .into_iter()
            .find(|account| account.id == id)?;
        let key = account.key?;
        let folders = mailbox
            .folders()
            .into_iter()
            .filter(|folder| folder.account == id)
            .collect();
        Some(Account {
            id,
            key,
            name: account.name,
            folders,
        })
    }

    fn role(&self, role: FolderRole) -> Option<&Folder> {
        self.folders.iter().find(|folder| folder.role == role)
    }

    /// Its Inbox's and Junk folder's full names, as the rules name them.
    pub fn sieve_folders(&self) -> Folders {
        let defaults = Folders::default();
        Folders {
            inbox: self
                .role(FolderRole::Inbox)
                .map_or(defaults.inbox, |folder| folder.name.clone()),
            junk: self
                .role(FolderRole::Junk)
                .map_or(defaults.junk, |folder| folder.name.clone()),
        }
    }

    pub fn place(&self, state: &AppState) -> Place {
        Place {
            dir: state.config().dirs.rules_dir(),
            key: self.key.clone(),
            folders: self.sieve_folders(),
        }
    }

    /// The folders a rule can file into: those that take messages, the role folders first in
    /// the sidebar's order, then the others by name.
    pub fn targets(&self) -> Vec<editor::Target> {
        let mut folders: Vec<&Folder> = self
            .folders
            .iter()
            .filter(|folder| folder.selectable)
            .collect();
        folders.sort_by_key(|folder| {
            (
                ROLES
                    .iter()
                    .position(|role| *role == folder.role)
                    .unwrap_or(ROLES.len()),
                folder.name.to_lowercase(),
            )
        });
        folders
            .into_iter()
            .map(|folder| editor::Target {
                name: folder.name.clone(),
                label: text::folder_label(&folder.name, &self.folders),
            })
            .collect()
    }

    pub fn folder_id(&self, name: &str) -> Option<FolderId> {
        self.folders
            .iter()
            .find(|folder| folder.name == name)
            .map(|folder| folder.id)
    }

    fn junk_ids(&self) -> Vec<FolderId> {
        self.folders
            .iter()
            .filter(|folder| folder.role == FolderRole::Junk)
            .map(|folder| folder.id)
            .collect()
    }
}

/// What Undo of Always or Never Junk takes back of the rules file.
#[derive(Clone)]
struct Revert {
    account: Account,
    place: Place,
    address: String,
    always: bool,
    listed: Listed,
}

type Opener = Rc<dyn Fn(&Path)>;

#[derive(Default)]
struct Shared {
    state: RefCell<Weak<AppState>>,
    /// The accounts whose rules can't run, and why, in the order they were found.
    problems: RefCell<Vec<(AccountId, String)>>,
    /// Apply Rules requests waiting for the engine: the folder's name and the toast that says
    /// they run.
    applying: RefCell<HashMap<RequestId, (String, adw::Toast)>>,
    /// The last action, when it was a list change alone (Always or Never Junk with no message
    /// to move), and when its Undo stops.
    last: RefCell<Option<Revert>>,
    expiry: RefCell<Option<glib::SourceId>>,
    /// The self-test's stand-in for the desktop's file opener.
    opener: RefCell<Option<Opener>>,
    watch: RefCell<Option<Rc<DirWatch>>>,
    subscription: RefCell<Option<Subscription>>,
}

thread_local! {
    static SHARED: Rc<Shared> = Rc::default();
}

fn shared() -> Rc<Shared> {
    SHARED.with(Rc::clone)
}

fn app_state() -> Option<Rc<AppState>> {
    shared().state.borrow().upgrade()
}

/// Listens to the engine's rules events and watches the rules folder, once at startup.
pub fn install(state: &Rc<AppState>) {
    let shared = shared();
    shared.state.replace(Rc::downgrade(state));
    let subscription = state.engine().subscribe(|event| match event {
        Event::RulesFailed { account, error } => {
            set_problem(*account, Some(text::problem_text(error)));
        }
        Event::RulesApplied {
            request,
            folder: _,
            result,
        } => applied(*request, result),
        _ => {}
    });
    shared.subscription.replace(Some(subscription));
    let rule: TriggerRule = Box::new(|_, file, other| {
        let script = |name: &str| name.ends_with(".sieve");
        script(file) || other.is_some_and(script)
    });
    let watch = DirWatch::start(state.config().dirs.rules_dir(), rule, check_all);
    shared.watch.replace(Some(watch));
}

/// Connects the commands on the main window's action set and in the sidebar's menus.
pub fn install_window(state: &Rc<AppState>, window: &Window, actions: &crate::actions::ActionSet) {
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::EditRules, move || {
        if let Some(state) = weak.upgrade()
            && let Some(account) = shown_account(&state)
        {
            dialog::open(&state, account, Start::List);
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::NewRuleFromMessage, move || {
        if let Some(state) = weak.upgrade() {
            new_rule_from_message(&state);
        }
    });
    for (id, always) in [(ActionId::JunkSender, true), (ActionId::TrustSender, false)] {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade() {
                list_sender(&state, always);
            }
        });
    }
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::ApplyRules, move || {
        if let Some(state) = weak.upgrade()
            && let Ok(folder) = shown_folder(&state)
        {
            apply(&state, folder);
        }
    });
    // Apply Rules follows the search, which leaves the folder shown as it is.
    if let Some(list) = state.list() {
        let weak = Rc::downgrade(state);
        list.connect_search_changed(move |_| {
            if let Some(state) = weak.upgrade() {
                state.refresh_commands();
            }
        });
    }
    let weak = Rc::downgrade(state);
    window.sidebar().connect_edit_rules(move |key| {
        if let Some(state) = weak.upgrade()
            && let Some(account) =
                crate::mailbox::account_for_key(key).and_then(|id| Account::of(&state, id))
        {
            dialog::open(&state, account, Start::List);
        }
    });
    let (applies, activate) = (Rc::downgrade(state), Rc::downgrade(state));
    window.sidebar().connect_apply_rules(
        move |key| match applies.upgrade() {
            Some(state) => folder_of_key(&state, key).map(drop),
            None => Err("Sealwax is closing"),
        },
        move |key| {
            if let Some(state) = activate.upgrade()
                && let Ok(folder) = folder_of_key(&state, key)
            {
                apply(&state, folder);
            }
        },
    );
    check_all();
}

/// The account the list shows (in All Inboxes the selected message's), else the first with
/// rules.
fn shown_account(state: &AppState) -> Option<Account> {
    let folders = state.mailbox().folders();
    let shown = state.list().and_then(|list| {
        let selected = list
            .selected_targets()
            .ok()
            .and_then(|targets| targets.first().map(|target| target.folder));
        crate::compose_actions::compose_account(list.scope(), selected, &folders)
    });
    shown
        .and_then(|id| Account::of(state, id))
        .or_else(|| {
            state
                .mailbox()
                .accounts()
                .iter()
                .find_map(|account| Account::of(state, account.id))
        })
}

/// The folder a sidebar key names, when the rules can run on it.
fn folder_of_key(state: &AppState, key: &str) -> Result<FolderId, &'static str> {
    match state.mailbox().scope_for_key(key) {
        Some(ViewScope::Folder(id)) => runs_on(state, id).map(|()| id),
        _ => Err("Choose one folder"),
    }
}

fn runs_on(state: &AppState, folder: FolderId) -> Result<(), &'static str> {
    let folder = state
        .mailbox()
        .folder(folder)
        .ok_or("The folder is gone")?;
    if !folder.selectable {
        return Err("This folder holds no messages");
    }
    Account::of(state, folder.account)
        .map(drop)
        .ok_or("Only accounts set up in Sealwax have rules")
}

/// The folder the list shows, when the rules can run on it: not All Inboxes or a search.
fn shown_folder(state: &AppState) -> Result<FolderId, &'static str> {
    let list = state.list().ok_or("There is no message list")?;
    match list.scope() {
        Some(ViewScope::Folder(id)) if list.query().is_empty() => runs_on(state, id).map(|()| id),
        _ => Err("Choose one folder"),
    }
}

/// The selected messages' sender, when they have one, in one account with rules.
struct Picked {
    account: Account,
    address: String,
    name: String,
    targets: Vec<Target>,
    first: MessageId,
}

fn picked(state: &AppState) -> Result<Picked, &'static str> {
    let list = state.list().ok_or("Select a message")?;
    let positions = list.selected_positions();
    if positions.is_empty() {
        return Err("Select a message");
    }
    let mut rows = Vec::with_capacity(positions.len());
    for position in positions {
        rows.push(
            list.model()
                .row_at(position)
                .ok_or("Some selected messages haven't loaded")?,
        );
    }
    let address = rows[0].sender_email.trim().to_owned();
    if !address.contains('@') {
        return Err("The message has no sender's address");
    }
    if rows
        .iter()
        .any(|row| !row.sender_email.trim().eq_ignore_ascii_case(&address))
    {
        return Err("The messages are from more than one sender");
    }
    let folders = state.mailbox().folders();
    let accounts: BTreeSet<AccountId> = rows
        .iter()
        .filter_map(|row| folders.iter().find(|folder| folder.id == row.folder))
        .map(|folder| folder.account)
        .collect();
    let [account] = accounts.into_iter().collect::<Vec<_>>()[..] else {
        return Err("The messages are in more than one account");
    };
    let account =
        Account::of(state, account).ok_or("Only accounts set up in Sealwax have rules")?;
    Ok(Picked {
        account,
        address,
        name: rows[0].sender_name.trim().to_owned(),
        targets: rows
            .iter()
            .map(|row| Target {
                id: row.id,
                folder: row.folder,
                flags: row.flags,
            })
            .collect(),
        first: rows[0].id,
    })
}

/// Whether `id`, one of [`COMMANDS`], applies now, or why not.
pub fn applies(state: &AppState, id: ActionId) -> Result<(), &'static str> {
    applies_with(state, id, &picked(state))
}

fn applies_with(
    state: &AppState,
    id: ActionId,
    picked: &Result<Picked, &'static str>,
) -> Result<(), &'static str> {
    let account = || {
        picked
            .as_ref()
            .map(|picked| &picked.account)
            .map_err(|why| *why)
    };
    match id {
        ActionId::EditRules => shown_account(state)
            .map(drop)
            .ok_or("No account has rules"),
        ActionId::ApplyRules => shown_folder(state).map(drop),
        ActionId::NewRuleFromMessage => account().map(drop),
        ActionId::JunkSender => match account()?.role(FolderRole::Junk) {
            Some(_) => Ok(()),
            None => Err("The account has no Junk folder"),
        },
        ActionId::TrustSender => match account()?.role(FolderRole::Inbox) {
            Some(_) => Ok(()),
            None => Err("The account has no Inbox"),
        },
        _ => Err("Not a rules command"),
    }
}

/// Whether each command applies to what the window shows and has selected.
pub fn refresh(state: &AppState, actions: &crate::actions::ActionSet) {
    let picked = picked(state);
    for id in COMMANDS {
        actions.set_applies(id, applies_with(state, id, &picked).is_ok());
    }
}

/// The selected messages' sender, for the menus' and the palette's titles.
pub fn sender(state: &AppState) -> Option<String> {
    picked(state).ok().map(|picked| picked.address)
}

/// The commands' titles where they differ from the registry's, for the palette: Always and
/// Never Junk name the sender.
pub fn titles(state: &AppState) -> Vec<(ActionId, String)> {
    let Some(address) = sender(state) else {
        return Vec::new();
    };
    vec![
        (
            ActionId::JunkSender,
            text::sender_title(true, Some(&address)),
        ),
        (
            ActionId::TrustSender,
            text::sender_title(false, Some(&address)),
        ),
    ]
}

/// Create Rule from Message: the Rules dialog with a new rule filled in from the message,
/// From is its sender (or List-Id contains its list's identifier) and a move to a folder not
/// chosen yet. The List-Id comes from the store, off the GTK thread.
fn new_rule_from_message(state: &Rc<AppState>) {
    let Ok(picked) = picked(state) else {
        return;
    };
    let reply = state
        .mailbox()
        .store()
        .map(|store| store.message(picked.first));
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let list_id = match reply {
            Some(reply) => reply
                .recv()
                .await
                .ok()
                .and_then(|detail| detail.value)
                .and_then(|detail| detail.headers.list_id),
            None => None,
        };
        if let Some(state) = weak.upgrade() {
            let draft = Draft::from_message(&picked.address, &picked.name, list_id.as_deref());
            dialog::open(&state, picked.account, Start::New(draft));
        }
    });
}

/// Always Junk (`always`) or Never Junk Messages from Sender: the address on that list, the
/// file saved, then the selected messages moved as Move to Junk or Not Junk moves them: those
/// not in Junk to it, or those in Junk back to the Inbox. Undo takes back both.
fn list_sender(state: &Rc<AppState>, always: bool) {
    let Ok(picked) = picked(state) else {
        return;
    };
    let place = picked.account.place(state);
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let (job_place, job_address) = (place.clone(), picked.address.clone());
        let listed = file::run(move || file::list_sender(&job_place, &job_address, always)).await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let listed = match listed {
            Some(Ok(listed)) => listed,
            Some(Err(error)) => {
                tracing::info!(account = picked.account.id.0, "a sender wasn't listed");
                not_changed(&state, &picked.account, &picked.address, &error);
                return;
            }
            None => return,
        };
        if listed.changed {
            saved(&picked.account);
            dialog::reload_if_open(&picked.account);
        }
        let lead = text::listed_text(always, &picked.address, listed.changed);
        let revert = Revert {
            account: picked.account.clone(),
            place,
            address: picked.address.clone(),
            always,
            listed,
        };
        let junk = picked.account.junk_ids();
        let moving: Vec<Target> = picked
            .targets
            .into_iter()
            .filter(|target| junk.contains(&target.folder) != always)
            .collect();
        match (state.triage(), state.list()) {
            (Some(triage), Some(list)) if !moving.is_empty() => {
                let ids: Vec<MessageId> = moving.iter().map(|target| target.id).collect();
                list.advance_after(&ids);
                let undo: Rc<dyn Fn()> = Rc::new(move || revert.clone().run());
                triage.queue_with(Triage::Junk, moving, lead, undo);
            }
            _ => remember(&state, revert, lead),
        }
    });
}

/// The toast when Always or Never Junk couldn't change the rules file.
fn not_changed(state: &Rc<AppState>, account: &Account, address: &str, error: &ChangeError) {
    let why = match error {
        ChangeError::Unreadable(_) => "the rules file can't be read",
        ChangeError::Save(file::SaveError::Changed) => "the rules file changed meanwhile",
        ChangeError::Save(_) => "the rules file couldn't be saved",
    };
    let toast = adw::Toast::builder()
        .title(format!("{address} wasn't listed: {why}"))
        .use_markup(false)
        .build();
    if matches!(error, ChangeError::Unreadable(_)) {
        toast.set_button_label(Some("Open File"));
        let (weak, place) = (Rc::downgrade(state), account.place(state));
        toast.connect_button_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                open_file(&state, &place);
            }
        });
    }
    state.show_toast(&toast, false);
}

impl Revert {
    /// Takes the sender off the list again, off the GTK thread.
    fn run(self) {
        glib::spawn_future_local(async move {
            let Revert {
                account,
                place,
                address,
                always,
                listed,
            } = self;
            let undone =
                file::run(move || file::unlist_sender(&place, &address, always, listed)).await;
            let Some(state) = app_state() else {
                return;
            };
            match undone {
                Some(Ok(_)) => {
                    saved(&account);
                    dialog::reload_if_open(&account);
                }
                _ => {
                    tracing::info!(account = account.id.0, "a sender's listing wasn't undone");
                    let toast = adw::Toast::builder()
                        .title("The rules file couldn't be changed back")
                        .use_markup(false)
                        .build();
                    state.show_toast(&toast, false);
                }
            }
        });
    }
}

/// A list change with no message to move becomes the last action: its toast's Undo, `z` and
/// Ctrl+Z take it back within the undo window (ADR-015).
fn remember(state: &Rc<AppState>, revert: Revert, lead: String) {
    let window = state
        .triage()
        .map_or(triage::UNDO_WINDOW, |triage| triage.window());
    let changed = revert.listed.changed;
    let toast = adw::Toast::builder()
        .title(lead)
        .use_markup(false)
        .timeout(window.as_millis().div_ceil(1000).max(1) as u32)
        .priority(adw::ToastPriority::High)
        .build();
    if changed {
        if let Some(triage) = state.triage() {
            triage.forget_last();
        }
        crate::outbox::forget_last();
        let shared = shared();
        shared.last.replace(Some(revert));
        let expiry = glib::timeout_add_local_once(window, || {
            let held = self::shared();
            held.expiry.replace(None);
            held.last.replace(None);
            if let Some(state) = app_state() {
                state.refresh_commands();
            }
        });
        if let Some(old) = shared.expiry.replace(Some(expiry)) {
            old.remove();
        }
        toast.set_button_label(Some("Undo"));
        let weak = Rc::downgrade(state);
        toast.connect_button_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                undo_last(&state);
            }
        });
    }
    state.show_toast(&toast, true);
    state.refresh_commands();
}

/// Whether Undo would take back a list change: the last action is one.
pub fn can_undo() -> bool {
    shared().last.borrow().is_some()
}

/// Another action became the last one: the list change's Undo no longer works.
pub fn forget_last() {
    let shared = shared();
    shared.last.replace(None);
    if let Some(expiry) = shared.expiry.take() {
        expiry.remove();
    }
}

/// Undo (`z`, Ctrl+Z, the toast): takes back the last list change if the last action is one;
/// `false` leaves Undo to the others.
pub fn undo_last(state: &Rc<AppState>) -> bool {
    let shared = shared();
    let Some(revert) = shared.last.take() else {
        return false;
    };
    if let Some(expiry) = shared.expiry.take() {
        expiry.remove();
    }
    revert.run();
    let toast = adw::Toast::builder()
        .title("Undone")
        .use_markup(false)
        .build();
    state.show_toast(&toast, true);
    state.refresh_commands();
    true
}

/// Apply Rules to This Folder: the account's rules on every message `folder` holds, by the
/// engine; a toast says they run, then what they changed.
pub fn apply(state: &Rc<AppState>, folder: FolderId) {
    let title = state
        .mailbox()
        .folder(folder)
        .map(|folder| triage::folder_title(&folder))
        .unwrap_or_default();
    let engine = state.engine();
    let request = engine.next_request();
    if engine
        .send(Command::ApplyRules { request, folder })
        .is_err()
    {
        let toast = adw::Toast::builder()
            .title("Not connected to the mail engine")
            .use_markup(false)
            .build();
        state.show_toast(&toast, false);
        return;
    }
    let toast = adw::Toast::builder()
        .title(text::applying_text(&title))
        .use_markup(false)
        .timeout(0)
        .build();
    state.show_toast(&toast, false);
    shared()
        .applying
        .borrow_mut()
        .insert(request, (title, toast));
}

fn applied(request: RequestId, result: &Result<u32, RulesError>) {
    let Some((title, running)) = shared().applying.borrow_mut().remove(&request) else {
        return;
    };
    running.dismiss();
    if let Some(state) = app_state() {
        let toast = adw::Toast::builder()
            .title(text::applied_text(result, &title))
            .use_markup(false)
            .build();
        state.show_toast(&toast, false);
    }
}

/// The account's rules can run again (`None`), or can't, and why.
fn set_problem(account: AccountId, problem: Option<String>) {
    let shared = shared();
    let changed = {
        let mut problems = shared.problems.borrow_mut();
        let before = problems.clone();
        problems.retain(|(id, _)| *id != account);
        if let Some(problem) = problem {
            problems.push((account, problem));
        }
        *problems != before
    };
    if changed && let Some(state) = app_state() {
        state.update_banner();
    }
}

/// The app wrote the account's rules: they run.
fn saved(account: &Account) {
    set_problem(account.id, None);
}

/// Checks every account's file off the GTK thread, after the rules folder changed: a file
/// that can't run puts up the banner, one that runs again takes it down.
fn check_all() {
    let Some(state) = app_state() else {
        return;
    };
    let dir = state.config().dirs.rules_dir();
    let accounts: Vec<(AccountId, AccountKey)> = state
        .mailbox()
        .accounts()
        .into_iter()
        .filter_map(|account| Some((account.id, account.key?)))
        .collect();
    glib::spawn_future_local(async move {
        let checked = file::run(move || {
            accounts
                .into_iter()
                .map(|(id, key)| (id, file::check(&dir, &key)))
                .collect::<Vec<_>>()
        })
        .await;
        for (account, result) in checked.unwrap_or_default() {
            let problem = match result {
                Ok(()) => None,
                Err(Unreadable::Script(error)) => {
                    Some(text::problem_text(&RulesError::Script(error)))
                }
                Err(Unreadable::File) => Some(text::problem_text(&RulesError::File)),
            };
            set_problem(account, problem);
        }
    });
}

/// The banner's entries for accounts whose rules can't run, before the offline entry.
pub fn add_waiting(state: &AppState, queue: &mut Vec<Waiting>) {
    let problems = shared().problems.borrow().clone();
    let at = queue
        .iter()
        .position(|waiting| matches!(waiting.attention, Attention::Offline { .. }))
        .unwrap_or(queue.len());
    let entries: Vec<Waiting> = problems
        .into_iter()
        .filter_map(|(id, problem)| {
            let account = Account::of(state, id)?;
            Some(Waiting {
                attention: Attention::RulesFailed {
                    name: account.name,
                    problem,
                },
                account: Some(account.key),
            })
        })
        .collect();
    queue.splice(at..at, entries);
}

/// The banner's Open File, for the account it shows.
pub fn open_shown(state: &Rc<AppState>) {
    let shown = state.waiting().into_iter().next();
    let Some(Waiting {
        attention: Attention::RulesFailed { .. },
        account: Some(key),
    }) = shown
    else {
        return;
    };
    let account = state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|account| account.key.as_ref() == Some(&key))
        .and_then(|account| Account::of(state, account.id));
    if let Some(account) = account {
        open_file(state, &account.place(state));
    }
}

/// The self-test's stand-in for the desktop's file opener: it gets each path Open File would
/// open.
pub fn set_opener(opener: impl Fn(&Path) + 'static) {
    shared().opener.replace(Some(Rc::new(opener)));
}

/// The Apply Rules requests waiting for the engine's answer, oldest first, for the self-test.
pub fn pending_applies() -> Vec<RequestId> {
    let mut requests: Vec<RequestId> = shared().applying.borrow().keys().copied().collect();
    requests.sort_unstable();
    requests
}

/// The accounts the banner says can't run their rules, for the self-test.
pub fn problems() -> Vec<(AccountId, String)> {
    shared().problems.borrow().clone()
}

/// Opens the account's rules file in the desktop's default application for it (`gtk::
/// FileLauncher`, the portal's where there is one), as Preferences opens `config.toml`; never
/// from a headless run.
pub fn open_file(state: &AppState, place: &Place) {
    let Some(path) = place.path() else {
        return;
    };
    let opener = shared().opener.borrow().clone();
    if let Some(opener) = opener {
        opener(&path);
        return;
    }
    if gdk::Display::default()
        .is_some_and(|display| display.type_().name() == "GdkBroadwayDisplay")
    {
        tracing::warn!("the rules file doesn't open from a headless (Broadway) run");
        return;
    }
    let Some(window) = state.window() else {
        return;
    };
    glib::spawn_future_local(async move {
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
        if let Err(error) = launcher.launch_future(Some(window.window())).await {
            window.add_toast(
                adw::Toast::builder()
                    .title(format!("Couldn't open the rules file: {}", error.message()))
                    .use_markup(false)
                    .build(),
            );
        }
    });
}

/// Renames the folders the rules of `set` file into: `old` and every folder under it become
/// `new` and the same under it. Whether a rule changed.
pub(crate) fn rename_folders(
    set: &mut RuleSet,
    old: &str,
    new: &str,
    delimiter: Option<char>,
) -> bool {
    let mut changed = false;
    for item in &mut set.items {
        let Item::Rule(rule) = item else {
            continue;
        };
        for action in &mut rule.actions {
            if let Action::MoveTo(folder) | Action::CopyTo(folder) = action
                && let Some(renamed) = folders::renamed(folder, old, new, delimiter)
            {
                *folder = renamed;
                changed = true;
            }
        }
    }
    changed
}

/// A folder of `account` was renamed in Sealwax from `old` to `new` (full names; the folders
/// under it follow): the account's rules that file into them follow too, off the GTK thread.
/// A folder deleted leaves the rules as they are: a rule filing into a missing folder keeps
/// the message where it is.
pub fn folder_renamed(account: AccountId, old: String, new: String, delimiter: Option<char>) {
    let Some(state) = app_state() else {
        return;
    };
    let Some(account) = Account::of(&state, account) else {
        return;
    };
    let place = account.place(&state);
    glib::spawn_future_local(async move {
        let shown = folders::leaf(&old, delimiter).to_owned();
        let followed =
            file::run(move || file::follow_rename(&place, &old, &new, delimiter)).await;
        let Some(state) = app_state() else {
            return;
        };
        match followed {
            Some(Ok(true)) => {
                saved(&account);
                dialog::reload_if_open(&account);
            }
            Some(Ok(false)) => {}
            _ => {
                tracing::info!(account = account.id.0, "the rules didn't follow a renamed folder");
                let toast = adw::Toast::builder()
                    .title(format!(
                        "Rules that file into “{shown}” still name it: the rules file can't be \
                         changed"
                    ))
                    .use_markup(false)
                    .build();
                state.show_toast(&toast, false);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::sieve::{Condition, Field, Match, Operator, Rule};

    #[test]
    fn rules_follow_a_renamed_folder_and_those_under_it() {
        let rule = |actions: Vec<Action>| {
            Item::Rule(Rule {
                name: "Kunder".to_owned(),
                enabled: true,
                matching: Match::All,
                conditions: vec![Condition::Field {
                    field: Field::From,
                    operator: Operator::Contains,
                    value: "kari@example.test".to_owned(),
                }],
                actions,
            })
        };
        let mut set = RuleSet {
            items: vec![
                rule(vec![Action::MoveTo("Kunder/Acme".to_owned())]),
                Item::HandWritten("fileinto \"Kunder\";\n".to_owned()),
                rule(vec![
                    Action::CopyTo("Kunder".to_owned()),
                    Action::MoveTo("Kunderne".to_owned()),
                ]),
            ],
            ..RuleSet::default()
        };
        assert!(rename_folders(&mut set, "Kunder", "Klienter", Some('/')));
        let actions: Vec<Vec<Action>> = set
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Rule(rule) => Some(rule.actions.clone()),
                Item::HandWritten(_) => None,
            })
            .collect();
        assert!(
            actions
                == [
                    vec![Action::MoveTo("Klienter/Acme".to_owned())],
                    vec![
                        Action::CopyTo("Klienter".to_owned()),
                        Action::MoveTo("Kunderne".to_owned()),
                    ],
                ],
            "a sibling whose name starts the same stays"
        );
        assert!(
            set.items[1] == Item::HandWritten("fileinto \"Kunder\";\n".to_owned()),
            "hand-written Sieve stays as written"
        );
        assert!(!rename_folders(&mut set, "Arkiv", "Gammelt", Some('/')));
    }
}
