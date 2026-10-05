//! The engine service (ADR-003): a thread of its own runs the engine's tokio runtime, takes the
//! app's commands and sends events back. M2: autodetect, the login check, adding and removing
//! accounts, and for every account its folder list and first INBOX header sync; M3 grows it
//! into the sync engine.
//!
//! Autodetect and login checks run side by side. Everything that touches `accounts.toml`, the
//! keyring entries or an account's store rows runs in one lane, one job at a time, so an add
//! never races a removal or a reconcile.

use crate::accounts::Accounts;
use crate::api::{Command, Event, Password, RequestId};
use crate::autoconfig::{self, DetectOptions};
use crate::imap::{self, Credentials, SyncError};
use crate::net::Trust;
use crate::secrets::{MemoryStore, SecretKind, SecretStore, Secrets};
use crate::verify::verify;
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, LoginError, LoginFailure, Protocol,
};
use sealwax_domain::model::AccountId;
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::Store;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::OnceCell;

/// How long stopping waits for tasks still writing files.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

/// Where secrets go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretBackend {
    /// The Secret Service, or memory for the session when none answers (ADR-008).
    Keyring,
    /// Memory only: self-tests and headless runs, which never touch the user's keyring.
    Memory,
}

pub struct EngineConfig {
    /// The app-id, which scopes the keyring's items.
    pub application: String,
    /// Shared with the app, which reads it; the engine writes what it syncs.
    pub store: Arc<Store>,
    pub blobs: BlobStore,
    pub paths: AccountPaths,
    pub trust: Trust,
    pub secrets: SecretBackend,
    pub detect: DetectOptions,
}

/// The running engine: its command and event channels and its thread.
pub struct Engine {
    commands: async_channel::Sender<Command>,
    events: async_channel::Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(config: EngineConfig) -> std::io::Result<Engine> {
        let (commands, received) = async_channel::unbounded();
        let (sender, events) = async_channel::unbounded();
        let runtime = crate::runtime()?;
        let thread = std::thread::Builder::new()
            .name("sealwax-service".into())
            .spawn(move || {
                runtime.block_on(run(Arc::new(Shared::new(config, sender)), received));
                runtime.shutdown_timeout(SHUTDOWN_WAIT);
            })?;
        Ok(Engine {
            commands,
            events,
            thread: Some(thread),
        })
    }

    pub fn commands(&self) -> async_channel::Sender<Command> {
        self.commands.clone()
    }

    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.events.clone()
    }

    /// Stops every task and connection and waits for the engine's thread.
    pub fn stop(mut self) {
        let _ = self.commands.try_send(Command::Shutdown);
        self.commands.close();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the engine thread panicked");
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.commands.close();
    }
}

struct Shared {
    application: String,
    store: Arc<Store>,
    blobs: BlobStore,
    paths: AccountPaths,
    trust: Trust,
    backend: SecretBackend,
    secrets: OnceCell<Secrets>,
    detect: DetectOptions,
    events: async_channel::Sender<Event>,
    /// Each account's running sync.
    syncs: Mutex<HashMap<AccountKey, tokio::task::JoinHandle<()>>>,
}

impl Shared {
    fn new(config: EngineConfig, events: async_channel::Sender<Event>) -> Self {
        Self {
            application: config.application,
            store: config.store,
            blobs: config.blobs,
            paths: config.paths,
            trust: config.trust,
            backend: config.secrets,
            secrets: OnceCell::new(),
            detect: config.detect,
            events,
            syncs: Mutex::default(),
        }
    }

    /// The keyring, opened on first use so it never delays a lookup or the first frame.
    async fn secrets(&self) -> &Secrets {
        self.secrets
            .get_or_init(|| async {
                match self.backend {
                    SecretBackend::Keyring => Secrets::open(&self.application).await,
                    SecretBackend::Memory => Secrets::Memory(MemoryStore::default()),
                }
            })
            .await
    }

