//! The engine service (ADR-003): a thread of its own runs the engine's tokio runtime, takes the
//! app's commands and sends events back. M2: autodetect, the login check, Microsoft sign-in
//! (ML-032), adding and removing accounts. M3: one sync actor per account (`actor`), running
//! side by side, with body downloads (`bodies`) beside it.
//!
//! Autodetect, login checks and sign-ins run side by side. Everything that touches
//! `accounts.toml`, the keyring entries or an account's store rows runs in one lane, one job at
//! a time, so an add never races a removal or a reconcile; a re-sign-in's refresh token and a
//! new password (ML-048), each checked on the servers beside the lane, are stored there too.
//! The only other keyring write is an account's sync storing a rotated refresh token, and the
//! lane stops that actor before it removes the account or replaces its token.

use crate::accounts::{Accounts, secret_label};
use crate::api::{
    AccountError, Command, Event, FolderChange, FolderError, Password, RequestId, SendError,
    UncertainReason,
};
use crate::autoconfig::{self, DetectOptions};
use crate::imap::{self, Credentials, ImapError, Stop};
use crate::net::{self, Trust};
use crate::oauth::{self, AccessToken, AccessTokens, Identity};
use crate::secrets::{MemoryStore, Secret, SecretKind, SecretStore, Secrets};
use crate::smtp;
use crate::verify::verify;
use rustls::ClientConfig;
use sealwax_domain::account::{
    AccountColor, AccountKey, AccountSettings, AuthMethod, Endpoint, LoginError, LoginFailure,
    OAuthProvider, Protocol, Receive,
};
use sealwax_domain::autoconfig::{address_domain, presets};
use sealwax_domain::model::{AccountId, FolderId};
use sealwax_infrastructure::accounts::{self as files, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::Store;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

mod actor;
mod bodies;
mod rules;

pub use actor::{SyncConfig, backoff_delay};

/// How long stopping waits for tasks still writing files.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);
/// How long reading a server's certificate may take (`Command::ReadCertificate`).
const CERTIFICATE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a sign-in waits for the browser.
pub const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
/// How long a finished sign-in's tokens wait for the `AddAccount` that names it.
pub const SIGN_IN_KEPT: Duration = Duration::from_secs(30 * 60);

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
    /// The Microsoft client ID (ADR-008: the build's, or `config.toml`'s). Without one, a
    /// sign-in answers that it's unavailable and Microsoft accounts can't sync.
    pub microsoft_client_id: Option<String>,
    pub sign_in: SignInOptions,
    /// The sync actors' schedule and patience.
    pub sync: SyncConfig,
}

/// Where sign-ins go and how long they wait; tests point them at local servers.
#[derive(Clone, Debug)]
pub struct SignInOptions {
    /// Microsoft's authorize and token endpoints.
    pub microsoft: oauth::Provider,
    /// The IMAP and SMTP servers a new account's sign-in is checked on; `None` takes Outlook.com's
    /// or Microsoft 365's from the presets.
    pub servers: Option<(Endpoint, Endpoint)>,
    /// How long to wait for the browser.
    pub wait: Duration,
}

