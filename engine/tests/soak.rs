//! The sync soak for ML-049 (M3 phase B): the engine service, with the production intervals,
//! keeps four testbed accounts through S1's fault proxy: rand on Dovecot, idle on the IMAP4rev1
//! Dovecot (no IDLE: INBOX is polled), fault on Stalwart, all three changed by a slow other
//! client, and perf on Stalwart (10,000 messages, read only). Meanwhile the user opens folders,
//! asks for older mail and reads messages, and faults come every few minutes: every connection
//! cut; a black hole of a few minutes lifted with `Reconnect` (the network monitor) or without
//! (a route that came back unannounced); a suspend (every connection silently dead for a while,
//! then `Reconnect`, as after logind's resume). Every check interval and at the end, the changes
//! and faults pause and every account's cache is compared with an independent read of its
//! server. It logs counters only: no names, subjects or addresses.
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem SEALWAX_SOAK_MINUTES=1440 \
//!   cargo test -p sealwax-engine --release --test soak -- --ignored --nocapture
//!
//! `SEALWAX_SOAK_MINUTES` (30), `SEALWAX_SOAK_CHECK_MINUTES` (60), `SEALWAX_SOAK_SEED` (1). It
//! resets and changes the users rand, idle and fault, and only reads perf.

mod common;
mod mutator;
mod proxy;
mod snapshot;
mod testbed_client;
mod testbed_engine;

use common::{DOVECOT, DOVECOT_REV1, STALWART, Server};
use mutator::{FOLDERS, append, client, mutate, reset_account, uidvalidity_reset, wire};
use proxy::Proxy;
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_engine::api::{Command, Event, SyncStatus};
use sealwax_engine::service::SyncConfig;
use sealwax_infrastructure::store::Store;
use snapshot::Rng;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use testbed_engine::{TestEngine, converge, status_name};
use tokio::sync::{RwLock, watch};

fn minutes(name: &str, default: u64) -> Duration {
    let minutes = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(default as f64);
    Duration::from_secs_f64(minutes * 60.0)
}

/// Counters, shared by the tasks; printed at every check.
#[derive(Debug, Default)]
struct Counters {
    ops: BTreeMap<&'static str, usize>,
    faults: BTreeMap<&'static str, usize>,
    events: BTreeMap<&'static str, usize>,
    asked: BTreeMap<&'static str, usize>,
}

type Shared = Arc<Mutex<Counters>>;

fn count(
    counters: &Shared,
    pick: impl FnOnce(&mut Counters) -> &mut BTreeMap<&'static str, usize>,
    kind: &'static str,
) {
    let mut counters = counters.lock().unwrap();
    *pick(&mut counters).entry(kind).or_default() += 1;
}

struct Account {
    user: &'static str,
    server: Server,
    changed: bool,
    id: AccountId,
}

impl Account {
    fn required(&self) -> Vec<String> {
        if self.changed {
            FOLDERS.iter().map(|name| wire(name)).collect()
        } else {
            vec!["INBOX".to_owned()]
        }
    }
}

