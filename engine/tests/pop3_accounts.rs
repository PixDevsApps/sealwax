//! A POP3 account through the engine service (M8, ML-088), offline: listed, it gets its local
//! folders and its POP3 actor (no password stored); its folders answer `LoadOlder` complete
//! without an actor; and removed, it leaves nothing in
//! the store. The passes themselves are tested against a fake maildrop in the engine's unit
//! tests; the testbed's POP3 servers come with the client (ML-087), and `accounts.toml`'s POP3
//! table, which `AddAccount` writes, with phase A's setup work.

use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, Endpoint, LoginFailure, Pop3Policy, Receive, Security,
};
use sealwax_domain::model::FolderRole;
use sealwax_engine::api::{Command, Event, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

fn start(dirs: &SealwaxDirs) -> (Engine, Arc<Store>) {
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let options = StoreOptions {
        blobs: Some(blobs.clone()),
        ..StoreOptions::default()
    };
    let store = Arc::new(Store::open(&dirs.store_file(), options).unwrap());
    let engine = Engine::start(EngineConfig {
        application: "io.github.pixdevsapps.Sealwax.Test".into(),
        store: store.clone(),
        blobs,
        paths: AccountPaths::new(dirs),
        trust: Trust::Platform,
        secrets: SecretBackend::Memory,
        detect: DetectOptions::default(),
        microsoft_client_id: None,
        sign_in: SignInOptions::default(),
        sync: SyncConfig::default(),
    })
    .unwrap();
    (engine, store)
}

/// The next event that `pick` accepts; other events are skipped.
fn next<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let events = engine.events();
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no such event within {WAIT:?}");
        match events.try_recv() {
            Ok(event) => {
                if let Some(found) = pick(event) {
                    return found;
                }
            }
            Err(async_channel::TryRecvError::Closed) => panic!("the engine stopped"),
            Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// A local port with nothing listening on it.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

#[test]
fn a_listed_pop3_account_gets_its_local_folders_and_leaves_nothing_when_removed() {
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let (engine, store) = start(&dirs);
    let endpoint = |port| Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
    };
    let key = AccountKey("pop3-service".into());
    let settings = AccountSettings {
        key: key.clone(),
        name: "Pop".into(),
        label: None,
        color: None,
        address: "pop@example.test".into(),
        incoming: endpoint(closed_port()),
        receive: Receive::Pop3(Pop3Policy::default()),
        smtp: endpoint(closed_port()),
        username: "pop".into(),
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    };
    let commands = engine.commands();
    commands
        .send_blocking(Command::Accounts {
            accounts: vec![settings],
        })
        .unwrap();
    let account = next(&engine, |event| match event {
        Event::AccountStatus { account, .. } => Some(account),
        _ => None,
    });
    let status = next(&engine, |event| match event {
        Event::AccountStatus {
            account: id,
            status,
        } if id == account && status != SyncStatus::Syncing => Some(status),
        _ => None,
    });
    // Listed without a password in the keyring: the POP3 actor asks for one.
    assert_eq!(status, SyncStatus::NeedsSignIn(LoginFailure::WrongPassword));
    let roles: Vec<FolderRole> = store
        .account_folders(account)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|folder| folder.role)
        .collect();
    assert_eq!(
        roles,
        [
            FolderRole::Inbox,
            FolderRole::Sent,
            FolderRole::Drafts,
            FolderRole::Junk,
            FolderRole::Trash,
            FolderRole::Archive
        ]
    );
    assert!(
        store
            .pop3_uidls(account)
            .recv_blocking()
            .unwrap()
            .is_empty()
    );
    // Detached (accounts.toml not read yet, or no longer listing it), its folders still show:
    // older mail is answered complete without an actor, and opening one asks for nothing.
    commands
        .send_blocking(Command::Accounts {
            accounts: Vec::new(),
        })
        .unwrap();
    let inbox = store
        .account_folders(account)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .find(|folder| folder.role == FolderRole::Inbox)
        .unwrap()
        .folder;
    commands
        .send_blocking(Command::FolderOpened { folder: inbox })
        .unwrap();
    commands
        .send_blocking(Command::LoadOlder { folder: inbox })
        .unwrap();
    let older = next(&engine, |event| match event {
        Event::OlderLoaded {
            folder,
            fetched,
            complete,
        } => Some((folder, fetched, complete)),
        _ => None,
    });
    assert_eq!(older, (inbox, 0, true));

    commands
        .send_blocking(Command::RemoveAccount { request: 2, key })
        .unwrap();
    let removed = next(&engine, |event| match event {
        Event::AccountRemoved { request: 2, result } => Some(result),
        _ => None,
    });
    assert_eq!(removed, Ok(()));
    assert!(store.accounts().recv_blocking().unwrap().value.is_empty());
    assert!(
        store
            .account_folders(account)
            .recv_blocking()
            .unwrap()
            .is_empty()
    );
    engine.stop();
}