impl Default for SignInOptions {
    fn default() -> Self {
        Self {
            microsoft: oauth::Provider::microsoft(),
            servers: None,
            wait: SIGN_IN_WAIT,
        }
    }
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

/// A sign-in the app started.
enum SignIn {
    /// Waiting for the browser, or checking IMAP and SMTP.
    Running(tokio::task::AbortHandle),
    /// Finished: its tokens wait for the `AddAccount` that names it.
    Done(Box<Kept>),
}

/// A finished sign-in's tokens, in memory only.
struct Kept {
    provider: OAuthProvider,
    /// The address the sign-in was made for.
    address: String,
    /// The identity the token belongs to: XOAUTH2's `user=`.
    username: String,
    refresh: Zeroizing<String>,
    access: AccessToken,
    until: Instant,
}

impl Kept {
    /// An add of `address` may use it: the address it was made for, or the one it belongs to
    /// (the user chose to send from the address they signed in with; issue #2).
    fn fits(&self, address: &str) -> bool {
        [&self.address, &self.username]
            .iter()
            .any(|own| own.trim().eq_ignore_ascii_case(address.trim()))
    }
}

struct MicrosoftClient {
    id: Option<String>,
    client: Option<Arc<oauth::Client>>,
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
    sign_in: SignInOptions,
    /// The client ID in use and its client, built on first use (so the engine's start makes no
    /// TLS setup it may not need) and again when `config.toml` changes the ID.
    oauth: Mutex<MicrosoftClient>,
    sign_ins: Mutex<HashMap<RequestId, SignIn>>,
    access: AccessTokens,
    events: async_channel::Sender<Event>,
    /// Each account's sync actor.
    actors: Mutex<HashMap<AccountKey, actor::Handle>>,
    sync: SyncConfig,
    /// Body downloads and the disk cap (ML-044).
    bodies: bodies::Bodies,
    /// Each account's submission size limit, once read (`SubmissionLimit`).
    limits: Mutex<HashMap<AccountId, Option<u64>>>,
    /// Each account's rules as last read (ADR-020).
    rules: rules::Cache,
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
            sign_in: config.sign_in,
            oauth: Mutex::new(MicrosoftClient {
                id: config.microsoft_client_id,
                client: None,
            }),
            sign_ins: Mutex::default(),
            access: AccessTokens::default(),
            events,
            actors: Mutex::default(),
            sync: config.sync,
            bodies: bodies::Bodies::default(),
            limits: Mutex::default(),
            rules: rules::Cache::default(),
        }
    }

    /// The keyring, opened on first use so it never delays a lookup or the first frame. When
    /// no Secret Service answers, secrets stay in memory and the app hears it once (ML-048).
    async fn secrets(&self) -> &Secrets {
        self.secrets
            .get_or_init(|| async {
                match self.backend {
                    SecretBackend::Keyring => {
                        let secrets = Secrets::open(&self.application).await;
                        if !secrets.is_persistent() {
                            self.send(Event::KeyringUnavailable);
                        }
                        secrets
                    }
                    SecretBackend::Memory => Secrets::Memory(MemoryStore::default()),
                }
            })
            .await
    }

    /// The Microsoft client, when a client ID is configured.
    fn oauth(&self) -> Option<Arc<oauth::Client>> {
        let mut current = self.oauth.lock().unwrap_or_else(PoisonError::into_inner);
        if current.client.is_none() {
            let id = current
                .id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())?;
            current.client = oauth::Client::new(id.to_owned(), self.sign_in.microsoft.clone())
                .inspect_err(|error| tracing::warn!(%error, "Microsoft sign-in is unavailable"))
                .ok()
                .map(Arc::new);
        }
        current.client.clone()
    }

    /// A new client ID from `config.toml`; the next sign-in or refresh builds its client.
    fn set_microsoft_client_id(&self, id: Option<String>) {
        let mut current = self.oauth.lock().unwrap_or_else(PoisonError::into_inner);
        if current.id != id {
            *current = MicrosoftClient { id, client: None };
        }
    }

    /// Fails only when the app has stopped listening.
    fn send(&self, event: Event) {
        let _ = self.events.try_send(event);
    }

    fn actors(&self) -> MutexGuard<'_, HashMap<AccountKey, actor::Handle>> {
        self.actors.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The settings of the account with store id `account`, from its running actor.
    fn actor_settings(&self, account: AccountId) -> Option<AccountSettings> {
        self.actors()
            .values()
            .find(|actor| actor.account() == Some(account))
            .map(|actor| actor.settings.clone())
    }

    /// The reconnect signal of the account's actor, which body downloads follow too.
    fn actor_signal(
        &self,
        account: AccountId,
    ) -> Option<tokio::sync::watch::Receiver<actor::Signal>> {
        self.actors()
            .values()
            .find(|actor| actor.account() == Some(account))
            .map(|actor| actor.signal.subscribe())
    }

    fn sign_ins(&self) -> MutexGuard<'_, HashMap<RequestId, SignIn>> {
        self.sign_ins.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The finished sign-in `request` for an add of `address`. One that is still running or was
    /// made for another address stays; an expired one is dropped.
    fn take_sign_in(&self, request: RequestId, address: &str) -> Option<Box<Kept>> {
        let mut sign_ins = self.sign_ins();
        match sign_ins.remove(&request)? {
            SignIn::Done(kept) if kept.until <= Instant::now() => None,
            SignIn::Done(kept) if kept.fits(address) => Some(kept),
            other => {
                sign_ins.insert(request, other);
                None
            }
        }
    }

    /// Gives a failed add's sign-in back, so the app can try the add again.
    fn restore_sign_in(&self, request: RequestId, kept: Box<Kept>) {
        if kept.until > Instant::now() {
            self.sign_ins().entry(request).or_insert(SignIn::Done(kept));
        }
    }
}

/// The account lane's work.
enum Job {
    Accounts(Vec<AccountSettings>),
    Add {
        request: RequestId,
        settings: AccountSettings,
        password: Password,
        /// An OAuth account's sign-in, whose refresh token is its secret.
        sign_in: Option<(RequestId, Box<Kept>)>,
    },
    Remove {
        request: RequestId,
        key: AccountKey,
    },
    Edit {
        request: RequestId,
        key: AccountKey,
        label: Option<String>,
        color: Option<AccountColor>,
    },
    /// A re-sign-in's tokens for the account in `settings`.
    SignedInAgain {
        request: RequestId,
        settings: AccountSettings,
        kept: Box<Kept>,
    },
    /// A new password that IMAP and SMTP took, for the account in `settings`.
    PasswordChecked {
        request: RequestId,
        settings: AccountSettings,
        password: Password,
    },
}

