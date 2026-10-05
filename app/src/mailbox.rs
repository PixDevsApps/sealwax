//! The message store in the app (ADR-003, ADR-005). The store and the blob store open on a
//! thread of their own as the application starts, so the GTK thread never runs SQL; the
//! mailbox then lends the store to the list and the reader, turns accounts and folders into the
//! sidebar's model, keeps it current from change events, passes every change event on to the
//! lists, and closes the store when the application shuts down.

use crate::list::model::PAGE;
use crate::list::sections::Thresholds;
use crate::window::{
    ALL_INBOXES, AccountStatus, FolderRole, SidebarAccount, SidebarFolder, SidebarModel,
};
use gtk4::glib;
use sealwax_domain::model::{Account, AccountId, Folder, FolderId, FolderRole as StoreRole};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    ChangeEvent, FolderCounts, OpenView, Store, StoreOptions, StoreVersion, Versioned, ViewFilter,
    ViewOpened, ViewScope, ViewSpec,
};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A folder's key in the sidebar and in `nav.folder`.
pub fn folder_key(folder: FolderId) -> String {
    format!("folder-{folder}")
}

fn account_key(account: &Account) -> String {
    format!("account-{}", account.id)
}

/// The sidebar's role for a store folder; the rest go under "More".
fn sidebar_role(role: StoreRole) -> Option<FolderRole> {
    match role {
        StoreRole::Inbox => Some(FolderRole::Inbox),
        StoreRole::Drafts => Some(FolderRole::Drafts),
        StoreRole::Sent => Some(FolderRole::Sent),
        StoreRole::Archive => Some(FolderRole::Archive),
        StoreRole::Junk => Some(FolderRole::Junk),
        StoreRole::Trash => Some(FolderRole::Trash),
        StoreRole::All | StoreRole::Other => None,
    }
}

/// Accounts and folders as the sidebar shows them: role folders with their unread counts (drafts
/// for Drafts), other folders under "More". An account the engine hasn't reported on shows as
/// synced.
pub fn sidebar_model(
    accounts: &[Account],
    folders: &[Folder],
    statuses: &HashMap<AccountId, AccountStatus>,
) -> SidebarModel {
    SidebarModel {
        accounts: accounts
            .iter()
            .map(|account| {
                let mut role_folders = Vec::new();
                let mut more = Vec::new();
                for folder in folders.iter().filter(|folder| folder.account == account.id) {
                    let role = sidebar_role(folder.role);
                    let entry = SidebarFolder {
                        key: folder_key(folder.id),
                        name: folder.name.clone(),
                        role,
                        count: if role == Some(FolderRole::Drafts) {
                            folder.total
                        } else {
                            folder.unread
                        },
                    };
                    if role.is_some() {
                        role_folders.push(entry);
                    } else {
                        more.push(entry);
                    }
                }
                SidebarAccount {
                    key: account_key(account),
                    name: account.name.clone(),
                    status: statuses
                        .get(&account.id)
                        .copied()
                        .unwrap_or(AccountStatus::Synced),
                    folders: role_folders,
                    more,
                }
            })
            .collect(),
    }
}

/// What a sidebar key shows: a folder, or the unified inbox.
pub fn scope_for_key(key: &str, folders: &[Folder]) -> Option<ViewScope> {
    if key == ALL_INBOXES {
        return Some(ViewScope::UnifiedInbox);
    }
    let id: i64 = key.strip_prefix("folder-")?.parse().ok()?;
    folders
        .iter()
        .any(|folder| folder.id.0 == id)
        .then_some(ViewScope::Folder(FolderId(id)))
}

#[derive(Debug, Default)]
enum Phase {
    #[default]
    Idle,
    Opening,
    Open,
    Failed(String),
    Closed,
}

/// A one-shot signal that futures can wait for.
#[derive(Default)]
pub struct Latch {
    open: Cell<bool>,
    waiters: RefCell<Vec<async_channel::Sender<()>>>,
}

