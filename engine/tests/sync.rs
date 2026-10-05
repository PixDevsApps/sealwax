//! The sync engine against a fake IMAP server (no network beyond 127.0.0.1, in CI): QRESYNC,
//! CONDSTORE-only and IMAP4rev1 paths, UIDVALIDITY resets, the count check's UID diff, folders
//! that disappear, the first-sync window and older mail, the folder policy, connections cut in
//! the middle of a FETCH, and a short randomized run per flavour.

mod fake;
mod snapshot;

use fake::{Fake, Flavor, Mailbox, NOW};
use sealwax_domain::model::{AccountId, Flags, FolderId};
use sealwax_domain::sync::Window;
use sealwax_engine::imap::{
    Connection, Elsewhere, FolderLocks, SyncOptions, SyncReport, load_older, sync_account,
    sync_folder,
};
use sealwax_infrastructure::store::{NewAccount, Store, StoreOptions, SyncState};
use snapshot::{Folder, Rng};
use std::collections::BTreeMap;
use std::path::Path;

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn store() -> (tempfile::TempDir, Store, AccountId) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("sync-")
        .tempdir_in(base)
        .unwrap();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Fake".into(),
            address: "test@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    (dir, store, account)
}

fn options() -> SyncOptions {
    SyncOptions {
        now: Some(NOW),
        ..SyncOptions::default()
    }
}

/// The fake's mailboxes as the cache should hold them.
fn server(mailboxes: &BTreeMap<String, Mailbox>) -> BTreeMap<String, Folder> {
    mailboxes
        .iter()
        .filter(|(_, mailbox)| {
            !mailbox
                .attributes
                .iter()
                .any(|a| a.eq_ignore_ascii_case("\\Noselect"))
        })
        .map(|(name, mailbox)| {
            let messages = mailbox
                .messages
                .iter()
                .map(|(uid, message)| {
                    let (flags, keywords) =
                        Flags::from_imap(message.flags.iter().map(String::as_str));
                    (*uid, (flags, keywords, Some(message.message_id.clone())))
                })
                .collect();
            (
                name.clone(),
                Folder {
                    uidvalidity: Some(mailbox.uidvalidity),
                    floor: 1,
                    synced: true,
                    messages,
                },
            )
        })
        .collect()
}

async fn diverged(fake: &Fake, store: &Store, account: AccountId) -> Vec<String> {
    let mailboxes = fake.world().mailboxes();
    snapshot::diff(&server(&mailboxes), &snapshot::local(store, account).await)
}

async fn account_pass(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
    options: &SyncOptions,
) -> SyncReport {
    sync_account(
        conn,
        store,
        account,
        options,
        &FolderLocks::default(),
        |_, _| {},
    )
    .await
    .unwrap()
}

async fn folder(store: &Store, account: AccountId, name: &str) -> FolderId {
    store
        .account_folders(account)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .find(|state| state.name_raw == name)
        .unwrap_or_else(|| panic!("{name} not stored"))
        .folder
}

async fn state(store: &Store, folder: FolderId) -> sealwax_infrastructure::store::FolderSync {
    store.folder_sync(folder).recv().await.unwrap().unwrap()
}

/// INBOX with 30 recent messages, a Sent folder with 5 and a plain folder with 3.
fn seed(fake: &Fake) {
    let mut world = fake.world();
    world.create("INBOX", &[]);
    world.create("Sendt", &["\\Sent"]);
    world.create("Arkiv2", &[]);
    for n in 0..30 {
        let flags: &[&str] = if n % 3 == 0 { &[] } else { &["\\Seen"] };
        world.append("INBOX", 1 + n % 5, flags);
    }
    world.append_many("Sendt", 5, |_| 2);
    world.append_many("Arkiv2", 3, |_| 2);
}

