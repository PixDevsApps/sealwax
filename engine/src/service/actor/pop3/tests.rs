//! The POP3 actor against an in-memory maildrop (the client is ML-087's): the first pass isn't
//! new mail and later mail is, newest first; a cut mid-pass stores nothing twice; the policy's
//! deletions, only of what the store holds and only at a QUIT that answered; `Reconnect`; a
//! refused login; sending through a scripted SMTP server with the Sent copy filed locally,
//! beside a long download; drafts' copies in the local Drafts folder; the bytes of mail deleted
//! for good leaving the disk; a message above IMAP's whole-message limit stored whole, kept by
//! the disk cap and answered from the store only; Refresh's `CheckMail`; messages above the
//! client's cap kept on the server as header rows; a busy maildrop and LOGIN-DELAY. Test data
//! and dummy addresses only.

use super::super::ops::unix_ms;
use super::*;
use crate::api::{BodyError, Event};
use crate::autoconfig::DetectOptions;
use crate::imap::Stop;
use crate::net::Trust;
use crate::net::testing::{Peer, bind, test_ca};
use crate::secrets::{SecretKind, SecretStore};
use crate::service::bodies;
use crate::service::{EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use base64::Engine as _;
use sealwax_domain::account::{AccountKey, Endpoint, Security};
use sealwax_domain::compose::Draft;
use sealwax_domain::model::{Address, FolderRole};
use sealwax_domain::triage::Triage;
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    BodyState, NewAccount, Pop3Download, QueueTriage, SaveDraft, SendQueued, Store, StoreOptions,
    ViewSpec,
};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::sync::MutexGuard;
use tokio::net::TcpListener;

const WAIT: Duration = Duration::from_secs(15);

/// Message `n`: a small plain-text message with a Date and a Message-ID of its own.
fn mail(n: u32) -> Vec<u8> {
    format!(
        "From: Avsender {n} <avsender{n}@example.test>\r\nTo: pop@example.test\r\n\
         Subject: Melding {n}\r\nDate: Mon, 05 Oct 2026 09:{:02}:00 +0000\r\n\
         Message-ID: <pop{n}@example.test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hei nummer {n}.\r\n",
        n % 60
    )
    .into_bytes()
}

/// The maildrop and what the fake server has seen.
#[derive(Default)]
struct Server {
    /// UIDL and bytes, oldest first: a message's number is its place, from 1.
    messages: Vec<(String, Vec<u8>)>,
    /// Logins fail: the server can't be reached.
    down: bool,
    /// Logins are refused.
    refuse: Option<LoginFailure>,
    /// The connection breaks at the RETR after this many in one session (once).
    cut_after: Option<usize>,
    /// QUIT answers -ERR and deletes nothing.
    quit_fails: bool,
    /// Every RETR waits this long first.
    slow: Option<Duration>,
    /// UIDLs whose RETR the client refuses as too large.
    large: Vec<String>,
    /// UIDLs whose data outgrows the size cap while it comes: the session closes.
    outgrow: Vec<String>,
    /// TOP answers -ERR.
    no_top: bool,
    /// The next logins are refused as busy, one each.
    busy: Vec<Busy>,
    /// What CAPA says at the login.
    offers: Offers,
    /// `CONNECT`, `LIST`, `RETR <uidl>` (`RETR <uidl> cut`, `RETR <uidl> too large`,
    /// `RETR <uidl> outgrew`), `TOP <uidl>`, `DELE <uidl>`, `QUIT`.
    log: Vec<String>,
    /// When each login came.
    logins: Vec<Instant>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Server>>);

impl Fake {
    fn with(messages: impl IntoIterator<Item = u32>) -> Fake {
        let fake = Fake::default();
        for n in messages {
            fake.add(n);
        }
        fake
    }

    fn lock(&self) -> MutexGuard<'_, Server> {
        self.0.lock().unwrap()
    }

    fn add(&self, n: u32) {
        self.lock().messages.push((format!("u{n}"), mail(n)));
    }

    fn log(&self) -> Vec<String> {
        self.lock().log.clone()
    }

    fn count(&self, entry: &str) -> usize {
        self.log().iter().filter(|line| *line == entry).count()
    }

    fn uidls(&self) -> Vec<String> {
        self.lock()
            .messages
            .iter()
            .map(|(uidl, _)| uidl.clone())
            .collect()
    }
}

struct FakeSession {
    fake: Fake,
    listed: Vec<String>,
    marked: Vec<String>,
    retrieved: usize,
    /// Closed by data that outgrew the cap.
    closed: bool,
}

impl FakeSession {
    fn uidl(&self, number: u32) -> Result<String, Pop3Error> {
        if self.closed {
            return Err(Pop3Error::ConnectionLost);
        }
        self.listed
            .get(number as usize - 1)
            .cloned()
            .ok_or(Pop3Error::Err)
    }
}

impl Mailbox for FakeSession {
    async fn listing(&mut self) -> Result<Vec<Listing>, Pop3Error> {
        let mut drop = self.fake.lock();
        drop.log.push("LIST".into());
        self.listed = drop.messages.iter().map(|(uidl, _)| uidl.clone()).collect();
        Ok(drop
            .messages
            .iter()
            .enumerate()
            .map(|(index, (uidl, bytes))| Listing {
                number: index as u32 + 1,
                uidl: uidl.clone(),
                size: bytes.len() as u64,
            })
            .collect())
    }