impl Latch {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn open(&self) {
        if self.open.replace(true) {
            return;
        }
        for waiter in self.waiters.take() {
            let _ = waiter.try_send(());
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn wait(self: &Rc<Self>) -> impl Future<Output = ()> + 'static {
        let receiver = if self.open.get() {
            None
        } else {
            let (sender, receiver) = async_channel::bounded(1);
            self.waiters.borrow_mut().push(sender);
            Some(receiver)
        };
        async move {
            if let Some(receiver) = receiver {
                let _ = receiver.recv().await;
            }
        }
    }
}

/// Resolves with `future`'s output, or `None` once `limit` has passed.
pub async fn with_timeout<T>(future: impl Future<Output = T>, limit: Duration) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let mut timeout = std::pin::pin!(glib::timeout_future(limit));
    std::future::poll_fn(|context| {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(context) {
            return std::task::Poll::Ready(Some(value));
        }
        if timeout.as_mut().poll(context).is_ready() {
            return std::task::Poll::Ready(None);
        }
        std::task::Poll::Pending
    })
    .await
}

type EventListener = Rc<dyn Fn(&Arc<ChangeEvent>)>;
type Listener = Rc<dyn Fn()>;

/// The view the first window shows, read ahead.
pub struct FirstView {
    pub spec: ViewSpec,
    pub thresholds: Thresholds,
    pub opened: ViewOpened,
}

/// What the open thread hands to the GTK thread.
struct Opened {
    store: Store,
    blobs: BlobStore,
    events: async_channel::Receiver<Arc<ChangeEvent>>,
    accounts: Versioned<Vec<Account>>,
    folders: Versioned<Vec<Folder>>,
    first: Option<FirstView>,
    elapsed: Duration,
}

/// Opens the blob store and the store, subscribes to change events, reads the accounts and
/// folders, and opens the view a new window shows first (its first account's Inbox) with
/// today's sections. Runs on the open thread; it may block.
fn open_store(store_file: &Path, blobs_dir: &Path) -> Result<Opened, String> {
    let started = Instant::now();
    let blobs = BlobStore::open(blobs_dir).map_err(|error| format!("blob store: {error}"))?;
    let options = StoreOptions {
        blobs: Some(blobs.clone()),
        ..StoreOptions::default()
    };
    let store = Store::open(store_file, options).map_err(|error| error.to_string())?;
    let events = store.subscribe();
    let accounts = store
        .accounts()
        .recv_blocking()
        .map_err(|error| error.to_string())?;
    let folders = store
        .folders()
        .recv_blocking()
        .map_err(|error| error.to_string())?;
    let scope = sidebar_model(&accounts.value, &folders.value, &HashMap::new())
        .default_selection()
        .and_then(|key| scope_for_key(&key, &folders.value));
    let thresholds = glib::DateTime::now_local()
        .ok()
        .and_then(|now| Thresholds::at(&now).ok());
    let first = match (scope, thresholds) {
        (Some(scope), Some(thresholds)) => {
            let spec = ViewSpec {
                scope,
                filter: ViewFilter::All,
            };
            store
                .open_view(OpenView {
                    spec,
                    first_page: PAGE,
                    section_starts: thresholds.starts(),
                })
                .recv_blocking()
                .ok()
                .map(|opened| FirstView {
                    spec,
                    thresholds,
                    opened,
                })
        }
        _ => None,
    };
    Ok(Opened {
        store,
        blobs,
        events,
        accounts,
        folders,
        first,
        elapsed: started.elapsed(),
    })
}

