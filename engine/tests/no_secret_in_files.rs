//! ADR-009's consequence and SECURITY.md's credential rule: an account added through the engine
//! keeps its password in the keyring only. With a known dummy password, nothing under the
//! account's folders (`accounts.toml`, the journal, the store and its WAL, the blobs) and no
//! log line at trace level contains it, and no log line contains the address.

use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Security};
use sealwax_domain::model::FolderRole;
use sealwax_engine::accounts::Accounts;
use sealwax_engine::api::{AccountError, Password};
use sealwax_engine::secrets::{MemoryStore, SecretKind, SecretStore};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{RawMessage, ingest_raw};
use sealwax_infrastructure::store::{FolderSpec, Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    let logs = Captured::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    )
    .unwrap();

    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    fs::create_dir_all(&base).unwrap();
    let root = tempfile::Builder::new()
        .prefix("no-secret-")
        .tempdir_in(base)
        .unwrap();
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
