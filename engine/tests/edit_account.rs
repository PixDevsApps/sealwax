//! Edit Account through the engine service (the user's UI tweak, 2026-10-08), offline: the
//! account's label and colour go into `accounts.toml`, its sender name stays, and once the
//! file is sent again with `Accounts` the store names the account by its label, else its
//! address.

use sealwax_domain::account::{
    AccountColor, AccountKey, AccountSettings, AuthMethod, Endpoint, Receive, Security,
};
use sealwax_engine::api::{AccountError, Command, Event};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::{self as files, AccountPaths};
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

/// The store's name for the account with `key`, once it is `wanted`.
fn wait_for_name(store: &Store, key: &AccountKey, wanted: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        let name = store
            .account_by_key(key.clone())
            .recv_blocking()
            .unwrap()
            .value
            .map(|account| account.name);
        if name.as_deref() == Some(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the store says {name:?}, not {wanted}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_edited_label_and_color_reach_the_file_and_the_store() {
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let paths = AccountPaths::new(&dirs);
    let endpoint = |port| Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
    };
    let key = AccountKey("edited".into());
    files::add(
        &paths.settings,
        &AccountSettings {
            key: key.clone(),
            name: "Ada Lovelace".into(),
            label: None,
            color: None,
            address: "ada@example.test".into(),
            incoming: endpoint(closed_port()),
            receive: Receive::Imap,
            smtp: endpoint(closed_port()),
            username: "ada".into(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        },
    )
    .unwrap();
    let (engine, store) = start(&dirs);
    let commands = engine.commands();
    let send_file = || {
        commands
            .send_blocking(Command::Accounts {
                accounts: files::read(&paths.settings).unwrap(),
            })
            .unwrap();
    };
    send_file();
    wait_for_name(&store, &key, "ada@example.test");

    commands
        .send_blocking(Command::EditAccount {
            request: 7,
            key: key.clone(),
            label: Some("Work".into()),
            color: Some(AccountColor::Slate),
        })
        .unwrap();
    let edited = next(&engine, |event| match event {
        Event::AccountEdited { request: 7, result } => Some(result),
        _ => None,
    });
    assert_eq!(edited, Ok(()));
    let listed = files::read(&paths.settings).unwrap();
    assert_eq!(
        (listed[0].title(), listed[0].color, listed[0].name.as_str()),
        ("Work", Some(AccountColor::Slate), "Ada Lovelace")
    );
    send_file();
    wait_for_name(&store, &key, "Work");

    commands
        .send_blocking(Command::EditAccount {
            request: 8,
            key: key.clone(),
            label: None,
            color: None,
        })
        .unwrap();
    let cleared = next(&engine, |event| match event {
        Event::AccountEdited { request: 8, result } => Some(result),
        _ => None,
    });
    assert_eq!(cleared, Ok(()));
    send_file();
    wait_for_name(&store, &key, "ada@example.test");

    commands
        .send_blocking(Command::EditAccount {
            request: 9,
            key: AccountKey("nobody".into()),
            label: Some("X".into()),
            color: None,
        })
        .unwrap();
    let missing = next(&engine, |event| match event {
        Event::AccountEdited { request: 9, result } => Some(result),
        _ => None,
    });
    assert_eq!(missing, Err(AccountError::Missing));
    engine.stop();
}