#[test]
fn new_mail_flag_changes_and_expunges_reach_the_store_on_every_flavour() {
    runtime().block_on(async {
        for flavor in Flavor::ALL {
            let fake = Fake::start(flavor).await;
            seed(&fake);
            let (_dir, store, account) = store();
            let mut conn = fake.connect().await;
            let first = account_pass(&mut conn, &store, account, &options()).await;
            assert_eq!(first.stats.passes, 2, "{flavor:?}: INBOX and Sent");
            assert_eq!(first.stats.stored, 35, "{flavor:?}");
            let plain = folder(&store, account, "Arkiv2").await;
            let plain_state = state(&store, plain).await;
            assert_eq!(plain_state.state, SyncState::New, "{flavor:?}: counts only");
            let counts = store.folders().recv().await.unwrap().value;
            let shown = counts.iter().find(|f| f.id == plain).unwrap();
            assert_eq!((shown.total, shown.unread), (3, 3), "{flavor:?}");
            assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());

            {
                let mut world = fake.world();
                for _ in 0..3 {
                    world.append("INBOX", 0, &[]);
                }
                world.set_flags("INBOX", 2, &["\\Seen", "\\Flagged"]);
                world.set_flags("INBOX", 3, &["$Label1"]);
                world.set_flags("INBOX", 4, &["\\Seen", "Prioritert"]);
                world.set_flags("Sendt", 1, &["\\Answered"]);
                for uid in [5, 6, 7, 20] {
                    world.expunge("INBOX", uid);
                }
            }
            let second = account_pass(&mut conn, &store, account, &options()).await;
            assert_eq!(
                diverged(&fake, &store, account).await,
                Vec::<String>::new(),
                "{flavor:?}"
            );
            let stats = second.stats;
            assert_eq!(
                (stats.stored, stats.removed),
                (3, 4),
                "{flavor:?}: {stats:?}"
            );
            assert!(stats.flag_changes >= 4, "{flavor:?}: {stats:?}");
            match flavor {
                Flavor::Qresync => {
                    assert_eq!(stats.changedsince_fetches, 2, "{stats:?}");
                    assert_eq!((stats.vanished, stats.uid_diffs), (4, 0), "{stats:?}");
                }
                Flavor::Condstore => {
                    assert_eq!(stats.changedsince_fetches, 2, "{stats:?}");
                    assert!(
                        stats.count_mismatches >= 1 && stats.uid_diffs >= 1,
                        "{stats:?}"
                    );
                }
                Flavor::Rev1 => {
                    assert_eq!(stats.full_flag_scans, 2, "{stats:?}");
                    assert_eq!(stats.uid_diffs, 0, "{stats:?}");
                }
            }

            let third = account_pass(&mut conn, &store, account, &options()).await;
            match flavor {
                Flavor::Rev1 => assert_eq!(third.stats.full_flag_scans, 2, "no HIGHESTMODSEQ"),
                _ => assert_eq!(
                    (third.stats.status_skips, third.stats.passes),
                    (2, 0),
                    "{flavor:?}: STATUS shows no change"
                ),
            }
            assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
            conn.logout().await;
            store.close().unwrap();
        }
    });
}

#[test]
fn a_new_uidvalidity_resets_the_folder() {
    runtime().block_on(async {
        for flavor in Flavor::ALL {
            let fake = Fake::start(flavor).await;
            seed(&fake);
            let (_dir, store, account) = store();
            let mut conn = fake.connect().await;
            account_pass(&mut conn, &store, account, &options()).await;
            {
                let mut world = fake.world();
                world.recreate("INBOX");
                world.append_many("INBOX", 4, |_| 1);
            }
            let report = account_pass(&mut conn, &store, account, &options()).await;
            assert_eq!(report.stats.resets, 1, "{flavor:?}");
            assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
            let inbox = folder(&store, account, "INBOX").await;
            assert_eq!(state(&store, inbox).await.messages, 4);
            assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
            store.close().unwrap();
        }
    });
}

#[test]
fn the_count_check_repairs_expunges_no_notice_reported() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Qresync).await;
        seed(&fake);
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account, &options()).await;
        {
            let mut world = fake.world();
            world.forget_vanished = true;
            world.quiet = true;
            for uid in [1, 2, 30] {
                world.expunge("INBOX", uid);
            }
        }
        let report = account_pass(&mut conn, &store, account, &options()).await;
        let stats = report.stats;
        assert_eq!((stats.vanished, stats.removed), (0, 3), "{stats:?}");
        assert_eq!(
            (stats.count_mismatches, stats.uid_diffs),
            (1, 1),
            "{stats:?}"
        );
        assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
        store.close().unwrap();
    });
}

