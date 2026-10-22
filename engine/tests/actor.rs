//! The per-account sync actor through the engine service, against the fake IMAP server (CI, no
//! network beyond 127.0.0.1): new mail through IDLE, NOOP before IDLE, the STATUS backstop,
//! IDLE issued again, backoff and its reset, `Reconnect` after silently dead connections,
//! statuses, `FolderOpened`, `LoadOlder`, bodies beside the actor, two accounts at once,
//! throttled progress, a quick stop, Refresh's `CheckMail` and a message whose headers can't be
//! read (issue #20). Timings are short real durations: the store answers
//! from its own threads, so paused tokio time would jump past them.

mod fake;

use fake::{Fake, Flavor, Garble, NOW};
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, LoginFailure};
use sealwax_domain::config::Storage;
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_engine::api::{BodyError, Command, Event, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{
    Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig, backoff_delay,
};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

/// Short intervals, so a test sees each mechanism without the others: account passes rare,
/// IDLE restarts rare, backoff small.
fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        },
        account_pass: Duration::from_secs(60),
        inbox_poll: Duration::from_secs(60),
        opened_poll: Duration::from_millis(300),
        idle_restart: Duration::from_secs(60),
        reuse_check: Duration::from_secs(60),
        noop_timeout: Duration::from_secs(2),
        backoff_min: Duration::from_millis(100),
        backoff_max: Duration::from_millis(800),
        progress_every: Duration::from_millis(100),
        body_linger: Duration::from_secs(2),
        logout_wait: Duration::from_millis(300),
        ..SyncConfig::default()
    }
}

struct Running {
    _dir: tempfile::TempDir,
    engine: Option<Engine>,
    store: Arc<Store>,
    events: async_channel::Receiver<Event>,
    /// Events read while looking for others, oldest first.
    seen: Vec<Event>,
}

impl Running {
    fn start(sync: SyncConfig, trust: Trust) -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("actor-")
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
            trust,
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

    /// Adds an account on `fake` (whose trust the engine needs) and waits for its first
    /// "synced".
    async fn add(&mut self, fake: &Fake, request: u64, name: &str) -> AccountId {
        let settings = AccountSettings {
            key: AccountKey(String::new()),
            name: name.into(),
            label: None,
            color: None,
            address: format!("{name}@example.test"),
            incoming: fake.endpoint(),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: fake.endpoint(),
            username: format!("{name}@example.test"),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        };
        self.send(Command::AddAccount {
            request,
            settings,
            password: Password::new("fake-password".into()),
            sign_in: None,
        });
        let account = self
            .event(WAIT, |event| match event {
                Event::AccountAdded {
                    request: id,
                    result,
                } if *id == request => Some(result.clone().unwrap()),
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

    async fn count(&self, folder: FolderId) -> u32 {
        self.store
            .folder_sync(folder)
            .recv()
            .await
            .unwrap()
            .unwrap()
            .messages
    }

    /// Stops the engine and returns how long that took.
    fn stop(&mut self) -> Duration {
        let engine = self.engine.take().unwrap();
        let started = Instant::now();
        tokio::task::block_in_place(|| engine.stop());
        started.elapsed()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
    }
}

/// Polls `check` until it holds or `within` passes; returns how long it took.
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

/// A fake server whose CA the engine trusts: the engine's trust comes from the fake's file.
async fn fake(flavor: Flavor) -> Fake {
    let fake = Fake::start(flavor).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.create("Sendt", &["\\Sent"]);
        world.create("Arkiv2", &[]);
        world.append_many("INBOX", 20, |_| 1);
        world.append_many("Arkiv2", 3, |_| 1);
    }
    fake
}

/// The engine, trusting exactly the fake's throwaway CA.
fn running(fake: &Fake, sync: SyncConfig) -> Running {
    Running::start(sync, fake.trust.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_mail_flags_and_expunges_arrive_through_idle() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "idle").await;
    let inbox = run.folder(account, "INBOX").await;
    assert!(
        within(WAIT, || async { fake.world().count("IDLE") >= 1 })
            .await
            .is_some(),
        "the IDLE connection idles"
    );
    let started = Instant::now();
    fake.world().append("INBOX", 0, &[]);
    let arrived = within(Duration::from_secs(5), || async {
        run.count(inbox).await == 21
    })
    .await;
    assert!(arrived.is_some(), "new mail through IDLE within 5 s");
    println!("IDLE: new mail in the store after {:?}", started.elapsed());
    {
        let mut world = fake.world();
        world.expunge("INBOX", 3);
        world.set_flags("INBOX", 4, &["\\Flagged"]);
    }
    assert!(
        within(Duration::from_secs(5), || async {
            run.count(inbox).await == 20
                && run
                    .store
                    .folder_messages(inbox)
                    .recv()
                    .await
                    .unwrap()
                    .iter()
                    .any(|m| m.uid == 4 && m.flags.is_flagged())
        })
        .await
        .is_some()
    );
    let commands = fake.world().commands.clone();
    assert!(
        commands.iter().filter(|c| c.starts_with("LIST")).count() == 1,
        "no account pass was needed: {commands:?}"
    );
}

/// A message whose header FETCH reply async-imap can't parse breaks the connection; the passes
/// after it ask for less until it is left out, the account shows synced (`add` waits for it),
/// and the rest of the mail and new mail arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_that_cant_be_read_doesnt_stop_the_inbox() {
    let fake = fake(Flavor::Qresync).await;
    fake.world()
        .garbled
        .insert(("INBOX".into(), 7), Garble::Flags);
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "garbled").await;
    let inbox = run.folder(account, "INBOX").await;
    assert!(
        within(WAIT, || async { run.count(inbox).await == 19 })
            .await
            .is_some(),
        "all but the unreadable message stored"
    );
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    fake.world().append("INBOX", 0, &[]);
    assert!(
        within(Duration::from_secs(5), || async {
            run.count(inbox).await == 20
        })
        .await
        .is_some(),
        "new mail still arrives"
    );
}

