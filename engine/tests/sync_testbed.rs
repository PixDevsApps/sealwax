//! M3's testbed suites for the sync core (ML-041, ML-043): S1's randomized two-client test with
//! a polling driver (account passes in a loop on one connection; phase B adds the IDLE
//! connection), two controls that must diverge, the mid-FETCH faults through S1's proxy, and
//! the re-sync and first-sync measurements on the perf account. They need the Docker testbed
//! running and seeded and are skipped otherwise. They reset and change the users rand and fault
//! and only read perf:
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test sync_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod mutator;
mod proxy;
mod snapshot;
mod testbed_client;

use common::{DOVECOT, DOVECOT_REV1, PASSWORD, STALWART, Server};
use mutator::{FOLDERS, append, client, mutate, reset_account, uidvalidity_reset, wire};
use proxy::{Proxy, Rule};
use sealwax_domain::account::{Endpoint, Security};
use sealwax_domain::model::{AccountId, FolderId};
use sealwax_domain::sync::Window;
use sealwax_engine::imap::{
    self, ConnectOptions, Connection, Credentials, Elsewhere, FolderLocks, ImapError, Stats,
    SyncError, SyncOptions, load_older, sync_account, sync_folder,
};
use sealwax_infrastructure::store::Store;
use snapshot::Rng;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

/// A server, with or without QRESYNC enabled (Dovecot without it is the CONDSTORE-only path).
#[derive(Clone, Copy, Debug)]
struct Variant {
    server: Server,
    qresync: bool,
}

impl Variant {
    fn name(&self) -> String {
        if self.qresync {
            self.server.name.to_owned()
        } else {
            format!("{} without QRESYNC", self.server.name)
        }
    }
}

fn endpoint(port: u16) -> Endpoint {
    Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    }
}

async fn engine_connect(port: u16, user: &str, qresync: bool) -> Result<Connection, ImapError> {
    imap::connect_with(
        &endpoint(port),
        Credentials::Password {
            username: user,
            password: PASSWORD,
        },
        common::trust().client_config().unwrap(),
        ConnectOptions {
            qresync,
            ..ConnectOptions::default()
        },
    )
    .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    None,
    /// INBOX is synced once, then left to an IDLE connection that doesn't exist, with no
    /// backstop: it must diverge.
    InboxLeftAlone,
}

#[derive(Debug, Default)]
struct Report {
    ops: BTreeMap<&'static str, usize>,
    reset: Option<(u32, u32)>,
    divergences: Vec<String>,
    /// Synced folders whose counts shown aren't the server's.
    counts: Vec<String>,
    server_messages: usize,
    local_messages: usize,
    mutation: Duration,
    quiescence: Duration,
    stats: Stats,
    passes: u32,
    disconnects: u32,
}

/// What the polling driver did.
#[derive(Debug, Default)]
struct Driven {
    stats: Stats,
    passes: u32,
    disconnects: u32,
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

async fn sleep_or_stop(duration: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = stopped(stop) => true,
    }
}

const POLL: Duration = Duration::from_millis(100);

async fn inbox_of(store: &Store, account: AccountId) -> Option<FolderId> {
    store
        .account_folders(account)
        .recv()
        .await
        .ok()?
        .into_iter()
        .find(|state| state.name_raw.eq_ignore_ascii_case("INBOX"))
        .map(|state| state.folder)
}

