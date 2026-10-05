//! ADR-009's consequence and SECURITY.md's credential rule: an account added through the engine
//! keeps its password in the keyring only. With a known dummy password, nothing under the
//! account's folders (`accounts.toml`, the journal, the store and its WAL, the blobs) and no
//! log line at trace level contains it, and no log line contains the address. A Microsoft
//! account signed in through the engine against the fake Microsoft (`mock/`) does the same for
//! every code, verifier, access token, refresh token and ID token: none reaches a file or a log
//! line, through the sign-in, the add, rotating refreshes, a refused refresh, a new sign-in and
//! the removal; and no log line holds the username, the address or the sign-in page's URL.

mod mock;

use mock::{CLIENT, Microsoft};
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, Endpoint, LoginFailure, OAuthProvider, Security,
};
use sealwax_domain::model::FolderRole;
use sealwax_engine::accounts::Accounts;
use sealwax_engine::api::{AccountError, Command, Event, Password};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::secrets::{MemoryStore, SecretKind, SecretStore};
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions};
use sealwax_infrastructure::accounts::{self, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{RawMessage, ingest_raw};
use sealwax_infrastructure::store::{FolderSpec, Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tracing_subscriber::fmt::MakeWriter;

const PASSWORD: &str = "Sealwax-dummy-password-7f3c9e1a";
const ADDRESS: &str = "no-secret@example.test";

/// Every log line, as the formatter wrote it.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

/// Every log line of this test binary at trace level, with no target capped: both tests share
/// the one global subscriber.
fn logs() -> &'static Captured {
    static LOGS: OnceLock<Captured> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Captured::default();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .finish(),
        )
        .unwrap();
        logs
    })
}

/// A scratch folder under `target/` (on the real disk).
fn scratch(prefix: &str) -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .unwrap()
}

/// Every file under `dir` whose bytes contain `needle`, and how many files were read.
fn files_containing(dir: &Path, needle: &[u8]) -> (Vec<PathBuf>, usize) {
    let mut found = Vec::new();
    let mut read = 0;
    let mut stack = vec![dir.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                read += 1;
                let bytes = fs::read(&path).unwrap();
                if bytes.windows(needle.len()).any(|window| window == needle) {
                    found.push(path);
                }
            }
        }
    }
    (found, read)
}

fn settings(address: &str) -> AccountSettings {
    let endpoint = |host: &str, port| Endpoint {
        host: host.to_owned(),
        port,
        security: Security::Tls,
    };
    AccountSettings {
        key: AccountKey(String::new()),
        name: "No Secret".to_owned(),
        address: address.to_owned(),
        imap: endpoint("imap.example.test", 993),
        smtp: endpoint("smtp.example.test", 465),
        username: address.to_owned(),
        auth: AuthMethod::Password,
    }
}

