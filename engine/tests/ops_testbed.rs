//! M4's testbed suites for the op runner (ML-051 to ML-053, phase B), end to end through the
//! engine service and the store: triage on every server (read, star, archive, junk and not
//! junk, move, delete to the Trash and for good, an op re-targeted after a UIDVALIDITY change,
//! Mark Folder as Read below the window's floor); the crash-during-queue replay (a MOVE, a COPY,
//! a STORE or an EXPUNGE that reached the server before the store recorded it, the connection
//! cut or the engine stopped hard, then retried, in the same engine or a new one on the same
//! store and blobs); the undo window (an Undo inside it never reaches the server; one after the
//! action ran sends the inverse and brings the row back with its body); and a randomized
//! two-client run with local triage. They need the Docker testbed running and seeded, and use
//! only the users `ops` (direct) and `opsfault` (through the fault proxy), whose folders they
//! empty and fill with synthetic messages:
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test ops_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod mutator;
mod proxy;
mod snapshot;
mod testbed_client;
mod testbed_engine;

use common::{DOVECOT, DOVECOT_REV1, PASSWORD, STALWART, Server};
use mutator::{client, quote, reset_account, wire};
use proxy::Proxy;
use sealwax_domain::model::{AccountId, Flags, FolderId, FolderRole, MessageId};
use sealwax_domain::sync::Window;
use sealwax_domain::triage::{FolderInfo, Plan, Triage, plan};
use sealwax_engine::api::{Command, Event, Password};
use sealwax_engine::imap::{ConnectOptions, SyncOptions};
use sealwax_engine::service::SyncConfig;
use sealwax_infrastructure::store::{FolderSync, QueueTriage, Queued, Store};
use snapshot::Rng;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use testbed_client::Client;
use testbed_engine::{TestEngine, converge, counts_converge, until};

const WAIT: Duration = Duration::from_secs(30);
/// The user's folders, decoded; the Trash and Junk folders are found by role (Stalwart's own
/// SPECIAL-USE folders take those roles there).
const FOLDERS: [&str; 5] = ["INBOX", "Arkiv", "Papirkurv", "Søppelpost", "Prosjekter"];

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// The actor's suites' short intervals; `moves` off takes the COPY path where MOVE is offered.
fn config(moves: bool) -> SyncConfig {
    SyncConfig {
        options: SyncOptions::default(),
        account_pass: Duration::from_secs(3),
        inbox_poll: Duration::from_secs(1),
        opened_poll: Duration::from_secs(1),
        idle_restart: Duration::from_secs(20),
        reuse_check: Duration::from_secs(5),
        backoff_min: Duration::from_millis(200),
        backoff_max: Duration::from_secs(2),
        connect: ConnectOptions {
            moves,
            ..ConnectOptions::default()
        },
        ..SyncConfig::default()
    }
}

/// Seeds per scenario: `SEALWAX_OPS_SEEDS`, else `default`.
fn seeds(default: u64) -> u64 {
    std::env::var("SEALWAX_OPS_SEEDS")
        .ok()
        .and_then(|seeds| seeds.parse().ok())
        .unwrap_or(default)
}

/// Appends synthetic messages to `folder` with ids `<tag>.<n>@example.test`; returns the ids.
async fn fill(
    client: &mut Client,
    folder: &str,
    tag: &str,
    count: usize,
    rng: &mut Rng,
) -> Vec<String> {
    let mut ids = Vec::new();
    for n in 0..count {
        let id = format!("{tag}.{n}@example.test");
        let raw = testbed_client::message(&id, rng);
        client.append(&wire(folder), "", &raw).await.unwrap();
        ids.push(id);
    }
    ids
}

/// A copy of a message as the server holds it.
#[derive(Clone, Debug)]
struct Found {
    folder: String,
    flags: Flags,
}

/// Every copy of every message on the server, by Message-ID.
async fn on_server(client: &mut Client) -> BTreeMap<String, Vec<Found>> {
    let mut out: BTreeMap<String, Vec<Found>> = BTreeMap::new();
    for (name, folder) in client.snapshot().await.unwrap() {
        for (flags, _, id) in folder.messages.into_values() {
            if let Some(id) = id {
                out.entry(id).or_default().push(Found {
                    folder: name.clone(),
                    flags,
                });
            }
        }
    }
    out
}

/// The copies of `id` not flagged `\Deleted`.
fn live<'a>(copies: &'a BTreeMap<String, Vec<Found>>, id: &str) -> Vec<&'a Found> {
    copies
        .get(id)
        .map(|copies| {
            copies
                .iter()
                .filter(|copy| !copy.flags.contains(Flags::DELETED))
                .collect()
        })
        .unwrap_or_default()
}

/// An account on the engine, with its folders as the store has them.
struct Account {
    folders: Vec<FolderSync>,
}

impl Account {
    async fn load(store: &Store, id: AccountId) -> Account {
        Account {
            folders: store.account_folders(id).recv().await.unwrap(),
        }
    }

