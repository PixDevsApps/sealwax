//! Triage in the app (ML-052 to ML-055): the mail actions on the selected messages, planned
//! with `domain::triage`, handed to a [`Backend`] that queues them, and reported back as
//! notices for the Undo toast (ADR-015: only the last action is undoable from the toast).
//!
//! The backend is the seam phase B plugs the store's op queue into: queue an action on
//! messages that may span folders and accounts with the undo window's end, mark a folder read,
//! undo a batch, count an account's pending ops. [`StoreBackend`] is phase A's, over the
//! store's existing `set_flags` and `remove`: flags change, and moved or deleted messages
//! leave the store, so a move can't be undone there. After queuing, accounts with a server
//! get `Command::OpsQueued`.

use crate::engine_link::EngineLink;
use crate::mailbox::Mailbox;
use gtk4::{gio, glib};
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::{Account, AccountId, Flags, Folder, FolderId, FolderRole, MessageId};
use sealwax_domain::triage::{self, FolderInfo, Plan, Triage, Unavailable};
use sealwax_engine::api::{Command, Refused};
use sealwax_infrastructure::store::{ViewFilter, ViewScope, ViewSpec};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

/// How long archive, delete and move wait before they run, and the Undo toast shows (ADR-015:
/// 5–10 s; a proposal for the user).
pub const UNDO_WINDOW: Duration = Duration::from_secs(7);

/// Whether a message shown in the reader is marked read (a proposal for the user). In the
/// Unread view it is marked when the selection leaves it, so the view doesn't take it away
/// while it is read.
pub const MARK_READ_ON_OPEN: bool = true;

/// A message an action applies to, as the list holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub id: MessageId,
    pub folder: FolderId,
    pub flags: Flags,
}

/// One queued user action, which Undo names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BatchId(pub u64);

/// What the backend did with an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Queued {
    pub batch: BatchId,
    /// Messages the action applies to.
    pub applied: u32,
    /// Messages it doesn't apply to: their account has no folder for it, or the store no
    /// longer holds them.
    pub skipped: u32,
}

/// What an Undo brought back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Undone {
    pub restored: u32,
    /// Messages that can't come back.
    pub lost: u32,
    pub why: Option<Lost>,
}

/// Why messages can't come back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lost {
    /// Deleted for good once the undo window ended.
    DeletedForGood,
    /// Phase A's backend took them out of the store and can't put them back.
    NotBuilt,
}

/// The store couldn't take the change. It carries no store text, which could quote a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendError;

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the mail store didn't take the change")
    }
}

pub type Pending<T> = Pin<Box<dyn Future<Output = Result<T, BackendError>>>>;

/// Where actions are queued. Each method sends its requests before it returns, so the store
/// takes them in the frame the key was pressed in; the future reports the outcome.
pub trait Backend {
    /// Queues `action` on `targets`; moves and deletes wait for `not_before`, until which
    /// [`Backend::undo`] cancels them.
    fn queue(&self, action: Triage, targets: &[Target], not_before: SystemTime) -> Pending<Queued>;

    /// Marks every message of `folder` read.
    fn mark_folder_read(&self, folder: FolderId) -> Pending<Queued>;

    /// Cancels what of `batch` hasn't run, or queues the inverse.
    fn undo(&self, batch: BatchId) -> Pending<Undone>;

    /// `account`'s ops not run yet.
    fn pending(&self, account: AccountId) -> Pending<u32>;
}

/// The accounts and folders actions are planned with.
pub trait Directory {
    fn folders(&self) -> Vec<Folder>;
    fn accounts(&self) -> Vec<Account>;
}

impl Directory for Mailbox {
    fn folders(&self) -> Vec<Folder> {
        Mailbox::folders(self)
    }

    fn accounts(&self) -> Vec<Account> {
        Mailbox::accounts(self)
    }
}

fn info(folder: &Folder) -> FolderInfo {
    FolderInfo {
        id: folder.id,
        role: folder.role,
        selectable: folder.selectable,
    }
}

