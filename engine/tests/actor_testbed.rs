//! M3 phase B's testbed suites for the per-account actor (ML-040, ML-042, ML-045), through the
//! engine service: S1's randomized two-client test in the full layout (an IDLE connection on
//! INBOX, the account passes with the STATUS backstop, INBOX polled where the server has no
//! IDLE, opened folders and older mail asked for as the user would), new-mail latency through
//! IDLE, and faults through S1's proxy: a cut while idling, a cut in the middle of a FETCH, and
//! a black hole lifted with `Reconnect`. They need the Docker testbed running and seeded and are
//! skipped otherwise. They reset and change the users rand, fault and idle:
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test actor_testbed -- --ignored --test-threads=1 --nocapture
//!
//! The randomized runs pause between changes, 20 ms on average, so passes, IDLE wakes and an IDLE
//! issued again interleave with them; `SEALWAX_RANDOM_PACE_MS=0` gives S1's bursts instead (the
//! same changes in about 3 s).

mod common;
mod mutator;
mod proxy;
mod snapshot;
mod testbed_client;
mod testbed_engine;

use common::{DOVECOT, DOVECOT_REV1, STALWART, Server};
use mutator::{FOLDERS, append, client, mutate, reset_account, uidvalidity_reset, wire};
use proxy::{Proxy, Rule};
use sealwax_domain::model::{AccountId, FolderId};
use sealwax_domain::sync::Window;
use sealwax_engine::api::{Command, SyncStatus};
use sealwax_engine::imap::{ConnectOptions, SyncOptions};
use sealwax_engine::service::SyncConfig;
use sealwax_infrastructure::store::Store;
use snapshot::Rng;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use testbed_engine::{TestEngine, converge, counts_converge, statuses, tally, until};
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

/// The production layout with intervals short enough that a 1000-change run sees every
/// mechanism several times: account passes every 3 s, the opened folder and a no-IDLE INBOX
/// every second, IDLE issued again every 20 s, a NOOP before reusing a connection idle for 5 s;
/// and phase A's narrow window, so older mail is part of every run.
fn layout(qresync: bool) -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            window: Window {
                days: 90,
                max: 60,
                min: 20,
            },
            older: 25,
            ..SyncOptions::default()
        },
        account_pass: Duration::from_secs(3),
        inbox_poll: Duration::from_secs(1),
        opened_poll: Duration::from_secs(1),
        idle_restart: Duration::from_secs(20),
        reuse_check: Duration::from_secs(5),
        connect: ConnectOptions {
            qresync,
            ..ConnectOptions::default()
        },
        ..SyncConfig::default()
    }
}