/// Sends a stop left mid-way become uncertain (their data may have reached the server), and
/// those whose message was being built go back to their drafts as failed (the build may be what
/// stopped Sealwax), before any actor starts and asks for due sends; the app hears each.
async fn recover_sends(shared: &Shared) {
    match shared.store.recover_sends().recv().await {
        Ok(changes) => {
            for change in changes {
                let (account, draft) = (change.account, change.draft);
                shared.send(match change.failure {
                    Some(_) => Event::SendFailed {
                        account,
                        draft,
                        error: SendError::Interrupted,
                    },
                    None => Event::SendUncertain {
                        account,
                        draft,
                        reason: UncertainReason::Stopped,
                    },
                });
            }
        }
        Err(error) => tracing::warn!(%error, "interrupted sends couldn't be recovered"),
    }
}

async fn run(shared: Arc<Shared>, commands: async_channel::Receiver<Command>) {
    recover_sends(&shared).await;
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
            Command::ReadCertificate {
                request,
                endpoint,
                protocol,
            } => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let result =
                        net::read_certificate(&endpoint, protocol, CERTIFICATE_TIMEOUT).await;
                    if let Err(failure) = result {
                        tracing::info!(?failure, "a certificate couldn't be read");
                    }
                    shared.send(Event::CertificateRead { request, result });
                });
                continue;
            }
            Command::Accounts { accounts } => Job::Accounts(accounts),
            Command::AddAccount {
                request,
                settings,
                password,
                sign_in,
            } => match add_job(&shared, request, settings, password, sign_in) {
                Some(job) => job,
                None => continue,
            },
            Command::SignIn {
                request,
                provider,
                address,
                account,
            } => {
                start_sign_in(&shared, &jobs, request, provider, address, account);
                continue;
            }
            Command::CancelSignIn { request } => {
                cancel_sign_in(&shared, request);
                continue;
            }
            Command::MicrosoftClient { client_id } => {
                shared.set_microsoft_client_id(client_id);
                continue;
            }
            Command::CheckForUpdates { request } => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let result =
                        crate::updates::latest(&crate::updates::latest_release_url()).await;
                    match &result {
                        Ok(version) => tracing::info!(%version, "the latest release was read"),
                        Err(error) => tracing::info!(%error, "the update check failed"),
                    }
                    shared.send(Event::UpdateChecked { request, result });
                });
                continue;
            }
            Command::RemoveAccount { request, key } => Job::Remove { request, key },
            Command::EditAccount {
                request,
                key,
                label,
                color,
            } => Job::Edit {
                request,
                key,
                label,
                color,
            },
            Command::UpdatePassword {
                request,
                key,
                password,
            } => {
                tokio::spawn(check_password(
                    shared.clone(),
                    jobs.clone(),
                    request,
                    key,
                    password,
                ));
                continue;
            }
            Command::FetchBodies { messages } => {
                bodies::request(&shared, messages, true);
                continue;
            }
            Command::ApplyRules {
                request,
                folder,
                rule,
                undo_window,
            } => {
                tokio::spawn(rules::apply(
                    shared.clone(),
                    request,
                    folder,
                    rule,
                    undo_window,
                ));
                continue;
            }
            Command::FetchPart { message, part } => {
                bodies::request_part(&shared, message, part);
                continue;
            }
            Command::FetchOriginal { message } => {
                bodies::request_original(&shared, message);
                continue;
            }
            Command::Storage { storage } => {
                bodies::set_storage(&shared, storage);
                continue;
            }
            Command::FolderOpened { folder } => {
                tokio::spawn(opened(shared.clone(), folder));
                continue;
            }
            Command::LoadOlder { folder } => {
                tokio::spawn(older(shared.clone(), folder));
                continue;
            }
            // Without a running actor the ops, sends and drafts wait in the store for its start.
            Command::OpsQueued { account } => {
                wake(&shared, account, actor::Control::Ops);
                continue;
            }
            Command::OutboxQueued { account } => {
                wake(&shared, account, actor::Control::Outbox);
                continue;
            }
            Command::DraftsChanged { account } => {
                wake(&shared, account, actor::Control::Drafts);
                continue;
            }
            Command::SubmissionLimit { request, account } => {
                tokio::spawn(submission_limit(shared.clone(), request, account));
                continue;
            }
            Command::ChangeFolder {
                request,
                account,
                change,
            } => {
                change_folder(&shared, request, account, change);
                continue;
            }
            Command::Reconnect => {
                tracing::info!("reconnecting every account");
                for actor in shared.actors().values() {
                    actor.reconnect();
                }
                continue;
            }
            Command::CheckMail => {
                tracing::info!("checking every account for new mail");
                for actor in shared.actors().values() {
                    let _ = actor.control.try_send(actor::Control::CheckMail);
                }
                continue;
            }
        };
        let _ = jobs.send(job).await;
    }
    jobs.close();
    lane_task.abort();
    for (_, sign_in) in shared.sign_ins().drain() {
        if let SignIn::Running(task) = sign_in {
            task.abort();
        }
    }
    shared.bodies.stop();
    let actors: Vec<actor::Handle> = shared.actors().drain().map(|(_, actor)| actor).collect();
    stop_actors(actors, Stop::Logout(shared.sync.logout_wait)).await;
}