/// What `action` does to a message in `folder`, from the folders of its account.
pub fn plan_for(action: Triage, folder: FolderId, folders: &[Folder]) -> Result<Plan, Unavailable> {
    let source = folders
        .iter()
        .find(|candidate| candidate.id == folder)
        .ok_or(Unavailable::NoTarget)?;
    let account: Vec<FolderInfo> = folders
        .iter()
        .filter(|candidate| candidate.account == source.account)
        .map(info)
        .collect();
    triage::plan(action, info(source), &account)
}

/// The name a folder goes by in messages: its role's, else its own.
pub fn folder_title(folder: &Folder) -> String {
    match folder.role {
        FolderRole::Inbox => "Inbox",
        FolderRole::Drafts => "Drafts",
        FolderRole::Sent => "Sent",
        FolderRole::Archive => "Archive",
        FolderRole::Junk => "Junk",
        FolderRole::Trash => "Trash",
        FolderRole::All | FolderRole::Other => return folder.name.clone(),
    }
    .to_owned()
}

/// Whether and how a command applies to the selected messages, for its sensitivity, its
/// tooltip and its title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Availability {
    /// Messages the command applies to.
    pub applies: usize,
    /// Why it applies to none, for the tooltip.
    pub reason: Option<&'static str>,
    /// The command's title here: Move to Junk is Not Junk in a Junk folder.
    pub title: &'static str,
}

impl Availability {
    pub fn enabled(&self) -> bool {
        self.applies > 0
    }
}

/// The triage commands, by their registry ids.
pub const COMMANDS: [ActionId; 7] = [
    ActionId::Archive,
    ActionId::Delete,
    ActionId::Junk,
    ActionId::MoveTo,
    ActionId::Star,
    ActionId::MarkRead,
    ActionId::MarkUnread,
];

/// Whether all of `targets` are in a Junk folder, where Move to Junk means Not Junk.
pub fn all_in_junk(targets: &[Target], folders: &[Folder]) -> bool {
    !targets.is_empty()
        && targets.iter().all(|target| {
            folders
                .iter()
                .any(|folder| folder.id == target.folder && folder.role == FolderRole::Junk)
        })
}

/// How `id`, one of [`COMMANDS`], applies to `targets`.
pub fn availability(id: ActionId, targets: &[Target], folders: &[Folder]) -> Availability {
    let title = match id {
        ActionId::Junk if all_in_junk(targets, folders) => "Not Junk",
        ActionId::Star if star_action(targets) == Triage::Star(false) => "Unstar",
        ActionId::Star => "Star",
        other => other.title(),
    };
    let none = |reason| Availability {
        applies: 0,
        reason: Some(reason),
        title,
    };
    if targets.is_empty() {
        return Availability {
            applies: 0,
            reason: None,
            title,
        };
    }
    let action = match id {
        ActionId::Archive => Triage::Archive,
        ActionId::Delete => Triage::Delete,
        ActionId::Junk => Triage::Junk,
        ActionId::Star => star_action(targets),
        ActionId::MarkRead => {
            let unread = targets.iter().filter(|t| !t.flags.is_seen()).count();
            return match unread {
                0 => none("already read"),
                applies => Availability {
                    applies,
                    reason: None,
                    title,
                },
            };
        }
        ActionId::MarkUnread => {
            let read = targets.iter().filter(|t| t.flags.is_seen()).count();
            return match read {
                0 => none("already unread"),
                applies => Availability {
                    applies,
                    reason: None,
                    title,
                },
            };
        }
        ActionId::MoveTo => {
            return match move_choices(targets, folders) {
                Ok(choices) if choices.is_empty() => none("the account has no other folder"),
                Ok(_) => Availability {
                    applies: targets.len(),
                    reason: None,
                    title,
                },
                Err(reason) => none(reason),
            };
        }
        _ => return none("not a mail action"),
    };
    let mut applies = 0;
    let mut first = None;
    for target in targets {
        match plan_for(action, target.folder, folders) {
            Ok(_) => applies += 1,
            Err(why) => {
                first.get_or_insert((why, target.folder));
            }
        }
    }
    let reason = match (applies, first) {
        (0, Some((why, folder))) => Some(reason_for(action, why, folder, folders)),
        _ => None,
    };
    Availability {
        applies,
        reason,
        title,
    }
}

