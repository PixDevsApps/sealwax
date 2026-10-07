//! POP3 accounts end to end on the Docker testbed (M8, ML-088): the engine service with the
//! real POP3 client against Dovecot's and Stalwart's POP3 over implicit TLS. The `popclient`
//! user's INBOX is filled over IMAP; the account is added as the setup assistant adds it, its
//! first pass downloads everything byte for byte without new mail, later mail is new mail on
//! Refresh, a reply goes out through Stalwart's submission with its Sent copy filed locally,
//! removing the account leaves the server's mail, and an account added again without leaving
//! mail on the server downloads it all and deletes it there. Sending goes to the testbed's own
//! user only. Needs the testbed running and seeded, one test at a time (they share popclient):
//!
//! SEALWAX_TESTBED_CA=tools/testbed/certs/ca.pem cargo test -p sealwax-engine --release \
//!   --test pop3_accounts_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod snapshot;
mod testbed_client;

use async_imap::imap_proto::{AttributeValue, Response};
use common::{DOVECOT, PASSWORD, STALWART, Server as Imap, ca, submission, trust};
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, Endpoint, Pop3Policy, Receive, Security,
};
use sealwax_domain::compose::Draft;
use sealwax_domain::model::{AccountId, Address, FolderId, FolderRole, MessageId};
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::{self, AccountPaths};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{DraftSaved, SaveDraft, Store, StoreOptions, ViewSpec};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use testbed_client::Client;

const USER: &str = "popclient";
const WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
struct Pop3 {
    name: &'static str,
    port: u16,
    imap: Imap,
}

const DOVECOT_POP3: Pop3 = Pop3 {
    name: "dovecot",
    port: 14995,
    imap: DOVECOT,
};
const STALWART_POP3: Pop3 = Pop3 {
    name: "stalwart",
    port: 16995,
    imap: STALWART,
};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A small message of this run's for popclient's INBOX.
fn fixture(tag: &str, n: u32) -> Vec<u8> {
    format!(
        "From: Testbed <testbed@example.test>\r\nTo: {USER}@example.test\r\n\
         Subject: POP3 account {tag} {n}\r\nDate: Wed, 07 Oct 2026 04:{:02}:00 +0000\r\n\
         Message-ID: <{tag}.{n}@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n\
         Melding {n}: bl\u{e5}b\u{e6}r.\r\n.leading dot\r\n",
        n % 60
    )
    .into_bytes()
}

/// popclient's INBOX over IMAP: each message's bytes by Message-ID (EXAMINE, nothing changes).
async fn imap_inbox(server: Imap) -> BTreeMap<String, Vec<u8>> {
    let mut client = Client::connect(server.tls, &ca(), USER).await.unwrap();
    client.uidvalidity("INBOX").await.unwrap();
    let mut found = BTreeMap::new();
    let fetched = client
        .run("FETCH 1:* (BODY.PEEK[])", |response| {
            if let Response::Fetch(_, attributes) = response {
                for attribute in attributes {
                    if let AttributeValue::BodySection {
                        section: None,
                        data,
                        ..
                    } = attribute
                    {
                        let bytes = data.as_deref().unwrap_or_default().to_vec();
                        if let Some(id) = message_id(&bytes) {
                            found.insert(id, bytes);
                        }
                    }
                }
            }
        })
        .await;
    client.logout().await;
    if fetched.is_err() {
        return BTreeMap::new();
    }
    found
}

fn message_id(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("message-id")
                .then(|| value.trim().to_owned())
        })
}

/// Empties popclient's INBOX and appends `messages`, unseen.
async fn reset_inbox(server: Imap, messages: &[Vec<u8>]) {
    let mut client = Client::connect(server.tls, &ca(), USER).await.unwrap();
    client.empty("INBOX").await.unwrap();
    for message in messages {
        client.append("INBOX", "", message).await.unwrap();
    }
    client.logout().await;
}

async fn append(server: Imap, messages: &[Vec<u8>]) {
    let mut client = Client::connect(server.tls, &ca(), USER).await.unwrap();
    for message in messages {
        client.append("INBOX", "", message).await.unwrap();
    }
    client.logout().await;
}

struct Running {
    _dir: tempfile::TempDir,
    dirs: SealwaxDirs,
    engine: Option<Engine>,
    store: Arc<Store>,
    blobs: BlobStore,
    events: async_channel::Receiver<Event>,
    seen: Vec<Event>,
}

