//! New mail reported to the app (ML-083) through the engine service, against the fake IMAP server
//! (CI, no network beyond 127.0.0.1): `Event::NewMail` with exactly the rows that arrived, after
//! their commit, through IDLE and through polling, in batches past 200, from any synced folder;
//! nothing for a first sync, a UIDVALIDITY reset, older mail, mail stored before a restart (a new
//! connection on the same store), or the account's own moves into a folder (re-homed, or found
//! by the target's sync after a cut). The account's rules run on new mail first (ADR-020).
//! Synthetic data only.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod};
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_domain::sync::Window;
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Event, Password, RulesError, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::{self, Arrived, FolderLocks, SyncOptions};
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{NewAccount, QueueTriage, Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WAIT: Duration = Duration::from_secs(20);

/// Account passes and IDLE restarts rare; the opened folder and a server without IDLE polled
/// every 300 ms.
fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        },
        account_pass: Duration::from_secs(60),
        inbox_poll: Duration::from_millis(300),
        opened_poll: Duration::from_millis(300),
        idle_restart: Duration::from_secs(60),
        reuse_check: Duration::from_secs(60),
        noop_timeout: Duration::from_secs(2),
        backoff_min: Duration::from_millis(100),
        backoff_max: Duration::from_millis(800),
        logout_wait: Duration::from_millis(300),
        ..SyncConfig::default()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

struct Running {
    dir: tempfile::TempDir,
    trust: Trust,
    engine: Option<Engine>,
    store: Arc<Store>,
    blobs: BlobStore,
    events: async_channel::Receiver<Event>,
    /// Events read while looking for others, oldest first.
    seen: Vec<Event>,
}