fn reason_for(
    action: Triage,
    why: Unavailable,
    folder: FolderId,
    folders: &[Folder],
) -> &'static str {
    let in_junk = folders
        .iter()
        .any(|candidate| candidate.id == folder && candidate.role == FolderRole::Junk);
    match (action, why) {
        (Triage::Archive, Unavailable::AlreadyThere) => "already archived",
        (Triage::Archive, Unavailable::NoTarget) => "the account has no Archive folder",
        (Triage::Junk, Unavailable::NoTarget) if in_junk => "the account has no Inbox",
        (Triage::Junk, Unavailable::NoTarget) => "the account has no Junk folder",
        (_, Unavailable::AlreadyThere) => "already there",
        (_, Unavailable::NoTarget) => "the account has no folder for it",
    }
}

/// Stars every target unless all are starred, then unstars them.
pub fn star_action(targets: &[Target]) -> Triage {
    let all = !targets.is_empty() && targets.iter().all(|target| target.flags.is_flagged());
    Triage::Star(!all)
}

/// The folders Move to… offers for `targets`: their account's folders that take messages,
/// except the one they are all in; role folders first in the sidebar's order, then the others
/// by name. 1.0 doesn't move between accounts.
pub fn move_choices(targets: &[Target], folders: &[Folder]) -> Result<Vec<Folder>, &'static str> {
    let accounts: BTreeSet<AccountId> = targets
        .iter()
        .filter_map(|target| folders.iter().find(|folder| folder.id == target.folder))
        .map(|folder| folder.account)
        .collect();
    let account = match accounts.len() {
        0 => return Err("the messages' folders are gone"),
        1 => *accounts.first().expect("one account"),
        _ => return Err("the messages are in more than one account"),
    };
    let sources: BTreeSet<FolderId> = targets.iter().map(|target| target.folder).collect();
    let current = (sources.len() == 1).then(|| *sources.first().expect("one folder"));
    let order = |role: FolderRole| match role {
        FolderRole::Inbox => 0,
        FolderRole::Archive => 1,
        FolderRole::Drafts => 2,
        FolderRole::Sent => 3,
        FolderRole::Junk => 4,
        FolderRole::Trash => 5,
        FolderRole::All => 6,
        FolderRole::Other => 7,
    };
    let mut choices: Vec<Folder> = folders
        .iter()
        .filter(|folder| folder.account == account && folder.selectable)
        .filter(|folder| Some(folder.id) != current)
        .cloned()
        .collect();
    choices.sort_by(|a, b| {
        order(a.role)
            .cmp(&order(b.role))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(choices)
}

fn messages(count: u32) -> String {
    match count {
        1 => "1 message".to_owned(),
        count => format!("{count} messages"),
    }
}

fn were(count: u32) -> &'static str {
    if count == 1 { "was" } else { "were" }
}

/// What a queued action did, in the toast's words.
pub fn done_text(action: Triage, queued: Queued, plans: &[Plan], folders: &[Folder]) -> String {
    let n = messages(queued.applied);
    let to = |id: FolderId| {
        folders
            .iter()
            .find(|folder| folder.id == id)
            .map(folder_title)
            .unwrap_or_else(|| "another folder".to_owned())
    };
    let all_expunge = !plans.is_empty() && plans.iter().all(|plan| *plan == Plan::Expunge);
    let text = match action {
        Triage::Read(true) => format!("Marked {n} as read"),
        Triage::Read(false) => format!("Marked {n} as unread"),
        Triage::Star(true) => format!("Starred {n}"),
        Triage::Star(false) => format!("Unstarred {n}"),
        Triage::Archive => format!("Archived {n}"),
        Triage::Delete if all_expunge => format!("Deleted {n} for good"),
        Triage::Delete => match plans.first() {
            Some(Plan::Move { to: trash, .. }) if plans.iter().all(|plan| plan == &plans[0]) => {
                format!("Moved {n} to the {}", to(*trash))
            }
            _ => format!("Deleted {n}"),
        },
        Triage::Junk => match plans.first() {
            Some(Plan::Move { add, .. })
                if add.contains(Flags::NOT_JUNK) && plans.iter().all(|plan| plan == &plans[0]) =>
            {
                format!("Moved {n} to the Inbox as not junk")
            }
            _ => format!("Moved {n} to Junk"),
        },
        Triage::MoveTo(folder) => format!("Moved {n} to {}", to(folder)),
    };
    match queued.skipped {
        0 => text,
        skipped => format!(
            "{text}. {} {} left as {}",
            messages(skipped),
            were(skipped),
            { if skipped == 1 { "it was" } else { "they were" } }
        ),
    }
}

