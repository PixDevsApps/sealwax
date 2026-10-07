//! M9 (F15): imap-proto 0.17.0 panics on a reply whose response code is followed at once by a
//! non-ASCII character (`fuzz/corpus/imap/known-imap-proto-text-after-code`). Against the fake
//! IMAP server (CI, 127.0.0.1 only): the account's actor dies in that panic, the account shows
//! offline, the actor starts again after the backoff and syncs, new mail included, and the log
//! names the account and nothing the server sent. A child process checks that the panic's own
//! report on stderr leaves out its message, which names a character of the reply. Its own test
//! binary, because it installs the global subscriber. Test data only.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, LoginFailure, Receive};
use sealwax_domain::model::AccountId;
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::logging;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);
/// What the fake server says after the response code; nothing of it may reach a log.
const SERVER_TEXT: &str = "ServerOnlyText";
const CHILD: &str = "SEALWAX_TEST_PANIC_REPORT_CHILD";

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The next event `pick` accepts.
async fn event<T>(
    events: &async_channel::Receiver<Event>,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = tokio::time::timeout(left, events.recv())
            .await
            .expect("no such event in time")
            .expect("the engine stopped");
        if let Some(found) = pick(&event) {
            return found;
        }
    }
}

async fn status(events: &async_channel::Receiver<Event>, account: AccountId, want: SyncStatus) {
    event(events, |event| {
        matches!(event, Event::AccountStatus { account: id, status } if *id == account && *status == want)
            .then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imap_protos_panic_restarts_the_account() {
    let capture = Capture::default();
    let sink = capture.clone();
    tracing::subscriber::set_global_default(logging::subscriber(
        tracing_subscriber::EnvFilter::new("trace"),
        move || sink.clone(),
        false,
    ))
    .unwrap();
    logging::redact_dependency_panics();

    let fake = Fake::start(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.create("Sendt", &["\\Sent"]);
        world.append_many("INBOX", 5, |_| 1);
    }
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("actor-panic-")
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
        trust: fake.trust.clone(),
        secrets: SecretBackend::Memory,
        detect: DetectOptions::default(),
        microsoft_client_id: None,
        sign_in: SignInOptions::default(),
        sync: SyncConfig {
            options: SyncOptions {
                now: Some(NOW),
                ..SyncOptions::default()
            },
            account_pass: Duration::from_secs(60),
            backoff_min: Duration::from_millis(200),
            backoff_max: Duration::from_secs(2),
            ..SyncConfig::default()
        },
    })
    .unwrap();
    let events = engine.events();
    let commands = engine.commands();
    commands
        .try_send(Command::AddAccount {
            request: 1,
            settings: AccountSettings {
                key: AccountKey(String::new()),
                name: "Panic".into(),
                address: "panic@example.test".into(),
                incoming: fake.endpoint(),
                receive: Receive::Imap,
                smtp: fake.endpoint(),
                username: "panic@example.test".into(),
                auth: AuthMethod::Password,
                signature: None,
                identities: Vec::new(),
            },
            password: Password::new("fake-password".into()),
            sign_in: None,
        })
        .unwrap();
    let account = event(&events, |event| match event {
        Event::AccountAdded { request: 1, result } => Some(result.clone().unwrap()),
        _ => None,
    })
    .await;
    status(&events, account, SyncStatus::Synced).await;
    let inbox = store
        .account_folders(account)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .find(|folder| folder.name_raw == "INBOX")
        .unwrap()
        .folder;

    // The next account pass's LIST gets the reply imap-proto panics on.
    let lists = fake.world().count("LIST");
    fake.world()
        .refuse("LIST", &format!("OK [ALERT]\u{e9}{SERVER_TEXT}"));
    let asked = Instant::now();
    commands.try_send(Command::CheckMail).unwrap();
    status(&events, account, SyncStatus::Offline(LoginFailure::Other)).await;
    let crashed = asked.elapsed();
    let connections = fake.world().connections.len();
    status(&events, account, SyncStatus::Synced).await;
    let restarted = asked.elapsed();
    println!("the panic shown {crashed:?} after CheckMail; synced again after {restarted:?}");
    assert!(
        fake.world().count("LIST") >= lists + 2,
        "the restarted actor ran an account pass"
    );
    assert!(
        fake.world().connections.len() <= connections + 3,
        "no tight loop of connections"
    );

    // The restarted actor keeps INBOX current: new mail arrives.
    fake.world().append("INBOX", 0, &[]);
    let deadline = Instant::now() + WAIT;
    loop {
        let messages = store
            .folder_sync(inbox)
            .recv()
            .await
            .unwrap()
            .unwrap()
            .messages;
        if messages == 6 {
            break;
        }
        assert!(Instant::now() < deadline, "new mail never arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    tokio::task::block_in_place(|| engine.stop());
    let log = capture.text();
    let line = log
        .lines()
        .find(|line| line.contains("the account's sync panicked"))
        .unwrap_or_else(|| panic!("no line about the panic in:\n{log}"));
    assert!(
        line.contains(&format!("account={}", account.0)) && line.contains("crashes=1"),
        "{line}"
    );
    for leaked in [SERVER_TEXT, "\u{e9}", "ALERT", "char boundary"] {
        assert!(!log.contains(leaked), "the log holds {leaked:?}:\n{log}");
    }
}

/// The panic's message names a character of the server's reply; Sealwax's report leaves it out.
#[test]
fn a_dependencys_panic_is_reported_without_its_message() {
    if std::env::var_os(CHILD).is_some() {
        logging::redact_dependency_panics();
        let reply = format!("A1 OK [ALERT]\u{e9}{SERVER_TEXT}\r\n");
        let _ = async_imap::imap_proto::Response::parse(reply.as_bytes());
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_dependencys_panic_is_reported_without_its_message",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env_remove("RUST_BACKTRACE")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stderr}");
    assert!(
        !output.status.success(),
        "imap-proto 0.17.0 no longer panics"
    );
    assert!(
        stderr.contains("panicked at") && stderr.contains("the message is left out"),
        "{stderr}"
    );
    assert!(
        !stderr.contains(SERVER_TEXT) && !stderr.contains('\u{e9}'),
        "{stderr}"
    );
}