impl Running {
    fn start(fake: &Fake, sync: SyncConfig) -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("newmail-")
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
        let mut running = Running {
            dir,
            trust: fake.trust.clone(),
            engine: None,
            store,
            blobs,
            events: async_channel::unbounded().1,
            seen: Vec::new(),
        };
        running.engine(sync);
        running
    }

    /// A new engine on the same store, blobs and `accounts.toml`.
    fn engine(&mut self, sync: SyncConfig) {
        let dirs = SealwaxDirs::under(self.dir.path());
        let engine = Engine::start(EngineConfig {
            application: "io.github.pixdevsapps.Sealwax.Test".into(),
            store: self.store.clone(),
            blobs: self.blobs.clone(),
            paths: AccountPaths::new(&dirs),
            trust: self.trust.clone(),
            secrets: SecretBackend::Memory,
            detect: DetectOptions::default(),
            microsoft_client_id: None,
            sign_in: SignInOptions::default(),
            sync,
        })
        .unwrap();
        self.events = engine.events();
        self.seen.clear();
        self.engine = Some(engine);
    }

    fn stop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
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

    /// The next event `pick` accepts within `within`; every event read is kept in `seen`.
    async fn event<T>(&mut self, within: Duration, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = Instant::now() + within;
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

    /// Reads every event for `quiet`, into `seen`.
    async fn drain(&mut self, quiet: Duration) {
        let deadline = Instant::now() + quiet;
        while let Ok(Ok(event)) =
            tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
                self.events.recv().await
            })
            .await
        {
            self.seen.push(event);
        }
    }

    async fn status(&mut self, account: AccountId) -> SyncStatus {
        self.event(WAIT, |event| match event {
            Event::AccountStatus {
                account: id,
                status,
            } if *id == account && *status != SyncStatus::Syncing => Some(*status),
            _ => None,
        })
        .await
    }

    /// Adds an account on `fake` and waits for its first "synced".
    async fn add(&mut self, fake: &Fake) -> AccountId {
        let settings = AccountSettings {
            key: AccountKey(String::new()),
            name: "Ny post".into(),
            label: None,
            color: None,
            address: "newmail@example.test".into(),
            incoming: fake.endpoint(),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: fake.endpoint(),
            username: "newmail@example.test".into(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        };
        self.send(Command::AddAccount {
            request: 1,
            settings,
            password: Password::new("fake-password".into()),
            sign_in: None,
        });
        let account = self
            .event(WAIT, |event| match event {
                Event::AccountAdded { request: 1, result } => Some(result.clone().unwrap()),
                _ => None,
            })
            .await;
        assert_eq!(self.status(account).await, SyncStatus::Synced);
        account
    }

    async fn folder(&self, account: AccountId, name: &str) -> FolderId {
        self.store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|state| state.name_raw == name)
            .unwrap_or_else(|| panic!("{name} not stored"))
            .folder
    }

    /// The stored rows of `folder` from UID `from` up.
    async fn rows_from(&self, folder: FolderId, from: u32) -> BTreeSet<MessageId> {
        self.store
            .folder_messages(folder)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .filter(|message| message.uid >= from)
            .map(|message| message.id)
            .collect()
    }

    async fn count(&self, folder: FolderId) -> u32 {
        self.store
            .folder_sync(folder)
            .recv()
            .await
            .unwrap()
            .unwrap()
            .messages
    }

    /// The messages every `NewMail` read so far named, by folder.
    fn announced(&self, folder: FolderId) -> Vec<MessageId> {
        self.seen
            .iter()
            .filter_map(|event| match event {
                Event::NewMail {
                    folder: found,
                    messages,
                    ..
                } if *found == folder => Some(messages.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn new_mail_events(&self) -> usize {
        self.seen
            .iter()
            .filter(|event| matches!(event, Event::NewMail { .. }))
            .count()
    }

    /// Waits until the `NewMail` events read name `wanted` messages of `folder`.
    async fn announced_count(&mut self, folder: FolderId, wanted: usize) -> Vec<MessageId> {
        let deadline = Instant::now() + WAIT;
        while self.announced(folder).len() < wanted && Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Ok(Ok(event)) = tokio::time::timeout(left, self.events.recv()).await {
                self.seen.push(event);
            }
        }
        self.announced(folder)
    }

    async fn queue(&self, action: Triage, messages: Vec<MessageId>) {
        let queued = self
            .store
            .queue_triage(QueueTriage {
                action,
                messages,
                not_before: now_ms(),
            })
            .recv()
            .await
            .unwrap();
        for account in &queued.accounts {
            self.send(Command::OpsQueued { account: *account });
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn within<F: Future<Output = bool>>(
    within: Duration,
    mut check: impl FnMut() -> F,
) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < within {
        if check().await {
            return Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// INBOX with 20 messages, Sendt (`\Sent`) with 2 and Arkiv (`\Archive`) with 3.
async fn fake(flavor: Flavor) -> Fake {
    let fake = Fake::start(flavor).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.create("Sendt", &["\\Sent"]);
        world.create("Arkiv", &["\\Archive"]);
        world.append_many("INBOX", 20, |_| 1);
        world.append_many("Sendt", 2, |_| 1);
        world.append_many("Arkiv", 3, |_| 1);
    }
    fake
}

async fn idling(fake: &Fake, flavor: Flavor) {
    if flavor != Flavor::Rev1 {
        assert!(
            within(WAIT, || async { fake.world().count("IDLE") >= 1 })
                .await
                .is_some(),
            "the IDLE connection idles"
        );
    }
}

/// Three messages delivered at once to INBOX: one or more `NewMail` events after their commit
/// that name exactly the rows above the UIDNEXT the store held, none for the first sync.
async fn new_mail_in(flavor: Flavor) {
    let fake = fake(flavor).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    run.drain(Duration::from_millis(300)).await;
    assert_eq!(
        run.new_mail_events(),
        0,
        "{flavor:?}: none for the first sync"
    );
    idling(&fake, flavor).await;
    let started = Instant::now();
    {
        let mut world = fake.world();
        world.append("INBOX", 0, &[]);
        world.append("INBOX", 0, &["\\Seen"]);
        world.append("INBOX", 0, &[]);
    }
    let announced = run.announced_count(inbox, 3).await;
    let latency = started.elapsed();
    let stored = run.rows_from(inbox, 21).await;
    assert_eq!(stored.len(), 3, "{flavor:?}");
    for id in &announced {
        let detail = run.store.message(*id).recv().await.unwrap().value;
        assert!(
            detail.is_some(),
            "{flavor:?}: a NewMail row is committed first"
        );
    }
    assert_eq!(
        announced.iter().copied().collect::<BTreeSet<_>>(),
        stored,
        "{flavor:?}: exactly the new rows, read ones too"
    );
    assert_eq!(announced.len(), 3, "{flavor:?}: each once");
    println!(
        "{flavor:?}: NewMail for 3 messages {latency:?} after they arrived, in {} event(s)",
        run.new_mail_events()
    );
    let account_of = run.seen.iter().find_map(|event| match event {
        Event::NewMail { account, .. } => Some(*account),
        _ => None,
    });
    assert_eq!(account_of, Some(account));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_mail_through_idle_names_exactly_the_new_rows() {
    new_mail_in(Flavor::Qresync).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_mail_through_idle_without_qresync() {
    new_mail_in(Flavor::Condstore).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_mail_polled_on_an_imap4rev1_server() {
    new_mail_in(Flavor::Rev1).await;
}

/// More than a batch at once: SEARCH and batches of 200, each committed and announced on its
/// own, together exactly the new rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_past_a_batch_arrives_in_several_events() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    idling(&fake, Flavor::Qresync).await;
    fake.world().append_many("INBOX", 250, |_| 0);
    let announced = run.announced_count(inbox, 250).await;
    assert_eq!(
        announced.iter().copied().collect::<BTreeSet<_>>(),
        run.rows_from(inbox, 21).await
    );
    assert_eq!(announced.len(), 250, "each once");
    assert!(
        run.new_mail_events() >= 2,
        "one event per committed batch: {}",
        run.new_mail_events()
    );
}

/// Any synced folder reports its new mail; the app decides what notifies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_mail_in_another_synced_folder_is_reported_with_it() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let sent = run.folder(account, "Sendt").await;
    run.send(Command::FolderOpened { folder: sent });
    tokio::time::sleep(Duration::from_millis(500)).await;
    fake.world().append("Sendt", 0, &["\\Seen"]);
    let announced = run.announced_count(sent, 1).await;
    assert_eq!(
        announced.into_iter().collect::<BTreeSet<_>>(),
        run.rows_from(sent, 3).await
    );
}

/// INBOX recreated while idling: the new UIDVALIDITY's first sync stores its messages, and a
/// message that arrives during it (stored by the same pass after the first sync's batches);
/// none of them is new mail. A message after that pass is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_uidvalidity_reset_is_not_new_mail() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    idling(&fake, Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.recreate("INBOX");
        world.append_many("INBOX", 5, |_| 0);
        world.on("UID FETCH", |world| {
            world.append("INBOX", 0, &[]);
        });
    }
    assert!(
        within(WAIT, || async {
            run.store.folder_messages(inbox).recv().await.unwrap().len() == 6
        })
        .await
        .is_some()
    );
    run.drain(Duration::from_millis(500)).await;
    assert_eq!(
        run.new_mail_events(),
        0,
        "the reset's first sync is no new mail"
    );
    assert!(
        within(WAIT, || async { fake.world().count("IDLE") >= 2 })
            .await
            .is_some()
    );
    let uid = fake.world().append("INBOX", 0, &[]);
    let announced = run.announced_count(inbox, 1).await;
    assert_eq!(
        announced.into_iter().collect::<BTreeSet<_>>(),
        run.rows_from(inbox, uid).await
    );
}

/// Older mail below the window's floor is no new mail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_mail_is_not_new_mail() {
    let fake = Fake::start(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.append_many("INBOX", 60, |n| if n < 40 { 200 } else { 1 });
    }
    let mut sync = config();
    sync.options.window = Window {
        days: 90,
        max: 30,
        min: 10,
    };
    sync.options.older = 15;
    let mut run = Running::start(&fake, sync);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    assert_eq!(run.count(inbox).await, 20);
    for _ in 0..3 {
        run.send(Command::LoadOlder { folder: inbox });
        let fetched = run
            .event(WAIT, |event| match event {
                Event::OlderLoaded {
                    folder, fetched, ..
                } if *folder == inbox => Some(*fetched),
                _ => None,
            })
            .await;
        assert!(fetched > 0);
    }
    run.drain(Duration::from_millis(300)).await;
    assert_eq!(run.count(inbox).await, 60);
    assert_eq!(run.new_mail_events(), 0, "older mail is no new mail");
}

/// What a restart sees: a new connection on the same store. The mail stored before is no new
/// mail on any server flavour; mail that arrived in between is, step by step, in the report and
/// in the sink alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_restart_only_what_arrived_in_between_is_new_mail() {
    for flavor in Flavor::ALL {
        let fake = fake(flavor).await;
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("newmail-sync-")
            .tempdir_in(base)
            .unwrap();
        let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
        let account = store
            .add_account(NewAccount {
                name: "Ny post".into(),
                address: "newmail@example.test".into(),
            })
            .recv()
            .await
            .unwrap()
            .value;
        let (sink, arrivals) = async_channel::unbounded();
        let options = SyncOptions {
            now: Some(NOW),
            arrivals: Some(sink),
            ..SyncOptions::default()
        };
        let locks = FolderLocks::default();
        let pass = || async {
            let mut conn = fake.connect().await;
            let report =
                imap::sync_account(&mut conn, &store, account, &options, &locks, |_, _| {})
                    .await
                    .unwrap();
            conn.logout_within(Duration::from_millis(300)).await;
            report
        };
        let first = pass().await;
        assert!(first.stats.first_sync_batches > 0, "{flavor:?}");
        assert_eq!(first.arrived, Vec::new(), "{flavor:?}: the first sync");
        let again = pass().await;
        assert_eq!(
            again.arrived,
            Vec::new(),
            "{flavor:?}: stored before the restart"
        );
        let uid = {
            let mut world = fake.world();
            let uid = world.append("INBOX", 0, &[]);
            world.append("INBOX", 0, &["\\Seen"]);
            uid
        };
        let report = pass().await;
        let inbox = store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .find(|state| state.name_raw == "INBOX")
            .unwrap()
            .folder;
        let stored: BTreeSet<MessageId> = store
            .folder_messages(inbox)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .filter(|message| message.uid >= uid)
            .map(|message| message.id)
            .collect();
        assert_eq!(stored.len(), 2, "{flavor:?}");
        let reported: Vec<Arrived> = report.arrived.clone();
        assert!(
            reported.iter().all(|arrived| arrived.folder == inbox),
            "{flavor:?}"
        );
        assert_eq!(
            reported
                .iter()
                .flat_map(|arrived| arrived.messages.iter().copied())
                .collect::<BTreeSet<_>>(),
            stored,
            "{flavor:?}: what arrived in between"
        );
        let mut sent = Vec::new();
        while let Ok(arrived) = arrivals.try_recv() {
            sent.push(arrived);
        }
        assert_eq!(
            sent, reported,
            "{flavor:?}: the sink had each step as it committed"
        );
    }
}

/// The account's own move into INBOX: re-homed by COPYUID, or by the Message-ID found above the
/// target's mark without it, it is held already and never announced; a message after it is.
async fn own_move_into_inbox(copyuid: fake::CopyUid) {
    let fake = fake(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.moves = true;
        world.copyuid = copyuid;
    }
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let archive = run.folder(account, "Arkiv").await;
    idling(&fake, Flavor::Qresync).await;
    let moving: Vec<MessageId> = run
        .rows_from(archive, 1)
        .await
        .into_iter()
        .take(2)
        .collect();
    run.queue(Triage::MoveTo(inbox), moving.clone()).await;
    assert!(
        within(WAIT, || async {
            run.store.pending_ops(account).recv().await.unwrap() == 0
                && run.count(inbox).await == 22
        })
        .await
        .is_some(),
        "{copyuid:?}: moved"
    );
    run.drain(Duration::from_millis(800)).await;
    assert_eq!(run.announced(inbox), Vec::new(), "{copyuid:?}: no new mail");
    let uid = fake.world().append("INBOX", 0, &[]);
    let announced = run.announced_count(inbox, 1).await;
    assert_eq!(
        announced.into_iter().collect::<BTreeSet<_>>(),
        run.rows_from(inbox, uid).await,
        "{copyuid:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_own_move_rehomed_by_copyuid_is_not_new_mail() {
    own_move_into_inbox(fake::CopyUid::Untagged).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_own_move_rehomed_by_message_id_is_not_new_mail() {
    own_move_into_inbox(fake::CopyUid::Absent).await;
}

/// The move's reply is lost after the server moved: INBOX's IDLE pass stores the copy as a new
/// row before the retry finds it, and the account's record of its moves keeps it out of the new
/// mail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_own_move_found_by_the_targets_sync_is_not_new_mail() {
    let fake = fake(Flavor::Qresync).await;
    fake.world().moves = true;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let archive = run.folder(account, "Arkiv").await;
    idling(&fake, Flavor::Qresync).await;
    let moving: Vec<MessageId> = run
        .rows_from(archive, 1)
        .await
        .into_iter()
        .take(1)
        .collect();
    {
        let mut world = fake.world();
        world.lose_reply("UID MOVE");
        // The command connection can't come back until the IDLE pass has stored the copy.
        world.on("UID MOVE", |world| world.refuse_connections = true);
    }
    run.queue(Triage::MoveTo(inbox), moving).await;
    assert!(
        within(WAIT, || async { run.count(inbox).await == 21 })
            .await
            .is_some(),
        "the IDLE pass stored the moved copy"
    );
    assert_eq!(run.store.pending_ops(account).recv().await.unwrap(), 1);
    fake.world().refuse_connections = false;
    assert!(
        within(WAIT, || async {
            run.store.pending_ops(account).recv().await.unwrap() == 0
        })
        .await
        .is_some(),
        "the retry recorded it"
    );
    assert_eq!(run.count(inbox).await, 21);
    run.drain(Duration::from_millis(800)).await;
    assert_eq!(run.announced(inbox), Vec::new(), "no new mail");
    let uid = fake.world().append("INBOX", 0, &[]);
    let announced = run.announced_count(inbox, 1).await;
    assert_eq!(
        announced.into_iter().collect::<BTreeSet<_>>(),
        run.rows_from(inbox, uid).await
    );
}

const RULES: &str = "require [\"fileinto\", \"imap4flags\"];
# rule:[Kunder]
if header :is \"subject\" \"Fake message 21\" { addflag \"\\\\Flagged\"; fileinto \"Kunder\"; stop; }
# rule:[Spam]
if header :is \"x-spam-flag\" \"YES\" { addflag \"$Junk\"; fileinto \"Junk\"; stop; }
";

/// The flags of a fake mailbox's messages, by UID.
fn server(fake: &Fake, name: &str) -> Vec<(u32, Vec<String>)> {
    fake.world().mailboxes[name]
        .messages
        .iter()
        .map(|(uid, message)| (*uid, message.flags.clone()))
        .collect()
}

/// The account's rules run on new mail before it is announced: a rule's move and flag reach the
/// server, mail a rule sends to Junk isn't new mail, a header the store doesn't keep is fetched
/// for its test; a file that can't run is reported once and runs nothing; Apply Rules runs the
/// rules on a folder's messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rules_run_on_new_mail_before_it_is_announced() {
    let fake = fake(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.create("Junk", &["\\Junk"]);
        world.create("Kunder", &[]);
    }
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let key = run
        .store
        .accounts()
        .recv()
        .await
        .unwrap()
        .value
        .into_iter()
        .find(|found| found.id == account)
        .and_then(|found| found.key)
        .unwrap();
    let rules = SealwaxDirs::under(run.dir.path()).rules_dir();
    sealwax_infrastructure::rules::write(&rules, &key, RULES).unwrap();
    idling(&fake, Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.append("INBOX", 0, &[]);
        world.append_extra("INBOX", "X-Spam-Flag: YES\r\n");
        world.append("INBOX", 0, &[]);
    }
    let moved = within(WAIT, || async {
        fake.world().mailboxes["Kunder"].messages.len() == 1
            && fake.world().mailboxes["Junk"].messages.len() == 1
    })
    .await;
    assert!(moved.is_some(), "the rules' moves reach the server");
    assert_eq!(server(&fake, "Kunder")[0].1, ["\\Flagged"]);
    assert_eq!(server(&fake, "Junk")[0].1, ["$Junk"]);
    let inbox_uids: Vec<u32> = server(&fake, "INBOX").iter().map(|(uid, _)| *uid).collect();
    assert_eq!(inbox_uids, (1..=20).chain([23]).collect::<Vec<_>>());
    let announced = run.announced_count(inbox, 2).await;
    run.drain(Duration::from_millis(500)).await;
    assert_eq!(
        run.announced(inbox).len(),
        2,
        "the message the rules sent to Junk isn't new mail"
    );
    assert_eq!(announced.len(), 2);
    assert!(
        fake.world().count("BODY.PEEK[HEADER]") >= 1,
        "X-Spam-Flag isn't stored: the headers were fetched"
    );

    sealwax_infrastructure::rules::write(&rules, &key, "if foo {").unwrap();
    fake.world().append("INBOX", 0, &[]);
    let error = run
        .event(WAIT, |event| match event {
            Event::RulesFailed { account: id, error } if *id == account => Some(error.clone()),
            _ => None,
        })
        .await;
    assert!(matches!(error, RulesError::Script(_)), "{error:?}");
    run.announced_count(inbox, 3).await;
    fake.world().append("INBOX", 0, &[]);
    run.announced_count(inbox, 4).await;
    run.drain(Duration::from_millis(300)).await;
    let reported = run
        .seen
        .iter()
        .filter(|event| matches!(event, Event::RulesFailed { .. }))
        .count();
    assert_eq!(reported, 1, "once per change of the file");
    assert_eq!(server(&fake, "INBOX").len(), 23, "nothing ran");

    sealwax_infrastructure::rules::write(
        &rules,
        &key,
        "require \"fileinto\"; if header :is \"subject\" \"Fake message 1\" { fileinto \"Kunder\"; }",
    )
    .unwrap();
    run.send(Command::ApplyRules {
        request: 7,
        folder: inbox,
    });
    let applied = run
        .event(WAIT, |event| match event {
            Event::RulesApplied {
                request: 7,
                folder,
                result,
            } if *folder == inbox => Some(result.clone()),
            _ => None,
        })
        .await;
    assert_eq!(applied, Ok(1));
    let moved = within(WAIT, || async {
        fake.world().mailboxes["Kunder"].messages.len() == 2
    })
    .await;
    assert!(moved.is_some(), "Apply Rules moved message 1");
    run.stop();
}