/// Tells the account's actor, when one runs, that something waits in the store.
fn wake(shared: &Shared, account: AccountId, control: actor::Control) {
    if let Some(actor) = shared
        .actors()
        .values()
        .find(|actor| actor.account() == Some(account))
    {
        let _ = actor.control.try_send(control);
    }
}

/// `ChangeFolder`: the account's actor changes an IMAP account's folders on its command
/// connection, a POP3 account's at once; an account without a running actor is offline.
fn change_folder(shared: &Shared, request: RequestId, account: AccountId, change: FolderChange) {
    let job = actor::Control::Folder(actor::FolderJob { request, change });
    let sent = match shared
        .actors()
        .values()
        .find(|actor| actor.account() == Some(account))
    {
        Some(actor) => actor.control.try_send(job).is_ok(),
        None => false,
    };
    if !sent {
        shared.send(Event::FolderChanged {
            request,
            account,
            result: Err(FolderError::Offline),
        });
    }
}

/// `SubmissionLimit`: EHLO's SIZE from the account's submission server, without a login, once
/// per session (a limit read is kept until the engine stops).
async fn submission_limit(shared: Arc<Shared>, request: RequestId, account: AccountId) {
    let cached = shared
        .limits
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&account)
        .copied();
    let result = match (cached, shared.actor_settings(account)) {
        (Some(limit), _) => Ok(limit),
        (None, None) => Err(SendError::Other),
        (None, Some(settings)) => {
            let read = smtp::submit::submission_limit(
                &settings.smtp,
                &shared.trust,
                shared.sync.submission,
            )
            .await;
            if let Ok(limit) = read {
                shared
                    .limits
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(account, limit);
            }
            read.map_err(|error| error.send_error(&[]))
        }
    };
    shared.send(Event::SubmissionLimit {
        request,
        account,
        result,
    });
}

/// Stops actors together: each logs out within `stop`'s wait (and its IDLE connection with it);
/// whatever is still running a moment after is aborted.
async fn stop_actors(actors: Vec<actor::Handle>, stop: Stop) {
    let wait = match stop {
        Stop::Logout(wait) => wait * 4,
        Stop::Drop => Duration::from_millis(100),
    };
    let tasks: Vec<_> = actors
        .into_iter()
        .map(|actor| {
            actor.stop(stop);
            let mut task = actor.task;
            async move {
                if tokio::time::timeout(wait, &mut task).await.is_err() {
                    task.abort();
                }
                drop(actor.signal);
            }
        })
        .collect();
    futures_util::future::join_all(tasks).await;
}

/// The account whose folder this is, when its actor runs.
async fn owner(shared: &Shared, folder: FolderId) -> Option<AccountId> {
    shared
        .store
        .folder_sync(folder)
        .recv()
        .await
        .ok()
        .flatten()
        .map(|state| state.account)
}

/// `FolderOpened`: the folder's actor syncs it now and keeps it current; the others forget
/// the folder they had open.
async fn opened(shared: Arc<Shared>, folder: FolderId) {
    let owner = owner(&shared, folder).await;
    for actor in shared.actors().values() {
        let mine = owner.is_some() && actor.account() == owner;
        let _ = actor
            .control
            .try_send(actor::Control::Opened(mine.then_some(folder)));
    }
}