/// Account passes in a loop on one connection, reconnecting after errors. After every pass the
/// "user" opens the test folders the policy leaves alone, and every fifth pass loads older INBOX
/// mail. Publishes the start time of every completed round.
#[allow(clippy::too_many_arguments)]
async fn drive(
    variant: Variant,
    store: Arc<Store>,
    account: AccountId,
    mut options: SyncOptions,
    control: Control,
    mut stop: watch::Receiver<bool>,
    done: watch::Sender<Option<Instant>>,
) -> Driven {
    let opened: Vec<String> = FOLDERS.iter().map(|name| wire(name)).collect();
    let locks = FolderLocks::default();
    let mut driven = Driven::default();
    'connection: while !*stop.borrow() {
        let mut conn = match engine_connect(variant.server.tls, "rand", variant.qresync).await {
            Ok(conn) => conn,
            Err(_) => {
                driven.disconnects += 1;
                if sleep_or_stop(POLL, &mut stop).await {
                    break;
                }
                continue;
            }
        };
        loop {
            let started = Instant::now();
            match sync_account(&mut conn, &store, account, &options, &locks, |_, _| {}).await {
                Ok(report) => driven.stats.add(&report.stats),
                Err(_) => {
                    driven.disconnects += 1;
                    continue 'connection;
                }
            }
            if control == Control::InboxLeftAlone && options.elsewhere.is_none() {
                options.elsewhere = inbox_of(&store, account).await.map(|folder| Elsewhere {
                    folder,
                    backstop: false,
                });
            }
            let states = store.account_folders(account).recv().await.unwrap();
            for state in states {
                if !opened.contains(&state.name_raw)
                    || state.uidvalidity.is_some()
                    || !state.selectable
                    || control == Control::InboxLeftAlone && state.name_raw == "INBOX"
                {
                    continue;
                }
                match sync_folder(&mut conn, &store, state.folder, &options, &locks, |_| {}).await {
                    Ok(report) => driven.stats.add(&report.stats),
                    Err(SyncError::Imap(ImapError::No(_))) => {}
                    Err(_) => {
                        driven.disconnects += 1;
                        continue 'connection;
                    }
                }
            }
            driven.passes += 1;
            if driven.passes.is_multiple_of(5)
                && control == Control::None
                && let Some(inbox) = inbox_of(&store, account).await
            {
                match load_older(&mut conn, &store, inbox, &options, &locks).await {
                    Ok(older) => driven.stats.add(&older.report.stats),
                    Err(_) => {
                        driven.disconnects += 1;
                        continue 'connection;
                    }
                }
            }
            let _ = done.send(Some(started));
            if sleep_or_stop(POLL, &mut stop).await {
                conn.logout().await;
                break 'connection;
            }
        }
    }
    driven
}

/// The counts the synced folders show against the server's, checked again after each completed
/// round for at most 20 rounds, since they follow the account passes' STATUS; what still
/// differs.
async fn counts_settled(
    store: &Store,
    account: AccountId,
    server: &BTreeMap<String, snapshot::Folder>,
    done: &mut watch::Receiver<Option<Instant>>,
) -> Vec<String> {
    let mut rounds = 0;
    loop {
        let local = snapshot::local(store, account).await;
        let left = snapshot::count_diff(server, &local, &snapshot::shown(store, account).await);
        if left.is_empty() || rounds == 20 {
            return left;
        }
        rounds += 1;
        let _ = tokio::time::timeout(Duration::from_secs(5), done.changed()).await;
    }
}

/// Runs one seed end to end: reset and seed the account, start the driver on an empty store,
/// make `ops` random changes (one of them a delete-and-recreate), wait for a pass that started
/// after the last change, then compare the cache with an independent read of the server, and
/// the counts shown with its counts.
async fn randomized(variant: Variant, seed: u64, ops: usize, control: Control) -> Report {
    let mut rng = Rng(seed);
    let mut mutator = client(variant.server.tls, "rand").await;
    reset_account(&mut mutator, &FOLDERS).await.unwrap();
    for i in 0..120 {
        append(
            &mut mutator,
            "INBOX",
            &format!("rand.{seed}.init{i}@example.test"),
            &mut rng,
        )
        .await
        .unwrap();
    }
    for i in 0..40 {
        let folder = wire(FOLDERS[1 + rng.below(FOLDERS.len() - 1)]);
        append(
            &mut mutator,
            &folder,
            &format!("rand.{seed}.other{i}@example.test"),
            &mut rng,
        )
        .await
        .unwrap();
    }

    let (_dir, store, account) = common::store();
    let store = Arc::new(store);
    // A window narrower than INBOX's 120 messages, and older mail in small batches, so the
    // window, its floor and load_older are part of every run.
    let options = SyncOptions {
        window: Window {
            days: 90,
            max: 60,
            min: 20,
        },
        older: 25,
        ..SyncOptions::default()
    };
    let (stop_tx, stop_rx) = watch::channel(false);
    let (done_tx, mut done_rx) = watch::channel(None);
    let driver = tokio::spawn(drive(
        variant,
        store.clone(),
        account,
        options,
        control,
        stop_rx,
        done_tx,
    ));

    let mut report = Report::default();
    let reset_at = ops / 5 + rng.below(ops * 3 / 5 + 1);
    let started = Instant::now();
    for n in 0..ops {
        let kind = if n == reset_at {
            report.reset = Some(
                uidvalidity_reset(&mut mutator, &mut rng, seed)
                    .await
                    .unwrap(),
            );
            "uidvalidity-reset"
        } else {
            mutate(&mut mutator, &mut rng, seed, n)
                .await
                .unwrap_or_else(|error| panic!("mutation {n}: {error:?}"))
        };
        *report.ops.entry(kind).or_default() += 1;
        if rng.chance(10) {
            tokio::time::sleep(Duration::from_millis(rng.below(5) as u64)).await;
        }
    }
    append(
        &mut mutator,
        "INBOX",
        &format!("rand.{seed}.final@example.test"),
        &mut rng,
    )
    .await
    .unwrap();
    *report.ops.entry("append").or_default() += 1;
    let quiet = Instant::now();
    report.mutation = started.elapsed();
    mutator.logout().await;

    let deadline = quiet + Duration::from_secs(120);
    loop {
        if (*done_rx.borrow()).is_some_and(|started: Instant| started > quiet) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{} seed {seed}: no pass completed within 120 s after the last change",
            variant.name()
        );
        let _ = tokio::time::timeout(Duration::from_millis(50), done_rx.changed()).await;
    }
    report.quiescence = quiet.elapsed();
    let mut verifier = client(variant.server.tls, "rand").await;
    let server = verifier.snapshot().await.unwrap();
    verifier.logout().await;
    if control == Control::None {
        report.counts = counts_settled(&store, account, &server, &mut done_rx).await;
    }
    stop_tx.send(true).unwrap();
    let driven = driver.await.unwrap();
    report.stats = driven.stats;
    report.passes = driven.passes;
    report.disconnects = driven.disconnects;

    let local = snapshot::local(&store, account).await;
    report.server_messages = server.values().map(|f| f.messages.len()).sum();
    report.local_messages = local.values().map(|f| f.messages.len()).sum();
    report.divergences = snapshot::diff(&server, &local);
    for name in FOLDERS {
        if !local.get(&wire(name)).is_some_and(|folder| folder.synced) {
            report.divergences.push(format!("{name}: not synced"));
        }
    }
    assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
    Arc::try_unwrap(store).ok().unwrap().close().unwrap();
    report
}