#[test]
fn notices_during_a_pass_are_followed() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Qresync).await;
        seed(&fake);
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account, &options()).await;
        {
            let mut world = fake.world();
            world.set_flags("INBOX", 9, &["\\Flagged"]);
            // While the pass fetches flag changes, another client expunges two messages and
            // appends one: VANISHED and EXISTS arrive in the middle of the pass.
            world.on("CHANGEDSINCE", |world| {
                world.expunge("INBOX", 10);
                world.expunge("INBOX", 11);
                world.append("INBOX", 0, &[]);
            });
        }
        let report = account_pass(&mut conn, &store, account, &options()).await;
        assert_eq!(report.stats.vanished, 2, "{:?}", report.stats);
        assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
        store.close().unwrap();
    });
}

#[test]
fn folders_gone_from_the_server_leave_the_store() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Qresync).await;
        seed(&fake);
        fake.world().create("Utkast", &["\\Drafts"]);
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account, &options()).await;
        let sent = folder(&store, account, "Sendt").await;
        assert_eq!(state(&store, sent).await.messages, 5);
        {
            let mut world = fake.world();
            world.delete("Sendt");
            world.on("EXAMINE \"Utkast\"", |world| world.delete("Utkast"));
            world.append("Utkast", 1, &[]);
        }
        let report = account_pass(&mut conn, &store, account, &options()).await;
        assert_eq!(report.stats.folders_removed, 1);
        assert_eq!(
            report.stats.skipped_missing, 1,
            "Utkast vanished after LIST"
        );
        let names: Vec<String> = store
            .account_folders(account)
            .recv()
            .await
            .unwrap()
            .into_iter()
            .map(|state| state.name_raw)
            .collect();
        assert!(!names.contains(&"Sendt".to_owned()));
        assert!(names.contains(&"Utkast".to_owned()));
        let report = account_pass(&mut conn, &store, account, &options()).await;
        assert_eq!(report.stats.folders_removed, 1, "Utkast goes now");
        assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
        assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
        store.close().unwrap();
    });
}

#[test]
fn the_first_sync_keeps_a_window_and_older_mail_loads_on_request() {
    runtime().block_on(async {
        for flavor in Flavor::ALL {
            let fake = Fake::start(flavor).await;
            {
                let mut world = fake.world();
                world.create("INBOX", &[]);
                world.create("Sendt", &["\\Sent"]);
                // UIDs 1-400 are 200 days old, 401-600 ten days old.
                world.append_many("INBOX", 600, |n| if n < 400 { 200 } else { 10 });
                // Nothing recent: the newest 200 are kept anyway.
                world.append_many("Sendt", 300, |_| 400);
            }
            let (_dir, store, account) = store();
            let mut conn = fake.connect().await;
            let mut options = options();
            options.older = 150;
            account_pass(&mut conn, &store, account, &options).await;
            let inbox = folder(&store, account, "INBOX").await;
            let sent = folder(&store, account, "Sendt").await;
            let state_of = |s: sealwax_infrastructure::store::FolderSync| {
                (s.floor, s.below, s.messages, s.complete())
            };
            assert_eq!(
                state_of(state(&store, inbox).await),
                (Some(401), 400, 200, false),
                "{flavor:?}"
            );
            assert_eq!(
                state_of(state(&store, sent).await),
                (Some(101), 100, 200, false),
                "{flavor:?}"
            );
            assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
            let again = account_pass(&mut conn, &store, account, &options).await;
            if flavor != Flavor::Rev1 {
                assert_eq!(again.stats.status_skips, 2, "{flavor:?}: {:?}", again.stats);
            }
            assert_eq!(again.stats.uid_diffs, 0, "{flavor:?}: {:?}", again.stats);

            // An old message below the floor goes, and new mail arrives.
            {
                let mut world = fake.world();
                world.expunge("INBOX", 50);
                world.append("INBOX", 0, &["\\Seen"]);
            }
            let report = account_pass(&mut conn, &store, account, &options).await;
            assert_eq!(report.stats.stored, 1, "{flavor:?}: {:?}", report.stats);
            assert_eq!(
                state_of(state(&store, inbox).await),
                (Some(401), 399, 201, false),
                "{flavor:?}: the UID diff counted again"
            );
            let quiet = account_pass(&mut conn, &store, account, &options).await;
            assert_eq!(quiet.stats.uid_diffs, 0, "{flavor:?}: {:?}", quiet.stats);

            let locks = FolderLocks::default();
            let mut loads = Vec::new();
            loop {
                let older = load_older(&mut conn, &store, inbox, &options, &locks)
                    .await
                    .unwrap();
                loads.push((older.fetched, older.complete));
                assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
                if older.complete {
                    break;
                }
            }
            assert_eq!(
                loads,
                [(150, false), (150, false), (99, true)],
                "{flavor:?}"
            );
            let done = state(&store, inbox).await;
            assert_eq!(state_of(done), (Some(1), 0, 600, true), "{flavor:?}");
            let older = load_older(&mut conn, &store, inbox, &options, &locks)
                .await
                .unwrap();
            assert_eq!((older.fetched, older.complete), (0, true));
            let report = account_pass(&mut conn, &store, account, &options).await;
            assert_eq!(report.stats.uid_diffs, 0, "{flavor:?}: {:?}", report.stats);
            assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
            store.close().unwrap();
        }
    });
}