async fn seed_account(server: Server, user: &str, seed: u64) {
    let mut rng = Rng(seed);
    let mut client = client(server.tls, user).await;
    reset_account(&mut client, &FOLDERS).await.unwrap();
    for i in 0..120 {
        append(
            &mut client,
            "INBOX",
            &format!("soak.{seed}.init{i}@example.test"),
            &mut rng,
        )
        .await
        .unwrap();
    }
    for i in 0..40 {
        let folder = wire(FOLDERS[1 + rng.below(FOLDERS.len() - 1)]);
        append(
            &mut client,
            &folder,
            &format!("soak.{seed}.other{i}@example.test"),
            &mut rng,
        )
        .await
        .unwrap();
    }
    client.logout().await;
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

/// The other client: one change every two seconds or so, a delete-and-recreate now and then.
async fn changer(
    server: Server,
    user: &'static str,
    seed: u64,
    gate: Arc<RwLock<()>>,
    counters: Shared,
    mut stop: watch::Receiver<bool>,
) {
    let mut rng = Rng(seed);
    let mut client = None;
    let mut n = 0usize;
    loop {
        let pause = Duration::from_millis(1000 + rng.below(2000) as u64);
        if sleep_or_stop(pause, &mut stop).await {
            break;
        }
        let _open = gate.read().await;
        let mutator = match client.as_mut() {
            Some(mutator) => mutator,
            None => client.insert(mutator::client(server.tls, user).await),
        };
        n += 1;
        let result = if rng.chance(1) {
            uidvalidity_reset(mutator, &mut rng, seed)
                .await
                .map(|_| "uidvalidity-reset")
        } else {
            mutate(mutator, &mut rng, seed, n).await
        };
        match result {
            Ok(kind) => count(&counters, |c| &mut c.ops, kind),
            Err(_) => {
                count(&counters, |c| &mut c.ops, "client-error");
                client = None;
            }
        }
    }
    if let Some(client) = client {
        client.logout().await;
    }
}

/// The user: opens folders the account doesn't sync yet and, now and then, another one; asks
/// for older INBOX mail; reads a few messages.
async fn user(
    store: Arc<Store>,
    accounts: Vec<AccountId>,
    commands: async_channel::Sender<Command>,
    seed: u64,
    counters: Shared,
    mut stop: watch::Receiver<bool>,
) {
    let wanted: Vec<String> = FOLDERS.iter().map(|name| wire(name)).collect();
    let mut rng = Rng(seed);
    let mut tick = 0u64;
    while !sleep_or_stop(Duration::from_secs(5), &mut stop).await {
        tick += 1;
        for &account in &accounts {
            let Ok(states) = store.account_folders(account).recv().await else {
                return;
            };
            for state in &states {
                if wanted.contains(&state.name_raw)
                    && state.selectable
                    && state.uidvalidity.is_none()
                {
                    let _ = commands.try_send(Command::FolderOpened {
                        folder: state.folder,
                    });
                    count(&counters, |c| &mut c.asked, "open-unsynced");
                }
            }
            let selectable: Vec<FolderId> = states
                .iter()
                .filter(|state| state.selectable)
                .map(|state| state.folder)
                .collect();
            if tick.is_multiple_of(6) && rng.chance(50) && !selectable.is_empty() {
                let _ = commands.try_send(Command::FolderOpened {
                    folder: *rng.pick(&selectable),
                });
                count(&counters, |c| &mut c.asked, "open");
            }
            let inbox = states.iter().find(|state| state.name_raw == "INBOX");
            if let Some(inbox) = inbox {
                if tick.is_multiple_of(12) {
                    let _ = commands.try_send(Command::LoadOlder {
                        folder: inbox.folder,
                    });
                    count(&counters, |c| &mut c.asked, "older");
                }
                if tick.is_multiple_of(12)
                    && let Ok(messages) = store.folder_messages(inbox.folder).recv().await
                    && !messages.is_empty()
                {
                    let picked: Vec<MessageId> = (0..3).map(|_| rng.pick(&messages).id).collect();
                    let _ = commands.try_send(Command::FetchBodies { messages: picked });
                    count(&counters, |c| &mut c.asked, "bodies");
                }
            }
        }
    }
}

/// Faults every two to five minutes, on every proxy at once.
async fn faults(
    proxies: Vec<Arc<Proxy>>,
    commands: async_channel::Sender<Command>,
    seed: u64,
    gate: Arc<RwLock<()>>,
    counters: Shared,
    mut stop: watch::Receiver<bool>,
) {
    let mut rng = Rng(seed);
    let hole = |on: bool| {
        for proxy in &proxies {
            proxy.black_hole(on);
        }
    };
    loop {
        let wait = Duration::from_secs(120 + rng.below(180) as u64);
        if sleep_or_stop(wait, &mut stop).await {
            break;
        }
        let _open = gate.read().await;
        let roll = rng.below(100);
        match roll {
            0..30 => {
                for proxy in &proxies {
                    proxy.cut_all();
                }
                count(&counters, |c| &mut c.faults, "cut");
            }
            30..75 => {
                hole(true);
                tokio::time::sleep(Duration::from_secs(60 + rng.below(120) as u64)).await;
                hole(false);
                if roll < 55 {
                    let _ = commands.try_send(Command::Reconnect);
                    count(&counters, |c| &mut c.faults, "hole-then-reconnect");
                } else {
                    count(&counters, |c| &mut c.faults, "hole-unannounced");
                }
            }
            _ => {
                hole(true);
                tokio::time::sleep(Duration::from_secs(10 + rng.below(50) as u64)).await;
                hole(false);
                let _ = commands.try_send(Command::Reconnect);
                count(&counters, |c| &mut c.faults, "suspend");
            }
        }
    }
}

/// Counts the engine's events by kind, statuses by name.
async fn events(events: async_channel::Receiver<Event>, counters: Shared) {
    while let Ok(event) = events.recv().await {
        let kind = match &event {
            Event::AccountStatus { status, .. } => status_name(*status),
            Event::SyncProgress { .. } => "progress",
            Event::FoldersSynced { .. } => "folders-synced",
            Event::OlderLoaded { .. } => "older-loaded",
            Event::BodyFetched { result: Ok(()), .. } => "body-ok",
            Event::BodyFetched { result: Err(_), .. } => "body-failed",
            _ => "other",
        };
        count(&counters, |c| &mut c.events, kind);
    }
}

/// Open file descriptors and resident memory, from /proc.
fn resources() -> (usize, u64) {
    let fds = std::fs::read_dir("/proc/self/fd").map_or(0, Iterator::count);
    let rss = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4096 / (1024 * 1024));
    (fds, rss)
}