/// Marking a folder read, in the toast's words.
pub fn folder_read_text(queued: u32, name: &str) -> String {
    match queued {
        0 => format!("Nothing in {name} was unread"),
        count => format!("Marked {} in {name} as read", messages(count)),
    }
}

/// What an Undo did, in the toast's words.
pub fn undone_text(undone: Undone) -> String {
    match (undone.lost, undone.why) {
        (0, _) | (_, None) => "Undone".to_owned(),
        (lost, Some(Lost::DeletedForGood)) => {
            let lost_text = format!(
                "{} {} deleted for good and can't be brought back",
                messages(lost),
                were(lost)
            );
            if undone.restored > 0 {
                format!("Undone. {lost_text}")
            } else {
                capitalised(&lost_text)
            }
        }
        (lost, Some(Lost::NotBuilt)) => {
            let lost_text = format!("can't bring back {} yet", messages(lost));
            if undone.restored > 0 {
                format!("Undone, but {lost_text}")
            } else {
                capitalised(&lost_text)
            }
        }
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The server refused queued changes for good (`Event::OpsRefused`), and the store put the
/// messages back: what didn't happen, in the toast's words. `account` names the account when
/// there are several.
pub fn refused_text(change: Refused, count: u32, account: Option<&str>) -> String {
    let n = messages(count);
    let (they, were) = if count == 1 {
        ("it's", "it was")
    } else {
        ("they're", "they were")
    };
    let text = match change {
        Refused::Flags => format!("the server didn't change {n}; {they} back as {were}"),
        Refused::Move => format!("the server didn't move {n}; {they} back where {were}"),
        Refused::Expunge => format!("the server didn't delete {n}; {they} back"),
        Refused::FolderRead => {
            format!("the server didn't mark {n} as read; {they} back as {were}")
        }
    };
    match account {
        Some(name) => format!("{name}: {text}"),
        None => capitalised(&text),
    }
}

/// Where an action came from: the user's commands become the last action, which Undo undoes;
/// a message marked read because it was opened doesn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    User,
    Opened,
}

/// What the app shows for an outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    /// The toast's Undo undoes this batch, while it is the last action.
    pub undo: Option<BatchId>,
    /// How long the toast shows.
    pub shown: Duration,
}

impl Notice {
    fn plain(text: String) -> Self {
        Self {
            text,
            undo: None,
            shown: Duration::from_secs(4),
        }
    }
}

/// The last undoable action.
#[derive(Clone, Copy, Debug)]
struct Last {
    batch: BatchId,
    /// Deleting for good can be undone only inside the window.
    until: Option<Instant>,
}

type NoticeHandler = Rc<dyn Fn(&Notice)>;
type Listener = Rc<dyn Fn()>;

/// Plans, queues and undoes triage, and reports outcomes as notices.
#[derive(Clone)]
pub struct Controller {
    inner: Rc<Inner>,
}

struct Inner {
    backend: Rc<dyn Backend>,
    directory: Rc<dyn Directory>,
    engine: EngineLink,
    last: Cell<Option<Last>>,
    notices: RefCell<Vec<NoticeHandler>>,
    changed: RefCell<Vec<Listener>>,
    /// Actions queued so far, for the self-test and measurements.
    queued: Cell<u64>,
    /// Actions waiting for the backend's answer.
    in_flight: Cell<u32>,
}