    fn role(&self, role: FolderRole) -> &FolderSync {
        self.folders
            .iter()
            .find(|folder| folder.role == role && folder.selectable)
            .unwrap_or_else(|| panic!("no {role:?} folder"))
    }

    fn named(&self, name: &str) -> &FolderSync {
        let wire = wire(name);
        self.folders
            .iter()
            .find(|folder| folder.name_raw == wire)
            .unwrap_or_else(|| panic!("{name} isn't stored"))
    }

    fn wire_name(&self, folder: FolderId) -> &str {
        &self
            .folders
            .iter()
            .find(|state| state.folder == folder)
            .unwrap()
            .name_raw
    }

    fn infos(&self) -> Vec<FolderInfo> {
        self.folders
            .iter()
            .map(|folder| FolderInfo {
                id: folder.folder,
                role: folder.role,
                selectable: folder.selectable,
            })
            .collect()
    }
}

/// The store's ids of the messages it holds (not flagged `\Deleted` on the server), by
/// Message-ID.
async fn stored(store: &Store, account: AccountId) -> HashMap<String, MessageId> {
    let mut out = HashMap::new();
    for folder in store.account_folders(account).recv().await.unwrap() {
        if !folder.selectable {
            continue;
        }
        for message in store.folder_messages(folder.folder).recv().await.unwrap() {
            if message.flags.contains(Flags::DELETED) {
                continue;
            }
            let detail = store.message(message.id).recv().await.unwrap().value;
            if let Some(id) = detail.and_then(|detail| detail.headers.message_id) {
                out.insert(id, message.id);
            }
        }
    }
    out
}

/// Queues `action` as the app does and wakes the runner.
async fn queue(
    engine: &TestEngine,
    action: Triage,
    messages: Vec<MessageId>,
    window_ms: i64,
) -> Queued {
    let queued = engine
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
        engine.send(Command::OpsQueued { account: *account });
    }
    queued
}

async fn pending(store: &Store, account: AccountId) -> u32 {
    store.pending_ops(account).recv().await.unwrap()
}

/// Waits until the account has no op pending.
async fn drained(store: &Store, account: AccountId, within: Duration) -> bool {
    until(within, || async { pending(store, account).await == 0 })
        .await
        .is_some()
}

async fn check(store: &Store, what: &str) {
    let problems = store.check().recv().await.unwrap();
    assert!(problems.is_empty(), "{what}: store check: {problems:?}");
}

/// The store equals the server: its server columns (every synced folder) and the counts it
/// shows, with no op left to overlay them.
async fn equal(
    engine: &TestEngine,
    account: AccountId,
    test: &mut Client,
    required: &[String],
    what: &str,
) {
    let snapshot = test.snapshot().await.unwrap();
    let converged = converge(
        &engine.store,
        account,
        &snapshot,
        required,
        Duration::from_secs(60),
    )
    .await;
    assert!(converged.is_ok(), "{what}: {converged:?}");
    let counts = counts_converge(&engine.store, account, &snapshot, Duration::from_secs(60)).await;
    assert!(counts.is_ok(), "{what}: counts {counts:?}");
}

/// The refusals the engine reported, read from its events so far.
fn refusals(engine: &mut TestEngine) -> Vec<(String, u32)> {
    engine.drain();
    engine
        .seen
        .iter()
        .filter_map(|event| match event {
            Event::OpsRefused {
                change, messages, ..
            } => Some((format!("{change:?}"), *messages)),
            _ => None,
        })
        .collect()
}

/// Waits for the account's first "synced".
async fn synced(engine: &mut TestEngine, account: AccountId) {
    let status = engine.settled(account, Duration::from_secs(60)).await;
    assert!(
        matches!(status, Some(sealwax_engine::api::SyncStatus::Synced)),
        "{status:?}"
    );
}

