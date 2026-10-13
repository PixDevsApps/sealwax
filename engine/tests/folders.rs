//! Folder management through the engine service (ADR-020, M9), against the fake IMAP server
//! (CI, nothing beyond 127.0.0.1): CREATE and SUBSCRIBE with the store following before the
//! answer; RENAME of a folder with folders under it, their subscriptions moved and their
//! messages kept; DELETE deepest first, a folder the store didn't know included; a new folder at
//! the top of an `INBOX.` server; refusals (the engine's own checks send nothing, the server's
//! come back in its words); offline accounts; and a POP3 account's local folders, changed at
//! once. Every name is fictional.

mod fake;

use fake::{Fake, Flavor, NOW};
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, Endpoint, Pop3Policy, Receive, Security,
};
use sealwax_domain::folders::NameProblem;
use sealwax_domain::model::{AccountId, Folder, FolderId, FolderRole};
use sealwax_engine::api::{Command, Event, FolderChange, FolderError, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::net::Trust;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

fn config() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            now: Some(NOW),
            ..SyncOptions::default()
        },
        account_pass: Duration::from_secs(60),
        opened_poll: Duration::from_millis(300),
        backoff_min: Duration::from_millis(100),
        backoff_max: Duration::from_millis(800),
        logout_wait: Duration::from_millis(300),
        ..SyncConfig::default()
    }
}

struct Running {
    _dir: tempfile::TempDir,
    engine: Option<Engine>,
    store: Arc<Store>,
    events: async_channel::Receiver<Event>,
    next_request: u64,
}

impl Running {
    fn start(trust: Trust) -> Running {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("folders-")
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
            sync: config(),
        })
        .unwrap();
        let events = engine.events();
        Running {
            _dir: dir,
            engine: Some(engine),
            store,
            events,
            next_request: 100,
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
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    }

    async fn status(&mut self, account: AccountId) -> SyncStatus {
        self.event(|event| match event {
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
            name: "Folders".into(),
            label: None,
            color: None,
            address: "folders@example.test".into(),
            incoming: fake.endpoint(),
            receive: Receive::Imap,
            smtp: fake.endpoint(),
            username: "folders@example.test".into(),
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
            .event(|event| match event {
                Event::AccountAdded { request: 1, result } => Some(result.clone().unwrap()),
                _ => None,
            })
            .await;
        assert_eq!(self.status(account).await, SyncStatus::Synced);
        account
    }

    /// Sends `change` and waits for its answer, with how long it took.
    async fn change(
        &mut self,
        account: AccountId,
        change: FolderChange,
    ) -> (Result<(), FolderError>, Duration) {
        self.next_request += 1;
        let request = self.next_request;
        let started = Instant::now();
        self.send(Command::ChangeFolder {
            request,
            account,
            change,
        });
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = tokio::time::timeout(left, self.events.recv())
                .await
                .expect("no answer in time")
                .expect("the engine stopped");
            if let Event::FolderChanged {
                request: id,
                account: answered,
                result,
            } = event
                && id == request
            {
                assert_eq!(answered, account);
                return (result, started.elapsed());
            }
        }
    }

    async fn folders(&self, account: AccountId) -> Vec<Folder> {
        self.store
            .folders()
            .recv()
            .await
            .unwrap()
            .value
            .into_iter()
            .filter(|folder| folder.account == account)
            .collect()
    }

    async fn names(&self, account: AccountId) -> Vec<String> {
        let mut names: Vec<String> = self
            .folders(account)
            .await
            .into_iter()
            .map(|folder| folder.name)
            .collect();
        names.sort();
        names
    }

    async fn folder(&self, account: AccountId, name: &str) -> Folder {
        self.folders(account)
            .await
            .into_iter()
            .find(|folder| folder.name == name)
            .unwrap_or_else(|| panic!("{name} not stored"))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            tokio::task::block_in_place(|| engine.stop());
        }
    }
}

async fn fake(flavor: Flavor, mailboxes: &[(&str, &[&str])]) -> Fake {
    let fake = Fake::start(flavor).await;
    {
        let mut world = fake.world();
        for (name, attributes) in mailboxes {
            world.create(name, attributes);
        }
        world.append_many("INBOX", 3, |_| 1);
    }
    fake
}