impl Controller {
    pub fn new(backend: Rc<dyn Backend>, directory: Rc<dyn Directory>, engine: EngineLink) -> Self {
        Self {
            inner: Rc::new(Inner {
                backend,
                directory,
                engine,
                last: Cell::new(None),
                notices: RefCell::new(Vec::new()),
                changed: RefCell::new(Vec::new()),
                queued: Cell::new(0),
                in_flight: Cell::new(0),
            }),
        }
    }

    /// Runs `work` on the main loop, counted while it waits for the backend.
    fn spawn(&self, work: impl Future<Output = ()> + 'static) {
        let inner = self.inner.clone();
        inner.in_flight.set(inner.in_flight.get() + 1);
        glib::spawn_future_local(async move {
            work.await;
            inner.in_flight.set(inner.in_flight.get() - 1);
        });
    }

    /// Actions waiting for the backend's answer.
    pub fn in_flight(&self) -> u32 {
        self.inner.in_flight.get()
    }

    /// Runs `handler` with every notice: a toast's text and whether its Undo works.
    pub fn connect_notice(&self, handler: impl Fn(&Notice) + 'static) {
        self.inner.notices.borrow_mut().push(Rc::new(handler));
    }

    /// Runs `listener` when whether Undo can run changes.
    pub fn connect_changed(&self, listener: impl Fn() + 'static) {
        self.inner.changed.borrow_mut().push(Rc::new(listener));
    }

    /// Actions handed to the backend so far.
    pub fn queued(&self) -> u64 {
        self.inner.queued.get()
    }

    /// Whether Undo has something to undo: the last action, while it can be undone.
    pub fn can_undo(&self) -> bool {
        self.inner
            .last
            .get()
            .is_some_and(|last| last.until.is_none_or(|until| Instant::now() < until))
    }

    /// The batch Undo would undo.
    pub fn last_batch(&self) -> Option<BatchId> {
        self.inner.last.get().map(|last| last.batch)
    }