#[test]
fn a_cut_first_sync_resumes_without_loss_or_duplication() {
    runtime().block_on(async {
        for flavor in Flavor::ALL {
            let fake = Fake::start(flavor).await;
            {
                let mut world = fake.world();
                world.create("INBOX", &[]);
                world.append_many("INBOX", 700, |_| 1);
                // The second batch's FETCH is cut after 120 of its 200 responses.
                world.cut("BODY.PEEK[HEADER.FIELDS", 1, 120);
            }
            let (_dir, store, account) = store();
            let mut conn = fake.connect().await;
            let cut = sync_account(
                &mut conn,
                &store,
                account,
                &options(),
                &FolderLocks::default(),
                |_, _| {},
            )
            .await
            .unwrap_err();
            assert!(
                matches!(&cut, sealwax_engine::imap::SyncError::Imap(e) if e.is_connection()),
                "{flavor:?}: {cut}"
            );
            assert_eq!(fake.world().cuts, 1);
            let inbox = folder(&store, account, "INBOX").await;
            let cut_state = state(&store, inbox).await;
            assert_eq!(
                cut_state.messages, 200,
                "{flavor:?}: the first batch committed"
            );
            assert!(matches!(
                cut_state.state,
                SyncState::Initial { floor: 501, .. }
            ));
            let mut conn = fake.connect().await;
            let report = account_pass(&mut conn, &store, account, &options()).await;
            assert_eq!(
                report.stats.stored, 500,
                "{flavor:?}: resumed, not refetched"
            );
            assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
            assert_eq!(state(&store, inbox).await.messages, 700);
            assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
            store.close().unwrap();
        }
    });
}

#[test]
fn a_cut_flag_fetch_leaves_the_store_untouched() {
    runtime().block_on(async {
        for flavor in Flavor::ALL {
            let fake = Fake::start(flavor).await;
            {
                let mut world = fake.world();
                world.create("INBOX", &[]);
                world.append_many("INBOX", 300, |_| 1);
            }
            let (_dir, store, account) = store();
            let mut conn = fake.connect().await;
            account_pass(&mut conn, &store, account, &options()).await;
            let before = snapshot::local(&store, account).await;
            {
                let mut world = fake.world();
                for uid in 1..=300 {
                    world.set_flags("INBOX", uid, &["Kuttet"]);
                }
                world.cut("(UID FLAGS)", 0, 100);
            }
            let cut = sync_account(
                &mut conn,
                &store,
                account,
                &options(),
                &FolderLocks::default(),
                |_, _| {},
            )
            .await;
            assert!(cut.is_err(), "{flavor:?}");
            assert_eq!(
                snapshot::local(&store, account).await,
                before,
                "{flavor:?}: nothing of the partial response stored"
            );
            let mut conn = fake.connect().await;
            account_pass(&mut conn, &store, account, &options()).await;
            assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
            store.close().unwrap();
        }
    });
}