/// Pauses the changes and faults, then waits until every account matches its server.
async fn check(
    label: &str,
    started: Instant,
    accounts: &[Account],
    store: &Store,
    gate: &RwLock<()>,
    counters: &Shared,
    proxies: &[Arc<Proxy>],
) -> usize {
    let paused = Instant::now();
    let _closed = gate.write().await;
    let waited = paused.elapsed();
    let mut divergent = 0;
    let mut lines = Vec::new();
    for account in accounts {
        let mut verifier = client(account.server.tls, account.user).await;
        let server = verifier.snapshot().await.unwrap();
        verifier.logout().await;
        let converged = converge(
            store,
            account.id,
            &server,
            &account.required(),
            Duration::from_secs(600),
        )
        .await;
        let messages: usize = server
            .get("INBOX")
            .map_or(0, |folder| folder.messages.len());
        match converged {
            Ok(took) => lines.push(format!(
                "account {}: converged after {:.1} s, INBOX {messages} on the server",
                account.id.0,
                took.as_secs_f64()
            )),
            Err(differences) => {
                divergent += 1;
                lines.push(format!(
                    "account {}: {} differences after 600 s (first: {:?})",
                    account.id.0,
                    differences.len(),
                    differences.first()
                ));
            }
        }
    }
    let integrity = store.check().recv().await.unwrap();
    let (fds, rss) = resources();
    let connections: Vec<usize> = proxies.iter().map(|proxy| proxy.accepted()).collect();
    let cuts: Vec<usize> = proxies.iter().map(|proxy| proxy.fired()).collect();
    let counters = counters.lock().unwrap();
    println!(
        "soak {label} at {:.1} min (paused after {:.1} s): {}; store check {} problems; \
         {fds} fds, {rss} MiB resident; proxy connections {connections:?}, cuts {cuts:?}; \
         ops {:?}; faults {:?}; asked {:?}; events {:?}",
        started.elapsed().as_secs_f64() / 60.0,
        waited.as_secs_f64(),
        lines.join("; "),
        integrity.len(),
        counters.ops,
        counters.faults,
        counters.asked,
        counters.events,
    );
    divergent + usize::from(!integrity.is_empty())
}