impl Running {
    fn start() -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("pop3-accounts-")
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
            blobs: blobs.clone(),
            paths: AccountPaths::new(&dirs),
            trust: trust(),
            secrets: SecretBackend::Memory,
            detect: DetectOptions::default(),
            microsoft_client_id: None,
            sign_in: SignInOptions::default(),
            sync: SyncConfig::default(),
        })
        .unwrap();
        let events = engine.events();
        Running {
            _dir: dir,
            dirs,
            engine: Some(engine),
            store,
            blobs,
            events,
            seen: Vec::new(),
        }
    }

    fn send(&self, command: Command) {
        self.engine
            .as_ref()
            .unwrap()
            .commands()
            .try_send(command)
            .unwrap();
    }

    async fn event<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = tokio::time::timeout(left, self.events.recv())
                .await
                .expect("no such event in time")
                .expect("the engine stopped");
            let found = pick(&event);
            self.seen.push(event);
            if let Some(found) = found {
                return found;
            }
        }
    }

    async fn settled(&mut self, account: AccountId) -> SyncStatus {
        self.event(|event| match event {
            Event::AccountStatus {
                account: id,
                status,
            } if *id == account && *status != SyncStatus::Syncing => Some(*status),
            _ => None,
        })
        .await
    }

    async fn add(&mut self, request: u64, server: Pop3, policy: Pop3Policy) -> AccountId {
        let settings = AccountSettings {
            key: AccountKey(String::new()),
            name: "POP3 testbed".into(),
            address: format!("{USER}@example.test"),
            incoming: Endpoint {
                host: "127.0.0.1".into(),
                port: server.port,
                security: Security::Tls,
            },
            receive: Receive::Pop3(policy),
            smtp: submission(Security::Tls),
            username: USER.into(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        };
        self.send(Command::AddAccount {
            request,
            settings,
            password: Password::new(PASSWORD.into()),
            sign_in: None,
        });
        self.event(|event| match event {
            Event::AccountAdded {
                request: id,
                result,
            } if *id == request => Some(result.clone().expect("the add failed")),
            _ => None,
        })
        .await
    }

    async fn folder(&self, account: AccountId, role: FolderRole) -> FolderId {
        self.store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|folder| folder.role == role)
            .unwrap()
            .folder
    }

    /// A folder's messages, newest first, with their stored bytes by Message-ID.
    async fn stored(&self, folder: FolderId) -> BTreeMap<String, (MessageId, Vec<u8>)> {
        let rows = self
            .store
            .page(ViewSpec::folder(folder), 0, 1_000)
            .recv()
            .await
            .unwrap()
            .rows;
        let mut found = BTreeMap::new();
        for row in rows {
            let raw = self
                .store
                .raw_message(row.id)
                .recv()
                .await
                .unwrap()
                .unwrap();
            found.insert(message_id(&raw).unwrap(), (row.id, raw));
        }
        found
    }

    fn new_mail(&self) -> usize {
        self.seen
            .iter()
            .filter_map(|event| match event {
                Event::NewMail { messages, .. } => Some(messages.len()),
                _ => None,
            })
            .sum()
    }

    fn stop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Polls `check` until it holds.