#[derive(Default)]
pub struct Mailbox {
    phase: RefCell<Phase>,
    store: RefCell<Option<Arc<Store>>>,
    blobs: RefCell<Option<BlobStore>>,
    /// What the engine last reported for each account.
    statuses: RefCell<HashMap<AccountId, AccountStatus>>,
    accounts: RefCell<Vec<Account>>,
    folders: RefCell<Vec<Folder>>,
    /// The version the folder list was read at; change events up to it are already in it.
    folders_version: Cell<StoreVersion>,
    /// A folder read is on its way; counts that arrive meanwhile wait here.
    reading: Cell<bool>,
    read_again: Cell<bool>,
    buffered: RefCell<Vec<(StoreVersion, Vec<FolderCounts>)>>,
    sidebar_queued: Cell<bool>,
    event_listeners: RefCell<Vec<EventListener>>,
    folder_listeners: RefCell<Vec<Listener>>,
    ready: Rc<Latch>,
    first_view: RefCell<Option<FirstView>>,
}

impl Mailbox {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Opens the store and the blob store at `dirs` on a thread of their own, which also reads
    /// the accounts and folders and opens the view the window shows first (S6: the first page is
    /// then ready long before the window needs it). Neither the blob store's directory scan nor
    /// any SQL runs on the calling thread. Call it as early as possible
    /// (`AppState::connect_early_startup`).
    pub fn open(self: &Rc<Self>, dirs: &SealwaxDirs) {
        if !matches!(*self.phase.borrow(), Phase::Idle) {
            return;
        }
        self.phase.replace(Phase::Opening);
        let store_file = dirs.store_file();
        let blobs_dir = dirs.blobs_dir();
        let (sender, receiver) = async_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("sealwax-mailbox-open".into())
            .spawn(move || {
                let _ = sender.send_blocking(open_store(&store_file, &blobs_dir));
            });
        if let Err(error) = spawned {
            self.fail(format!("cannot start the store thread: {error}"));
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let opened = receiver.recv().await;
            let Some(mailbox) = weak.upgrade() else {
                return;
            };
            match opened {
                Ok(Ok(opened)) => mailbox.opened(opened),
                Ok(Err(error)) => mailbox.fail(error),
                Err(_) => mailbox.fail("the store thread ended without a reply".to_owned()),
            }
        });
    }

    fn opened(self: &Rc<Self>, opened: Opened) {
        let Opened {
            store,
            blobs,
            events,
            accounts,
            folders,
            first,
            elapsed,
        } = opened;
        if matches!(*self.phase.borrow(), Phase::Closed) {
            drop(first);
            let _ = store.close();
            return;
        }
        tracing::info!(
            ms = elapsed.as_secs_f64() * 1000.0,
            accounts = accounts.value.len(),
            folders = folders.value.len(),
            first_view = first.is_some(),
            "store opened"
        );
        self.store.replace(Some(Arc::new(store)));
        self.blobs.replace(Some(blobs));
        self.folders_version.set(folders.version);
        self.accounts.replace(accounts.value);
        self.folders.replace(folders.value);
        self.first_view.replace(first);
        self.phase.replace(Phase::Open);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(mailbox) = weak.upgrade() else {
                    break;
                };
                mailbox.dispatch(&event);
            }
        });
        self.ready.open();
        self.notify_folders();
    }

    /// The view the open thread read ahead for the first window, when it is `spec` with these
    /// sections; only once.
    pub fn take_first_view(&self, spec: ViewSpec, thresholds: Thresholds) -> Option<ViewOpened> {
        let first = self.first_view.take()?;
        (first.spec == spec && first.thresholds == thresholds).then_some(first.opened)
    }

    fn fail(&self, message: String) {
        tracing::error!("the message store could not be opened: {message}");
        self.phase.replace(Phase::Failed(message));
        self.ready.open();
        self.notify_folders();
    }

    /// Whether the store is open.
    pub fn is_open(&self) -> bool {
        matches!(*self.phase.borrow(), Phase::Open)
    }

    /// Why the store could not be opened.
    pub fn error(&self) -> Option<String> {
        match &*self.phase.borrow() {
            Phase::Failed(message) => Some(message.clone()),
            _ => None,
        }
    }

    /// Whether the first folder list is in, or the store failed to open.
    pub fn is_ready(&self) -> bool {
        self.ready.is_open()
    }

    /// Resolves once the first folder list is in, or the store failed to open.
    pub fn wait_ready(&self) -> impl Future<Output = ()> + 'static {
        self.ready.wait()
    }

    /// The open store, for requests. Requests only send: hold the borrow for the call, never
    /// across an `await`.
    pub fn store(&self) -> Option<Ref<'_, Store>> {
        Ref::filter_map(self.store.borrow(), |store| store.as_deref()).ok()
    }

    /// The open store for the engine, which writes what it syncs.
    pub fn shared_store(&self) -> Option<Arc<Store>> {
        self.store.borrow().clone()
    }

    pub fn blobs(&self) -> Option<BlobStore> {
        self.blobs.borrow().clone()
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.accounts.borrow().clone()
    }

    pub fn folders(&self) -> Vec<Folder> {
        self.folders.borrow().clone()
    }

    pub fn folder(&self, id: FolderId) -> Option<Folder> {
        self.folders
            .borrow()
            .iter()
            .find(|folder| folder.id == id)
            .cloned()
    }

    pub fn sidebar_model(&self) -> SidebarModel {
        sidebar_model(
            &self.accounts.borrow(),
            &self.folders.borrow(),
            &self.statuses.borrow(),
        )
    }

    /// Shows what the engine reported for `account` in the sidebar.
    pub fn set_status(self: &Rc<Self>, account: AccountId, status: AccountStatus) {
        if self.statuses.borrow_mut().insert(account, status) != Some(status) {
            self.queue_sidebar();
        }
    }

    pub fn scope_for_key(&self, key: &str) -> Option<ViewScope> {
        scope_for_key(key, &self.folders.borrow())
    }

    /// Runs `listener` for every change event, in commit order.
    pub fn connect_event(&self, listener: impl Fn(&Arc<ChangeEvent>) + 'static) {
        self.event_listeners.borrow_mut().push(Rc::new(listener));
    }

    /// Runs `listener` when accounts, folders or their counts change (at most once per main
    /// loop iteration), and when the store fails to open.
    pub fn connect_folders_changed(&self, listener: impl Fn() + 'static) {
        self.folder_listeners.borrow_mut().push(Rc::new(listener));
    }

    fn dispatch(self: &Rc<Self>, event: &Arc<ChangeEvent>) {
        if event.folders_changed {
            self.read_folders();
        } else if !event.folders.is_empty() {
            if self.reading.get() {
                self.buffered
                    .borrow_mut()
                    .push((event.version, event.folders.clone()));
            } else if event.version > self.folders_version.get() {
                self.apply_counts(&event.folders);
                self.queue_sidebar();
            }
        }
        let listeners = self.event_listeners.borrow().clone();
        for listener in listeners {
            listener(event);
        }
    }

    fn apply_counts(&self, counts: &[FolderCounts]) {
        let mut folders = self.folders.borrow_mut();
        for count in counts {
            if let Some(folder) = folders.iter_mut().find(|folder| folder.id == count.folder) {
                folder.total = count.total;
                folder.unread = count.unread;
            }
        }
    }

    /// Reads accounts and folders again; counts from events that arrive meanwhile are applied
    /// on top when they are newer than the read.
    fn read_folders(self: &Rc<Self>) {
        if self.reading.replace(true) {
            self.read_again.set(true);
            return;
        }
        let Some((accounts, folders)) = self
            .store()
            .map(|store| (store.accounts(), store.folders()))
        else {
            self.reading.set(false);
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let (accounts, folders) = (accounts.recv().await, folders.recv().await);
            let Some(mailbox) = weak.upgrade() else {
                return;
            };
            mailbox.reading.set(false);
            match (accounts, folders) {
                (Ok(accounts), Ok(folders)) => {
                    let version = folders.version;
                    mailbox.accounts.replace(accounts.value);
                    mailbox.folders.replace(folders.value);
                    mailbox.folders_version.set(version);
                    let buffered = mailbox.buffered.take();
                    for (event_version, counts) in buffered {
                        if event_version > version {
                            mailbox.apply_counts(&counts);
                        }
                    }
                }
                (Err(error), _) | (_, Err(error)) => {
                    tracing::warn!("the folder list could not be read: {error}");
                    mailbox.buffered.take();
                }
            }
            if mailbox.read_again.replace(false) {
                mailbox.read_folders();
            }
            mailbox.notify_folders();
        });
    }

    fn queue_sidebar(self: &Rc<Self>) {
        if self.sidebar_queued.replace(true) {
            return;
        }
        let weak: Weak<Self> = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(mailbox) = weak.upgrade() {
                mailbox.sidebar_queued.set(false);
                mailbox.notify_folders();
            }
        });
    }

    fn notify_folders(&self) {
        let listeners = self.folder_listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    /// Closes the store: queued writes finish, and the writer leaves an empty WAL. It waits for
    /// the store's threads, so call it once the main loop has ended (`shutdown`).
    pub fn close(&self) {
        self.phase.replace(Phase::Closed);
        self.event_listeners.borrow_mut().clear();
        self.folder_listeners.borrow_mut().clear();
        self.first_view.take();
        let Some(store) = self.store.borrow_mut().take() else {
            return;
        };
        let Ok(store) = Arc::try_unwrap(store) else {
            tracing::warn!("the store is still in use; it closes without waiting for its writes");
            return;
        };
        let started = Instant::now();
        match store.close() {
            Ok(()) => tracing::info!(
                ms = started.elapsed().as_secs_f64() * 1000.0,
                "store closed"
            ),
            Err(error) => tracing::warn!("the store did not close cleanly: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::AccountKey;

    fn folder(id: i64, account: i64, name: &str, role: StoreRole, unread: u32) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: name.to_owned(),
            role,
            total: unread * 3,
            unread,
        }
    }

    #[test]
    fn accounts_and_folders_become_the_sidebar() {
        let accounts = vec![
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
        ];
        let folders = vec![
            folder(1, 1, "INBOX", StoreRole::Inbox, 12),
            folder(2, 1, "Utkast", StoreRole::Drafts, 0),
            folder(3, 1, "Lister", StoreRole::Other, 5),
            folder(4, 2, "INBOX", StoreRole::Inbox, 4),
            folder(5, 2, "All Mail", StoreRole::All, 9),
        ];
        let model = sidebar_model(&accounts, &folders, &HashMap::new());
        assert_eq!(model.accounts.len(), 2);
        let personal = &model.accounts[0];
        assert_eq!(personal.key, "account-1");
        assert_eq!(personal.folders.len(), 2);
        assert_eq!(personal.folders[0].key, "folder-1");
        assert_eq!(personal.folders[0].count, 12);
        assert_eq!(personal.folders[1].role, Some(FolderRole::Drafts));
        assert_eq!(personal.folders[1].count, 0, "Drafts counts its drafts");
        assert_eq!(personal.more.len(), 1);
        assert_eq!(personal.more[0].name, "Lister");
        assert_eq!(model.accounts[1].more[0].role, None);
        assert_eq!(model.all_inboxes_unread(), 16);
        assert_eq!(model.default_selection().as_deref(), Some("folder-1"));
    }

    #[test]
    fn keys_name_folders_and_the_unified_inbox() {
        let folders = vec![folder(7, 1, "INBOX", StoreRole::Inbox, 0)];
        assert_eq!(folder_key(FolderId(7)), "folder-7");
        assert_eq!(
            scope_for_key("folder-7", &folders),
            Some(ViewScope::Folder(FolderId(7)))
        );
        assert_eq!(
            scope_for_key(ALL_INBOXES, &folders),
            Some(ViewScope::UnifiedInbox)
        );
        assert_eq!(scope_for_key("folder-8", &folders), None);
        assert_eq!(scope_for_key("account-1", &folders), None);
    }
}