#[test]
#[ignore = "soak (ML-049): needs the testbed; SEALWAX_SOAK_MINUTES sets its length"]
fn soak() {
    let length = minutes("SEALWAX_SOAK_MINUTES", 30);
    let every = minutes("SEALWAX_SOAK_CHECK_MINUTES", 60);
    let seed = std::env::var("SEALWAX_SOAK_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1u64);
    sealwax_engine::runtime().unwrap().block_on(async move {
        let plan = [
            ("rand", DOVECOT, true),
            ("idle", DOVECOT_REV1, true),
            ("fault", STALWART, true),
            ("perf", STALWART, false),
        ];
        for (user, server, changed) in plan {
            if changed {
                seed_account(server, user, seed).await;
            }
        }
        let ca = common::ca();
        let mut proxies: BTreeMap<u16, Arc<Proxy>> = BTreeMap::new();
        for server in [DOVECOT, DOVECOT_REV1, STALWART] {
            proxies.insert(server.tls, Arc::new(Proxy::start(server.tls, &ca).await));
        }
        let mut engine = TestEngine::start(SyncConfig::default());
        let mut accounts = Vec::new();
        for (n, (user, server, changed)) in plan.into_iter().enumerate() {
            let id = engine
                .add(n as u64 + 1, user, proxies[&server.tls].port)
                .await;
            accounts.push(Account {
                user,
                server,
                changed,
                id,
            });
        }
        for account in &accounts {
            assert_eq!(
                engine.status(account.id, Duration::from_secs(300)).await,
                Some(SyncStatus::Synced),
                "account {}'s first pass",
                account.id.0
            );
        }
        let started = Instant::now();
        let counters: Shared = Arc::default();
        let gate = Arc::new(RwLock::new(()));
        let (stop_tx, stop_rx) = watch::channel(false);
        let proxies: Vec<Arc<Proxy>> = proxies.into_values().collect();
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(events(
            engine.events.clone(),
            counters.clone(),
        )));
        for (n, account) in accounts.iter().enumerate().filter(|(_, a)| a.changed) {
            tasks.push(tokio::spawn(changer(
                account.server,
                account.user,
                seed * 100 + n as u64,
                gate.clone(),
                counters.clone(),
                stop_rx.clone(),
            )));
        }
        tasks.push(tokio::spawn(user(
            engine.store.clone(),
            accounts.iter().map(|account| account.id).collect(),
            engine.commands(),
            seed + 7,
            counters.clone(),
            stop_rx.clone(),
        )));
        tasks.push(tokio::spawn(faults(
            proxies.clone(),
            engine.commands(),
            seed + 13,
            gate.clone(),
            counters.clone(),
            stop_rx.clone(),
        )));

        let mut problems = 0;
        let mut next = every;
        while started.elapsed() < length {
            let left = length.saturating_sub(started.elapsed());
            let until_check = next.saturating_sub(started.elapsed());
            if until_check < left {
                tokio::time::sleep(until_check).await;
                problems += check(
                    "check",
                    started,
                    &accounts,
                    &engine.store,
                    &gate,
                    &counters,
                    &proxies,
                )
                .await;
                next += every;
            } else {
                tokio::time::sleep(left).await;
            }
        }
        stop_tx.send(true).unwrap();
        // The event counter ends with the engine; the others with the stop.
        let events_task = tasks.remove(0);
        for task in tasks {
            task.await.unwrap();
        }
        problems += check(
            "end",
            started,
            &accounts,
            &engine.store,
            &gate,
            &counters,
            &proxies,
        )
        .await;
        let stop = engine.stop();
        let _ = tokio::time::timeout(Duration::from_secs(5), events_task).await;
        println!(
            "soak: {:.1} min, {problems} problems; the engine stopped in {:.0} ms",
            started.elapsed().as_secs_f64() / 60.0,
            stop.as_secs_f64() * 1000.0
        );
        assert_eq!(problems, 0);
    });
}