    /// Fails only when the app has stopped listening.
    fn send(&self, event: Event) {
        let _ = self.events.try_send(event);
    }

    fn syncs(&self) -> std::sync::MutexGuard<'_, HashMap<AccountKey, tokio::task::JoinHandle<()>>> {
        self.syncs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The account lane's work.
enum Job {
    Accounts(Vec<AccountSettings>),
    Add {
        request: RequestId,
        settings: AccountSettings,
        password: Password,
    },
    Remove {
        request: RequestId,
        key: AccountKey,
    },
}

async fn run(shared: Arc<Shared>, commands: async_channel::Receiver<Command>) {
    let (jobs, lane_jobs) = async_channel::unbounded();
    let lane_task = tokio::spawn(lane(shared.clone(), lane_jobs));
    while let Ok(command) = commands.recv().await {
        let job = match command {
            Command::Shutdown => break,
            Command::Detect { request, address } => {
                tokio::spawn(detect(shared.clone(), request, address));
                continue;
            }
            Command::Verify {
                request,
                settings,
                second_username,
                password,
            } => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let result = verify(
                        &settings,
                        second_username.as_deref(),
                        &password,
                        &shared.trust,
                    )
                    .await;
                    shared.send(Event::Verified { request, result });
                });
                continue;
            }
            Command::Accounts { accounts } => Job::Accounts(accounts),
            Command::AddAccount {
                request,
                settings,
                password,
                sign_in: _,
            } => Job::Add {
                request,
                settings,
                password,
            },
            // ML-032 builds the browser sign-in; until then it answers as unavailable.
            Command::SignIn { request, .. } => {
                shared.send(Event::SignedIn {
                    request,
                    result: Err(LoginError {
                        protocol: Protocol::Imap,
                        failure: LoginFailure::OAuthRequired,
                    }),
                });
                continue;
            }
            Command::CancelSignIn { .. } => continue,
            Command::RemoveAccount { request, key } => Job::Remove { request, key },
        };
        let _ = jobs.send(job).await;
    }
    jobs.close();
    lane_task.abort();
    for (_, sync) in shared.syncs().drain() {
        sync.abort();
    }
}

async fn detect(shared: Arc<Shared>, request: RequestId, address: String) {
    let events = shared.events.clone();
    let detection = autoconfig::detect(&address, &shared.detect, |source| {
        let _ = events.try_send(Event::DetectProgress { request, source });
    })
    .await;
    shared.send(Event::Detected { request, detection });
}

/// Runs account jobs one at a time, after finishing what a crash interrupted.
async fn lane(shared: Arc<Shared>, jobs: async_channel::Receiver<Job>) {
    let accounts = Accounts {
        paths: &shared.paths,
        store: &shared.store,
        blobs: Some(&shared.blobs),
        secrets: shared.secrets().await,
    };
    if let Err(error) = accounts.recover().await {
        tracing::warn!(%error, "an interrupted account operation is still pending");
    }
    let mut listed: Vec<AccountKey> = Vec::new();
    while let Ok(job) = jobs.recv().await {
        match job {
            Job::Accounts(list) => {
                if let Err(error) = accounts.reconcile(&list).await {
                    tracing::warn!(%error, "the store doesn't follow accounts.toml yet");
                    continue;
                }
                for key in listed
                    .iter()
                    .filter(|key| list.iter().all(|account| &account.key != *key))
                {
                    stop_sync(&shared, key).await;
                }
                listed = list.iter().map(|account| account.key.clone()).collect();
                for settings in list {
                    start_sync(&shared, settings);
                }
            }
            Job::Add {
                request,
                mut settings,
                password,
            } => match accounts.add(settings.clone(), &password).await {
                Ok((key, id)) => {
                    shared.send(Event::AccountAdded {
                        request,
                        result: Ok(id),
                    });
                    settings.key = key;
                    start_sync(&shared, settings);
                }
                Err(error) => shared.send(Event::AccountAdded {
                    request,
                    result: Err(error),
                }),
            },
            Job::Remove { request, key } => {
                stop_sync(&shared, &key).await;
                let result = accounts.remove(&key).await;
                shared.send(Event::AccountRemoved { request, result });
            }
        }
    }
}