#[test]
fn the_inbox_left_to_another_connection_gets_only_the_backstop() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Qresync).await;
        seed(&fake);
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account, &options()).await;
        let inbox = folder(&store, account, "INBOX").await;
        fake.world().append("INBOX", 0, &[]);
        let mut options = options();
        options.elsewhere = Some(Elsewhere {
            folder: inbox,
            backstop: false,
        });
        let skipped = account_pass(&mut conn, &store, account, &options).await;
        assert_eq!(skipped.stats.passes, 0, "{:?}", skipped.stats);
        assert_eq!(state(&store, inbox).await.messages, 30);
        options.elsewhere = Some(Elsewhere {
            folder: inbox,
            backstop: true,
        });
        let backstop = account_pass(&mut conn, &store, account, &options).await;
        assert_eq!((backstop.stats.passes, backstop.stats.stored), (1, 1));
        let quiet = account_pass(&mut conn, &store, account, &options).await;
        assert_eq!((quiet.stats.passes, quiet.stats.status_skips), (0, 2));
        store.close().unwrap();
    });
}

#[test]
fn opened_folders_sync_and_all_mail_never_by_itself() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Qresync).await;
        seed(&fake);
        {
            let mut world = fake.world();
            world.create("Alt", &["\\All"]);
            world.append_many("Alt", 4, |_| 1);
            world.create("Mappe", &["\\Noselect"]);
        }
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        let locks = FolderLocks::default();
        account_pass(&mut conn, &store, account, &options()).await;
        let plain = folder(&store, account, "Arkiv2").await;
        let all = folder(&store, account, "Alt").await;
        for id in [plain, all] {
            assert_eq!(state(&store, id).await.state, SyncState::New);
            let opened = sync_folder(&mut conn, &store, id, &options(), &locks, |_| {})
                .await
                .unwrap();
            assert_eq!(opened.stats.passes, 1);
        }
        assert_eq!(state(&store, plain).await.messages, 3);
        {
            let mut world = fake.world();
            world.append("Arkiv2", 0, &[]);
            world.append("Alt", 0, &[]);
        }
        account_pass(&mut conn, &store, account, &options()).await;
        assert_eq!(
            state(&store, plain).await.messages,
            4,
            "opened once, synced since"
        );
        assert_eq!(
            state(&store, all).await.messages,
            4,
            "\\All: only when opened"
        );
        store.close().unwrap();
    });
}

/// One random change to the fake, like S1's mutator: appends, flag changes, moves, expunges,
/// copies and flag storms over INBOX and two folders, and one delete-and-recreate.
fn mutate(world: &mut fake::World, rng: &mut Rng, reset: bool) {
    const FOLDERS: [&str; 3] = ["INBOX", "Arkiv", "Midlertidig"];
    const FLAGS: [&str; 6] = [
        "\\Seen",
        "\\Flagged",
        "\\Answered",
        "\\Deleted",
        "$Label1",
        "Prioritert",
    ];
    if reset {
        world.recreate("Midlertidig");
        world.append("Midlertidig", 1, &[]);
        return;
    }
    let name = if rng.chance(50) {
        "INBOX"
    } else {
        *rng.pick(&FOLDERS)
    };
    let uids = world.uids(name);
    let flags: Vec<&str> = FLAGS.iter().copied().filter(|_| rng.chance(30)).collect();
    match rng.below(100) {
        _ if uids.is_empty() => {
            world.append(name, rng.below(200) as i64, &flags);
        }
        0..25 => {
            world.append(name, rng.below(200) as i64, &flags);
        }
        25..55 => {
            let uid = *rng.pick(&uids);
            world.set_flags(name, uid, &flags);
        }
        55..70 => {
            let uid = *rng.pick(&uids);
            let target = *rng.pick(&FOLDERS);
            if target != name {
                world.append(target, 1, &flags);
                world.expunge(name, uid);
            }
        }
        70..85 => {
            let uid = *rng.pick(&uids);
            world.expunge(name, uid);
        }
        85..93 => {
            let target = *rng.pick(&FOLDERS);
            world.append(target, rng.below(200) as i64, &flags);
        }
        _ => {
            for uid in world.uids("INBOX") {
                world.set_flags("INBOX", uid, &["\\Flagged"]);
            }
        }
    }
}