const ACCOUNT: &[(&str, &[&str])] = &[
    ("INBOX", &[]),
    ("Sendt", &["\\Sent"]),
    ("Kunder", &[]),
    ("Kunder/Acme", &[]),
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_folder_is_created_subscribed_and_listed_before_the_answer() {
    for flavor in Flavor::ALL {
        let fake = fake(flavor, ACCOUNT).await;
        let mut run = Running::start(fake.trust.clone());
        let account = run.add(&fake).await;
        let kunder = run.folder(account, "Kunder").await;
        let (result, _) = run
            .change(
                account,
                FolderChange::Create {
                    parent: Some(kunder.id),
                    name: " Høst 📬 ".into(),
                },
            )
            .await;
        assert!(result.is_ok(), "{flavor:?}: {result:?}");
        let wire = "Kunder/H&APg-st &2D3c7A-";
        assert!(fake.world().mailboxes.contains_key(wire), "{flavor:?}");
        assert!(fake.world().subscribed.contains(wire), "{flavor:?}");
        let created = run.folder(account, "Kunder/Høst 📬").await;
        assert_eq!(created.delimiter, Some('/'));
        let (result, _) = run
            .change(
                account,
                FolderChange::Create {
                    parent: None,
                    name: "Reiser & ferie".into(),
                },
            )
            .await;
        assert!(result.is_ok(), "{flavor:?}: {result:?}");
        assert!(fake.world().mailboxes.contains_key("Reiser &- ferie"));
        run.folder(account, "Reiser & ferie").await;
        let creates: Vec<String> = fake
            .world()
            .commands
            .iter()
            .filter(|command| command.starts_with("CREATE") || command.starts_with("SUBSCRIBE"))
            .cloned()
            .collect();
        assert_eq!(
            creates,
            [
                format!("CREATE \"{wire}\""),
                format!("SUBSCRIBE \"{wire}\""),
                "CREATE \"Reiser &- ferie\"".to_owned(),
                "SUBSCRIBE \"Reiser &- ferie\"".to_owned(),
            ],
            "{flavor:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rename_moves_the_folders_under_it_their_subscriptions_and_keeps_their_messages() {
    let fake = fake(
        Flavor::Qresync,
        &[
            ("INBOX", &[]),
            ("Kunder", &[]),
            ("Kunder/Acme", &[]),
            ("Kunderservice", &[]),
        ],
    )
    .await;
    {
        let mut world = fake.world();
        world.append_many("Kunder/Acme", 4, |_| 1);
        for name in ["INBOX", "Kunder", "Kunder/Acme", "Kunderservice"] {
            world.subscribed.insert(name.to_owned());
        }
    }
    let mut run = Running::start(fake.trust.clone());
    let account = run.add(&fake).await;
    let acme = run.folder(account, "Kunder/Acme").await;
    run.send(Command::FolderOpened { folder: acme.id });
    let synced = |store: Arc<Store>| async move {
        let deadline = Instant::now() + WAIT;
        loop {
            let state = store.folder_sync(acme.id).recv().await.unwrap().unwrap();
            if state.messages >= 4 {
                return state;
            }
            assert!(Instant::now() < deadline, "the opened folder never synced");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let before = synced(run.store.clone()).await;
    let kunder = run.folder(account, "Kunder").await;
    let (result, took) = run
        .change(
            account,
            FolderChange::Rename {
                folder: kunder.id,
                name: "Klienter æ".into(),
            },
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    println!("rename answered in {took:?}");
    let world = fake.world().names();
    assert_eq!(
        world,
        [
            "INBOX",
            "Klienter &AOY-",
            "Klienter &AOY-/Acme",
            "Kunderservice"
        ]
    );
    let subscribed: Vec<String> = fake.world().subscribed.iter().cloned().collect();
    assert_eq!(
        subscribed,
        [
            "INBOX",
            "Klienter &AOY-",
            "Klienter &AOY-/Acme",
            "Kunderservice"
        ]
    );
    let renamed = run.folder(account, "Klienter æ/Acme").await;
    assert_eq!((renamed.id, renamed.total), (acme.id, 4));
    assert_eq!(run.folder(account, "Klienter æ").await.id, kunder.id);
    let after = run
        .store
        .folder_sync(acme.id)
        .recv()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.uidvalidity, after.uidnext, after.messages),
        (before.uidvalidity, before.uidnext, before.messages)
    );
    assert_eq!(after.name_raw, "Klienter &AOY-/Acme");
    // The renamed folder still syncs under its new name: new mail arrives.
    fake.world().append("Klienter &AOY-/Acme", 0, &[]);
    run.send(Command::FolderOpened { folder: acme.id });
    let deadline = Instant::now() + WAIT;
    while run.folder(account, "Klienter æ/Acme").await.total < 5 {
        assert!(Instant::now() < deadline, "the renamed folder never synced");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_goes_deepest_first_with_folders_the_store_never_saw() {
    let fake = fake(
        Flavor::Condstore,
        &[
            ("INBOX", &[]),
            ("Kunder", &[]),
            ("Kunder/Acme", &[]),
            ("Kunder/Acme/2026", &[]),
            ("Kunderservice", &[]),
        ],
    )
    .await;
    fake.world().append_many("Kunder/Acme", 2, |_| 1);
    let mut run = Running::start(fake.trust.clone());
    let account = run.add(&fake).await;
    // Made by another program after the last list: the store doesn't know it.
    fake.world().create("Kunder/Fjord", &[]);
    let kunder = run.folder(account, "Kunder").await;
    let (result, took) = run
        .change(account, FolderChange::Delete { folder: kunder.id })
        .await;
    assert!(result.is_ok(), "{result:?}");
    println!("delete answered in {took:?}");
    assert_eq!(fake.world().names(), ["INBOX", "Kunderservice"]);
    let deletes: Vec<String> = fake
        .world()
        .commands
        .iter()
        .filter(|command| command.starts_with("DELETE"))
        .cloned()
        .collect();
    assert_eq!(
        deletes,
        [
            "DELETE \"Kunder/Acme/2026\"",
            "DELETE \"Kunder/Fjord\"",
            "DELETE \"Kunder/Acme\"",
            "DELETE \"Kunder\"",
        ]
    );
    assert_eq!(fake.world().count("UNSUBSCRIBE"), 4);
    assert_eq!(run.names(account).await, ["INBOX", "Kunderservice"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_an_inbox_server_new_folders_at_the_top_go_under_the_inbox() {
    let fake = Fake::start(Flavor::Rev1).await;
    {
        let mut world = fake.world();
        world.delimiter = '.';
        world.create("INBOX", &[]);
        world.create("INBOX.Sent", &["\\Sent"]);
        world.create("INBOX.Kunder", &[]);
    }
    let mut run = Running::start(fake.trust.clone());
    let account = run.add(&fake).await;
    let (result, _) = run
        .change(
            account,
            FolderChange::Create {
                parent: None,
                name: "Reiser".into(),
            },
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert!(fake.world().mailboxes.contains_key("INBOX.Reiser"));
    assert_eq!(
        run.folder(account, "INBOX.Reiser").await.delimiter,
        Some('.')
    );
    let (result, _) = run
        .change(
            account,
            FolderChange::Create {
                parent: None,
                name: "Kunder.Acme".into(),
            },
        )
        .await;
    assert!(
        matches!(result, Err(FolderError::Name(NameProblem::Delimiter('.')))),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_come_back_and_the_engines_own_send_nothing() {
    let fake = fake(Flavor::Qresync, ACCOUNT).await;
    let mut run = Running::start(fake.trust.clone());
    let account = run.add(&fake).await;
    let kunder = run.folder(account, "Kunder").await;
    let inbox = run.folder(account, "INBOX").await;
    let sent_before = fake.world().commands.len();
    let checks = [
        (
            FolderChange::Create {
                parent: Some(kunder.id),
                name: "acme".into(),
            },
            "taken",
        ),
        (
            FolderChange::Create {
                parent: None,
                name: "  ".into(),
            },
            "empty",
        ),
        (
            FolderChange::Rename {
                folder: inbox.id,
                name: "Innboks".into(),
            },
            "role",
        ),
        (FolderChange::Delete { folder: inbox.id }, "role"),
        (
            FolderChange::Delete {
                folder: FolderId(9999),
            },
            "missing",
        ),
    ];
    for (change, why) in checks {
        let (result, _) = run.change(account, change).await;
        let seen = match result {
            Err(FolderError::Name(NameProblem::Taken)) => "taken",
            Err(FolderError::Name(NameProblem::Empty)) => "empty",
            Err(FolderError::NotAllowed) => "role",
            Err(FolderError::Missing) => "missing",
            other => panic!("{why}: {other:?}"),
        };
        assert_eq!(seen, why);
    }
    let sent: Vec<String> = fake.world().commands[sent_before..].to_vec();
    assert!(
        sent.iter().all(|command| !["CREATE", "RENAME", "DELETE"]
            .iter()
            .any(|verb| command.starts_with(verb))),
        "{sent:?}"
    );
    // A name the store didn't know is the server's to refuse, in its words.
    fake.world().create("Kunder/Fjord", &[]);
    let (result, _) = run
        .change(
            account,
            FolderChange::Create {
                parent: Some(kunder.id),
                name: "Fjord".into(),
            },
        )
        .await;
    match result {
        Err(FolderError::Refused(text)) => {
            assert_eq!(text.code(), Some("ALREADYEXISTS"));
            assert!(text.as_str().contains("already exists"));
        }
        other => panic!("{other:?}"),
    }
    // The refused CREATE listed the folders again: the store knows Fjord now.
    run.folder(account, "Kunder/Fjord").await;
    fake.world()
        .refuse("RENAME", "NO [NOPERM] Permission denied");
    let acme = run.folder(account, "Kunder/Acme").await;
    let (result, _) = run
        .change(
            account,
            FolderChange::Rename {
                folder: acme.id,
                name: "Acme AS".into(),
            },
        )
        .await;
    assert!(
        matches!(&result, Err(FolderError::Refused(text)) if text.as_str().contains("Permission denied")),
        "{result:?}"
    );
    assert_eq!(run.folder(account, "Kunder/Acme").await.id, acme.id);
    // The connection breaks before DELETE is answered: lost, and the account reconnects.
    fake.world().lose_reply("DELETE");
    let (result, _) = run
        .change(account, FolderChange::Delete { folder: acme.id })
        .await;
    assert!(matches!(result, Err(FolderError::Lost)), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_offline_or_unknown_sends_nothing() {
    let fake = fake(Flavor::Qresync, ACCOUNT).await;
    let mut run = Running::start(fake.trust.clone());
    let account = run.add(&fake).await;
    let (result, _) = run
        .change(
            AccountId(4242),
            FolderChange::Create {
                parent: None,
                name: "Reiser".into(),
            },
        )
        .await;
    assert!(matches!(result, Err(FolderError::Offline)), "{result:?}");
    fake.world().refuse_connections = true;
    run.send(Command::Reconnect);
    let deadline = Instant::now() + WAIT;
    loop {
        let status = run.status(account).await;
        if matches!(status, SyncStatus::Offline(_)) {
            break;
        }
        assert!(Instant::now() < deadline, "never offline");
    }
    let before = fake.world().count("CREATE");
    let (result, _) = run
        .change(
            account,
            FolderChange::Create {
                parent: None,
                name: "Reiser".into(),
            },
        )
        .await;
    assert!(matches!(result, Err(FolderError::Offline)), "{result:?}");
    assert_eq!(fake.world().count("CREATE"), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pop3_accounts_local_folders_change_at_once() {
    let mut run = Running::start(Trust::Platform);
    let closed = || {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let endpoint = |port| Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    };
    run.send(Command::Accounts {
        accounts: vec![AccountSettings {
            key: AccountKey("pop3-folders".into()),
            name: "Pop".into(),
            label: None,
            color: None,
            address: "pop@example.test".into(),
            incoming: endpoint(closed()),
            receive: Receive::Pop3(Pop3Policy::default()),
            smtp: endpoint(closed()),
            username: "pop".into(),
            auth: AuthMethod::Password,
            signature: None,
            identities: Vec::new(),
        }],
    });
    let account = run
        .event(|event| match event {
            Event::AccountStatus { account, .. } => Some(*account),
            _ => None,
        })
        .await;
    // Signed out (no password stored), its local folders still change.
    let inbox = loop {
        if let Some(inbox) = run
            .folders(account)
            .await
            .into_iter()
            .find(|folder| folder.role == FolderRole::Inbox)
        {
            break inbox;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let (result, took) = run
        .change(
            account,
            FolderChange::Create {
                parent: Some(inbox.id),
                name: "Kvitteringer".into(),
            },
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    println!("local create answered in {took:?}");
    let receipts = run.folder(account, "INBOX/Kvitteringer").await;
    let (result, _) = run
        .change(
            account,
            FolderChange::Create {
                parent: Some(receipts.id),
                name: "2026".into(),
            },
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    let (result, _) = run
        .change(
            account,
            FolderChange::Rename {
                folder: receipts.id,
                name: "Kvitteringer & bilag".into(),
            },
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        run.folder(account, "INBOX/Kvitteringer & bilag/2026")
            .await
            .delimiter,
        Some('/')
    );
    let (result, _) = run
        .change(
            account,
            FolderChange::Rename {
                folder: inbox.id,
                name: "Innboks".into(),
            },
        )
        .await;
    assert!(matches!(result, Err(FolderError::NotAllowed)), "{result:?}");
    let (result, _) = run
        .change(
            account,
            FolderChange::Delete {
                folder: receipts.id,
            },
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        run.names(account).await,
        ["Archive", "Drafts", "INBOX", "Junk", "Sent", "Trash"]
    );
}