/// `LoadOlder`: the folder's actor loads it; without one, nothing older can come now.
async fn older(shared: Arc<Shared>, folder: FolderId) {
    let owner = owner(&shared, folder).await;
    let sent = owner.is_some_and(|owner| {
        shared
            .actors()
            .values()
            .find(|actor| actor.account() == Some(owner))
            .is_some_and(|actor| {
                actor
                    .control
                    .try_send(actor::Control::Older(folder))
                    .is_ok()
            })
    });
    if !sent {
        let complete = shared
            .store
            .folder_sync(folder)
            .recv()
            .await
            .ok()
            .flatten()
            .is_some_and(|state| state.complete());
        shared.send(Event::OlderLoaded {
            folder,
            fetched: 0,
            complete,
        });
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

/// An add for the lane. An OAuth account takes its sign-in's refresh token as its secret and
/// the sign-in's identity as its username; without a usable sign-in it fails at once.
fn add_job(
    shared: &Shared,
    request: RequestId,
    mut settings: AccountSettings,
    password: Password,
    sign_in: Option<RequestId>,
) -> Option<Job> {
    let kept = match sign_in {
        Some(sign_in) => shared.take_sign_in(sign_in, &settings.address),
        None => None,
    };
    let sign_in = match (sign_in, kept) {
        (Some(sign_in), Some(kept)) => {
            settings.username = kept.username.clone();
            settings.auth = AuthMethod::OAuth(kept.provider);
            Some((sign_in, kept))
        }
        (None, None) if settings.auth == AuthMethod::Password => None,
        _ => {
            tracing::info!(request, "an OAuth account's add names no usable sign-in");
            shared.send(Event::AccountAdded {
                request,
                result: Err(AccountError::SignInExpired),
            });
            return None;
        }
    };
    Some(Job::Add {
        request,
        settings,
        password,
        sign_in,
    })
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
                    shared.access.forget(key);
                }
                listed = list.iter().map(|account| account.key.clone()).collect();
                for settings in list {
                    start_sync(&shared, settings).await;
                }
            }
            Job::Add {
                request,
                mut settings,
                password,
                sign_in,
            } => {
                let secret = match &sign_in {
                    Some((_, kept)) => Password::new(kept.refresh.as_str().to_owned()),
                    None => password,
                };
                match accounts.add(settings.clone(), &secret).await {
                    Ok((key, id)) => {
                        if let Some((_, kept)) = sign_in {
                            shared.access.seed(&key, kept.access);
                        }
                        shared.send(Event::AccountAdded {
                            request,
                            result: Ok(id),
                        });
                        settings.key = key;
                        start_sync(&shared, settings).await;
                    }
                    Err(error) => {
                        if let Some((sign_in, kept)) = sign_in {
                            shared.restore_sign_in(sign_in, kept);
                        }
                        shared.send(Event::AccountAdded {
                            request,
                            result: Err(error),
                        });
                    }
                }
            }
            Job::Remove { request, key } => {
                stop_sync(&shared, &key).await;
                shared.access.forget(&key);
                let result = accounts.remove(&key).await;
                shared.send(Event::AccountRemoved { request, result });
            }
            Job::Edit {
                request,
                key,
                label,
                color,
            } => {
                let result = accounts.edit(&key, label, color).await;
                shared.send(Event::AccountEdited { request, result });
            }
            Job::SignedInAgain {
                request,
                settings,
                kept,
            } => {
                // A cancelled sign-in answers nothing and changes nothing.
                if shared.sign_ins().remove(&request).is_none() {
                    continue;
                }
                stop_sync(&shared, &settings.key).await;
                let result = store_sign_in(&shared, &accounts, &settings, *kept).await;
                shared.send(Event::SignedIn { request, result });
                start_sync(&shared, settings).await;
            }
            Job::PasswordChecked {
                request,
                settings,
                password,
            } => {
                let result = store_password(&shared, &accounts, &settings, &password).await;
                let stored = result.is_ok();
                shared.send(Event::PasswordUpdated { request, result });
                if stored {
                    stop_sync(&shared, &settings.key).await;
                    start_sync(&shared, settings).await;
                }
            }
        }
    }
}

/// Starts the account's sync actor, unless one runs with the same settings; one with other
/// settings (a server or a POP3 policy changed in `accounts.toml`) is stopped first. A POP3
/// account gets a POP3 actor (M8), never an IMAP one.
async fn start_sync(shared: &Arc<Shared>, settings: AccountSettings) {
    let running = shared
        .actors()
        .get(&settings.key)
        .map(|actor| (actor.settings == settings, actor.task.is_finished()));
    match running {
        Some((true, false)) => return,
        Some(_) => stop_sync(shared, &settings.key).await,
        None => {}
    }
    let actor = match settings.receive {
        Receive::Imap => actor::Handle::start(shared, settings),
        Receive::Pop3(_) => actor::Handle::start_pop3(shared, settings),
    };
    shared.actors().insert(actor.settings.key.clone(), actor);
}

/// Stops the account's actor (dropping its connections) and waits until it has.
async fn stop_sync(shared: &Shared, key: &AccountKey) {
    let actor = shared.actors().remove(key);
    if let Some(actor) = actor {
        stop_actors(vec![actor], Stop::Drop).await;
    }
}

fn tls(shared: &Shared) -> Result<Arc<ClientConfig>, LoginFailure> {
    shared
        .trust
        .client_config()
        .map_err(|_| LoginFailure::Certificate)
}

/// The account's password from the keyring; a missing one counts as a wrong one.
async fn password(shared: &Shared, settings: &AccountSettings) -> Result<Secret, LoginFailure> {
    shared
        .secrets()
        .await
        .read(&settings.key.0, SecretKind::Password)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "the account's password couldn't be read");
            LoginFailure::Other
        })?
        .ok_or(LoginFailure::WrongPassword)
}