#[test]
fn a_short_randomized_run_ends_equal_on_every_flavour() {
    runtime().block_on(async {
        for flavor in Flavor::ALL {
            for seed in 1..=4u64 {
                let fake = Fake::start(flavor).await;
                let mut rng = Rng(seed);
                {
                    let mut world = fake.world();
                    for name in ["INBOX", "Arkiv", "Midlertidig"] {
                        world.create(name, &[]);
                    }
                    world.append_many("INBOX", 120, |n| if n < 60 { 150 } else { 5 });
                    world.append_many("Arkiv", 30, |_| 3);
                    world.append_many("Midlertidig", 10, |_| 3);
                }
                let (_dir, store, account) = store();
                let mut options = options();
                options.window = Window {
                    days: 90,
                    max: 50,
                    min: 20,
                };
                options.older = 15;
                let locks = FolderLocks::default();
                let mut conn = fake.connect().await;
                let reset_at = 50 + rng.below(200);
                let mut report = account_pass(&mut conn, &store, account, &options).await;
                for step in 0..300 {
                    {
                        let mut world = fake.world();
                        mutate(&mut world, &mut rng, step == reset_at);
                        if rng.chance(10) {
                            let mut during = rng.clone();
                            during.next_u64();
                            world.on("UID FETCH", move |world| {
                                let mut during = during;
                                mutate(world, &mut during, false);
                            });
                        }
                    }
                    if rng.chance(25) {
                        report.add(account_pass(&mut conn, &store, account, &options).await);
                        for name in ["Arkiv", "Midlertidig"] {
                            let id = folder(&store, account, name).await;
                            if state(&store, id).await.uidvalidity.is_none() {
                                report.add(
                                    sync_folder(&mut conn, &store, id, &options, &locks, |_| {})
                                        .await
                                        .unwrap(),
                                );
                            }
                        }
                    }
                    if rng.chance(5) {
                        let inbox = folder(&store, account, "INBOX").await;
                        let older = load_older(&mut conn, &store, inbox, &options, &locks)
                            .await
                            .unwrap();
                        report.add(older.report);
                    }
                }
                fake.world().clear_hooks();
                report.add(account_pass(&mut conn, &store, account, &options).await);
                let mut divergences = diverged(&fake, &store, account).await;
                if !divergences.is_empty() {
                    // A change made during the last pass shows in the next one.
                    report.add(account_pass(&mut conn, &store, account, &options).await);
                    divergences = diverged(&fake, &store, account).await;
                }
                assert_eq!(
                    divergences,
                    Vec::<String>::new(),
                    "{flavor:?} seed {seed}: {:?}",
                    report.stats
                );
                assert!(report.stats.resets >= 1, "{flavor:?} seed {seed}");
                assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
                println!("{flavor:?} seed {seed}: {:?}", report.stats);
                store.close().unwrap();
            }
        }
    });
}

/// The control: a cache with one flag flipped and one row removed behind the sync's back must
/// show exactly those two divergences.
#[test]
fn the_comparison_catches_a_corrupted_cache() {
    runtime().block_on(async {
        let fake = Fake::start(Flavor::Qresync).await;
        seed(&fake);
        let (_dir, store, account) = store();
        let mut conn = fake.connect().await;
        account_pass(&mut conn, &store, account, &options()).await;
        assert_eq!(diverged(&fake, &store, account).await, Vec::<String>::new());
        let inbox = folder(&store, account, "INBOX").await;
        let rows = store.folder_messages(inbox).recv().await.unwrap();
        store
            .set_flags(vec![rows[3].id], Flags::FLAGGED, Flags::NONE)
            .recv()
            .await
            .unwrap();
        store.remove(vec![rows[7].id]).recv().await.unwrap();
        let found = diverged(&fake, &store, account).await;
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("uid 4 server") && found[1].contains("uid 8 missing"));
        store.close().unwrap();
    });
}