#[test]
fn no_secret_reaches_a_file_or_a_log() {
    let logs = logs();
    let root = scratch("no-secret-");
    let dirs = SealwaxDirs::under(root.path());
    let paths = AccountPaths::new(&dirs);
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let store = Store::open(
        &dirs.store_file(),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let secrets = MemoryStore::default();
    let accounts = Accounts {
        paths: &paths,
        store: &store,
        blobs: Some(&blobs),
        secrets: &secrets,
    };
    let runtime = sealwax_engine::runtime().unwrap();
    let password = Password::new(PASSWORD.to_owned());

    let key = runtime.block_on(async {
        let (key, id) = accounts.add(settings(ADDRESS), &password).await.unwrap();
        assert_eq!(
            accounts.add(settings(ADDRESS), &password).await,
            Err(AccountError::Exists)
        );
        let (other, _) = accounts
            .add(settings("other@example.test"), &password)
            .await
            .unwrap();
        accounts.remove(&other).await.unwrap();
        let inbox = store
            .upsert_folder(FolderSpec::local(id, "INBOX", FolderRole::Inbox))
            .recv()
            .await
            .unwrap()
            .value;
        let raw = format!(
            "From: Sender <sender@example.test>\r\nTo: {ADDRESS}\r\nSubject: Hello\r\n\
             Message-ID: <hello@example.test>\r\n\r\nA message, and no password in it.\r\n"
        );
        ingest_raw(
            &store,
            &blobs,
            vec![RawMessage {
                folder: inbox,
                raw: raw.into_bytes(),
                ..RawMessage::default()
            }],
        )
        .unwrap();
        key
    });

    // The test is not vacuous: the secret is in the keyring, the account in the file.
    let kept = runtime
        .block_on(secrets.read(&key.0, SecretKind::Password))
        .unwrap()
        .unwrap();
    assert_eq!(kept.as_slice(), PASSWORD.as_bytes());
    let file = fs::read_to_string(&paths.settings).unwrap();
    assert!(file.contains(ADDRESS) && file.contains(&key.0));

    let mut wal = dirs.store_file().into_os_string();
    wal.push("-wal");
    assert!(
        Path::new(&wal).exists(),
        "the store is open, its WAL in use"
    );
    let (found, read) = files_containing(root.path(), PASSWORD.as_bytes());
    assert_eq!(found, Vec::<PathBuf>::new(), "the password is in files");
    assert!(
        read >= 5,
        "accounts.toml, the store, its WAL and shm, a blob: {read} files"
    );

    store.close().unwrap();
    let (found, _) = files_containing(root.path(), PASSWORD.as_bytes());
    assert_eq!(
        found,
        Vec::<PathBuf>::new(),
        "the password is in files after close"
    );
    let planted = dirs.data.join("blobs/planted");
    fs::write(&planted, format!("x{PASSWORD}x")).unwrap();
    assert_eq!(
        files_containing(root.path(), PASSWORD.as_bytes()).0,
        [planted],
        "the search finds a planted password"
    );

    let logs = logs.text();
    assert!(logs.contains("account added"), "logs were captured: {logs}");
    assert!(
        logs.contains("header batch committed") && logs.contains("account removed"),
        "the store's threads and the removal logged too: {logs}"
    );
    assert!(!logs.contains(PASSWORD), "the password is in the logs");
    assert!(!logs.contains(ADDRESS), "the address is in the logs");
    assert!(
        !logs.contains("other@example.test"),
        "an address is in the logs"
    );
}

const USER: &str = "token-owner@example.test";
const TOKEN_ADDRESS: &str = "token-alias@example.test";
const WAIT: Duration = Duration::from_secs(20);

async fn next<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let events = engine.events();
    tokio::time::timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("the engine stopped");
            if let Some(found) = pick(event) {
                return found;
            }
        }
    })
    .await
    .expect("no such event in time")
}

/// A browser sign-in; returns the page's URL.
async fn sign_in(engine: &Engine, request: u64, account: Option<AccountKey>) -> String {
    engine
        .commands()
        .try_send(Command::SignIn {
            request,
            provider: OAuthProvider::Microsoft,
            address: TOKEN_ADDRESS.into(),
            account,
        })
        .unwrap();
    let url = next(engine, |event| match event {
        Event::SignInOpen { request: id, url } if id == request => Some(url),
        _ => None,
    })
    .await;
    tokio::spawn(mock::browse(url.clone()));
    let result = next(engine, |event| match event {
        Event::SignedIn {
            request: id,
            result,
        } if id == request => Some(result),
        _ => None,
    })
    .await;
    assert_eq!(result, Ok(USER.to_owned()));
    url
}