    async fn retrieve(&mut self, number: u32) -> Result<Vec<u8>, Pop3Error> {
        let slow = self.fake.lock().slow;
        if let Some(wait) = slow {
            tokio::time::sleep(wait).await;
        }
        let uidl = self.uidl(number)?;
        let mut drop = self.fake.lock();
        if drop.cut_after.is_some_and(|after| self.retrieved >= after) {
            drop.cut_after = None;
            drop.log.push(format!("RETR {uidl} cut"));
            return Err(Pop3Error::ConnectionLost);
        }
        if drop.large.contains(&uidl) {
            drop.log.push(format!("RETR {uidl} too large"));
            return Err(Pop3Error::TooLarge(200 * 1024 * 1024));
        }
        if drop.outgrow.contains(&uidl) {
            drop.log.push(format!("RETR {uidl} outgrew"));
            self.closed = true;
            return Err(Pop3Error::TooLarge(200 * 1024 * 1024));
        }
        drop.log.push(format!("RETR {uidl}"));
        self.retrieved += 1;
        drop.messages
            .iter()
            .find(|(held, _)| *held == uidl)
            .map(|(_, bytes)| bytes.clone())
            .ok_or(Pop3Error::Err)
    }

    async fn delete(&mut self, number: u32) -> Result<(), Pop3Error> {
        let uidl = self.uidl(number)?;
        self.fake.lock().log.push(format!("DELE {uidl}"));
        self.marked.push(uidl);
        Ok(())
    }

    async fn quit(self) -> Result<(), Pop3Error> {
        let mut drop = self.fake.lock();
        drop.log.push("QUIT".into());
        if drop.quit_fails {
            return Err(Pop3Error::Err);
        }
        drop.messages
            .retain(|(uidl, _)| !self.marked.contains(uidl));
        Ok(())
    }

    async fn header(&mut self, number: u32) -> Result<Vec<u8>, Pop3Error> {
        let uidl = self.uidl(number)?;
        let mut drop = self.fake.lock();
        drop.log.push(format!("TOP {uidl}"));
        if drop.no_top {
            return Err(Pop3Error::Err);
        }
        let bytes = drop
            .messages
            .iter()
            .find(|(held, _)| *held == uidl)
            .map(|(_, bytes)| bytes.clone())
            .ok_or(Pop3Error::Err)?;
        let end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map_or(bytes.len(), |at| at + 4);
        Ok(bytes[..end].to_vec())
    }
}

struct FakeConnect(Fake);

impl Connect for FakeConnect {
    type Mailbox = FakeSession;

    async fn connect(
        &self,
        _shared: &Shared,
        _settings: &AccountSettings,
    ) -> Result<(FakeSession, Offers), Refused> {
        let mut drop = self.0.lock();
        drop.log.push("CONNECT".into());
        drop.logins.push(Instant::now());
        if drop.down {
            return Err(Refused::Login(LoginFailure::Unreachable));
        }
        if let Some(failure) = drop.refuse {
            return Err(Refused::Login(failure));
        }
        if !drop.busy.is_empty() {
            return Err(Refused::Busy(drop.busy.remove(0)));
        }
        let session = FakeSession {
            fake: self.0.clone(),
            listed: Vec::new(),
            marked: Vec::new(),
            retrieved: 0,
            closed: false,
        };
        Ok((session, drop.offers))
    }
}

/// An engine's shared state with one POP3 account and its password, and its events.
struct Harness {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
    events: async_channel::Receiver<Event>,
    seen: Vec<Event>,
    account: AccountId,
    settings: AccountSettings,
    fake: Fake,
}

fn quick() -> SyncConfig {
    SyncConfig {
        pop3_poll: Duration::from_millis(200),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        progress_every: Duration::from_millis(1),
        ..SyncConfig::default()
    }
}

async fn harness(
    policy: Pop3Policy,
    sync: SyncConfig,
    smtp: Option<(u16, Trust)>,
    fake: Fake,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(dir.path());
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let store = Store::open(
        &dirs.store_file(),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let (smtp_port, trust) = smtp.unwrap_or((9, Trust::Platform));
    let (sender, events) = async_channel::unbounded();
    let shared = Arc::new(Shared::new(
        EngineConfig {
            application: "io.github.pixdevsapps.Sealwax.Test".into(),
            store: Arc::new(store),
            blobs,
            paths: AccountPaths::new(&dirs),
            trust,
            secrets: SecretBackend::Memory,
            detect: DetectOptions::default(),
            microsoft_client_id: None,
            sign_in: SignInOptions::default(),
            sync,
        },
        sender,
    ));
    let key = AccountKey("pop3-actor".into());
    let account = shared
        .store
        .ensure_account(
            key.clone(),
            NewAccount {
                name: "Pop".into(),
                address: "pop@example.test".into(),
            },
        )
        .recv()
        .await
        .unwrap()
        .value;
    shared
        .secrets()
        .await
        .store(
            &key.0,
            SecretKind::Password,
            "Sealwax test",
            b"fictional-pop-password",
        )
        .await
        .unwrap();
    let endpoint = |port| Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
    };
    let settings = AccountSettings {
        key,
        name: "Pop".into(),
        address: "pop@example.test".into(),
        incoming: endpoint(9),
        receive: Receive::Pop3(policy),
        smtp: endpoint(smtp_port),
        username: "pop".into(),
        auth: AuthMethod::Password,
        signature: None,
        identities: Vec::new(),
    };
    Harness {
        _dir: dir,
        shared,
        events,
        seen: Vec::new(),
        account,
        settings,
        fake,
    }
}

impl Harness {
    fn start(&self) -> Handle {
        start_with(
            &self.shared,
            self.settings.clone(),
            FakeConnect(self.fake.clone()),
        )
    }