/// Starts the account's folder list and first INBOX sync unless one is running.
fn start_sync(shared: &Arc<Shared>, settings: AccountSettings) {
    let mut syncs = shared.syncs();
    if syncs
        .get(&settings.key)
        .is_some_and(|sync| !sync.is_finished())
    {
        return;
    }
    let key = settings.key.clone();
    syncs.insert(key, tokio::spawn(sync_account(shared.clone(), settings)));
}

/// Stops the account's sync and waits until it has.
async fn stop_sync(shared: &Shared, key: &AccountKey) {
    let sync = shared.syncs().remove(key);
    if let Some(sync) = sync {
        sync.abort();
        let _ = sync.await;
    }
}

async fn sync_account(shared: Arc<Shared>, settings: AccountSettings) {
    let account = match shared
        .store
        .account_by_key(settings.key.clone())
        .recv()
        .await
    {
        Ok(found) => match found.value {
            Some(account) => account.id,
            None => return,
        },
        Err(error) => {
            tracing::warn!(%error, "the account's store row couldn't be read");
            return;
        }
    };
    match sync_once(&shared, &settings, account).await {
        Ok(()) => shared.send(Event::SyncFinished { account }),
        Err(failure) => {
            tracing::info!(account = account.0, ?failure, "the account's sync stopped");
            shared.send(Event::SyncFailed { account, failure });
        }
    }
}

/// Logs in, stores the folder list, then the INBOX's headers newest first.
async fn sync_once(
    shared: &Shared,
    settings: &AccountSettings,
    account: AccountId,
) -> Result<(), LoginFailure> {
    if let AuthMethod::OAuth(_) = settings.auth {
        // Microsoft sign-in waits for S5 (ML-032).
        return Err(LoginFailure::OAuthRequired);
    }
    let password = shared
        .secrets()
        .await
        .read(&settings.key.0, SecretKind::Password)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "the account's password couldn't be read");
            LoginFailure::Other
        })?
        .ok_or(LoginFailure::WrongPassword)?;
    let password = std::str::from_utf8(&password).map_err(|_| LoginFailure::WrongPassword)?;
    let tls = shared
        .trust
        .client_config()
        .map_err(|_| LoginFailure::Certificate)?;
    let credentials = Credentials::Password {
        username: &settings.username,
        password,
    };
    let mut connection = imap::connect(&settings.imap, credentials, tls)
        .await
        .map_err(|error| error.failure())?;
    let synced = sync_folders_and_inbox(shared, &mut connection, account).await;
    connection.logout().await;
    synced.map_err(|error| error.failure())
}

async fn sync_folders_and_inbox(
    shared: &Shared,
    connection: &mut imap::Connection,
    account: AccountId,
) -> Result<(), SyncError> {
    let folders = imap::sync_folders(connection, &shared.store, account).await?;
    shared.send(Event::FoldersSynced {
        account,
        folders: folders.folders.len() as u32,
    });
    let Some(inbox) = folders.inbox else {
        return Ok(());
    };
    let report = imap::first_sync(connection, &shared.store, inbox, |progress| {
        shared.send(Event::SyncProgress {
            account,
            folder: inbox,
            fetched: progress.fetched,
            total: progress.total,
        });
    })
    .await?;
    if !report.orphaned_blobs.is_empty() {
        let blobs = shared.blobs.clone();
        let orphaned = report.orphaned_blobs;
        let _ = tokio::task::spawn_blocking(move || {
            for key in &orphaned {
                if let Err(error) = blobs.remove(key) {
                    tracing::warn!(%error, "a raw message the reset left couldn't be deleted");
                }
            }
        })
        .await;
    }
    Ok(())
}