/// INBOX recreated (a new UIDVALIDITY) while idling, which ends the server's session: the IDLE
/// connection's next pass syncs INBOX afresh, and the account shows synced again when that pass
/// ends, not at the next account pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_pass_that_syncs_inbox_afresh_ends_synced() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "afresh").await;
    let inbox = run.folder(account, "INBOX").await;
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    {
        let mut world = fake.world();
        world.recreate("INBOX");
        world.append_many("INBOX", 5, |_| 1);
    }
    run.event(WAIT, |event| {
        matches!(
            event,
            Event::SyncProgress { account: id, folder, .. } if *id == account && *folder == inbox
        )
        .then_some(())
    })
    .await;
    assert_eq!(
        run.status(account).await,
        SyncStatus::Synced,
        "synced again after the IDLE connection's pass"
    );
    assert_eq!(run.count(inbox).await, 5);
    assert_eq!(
        fake.world()
            .commands
            .iter()
            .filter(|c| c.starts_with("LIST"))
            .count(),
        1,
        "no account pass was needed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_before_idle_is_caught_by_noop_or_by_the_backstop() {
    let fake = fake(Flavor::Qresync).await;
    fake.world().idle_misses_earlier = true;
    let mut sync = config();
    sync.account_pass = Duration::from_millis(1500);
    let mut run = running(&fake, sync);
    let account = run.add(&fake, 1, "noop").await;
    let inbox = run.folder(account, "INBOX").await;
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    // The IDLE connection's NOOP finds a change made after its pass: another pass, not IDLE.
    fake.world().on("NOOP", |world| {
        world.append("INBOX", 0, &[]);
    });
    fake.world().append("INBOX", 0, &[]);
    assert!(
        within(Duration::from_secs(3), || async {
            run.count(inbox).await == 22
        })
        .await
        .is_some(),
        "the NOOP's change was synced"
    );
    // A change made just as IDLE starts is never announced (Stalwart): the account pass's
    // STATUS backstop finds it.
    fake.world().on("IDLE", |world| {
        world.append("INBOX", 0, &[]);
    });
    fake.world().append("INBOX", 0, &[]);
    let started = Instant::now();
    let caught = within(Duration::from_secs(5), || async {
        run.count(inbox).await == 24
    })
    .await;
    assert!(
        caught.is_some(),
        "the backstop caught the change IDLE missed"
    );
    println!("backstop: caught after {:?}", started.elapsed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_is_issued_again_before_the_restart_interval() {
    let fake = fake(Flavor::Qresync).await;
    let mut sync = config();
    sync.idle_restart = Duration::from_millis(300);
    let mut run = running(&fake, sync);
    run.add(&fake, 1, "restart").await;
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let world = fake.world();
    let idles = world.commands.iter().filter(|c| *c == "IDLE").count();
    let dones = world.commands.iter().filter(|c| *c == "DONE").count();
    assert!(
        (3..=9).contains(&idles) && dones + 1 >= idles,
        "{idles} IDLEs and {dones} DONEs in about 1.6 s"
    );
}

#[test]
fn the_backoff_doubles_between_bounds_with_jitter() {
    let sync = config();
    assert_eq!(backoff_delay(&sync, 0, 0.5), Duration::ZERO);
    assert_eq!(
        backoff_delay(&sync, 1, 0.9),
        Duration::ZERO,
        "the first retry is at once"
    );
    assert_eq!(backoff_delay(&sync, 2, 0.0), Duration::from_millis(50));
    assert_eq!(backoff_delay(&sync, 2, 1.0), Duration::from_millis(100));
    assert_eq!(backoff_delay(&sync, 3, 1.0), Duration::from_millis(200));
    assert_eq!(backoff_delay(&sync, 5, 1.0), Duration::from_millis(800));
    assert_eq!(
        backoff_delay(&sync, 40, 0.0),
        Duration::from_millis(400),
        "capped"
    );
    let defaults = SyncConfig::default();
    assert_eq!(backoff_delay(&defaults, 2, 1.0), Duration::from_secs(1));
    assert_eq!(backoff_delay(&defaults, 30, 1.0), Duration::from_secs(300));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_connections_back_off_and_reconnect_tries_at_once() {
    let fake = fake(Flavor::Qresync).await;
    fake.world().refuse_connections = true;
    let mut run = running(&fake, config());
    run.send(Command::AddAccount {
        request: 1,
        settings: AccountSettings {
            key: AccountKey(String::new()),
            name: "Backoff".into(),
            label: None,
            color: None,
            address: "backoff@example.test".into(),
            incoming: fake.endpoint(),
            receive: sealwax_domain::account::Receive::Imap,
            smtp: fake.endpoint(),
            username: "backoff@example.test".into(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        },
        password: Password::new("fake-password".into()),
        sign_in: None,
    });
    let account = run
        .event(WAIT, |event| match event {
            Event::AccountAdded { result, .. } => Some(result.clone().unwrap()),
            _ => None,
        })
        .await;
    assert!(matches!(run.status(account).await, SyncStatus::Offline(_)));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let attempts = fake.world().connections.clone();
    let gaps: Vec<Duration> = attempts.windows(2).map(|w| w[1] - w[0]).collect();
    println!("attempts {}, gaps {gaps:?}", attempts.len());
    // The command connection's attempts (the IDLE connection starts only after a pass): at
    // once, then 50–100, 100–200, 200–400, 400–800, 400–800 ms… apart.
    assert!(
        (5..=12).contains(&attempts.len()),
        "{} attempts in 2.5 s",
        attempts.len()
    );
    assert!(
        gaps.iter()
            .skip(2)
            .all(|gap| *gap >= Duration::from_millis(45)),
        "{gaps:?}"
    );
    assert!(
        gaps.iter().all(|gap| *gap <= Duration::from_millis(900)),
        "{gaps:?}"
    );
    let before = fake.world().connections.len();
    fake.world().refuse_connections = false;
    let asked = Instant::now();
    run.send(Command::Reconnect);
    assert_eq!(run.status(account).await, SyncStatus::Synced);
    let first = fake.world().connections[before];
    assert!(
        first.duration_since(asked) < Duration::from_millis(100),
        "Reconnect tries at once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_replaces_silently_dead_connections() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "dead").await;
    let inbox = run.folder(account, "INBOX").await;
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    {
        let mut world = fake.world();
        world.black_hole(true);
        world.black_hole(false);
        world.append("INBOX", 0, &[]);
    }
    let missed = within(Duration::from_millis(1500), || async {
        run.count(inbox).await == 21
    })
    .await;
    assert!(missed.is_none(), "a dead IDLE connection hears nothing");
    let asked = Instant::now();
    run.send(Command::Reconnect);
    let converged = within(Duration::from_secs(10), || async {
        run.count(inbox).await == 21
    })
    .await;
    assert!(converged.is_some(), "converged after Reconnect");
    println!("Reconnect: converged after {:?}", asked.elapsed());
    assert!(asked.elapsed() < Duration::from_secs(2));
}

/// The server closes the IDLE connection (a restart, a cut): a new one at once, new mail
/// arrives through it, and the account never shows offline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_idle_connection_is_made_again_at_once() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "closed").await;
    let inbox = run.folder(account, "INBOX").await;
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    let connections = fake.world().connections.len();
    let closed = Instant::now();
    fake.world().close_idling();
    let idling_again = within(Duration::from_secs(5), || async {
        fake.world().count("IDLE") >= 2
    })
    .await;
    assert!(idling_again.is_some(), "a new IDLE within 5 s");
    println!(
        "a closed IDLE connection idles again after {:?}",
        closed.elapsed()
    );
    fake.world().append("INBOX", 0, &[]);
    assert!(
        within(Duration::from_secs(5), || async {
            run.count(inbox).await == 21
        })
        .await
        .is_some(),
        "new mail through the new IDLE connection"
    );
    assert_eq!(
        fake.world().connections.len(),
        connections + 1,
        "only the IDLE connection was made again"
    );
    while let Ok(event) = run.events.try_recv() {
        run.seen.push(event);
    }
    assert!(
        !run.seen.iter().any(|event| matches!(
            event,
            Event::AccountStatus { account: id, status: SyncStatus::Offline(_) } if *id == account
        )),
        "no offline for a connection made again at once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_login_halts_until_reconnect() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "halt").await;
    fake.world().refuse_login = true;
    run.send(Command::Reconnect);
    assert_eq!(
        run.status(account).await,
        SyncStatus::NeedsSignIn(LoginFailure::WrongPassword)
    );
    // The IDLE connection logs in again beside the command connection and is refused too; its
    // AUTHENTICATE can arrive just after the status.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let logins = fake.world().count("AUTHENTICATE");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        fake.world().count("AUTHENTICATE"),
        logins,
        "no retries while it needs sign-in"
    );
    fake.world().refuse_login = false;
    run.send(Command::Reconnect);
    assert_eq!(run.status(account).await, SyncStatus::Synced);
    let shown: Vec<SyncStatus> = run
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::AccountStatus {
                account: id,
                status,
            } if *id == account => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(
        shown,
        [
            SyncStatus::Syncing,
            SyncStatus::Synced,
            SyncStatus::Syncing,
            SyncStatus::NeedsSignIn(LoginFailure::WrongPassword),
            SyncStatus::Syncing,
            SyncStatus::Synced
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_opened_folder_syncs_now_and_stays_current_until_another_opens() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "opened").await;
    let plain = run.folder(account, "Arkiv2").await;
    assert_eq!(run.count(plain).await, 0, "counts only until opened");
    run.send(Command::FolderOpened { folder: plain });
    assert!(
        within(Duration::from_secs(2), || async {
            run.count(plain).await == 3
        })
        .await
        .is_some()
    );
    fake.world().append("Arkiv2", 0, &[]);
    assert!(
        within(Duration::from_secs(2), || async {
            run.count(plain).await == 4
        })
        .await
        .is_some(),
        "kept current while open"
    );
    let inbox = run.folder(account, "INBOX").await;
    run.send(Command::FolderOpened { folder: inbox });
    tokio::time::sleep(Duration::from_millis(500)).await;
    fake.world().append("Arkiv2", 0, &[]);
    assert!(
        within(Duration::from_millis(1500), || async {
            run.count(plain).await == 5
        })
        .await
        .is_none(),
        "no longer polled at the open folder's interval"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_mail_loads_through_the_actor() {
    let fake = Fake::start(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.append_many("INBOX", 500, |n| if n < 300 { 200 } else { 10 });
    }
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "older").await;
    let inbox = run.folder(account, "INBOX").await;
    assert_eq!(run.count(inbox).await, 200);
    let mut answers = Vec::new();
    loop {
        run.send(Command::LoadOlder { folder: inbox });
        let answer = run
            .event(WAIT, |event| match event {
                Event::OlderLoaded {
                    folder,
                    fetched,
                    complete,
                } if *folder == inbox => Some((*fetched, *complete)),
                _ => None,
            })
            .await;
        answers.push(answer);
        if answer.1 {
            break;
        }
    }
    assert_eq!(answers, [(200, false), (100, true)]);
    assert_eq!(run.count(inbox).await, 500);
}

/// The M3 gate's decision (2026-10-06): a windowed INBOX under IDLE shows the server's counts.
/// New mail counts at once through IDLE; a change below the floor shows with the next account
/// pass's STATUS; older mail moves into the store without changing them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_windowed_inbox_shows_the_servers_counts() {
    let fake = Fake::start(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        // 300 unread messages 200 days old, then 200 read ones ten days old.
        world.append_many("INBOX", 300, |_| 200);
        for _ in 0..200 {
            world.append("INBOX", 10, &["\\Seen"]);
        }
    }
    let mut sync = config();
    sync.account_pass = Duration::from_millis(1500);
    let mut run = running(&fake, sync);
    let account = run.add(&fake, 1, "counts").await;
    let inbox = run.folder(account, "INBOX").await;
    assert_eq!(run.count(inbox).await, 200);
    let store = run.store.clone();
    let shown = || async {
        let folders = store.folders().recv().await.unwrap().value;
        let found = folders.iter().find(|f| f.id == inbox).unwrap();
        (found.total, found.unread)
    };
    assert_eq!(
        shown().await,
        (500, 300),
        "counted in the first account pass"
    );
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;

    fake.world().append("INBOX", 0, &[]);
    assert!(
        within(Duration::from_secs(5), || async {
            shown().await == (501, 301)
        })
        .await
        .is_some(),
        "new mail through IDLE"
    );
    for uid in 1..=20 {
        fake.world().set_flags("INBOX", uid, &["\\Seen"]);
    }
    let started = Instant::now();
    let caught = within(Duration::from_secs(5), || async {
        shown().await == (501, 281)
    })
    .await;
    assert!(
        caught.is_some(),
        "20 read below the floor: {:?}",
        shown().await
    );
    println!(
        "counts below the floor: caught after {:?}",
        started.elapsed()
    );

    run.send(Command::LoadOlder { folder: inbox });
    let older = run
        .event(WAIT, |event| match event {
            Event::OlderLoaded {
                folder, fetched, ..
            } if *folder == inbox => Some(*fetched),
            _ => None,
        })
        .await;
    assert_eq!(older, 200);
    assert_eq!(
        shown().await,
        (501, 281),
        "older mail moves, the counts stay"
    );
    assert_eq!(
        run.store.check().recv().await.unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bodies_download_beside_the_actor_and_the_prefetch_follows_new_mail() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "bodies").await;
    let inbox = run.folder(account, "INBOX").await;
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    let ids: Vec<MessageId> = run
        .store
        .folder_messages(inbox)
        .recv()
        .await
        .unwrap()
        .iter()
        .map(|m| m.id)
        .take(2)
        .collect();
    run.send(Command::FetchBodies {
        messages: ids.clone(),
    });
    let mut answered = Vec::new();
    while answered.len() < 2 {
        answered.push(
            run.event(WAIT, |event| match event {
                Event::BodyFetched { message, result } => Some((*message, *result)),
                _ => None,
            })
            .await,
        );
    }
    answered.sort_by_key(|(id, _)| *id);
    assert_eq!(answered, [(ids[0], Ok(())), (ids[1], Ok(()))]);
    for id in &ids {
        assert!(run.store.raw_message(*id).recv().await.unwrap().is_some());
    }
    let idles = fake.world().count("IDLE");
    assert!(idles >= 1, "the IDLE connection kept idling");
    // The prefetch: with [storage] known, new mail is downloaded after the pass that stored it.
    run.send(Command::Storage {
        storage: Storage {
            cache_size: None,
            prefetch_days: 14,
        },
    });
    fake.world().append("INBOX", 0, &[]);
    let usage = within(Duration::from_secs(5), || async {
        run.store.body_usage().recv().await.unwrap().bodies > 2
    })
    .await;
    assert!(usage.is_some(), "the prefetch downloaded recent INBOX mail");
    run.send(Command::FetchBodies {
        messages: vec![MessageId(999_999)],
    });
    let gone = run
        .event(WAIT, |event| match event {
            Event::BodyFetched { message, result } if *message == MessageId(999_999) => {
                Some(*result)
            }
            _ => None,
        })
        .await;
    assert_eq!(gone, Err(BodyError::Gone));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_accounts_sync_at_once_and_the_engine_stops_quickly() {
    let one = fake(Flavor::Qresync).await;
    let two = fake(Flavor::Condstore).await;
    // Each fake has its own throwaway CA: trust one file holding both.
    let pem = |fake: &Fake| match &fake.trust {
        Trust::Roots(path) => std::fs::read_to_string(path).unwrap(),
        Trust::Platform => unreachable!(),
    };
    let cas = tempfile::tempdir().unwrap();
    let both = cas.path().join("both.pem");
    std::fs::write(&both, format!("{}{}", pem(&one), pem(&two))).unwrap();
    let mut run = Running::start(config(), Trust::Roots(both));
    for (fake, name, request) in [(&one, "first", 1), (&two, "second", 2)] {
        run.send(Command::AddAccount {
            request,
            settings: AccountSettings {
                key: AccountKey(String::new()),
                name: name.into(),
                label: None,
                color: None,
                address: format!("{name}@example.test"),
                incoming: fake.endpoint(),
                receive: sealwax_domain::account::Receive::Imap,
                smtp: fake.endpoint(),
                username: format!("{name}@example.test"),
                auth: AuthMethod::Password,
                signature: None,
                identities: Vec::new(),
            },
            password: Password::new("fake-password".into()),
            sign_in: None,
        });
    }
    let mut accounts = Vec::new();
    while accounts.len() < 2 {
        accounts.push(
            run.event(WAIT, |event| match event {
                Event::AccountAdded { result, .. } => Some(result.clone().unwrap()),
                _ => None,
            })
            .await,
        );
    }
    for account in &accounts {
        assert!(
            within(WAIT, || async {
                run.store
                    .account_folders(*account)
                    .recv()
                    .await
                    .unwrap()
                    .iter()
                    .any(|f| f.name_raw == "INBOX" && f.messages == 20)
            })
            .await
            .is_some()
        );
    }
    within(WAIT, || async {
        one.world().count("IDLE") >= 1 && two.world().count("IDLE") >= 1
    })
    .await;
    one.world().append("INBOX", 0, &[]);
    two.world().append("INBOX", 0, &[]);
    let inboxes = [
        run.folder(accounts[0], "INBOX").await,
        run.folder(accounts[1], "INBOX").await,
    ];
    assert!(
        within(Duration::from_secs(5), || async {
            run.count(inboxes[0]).await == 21 && run.count(inboxes[1]).await == 21
        })
        .await
        .is_some(),
        "both accounts' IDLE connections"
    );
    let stopped = run.stop();
    println!("stop with two idling accounts: {stopped:?}");
    assert!(stopped < Duration::from_millis(1500), "{stopped:?}");
    let logouts = one.world().count("LOGOUT") + two.world().count("LOGOUT");
    assert!(logouts >= 2, "LOGOUT sent: {logouts}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_is_throttled_and_ends_with_the_last_batch() {
    let fake = Fake::start(Flavor::Qresync).await;
    {
        let mut world = fake.world();
        world.create("INBOX", &[]);
        world.append_many("INBOX", 3000, |_| 1);
    }
    let mut run = running(&fake, config());
    let started = Instant::now();
    let account = run.add(&fake, 1, "progress").await;
    let elapsed = started.elapsed();
    let progress: Vec<(u32, u32)> = run
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::SyncProgress {
                account: id,
                fetched,
                total,
                ..
            } if *id == account => Some((*fetched, *total)),
            _ => None,
        })
        .collect();
    println!(
        "{} progress events in {elapsed:?} for 15 batches",
        progress.len()
    );
    assert_eq!(progress.last(), Some(&(3000, 3000)));
    let allowed = (elapsed.as_millis() / 100) as usize + 2;
    assert!(progress.len() <= allowed, "{} > {allowed}", progress.len());
}

/// Refresh (`CheckMail`, M8): an account pass now, on the connection the actor has, instead of
/// the next one a minute later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_mail_runs_an_account_pass_now() {
    let fake = fake(Flavor::Qresync).await;
    let mut run = running(&fake, config());
    let account = run.add(&fake, 1, "check").await;
    let sent = run.folder(account, "Sendt").await;
    within(WAIT, || async { fake.world().count("IDLE") >= 1 }).await;
    let logins = |fake: &Fake| {
        let world = fake.world();
        world.count("LOGIN") + world.count("AUTHENTICATE")
    };
    let before = logins(&fake);
    fake.world().append("Sendt", 0, &[]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        run.count(sent).await,
        0,
        "the next account pass is a minute away"
    );
    let lists = fake.world().count("LIST");
    run.send(Command::CheckMail);
    assert!(
        within(Duration::from_secs(5), || async {
            run.count(sent).await == 1
        })
        .await
        .is_some(),
        "the account pass ran"
    );
    assert!(
        fake.world().count("LIST") > lists,
        "an account pass lists the folders"
    );
    assert_eq!(logins(&fake), before, "no connection was made again");
}