async fn log_in_with_password(
    shared: &Shared,
    settings: &AccountSettings,
) -> Result<imap::Connection, LoginFailure> {
    let password = password(shared, settings).await?;
    let password = std::str::from_utf8(&password).map_err(|_| LoginFailure::WrongPassword)?;
    let credentials = Credentials::Password {
        username: &settings.username,
        password,
    };
    imap::connect_with(
        &settings.incoming,
        credentials,
        tls(shared)?,
        shared.sync.connect,
    )
    .await
    .map_err(|error| error.failure())
}

/// XOAUTH2 with the account's access token, refreshed when it is about to expire and once more
/// when the server refuses it. A refused refresh, or a server refusing a fresh token, means the
/// account must sign in again (`OAuthRequired`).
async fn log_in_with_token(
    shared: &Shared,
    settings: &AccountSettings,
) -> Result<imap::Connection, LoginFailure> {
    let tls = tls(shared)?;
    let secrets = shared.secrets().await;
    let label = secret_label(&settings.address);
    let mut renew = false;
    loop {
        let fetched = shared
            .access
            .get(
                &settings.key,
                &label,
                secrets,
                shared.oauth().as_deref(),
                renew,
            )
            .await?;
        let credentials = Credentials::OAuth2 {
            username: &settings.username,
            token: &fetched.token,
        };
        match imap::connect_with(
            &settings.incoming,
            credentials,
            tls.clone(),
            shared.sync.connect,
        )
        .await
        {
            Ok(connection) => return Ok(connection),
            Err(ImapError::Rejected(_)) if !fetched.refreshed => {
                tracing::debug!("the server refused the access token; refreshing it");
                renew = true;
            }
            Err(ImapError::Rejected(LoginFailure::WrongPassword)) => {
                return Err(LoginFailure::OAuthRequired);
            }
            Err(error) => return Err(error.failure()),
        }
    }
}

fn imap_error(failure: LoginFailure) -> LoginError {
    LoginError {
        protocol: Protocol::Imap,
        failure,
    }
}

/// Starts a browser sign-in as its own task, or answers at once that sign-in is unavailable.
fn start_sign_in(
    shared: &Arc<Shared>,
    jobs: &async_channel::Sender<Job>,
    request: RequestId,
    provider: OAuthProvider,
    address: String,
    account: Option<AccountKey>,
) {
    if shared.oauth().is_none() {
        shared.send(Event::SignedIn {
            request,
            result: Err(imap_error(LoginFailure::OAuthRequired)),
        });
        return;
    }
    // Registered before the task can finish, which looks for its entry.
    let mut sign_ins = shared.sign_ins();
    let task = tokio::spawn(sign_in(
        shared.clone(),
        jobs.clone(),
        request,
        provider,
        address,
        account,
    ));
    if let Some(SignIn::Running(earlier)) =
        sign_ins.insert(request, SignIn::Running(task.abort_handle()))
    {
        earlier.abort();
    }
}

/// Stops a running sign-in, or drops a finished one's tokens. Nothing is answered.
fn cancel_sign_in(shared: &Shared, request: RequestId) {
    match shared.sign_ins().remove(&request) {
        Some(SignIn::Running(task)) => {
            task.abort();
            tracing::info!(request, "sign-in cancelled");
        }
        Some(SignIn::Done(_)) => tracing::info!(request, "a finished sign-in was dropped"),
        None => {}
    }
}

async fn sign_in(
    shared: Arc<Shared>,
    jobs: async_channel::Sender<Job>,
    request: RequestId,
    provider: OAuthProvider,
    address: String,
    account: Option<AccountKey>,
) {
    match signed_in(&shared, request, provider, &address, account.as_ref()).await {
        Ok((kept, Some(settings))) => {
            let job = Job::SignedInAgain {
                request,
                settings,
                kept,
            };
            let _ = jobs.send(job).await;
        }
        Ok((kept, None)) => {
            let username = kept.username.clone();
            let mut sign_ins = shared.sign_ins();
            if let Some(entry @ SignIn::Running(_)) = sign_ins.get_mut(&request) {
                *entry = SignIn::Done(kept);
                shared.send(Event::SignedIn {
                    request,
                    result: Ok(username),
                });
                drop(sign_ins);
                tokio::spawn(expire_sign_in(shared.clone(), request));
            }
        }
        Err(error) => {
            if shared.sign_ins().remove(&request).is_some() {
                shared.send(Event::SignedIn {
                    request,
                    result: Err(error),
                });
            }
        }
    }
}

/// Drops a finished sign-in's tokens once nothing can use them any more.
async fn expire_sign_in(shared: Arc<Shared>, request: RequestId) {
    tokio::time::sleep(SIGN_IN_KEPT).await;
    let mut sign_ins = shared.sign_ins();
    if let Some(SignIn::Done(kept)) = sign_ins.get(&request)
        && kept.until <= Instant::now()
    {
        sign_ins.remove(&request);
        tracing::info!(request, "an unused sign-in expired");
    }
}

