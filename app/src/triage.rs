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
pub struct BatchId(pub i64);

/// Why an action left messages alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NotApplied {
    /// The account has no folder for it, or the folder named takes no messages.
    NoTarget,
    /// The messages are already where the action would put them.
    AlreadyThere,
    /// Not in the store any more, or about to be deleted for good.
    Gone,
}

/// What the backend did with an action.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Queued {
    /// What Undo takes; `None` when no message changed.
    pub batch: Option<BatchId>,
    /// Messages the action applies to, those already as it asks included.
    pub applied: u32,
    /// Messages left alone, by reason.
    pub not_applied: Vec<(NotApplied, u32)>,
    /// Accounts with a server that have new ops: each gets `Command::OpsQueued`.
    pub accounts: Vec<AccountId>,
}

impl Queued {
    /// Messages left alone, for whatever reason.
    pub fn skipped(&self) -> u32 {
        self.not_applied.iter().map(|(_, count)| count).sum()
    }
}

/// What an Undo brought back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Undone {
    /// Messages put back before their change reached a server.
    pub restored: u32,
    /// Messages whose change reached the server, or may have, with the inverse queued.
    pub reverted: u32,
    /// Messages that can't come back, by reason.
    pub lost: Vec<(Lost, u32)>,
    /// Accounts with a server that have new ops: each gets `Command::OpsQueued`.
    pub accounts: Vec<AccountId>,
}

/// Why messages can't come back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lost {
    /// Deleted for good.
    Deleted,
    /// The store no longer knows where the message is.
    Unknown,
    /// A folder's unread messages on the server below its floor were marked read; which ones
    /// isn't known.
    ReadBelow,
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

/// Where actions are queued, shaped like the store's op queue (`queue_triage`,
/// `mark_folder_read`, `undo`, `pending_ops`). Each method sends its requests before it
/// returns, so the store takes them in the frame the key was pressed in; the future reports
/// the outcome.
pub trait Backend {
    /// Queues `action` on `targets` (they may span folders and accounts); moves and deletes
    /// wait for `not_before`, until which [`Backend::undo`] cancels them.
    fn queue(&self, action: Triage, targets: &[Target], not_before: SystemTime) -> Pending<Queued>;

    /// Marks every message of `folder` read.
    fn mark_folder_read(&self, folder: FolderId, not_before: SystemTime) -> Pending<Queued>;

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

/// The accounts with a server (a key) that `folders` belong to, each once.
pub fn server_accounts(
    folders: impl IntoIterator<Item = FolderId>,
    known: &[Folder],
    accounts: &[Account],
) -> Vec<AccountId> {
    let ids: BTreeSet<AccountId> = folders
        .into_iter()
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
pub fn done_text(action: Triage, queued: &Queued, plans: &[Plan], folders: &[Folder]) -> String {
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
    match queued.skipped() {
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
pub fn undone_text(undone: &Undone) -> String {
    let lost: Vec<String> = undone
        .lost
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|&(why, count)| {
            let n = messages(count);
            match why {
                Lost::Deleted => format!(
                    "{n} {} deleted for good and can't be brought back",
                    were(count)
                ),
                Lost::Unknown => format!("{n} couldn't be found to bring back"),
                Lost::ReadBelow => format!("{n} further back on the server stay read"),
                Lost::NotBuilt => format!("can't bring back {n} yet"),
            }
        })
        .collect();
    if lost.is_empty() {
        return "Undone".to_owned();
    }
    let text = lost
        .iter()
        .map(|part| capitalised(part))
        .collect::<Vec<_>>()
        .join(". ");
    if undone.restored + undone.reverted > 0 {
        format!("Undone. {text}")
    } else {
        text
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
    /// Messages it applied to.
    count: u32,
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
    /// [`UNDO_WINDOW`], shorter in the self-test.
    window: Cell<Duration>,
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
                window: Cell::new(UNDO_WINDOW),
            }),
        }
    }

    /// The undo window: how long moves wait and the toast shows.
    pub fn window(&self) -> Duration {
        self.inner.window.get()
    }