    fn set_last(&self, last: Option<Last>) {
        self.inner.last.set(last);
        let listeners = self.inner.changed.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    fn notify(&self, notice: &Notice) {
        let handlers = self.inner.notices.borrow().clone();
        for handler in handlers {
            handler(notice);
        }
    }

    /// Queues `action` on `targets` and reports it when the backend answers. The store gets
    /// the request before this returns.
    pub fn queue(&self, action: Triage, targets: Vec<Target>, origin: Origin) {
        let Some(outcome) = self.start(action, targets, origin) else {
            return;
        };
        let this = self.clone();
        self.spawn(async move {
            if let Some(notice) = this.finish(outcome).await {
                this.notify(&notice);
            }
        });
    }

    /// The same, for tests: the notice instead of the handlers.
    pub async fn run(
        &self,
        action: Triage,
        targets: Vec<Target>,
        origin: Origin,
    ) -> Option<Notice> {
        let outcome = self.start(action, targets, origin)?;
        self.finish(outcome).await
    }

    fn start(&self, action: Triage, targets: Vec<Target>, origin: Origin) -> Option<Started> {
        if targets.is_empty() {
            return None;
        }
        let folders = self.inner.directory.folders();
        let plans: Vec<Plan> = targets
            .iter()
            .filter_map(|target| plan_for(action, target.folder, &folders).ok())
            .collect();
        let leaves = plans.iter().any(|plan| plan.leaves_folder());
        let not_before = if leaves {
            SystemTime::now() + UNDO_WINDOW
        } else {
            SystemTime::now()
        };
        let pending = self.inner.backend.queue(action, &targets, not_before);
        self.inner.queued.set(self.inner.queued.get() + 1);
        let accounts = self.accounts_of(targets.iter().map(|target| target.folder), &folders);
        // A single message's read or star change shows in place and toggles back with its
        // key; it gets no toast and leaves the last action as it was.
        let notable = origin == Origin::User && (leaves || targets.len() > 1);
        let expunge = !plans.is_empty() && plans.iter().all(|plan| *plan == Plan::Expunge);
        Some(Started {
            pending,
            action,
            plans,
            folders,
            accounts,
            notable,
            expunge,
        })
    }

    async fn finish(&self, started: Started) -> Option<Notice> {
        let Started {
            pending,
            action,
            plans,
            folders,
            accounts,
            notable,
            expunge,
        } = started;
        let queued = match pending.await {
            Ok(queued) => queued,
            Err(error) => {
                tracing::warn!("a mail action wasn't queued: {error}");
                return notable.then(|| {
                    Notice::plain("The change couldn't be saved in the mail store".into())
                });
            }
        };
        self.wake(&accounts);
        if !notable {
            return None;
        }
        if queued.applied == 0 {
            return Some(Notice::plain("Nothing to change".to_owned()));
        }
        self.set_last(Some(Last {
            batch: queued.batch,
            until: expunge.then(|| Instant::now() + UNDO_WINDOW),
        }));
        Some(Notice {
            text: done_text(action, queued, &plans, &folders),
            undo: Some(queued.batch),
            shown: UNDO_WINDOW,
        })
    }

    /// Marks the messages of `folders` read: the folder the list shows, or every Inbox for
    /// All Inboxes. `name` names it in the toast.
    pub fn mark_folders_read(&self, folders: Vec<FolderId>, name: String) {
        let this = self.clone();
        let known = self.inner.directory.folders();
        let accounts = self.accounts_of(folders.iter().copied(), &known);
        let pending: Vec<Pending<Queued>> = folders
            .iter()
            .map(|folder| self.inner.backend.mark_folder_read(*folder))
            .collect();
        self.inner.queued.set(self.inner.queued.get() + 1);
        self.spawn(async move {
            let mut applied = 0;
            let mut batches = Vec::new();
            for pending in pending {
                match pending.await {
                    Ok(queued) => {
                        applied += queued.applied;
                        batches.push(queued.batch);
                    }
                    Err(error) => {
                        tracing::warn!("a folder wasn't marked read: {error}");
                        this.notify(&Notice::plain(
                            "The change couldn't be saved in the mail store".into(),
                        ));
                        return;
                    }
                }
            }
            this.wake(&accounts);
            // Several Inboxes are several batches; Undo takes back the last one alone, so the
            // toast offers it only for one folder.
            let batch = match batches.as_slice() {
                [batch] if applied > 0 => Some(*batch),
                _ => None,
            };
            if let Some(batch) = batch {
                this.set_last(Some(Last { batch, until: None }));
            }
            this.notify(&Notice {
                text: folder_read_text(applied, &name),
                undo: batch,
                shown: UNDO_WINDOW,
            });
        });
    }

    /// Undoes the last action, if it is `batch` (the toast's) or any (`None`: `z`, Ctrl+Z).
    pub fn undo(&self, batch: Option<BatchId>) {
        let Some(last) = self.inner.last.get() else {
            return;
        };
        if batch.is_some_and(|batch| batch != last.batch) {
            return;
        }
        if last.until.is_some_and(|until| Instant::now() >= until) {
            self.set_last(None);
            self.notify(&Notice::plain(undone_text(Undone {
                restored: 0,
                lost: 1,
                why: Some(Lost::DeletedForGood),
            })));
            return;
        }
        self.set_last(None);
        let pending = self.inner.backend.undo(last.batch);
        let accounts: Vec<AccountId> = self
            .inner
            .directory
            .accounts()
            .into_iter()
            .filter(|account| account.key.is_some())
            .map(|account| account.id)
            .collect();
        let this = self.clone();
        self.spawn(async move {
            match pending.await {
                Ok(undone) => {
                    this.wake(&accounts);
                    this.notify(&Notice::plain(undone_text(undone)));
                }
                Err(error) => {
                    tracing::warn!("an undo wasn't queued: {error}");
                    this.notify(&Notice::plain("The change couldn't be undone".into()));
                }
            }
        });
    }

    /// The server refused queued changes for good: the notice that says so.
    pub fn refused(&self, account: AccountId, change: Refused, count: u32) -> Notice {
        let accounts = self.inner.directory.accounts();
        let name = (accounts.len() > 1)
            .then(|| accounts.iter().find(|known| known.id == account))
            .flatten()
            .map(|known| known.name.as_str());
        Notice {
            text: refused_text(change, count, name),
            undo: None,
            shown: Duration::from_secs(10),
        }
    }

    /// The accounts with a server that `folders` belong to.
    fn accounts_of(
        &self,
        folders: impl Iterator<Item = FolderId>,
        known: &[Folder],
    ) -> Vec<AccountId> {
        let accounts = self.inner.directory.accounts();
        let ids: BTreeSet<AccountId> = folders
            .filter_map(|id| known.iter().find(|folder| folder.id == id))
            .map(|folder| folder.account)
            .filter(|id| {
                accounts
                    .iter()
                    .any(|account| account.id == *id && account.key.is_some())
            })
            .collect();
        ids.into_iter().collect()
    }

    /// Tells each account's op runner it has work (ML-051); local accounts have none.
    fn wake(&self, accounts: &[AccountId]) {
        for account in accounts {
            if self
                .inner
                .engine
                .send(Command::OpsQueued { account: *account })
                .is_err()
            {
                tracing::debug!(account = account.0, "no engine to run the queued ops");
            }
        }
    }
}

/// An action handed to the backend, waiting for its answer.
struct Started {
    pending: Pending<Queued>,
    action: Triage,
    plans: Vec<Plan>,
    folders: Vec<Folder>,
    accounts: Vec<AccountId>,
    /// It gets a toast and becomes the last action.
    notable: bool,
    expunge: bool,
}

/// What phase A's backend did with a batch, so Undo can reverse it.
enum Applied {
    /// Flags added to and removed from these messages, with the flags each had before.
    Flags {
        changes: Vec<(MessageId, Flags)>,
        add: Flags,
        remove: Flags,
    },
    /// Messages taken out of the store.
    Removed(u32),
}

/// Phase A's backend, over the store's `set_flags` and `remove`: actions take effect at once,
/// moved and deleted messages leave the store, and only flag changes can be undone.
pub struct StoreBackend {
    mailbox: Rc<Mailbox>,
    batches: Rc<RefCell<HashMap<BatchId, Vec<Applied>>>>,
    next: Cell<u64>,
}

impl StoreBackend {
    pub fn new(mailbox: Rc<Mailbox>) -> Self {
        Self {
            mailbox,
            batches: Rc::default(),
            next: Cell::new(0),
        }
    }