    /// Events until one that `pick` takes, which it returns; every event is kept in `seen`.
    async fn until<T>(&mut self, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = tokio::time::timeout(left, self.events.recv())
                .await
                .expect("no such event in time")
                .unwrap();
            let found = pick(&event);
            self.seen.push(event);
            if let Some(found) = found {
                return found;
            }
        }
    }

    async fn status(&mut self, want: SyncStatus) {
        let account = self.account;
        self.until(|event| match event {
            Event::AccountStatus {
                account: id,
                status,
            } if *id == account && *status == want => Some(()),
            _ => None,
        })
        .await;
    }

    fn new_mail(&self) -> Vec<MessageId> {
        self.seen
            .iter()
            .filter_map(|event| match event {
                Event::NewMail { messages, .. } => Some(messages.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    async fn folder(&self, role: FolderRole) -> FolderId {
        self.shared
            .store
            .account_folders(self.account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|folder| folder.role == role)
            .unwrap()
            .folder
    }

    /// The subjects of a folder's messages, newest first.
    async fn subjects(&self, folder: FolderId) -> Vec<String> {
        self.shared
            .store
            .page(ViewSpec::folder(folder), 0, 1_000)
            .recv()
            .await
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row.subject)
            .collect()
    }

    async fn uidls(&self) -> Vec<Pop3Uidl> {
        self.shared
            .store
            .pop3_uidls(self.account)
            .recv()
            .await
            .unwrap()
    }

    /// Waits until `check` holds of the store, polling.
    async fn eventually<F: Future<Output = bool>>(&self, mut check: impl FnMut() -> F) {
        let deadline = Instant::now() + WAIT;
        while !check().await {
            assert!(Instant::now() < deadline, "the store never got there");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn stop(actor: Handle) {
    actor.stop(Stop::Drop);
    tokio::time::timeout(WAIT, actor.task)
        .await
        .unwrap()
        .unwrap();
}

fn retrieved(log: &[String]) -> Vec<String> {
    log.iter()
        .filter_map(|line| line.strip_prefix("RETR "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_policy_deletes_by_download_age() {
    let now = 1_790_000_000;
    let keep = Pop3Policy::default();
    let at_once = Pop3Policy {
        leave_on_server: false,
        delete_after_days: Some(30),
    };
    let week = Pop3Policy {
        leave_on_server: true,
        delete_after_days: Some(7),
    };
    assert!(!deletes(keep, now, now - 1_000 * DAY));
    assert!(deletes(at_once, now, now));
    assert!(deletes(week, now, now - 7 * DAY));
    assert!(!deletes(week, now, now - 7 * DAY + 1));
    assert!(!deletes(week, now, now));
    let same_day = Pop3Policy {
        delete_after_days: Some(0),
        ..week
    };
    assert!(deletes(same_day, now, now));
}

#[test]
fn downloads_go_newest_first_without_shared_uidls() {
    let listing: Vec<Listing> = [(1, "a"), (2, "b"), (3, "c"), (4, "b"), (5, "d")]
        .into_iter()
        .map(|(number, uidl)| Listing {
            number,
            uidl: uidl.into(),
            size: 10,
        })
        .collect();
    let known = HashSet::from(["c"]);
    let order: Vec<(u32, &str)> = to_download(&listing, &known)
        .into_iter()
        .map(|entry| (entry.number, entry.uidl.as_str()))
        .collect();
    assert_eq!(order, [(5, "d"), (1, "a")], "b is shared: left alone");
    assert_eq!(shared_uidls(&listing), HashSet::from(["b"]));
}

#[test]
fn a_status_header_with_r_marks_a_message_read() {
    let read = |raw: &str| read_by_header(raw.as_bytes());
    assert!(read("From: a@example.test\r\nStatus: RO\r\n\r\nBody\r\n"));
    assert!(read("status:R\n\nBody"));
    assert!(!read("From: a@example.test\r\nStatus: O\r\n\r\nBody\r\n"));
    assert!(!read("X-Status: R\r\n\r\nBody\r\n"));
    assert!(
        !read("Subject: x\r\n\r\nStatus: R\r\n"),
        "a body line isn't a header"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_pass_isnt_new_mail_and_later_mail_is() {
    let mut h = harness(Pop3Policy::default(), quick(), None, Fake::with(1..=3)).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    assert!(h.new_mail().is_empty(), "the first pass notifies nothing");
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(
        h.subjects(inbox).await,
        ["Melding 3", "Melding 2", "Melding 1"]
    );
    assert_eq!(retrieved(&h.fake.log()), ["u3", "u2", "u1"], "newest first");
    let progress: Vec<(u32, u32)> = h
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::SyncProgress { fetched, total, .. } => Some((*fetched, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(progress.last(), Some(&(3, 3)));
    let first = h.shared.store.pop3_account(h.account).recv().await.unwrap();
    assert!(first.first_pass.is_some());

    // Later mail is new mail, as each batch commits.
    h.fake.add(4);
    h.fake.add(5);
    let account = h.account;
    let arrived = h
        .until(|event| match event {
            Event::NewMail {
                account: id,
                folder,
                messages,
            } if *id == account => Some((*folder, messages.clone())),
            _ => None,
        })
        .await;
    assert_eq!(arrived.0, inbox);
    assert_eq!(arrived.1.len(), 2);
    let newest = h
        .shared
        .store
        .message(arrived.1[0])
        .recv()
        .await
        .unwrap()
        .value
        .unwrap();
    assert_eq!(newest.summary.subject, "Melding 5");
    // The default policy leaves everything on the server.
    assert_eq!(h.fake.uidls(), ["u1", "u2", "u3", "u4", "u5"]);
    assert!(!h.fake.log().iter().any(|line| line.starts_with("DELE")));
    let uidls = h.uidls().await;
    assert_eq!(uidls.len(), 5);
    assert!(
        uidls
            .iter()
            .all(|uidl| uidl.message.is_some() && uidl.deleted.is_none())
    );
    // A later pass with nothing new downloads nothing.
    let lists = h.fake.count("LIST");
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.count("LIST") > lists + 1 }
    })
    .await;
    assert_eq!(retrieved(&h.fake.log()).len(), 5);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_mid_pass_downloads_the_cut_message_again_and_stores_none_twice() {
    let fake = Fake::with(1..=10);
    fake.lock().cut_after = Some(4);
    let mut h = harness(Pop3Policy::default(), quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let log = h.fake.log();
    let retrieved = retrieved(&log);
    assert_eq!(
        retrieved,
        [
            "u10", "u9", "u8", "u7", "u6 cut", "u6", "u5", "u4", "u3", "u2", "u1"
        ]
    );
    assert_eq!(h.fake.count("CONNECT"), 2, "again at once after a cut");
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await.len(), 10);
    let uidls = h.uidls().await;
    assert_eq!(uidls.len(), 10);
    let mut rows: Vec<MessageId> = uidls.iter().map(|uidl| uidl.message.unwrap()).collect();
    rows.sort();
    rows.dedup();
    assert_eq!(rows.len(), 10, "each UIDL its own row");
    assert!(
        h.new_mail().is_empty(),
        "the first complete pass ends after the cut"
    );
    assert_eq!(
        h.shared.store.check().recv().await.unwrap(),
        Vec::<String>::new()
    );
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_leaving_mail_deletes_what_is_stored_at_quit() {
    let policy = Pop3Policy {
        leave_on_server: false,
        delete_after_days: None,
    };
    let mut h = harness(policy, quick(), None, Fake::with(1..=3)).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    assert!(h.fake.uidls().is_empty());
    let log = h.fake.log();
    let last_retr = log
        .iter()
        .rposition(|line| line.starts_with("RETR"))
        .unwrap();
    let first_dele = log
        .iter()
        .position(|line| line.starts_with("DELE"))
        .unwrap();
    let quit = log.iter().position(|line| line == "QUIT").unwrap();
    assert!(last_retr < first_dele && first_dele < quit);
    let uidls = h.uidls().await;
    assert_eq!(uidls.len(), 3);
    assert!(uidls.iter().all(|uidl| uidl.deleted.is_some()));
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await.len(), 3, "the local copies stay");
    // The next pass doesn't list them: Sealwax forgets their UIDLs, never their mail.
    let lists = h.fake.count("LIST");
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.count("LIST") > lists }
    })
    .await;
    h.eventually(|| {
        let store = h.shared.store.clone();
        let account = h.account;
        async move { store.pop3_uidls(account).recv().await.unwrap().is_empty() }
    })
    .await;
    assert_eq!(h.subjects(inbox).await.len(), 3);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_deleted_from_the_server_that_the_store_didnt_record() {
    let policy = Pop3Policy {
        leave_on_server: false,
        delete_after_days: None,
    };
    let mut h = harness(policy, quick(), None, Fake::with(1..=3)).await;
    // A fault planted in the database: every batch with u2 fails to commit.
    let path = h.shared.store.path().to_owned();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fault BEFORE INSERT ON pop3_uidls WHEN NEW.uidl = 'u2' \
         BEGIN SELECT RAISE(ABORT, 'planted'); END;",
    )
    .unwrap();
    let actor = h.start();
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.count("CONNECT") >= 3 }
    })
    .await;
    let log = h.fake.log();
    assert!(!log.iter().any(|line| line.starts_with("DELE")), "{log:?}");
    assert!(!log.iter().any(|line| line == "QUIT"));
    assert_eq!(h.fake.uidls(), ["u1", "u2", "u3"]);
    assert!(h.uidls().await.is_empty());
    // With the fault gone, the next pass stores them and deletes them.
    conn.execute_batch("DROP TRIGGER fault;").unwrap();
    drop(conn);
    actor.reconnect();
    h.status(SyncStatus::Synced).await;
    assert!(h.fake.uidls().is_empty());
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await.len(), 3);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_quit_is_settled_by_the_next_listing() {
    let policy = Pop3Policy {
        leave_on_server: false,
        delete_after_days: None,
    };
    let fake = Fake::with(1..=2);
    fake.lock().quit_fails = true;
    let mut h = harness(policy, quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    // QUIT went unanswered: recorded as possibly deleted (here the server kept them).
    assert_eq!(h.fake.uidls(), ["u1", "u2"]);
    assert!(h.uidls().await.iter().all(|uidl| uidl.deleted.is_some()));
    // The next listing still shows them: marked again, and gone at a QUIT that answers.
    h.fake.lock().quit_fails = false;
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.uidls().is_empty() }
    })
    .await;
    // Then no listing shows them: their UIDLs are forgotten.
    h.eventually(|| {
        let store = h.shared.store.clone();
        let account = h.account;
        async move { store.pop3_uidls(account).recv().await.unwrap().is_empty() }
    })
    .await;
    assert_eq!(retrieved(&h.fake.log()).len(), 2, "downloaded once");
    assert_eq!(h.fake.count("DELE u1"), 2);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mail_downloaded_n_days_ago_is_deleted_from_the_server() {
    let policy = Pop3Policy {
        leave_on_server: true,
        delete_after_days: Some(7),
    };
    let mut h = harness(policy, quick(), None, Fake::with(1..=3)).await;
    // u1 was downloaded eight days ago, u2 two days ago (as an earlier session recorded them).
    let local = h.shared.store.pop3_account(h.account).recv().await.unwrap();
    let now = unix_seconds();
    for (n, age) in [(1, 8), (2, 2)] {
        let blobs = h.shared.blobs.clone();
        let raw = RawMessage {
            folder: local.inbox,
            raw: mail(n),
            ..RawMessage::default()
        };
        let message = ingest::prepare(&blobs, vec![raw], 1)
            .unwrap()
            .messages
            .remove(0);
        h.shared
            .store
            .pop3_store(
                h.account,
                vec![Pop3Download {
                    uidl: format!("u{n}"),
                    message,
                    whole: true,
                }],
                now - age * DAY,
            )
            .recv()
            .await
            .unwrap();
    }
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    assert_eq!(retrieved(&h.fake.log()), ["u3"]);
    assert_eq!(h.fake.uidls(), ["u2", "u3"]);
    let deleted: Vec<(String, bool)> = h
        .uidls()
        .await
        .into_iter()
        .map(|uidl| (uidl.uidl, uidl.deleted.is_some()))
        .collect();
    assert_eq!(
        deleted,
        [
            ("u1".to_owned(), true),
            ("u2".to_owned(), false),
            ("u3".to_owned(), false)
        ]
    );
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_tries_again_at_once_after_the_backoff() {
    let fake = Fake::with(1..=1);
    fake.lock().down = true;
    let sync = SyncConfig {
        backoff_min: Duration::from_secs(60),
        backoff_max: Duration::from_secs(60),
        ..quick()
    };
    let mut h = harness(Pop3Policy::default(), sync, None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Offline(LoginFailure::Unreachable))
        .await;
    h.fake.lock().down = false;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        h.fake.count("CONNECT"),
        1,
        "a failed login waits out the backoff, never tried again at once"
    );
    actor.reconnect();
    h.status(SyncStatus::Synced).await;
    assert_eq!(h.fake.count("CONNECT"), 2);
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await, ["Melding 1"]);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_login_needs_sign_in_until_reconnect() {
    let fake = Fake::with(1..=1);
    fake.lock().refuse = Some(LoginFailure::WrongPassword);
    let mut h = harness(Pop3Policy::default(), quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::NeedsSignIn(LoginFailure::WrongPassword))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        h.fake.count("CONNECT"),
        1,
        "no retries while the login is refused"
    );
    h.fake.lock().refuse = None;
    actor.reconnect();
    h.status(SyncStatus::Synced).await;
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_folders_answer_older_mail_at_once() {
    let mut h = harness(Pop3Policy::default(), quick(), None, Fake::default()).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let archive = h.folder(FolderRole::Archive).await;
    actor
        .control
        .send(Control::Opened(Some(archive)))
        .await
        .unwrap();
    actor.control.send(Control::Older(archive)).await.unwrap();
    let answer = h
        .until(|event| match event {
            Event::OlderLoaded {
                folder,
                fetched,
                complete,
            } => Some((*folder, *fetched, *complete)),
            _ => None,
        })
        .await;
    assert_eq!(answer, (archive, 0, true));
    stop(actor).await;
}

/// A scripted submission server over TLS: accepts the login and every message, once per
/// connection, and keeps the messages' data.
async fn smtp(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    data: Arc<Mutex<Vec<String>>>,
) {
    loop {
        let mut peer = Peer::accept_tls(&listener, config.clone()).await;
        peer.send("220 test ESMTP\r\n").await;
        while let Some(line) = peer.line().await {
            let verb = line
                .split([' ', ':'])
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            let reply = match verb.as_str() {
                "EHLO" => "250-test\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n",
                "AUTH" => "235 2.7.0 accepted\r\n",
                "MAIL" | "RCPT" => "250 2.1.0 ok\r\n",
                "DATA" => {
                    peer.send("354 go ahead\r\n").await;
                    let mut message = String::new();
                    while let Some(line) = peer.line().await {
                        if line == "." {
                            break;
                        }
                        message.push_str(&line);
                        message.push('\n');
                    }
                    data.lock().unwrap().push(message);
                    "250 2.0.0 queued\r\n"
                }
                "QUIT" => {
                    peer.send("221 bye\r\n").await;
                    break;
                }
                _ => "250 ok\r\n",
            };
            peer.send(reply).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_goes_out_beside_a_long_download_and_its_sent_copy_is_filed_locally() {
    let ca = test_ca();
    let (listener, port) = bind().await;
    let data = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(smtp(listener, ca.server.clone(), data.clone()));
    // A first download of 20 messages, 100 ms each.
    let fake = Fake::with(1..=20);
    fake.lock().slow = Some(Duration::from_millis(100));
    let mut h = harness(
        Pop3Policy::default(),
        quick(),
        Some((port, ca.trust.clone())),
        fake,
    )
    .await;
    let draft = Draft {
        account: h.account,
        from: Address::new("Pop", "pop@example.test"),
        to: vec![Address::new("Kari", "kari@example.test")],
        subject: "Svar fra POP3".into(),
        html: "<p>Hei</p>".into(),
        message_id: "pop3-send@example.test".into(),
        ..Draft::default()
    };
    let saved = h
        .shared
        .store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: unix_ms(),
        })
        .recv()
        .await
        .unwrap();
    let sealwax_infrastructure::store::DraftSaved::Saved { id, .. } = saved else {
        panic!("not saved: {saved:?}");
    };
    let queued = h
        .shared
        .store
        .queue_send(id, unix_ms())
        .recv()
        .await
        .unwrap();
    assert!(matches!(queued, SendQueued::Queued { .. }), "{queued:?}");
    let actor = h.start();
    let account = h.account;
    h.until(|event| match event {
        Event::Sent { account: a, draft } if *a == account && *draft == id => Some(()),
        _ => None,
    })
    .await;
    assert!(
        retrieved(&h.fake.log()).len() < 20,
        "sent while the download went on"
    );
    assert_eq!(data.lock().unwrap().len(), 1, "sent once");
    assert!(data.lock().unwrap()[0].contains("Subject: Svar fra POP3"));
    let sent = h.folder(FolderRole::Sent).await;
    h.eventually(|| {
        let store = h.shared.store.clone();
        async move { store.draft(id).recv().await.unwrap().is_none() }
    })
    .await;
    assert_eq!(h.subjects(sent).await, ["Svar fra POP3"]);
    let row = &h
        .shared
        .store
        .page(ViewSpec::folder(sent), 0, 10)
        .recv()
        .await
        .unwrap()
        .rows[0];
    assert!(row.flags.contains(Flags::SEEN));
    assert!(h.shared.store.outbox(None).recv().await.unwrap().is_empty());
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await.len(), 20);
    assert_eq!(data.lock().unwrap().len(), 1);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drafts_copy_is_filed_into_the_local_drafts_folder() {
    let mut h = harness(Pop3Policy::default(), quick(), None, Fake::default()).await;
    let draft = Draft {
        account: h.account,
        from: Address::new("Pop", "pop@example.test"),
        to: vec![Address::new("Kari", "kari@example.test")],
        subject: "Utkast fra POP3".into(),
        html: "<p>Halvferdig</p>".into(),
        message_id: "pop3-draft@example.test".into(),
        ..Draft::default()
    };
    let saved = h
        .shared
        .store
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: unix_ms() - 60_000,
        })
        .recv()
        .await
        .unwrap();
    let sealwax_infrastructure::store::DraftSaved::Saved { id, .. } = saved else {
        panic!("not saved: {saved:?}");
    };
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let drafts = h.folder(FolderRole::Drafts).await;
    h.eventually(|| {
        let store = h.shared.store.clone();
        async move { store.folder_messages(drafts).recv().await.unwrap().len() == 1 }
    })
    .await;
    let rows = h
        .shared
        .store
        .page(ViewSpec::folder(drafts), 0, 10)
        .recv()
        .await
        .unwrap()
        .rows;
    assert_eq!(rows[0].subject, "Utkast fra POP3");
    assert!(rows[0].flags.contains(Flags::DRAFT));
    assert_eq!(
        h.shared
            .store
            .draft_for_message(rows[0].id)
            .recv()
            .await
            .unwrap(),
        Some(id),
        "Edit Draft finds the draft"
    );
    // Discarded, its copy goes, and its bytes with the next pass.
    let blob = h
        .shared
        .store
        .body_targets(vec![rows[0].id])
        .recv()
        .await
        .unwrap()[0]
        .blob
        .unwrap();
    assert!(h.shared.blobs.contains(&blob));
    h.shared.store.discard_draft(id).recv().await.unwrap();
    assert!(h.subjects(drafts).await.is_empty());
    h.eventually(|| {
        let blobs = h.shared.blobs.clone();
        async move { !blobs.contains(&blob) }
    })
    .await;
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mail_deleted_for_good_leaves_the_disk_after_the_next_pass() {
    let mut h = harness(Pop3Policy::default(), quick(), None, Fake::with(1..=2)).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    let ids: Vec<MessageId> = h
        .shared
        .store
        .folder_messages(inbox)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .map(|stored| stored.id)
        .collect();
    let targets = h
        .shared
        .store
        .body_targets(ids.clone())
        .recv()
        .await
        .unwrap();
    let blobs: Vec<_> = targets.iter().map(|target| target.blob.unwrap()).collect();
    for _ in 0..2 {
        h.shared
            .store
            .queue_triage(QueueTriage {
                action: Triage::Delete,
                messages: ids.clone(),
                not_before: 0,
            })
            .recv()
            .await
            .unwrap();
    }
    assert!(h.subjects(inbox).await.is_empty());
    h.eventually(|| {
        let store = h.shared.blobs.clone();
        let blobs = blobs.clone();
        async move { blobs.iter().all(|blob| !store.contains(blob)) }
    })
    .await;
    // Their UIDLs stay: the server's copies are never downloaded again.
    let lists = h.fake.count("LIST");
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.count("LIST") > lists }
    })
    .await;
    assert_eq!(retrieved(&h.fake.log()).len(), 2);
    assert!(h.uidls().await.iter().all(|uidl| uidl.message.is_none()));
    stop(actor).await;
}