async fn eventually<F: std::future::Future<Output = bool>>(
    what: &str,
    mut check: impl FnMut() -> F,
) {
    let deadline = Instant::now() + WAIT;
    while !check().await {
        assert!(Instant::now() < deadline, "{what}: not within {WAIT:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn end_to_end(server: Pop3) {
    let tag = format!("pop3acct-{}-{}", server.name, now_ms());
    let first: Vec<Vec<u8>> = (1..=200).map(|n| fixture(&tag, n)).collect();
    reset_inbox(server.imap, &first).await;
    let on_server = imap_inbox(server.imap).await;
    assert_eq!(on_server.len(), 200);

    // Added as the assistant adds it: leave on server, never delete.
    let mut run = Running::start();
    let started = Instant::now();
    let account = run.add(1, server, Pop3Policy::default()).await;
    assert_eq!(run.settled(account).await, SyncStatus::Synced);
    let first_pass = started.elapsed();
    let inbox = run.folder(account, FolderRole::Inbox).await;
    let stored = run.stored(inbox).await;
    assert_eq!(stored.len(), 200);
    for (id, bytes) in &on_server {
        assert_eq!(&stored[id].1, bytes, "{id}: byte for byte with IMAP");
    }
    assert_eq!(run.new_mail(), 0, "the first pass is no new mail");
    let uidls = run.store.pop3_uidls(account).recv().await.unwrap();
    assert_eq!(uidls.len(), 200);
    assert!(
        uidls
            .iter()
            .all(|uidl| uidl.whole && uidl.deleted.is_none())
    );
    println!(
        "{}: first pass of 200 messages, AddAccount → synced in {:.1} ms",
        server.name,
        first_pass.as_secs_f64() * 1000.0
    );

    // Later mail, on Refresh.
    let later: Vec<Vec<u8>> = (201..=202).map(|n| fixture(&tag, n)).collect();
    append(server.imap, &later).await;
    let asked = Instant::now();
    run.send(Command::CheckMail);
    let arrived = run
        .event(|event| match event {
            Event::NewMail {
                account: id,
                folder,
                messages,
            } if *id == account => Some((*folder, messages.len())),
            _ => None,
        })
        .await;
    assert_eq!(arrived, (inbox, 2));
    println!(
        "{}: CheckMail → NewMail for 2 messages in {:.1} ms",
        server.name,
        asked.elapsed().as_secs_f64() * 1000.0
    );

    // A reply through Stalwart's submission, to the testbed's own user; its Sent copy local.
    let (source, _) = run.stored(inbox).await[&format!("<{tag}.1@example.test>")].clone();
    let draft = Draft {
        account,
        from: Address::new("POP3 testbed", format!("{USER}@example.test")),
        to: vec![Address::new("", format!("{USER}@example.test"))],
        subject: format!("Re: POP3 account {tag} 1"),
        html: "<p>Svar</p>".into(),
        message_id: format!("{tag}.reply@example.test"),
        in_reply_to: Some(format!("{tag}.1@example.test")),
        references: vec![format!("{tag}.1@example.test")],
        source: Some(sealwax_domain::compose::DraftSource {
            message: source,
            kind: sealwax_domain::compose::ReplyKind::Reply,
        }),
        ..Draft::default()
    };
    let saved = run
        .store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: now_ms(),
        })
        .recv()
        .await
        .unwrap();
    let DraftSaved::Saved { id: draft, .. } = saved else {
        panic!("not saved: {saved:?}");
    };
    run.store.queue_send(draft, now_ms()).recv().await.unwrap();
    run.send(Command::OutboxQueued { account });
    run.event(|event| match event {
        Event::Sent { draft: id, .. } if *id == draft => Some(()),
        _ => None,
    })
    .await;
    let sent = run.folder(account, FolderRole::Sent).await;
    let store = run.store.clone();
    eventually("the Sent copy filed", || {
        let store = store.clone();
        async move { store.draft(draft).recv().await.unwrap().is_none() }
    })
    .await;
    let filed = run.stored(sent).await;
    assert!(filed.contains_key(&format!("<{tag}.reply@example.test>")));
    let answered = run
        .store
        .message(source)
        .recv()
        .await
        .unwrap()
        .value
        .unwrap();
    assert!(
        answered
            .summary
            .flags
            .contains(sealwax_domain::model::Flags::ANSWERED)
    );
    if server.name == "stalwart" {
        // Delivered to popclient on Stalwart: it comes back through POP3.
        let reply = format!("<{tag}.reply@example.test>");
        let store = run.store.clone();
        eventually("the reply back in the Inbox", || {
            run.send(Command::CheckMail);
            let store = store.clone();
            let reply = reply.clone();
            async move {
                let rows = store
                    .page(ViewSpec::folder(inbox), 0, 1_000)
                    .recv()
                    .await
                    .unwrap()
                    .rows;
                for row in rows {
                    let raw = store.raw_message(row.id).recv().await.unwrap().unwrap();
                    if message_id(&raw).as_deref() == Some(reply.as_str()) {
                        return true;
                    }
                }
                false
            }
        })
        .await;
    }

    // Removed: its folders, mail and UIDLs leave the store; the server's mail stays.
    let key = accounts::read(&run.dirs.accounts_file()).unwrap()[0]
        .key
        .clone();
    let blobs = run.blobs.clone();
    run.send(Command::RemoveAccount { request: 2, key });
    let removed = run
        .event(|event| match event {
            Event::AccountRemoved { request: 2, result } => Some(result.clone()),
            _ => None,
        })
        .await;
    assert_eq!(removed, Ok(()));
    assert!(run.store.accounts().recv().await.unwrap().value.is_empty());
    assert!(
        run.store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        blobs.disk_usage().unwrap().files,
        0,
        "its raw messages are gone"
    );
    let left = imap_inbox(server.imap).await.len();
    assert!(left >= 202, "the server keeps its mail: {left}");

    // Added again without leaving mail on the server: all downloaded, then deleted there.
    let deleting = Pop3Policy {
        leave_on_server: false,
        delete_after_days: None,
    };
    let account = run.add(3, server, deleting).await;
    assert_eq!(run.settled(account).await, SyncStatus::Synced);
    let inbox = run.folder(account, FolderRole::Inbox).await;
    assert_eq!(run.stored(inbox).await.len(), left);
    eventually("the server's INBOX emptied", || async {
        imap_inbox(server.imap).await.is_empty()
    })
    .await;
    let uidls = run.store.pop3_uidls(account).recv().await.unwrap();
    assert!(uidls.iter().all(|uidl| uidl.deleted.is_some()));
    assert_eq!(run.stored(inbox).await.len(), left, "the local copies stay");
    run.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn a_pop3_account_end_to_end_on_dovecot() {
    end_to_end(DOVECOT_POP3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the testbed: tools/testbed/up.sh and seed.sh"]
async fn a_pop3_account_end_to_end_on_stalwart() {
    end_to_end(STALWART_POP3).await;
}