/// Read, star, archive, junk and not junk, move, delete to the Trash and for good, an op
/// re-targeted after its folder was made again, and Mark Folder as Read below the floor.
fn triage_end_to_end(server: Server) {
    runtime().block_on(async {
        let mut test = client(server.tls, "ops").await;
        reset_account(&mut test, &FOLDERS).await.unwrap();
        let mut rng = Rng(31);
        let tag = format!("e2e.{}", server.name);
        let ids = fill(&mut test, "INBOX", &tag, 20, &mut rng).await;
        let mut sync = config(true);
        sync.options.window = Window {
            days: 90,
            max: 12,
            min: 12,
        };
        let mut engine = TestEngine::start(sync);
        let account = engine.add(1, "ops", server.tls).await;
        synced(&mut engine, account).await;
        let mut folders = Account::load(&engine.store, account).await;
        let projects = folders.named("Prosjekter").folder;
        engine.send(Command::FolderOpened { folder: projects });
        assert!(
            until(WAIT, || async {
                engine
                    .store
                    .folder_sync(projects)
                    .recv()
                    .await
                    .unwrap()
                    .is_some_and(|f| f.uidvalidity.is_some())
            })
            .await
            .is_some()
        );
        folders = Account::load(&engine.store, account).await;
        let (inbox, archive, junk, trash) = (
            folders.role(FolderRole::Inbox).name_raw.clone(),
            folders.role(FolderRole::Archive).name_raw.clone(),
            folders.role(FolderRole::Junk).name_raw.clone(),
            folders.role(FolderRole::Trash).name_raw.clone(),
        );
        let started = Instant::now();
        let step = |name: &'static str| {
            println!(
                "{} {name}: {:.0} ms",
                server.name,
                started.elapsed().as_secs_f64() * 1000.0
            )
        };

        let act = async |engine: &TestEngine, action: Triage, which: &[usize], window: i64| {
            let held = stored(&engine.store, account).await;
            let messages: Vec<MessageId> = which.iter().map(|n| held[&ids[*n]]).collect();
            let queued = queue(engine, action, messages, window).await;
            assert!(
                drained(&engine.store, account, WAIT).await,
                "{action:?} ran"
            );
            check(&engine.store, &format!("{action:?}")).await;
            queued
        };

        act(&engine, Triage::Read(true), &[19, 18], 0).await;
        let copies = on_server(&mut test).await;
        for n in [19, 18] {
            assert!(live(&copies, &ids[n])[0].flags.contains(Flags::SEEN));
        }
        assert!(!live(&copies, &ids[17])[0].flags.contains(Flags::SEEN));
        step("read");

        act(&engine, Triage::Star(true), &[17], 0).await;
        assert!(
            live(&on_server(&mut test).await, &ids[17])[0]
                .flags
                .contains(Flags::FLAGGED)
        );
        act(&engine, Triage::Star(false), &[17], 0).await;
        assert!(
            !live(&on_server(&mut test).await, &ids[17])[0]
                .flags
                .contains(Flags::FLAGGED)
        );
        step("star and unstar");

        act(&engine, Triage::Archive, &[16], 300).await;
        let copies = on_server(&mut test).await;
        let found = live(&copies, &ids[16]);
        assert_eq!(found.len(), 1, "{}: one live copy", server.name);
        assert_eq!(found[0].folder, archive);
        step("archive (300 ms window)");

        act(&engine, Triage::Junk, &[15], 0).await;
        let copies = on_server(&mut test).await;
        let found = live(&copies, &ids[15]);
        assert_eq!((found.len(), found[0].folder.as_str()), (1, junk.as_str()));
        assert!(found[0].flags.contains(Flags::JUNK) && !found[0].flags.contains(Flags::NOT_JUNK));
        act(&engine, Triage::Junk, &[15], 0).await;
        let copies = on_server(&mut test).await;
        let found = live(&copies, &ids[15]);
        assert_eq!((found.len(), found[0].folder.as_str()), (1, inbox.as_str()));
        assert!(found[0].flags.contains(Flags::NOT_JUNK) && !found[0].flags.contains(Flags::JUNK));
        step("junk, then not junk");

        act(&engine, Triage::MoveTo(projects), &[14, 12, 11], 0).await;
        let copies = on_server(&mut test).await;
        for n in [14, 12, 11] {
            let found = live(&copies, &ids[n]);
            assert_eq!(
                (found.len(), found[0].folder.as_str()),
                (1, wire("Prosjekter").as_str())
            );
        }
        step("move to");

        act(&engine, Triage::Delete, &[13], 0).await;
        let copies = on_server(&mut test).await;
        let found = live(&copies, &ids[13]);
        assert_eq!((found.len(), found[0].folder.as_str()), (1, trash.as_str()));
        act(&engine, Triage::Delete, &[13], 0).await;
        assert!(
            live(&on_server(&mut test).await, &ids[13]).is_empty(),
            "{}: deleted for good",
            server.name
        );
        step("delete to the Trash, then for good");

        // Star a message of Prosjekter with a window, and make the folder again meanwhile with
        // the same messages in the other order: the op lands on its message, by Message-ID.
        let held = stored(&engine.store, account).await;
        let starred = held[&ids[14]];
        let queued = queue(&engine, Triage::Star(true), vec![starred], 2_500).await;
        assert!(queued.batch.is_some());
        let name = wire("Prosjekter");
        test.select("INBOX").await.unwrap();
        test.run(&format!("DELETE {}", quote(&name)), |_| {})
            .await
            .unwrap();
        test.run(&format!("CREATE {}", quote(&name)), |_| {})
            .await
            .unwrap();
        for n in [11, 12, 14] {
            let raw = testbed_client::message(&ids[n], &mut rng);
            test.append(&name, "", &raw).await.unwrap();
        }
        assert!(
            drained(&engine.store, account, Duration::from_secs(60)).await,
            "re-targeted"
        );
        let copies = on_server(&mut test).await;
        for n in [14, 12, 11] {
            let found = live(&copies, &ids[n]);
            assert_eq!(found.len(), 1);
            assert_eq!(
                found[0].flags.contains(Flags::FLAGGED),
                n == 14,
                "{} {n}",
                server.name
            );
        }
        step("star re-targeted after Prosjekter was made again");

        let inbox_id = folders.role(FolderRole::Inbox).folder;
        let floor = engine
            .store
            .folder_sync(inbox_id)
            .recv()
            .await
            .unwrap()
            .unwrap()
            .floor;
        assert!(
            floor.is_some_and(|floor| floor > 1),
            "{}: INBOX is windowed ({floor:?})",
            server.name
        );
        let read = engine
            .store
            .mark_folder_read(inbox_id, now_ms())
            .recv()
            .await
            .unwrap();
        for account in &read.accounts {
            engine.send(Command::OpsQueued { account: *account });
        }
        assert!(drained(&engine.store, account, WAIT).await);
        let copies = on_server(&mut test).await;
        let unread: Vec<&String> = copies
            .iter()
            .filter(|(_, copies)| {
                copies.iter().any(|c| {
                    c.folder == inbox
                        && !c.flags.contains(Flags::DELETED)
                        && !c.flags.contains(Flags::SEEN)
                })
            })
            .map(|(id, _)| id)
            .collect();
        assert!(
            unread.is_empty(),
            "{}: unread left in the Inbox: {unread:?}",
            server.name
        );
        step("mark folder read, below the floor too");

        let required: Vec<String> = [&inbox, &archive, &junk, &trash]
            .iter()
            .map(|n| n.to_string())
            .collect();
        equal(&engine, account, &mut test, &required, server.name).await;
        let refused = refusals(&mut engine);
        assert!(refused.is_empty(), "{refused:?}");
        println!(
            "{}: every triage action on the server, the store equal to it, nothing refused",
            server.name
        );
        drop(engine);
        reset_account(&mut test, &FOLDERS).await.unwrap();
        test.logout().await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn triage_end_to_end_dovecot() {
    triage_end_to_end(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn triage_end_to_end_dovecot_rev1() {
    triage_end_to_end(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn triage_end_to_end_stalwart() {
    triage_end_to_end(STALWART);
}

/// What reached the server before the store could record it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Move,
    Copy,
    Store,
    Expunge,
}

/// How the engine lost the outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Crash {
    /// The connection cut before the reply: the same engine retries.
    Cut,
    /// The reply held and the engine stopped hard (its actors aborted, no LOGOUT waited for):
    /// a new engine on the same store and blobs retries.
    Stop,
}

fn kinds(server: Server) -> Vec<Kind> {
    if server == DOVECOT_REV1 {
        vec![Kind::Copy, Kind::Store, Kind::Expunge]
    } else {
        vec![Kind::Move, Kind::Copy, Kind::Store, Kind::Expunge]
    }
}

fn needle(kind: Kind, server: Server) -> &'static str {
    match kind {
        Kind::Move => "UID MOVE",
        Kind::Copy => "UID COPY",
        Kind::Store => "+FLAGS.SILENT (\\Flagged)",
        Kind::Expunge if server == DOVECOT_REV1 => "+FLAGS.SILENT (\\Deleted)",
        Kind::Expunge => "UID EXPUNGE",
    }
}

#[derive(Default)]
struct Tally {
    runs: usize,
    messages: usize,
    duplicates: usize,
    lost: usize,
    wrong: usize,
}

/// One replay: messages chosen, the rule armed, the action queued, the outcome lost, then the
/// retry; the end state checked on the server and against the store.
async fn replay(
    server: Server,
    proxy: &Proxy,
    kind: Kind,
    crash: Crash,
    seed: u64,
    tally: &mut Tally,
) {
    let mut rng = Rng(seed);
    let mut test = client(server.tls, "opsfault").await;
    reset_account(&mut test, &FOLDERS).await.unwrap();
    let tag = format!("replay.{}.{seed}", server.name);
    let ids = fill(&mut test, "INBOX", &tag, 4 + rng.below(12), &mut rng).await;
    let mut sync = config(kind != Kind::Copy);
    if crash == Crash::Stop {
        sync.logout_wait = Duration::ZERO;
    }
    let mut engine = TestEngine::start(sync);
    let account = engine.add(1, "opsfault", proxy.port).await;
    synced(&mut engine, account).await;
    let folders = Account::load(&engine.store, account).await;
    let trash = folders.role(FolderRole::Trash).name_raw.clone();
    let archive = folders.role(FolderRole::Archive).name_raw.clone();
    let mut chosen: Vec<usize> = (0..ids.len()).filter(|_| rng.chance(50)).collect();
    if chosen.is_empty() {
        chosen.push(0);
    }
    if kind == Kind::Expunge {
        // Into the Trash first, as the user would (no faults armed yet).
        let held = stored(&engine.store, account).await;
        let messages = chosen.iter().map(|n| held[&ids[*n]]).collect();
        queue(&engine, Triage::Delete, messages, 0).await;
        assert!(drained(&engine.store, account, WAIT).await);
    }
    let held = stored(&engine.store, account).await;
    let messages: Vec<MessageId> = chosen.iter().map(|n| held[&ids[*n]]).collect();
    match crash {
        Crash::Cut => proxy.lose_reply(needle(kind, server)),
        Crash::Stop => proxy.hold_reply(needle(kind, server)),
    }
    let action = match kind {
        Kind::Move | Kind::Copy => Triage::Archive,
        Kind::Store => Triage::Star(true),
        Kind::Expunge => Triage::Delete,
    };
    queue(&engine, action, messages, 0).await;
    let fired = until(WAIT, || async { !proxy.sent_armed() }).await;
    assert!(fired.is_some(), "{} {kind:?}: the rule fired", server.name);
    let cut_at = Instant::now();
    let unrecorded = pending(&engine.store, account).await;
    let engine = match crash {
        Crash::Cut => engine,
        Crash::Stop => {
            assert!(unrecorded > 0, "started, not recorded");
            let mut restarted = engine.restart(config(kind != Kind::Copy));
            let key = restarted.accounts()[0].key.clone();
            restarted.send(Command::UpdatePassword {
                request: 2,
                key,
                password: Password::new(PASSWORD.into()),
            });
            let updated = restarted
                .event(WAIT, |event| match event {
                    Event::PasswordUpdated { result, .. } => Some(result.is_ok()),
                    _ => None,
                })
                .await;
            assert_eq!(updated, Some(true));
            restarted
        }
    };
    assert!(
        drained(&engine.store, account, Duration::from_secs(60)).await,
        "{} {kind:?} {crash:?}: the retry recorded it",
        server.name
    );
    println!(
        "{} {kind:?} {crash:?} seed {seed}: {} of {} messages; {unrecorded} ops pending after the cut; recorded {:.0} ms later",
        server.name,
        chosen.len(),
        ids.len(),
        cut_at.elapsed().as_secs_f64() * 1000.0
    );
    let copies = on_server(&mut test).await;
    for (n, id) in ids.iter().enumerate() {
        let picked = chosen.contains(&n);
        let found = live(&copies, id);
        let (expected, folder) = match kind {
            Kind::Move | Kind::Copy if picked => (1, Some(&archive)),
            Kind::Expunge if picked => (0, None),
            _ => (1, None),
        };
        if picked {
            tally.messages += 1;
            tally.duplicates += found.len().saturating_sub(1);
            tally.lost += usize::from(found.len() < expected);
        }
        let in_place = folder.is_none_or(|folder| found.iter().all(|copy| &copy.folder == folder));
        let starred = found
            .iter()
            .all(|copy| copy.flags.contains(Flags::FLAGGED) == (kind == Kind::Store && picked));
        if found.len() != expected || !in_place || !starred {
            tally.wrong += 1;
            println!(
                "{} {kind:?} {crash:?} seed {seed}: {id} has {found:?}",
                server.name
            );
        }
    }
    let required = vec![archive, trash];
    equal(
        &engine,
        account,
        &mut test,
        &required,
        &format!("{} {kind:?} {crash:?} seed {seed}", server.name),
    )
    .await;
    check(&engine.store, "after the replay").await;
    tally.runs += 1;
    drop(engine);
    test.logout().await;
}

fn replay_suite(server: Server) {
    runtime().block_on(async {
        let proxy = Proxy::start(server.tls, &common::ca()).await;
        let seeds = seeds(2);
        let mut rows = Vec::new();
        for kind in kinds(server) {
            for crash in [Crash::Cut, Crash::Stop] {
                let mut tally = Tally::default();
                for seed in 1..=seeds {
                    replay(server, &proxy, kind, crash, seed, &mut tally).await;
                }
                rows.push((kind, crash, tally));
            }
        }
        let mut test = client(server.tls, "opsfault").await;
        reset_account(&mut test, &FOLDERS).await.unwrap();
        test.logout().await;
        println!("| server | change | lost outcome | runs | messages | duplicates | lost | other differences |");
        for (kind, crash, tally) in &rows {
            println!(
                "| {} | {kind:?} | {crash:?} | {} | {} | {} | {} | {} |",
                server.name, tally.runs, tally.messages, tally.duplicates, tally.lost, tally.wrong
            );
        }
        for (kind, crash, tally) in &rows {
            assert_eq!((tally.duplicates, tally.lost, tally.wrong), (0, 0, 0), "{kind:?} {crash:?}");
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn crash_replay_dovecot() {
    replay_suite(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn crash_replay_dovecot_rev1() {
    replay_suite(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn crash_replay_stalwart() {
    replay_suite(STALWART);
}

/// An Undo inside the window: the server never sees a command for the message. An Undo after
/// the archive ran: the inverse moves it back, and the row it had (re-homed on COPYUID, its
/// body downloaded) comes back with its body.
fn undo_window(server: Server) {
    runtime().block_on(async {
        let proxy = Proxy::start(server.tls, &common::ca()).await;
        let mut test = client(server.tls, "opsfault").await;
        reset_account(&mut test, &FOLDERS).await.unwrap();
        let mut rng = Rng(41);
        let ids = fill(
            &mut test,
            "INBOX",
            &format!("undo.{}", server.name),
            6,
            &mut rng,
        )
        .await;
        let mut engine = TestEngine::start(config(true));
        let account = engine.add(1, "opsfault", proxy.port).await;
        synced(&mut engine, account).await;
        let folders = Account::load(&engine.store, account).await;
        let inbox = folders.role(FolderRole::Inbox).clone();
        let archive = folders.role(FolderRole::Archive).clone();
        proxy.record();

        let held = stored(&engine.store, account).await;
        let from = proxy.sent_lines().len();
        let queued = queue(&engine, Triage::Archive, vec![held[&ids[0]]], 2_500).await;
        let undone = engine
            .store
            .undo(queued.batch.unwrap())
            .recv()
            .await
            .unwrap();
        assert_eq!((undone.restored, undone.reverted), (1, 0));
        tokio::time::sleep(Duration::from_millis(4_000)).await;
        let sent = proxy.sent_lines()[from..].to_vec();
        let writes: Vec<&String> = sent
            .iter()
            .filter(|line| {
                ["UID MOVE", "UID COPY", "UID STORE", "UID EXPUNGE"]
                    .iter()
                    .any(|w| line.starts_with(w))
            })
            .collect();
        assert!(writes.is_empty(), "{}: {writes:?}", server.name);
        let copies = on_server(&mut test).await;
        assert_eq!(live(&copies, &ids[0])[0].folder, inbox.name_raw);
        println!(
            "{}: an Undo inside the window: {} lines sent in 4 s, no write among them",
            server.name,
            sent.len()
        );

        let message = held[&ids[1]];
        engine.send(Command::FetchBodies {
            messages: vec![message],
        });
        let fetched = engine
            .event(WAIT, |event| match event {
                Event::BodyFetched {
                    message: id,
                    result,
                } if *id == message => Some(result.is_ok()),
                _ => None,
            })
            .await;
        assert_eq!(fetched, Some(true));
        let blob = engine
            .store
            .message(message)
            .recv()
            .await
            .unwrap()
            .value
            .unwrap()
            .blob;
        assert!(blob.is_some());
        let queued = queue(&engine, Triage::Archive, vec![message], 0).await;
        assert!(drained(&engine.store, account, WAIT).await);
        let detail = engine
            .store
            .message(message)
            .recv()
            .await
            .unwrap()
            .value
            .unwrap();
        assert_eq!(
            (detail.summary.folder, detail.blob),
            (archive.folder, blob),
            "{}: re-homed with its body",
            server.name
        );
        assert_eq!(
            live(&on_server(&mut test).await, &ids[1])[0].folder,
            archive.name_raw
        );
        let undone = engine
            .store
            .undo(queued.batch.unwrap())
            .recv()
            .await
            .unwrap();
        assert_eq!(
            (undone.restored, undone.reverted, undone.lost.len()),
            (0, 1, 0),
            "{}",
            server.name
        );
        for account in &undone.accounts {
            engine.send(Command::OpsQueued { account: *account });
        }
        assert!(drained(&engine.store, account, WAIT).await);
        let copies = on_server(&mut test).await;
        let found = live(&copies, &ids[1]);
        assert_eq!(found.len(), 1, "{}: {found:?}", server.name);
        assert_eq!(found[0].folder, inbox.name_raw);
        let detail = engine
            .store
            .message(message)
            .recv()
            .await
            .unwrap()
            .value
            .unwrap();
        assert_eq!(
            (detail.summary.folder, detail.blob),
            (inbox.folder, blob),
            "{}: back with its body",
            server.name
        );
        let raw = engine
            .store
            .raw_message(message)
            .recv()
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&raw).contains(&ids[1]));
        println!(
            "{}: an Undo after the archive ran moved it back; the same row, its body kept",
            server.name
        );
        let required = vec![inbox.name_raw.clone(), archive.name_raw.clone()];
        equal(&engine, account, &mut test, &required, server.name).await;
        drop(engine);
        reset_account(&mut test, &FOLDERS).await.unwrap();
        test.logout().await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn undo_window_dovecot() {
    undo_window(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn undo_window_dovecot_rev1() {
    undo_window(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn undo_window_stalwart() {
    undo_window(STALWART);
}

/// The other client's folders: its own messages move and vanish only there.
const OTHER: [&str; 2] = ["Midlertidig", "Annet"];
/// The flags the other client changes on the user's messages: none the user's triage sets.
const OTHER_FLAGS: [&str; 4] = ["\\Answered", "\\Draft", "$Label1", "Prioritert"];

/// One change by the other client, S1's mix kept off what the user's triage decides: appends
/// and flag changes in INBOX (its flags only, and flag storms), appends, moves, copies and
/// expunges of its own messages in its own folders, and once a delete-and-recreate of
/// Midlertidig.
async fn other_change(
    test: &mut Client,
    rng: &mut Rng,
    seed: u64,
    n: usize,
    reset: bool,
) -> &'static str {
    if reset {
        mutator::uidvalidity_reset(test, rng, seed).await.unwrap();
        return "uidvalidity-reset";
    }
    let roll = rng.below(100);
    if roll < 15 {
        let id = format!("other.{seed}.{n}@example.test");
        let raw = testbed_client::message(&id, rng);
        test.append("INBOX", "", &raw).await.unwrap();
        return "append";
    }
    if roll < 50 {
        test.select("INBOX").await.unwrap();
        let uids = test.uids().await.unwrap();
        if uids.is_empty() {
            return "flags";
        }
        let picks: Vec<u32> = (0..1 + rng.below(3)).map(|_| *rng.pick(&uids)).collect();
        let sign = if rng.chance(50) { "+" } else { "-" };
        let flag = rng.pick(&OTHER_FLAGS);
        test.store(&picks, &format!("{sign}FLAGS ({flag})"))
            .await
            .unwrap();
        return "flags";
    }
    if roll < 55 {
        test.select("INBOX").await.unwrap();
        let sign = if rng.chance(50) { "+" } else { "-" };
        test.run(&format!("UID STORE 1:* {sign}FLAGS (\\Answered)"), |_| {})
            .await
            .unwrap();
        return "flag-storm";
    }
    let from = wire(rng.pick(&OTHER));
    let to = wire(OTHER.iter().find(|name| wire(name) != from).unwrap());
    if roll < 70 {
        let id = format!("other.{seed}.{n}@example.test");
        let raw = testbed_client::message(&id, rng);
        test.append(&from, "", &raw).await.unwrap();
        return "append";
    }
    test.select(&from).await.unwrap();
    let uids = test.uids().await.unwrap();
    if uids.is_empty() {
        return "nothing";
    }
    let uid = *rng.pick(&uids);
    if roll < 80 {
        test.store(&[uid], "+FLAGS.SILENT (\\Deleted)")
            .await
            .unwrap();
        test.run("EXPUNGE", |_| {}).await.unwrap();
        return "expunge";
    }
    test.run(&format!("UID COPY {uid} {}", quote(&to)), |_| {})
        .await
        .unwrap();
    if roll < 92 {
        test.store(&[uid], "+FLAGS.SILENT (\\Deleted)")
            .await
            .unwrap();
        test.run("EXPUNGE", |_| {}).await.unwrap();
        return "move";
    }
    "copy"
}

/// A user's message as the triage it asked for leaves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Expected {
    /// `None` once deleted for good.
    folder: Option<FolderId>,
    flags: Flags,
}

const TRACKED: Flags = Flags::from_bits(
    Flags::SEEN.bits() | Flags::FLAGGED.bits() | Flags::JUNK.bits() | Flags::NOT_JUNK.bits(),
);

/// Applies `action` to the model as the store plans it.
fn apply(model: &mut Expected, action: Triage, infos: &[FolderInfo]) {
    let Some(folder) = model.folder else {
        return;
    };
    let Some(source) = infos.iter().find(|info| info.id == folder).copied() else {
        return;
    };
    match plan(action, source, infos) {
        Ok(Plan::Flags { add, remove }) => model.flags = model.flags.union(add).difference(remove),
        Ok(Plan::Move { to, add, remove }) => {
            model.folder = Some(to);
            model.flags = model.flags.union(add).difference(remove);
        }
        Ok(Plan::Expunge) => model.folder = None,
        Err(_) => {}
    }
    model.flags = Flags::from_bits(model.flags.bits() & TRACKED.bits());
}

/// The randomized two-client run with local triage: the other client changes the account while
/// the user queues random triage, undoing some of it right away or after its window. When it
/// has settled: no op pending, the store equal to the server, and every action that wasn't
/// refused or undone on the server.
async fn randomized(server: Server, seed: u64, changes: usize, actions: usize) -> String {
    let mut rng = Rng(seed);
    let mut test = client(server.tls, "ops").await;
    let mut setup: Vec<&str> = FOLDERS.to_vec();
    setup.extend(OTHER);
    reset_account(&mut test, &setup).await.unwrap();
    let ids = fill(&mut test, "INBOX", &format!("user.{seed}"), 30, &mut rng).await;
    fill(
        &mut test,
        OTHER[0],
        &format!("other.{seed}.start"),
        10,
        &mut rng,
    )
    .await;
    let mut engine = TestEngine::start(config(true));
    let account = engine.add(1, "ops", server.tls).await;
    synced(&mut engine, account).await;
    let mut folders = Account::load(&engine.store, account).await;
    let projects = folders.named("Prosjekter").folder;
    engine.send(Command::FolderOpened { folder: projects });
    until(WAIT, || async {
        engine
            .store
            .folder_sync(projects)
            .recv()
            .await
            .unwrap()
            .is_some_and(|f| f.uidvalidity.is_some())
    })
    .await;
    folders = Account::load(&engine.store, account).await;
    let infos = folders.infos();
    let inbox = folders.role(FolderRole::Inbox).folder;
    let mut model: BTreeMap<String, Expected> = ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                Expected {
                    folder: Some(inbox),
                    flags: Flags::NONE,
                },
            )
        })
        .collect();

    let mut other_client = client(server.tls, "ops").await;
    let mut other_rng = Rng(seed ^ 0x5eed);
    let reset_at = changes / 2;
    let other = async move {
        let mut tally: BTreeMap<&'static str, usize> = BTreeMap::new();
        for n in 0..changes {
            let kind =
                other_change(&mut other_client, &mut other_rng, seed, n, n == reset_at).await;
            *tally.entry(kind).or_default() += 1;
            tokio::time::sleep(Duration::from_millis(20 + other_rng.below(60) as u64)).await;
        }
        other_client.logout().await;
        tally
    };
    let user = async {
        let mut asked: BTreeMap<&'static str, usize> = BTreeMap::new();
        for _ in 0..actions {
            let held = stored(&engine.store, account).await;
            let candidates: Vec<&String> = model
                .iter()
                .filter(|(id, expected)| {
                    expected.folder.is_some() && held.contains_key(id.as_str())
                })
                .map(|(id, _)| id)
                .collect();
            if candidates.is_empty() {
                break;
            }
            let count = 1 + rng.below(3.min(candidates.len()));
            let mut picked: Vec<String> = Vec::new();
            while picked.len() < count {
                let id = rng.pick(&candidates).to_string();
                if !picked.contains(&id) {
                    picked.push(id);
                }
            }
            let (action, name) = match rng.below(10) {
                0 => (Triage::Read(true), "read"),
                1 => (Triage::Read(false), "unread"),
                2 => (Triage::Star(true), "star"),
                3 => (Triage::Star(false), "unstar"),
                4 | 5 => (Triage::Archive, "archive"),
                6 => (Triage::Delete, "delete"),
                7 | 8 => (Triage::Junk, "junk"),
                _ => (Triage::MoveTo(projects), "move"),
            };
            let window = if rng.chance(50) {
                0
            } else {
                200 + rng.below(800) as i64
            };
            let before: Vec<(String, Expected)> =
                picked.iter().map(|id| (id.clone(), model[id])).collect();
            let messages = picked.iter().map(|id| held[id]).collect();
            let queued = queue(&engine, action, messages, window).await;
            for id in &picked {
                apply(model.get_mut(id).unwrap(), action, &infos);
            }
            *asked.entry(name).or_default() += 1;
            // The toast's Undo, for single messages: at once, or after the action may have run.
            if picked.len() == 1 && rng.chance(25) {
                if rng.chance(50) {
                    tokio::time::sleep(Duration::from_millis(rng.below(1_500) as u64)).await;
                }
                if let Some(batch) = queued.batch {
                    let undone = engine.store.undo(batch).recv().await.unwrap();
                    for account in &undone.accounts {
                        engine.send(Command::OpsQueued { account: *account });
                    }
                    // What Undo couldn't put back (deleted for good) stays as the action left it.
                    if undone.lost.is_empty() {
                        for (id, was) in &before {
                            model.insert(id.clone(), *was);
                        }
                    }
                    *asked.entry("undo").or_default() += 1;
                }
            }
            tokio::time::sleep(Duration::from_millis(30 + rng.below(120) as u64)).await;
        }
        asked
    };
    let (changed, asked) = tokio::join!(other, user);
    assert!(
        drained(&engine.store, account, Duration::from_secs(90)).await,
        "{} seed {seed}: ops drained",
        server.name
    );
    let required: Vec<String> = [
        FolderRole::Inbox,
        FolderRole::Archive,
        FolderRole::Junk,
        FolderRole::Trash,
    ]
    .iter()
    .map(|role| folders.role(*role).name_raw.clone())
    .collect();
    equal(
        &engine,
        account,
        &mut test,
        &required,
        &format!("{} seed {seed}", server.name),
    )
    .await;
    check(&engine.store, "after the randomized run").await;
    let refused = refusals(&mut engine);
    let copies = on_server(&mut test).await;
    let mut differences = Vec::new();
    for (id, expected) in &model {
        let found = live(&copies, id);
        let ok = match expected.folder {
            None => found.is_empty(),
            Some(folder) => {
                found.len() == 1
                    && found[0].folder == folders.wire_name(folder)
                    && Flags::from_bits(found[0].flags.bits() & TRACKED.bits()) == expected.flags
            }
        };
        if !ok {
            differences.push(format!("{id}: expected {expected:?}, server {found:?}"));
        }
    }
    assert!(
        refused.is_empty(),
        "{} seed {seed}: refused {refused:?}",
        server.name
    );
    assert!(
        differences.is_empty(),
        "{} seed {seed}: {differences:#?}",
        server.name
    );
    drop(engine);
    format!(
        "{} seed {seed}: 0 differences; the other client {changed:?}; the user {asked:?}; 0 refused",
        server.name
    )
}

fn randomized_suite(server: Server) {
    runtime().block_on(async {
        let mut lines = Vec::new();
        for seed in 1..=seeds(2) {
            lines.push(randomized(server, seed, 120, 40).await);
        }
        let mut test = client(server.tls, "ops").await;
        let mut setup: Vec<&str> = FOLDERS.to_vec();
        setup.extend(OTHER);
        reset_account(&mut test, &setup).await.unwrap();
        test.logout().await;
        for line in lines {
            println!("{line}");
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_triage_dovecot() {
    randomized_suite(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_triage_dovecot_rev1() {
    randomized_suite(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_triage_stalwart() {
    randomized_suite(STALWART);
}