fn randomized_suite(variant: Variant) {
    runtime().block_on(async {
        let mut failures = Vec::new();
        let mut totals = Stats::default();
        for seed in 1..=10 {
            let report = randomized(variant, seed, 1000, Control::None).await;
            println!(
                "{} seed {seed}: {} divergences, {} count mismatches; server {} / local {} \
                 messages; mutation {:.1} s, quiescence {:.2} s; {} rounds, {} reconnects; \
                 ops {:?}; UIDVALIDITY {:?}; {:?}",
                variant.name(),
                report.divergences.len(),
                report.counts.len(),
                report.server_messages,
                report.local_messages,
                report.mutation.as_secs_f64(),
                report.quiescence.as_secs_f64(),
                report.passes,
                report.disconnects,
                report.ops,
                report.reset,
                report.stats
            );
            assert!(
                report.reset.is_some_and(|(before, after)| before != after),
                "seed {seed}: no UIDVALIDITY change"
            );
            totals.add(&report.stats);
            if !report.divergences.is_empty() || !report.counts.is_empty() {
                failures.push((seed, [report.divergences, report.counts].concat()));
            }
        }
        println!("{} totals: {totals:?}", variant.name());
        assert!(failures.is_empty(), "{}: {failures:?}", variant.name());
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_two_client_dovecot() {
    randomized_suite(Variant {
        server: DOVECOT,
        qresync: true,
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_two_client_dovecot_rev1() {
    randomized_suite(Variant {
        server: DOVECOT_REV1,
        qresync: true,
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_two_client_stalwart() {
    randomized_suite(Variant {
        server: STALWART,
        qresync: true,
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_two_client_dovecot_condstore_only() {
    randomized_suite(Variant {
        server: DOVECOT,
        qresync: false,
    });
}

/// The controls: a cache whose INBOX nobody keeps current, and a corrupted cache, must diverge.
#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn controls_diverge() {
    runtime().block_on(async {
        for server in [DOVECOT, DOVECOT_REV1, STALWART] {
            let variant = Variant {
                server,
                qresync: true,
            };
            let report = randomized(variant, 1, 1000, Control::InboxLeftAlone).await;
            let inbox = report
                .divergences
                .iter()
                .filter(|d| d.starts_with("INBOX"))
                .count();
            println!(
                "{} control, INBOX left alone: {} divergences ({inbox} in INBOX)",
                variant.name(),
                report.divergences.len()
            );
            assert!(
                inbox > 0,
                "{}: the check missed a stale INBOX",
                variant.name()
            );
        }
    });
}

/// The fault account's INBOX with `count` messages and an empty Arkiv.
async fn prepare(server: Server, count: usize, seed: u64) {
    let mut client = client(server.tls, "fault").await;
    reset_account(&mut client, &["INBOX", "Arkiv"])
        .await
        .unwrap();
    let mut rng = Rng(seed);
    for i in 0..count {
        append(
            &mut client,
            "INBOX",
            &format!("fault.{seed}.{i}@example.test"),
            &mut rng,
        )
        .await
        .unwrap();
    }
    client.logout().await;
}

async fn fault_diff(server: Server, store: &Store, account: AccountId) -> Vec<String> {
    let mut client = client(server.tls, "fault").await;
    let snapshot = client.snapshot().await.unwrap();
    client.logout().await;
    snapshot::diff(&snapshot, &snapshot::local(store, account).await)
}

async fn pass_through(
    proxy: &Proxy,
    store: &Store,
    account: AccountId,
) -> Result<imap::SyncReport, SyncError> {
    let mut conn = engine_connect(proxy.port, "fault", true).await?;
    let result = sync_account(
        &mut conn,
        store,
        account,
        &SyncOptions::default(),
        &FolderLocks::default(),
        |_, _| {},
    )
    .await;
    if result.is_ok() {
        conn.logout().await;
    }
    result
}

fn cut_by_the_proxy(result: &Result<imap::SyncReport, SyncError>) -> bool {
    matches!(result, Err(SyncError::Imap(error)) if error.is_connection())
}

async fn inbox_count(store: &Store, account: AccountId) -> u32 {
    let inbox = inbox_of(store, account).await.unwrap();
    store
        .folder_sync(inbox)
        .recv()
        .await
        .unwrap()
        .unwrap()
        .messages
}

/// Cut in the second batch of a first sync: the retry resumes from the committed floor.
async fn mid_fetch_first_sync(server: Server) {
    prepare(server, 1200, 11).await;
    let proxy = Proxy::start(server.tls, &common::ca()).await;
    let (_dir, store, account) = common::store();
    proxy.arm(Rule {
        needle: b" FETCH (".to_vec(),
        nth: 300,
        extra: 37,
    });
    let first = pass_through(&proxy, &store, account).await;
    let committed = inbox_count(&store, account).await;
    let retry = pass_through(&proxy, &store, account).await.unwrap();
    let divergences = fault_diff(server, &store, account).await;
    println!(
        "{} mid-FETCH, first sync: cut {}, {committed} committed before the cut, the retry \
         stored {}, {} divergences",
        server.name,
        cut_by_the_proxy(&first),
        retry.stats.stored,
        divergences.len()
    );
    assert!(cut_by_the_proxy(&first) && proxy.fired() == 1);
    assert_eq!(
        (committed, retry.stats.stored),
        (200, 1000),
        "{}",
        server.name
    );
    assert_eq!(divergences, Vec::<String>::new());
    assert_eq!(inbox_count(&store, account).await, 1200);
    store.close().unwrap();
}

/// Cut in the flag FETCH of an incremental pass (CHANGEDSINCE, or the full scan without
/// CONDSTORE): nothing of the partial response is stored, and the retry converges.
async fn mid_fetch_flags(server: Server) {
    prepare(server, 1200, 12).await;
    let proxy = Proxy::start(server.tls, &common::ca()).await;
    let (_dir, store, account) = common::store();
    pass_through(&proxy, &store, account).await.unwrap();
    let before = snapshot::local(&store, account).await;
    let mut client = client(server.tls, "fault").await;
    client.select("INBOX").await.unwrap();
    client
        .run("UID STORE 1:* +FLAGS.SILENT (Kuttet)", |_| {})
        .await
        .unwrap();
    client.logout().await;
    proxy.arm(Rule {
        needle: b" FETCH (".to_vec(),
        nth: 300,
        extra: 10,
    });
    let first = pass_through(&proxy, &store, account).await;
    let untouched = snapshot::local(&store, account).await == before;
    pass_through(&proxy, &store, account).await.unwrap();
    let divergences = fault_diff(server, &store, account).await;
    println!(
        "{} mid-FETCH, flags: cut {}, store untouched {untouched}, {} divergences after the \
         retry",
        server.name,
        cut_by_the_proxy(&first),
        divergences.len()
    );
    assert!(cut_by_the_proxy(&first) && untouched, "{}", server.name);
    assert_eq!(divergences, Vec::<String>::new());
    store.close().unwrap();
}

/// Cut in the first batch of new mail: the batch isn't stored, and the retry stores it once.
async fn mid_fetch_new_mail(server: Server) {
    prepare(server, 200, 13).await;
    let proxy = Proxy::start(server.tls, &common::ca()).await;
    let (_dir, store, account) = common::store();
    pass_through(&proxy, &store, account).await.unwrap();
    let mut client = client(server.tls, "fault").await;
    let mut rng = Rng(14);
    for i in 0..300 {
        append(
            &mut client,
            "INBOX",
            &format!("fault.new.{i}@example.test"),
            &mut rng,
        )
        .await
        .unwrap();
    }
    client.logout().await;
    proxy.arm(Rule {
        needle: b" FETCH (".to_vec(),
        nth: 150,
        extra: 20,
    });
    let first = pass_through(&proxy, &store, account).await;
    let after_cut = inbox_count(&store, account).await;
    let retry = pass_through(&proxy, &store, account).await.unwrap();
    let divergences = fault_diff(server, &store, account).await;
    println!(
        "{} mid-FETCH, new mail: cut {}, {after_cut} stored after the cut, the retry stored {}, \
         {} divergences",
        server.name,
        cut_by_the_proxy(&first),
        retry.stats.stored,
        divergences.len()
    );
    assert!(cut_by_the_proxy(&first));
    assert_eq!(
        (after_cut, retry.stats.stored),
        (200, 300),
        "{}",
        server.name
    );
    assert_eq!(divergences, Vec::<String>::new());
    store.close().unwrap();
}

fn fault_suite(server: Server) {
    runtime().block_on(async {
        mid_fetch_first_sync(server).await;
        mid_fetch_flags(server).await;
        mid_fetch_new_mail(server).await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot() {
    fault_suite(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot_rev1() {
    fault_suite(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_stalwart() {
    fault_suite(STALWART);
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn summary(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    (samples[samples.len() / 2], samples[samples.len() - 1])
}

/// S1's re-sync bars on the perf account (10,000 messages in INBOX, all older than 90 days):
/// a no-change account pass ≤ 200 ms with CONDSTORE and ≤ 2 s without, with the default window
/// (INBOX keeps its newest 200) and with the whole folder. Also the first account pass in both.
/// Release build.
#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn resync_speed_on_the_perf_account() {
    runtime().block_on(async {
        for server in [DOVECOT, STALWART, DOVECOT_REV1] {
            for (label, window) in [("window", Window::DEFAULT), ("whole", Window::ALL)] {
                let options = SyncOptions {
                    window,
                    ..SyncOptions::default()
                };
                let locks = FolderLocks::default();
                let (_dir, store, account) = common::store();
                let started = Instant::now();
                let mut conn = engine_connect(server.tls, "perf", true).await.unwrap();
                let first = sync_account(&mut conn, &store, account, &options, &locks, |_, _| {})
                    .await
                    .unwrap();
                let first_ms = ms(started.elapsed());
                let inbox = inbox_of(&store, account).await.unwrap();
                let state = store.folder_sync(inbox).recv().await.unwrap().unwrap();
                let expected = if window == Window::ALL { 10_000 } else { 200 };
                assert_eq!(state.messages, expected, "{} {label}", server.name);
                assert_eq!(
                    state.messages + state.below,
                    10_000,
                    "{} {label}",
                    server.name
                );
                let mut account_ms = Vec::new();
                let mut passes = 0;
                for _ in 0..20 {
                    let started = Instant::now();
                    let report =
                        sync_account(&mut conn, &store, account, &options, &locks, |_, _| {})
                            .await
                            .unwrap();
                    account_ms.push(ms(started.elapsed()));
                    passes += report.stats.passes;
                    assert_eq!(report.stats.uid_diffs, 0);
                    assert_eq!(report.stats.stored, 0);
                }
                conn.logout().await;
                let mut reconnect_ms = Vec::new();
                for _ in 0..5 {
                    let started = Instant::now();
                    let mut conn = engine_connect(server.tls, "perf", true).await.unwrap();
                    sync_account(&mut conn, &store, account, &options, &locks, |_, _| {})
                        .await
                        .unwrap();
                    reconnect_ms.push(ms(started.elapsed()));
                    conn.logout().await;
                }
                let (p50, max) = summary(&mut account_ms);
                let (reconnect_p50, reconnect_max) = summary(&mut reconnect_ms);
                println!(
                    "{} {label}: first account pass {first_ms:.1} ms ({} stored, {} batches); \
                     no-change account pass p50 {p50:.1} ms, max {max:.1} ms ({passes} folder \
                     passes in 20 runs); reconnect + account p50 {reconnect_p50:.1} ms, max \
                     {reconnect_max:.1} ms",
                    server.name, first.stats.stored, first.stats.first_sync_batches
                );
                let bar = if server == DOVECOT_REV1 {
                    2_000.0
                } else {
                    200.0
                };
                assert!(max <= bar, "{} {label}: {max:.1} ms", server.name);
                store.close().unwrap();
            }
        }
    });
}
