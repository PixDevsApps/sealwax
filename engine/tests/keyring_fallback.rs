//! The no-keyring fallback (ADR-008, ML-048): when no Secret Service answers, the engine keeps
//! secrets in memory and tells the app once. The session bus address points at a socket that
//! doesn't exist, so this test can never reach the user's keyring.

use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Security};
use sealwax_engine::api::{Command, Event, Password};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

/// The events that arrive within `wait`, up to the first one `done` accepts.
fn events(engine: &Engine, wait: Duration, mut done: impl FnMut(&Event) -> bool) -> Vec<Event> {
    let receiver = engine.events();
    let started = Instant::now();
    let mut seen = Vec::new();
    while started.elapsed() < wait {
        match receiver.try_recv() {
            Ok(event) => {
                let finished = done(&event);
                seen.push(event);
                if finished {
                    break;
                }
            }
            Err(async_channel::TryRecvError::Closed) => break,
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    seen
}

#[test]
fn without_a_keyring_secrets_stay_in_memory_and_the_app_hears_it_once() {
    let root = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary, before the engine starts its threads.
    unsafe {
        std::env::set_var(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", root.path().join("no-bus").display()),
        );
    }
    let dirs = SealwaxDirs::under(root.path());
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let options = StoreOptions {
        blobs: Some(blobs.clone()),
        ..StoreOptions::default()
    };
    let store = Store::open(&dirs.store_file(), options).unwrap();
    let engine = Engine::start(EngineConfig {
        application: "io.github.pixdevsapps.Sealwax.Test".into(),
        store: Arc::new(store),
        blobs,
        paths: AccountPaths::new(&dirs),
        trust: Trust::Platform,
        secrets: SecretBackend::Keyring,
        detect: DetectOptions::default(),
        microsoft_client_id: None,
        sign_in: SignInOptions::default(),
        sync: SyncConfig::default(),
    })
    .unwrap();
    let seen = events(&engine, WAIT, |event| {
        matches!(event, Event::KeyringUnavailable)
    });
    assert!(
        matches!(seen.last(), Some(Event::KeyringUnavailable)),
        "the fallback is reported"
    );

    // The add's secret goes to memory, and the fallback isn't reported again.
    let closed = Endpoint {
        host: "127.0.0.1".into(),
        port: 9,
        security: Security::Tls,
    };
    let settings = AccountSettings {
        key: AccountKey(String::new()),
        name: "Memory".into(),
        address: "memory@example.test".into(),
        imap: closed.clone(),
        smtp: closed,
        username: "memory".into(),
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    };
    engine
        .commands()
        .send_blocking(Command::AddAccount {
            request: 1,
            settings,
            password: Password::new("fictional-memory-password".into()),
            sign_in: None,
        })
        .unwrap();
    let seen = events(
        &engine,
        WAIT,
        |event| matches!(event, Event::AccountStatus { status, .. } if status.failure().is_some()),
    );
    assert!(
        seen.iter()
            .any(|event| matches!(event, Event::AccountAdded { result: Ok(_), .. })),
        "the account was added"
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::KeyringUnavailable)),
        "reported once"
    );
    engine.stop();
}