    /// Changes the undo window (the self-test waits less).
    pub fn set_window(&self, window: Duration) {
        self.inner.window.set(window);
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
            SystemTime::now() + self.window()
        } else {
            SystemTime::now()
        };
        let pending = self.inner.backend.queue(action, &targets, not_before);
        self.inner.queued.set(self.inner.queued.get() + 1);
        // A single message's read or star change shows in place and toggles back with its
        // key; it gets no toast and leaves the last action as it was.
        let notable = origin == Origin::User && (leaves || targets.len() > 1);
        let expunge = !plans.is_empty() && plans.iter().all(|plan| *plan == Plan::Expunge);
        Some(Started {
            pending,
            action,
            plans,
            folders,
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
        self.wake(&queued.accounts);
        if !notable {
            return None;
        }
        if queued.applied == 0 {
            return Some(Notice::plain("Nothing to change".to_owned()));
        }
        // No batch: nothing changed, and the last action stays as it was.
        if let Some(batch) = queued.batch {
            self.set_last(Some(Last {
                batch,
                until: expunge.then(|| Instant::now() + self.window()),
                count: queued.applied,
            }));
            if expunge {
                // Once the window ends Undo can't run any more: say so to the listeners.
                let this = self.clone();
                glib::timeout_add_local_once(self.window(), move || {
                    if this.last_batch() == Some(batch) {
                        let listeners = this.inner.changed.borrow().clone();
                        for listener in listeners {
                            listener();
                        }
                    }
                });
            }
        }
        Some(Notice {
            text: done_text(action, &queued, &plans, &folders),
            undo: queued.batch,
            shown: self.window(),
        })
    }

    /// Marks the messages of `folders` read: the folder the list shows, or every Inbox for
    /// All Inboxes. `name` names it in the toast.
    pub fn mark_folders_read(&self, folders: Vec<FolderId>, name: String) {
        let this = self.clone();
        let now = SystemTime::now();
        let pending: Vec<Pending<Queued>> = folders
            .iter()
            .map(|folder| self.inner.backend.mark_folder_read(*folder, now))
            .collect();
        self.inner.queued.set(self.inner.queued.get() + 1);
        self.spawn(async move {
            let mut applied = 0;
            let mut batches = Vec::new();
            let mut accounts = BTreeSet::new();
            for pending in pending {
                match pending.await {
                    Ok(queued) => {
                        applied += queued.applied;
                        batches.push(queued.batch);
                        accounts.extend(queued.accounts);
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
            this.wake(&accounts.into_iter().collect::<Vec<_>>());
            // Several Inboxes are several batches; Undo takes back the last one alone, so the
            // toast offers it only for one folder.
            let batch = match batches.as_slice() {
                [Some(batch)] if applied > 0 => Some(*batch),
                _ => None,
            };
            if let Some(batch) = batch {
                this.set_last(Some(Last {
                    batch,
                    until: None,
                    count: applied,
                }));
            }
            this.notify(&Notice {
                text: folder_read_text(applied, &name),
                undo: batch,
                shown: this.window(),
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
            self.notify(&Notice::plain(undone_text(&Undone {
                lost: vec![(Lost::Deleted, last.count)],
                ..Undone::default()
            })));
            return;
        }
        self.set_last(None);
        let pending = self.inner.backend.undo(last.batch);
        let this = self.clone();
        self.spawn(async move {
            match pending.await {
                Ok(undone) => {
                    this.wake(&undone.accounts);
                    this.notify(&Notice::plain(undone_text(&undone)));
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

/// A batch phase A's backend keeps for Undo.
struct Batch {
    applied: Vec<Applied>,
    /// The accounts with a server it touched.
    accounts: Vec<AccountId>,
}

/// Phase A's backend, over the store's `set_flags` and `remove`: actions take effect at once,
/// moved and deleted messages leave the store, and only flag changes can be undone.
pub struct StoreBackend {
    mailbox: Rc<Mailbox>,
    batches: Rc<RefCell<HashMap<BatchId, Batch>>>,
    next: Cell<i64>,
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
        let mut planned = Vec::new();
        let (mut no_target, mut already_there) = (0, 0);
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
                    planned.push(target.folder);
                }
                Ok(Plan::Move { .. } | Plan::Expunge) => {
                    removed.push(target.id);
                    planned.push(target.folder);
                }
                Err(Unavailable::NoTarget) => no_target += 1,
                Err(Unavailable::AlreadyThere) => already_there += 1,
            }
        }
        let not_applied: Vec<(NotApplied, u32)> = [
            (NotApplied::NoTarget, no_target),
            (NotApplied::AlreadyThere, already_there),
        ]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect();
        let accounts = server_accounts(planned, &folders, &self.mailbox.accounts());
        if flag_groups.is_empty() && removed.is_empty() {
            return Box::pin(async move {
                Ok(Queued {
                    not_applied,
                    ..Queued::default()
                })
            });
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
        self.batches.borrow_mut().insert(
            batch,
            Batch {
                applied,
                accounts: accounts.clone(),
            },
        );
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
                batch: Some(batch),
                applied: total - no_target - already_there,
                not_applied,
                accounts,
            })
        })
    }

    fn mark_folder_read(&self, folder: FolderId, _not_before: SystemTime) -> Pending<Queued> {
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
        let accounts = server_accounts([folder], &self.mailbox.folders(), &self.mailbox.accounts());
        let mailbox = self.mailbox.clone();
        let batch = self.batch();
        let batches = self.batches.clone();
        Box::pin(async move {
            let page = reply.recv().await.map_err(|_| BackendError)?;
            let ids: Vec<MessageId> = page.rows.iter().map(|row| row.id).collect();
            if ids.is_empty() {
                return Ok(Queued::default());
            }
            let reply = mailbox
                .store()
                .map(|store| store.set_flags(ids.clone(), Flags::SEEN, Flags::NONE))
                .ok_or(BackendError)?;
            reply.recv().await.map_err(|_| BackendError)?;
            let changes = page.rows.iter().map(|row| (row.id, row.flags)).collect();
            batches.borrow_mut().insert(
                batch,
                Batch {
                    applied: vec![Applied::Flags {
                        changes,
                        add: Flags::SEEN,
                        remove: Flags::NONE,
                    }],
                    accounts: accounts.clone(),
                },
            );
            Ok(Queued {
                batch: Some(batch),
                applied: ids.len() as u32,
                not_applied: Vec::new(),
                accounts,
            })
        })
    }

    fn undo(&self, batch: BatchId) -> Pending<Undone> {
        let Some(Batch { applied, accounts }) = self.batches.borrow_mut().remove(&batch) else {
            return Box::pin(async { Ok(Undone::default()) });
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
                reverted: 0,
                lost: if lost > 0 {
                    vec![(Lost::NotBuilt, lost)]
                } else {
                    Vec::new()
                },
                accounts,
            })
        })
    }

    fn pending(&self, _account: AccountId) -> Pending<u32> {
        Box::pin(async { Ok(0) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::AccountKey;

    fn folder(id: i64, account: i64, role: FolderRole, name: &str) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: name.to_owned(),
            role,
            selectable: true,
            total: 0,
            unread: 0,
        }
    }

    /// Account 1 has every role folder and a folder of its own; account 2 (a server account)
    /// has an Inbox, All Mail and Trash, as where folders are labels.
    fn folders() -> Vec<Folder> {
        vec![
            folder(1, 1, FolderRole::Inbox, "INBOX"),
            folder(2, 1, FolderRole::Archive, "Arkiv"),
            folder(3, 1, FolderRole::Junk, "Søppelpost"),
            folder(4, 1, FolderRole::Trash, "Papirkurv"),
            folder(5, 1, FolderRole::Other, "Prosjekter"),
            folder(6, 1, FolderRole::Sent, "Sendt"),
            Folder {
                selectable: false,
                ..folder(7, 1, FolderRole::Other, "Beholder")
            },
            folder(11, 2, FolderRole::Inbox, "INBOX"),
            folder(12, 2, FolderRole::All, "All Mail"),
            folder(13, 2, FolderRole::Trash, "Trash"),
        ]
    }

    fn accounts() -> Vec<Account> {
        vec![
            Account {
                id: AccountId(1),
                name: "Personal".into(),
                address: "personal@example.test".into(),
                key: None,
            },
            Account {
                id: AccountId(2),
                name: "Work".into(),
                address: "work@example.test".into(),
                key: Some(AccountKey("work".into())),
            },
        ]
    }

    fn target(id: i64, folder: i64, flags: Flags) -> Target {
        Target {
            id: MessageId(id),
            folder: FolderId(folder),
            flags,
        }
    }

    #[test]
    fn commands_apply_where_the_plan_does_and_say_why_not() {
        let all = folders();
        let inbox = [target(1, 1, Flags::NONE)];
        let archived = [target(2, 2, Flags::SEEN)];
        let labels = [target(3, 11, Flags::NONE)];
        assert!(availability(ActionId::Archive, &inbox, &all).enabled());
        assert!(availability(ActionId::Archive, &labels, &all).enabled());
        assert_eq!(
            availability(ActionId::Archive, &archived, &all).reason,
            Some("already archived")
        );
        let no_archive = vec![folder(1, 1, FolderRole::Inbox, "INBOX")];
        assert_eq!(
            availability(ActionId::Archive, &inbox, &no_archive).reason,
            Some("the account has no Archive folder")
        );
        assert_eq!(
            availability(ActionId::Junk, &labels, &all).reason,
            Some("the account has no Junk folder")
        );
        let junk = [target(4, 3, Flags::NONE)];
        assert_eq!(availability(ActionId::Junk, &junk, &all).title, "Not Junk");
        assert_eq!(
            availability(ActionId::Junk, &inbox, &all).title,
            "Move to Junk"
        );
        assert!(availability(ActionId::Delete, &archived, &all).enabled());
        assert_eq!(
            availability(ActionId::MarkRead, &archived, &all).reason,
            Some("already read")
        );
        assert_eq!(
            availability(ActionId::MarkUnread, &inbox, &all).reason,
            Some("already unread")
        );
        let mixed = [target(1, 1, Flags::NONE), target(2, 2, Flags::SEEN)];
        assert_eq!(availability(ActionId::MarkRead, &mixed, &all).applies, 1);
        assert_eq!(availability(ActionId::Archive, &mixed, &all).applies, 1);
        assert_eq!(availability(ActionId::Archive, &[], &all).reason, None);
        assert!(!availability(ActionId::Archive, &[], &all).enabled());
    }

    #[test]
    fn star_stars_all_unless_all_are_starred() {
        let all = folders();
        let some = [target(1, 1, Flags::FLAGGED), target(2, 1, Flags::NONE)];
        let every = [target(1, 1, Flags::FLAGGED), target(2, 1, Flags::FLAGGED)];
        assert_eq!(star_action(&some), Triage::Star(true));
        assert_eq!(star_action(&every), Triage::Star(false));
        assert_eq!(availability(ActionId::Star, &some, &all).title, "Star");
        assert_eq!(availability(ActionId::Star, &every, &all).title, "Unstar");
    }

    #[test]
    fn move_to_offers_the_accounts_other_folders_that_take_messages() {
        let all = folders();
        let names = |targets: &[Target]| -> Result<Vec<String>, &'static str> {
            move_choices(targets, &all).map(|choices| choices.iter().map(folder_title).collect())
        };
        assert_eq!(
            names(&[target(1, 1, Flags::NONE)]).unwrap(),
            ["Archive", "Sent", "Junk", "Trash", "Prosjekter"]
        );
        // Messages from two folders: every folder that takes messages.
        assert_eq!(
            names(&[target(1, 1, Flags::NONE), target(2, 2, Flags::NONE)])
                .unwrap()
                .len(),
            6
        );
        let across = [target(1, 1, Flags::NONE), target(3, 11, Flags::NONE)];
        assert_eq!(
            names(&across),
            Err("the messages are in more than one account")
        );
        assert_eq!(
            availability(ActionId::MoveTo, &across, &all).reason,
            Some("the messages are in more than one account")
        );
    }

    #[test]
    fn toasts_say_what_happened() {
        let all = folders();
        let queued = |applied, skipped| Queued {
            batch: Some(BatchId(1)),
            applied,
            not_applied: vec![(NotApplied::NoTarget, skipped)],
            accounts: Vec::new(),
        };
        let to = |id| Plan::Move {
            to: FolderId(id),
            add: Flags::NONE,
            remove: Flags::NONE,
        };
        assert_eq!(
            done_text(Triage::Archive, &queued(1, 0), &[to(2)], &all),
            "Archived 1 message"
        );
        assert_eq!(
            done_text(Triage::Delete, &queued(3, 0), &[to(4); 3], &all),
            "Moved 3 messages to the Trash"
        );
        assert_eq!(
            done_text(Triage::Delete, &queued(2, 0), &[Plan::Expunge; 2], &all),
            "Deleted 2 messages for good"
        );
        let not_junk = Plan::Move {
            to: FolderId(1),
            add: Flags::NOT_JUNK,
            remove: Flags::JUNK,
        };
        assert_eq!(
            done_text(Triage::Junk, &queued(1, 0), &[not_junk], &all),
            "Moved 1 message to the Inbox as not junk"
        );
        assert_eq!(
            done_text(
                Triage::MoveTo(FolderId(5)),
                &queued(2, 0),
                &[to(5); 2],
                &all
            ),
            "Moved 2 messages to Prosjekter"
        );
        assert_eq!(
            done_text(Triage::Junk, &queued(2, 1), &[to(3); 2], &all),
            "Moved 2 messages to Junk. 1 message was left as it was"
        );
        assert_eq!(
            done_text(Triage::Star(false), &queued(4, 0), &[], &all),
            "Unstarred 4 messages"
        );
        assert_eq!(
            folder_read_text(12, "Inbox"),
            "Marked 12 messages in Inbox as read"
        );
        assert_eq!(folder_read_text(0, "Inbox"), "Nothing in Inbox was unread");
    }

    #[test]
    fn undo_and_refusals_say_what_came_back() {
        let undone = |restored, reverted, lost: &[(Lost, u32)]| Undone {
            restored,
            reverted,
            lost: lost.to_vec(),
            accounts: Vec::new(),
        };
        assert_eq!(undone_text(&undone(3, 0, &[])), "Undone");
        assert_eq!(undone_text(&undone(1, 2, &[(Lost::Deleted, 0)])), "Undone");
        assert_eq!(
            undone_text(&undone(0, 0, &[(Lost::Deleted, 1)])),
            "1 message was deleted for good and can't be brought back"
        );
        assert_eq!(
            undone_text(&undone(2, 0, &[(Lost::Deleted, 3)])),
            "Undone. 3 messages were deleted for good and can't be brought back"
        );
        assert_eq!(
            undone_text(&undone(0, 0, &[(Lost::NotBuilt, 2)])),
            "Can't bring back 2 messages yet"
        );
        assert_eq!(
            undone_text(&undone(0, 4, &[(Lost::Unknown, 1), (Lost::ReadBelow, 40)])),
            "Undone. 1 message couldn't be found to bring back. 40 messages further back on the \
             server stay read"
        );
        assert_eq!(
            refused_text(Refused::Move, 3, None),
            "The server didn't move 3 messages; they're back where they were"
        );
        assert_eq!(
            refused_text(Refused::Flags, 1, Some("Work")),
            "Work: the server didn't change 1 message; it's back as it was"
        );
        assert_eq!(
            refused_text(Refused::Expunge, 2, None),
            "The server didn't delete 2 messages; they're back"
        );
        assert_eq!(
            refused_text(Refused::FolderRead, 12, None),
            "The server didn't mark 12 messages as read; they're back as they were"
        );
    }

    /// Records what it is asked and answers at once, naming the server accounts of the
    /// messages' folders as the store does.
    #[derive(Default)]
    struct Recording {
        calls: RefCell<Vec<String>>,
        next: Cell<i64>,
        accounts: RefCell<HashMap<BatchId, Vec<AccountId>>>,
        /// Answers that nothing changed (no batch).
        unchanged: Cell<bool>,
    }

    impl Recording {
        fn batch(&self, applied: u32, sources: impl IntoIterator<Item = FolderId>) -> Queued {
            if self.unchanged.get() {
                return Queued {
                    applied,
                    ..Queued::default()
                };
            }
            self.next.set(self.next.get() + 1);
            let batch = BatchId(self.next.get());
            let accounts = server_accounts(sources, &folders(), &accounts());
            self.accounts.borrow_mut().insert(batch, accounts.clone());
            Queued {
                batch: Some(batch),
                applied,
                not_applied: Vec::new(),
                accounts,
            }
        }
    }

    impl Backend for Recording {
        fn queue(
            &self,
            action: Triage,
            targets: &[Target],
            not_before: SystemTime,
        ) -> Pending<Queued> {
            let waits = not_before > SystemTime::now() + Duration::from_secs(1);
            self.calls
                .borrow_mut()
                .push(format!("{action:?} x{} waits {waits}", targets.len()));
            let queued = self.batch(
                targets.len() as u32,
                targets.iter().map(|target| target.folder),
            );
            Box::pin(async move { Ok(queued) })
        }

        fn mark_folder_read(&self, folder: FolderId, _not_before: SystemTime) -> Pending<Queued> {
            self.calls.borrow_mut().push(format!("read {}", folder.0));
            let queued = self.batch(2, [folder]);
            Box::pin(async move { Ok(queued) })
        }

        fn undo(&self, batch: BatchId) -> Pending<Undone> {
            self.calls.borrow_mut().push(format!("undo {}", batch.0));
            let accounts = self
                .accounts
                .borrow_mut()
                .remove(&batch)
                .unwrap_or_default();
            Box::pin(async move {
                Ok(Undone {
                    restored: 1,
                    accounts,
                    ..Undone::default()
                })
            })
        }

        fn pending(&self, _account: AccountId) -> Pending<u32> {
            Box::pin(async { Ok(0) })
        }
    }

    struct Fixed;

    impl Directory for Fixed {
        fn folders(&self) -> Vec<Folder> {
            folders()
        }

        fn accounts(&self) -> Vec<Account> {
            accounts()
        }
    }

    /// A controller over a recording backend, with an engine link whose commands the test
    /// reads.
    fn controller() -> (
        Controller,
        Rc<Recording>,
        async_channel::Receiver<Command>,
        async_channel::Sender<sealwax_engine::api::Event>,
    ) {
        let backend = Rc::new(Recording::default());
        let link = EngineLink::new();
        let (commands, engine) = async_channel::unbounded();
        let (events, receiver) = async_channel::unbounded();
        link.attach(commands, receiver);
        let controller = Controller::new(backend.clone(), Rc::new(Fixed), link);
        (controller, backend, engine, events)
    }

    fn woken(engine: &async_channel::Receiver<Command>) -> Vec<i64> {
        std::iter::from_fn(|| engine.try_recv().ok())
            .filter_map(|command| match command {
                Command::OpsQueued { account } => Some(account.0),
                _ => None,
            })
            .collect()
    }

    fn drain(context: &glib::MainContext) {
        while context.iteration(false) {}
    }

    #[test]
    fn notable_actions_become_the_last_and_wake_server_accounts() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let (controller, backend, engine, _events) = controller();
                // A single star: no toast, not the last action; a local account wakes nothing.
                let notice = context.block_on(controller.run(
                    Triage::Star(true),
                    vec![target(1, 1, Flags::NONE)],
                    Origin::User,
                ));
                assert_eq!(notice, None);
                assert!(!controller.can_undo());
                assert!(woken(&engine).is_empty());
                // Archive on the server account: it waits for the window and wakes the account.
                let notice = context
                    .block_on(controller.run(
                        Triage::Archive,
                        vec![target(2, 11, Flags::NONE)],
                        Origin::User,
                    ))
                    .expect("a toast");
                assert_eq!(notice.text, "Archived 1 message");
                assert_eq!(notice.undo, Some(BatchId(2)));
                assert_eq!(notice.shown, UNDO_WINDOW);
                assert_eq!(controller.last_batch(), Some(BatchId(2)));
                assert_eq!(woken(&engine), [2]);
                // Two messages read: a toast and the last action, without the window.
                let notice = context
                    .block_on(controller.run(
                        Triage::Read(true),
                        vec![target(3, 1, Flags::NONE), target(4, 1, Flags::NONE)],
                        Origin::User,
                    ))
                    .expect("a toast");
                assert_eq!(notice.text, "Marked 2 messages as read");
                assert_eq!(controller.last_batch(), Some(BatchId(3)));
                // Opened: never a toast, never the last action.
                let notice = context.block_on(controller.run(
                    Triage::Read(true),
                    vec![target(5, 1, Flags::NONE), target(6, 1, Flags::NONE)],
                    Origin::Opened,
                ));
                assert_eq!(notice, None);
                assert_eq!(controller.last_batch(), Some(BatchId(3)));
                assert_eq!(
                    *backend.calls.borrow(),
                    [
                        "Star(true) x1 waits false",
                        "Archive x1 waits true",
                        "Read(true) x2 waits false",
                        "Read(true) x2 waits false",
                    ]
                );
                // Nothing changed (no batch): a toast without Undo, the last action kept.
                backend.unchanged.set(true);
                let notice = context
                    .block_on(controller.run(
                        Triage::Read(false),
                        vec![target(7, 11, Flags::NONE), target(8, 11, Flags::NONE)],
                        Origin::User,
                    ))
                    .expect("a toast");
                assert_eq!(notice.text, "Marked 2 messages as unread");
                assert_eq!(notice.undo, None);
                assert_eq!(controller.last_batch(), Some(BatchId(3)));
                assert!(woken(&engine).is_empty());
            })
            .unwrap();
    }