/// The sign-in itself: the browser, the code exchange, the identity from the ID token, then
/// IMAP and SMTP with the token as that identity. A re-sign-in (`account`) uses the account's
/// servers and must be the account's own user; it returns the account's settings too.
async fn signed_in(
    shared: &Shared,
    request: RequestId,
    provider: OAuthProvider,
    address: &str,
    account: Option<&AccountKey>,
) -> Result<(Box<Kept>, Option<AccountSettings>), LoginError> {
    let OAuthProvider::Microsoft = provider;
    let client = shared
        .oauth()
        .ok_or(imap_error(LoginFailure::OAuthRequired))?;
    let target = match account {
        Some(key) => Some(listed_account(shared, key).await?),
        None => None,
    };
    let failed = |error: oauth::OAuthError| {
        tracing::info!(request, %error, "the sign-in failed");
        imap_error(error.failure())
    };
    let authorization = client.begin().await.map_err(failed)?;
    shared.send(Event::SignInOpen {
        request,
        url: authorization.url().to_owned(),
    });
    tracing::info!(request, "sign-in page ready");
    let started = Instant::now();
    let granted = client
        .finish(authorization, shared.sign_in.wait)
        .await
        .map_err(failed)?;
    tracing::info!(
        request,
        seconds = started.elapsed().as_secs_f64(),
        consumer = granted.identity.consumer(),
        imap_scope = granted
            .tokens
            .granted("https://outlook.office.com/IMAP.AccessAsUser.All"),
        smtp_scope = granted
            .tokens
            .granted("https://outlook.office.com/SMTP.Send"),
        "signed in in the browser"
    );
    let username = granted.identity.username.clone();
    let (imap, smtp) = match &target {
        Some(settings) => {
            if !settings.username.trim().eq_ignore_ascii_case(&username) {
                tracing::info!(
                    request,
                    "the sign-in belongs to another user than the account"
                );
                return Err(imap_error(LoginFailure::WrongPassword));
            }
            (settings.incoming.clone(), settings.smtp.clone())
        }
        None => match &shared.sign_in.servers {
            Some(servers) => servers.clone(),
            None => microsoft_servers(&granted.identity, address)
                .ok_or(imap_error(LoginFailure::Other))?,
        },
    };
    check_servers(shared, &imap, &smtp, &username, &granted.tokens.access).await?;
    let oauth::Granted { tokens, .. } = granted;
    let refresh = tokens.refresh.ok_or(imap_error(LoginFailure::Other))?;
    let kept = Box::new(Kept {
        provider,
        address: target
            .as_ref()
            .map_or(address, |settings| settings.address.as_str())
            .to_owned(),
        username,
        refresh,
        access: AccessToken::new(tokens.access, tokens.lifetime),
        until: Instant::now() + SIGN_IN_KEPT,
    });
    Ok((kept, target))
}

/// The accounts `accounts.toml` lists now; none when it can't be read.
async fn listed(shared: &Shared) -> Vec<AccountSettings> {
    let path = shared.paths.settings.clone();
    tokio::task::spawn_blocking(move || files::read(&path))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

/// The Microsoft account `key` from `accounts.toml`, for a re-sign-in.
async fn listed_account(shared: &Shared, key: &AccountKey) -> Result<AccountSettings, LoginError> {
    listed(shared)
        .await
        .into_iter()
        .find(|account| &account.key == key && matches!(account.auth, AuthMethod::OAuth(_)))
        .ok_or_else(|| {
            tracing::info!("no such Microsoft account to sign in again");
            imap_error(LoginFailure::Other)
        })
}

/// Outlook.com's servers for a personal Microsoft account or an Outlook.com address, Microsoft
/// 365's otherwise, from the presets.
fn microsoft_servers(identity: &Identity, address: &str) -> Option<(Endpoint, Endpoint)> {
    let outlook = identity.consumer()
        || address_domain(address)
            .and_then(|domain| presets::by_domain(&domain))
            .is_some_and(|preset| preset.id == "outlook");
    let id = if outlook { "outlook" } else { "microsoft365" };
    let preset = presets::PRESETS.iter().find(|preset| preset.id == id)?;
    let presets::Kind::Servers { imap, smtp, .. } = &preset.kind else {
        return None;
    };
    let endpoint = |found: &presets::Endpoint| Endpoint {
        host: found.host.to_owned(),
        port: found.port,
        security: found.security,
        certificate: None,
    };
    Some((endpoint(imap.first()?), endpoint(smtp.first()?)))
}

/// IMAP, then SMTP, with XOAUTH2 as `username`; nothing is read or sent.
async fn check_servers(
    shared: &Shared,
    imap: &Endpoint,
    smtp: &Endpoint,
    username: &str,
    token: &str,
) -> Result<(), LoginError> {
    let credentials = Credentials::OAuth2 { username, token };
    let tls = tls(shared).map_err(imap_error)?;
    let connection = imap::connect(imap, credentials, tls)
        .await
        .map_err(|error| imap_error(error.failure()))?;
    connection.logout().await;
    smtp::check_login(smtp, credentials, &shared.trust)
        .await
        .map_err(|failure| LoginError {
            protocol: Protocol::Smtp,
            failure,
        })
}

/// A re-sign-in's refresh token replaces the account's, and its access token is kept.
async fn store_sign_in(
    shared: &Shared,
    accounts: &Accounts<'_, Secrets>,
    settings: &AccountSettings,
    kept: Kept,
) -> Result<String, LoginError> {
    if !listed(shared)
        .await
        .iter()
        .any(|account| account.key == settings.key)
    {
        tracing::info!("the account was removed during its sign-in");
        return Err(imap_error(LoginFailure::Other));
    }
    accounts
        .secrets
        .store(
            &settings.key.0,
            SecretKind::RefreshToken,
            &secret_label(&settings.address),
            kept.refresh.as_bytes(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "the keyring refused the new refresh token");
            imap_error(LoginFailure::Other)
        })?;
    shared.access.seed(&settings.key, kept.access);
    tracing::info!("signed in again; the new refresh token replaced the old one");
    Ok(kept.username)
}