/// The mean pause between two changes of a randomized run, in milliseconds.
fn pace() -> usize {
    std::env::var("SEALWAX_RANDOM_PACE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20)
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

#[derive(Debug, Default)]
struct Asked {
    opened: usize,
    older: usize,
}

/// The user: opens every test folder not synced yet (again after 3 s if it still isn't), and
/// asks for INBOX's older mail every 2 s, as scrolling to the end would.
async fn user(
    store: Arc<Store>,
    account: AccountId,
    commands: async_channel::Sender<Command>,
    mut stop: watch::Receiver<bool>,
) -> Asked {
    let wanted: Vec<String> = FOLDERS.iter().map(|name| wire(name)).collect();
    let mut asked = Asked::default();
    let mut last: HashMap<FolderId, Instant> = HashMap::new();
    let mut older_at = Instant::now();
    while !sleep_or_stop(Duration::from_millis(300), &mut stop).await {
        let Ok(states) = store.account_folders(account).recv().await else {
            break;
        };
        for state in &states {
            if !wanted.contains(&state.name_raw)
                || !state.selectable
                || state.uidvalidity.is_some()
                || last
                    .get(&state.folder)
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(3))
            {
                continue;
            }
            last.insert(state.folder, Instant::now());
            let _ = commands.try_send(Command::FolderOpened {
                folder: state.folder,
            });
            asked.opened += 1;
        }
        if older_at.elapsed() >= Duration::from_secs(2)
            && let Some(inbox) = states.iter().find(|state| state.name_raw == "INBOX")
        {
            older_at = Instant::now();
            let _ = commands.try_send(Command::LoadOlder {
                folder: inbox.folder,
            });
            asked.older += 1;
        }
    }
    asked
}

#[derive(Debug, Default)]
struct Report {
    ops: BTreeMap<&'static str, usize>,
    reset: Option<(u32, u32)>,
    divergences: Vec<String>,
    server_messages: usize,
    local_messages: usize,
    mutation: Duration,
    quiescence: Option<Duration>,
    /// From the cache matching until the counts shown were the server's too.
    counts: Option<Duration>,
    asked: Asked,
    events: BTreeMap<&'static str, usize>,
    stop: Duration,
}

/// Runs one seed end to end: reset and seed the account, add it to an engine with an empty
/// store, make `ops` random changes (one of them a delete-and-recreate) while the user opens
/// folders and asks for older mail, then wait until the cache matches an independent read of
/// the server, and then until the counts shown are its counts.
async fn randomized(variant: Variant, seed: u64, ops: usize) -> Report {
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

    let mut engine = TestEngine::start(layout(variant.qresync));
    let account = engine.add(seed, "rand", variant.server.tls).await;
    assert_eq!(
        engine.status(account, Duration::from_secs(60)).await,
        Some(SyncStatus::Synced),
        "{} seed {seed}: the first pass",
        variant.name()
    );
    let (stop_tx, stop_rx) = watch::channel(false);
    let user = tokio::spawn(user(
        engine.store.clone(),
        account,
        engine.commands(),
        stop_rx,
    ));

    let mut report = Report::default();
    let reset_at = ops / 5 + rng.below(ops * 3 / 5 + 1);
    let pace = pace();
    let mut pauses = Rng(seed ^ 0x5eed);
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
        // The bursts' short pauses draw from `rng` either way, so every pace makes the same
        // changes.
        let short = rng.chance(10).then(|| rng.below(5));
        let pause = if pace > 0 {
            pauses.below(2 * pace + 1)
        } else {
            short.unwrap_or(0)
        };
        if pause > 0 {
            tokio::time::sleep(Duration::from_millis(pause as u64)).await;
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
    report.mutation = started.elapsed();
    mutator.logout().await;

    let quiet = Instant::now();
    let mut verifier = client(variant.server.tls, "rand").await;
    let server = verifier.snapshot().await.unwrap();
    verifier.logout().await;
    let required: Vec<String> = FOLDERS.iter().map(|name| wire(name)).collect();
    let converged = converge(
        &engine.store,
        account,
        &server,
        &required,
        Duration::from_secs(120),
    )
    .await;
    match converged {
        Ok(_) => report.quiescence = Some(quiet.elapsed()),
        Err(differences) => report.divergences = differences,
    }
    if report.divergences.is_empty() {
        match counts_converge(&engine.store, account, &server, Duration::from_secs(30)).await {
            Ok(settled) => report.counts = Some(settled),
            Err(differences) => report.divergences = differences,
        }
    }
    stop_tx.send(true).unwrap();
    report.asked = user.await.unwrap();
    engine.drain();
    report.events = tally(&engine.seen);
    let local = snapshot::local(&engine.store, account).await;
    report.server_messages = server.values().map(|f| f.messages.len()).sum();
    report.local_messages = local.values().map(|f| f.messages.len()).sum();
    assert_eq!(
        engine.store.check().recv().await.unwrap(),
        Vec::<String>::new()
    );
    report.stop = engine.stop();
    report
}

fn randomized_suite(variant: Variant) {
    runtime().block_on(async {
        let mut failures = Vec::new();
        let mut quiescence = Vec::new();
        for seed in 1..=10 {
            let report = randomized(variant, seed, 1000).await;
            println!(
                "{} seed {seed}: {} divergences; server {} / local {} messages; mutation {:.1} s, \
                 quiescence {}, counts {}; opened {} / older {} asked; events {:?}; stop {:.0} ms; \
                 ops {:?}; UIDVALIDITY {:?}",
                variant.name(),
                report.divergences.len(),
                report.server_messages,
                report.local_messages,
                report.mutation.as_secs_f64(),
                report
                    .quiescence
                    .map_or("none".to_owned(), |q| format!("{:.2} s", q.as_secs_f64())),
                report
                    .counts
                    .map_or("none".to_owned(), |c| format!("+{:.2} s", c.as_secs_f64())),
                report.asked.opened,
                report.asked.older,
                report.events,
                report.stop.as_secs_f64() * 1000.0,
                report.ops,
                report.reset,
            );
            assert!(
                report.reset.is_some_and(|(before, after)| before != after),
                "seed {seed}: no UIDVALIDITY change"
            );
            if let Some(q) = report.quiescence {
                quiescence.push(q.as_secs_f64());
            }
            if !report.divergences.is_empty() {
                failures.push((seed, report.divergences));
            }
        }
        quiescence.sort_by(f64::total_cmp);
        println!(
            "{} (pace {} ms): {} of 10 seeds converged; quiescence median {:.2} s, max {:.2} s",
            variant.name(),
            pace(),
            quiescence.len(),
            quiescence
                .get(quiescence.len() / 2)
                .copied()
                .unwrap_or(f64::NAN),
            quiescence.last().copied().unwrap_or(f64::NAN),
        );
        assert!(failures.is_empty(), "{}: {failures:?}", variant.name());
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_full_layout_dovecot() {
    randomized_suite(Variant {
        server: DOVECOT,
        qresync: true,
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_full_layout_dovecot_condstore_only() {
    randomized_suite(Variant {
        server: DOVECOT,
        qresync: false,
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_full_layout_dovecot_rev1() {
    randomized_suite(Variant {
        server: DOVECOT_REV1,
        qresync: true,
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn randomized_full_layout_stalwart() {
    randomized_suite(Variant {
        server: STALWART,
        qresync: true,
    });
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// New mail through IDLE with the production intervals: from the APPEND's OK until the store
/// holds the message, 20 times, polled every 5 ms.
fn idle_latency(server: Server) {
    runtime().block_on(async {
        let mut appender = client(server.tls, "idle").await;
        reset_account(&mut appender, &["INBOX"]).await.unwrap();
        let mut engine = TestEngine::start(SyncConfig::default());
        let account = engine.add(1, "idle", server.tls).await;
        assert_eq!(
            engine.status(account, Duration::from_secs(60)).await,
            Some(SyncStatus::Synced)
        );
        let inbox = engine.folder(account, "INBOX").await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut rng = Rng(31);
        let mut samples = Vec::new();
        for i in 0..20u32 {
            append(
                &mut appender,
                "INBOX",
                &format!("idle.latency.{i}@example.test"),
                &mut rng,
            )
            .await
            .unwrap();
            let store = engine.store.clone();
            let seen = until(Duration::from_secs(10), || {
                let store = store.clone();
                async move {
                    store
                        .folder_sync(inbox)
                        .recv()
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|sync| sync.messages > i)
                }
            })
            .await;
            samples.push(seen.map_or(f64::INFINITY, ms));
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        appender.logout().await;
        let stop = engine.stop();
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        println!(
            "{} IDLE latency, APPEND OK to stored, 20 samples: median {:.0} ms, p95 {:.0} ms, \
             max {:.0} ms; all {samples:.0?}; stop {:.0} ms",
            server.name,
            sorted[10],
            sorted[18],
            sorted[19],
            ms(stop)
        );
        assert!(sorted[19] <= 5000.0, "{}: over 5 s", server.name);
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn idle_latency_dovecot() {
    idle_latency(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn idle_latency_stalwart() {
    idle_latency(STALWART);
}

/// The fault user's INBOX with `count` messages and an empty Arkiv.
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

async fn deliver(server: Server, count: usize, tag: &str) {
    let mut client = client(server.tls, "fault").await;
    let mut rng = Rng(count as u64);
    for i in 0..count {
        append(
            &mut client,
            "INBOX",
            &format!("fault.{tag}.{i}@example.test"),
            &mut rng,
        )
        .await
        .unwrap();
    }
    client.logout().await;
}

async fn server_snapshot(server: Server) -> BTreeMap<String, snapshot::Folder> {
    let mut client = client(server.tls, "fault").await;
    let snapshot = client.snapshot().await.unwrap();
    client.logout().await;
    snapshot
}

/// The fault user through a proxy, synced, with the production intervals.
async fn through_proxy(server: Server, count: usize, seed: u64) -> (Proxy, TestEngine, AccountId) {
    prepare(server, count, seed).await;
    let proxy = Proxy::start(server.tls, &common::ca()).await;
    let mut engine = TestEngine::start(SyncConfig::default());
    let account = engine.add(1, "fault", proxy.port).await;
    assert_eq!(
        engine.status(account, Duration::from_secs(60)).await,
        Some(SyncStatus::Synced),
        "{}",
        server.name
    );
    // The IDLE connection (or the poller's wait) is in place.
    tokio::time::sleep(Duration::from_secs(1)).await;
    (proxy, engine, account)
}

async fn inbox_holds(
    engine: &TestEngine,
    account: AccountId,
    count: u32,
    within: Duration,
) -> Option<Duration> {
    let inbox = engine.folder(account, "INBOX").await.unwrap();
    let store = engine.store.clone();
    until(within, || {
        let store = store.clone();
        async move {
            store
                .folder_sync(inbox)
                .recv()
                .await
                .ok()
                .flatten()
                .is_some_and(|sync| sync.messages == count)
        }
    })
    .await
}

/// Every connection cut while the account idles (or, without IDLE, waits to poll), then five
/// messages delivered: the actor connects again and stores them.
async fn cut_while_idle(server: Server, idle: bool) {
    let (proxy, mut engine, account) = through_proxy(server, 50, 41).await;
    let accepted = proxy.accepted();
    proxy.cut_all();
    deliver(server, 5, "cut").await;
    let within = if idle {
        Duration::from_secs(10)
    } else {
        Duration::from_secs(90)
    };
    let stored = inbox_holds(&engine, account, 55, within).await;
    let converged = converge(
        &engine.store,
        account,
        &server_snapshot(server).await,
        &["INBOX".to_owned()],
        Duration::from_secs(30),
    )
    .await;
    engine.drain();
    println!(
        "{} cut while {}: {} connections cut, {} new ones; the 5 messages stored after {}; \
         converged {:?}; statuses {:?}",
        server.name,
        if idle { "idling" } else { "waiting to poll" },
        proxy.fired(),
        proxy.accepted() - accepted,
        stored.map_or("never".to_owned(), |t| format!("{:.0} ms", ms(t))),
        converged.as_ref().map(|t| format!("{:.0} ms", ms(*t))),
        statuses(&engine.seen, account),
    );
    assert!(proxy.fired() >= 1, "{}: nothing was cut", server.name);
    assert!(
        stored.is_some(),
        "{}: not stored within {within:?}",
        server.name
    );
    assert!(converged.is_ok(), "{}: {converged:?}", server.name);
    engine.stop();
}

/// A cut 20 bytes into the 150th FETCH response line while 300 new messages arrive: nothing is
/// lost or stored twice.
async fn cut_mid_fetch(server: Server) {
    let (proxy, mut engine, account) = through_proxy(server, 200, 42).await;
    proxy.arm(Rule {
        needle: b" FETCH (".to_vec(),
        nth: 150,
        extra: 20,
    });
    deliver(server, 300, "fetch").await;
    let fired = until(Duration::from_secs(90), || {
        let fired = proxy.fired();
        async move { fired >= 1 }
    })
    .await;
    let stored = inbox_holds(&engine, account, 500, Duration::from_secs(90)).await;
    let converged = converge(
        &engine.store,
        account,
        &server_snapshot(server).await,
        &["INBOX".to_owned()],
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(
        engine.store.check().recv().await.unwrap(),
        Vec::<String>::new()
    );
    engine.drain();
    println!(
        "{} cut mid-FETCH of new mail: cut {}, all 500 stored {}; converged {:?}; statuses {:?}",
        server.name,
        fired.is_some(),
        stored.map_or("never".to_owned(), |t| format!(
            "{:.0} ms after the cut",
            ms(t)
        )),
        converged.as_ref().map(|t| format!("{:.0} ms", ms(*t))),
        statuses(&engine.seen, account),
    );
    assert!(fired.is_some(), "{}: the rule never fired", server.name);
    assert!(
        stored.is_some() && converged.is_ok(),
        "{}: {converged:?}",
        server.name
    );
    engine.stop();
}

/// A black hole (every connection silent, new ones unanswered) for 5 s while ten messages
/// arrive; lifted, `Reconnect` as the app sends it when the network is back: converged within
/// 10 s.
async fn black_hole_then_reconnect(server: Server) {
    let (proxy, mut engine, account) = through_proxy(server, 50, 43).await;
    proxy.black_hole(true);
    tokio::time::sleep(Duration::from_secs(3)).await;
    deliver(server, 10, "hole").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let still = engine
        .count(engine.folder(account, "INBOX").await.unwrap())
        .await;
    proxy.black_hole(false);
    engine.drain();
    let before = engine.seen.len();
    let accepted = proxy.accepted();
    engine.send(Command::Reconnect);
    let reconnected = Instant::now();
    let stored = inbox_holds(&engine, account, 60, Duration::from_secs(30)).await;
    let converged = converge(
        &engine.store,
        account,
        &server_snapshot(server).await,
        &["INBOX".to_owned()],
        Duration::from_secs(30),
    )
    .await;
    let total = reconnected.elapsed();
    engine.drain();
    println!(
        "{} black hole 5 s, then Reconnect: {still} of 60 stored before; all stored {} after \
         Reconnect; converged (checked) by {:.0} ms; {} new connections; statuses after {:?}",
        server.name,
        stored.map_or("never".to_owned(), |t| format!("{:.0} ms", ms(t))),
        ms(total),
        proxy.accepted() - accepted,
        statuses(&engine.seen[before..], account),
    );
    assert_eq!(still, 50, "{}: stored during the black hole", server.name);
    assert!(
        stored.is_some_and(|t| t <= Duration::from_secs(10)),
        "{}: not within 10 s",
        server.name
    );
    assert!(converged.is_ok(), "{}: {converged:?}", server.name);
    engine.stop();
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot() {
    runtime().block_on(async {
        cut_while_idle(DOVECOT, true).await;
        cut_mid_fetch(DOVECOT).await;
        black_hole_then_reconnect(DOVECOT).await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_dovecot_rev1() {
    runtime().block_on(async {
        cut_while_idle(DOVECOT_REV1, false).await;
        cut_mid_fetch(DOVECOT_REV1).await;
        black_hole_then_reconnect(DOVECOT_REV1).await;
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn faults_stalwart() {
    runtime().block_on(async {
        cut_while_idle(STALWART, true).await;
        cut_mid_fetch(STALWART).await;
        black_hole_then_reconnect(STALWART).await;
    });
}