async fn synced(engine: &Engine) -> Result<(), LoginFailure> {
    next(engine, |event| match event {
        Event::SyncFinished { .. } => Some(Ok(())),
        Event::SyncFailed { failure, .. } => Some(Err(failure)),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_token_reaches_a_file_or_a_log() {
    let logs = logs();
    let root = scratch("no-token-");
    let dirs = SealwaxDirs::under(root.path());
    let tls = mock::tls();
    let microsoft = Microsoft::start(USER).await;
    // Every access token is short of the refresh margin, so every sync refreshes and rotates.
    microsoft.state().lifetime = 60;
    let imap = mock::imap(&tls, &microsoft).await;
    let smtp = mock::smtp(&tls, &microsoft).await;
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let store = Store::open(
        &dirs.store_file(),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let store = Arc::new(store);
    let engine = Engine::start(EngineConfig {
        application: "io.github.pixdevsapps.Sealwax.Test".into(),
        store: store.clone(),
        blobs,
        paths: AccountPaths::new(&dirs),
        trust: tls.trust.clone(),
        secrets: SecretBackend::Memory,
        detect: DetectOptions::default(),
        microsoft_client_id: Some(CLIENT.into()),
        sign_in: SignInOptions {
            microsoft: microsoft.provider(),
            servers: Some((imap.endpoint(), smtp.endpoint())),
            wait: Duration::from_secs(10),
        },
    })
    .unwrap();

    let mut urls = vec![sign_in(&engine, 1, None).await];
    engine
        .commands()
        .try_send(Command::AddAccount {
            request: 2,
            settings: AccountSettings {
                key: AccountKey(String::new()),
                name: "Tokens".into(),
                address: TOKEN_ADDRESS.into(),
                imap: imap.endpoint(),
                smtp: smtp.endpoint(),
                username: TOKEN_ADDRESS.into(),
                auth: AuthMethod::OAuth(OAuthProvider::Microsoft),
            },
            password: Password::new(String::new()),
            sign_in: Some(1),
        })
        .unwrap();
    next(&engine, |event| match event {
        Event::AccountAdded { request: 2, result } => Some(result.unwrap()),
        _ => None,
    })
    .await;
    assert_eq!(synced(&engine).await, Ok(()));
    let listed = accounts::read(&dirs.accounts_file()).unwrap();
    let again = || Command::Accounts {
        accounts: listed.clone(),
    };
    engine.commands().try_send(again()).unwrap();
    assert_eq!(synced(&engine).await, Ok(()));
    microsoft.state().refresh.clear();
    engine.commands().try_send(again()).unwrap();
    assert_eq!(synced(&engine).await, Err(LoginFailure::OAuthRequired));
    urls.push(sign_in(&engine, 3, Some(listed[0].key.clone())).await);
    assert_eq!(synced(&engine).await, Ok(()));

    let secrets = microsoft.state().secrets.clone();
    let kinds = |kind: &str| secrets.iter().filter(|s| s.contains(kind)).count();
    assert!(
        kinds("-code-") == 2 && kinds("-access-") >= 4 && kinds("-refresh-") >= 4,
        "two sign-ins and their refreshes: {} secrets",
        secrets.len()
    );
    assert!(
        secrets.iter().any(|s| s.matches('.').count() == 2),
        "ID tokens"
    );
    let used: Vec<String> = imap.logins().into_iter().map(|(_, token)| token).collect();
    assert!(
        used.iter().all(|token| secrets.contains(token)),
        "the servers saw the tokens"
    );

    let file = fs::read_to_string(dirs.accounts_file()).unwrap();
    assert!(
        file.contains(TOKEN_ADDRESS) && file.contains(USER),
        "the account is in the file"
    );
    let check_files = |when: &str, least: usize| {
        let mut read = 0;
        for secret in &secrets {
            let (found, files) = files_containing(root.path(), secret.as_bytes());
            assert_eq!(found, Vec::<PathBuf>::new(), "a secret is in files {when}");
            read = files;
        }
        assert!(
            read >= least,
            "accounts.toml and the store files {when}: {read}"
        );
    };
    check_files("while the engine runs", 4);
    engine
        .commands()
        .try_send(Command::RemoveAccount {
            request: 4,
            key: listed[0].key.clone(),
        })
        .unwrap();
    let removed = next(&engine, |event| match event {
        Event::AccountRemoved { request: 4, result } => Some(result),
        _ => None,
    })
    .await;
    assert_eq!(removed, Ok(()));
    // As the app does: the engine stops, then the store closes.
    tokio::task::block_in_place(|| {
        engine.stop();
        Arc::into_inner(store).unwrap().close().unwrap();
    });
    check_files("after the engine stopped and the store closed", 2);

    let logs = logs.text();
    for expected in [
        "signed in in the browser",
        "the rotated refresh token replaced the stored one",
        "signed in again",
        "account removed",
    ] {
        assert!(logs.contains(expected), "logs were captured: {expected}");
    }
    for secret in &secrets {
        assert!(!logs.contains(secret.as_str()), "a secret is in the logs");
    }
    for url in &urls {
        let query = url.split_once('?').unwrap().1;
        assert!(
            !logs.contains(query),
            "the sign-in page's URL is in the logs"
        );
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if key == "state" || key == "code_challenge" {
                assert!(!logs.contains(&*value), "the {key} is in the logs");
            }
        }
    }
    assert!(!logs.contains(USER), "the username is in the logs");
    assert!(!logs.contains(TOKEN_ADDRESS), "the address is in the logs");
}