/// `UpdatePassword`'s check, beside the lane (ML-048): the account from `accounts.toml`, then
/// IMAP and SMTP with the new password as the account's username. A password both took goes
/// to the lane to be stored; anything else is answered here, and nothing is stored.
async fn check_password(
    shared: Arc<Shared>,
    jobs: async_channel::Sender<Job>,
    request: RequestId,
    key: AccountKey,
    password: Password,
) {
    let checked = match listed(&shared)
        .await
        .into_iter()
        .find(|account| account.key == key)
    {
        Some(settings) => verify(&settings, None, &password, &shared.trust)
            .await
            .map(|_| settings),
        None => {
            tracing::info!(request, "no such account for a new password");
            Err(imap_error(LoginFailure::Other))
        }
    };
    match checked {
        Ok(settings) => {
            let job = Job::PasswordChecked {
                request,
                settings,
                password,
            };
            let _ = jobs.send(job).await;
        }
        Err(error) => {
            tracing::info!(request, failure = ?error.failure, "the new password was refused");
            shared.send(Event::PasswordUpdated {
                request,
                result: Err(error),
            });
        }
    }
}

/// A checked password replaces the account's in the keyring, unless the account was removed
/// while it was checked.
async fn store_password(
    shared: &Shared,
    accounts: &Accounts<'_, Secrets>,
    settings: &AccountSettings,
    password: &Password,
) -> Result<(), LoginError> {
    if !listed(shared)
        .await
        .iter()
        .any(|account| account.key == settings.key)
    {
        tracing::info!("the account was removed while its new password was checked");
        return Err(imap_error(LoginFailure::Other));
    }
    accounts
        .secrets
        .store(
            &settings.key.0,
            SecretKind::Password,
            &secret_label(&settings.address),
            password.expose().as_bytes(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "the keyring refused the new password");
            imap_error(LoginFailure::Other)
        })?;
    tracing::info!("the new password replaced the old one");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::CONSUMER_TENANT;
    use sealwax_domain::account::Security;

    fn identity(tenant: &str) -> Identity {
        Identity {
            username: "signin@example.test".into(),
            tenant: tenant.into(),
        }
    }

    #[test]
    fn a_finished_sign_in_fits_the_address_typed_or_the_one_signed_in_with() {
        let kept = Kept {
            provider: OAuthProvider::Microsoft,
            address: "ann@example.test".into(),
            username: "anna.berg@example.test".into(),
            refresh: Zeroizing::new(String::new()),
            access: AccessToken::new(Zeroizing::new(String::new()), Duration::from_secs(60)),
            until: Instant::now(),
        };
        assert!(kept.fits("ann@example.test"));
        assert!(kept.fits(" Anna.Berg@Example.test "));
        assert!(!kept.fits("someone@example.test"));
    }

    #[test]
    fn a_sign_in_is_checked_on_outlook_com_or_microsoft_365() {
        let work = "0b5e8d2a-1c3f-4e6a-9d7b-2f4a6c8e0a1b";
        let smtp = |identity: &Identity, address: &str| {
            let (imap, smtp) = microsoft_servers(identity, address).unwrap();
            assert_eq!(
                (imap.host.as_str(), imap.port, imap.security),
                ("outlook.office365.com", 993, Security::Tls)
            );
            assert_eq!((smtp.port, smtp.security), (587, Security::StartTls));
            smtp.host
        };
        let consumer = identity(CONSUMER_TENANT);
        assert_eq!(
            smtp(&consumer, "someone@example.test"),
            "smtp-mail.outlook.com"
        );
        assert_eq!(
            smtp(&identity(work), "someone@hotmail.com"),
            "smtp-mail.outlook.com"
        );
        assert_eq!(
            smtp(&identity(work), "someone@example.test"),
            "smtp.office365.com"
        );
    }
}
