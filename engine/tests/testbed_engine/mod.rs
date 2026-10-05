//! The engine service on the Docker testbed, for the actor suites and the soak (M3 phase B): a
//! scratch store and blob store under `target/`, secrets in memory, only the testbed CA trusted.
//! Test users only; every address is at `example.test`.

#![allow(dead_code)]

use crate::common::{self, PASSWORD};
use crate::snapshot::{self, Folder};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Security};
use sealwax_domain::model::{AccountId, FolderId};
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct TestEngine {
    _dir: tempfile::TempDir,
    engine: Option<Engine>,
    pub store: Arc<Store>,
    pub events: async_channel::Receiver<Event>,
    /// Events read while looking for others, oldest first.
    pub seen: Vec<Event>,
}

impl TestEngine {
    pub fn start(sync: SyncConfig) -> TestEngine {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("actor-testbed-")
            .tempdir_in(base)
            .unwrap();
        let dirs = SealwaxDirs::under(dir.path());
        let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
        let store = Arc::new(
            Store::open(
                &dirs.store_file(),
                StoreOptions {
                    blobs: Some(blobs.clone()),
                    ..StoreOptions::default()
                },
            )
            .unwrap(),
        );
        let engine = Engine::start(EngineConfig {
            application: "io.github.pixdevsapps.Sealwax.Test".into(),
            store: store.clone(),
            blobs,
            paths: AccountPaths::new(&dirs),
            trust: common::trust(),
            secrets: SecretBackend::Memory,
            detect: DetectOptions::default(),
            microsoft_client_id: None,
            sign_in: SignInOptions::default(),
            sync,
        })
        .unwrap();
        let events = engine.events();
        TestEngine {
            _dir: dir,
            engine: Some(engine),
            store,
            events,
            seen: Vec::new(),
        }
    }

    pub fn send(&self, command: Command) {
        self.engine
            .as_ref()
            .unwrap()
            .commands()
            .try_send(command)
            .unwrap();
    }

    /// A clone of the command sender, for tasks that act as the user.
    pub fn commands(&self) -> async_channel::Sender<Command> {
        self.engine.as_ref().unwrap().commands()
    }

    /// The next event `pick` accepts within `within`; every event read is kept in `seen`.
    pub async fn event<T>(
        &mut self,
        within: Duration,
        mut pick: impl FnMut(&Event) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = tokio::time::timeout(left, self.events.recv())
                .await
                .ok()?
                .expect("the engine stopped");
            let found = pick(&event);
            self.seen.push(event);
            if found.is_some() {
                return found;
            }
        }
    }

    /// Moves every event waiting into `seen`.
    pub fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
    }

    /// The account's next status other than syncing.
    pub async fn status(&mut self, account: AccountId, within: Duration) -> Option<SyncStatus> {
        self.event(within, |event| match event {
            Event::AccountStatus {
                account: id,
                status,
            } if *id == account && *status != SyncStatus::Syncing => Some(*status),
            _ => None,
        })
        .await
    }

    /// Adds the testbed user `user` on the TLS port `port` (a server's, or a proxy's) and
    /// waits until it is saved; its sync starts then.
    pub async fn add(&mut self, request: u64, user: &str, port: u16) -> AccountId {
        let settings = AccountSettings {
            key: AccountKey(String::new()),
            name: user.into(),
            address: format!("{user}@example.test"),
            imap: common::endpoint(port, Security::Tls),
            smtp: common::submission(Security::Tls),
            username: user.into(),
            auth: AuthMethod::Password,
        };
        self.send(Command::AddAccount {
            request,
            settings,
            password: Password::new(PASSWORD.into()),
            sign_in: None,
        });
        self.event(Duration::from_secs(30), |event| match event {
            Event::AccountAdded {
                request: id,
                result,
            } if *id == request => Some(result.clone().expect("the account wasn't added")),
            _ => None,
        })
        .await
        .expect("no AccountAdded")
    }

    pub async fn folder(&self, account: AccountId, name_raw: &str) -> Option<FolderId> {
        self.store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|state| state.name_raw.eq_ignore_ascii_case(name_raw))
            .map(|state| state.folder)
    }

    /// Messages the store holds in `folder`.
    pub async fn count(&self, folder: FolderId) -> u32 {
        self.store
            .folder_sync(folder)
            .recv()
            .await
            .unwrap()
            .map_or(0, |sync| sync.messages)
    }

    /// Stops the engine and returns how long that took.
    pub fn stop(&mut self) -> Duration {
        let engine = self.engine.take().unwrap();
        let started = Instant::now();
        tokio::task::block_in_place(|| engine.stop());
        started.elapsed()
    }
}

impl Drop for TestEngine {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
    }
}

/// Polls the cache of `account` until it matches `server` and every folder in `required` is
/// synced, for at most `within`; returns how long that took, or the differences left.
pub async fn converge(
    store: &Store,
    account: AccountId,
    server: &BTreeMap<String, Folder>,
    required: &[String],
    within: Duration,
) -> Result<Duration, Vec<String>> {
    let started = Instant::now();
    loop {
        let local = snapshot::local(store, account).await;
        let mut differences = snapshot::diff(server, &local);
        for name in required {
            if !local.get(name).is_some_and(|folder| folder.synced) {
                differences.push(format!("{name}: not synced"));
            }
        }
        if differences.is_empty() {
            return Ok(started.elapsed());
        }
        if started.elapsed() > within {
            return Err(differences);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Polls until `check` holds, every 5 ms, for at most `within`; returns how long it took.
pub async fn until<F: std::future::Future<Output = bool>>(
    within: Duration,
    mut check: impl FnMut() -> F,
) -> Option<Duration> {
    let started = Instant::now();
    loop {
        if check().await {
            return Some(started.elapsed());
        }
        if started.elapsed() > within {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The statuses in `events` for `account`, as short names.
pub fn statuses(events: &[Event], account: AccountId) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::AccountStatus {
                account: id,
                status,
            } if *id == account => Some(status_name(*status)),
            _ => None,
        })
        .collect()
}

pub fn status_name(status: SyncStatus) -> &'static str {
    match status {
        SyncStatus::Syncing => "syncing",
        SyncStatus::Synced => "synced",
        SyncStatus::Offline(_) => "offline",
        SyncStatus::NeedsSignIn(_) => "needs-sign-in",
        SyncStatus::CertificateChanged => "certificate",
        SyncStatus::QuotaFull => "quota",
    }
}

/// How many of each kind of event `events` has.
pub fn tally(events: &[Event]) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for event in events {
        let kind = match event {
            Event::AccountStatus { status, .. } => status_name(*status),
            Event::SyncProgress { .. } => "progress",
            Event::FoldersSynced { .. } => "folders-synced",
            Event::OlderLoaded { .. } => "older-loaded",
            Event::BodyFetched { result: Ok(()), .. } => "body-ok",
            Event::BodyFetched { result: Err(_), .. } => "body-failed",
            _ => "other",
        };
        *out.entry(kind).or_default() += 1;
    }
    out
}