    fn batch(&self) -> BatchId {
        self.next.set(self.next.get() + 1);
        BatchId(self.next.get())
    }
}

fn failed<T: 'static>() -> Pending<T> {
    Box::pin(async { Err(BackendError) })
}

impl Backend for StoreBackend {
    fn queue(
        &self,
        action: Triage,
        targets: &[Target],
        _not_before: SystemTime,
    ) -> Pending<Queued> {
        let folders = self.mailbox.folders();
        let mut flag_groups: Vec<((Flags, Flags), Vec<Target>)> = Vec::new();
        let mut removed = Vec::new();
        let mut skipped = 0;
        for target in targets {
            match plan_for(action, target.folder, &folders) {
                Ok(Plan::Flags { add, remove }) => {
                    match flag_groups
                        .iter_mut()
                        .find(|(key, _)| *key == (add, remove))
                    {
                        Some((_, group)) => group.push(*target),
                        None => flag_groups.push(((add, remove), vec![*target])),
                    }
                }
                Ok(Plan::Move { .. } | Plan::Expunge) => removed.push(target.id),
                Err(_) => skipped += 1,
            }
        }
        let Some(store) = self.mailbox.store() else {
            return failed();
        };
        let flag_replies: Vec<_> = flag_groups
            .iter()
            .map(|((add, remove), group)| {
                store.set_flags(
                    group.iter().map(|target| target.id).collect(),
                    *add,
                    *remove,
                )
            })
            .collect();
        let removal = (!removed.is_empty()).then(|| store.remove(removed.clone()));
        drop(store);
        let batch = self.batch();
        let mut applied: Vec<Applied> = flag_groups
            .into_iter()
            .map(|((add, remove), group)| Applied::Flags {
                changes: group
                    .iter()
                    .map(|target| (target.id, target.flags))
                    .collect(),
                add,
                remove,
            })
            .collect();
        if !removed.is_empty() {
            applied.push(Applied::Removed(removed.len() as u32));
        }
        self.batches.borrow_mut().insert(batch, applied);
        let blobs = self.mailbox.blobs();
        let total = targets.len() as u32;
        Box::pin(async move {
            for reply in flag_replies {
                reply.recv().await.map_err(|_| BackendError)?;
            }
            if let Some(removal) = removal {
                let removed = removal.recv().await.map_err(|_| BackendError)?;
                if let Some(blobs) = blobs
                    && !removed.orphaned_blobs.is_empty()
                {
                    // The files go on GIO's thread pool; nothing waits for them.
                    let orphans = removed.orphaned_blobs;
                    drop(gio::spawn_blocking(move || {
                        for key in orphans {
                            let _ = blobs.remove(&key);
                        }
                    }));
                }
            }
            Ok(Queued {
                batch,
                applied: total - skipped,
                skipped,
            })
        })
    }