    #[test]
    fn only_the_last_action_is_undone_and_deleting_for_good_only_in_the_window() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let (controller, backend, engine, _events) = controller();
                let notices: Rc<RefCell<Vec<Notice>>> = Rc::default();
                let record = notices.clone();
                controller.connect_notice(move |notice| record.borrow_mut().push(notice.clone()));
                for (id, folder) in [(1, 1), (2, 11)] {
                    context.block_on(controller.run(
                        Triage::Archive,
                        vec![target(id, folder, Flags::NONE)],
                        Origin::User,
                    ));
                }
                assert_eq!(woken(&engine), [2]);
                // The first toast's Undo is no longer the last action's: nothing happens.
                controller.undo(Some(BatchId(1)));
                drain(&context);
                assert!(controller.can_undo());
                controller.undo(None);
                drain(&context);
                assert!(!controller.can_undo());
                assert_eq!(backend.calls.borrow().last().unwrap(), "undo 2");
                assert_eq!(notices.borrow().last().unwrap().text, "Undone");
                // The undo wakes the server account the batch touched.
                assert_eq!(woken(&engine), [2]);
                // Deleting for good from the Trash: undoable only inside the window.
                controller.set_window(Duration::from_millis(20));
                context.block_on(controller.run(
                    Triage::Delete,
                    vec![target(3, 4, Flags::NONE), target(4, 4, Flags::NONE)],
                    Origin::User,
                ));
                assert!(controller.can_undo());
                std::thread::sleep(Duration::from_millis(30));
                assert!(!controller.can_undo());
                controller.undo(Some(BatchId(3)));
                drain(&context);
                assert_eq!(
                    notices.borrow().last().unwrap().text,
                    "2 messages were deleted for good and can't be brought back"
                );
                assert_ne!(backend.calls.borrow().last().unwrap(), "undo 3");
            })
            .unwrap();
    }

    #[test]
    fn marking_folders_read_offers_undo_for_one_folder() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let (controller, backend, engine, _events) = controller();
                let notices: Rc<RefCell<Vec<Notice>>> = Rc::default();
                let record = notices.clone();
                controller.connect_notice(move |notice| record.borrow_mut().push(notice.clone()));
                controller.mark_folders_read(vec![FolderId(11)], "Inbox".into());
                drain(&context);
                let last = notices.borrow().last().cloned().unwrap();
                assert_eq!(last.text, "Marked 2 messages in Inbox as read");
                assert!(last.undo.is_some());
                assert_eq!(woken(&engine), [2]);
                controller.mark_folders_read(vec![FolderId(1), FolderId(11)], "All Inboxes".into());
                drain(&context);
                let last = notices.borrow().last().cloned().unwrap();
                assert_eq!(last.text, "Marked 4 messages in All Inboxes as read");
                assert_eq!(last.undo, None);
                assert_eq!(backend.calls.borrow().len(), 3);
            })
            .unwrap();
    }
}
