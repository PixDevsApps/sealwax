//! The engine service end to end, offline: commands in, events out. A preset address is detected
//! without the network; an account whose server refuses connections is added, reports its sync
//! failure, and is removed with its entry and its secret.

use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, DetectSource, Detection, Endpoint, LoginFailure,
    Security,
};
use sealwax_engine::api::{Command, Event, Password};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend};
use sealwax_infrastructure::accounts::{self, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

fn start(dirs: &SealwaxDirs) -> Engine {
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let options = StoreOptions {
        blobs: Some(blobs.clone()),
        ..StoreOptions::default()
    };
    let store = Store::open(&dirs.store_file(), options).unwrap();
    Engine::start(EngineConfig {
        application: "io.github.pixdevsapps.Sealwax.Test".into(),
        store: Arc::new(store),
        blobs,
        paths: AccountPaths::new(dirs),
        trust: Trust::Platform,
        secrets: SecretBackend::Memory,
        detect: DetectOptions::default(),
    })
    .unwrap()
}

/// The next event that `pick` accepts; other events are skipped.
fn next<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let events = engine.events();
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "no such event within {WAIT:?}");
        let event = recv_within(&events, left).expect("the engine stopped");
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

fn recv_within(events: &async_channel::Receiver<Event>, wait: Duration) -> Option<Event> {
    let started = std::time::Instant::now();
    loop {
        match events.try_recv() {
            Ok(event) => return Some(event),
            Err(async_channel::TryRecvError::Closed) => return None,
            Err(async_channel::TryRecvError::Empty) if started.elapsed() > wait => return None,
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
fn a_preset_address_is_detected_without_the_network() {
    let root = tempfile::tempdir().unwrap();
    let engine = start(&SealwaxDirs::under(root.path()));
    engine
        .commands()
        .send_blocking(Command::Detect {
            request: 7,
            address: "test@fastmail.com".into(),
        })
        .unwrap();
    let source = next(&engine, |event| match event {
        Event::DetectProgress { request: 7, source } => Some(source),
        _ => None,
    });
    assert_eq!(source, DetectSource::Preset);
    let detection = next(&engine, |event| match event {
        Event::Detected {
            request: 7,
            detection,
        } => Some(detection),
        _ => None,
    });
    let Detection::Found(found) = detection else {
        panic!("Fastmail is a preset: {detection:?}");
    };
    assert_eq!(found.imap.security, Security::Tls);
    engine.stop();
}

#[test]
fn an_account_is_added_reports_its_sync_and_is_removed() {
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let engine = start(&dirs);
    let port = closed_port();
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
    };
    let settings = AccountSettings {
        key: AccountKey(String::new()),
        name: "Service".into(),
        address: "service@example.test".into(),
        imap: endpoint.clone(),
        smtp: endpoint,
        username: "service".into(),
        auth: AuthMethod::Password,
    };
    let commands = engine.commands();
    commands
        .send_blocking(Command::AddAccount {
            request: 1,
            settings,
            password: Password::new("fictional-service-password".into()),
        })
        .unwrap();
    let account = next(&engine, |event| match event {
        Event::AccountAdded { request: 1, result } => Some(result.expect("the add failed")),
        _ => None,
    });
    let failure = next(&engine, |event| match event {
        Event::SyncFailed {
            account: id,
            failure,
        } if id == account => Some(failure),
        _ => None,
    });
    assert_eq!(failure, LoginFailure::Unreachable);

    let listed = accounts::read(&dirs.accounts_file()).unwrap();
    assert_eq!(listed.len(), 1);
    let key = listed[0].key.clone();
    assert_eq!(key.0.len(), 12, "a generated key");

    commands
        .send_blocking(Command::RemoveAccount { request: 2, key })
        .unwrap();
    let removed = next(&engine, |event| match event {
        Event::AccountRemoved { request: 2, result } => Some(result),
        _ => None,
    });
    assert_eq!(removed, Ok(()));
    assert!(accounts::read(&dirs.accounts_file()).unwrap().is_empty());
    engine.stop();
}