    fn mark_folder_read(&self, folder: FolderId) -> Pending<Queued> {
        let spec = ViewSpec {
            scope: ViewScope::Folder(folder),
            filter: ViewFilter::Unread,
        };
        let Some(reply) = self
            .mailbox
            .store()
            .map(|store| store.page(spec, 0, 100_000))
        else {
            return failed();
        };
        let mailbox = self.mailbox.clone();
        let batch = self.batch();
        let batches = self.batches.clone();
        Box::pin(async move {
            let page = reply.recv().await.map_err(|_| BackendError)?;
            let ids: Vec<MessageId> = page.rows.iter().map(|row| row.id).collect();
            if !ids.is_empty() {
                let reply = mailbox
                    .store()
                    .map(|store| store.set_flags(ids.clone(), Flags::SEEN, Flags::NONE))
                    .ok_or(BackendError)?;
                reply.recv().await.map_err(|_| BackendError)?;
            }
            let changes = page.rows.iter().map(|row| (row.id, row.flags)).collect();
            batches.borrow_mut().insert(
                batch,
                vec![Applied::Flags {
                    changes,
                    add: Flags::SEEN,
                    remove: Flags::NONE,
                }],
            );
            Ok(Queued {
                batch,
                applied: ids.len() as u32,
                skipped: 0,
            })
        })
    }

    fn undo(&self, batch: BatchId) -> Pending<Undone> {
        let Some(applied) = self.batches.borrow_mut().remove(&batch) else {
            return Box::pin(async {
                Ok(Undone {
                    restored: 0,
                    lost: 0,
                    why: None,
                })
            });
        };
        let Some(store) = self.mailbox.store() else {
            return failed();
        };
        let mut replies = Vec::new();
        let (mut restored, mut lost) = (0, 0);
        for part in applied {
            match part {
                Applied::Flags {
                    changes,
                    add,
                    remove,
                } => {
                    let added: Vec<MessageId> = changes
                        .iter()
                        .filter(|(_, before)| !before.contains(add))
                        .map(|(id, _)| *id)
                        .collect();
                    let cleared: Vec<MessageId> = changes
                        .iter()
                        .filter(|(_, before)| before.difference(remove) != *before)
                        .map(|(id, _)| *id)
                        .collect();
                    restored += changes.len() as u32;
                    if add != Flags::NONE && !added.is_empty() {
                        replies.push(store.set_flags(added, Flags::NONE, add));
                    }
                    if remove != Flags::NONE && !cleared.is_empty() {
                        replies.push(store.set_flags(cleared, remove, Flags::NONE));
                    }
                }
                Applied::Removed(count) => lost += count,
            }
        }
        drop(store);
        Box::pin(async move {
            for reply in replies {
                reply.recv().await.map_err(|_| BackendError)?;
            }
            Ok(Undone {
                restored,
                lost,
                why: (lost > 0).then_some(Lost::NotBuilt),
            })
        })
    }

    fn pending(&self, _account: AccountId) -> Pending<u32> {
        Box::pin(async { Ok(0) })
    }
}