/// A message above IMAP's whole-message limit (10 MiB): a text part and an 8 MiB attachment.
fn large_mail() -> Vec<u8> {
    let attachment: Vec<u8> = (0..8 * 1024 * 1024).map(|n| (n % 251) as u8).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&attachment);
    let mut raw = String::from(
        "From: Avsender <avsender@example.test>\r\nTo: pop@example.test\r\n\
         Subject: Stort vedlegg\r\nDate: Mon, 05 Oct 2026 09:00:00 +0000\r\n\
         Message-ID: <pop-large@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nSe vedlegget.\r\n--b1\r\n\
         Content-Type: application/octet-stream; name=\"data.bin\"\r\n\
         Content-Disposition: attachment; filename=\"data.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n",
    );
    for line in encoded.as_bytes().chunks(76) {
        raw.push_str(std::str::from_utf8(line).unwrap());
        raw.push_str("\r\n");
    }
    raw.push_str("--b1--\r\n");
    raw.into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_message_is_stored_whole_and_answered_from_the_store_only() {
    let raw = large_mail();
    assert!(raw.len() > 10 * 1024 * 1024);
    let fake = Fake::default();
    fake.lock().messages.push(("big".into(), raw.clone()));
    let sync = SyncConfig {
        pop3_poll: Duration::from_secs(3_600),
        ..quick()
    };
    let mut h = harness(Pop3Policy::default(), sync, None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    let id = h.shared.store.folder_messages(inbox).recv().await.unwrap()[0].id;
    let target = h.shared.store.body_targets(vec![id]).recv().await.unwrap()[0].clone();
    assert_eq!(target.body_state, BodyState::Full);
    let blob = target.blob.unwrap();
    assert_eq!(h.shared.blobs.get(&blob).unwrap(), raw, "stored whole");

    // The disk cap leaves it alone.
    bodies::set_storage(
        &h.shared,
        sealwax_domain::config::Storage {
            cache_size: Some(1),
            prefetch_days: 0,
        },
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(h.shared.blobs.contains(&blob));
    let target = h.shared.store.body_targets(vec![id]).recv().await.unwrap()[0].clone();
    assert_eq!(target.body_state, BodyState::Full);

    // Bodies, parts and originals come from the blob store: no server is asked (the account's
    // incoming port is closed, and the actor isn't one the body downloads know).
    let part = sealwax_domain::message::parse(&raw).attachments[0].part;
    bodies::request(&h.shared, vec![id], true);
    bodies::request_part(&h.shared, id, part);
    bodies::request_original(&h.shared, id);
    let mut body = None;
    let mut fetched_part = None;
    let mut original = None;
    while body.is_none() || fetched_part.is_none() || original.is_none() {
        h.until(|event| match event {
            Event::BodyFetched { message, result } if *message == id => {
                body = Some(*result);
                Some(())
            }
            Event::PartFetched {
                message, result, ..
            } if *message == id => {
                fetched_part = Some(result.clone());
                Some(())
            }
            Event::OriginalFetched { message, result } if *message == id => {
                original = Some(result.clone());
                Some(())
            }
            _ => None,
        })
        .await;
    }
    assert_eq!(body, Some(Ok(())));
    let fetched_part = fetched_part.unwrap().unwrap();
    assert_eq!(fetched_part.mime_type, "application/octet-stream");
    assert_eq!(fetched_part.data.len(), 8 * 1024 * 1024);
    assert_eq!(original.unwrap().unwrap().0, raw);

    // Without its bytes, it is gone: never fetched from a server.
    h.shared.blobs.remove(&blob).unwrap();
    bodies::request(&h.shared, vec![id], true);
    bodies::request_part(&h.shared, id, part);
    bodies::request_original(&h.shared, id);
    let mut answers = Vec::new();
    while answers.len() < 3 {
        let answer = h
            .until(|event| match event {
                Event::BodyFetched { message, result } if *message == id => Some(result.err()),
                Event::PartFetched {
                    message, result, ..
                } if *message == id => Some(result.clone().err()),
                Event::OriginalFetched { message, result } if *message == id => {
                    Some(result.clone().err())
                }
                _ => None,
            })
            .await;
        answers.push(answer);
    }
    assert_eq!(answers, [Some(BodyError::Gone); 3]);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_mail_runs_a_pass_now() {
    let sync = SyncConfig {
        pop3_poll: Duration::from_secs(3_600),
        ..quick()
    };
    let mut h = harness(Pop3Policy::default(), sync, None, Fake::with(1..=1)).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    h.fake.add(2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.fake.count("LIST"), 1, "the next pass is an hour away");
    actor.control.send(Control::CheckMail).await.unwrap();
    let account = h.account;
    let arrived = h
        .until(|event| match event {
            Event::NewMail {
                account: id,
                messages,
                ..
            } if *id == account => Some(messages.len()),
            _ => None,
        })
        .await;
    assert_eq!(arrived, 1);
    assert_eq!(h.fake.count("LIST"), 2);
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_too_large_to_download_shows_its_header_and_stays_on_the_server() {
    let policy = Pop3Policy {
        leave_on_server: false,
        delete_after_days: None,
    };
    let fake = Fake::with(1..=2);
    fake.lock().large.push("u2".into());
    let mut h = harness(policy, quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await, ["Melding 2", "Melding 1"]);
    let large = h
        .uidls()
        .await
        .into_iter()
        .find(|uidl| uidl.uidl == "u2")
        .unwrap();
    assert!(!large.whole && large.deleted.is_none());
    let target = &h
        .shared
        .store
        .body_targets(vec![large.message.unwrap()])
        .recv()
        .await
        .unwrap()[0];
    assert_eq!((target.body_state, target.blob), (BodyState::Headers, None));
    assert_eq!(target.size, mail(2).len() as u64, "the server's size");
    // Never deleted from the server, never asked for again; the other one goes.
    assert_eq!(h.fake.uidls(), ["u2"]);
    let lists = h.fake.count("LIST");
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.count("LIST") > lists }
    })
    .await;
    assert_eq!(h.fake.count("RETR u2 too large"), 1);
    assert_eq!(h.fake.count("TOP u2"), 1);
    assert_eq!(h.fake.count("DELE u2"), 0);
    // A FetchBodies for it answers from the store: gone, no server is asked.
    bodies::request(&h.shared, vec![large.message.unwrap()], true);
    let answer = h
        .until(|event| match event {
            Event::BodyFetched { message, result } if Some(*message) == large.message => {
                Some(*result)
            }
            _ => None,
        })
        .await;
    assert_eq!(answer, Err(BodyError::Gone));
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_top_a_message_too_large_is_skipped_and_the_pass_completes() {
    let fake = Fake::with(1..=2);
    {
        let mut drop = fake.lock();
        drop.large.push("u2".into());
        drop.no_top = true;
    }
    let mut h = harness(Pop3Policy::default(), quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await, ["Melding 1"]);
    let first = h.shared.store.pop3_account(h.account).recv().await.unwrap();
    assert!(
        first.first_pass.is_some(),
        "complete: nothing more can come"
    );
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_sharing_a_uidl_are_never_downloaded_nor_deleted() {
    let policy = Pop3Policy {
        leave_on_server: false,
        delete_after_days: None,
    };
    let fake = Fake::with(1..=1);
    {
        let mut drop = fake.lock();
        drop.messages.push(("dup".into(), mail(2)));
        drop.messages.push(("dup".into(), mail(3)));
    }
    let mut h = harness(policy, quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await, ["Melding 1"]);
    assert_eq!(h.fake.uidls(), ["dup", "dup"]);
    let log = h.fake.log();
    assert!(!log.iter().any(|line| line.contains("dup")), "{log:?}");
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloaded_mail_is_unread_unless_its_header_says_read() {
    let fake = Fake::with(1..=1);
    let mut read = mail(2);
    read.splice(0..0, b"Status: RO\r\n".iter().copied());
    fake.lock().messages.push(("u2".into(), read));
    let mut h = harness(Pop3Policy::default(), quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    let rows = h
        .shared
        .store
        .page(ViewSpec::folder(inbox), 0, 10)
        .recv()
        .await
        .unwrap()
        .rows;
    let seen: Vec<(String, bool)> = rows
        .into_iter()
        .map(|row| (row.subject, row.flags.contains(Flags::SEEN)))
        .collect();
    assert_eq!(
        seen,
        [
            ("Melding 2".to_owned(), true),
            ("Melding 1".to_owned(), false)
        ]
    );
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_whose_data_outgrows_the_cap_gets_its_header_at_the_next_pass() {
    let fake = Fake::with(1..=2);
    fake.lock().outgrow.push("u2".into());
    let mut h = harness(Pop3Policy::default(), quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let inbox = h.folder(FolderRole::Inbox).await;
    assert_eq!(h.subjects(inbox).await, ["Melding 2", "Melding 1"]);
    let log = h.fake.log();
    assert_eq!(h.fake.count("RETR u2 outgrew"), 1, "{log:?}");
    assert_eq!(h.fake.count("TOP u2"), 1, "in the next session");
    assert_eq!(h.fake.count("RETR u2"), 0, "never downloaded again");
    assert!(
        h.uidls()
            .await
            .iter()
            .any(|uidl| uidl.uidl == "u2" && !uidl.whole)
    );
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_maildrop_waits_its_login_delay_and_keeps_the_status() {
    let fake = Fake::with(1..=1);
    fake.lock().busy = vec![
        Busy::InUse,
        Busy::LoginDelay(Some(Duration::from_millis(700))),
    ];
    let mut h = harness(Pop3Policy::default(), quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    let logins = h.fake.lock().logins.clone();
    assert_eq!(logins.len(), 3);
    assert!(
        logins[2] - logins[1] >= Duration::from_millis(700),
        "no login before the LOGIN-DELAY: {:?}",
        logins[2] - logins[1]
    );
    let statuses: Vec<SyncStatus> = h
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::AccountStatus { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        [SyncStatus::Syncing, SyncStatus::Synced],
        "busy isn't offline"
    );
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn polls_are_spaced_by_the_servers_login_delay() {
    let fake = Fake::with(1..=1);
    fake.lock().offers = Offers {
        login_delay: Some(Duration::from_millis(900)),
        expire: Some(Expire::Days(0)),
    };
    let mut h = harness(Pop3Policy::default(), quick(), None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.lock().logins.len() >= 2 }
    })
    .await;
    let logins = h.fake.lock().logins.clone();
    assert!(
        logins[1] - logins[0] >= Duration::from_millis(900),
        "the poll waits out LOGIN-DELAY (poll 200 ms): {:?}",
        logins[1] - logins[0]
    );
    stop(actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_and_reconnect_wait_out_the_login_delay() {
    let fake = Fake::with(1..=1);
    fake.lock().offers.login_delay = Some(Duration::from_millis(800));
    let sync = SyncConfig {
        pop3_poll: Duration::from_secs(3_600),
        ..quick()
    };
    let mut h = harness(Pop3Policy::default(), sync, None, fake).await;
    let actor = h.start();
    h.status(SyncStatus::Synced).await;
    actor.control.send(Control::CheckMail).await.unwrap();
    actor.reconnect();
    h.eventually(|| {
        let fake = h.fake.clone();
        async move { fake.lock().logins.len() >= 2 }
    })
    .await;
    let logins = h.fake.lock().logins.clone();
    assert!(
        logins[1] - logins[0] >= Duration::from_millis(800),
        "{:?}",
        logins[1] - logins[0]
    );
    stop(actor).await;
}
