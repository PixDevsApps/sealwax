//! The op runner on the per-account actor (ML-051, M4 phase B) through the engine service, against
//! the fake IMAP server (CI, no network beyond 127.0.0.1): queued triage reaching the server when
//! the runner wakes, the undo window's timer, a retry after a cut and after `Reconnect`
//! interrupts a write, a refusal reaching the app as `OpsRefused`, a full mailbox, ops finding
//! their messages again after a UIDVALIDITY change, an Undo within the window never reaching
//! the server, ops waiting while the account is offline, and a folder marked read below its
//! window's floor. Synthetic data only.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod};
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_domain::sync::Window;
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Event, Password, Refused, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{QueueTriage, Queued, Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WAIT: Duration = Duration::from_secs(20);

/// Account passes and IDLE restarts rare, so what a test sees comes from the runner; backoff
/// small, so retries come quickly.
fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        },
        account_pass: Duration::from_secs(60),
        inbox_poll: Duration::from_secs(60),
        opened_poll: Duration::from_secs(60),
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
    _dir: tempfile::TempDir,
    engine: Option<Engine>,
    store: Arc<Store>,
    events: async_channel::Receiver<Event>,
}

impl Running {
    fn start(fake: &Fake, sync: SyncConfig) -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("ops-")
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
            sync,
        })
        .unwrap();
        let events = engine.events();
        Running {
            _dir: dir,
            engine: Some(engine),
            store,
            events,
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

    async fn event<T>(&mut self, within: Duration, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = tokio::time::timeout(left, self.events.recv())
                .await
                .expect("no such event in time")
                .expect("the engine stopped");
            if let Some(found) = pick(&event) {
                return found;
            }
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

    async fn add(&mut self, fake: &Fake) -> AccountId {
        let settings = AccountSettings {
            key: AccountKey(String::new()),
            name: "ops".into(),
            address: "ops@example.test".into(),
            imap: fake.endpoint(),
            smtp: fake.endpoint(),
            username: "ops@example.test".into(),
            auth: AuthMethod::Password,
        };
        self.send(Command::AddAccount {
            request: 1,
            settings,
            password: Password::new("fake-password".into()),
            sign_in: None,
        });
        let account = self
            .event(WAIT, |event| match event {
                Event::AccountAdded { result, .. } => Some(result.clone().unwrap()),
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

    /// The store's ids of the messages with these UIDs in `folder`.
    async fn ids(&self, folder: FolderId, uids: &[u32]) -> Vec<MessageId> {
        let stored = self.store.folder_messages(folder).recv().await.unwrap();
        uids.iter()
            .map(|uid| {
                stored
                    .iter()
                    .find(|message| message.uid == *uid)
                    .unwrap_or_else(|| panic!("UID {uid} not stored"))
                    .id
            })
            .collect()
    }

    /// Queues `action` with an undo window of `window_ms` and wakes the runner, as the app does.
    async fn queue(&self, action: Triage, messages: Vec<MessageId>, window_ms: i64) -> Queued {
        let queued = self
            .store
            .queue_triage(QueueTriage {
                action,
                messages,
                not_before: now_ms() + window_ms,
            })
            .recv()
            .await
            .unwrap();
        for account in &queued.accounts {
            self.send(Command::OpsQueued { account: *account });
        }
        queued
    }

    async fn pending(&self, account: AccountId) -> u32 {
        self.store.pending_ops(account).recv().await.unwrap()
    }

    async fn check(&self) {
        let problems = self.store.check().recv().await.unwrap();
        assert!(problems.is_empty(), "store check: {problems:?}");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// INBOX with 12 messages, Arkiv (`\Archive`) with 4, Papirkurv (`\Trash`) and Søppelpost
/// (`\Junk`); MOVE as given.
async fn fake(flavor: Flavor, moves: bool) -> Fake {
    let fake = Fake::start(flavor).await;
    {
        let mut world = fake.world();
        world.moves = moves;
        world.create("INBOX", &[]);
        world.create("Arkiv", &["\\Archive"]);
        world.create("Papirkurv", &["\\Trash"]);
        world.create("S&APg-ppelpost", &["\\Junk"]);
        world.append_many("INBOX", 12, |_| 1);
        world.append_many("Arkiv", 4, |_| 1);
    }
    fake
}

fn has(fake: &Fake, folder: &str, uid: u32, flag: &str) -> bool {
    fake.world().mailboxes[folder]
        .messages
        .get(&uid)
        .is_some_and(|message| message.flags.iter().any(|f| f == flag))
}

/// The Message-IDs of `uids` in `folder` on the fake server.
fn message_ids(fake: &Fake, folder: &str, uids: &[u32]) -> Vec<String> {
    let world = fake.world();
    uids.iter()
        .map(|uid| world.mailboxes[folder].messages[uid].message_id.clone())
        .collect()
}

fn copies(fake: &Fake, folder: &str, id: &str) -> usize {
    fake.world()
        .ids(folder)
        .iter()
        .filter(|found| found.as_str() == id)
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_flags_reach_the_server_when_the_runner_wakes() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let messages = run.ids(inbox, &[1, 2, 3]).await;
    let started = Instant::now();
    run.queue(Triage::Read(true), messages, 0).await;
    let done = within(WAIT, || async {
        (1..=3).all(|uid| has(&fake, "INBOX", uid, "\\Seen")) && run.pending(account).await == 0
    })
    .await;
    assert!(done.is_some(), "the flags reach the server");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(!has(&fake, "INBOX", 4, "\\Seen"));
    assert!(
        fake.world()
            .commands
            .iter()
            .any(|c| c == "UID STORE 1:3 +FLAGS.SILENT (\\Seen)")
    );
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_waits_for_its_undo_window_and_runs_when_it_ends() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let messages = run.ids(inbox, &[4]).await;
    let queued_at = Instant::now();
    run.queue(Triage::Archive, messages, 1_500).await;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(
        fake.world().count("UID MOVE"),
        0,
        "nothing before the window ends"
    );
    let moved = within(WAIT, || async { fake.world().count("UID MOVE") == 1 }).await;
    assert!(moved.is_some());
    let at = queued_at.elapsed();
    println!("the move reached the server {at:?} after it was queued with a 1.5 s window");
    assert!(at >= Duration::from_millis(1_500) && at < Duration::from_millis(3_000));
    within(WAIT, || async { run.pending(account).await == 0 }).await;
    assert!(!fake.world().uids("INBOX").contains(&4));
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_cut_after_the_server_did_it_is_retried_once() {
    for (flavor, moves, needle) in [
        (Flavor::Qresync, true, "UID MOVE"),
        (Flavor::Condstore, false, "UID COPY"),
        (Flavor::Rev1, false, "UID COPY"),
    ] {
        let fake = fake(flavor, moves).await;
        let mut run = Running::start(&fake, config());
        let account = run.add(&fake).await;
        let inbox = run.folder(account, "INBOX").await;
        let archive = run.folder(account, "Arkiv").await;
        let ids = message_ids(&fake, "INBOX", &[5, 6]);
        let messages = run.ids(inbox, &[5, 6]).await;
        fake.world().lose_reply(needle);
        run.queue(Triage::Archive, messages.clone(), 0).await;
        let done = within(WAIT, || async { run.pending(account).await == 0 }).await;
        assert!(done.is_some(), "{flavor:?}: the retry finishes");
        assert_eq!(fake.world().cuts, 1, "{flavor:?}");
        for id in &ids {
            assert_eq!(
                copies(&fake, "Arkiv", id),
                1,
                "{flavor:?}: one copy of {id}"
            );
        }
        let inbox_ids = fake.world().ids("INBOX");
        for (uid, id) in [5, 6].iter().zip(&ids) {
            let left = inbox_ids.contains(id);
            if flavor == Flavor::Rev1 {
                assert!(left && has(&fake, "INBOX", *uid, "\\Deleted"), "{flavor:?}");
            } else {
                assert!(!left, "{flavor:?}");
            }
        }
        // The store follows: the rows are re-homed when the runner records the move first, or
        // fetched by the target's pass when the IDLE connection's pass saw the source's
        // expunge first and counted the move done.
        for (folder, name) in [(archive, "Arkiv"), (inbox, "INBOX")] {
            let equal = within(WAIT, || async {
                let stored: Vec<u32> = run
                    .store
                    .folder_messages(folder)
                    .recv()
                    .await
                    .unwrap()
                    .iter()
                    .map(|message| message.uid)
                    .collect();
                stored == fake.world().uids(name)
            })
            .await;
            assert!(equal.is_some(), "{flavor:?}: {name} as the server has it");
        }
        run.check().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_interrupts_a_held_op_and_the_retry_finds_it_done() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let ids = message_ids(&fake, "INBOX", &[7, 8]);
    let messages = run.ids(inbox, &[7, 8]).await;
    fake.world().hold("UID MOVE");
    run.queue(Triage::Archive, messages, 0).await;
    assert!(
        within(WAIT, || async { fake.world().cuts == 1 })
            .await
            .is_some()
    );
    assert_eq!(run.pending(account).await, 2, "started, not recorded");
    run.send(Command::Reconnect);
    let done = within(WAIT, || async { run.pending(account).await == 0 }).await;
    assert!(done.is_some(), "the retry records it");
    for id in &ids {
        assert_eq!(copies(&fake, "Arkiv", id), 1);
    }
    assert_eq!(
        fake.world().count("UID MOVE"),
        1,
        "the retry found the move done"
    );
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_move_puts_the_messages_back_and_says_so() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let messages = run.ids(inbox, &[9, 10]).await;
    let shown = |run: &Running| {
        let store = run.store.clone();
        async move {
            store
                .folders()
                .recv()
                .await
                .unwrap()
                .value
                .into_iter()
                .find(|folder| folder.id == inbox)
                .unwrap()
                .total
        }
    };
    let before = shown(&run).await;
    fake.world()
        .refuse("UID MOVE", "NO [CANNOT] Not with this folder");
    run.queue(Triage::Archive, messages, 0).await;
    let refused = run
        .event(WAIT, |event| match event {
            Event::OpsRefused {
                account: id,
                change,
                messages,
            } if *id == account => Some((*change, *messages)),
            _ => None,
        })
        .await;
    assert_eq!(refused, (Refused::Move, 2));
    assert_eq!(run.pending(account).await, 0);
    assert_eq!(
        shown(&run).await,
        before,
        "the messages are back in the Inbox"
    );
    assert_eq!(fake.world().uids("INBOX").len(), 12);
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_mailbox_keeps_the_ops_and_shows_quota_until_they_get_through() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let messages = run.ids(inbox, &[11]).await;
    fake.world()
        .refuse("UID MOVE", "NO [OVERQUOTA] Quota exceeded");
    run.queue(Triage::Archive, messages, 0).await;
    assert_eq!(run.status(account).await, SyncStatus::QuotaFull);
    assert_eq!(
        run.status(account).await,
        SyncStatus::Synced,
        "after the retry got through"
    );
    assert_eq!(run.pending(account).await, 0);
    assert_eq!(fake.world().count("UID MOVE"), 2);
    assert!(!fake.world().uids("INBOX").contains(&11));
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ops_find_their_messages_again_after_a_uidvalidity_change() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let archive = run.folder(account, "Arkiv").await;
    let ids = message_ids(&fake, "Arkiv", &[1, 2, 3, 4]);
    let messages = run.ids(archive, &[1, 2]).await;
    run.queue(Triage::Star(true), messages, 800).await;
    {
        // Made again with the same messages in the other order: UIDs 1 and 2 are now the
        // messages that were 4 and 3.
        let mut world = fake.world();
        world.recreate("Arkiv");
        for id in ids.iter().rev() {
            world.append_id("Arkiv", id, 1, &[]);
        }
    }
    let done = within(WAIT, || async { run.pending(account).await == 0 }).await;
    assert!(done.is_some(), "the ops ran on the re-targeted messages");
    let flagged: Vec<String> = fake.world().mailboxes["Arkiv"]
        .messages
        .values()
        .filter(|message| message.flags.iter().any(|f| f == "\\Flagged"))
        .map(|message| message.message_id.clone())
        .collect();
    assert_eq!(flagged, [ids[1].clone(), ids[0].clone()]);
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_undo_within_the_window_never_reaches_the_server() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let messages = run.ids(inbox, &[12]).await;
    let from = fake.world().commands.len();
    let queued = run.queue(Triage::Delete, messages, 1_000).await;
    let undone = run.store.undo(queued.batch.unwrap()).recv().await.unwrap();
    assert_eq!((undone.restored, undone.reverted), (1, 0));
    tokio::time::sleep(Duration::from_millis(1_800)).await;
    let sent: Vec<String> = fake.world().commands[from..].to_vec();
    assert!(
        sent.iter()
            .all(|c| !c.starts_with("UID MOVE") && !c.starts_with("UID STORE")),
        "{sent:?}"
    );
    assert_eq!(run.pending(account).await, 0);
    assert!(fake.world().uids("INBOX").contains(&12));
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ops_wait_while_the_account_is_offline() {
    let fake = fake(Flavor::Qresync, true).await;
    let mut run = Running::start(&fake, config());
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let messages = run.ids(inbox, &[2]).await;
    fake.world().black_hole(true);
    run.send(Command::Reconnect);
    run.queue(Triage::Star(true), messages, 0).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(run.pending(account).await, 1, "the op waits");
    assert!(!has(&fake, "INBOX", 2, "\\Flagged"));
    fake.world().black_hole(false);
    run.send(Command::Reconnect);
    let done = within(WAIT, || async {
        run.pending(account).await == 0 && has(&fake, "INBOX", 2, "\\Flagged")
    })
    .await;
    assert!(done.is_some(), "the op runs once the account is back");
    run.check().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_marked_read_reaches_the_messages_below_the_floor() {
    let fake = fake(Flavor::Qresync, true).await;
    fake.world().append_many("INBOX", 18, |_| 1);
    let mut sync = config();
    sync.options.window = Window {
        days: 90,
        max: 10,
        min: 10,
    };
    let mut run = Running::start(&fake, sync);
    let account = run.add(&fake).await;
    let inbox = run.folder(account, "INBOX").await;
    let floor = run
        .store
        .folder_sync(inbox)
        .recv()
        .await
        .unwrap()
        .unwrap()
        .floor
        .unwrap();
    assert_eq!(floor, 21, "the window keeps the newest 10 of 30");
    let queued = run
        .store
        .mark_folder_read(inbox, now_ms())
        .recv()
        .await
        .unwrap();
    for account in &queued.accounts {
        run.send(Command::OpsQueued { account: *account });
    }
    let done = within(WAIT, || async {
        run.pending(account).await == 0
            && fake.world().mailboxes["INBOX"]
                .messages
                .values()
                .all(|message| message.flags.iter().any(|f| f == "\\Seen"))
    })
    .await;
    assert!(done.is_some(), "every message read on the server");
    let commands = fake.world().commands.clone();
    assert!(
        commands.contains(&"UID SEARCH UNSEEN UNDELETED UID 1:20".to_owned()),
        "{commands:?}"
    );
    run.check().await;
}
