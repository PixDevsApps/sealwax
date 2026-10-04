//! Randomized two-client test: a mutator makes seeded random changes while the sync client
//! (IDLE on INBOX plus a polling connection) runs; after quiescence the cache must equal the
//! server exactly.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, watch};

use crate::idle::{self, Event};
use crate::mutf7;
use crate::net::{ConnectOptions, Connection};
use crate::ops;
use crate::raw::{self, ImapError, quote};
use crate::rng::Rng;
use crate::store::Store;
use crate::sync::{self, Shared, Stats, View};
use crate::synth;
use crate::testbed::{self, Server};

/// Decoded names. INBOX takes the most traffic; `RESET` is deleted and re-created once.
pub const FOLDERS: [&str; 6] = [
    "INBOX",
    "Arkiv",
    "Papirkurv",
    "Søppelpost",
    "Prosjekter/Høst 📬 2026",
    RESET,
];
pub const RESET: &str = "Midlertidig";
const FLAGS: [&str; 7] = [
    "\\Seen",
    "\\Flagged",
    "\\Answered",
    "\\Draft",
    "\\Deleted",
    "$Label1",
    "Prioritert",
];

#[derive(Clone, Debug)]
pub struct Config {
    pub server: Server,
    pub user: String,
    pub seed: u64,
    pub ops: usize,
    pub initial_inbox: usize,
    pub initial_other: usize,
    pub poll: Duration,
    pub sabotage: sync::Sabotage,
    /// ADR-006 topology: the IDLE connection owns INBOX; the poller leaves it alone except,
    /// with `inbox_backstop`, when its STATUS shows a change.
    pub idle_owns_inbox: bool,
    pub inbox_backstop: bool,
    pub list_status: bool,
}

impl Config {
    pub fn new(server: Server, seed: u64, ops: usize) -> Config {
        Config {
            server,
            user: "rand".into(),
            seed,
            ops,
            initial_inbox: 120,
            initial_other: 40,
            poll: Duration::from_millis(250),
            sabotage: sync::Sabotage::None,
            idle_owns_inbox: true,
            inbox_backstop: true,
            list_status: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub server: &'static str,
    pub seed: u64,
    pub ops: BTreeMap<&'static str, usize>,
    pub uidvalidity_reset: Option<(u32, u32)>,
    pub divergences: Vec<String>,
    pub server_messages: usize,
    pub local_messages: usize,
    pub mutation: Duration,
    pub quiescence: Duration,
    pub idle_stats: Stats,
    pub poller_stats: Stats,
    pub idle_notices: usize,
    pub disconnects: usize,
}

pub fn wire(name: &str) -> String {
    mutf7::encode(name)
}

pub async fn ensure_mailbox(conn: &mut Connection, name: &str) -> Result<(), ImapError> {
    match raw::run(
        &mut conn.session,
        &format!("CREATE {}", quote(&wire(name))),
        |_| {},
    )
    .await
    {
        Ok(_) | Err(ImapError::No(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn empty_mailbox(conn: &mut Connection, name_raw: &str) -> Result<(), ImapError> {
    let view = ops::select(conn, name_raw).await?;
    if view.exists > 0 {
        raw::run(
            &mut conn.session,
            "UID STORE 1:* +FLAGS.SILENT (\\Deleted)",
            |_| {},
        )
        .await?;
        raw::run(&mut conn.session, "EXPUNGE", |_| {}).await?;
    }
    Ok(())
}

/// Empties every selectable mailbox of the account and makes sure the test folders exist.
pub async fn reset_account(conn: &mut Connection, folders: &[&str]) -> Result<()> {
    for folder in folders {
        ensure_mailbox(conn, folder).await?;
    }
    for entry in sync::list(conn)
        .await?
        .into_iter()
        .filter(sync::ListEntry::selectable)
    {
        empty_mailbox(conn, &entry.name_raw).await?;
    }
    ops::select(conn, "INBOX").await?;
    Ok(())
}

fn random_flags(rng: &mut Rng) -> String {
    FLAGS
        .iter()
        .filter(|_| rng.chance(25))
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn append(
    conn: &mut Connection,
    name_raw: &str,
    message_id: &str,
    rng: &mut Rng,
) -> Result<(), ImapError> {
    let flags = random_flags(rng);
    let message = synth::message(message_id, rng);
    let literal_plus = conn.caps.has("LITERAL+");
    raw::append(
        &mut conn.session,
        name_raw,
        &flags,
        &message,
        literal_plus,
        |_| {},
    )
    .await?;
    Ok(())
}

async fn uids(conn: &mut Connection, name_raw: &str) -> Result<Vec<u32>, ImapError> {
    let mut view: View = ops::select(conn, name_raw).await?;
    sync::uid_search(conn, "ALL", &mut view).await
}

fn pick_folder(rng: &mut Rng) -> &'static str {
    if rng.chance(40) {
        FOLDERS[0]
    } else {
        FOLDERS[1 + rng.below(FOLDERS.len() - 1)]
    }
}

fn pick_uids(rng: &mut Rng, uids: &[u32]) -> Vec<u32> {
    let n = 1 + rng.below(3.min(uids.len()));
    (0..n).map(|_| *rng.pick(uids)).collect()
}

async fn examine_uidvalidity(conn: &mut Connection, name: &str) -> Result<u32, ImapError> {
    Ok(sync::examine(conn, &wire(name)).await?.uidvalidity)
}

/// One mutation; returns its kind for the report.
async fn mutate(conn: &mut Connection, rng: &mut Rng, seed: u64, n: usize) -> Result<&'static str> {
    let folder = wire(pick_folder(rng));
    let roll = rng.below(100);
    let existing = uids(conn, &folder).await?;
    if roll < 20 || existing.is_empty() {
        append(conn, &folder, &format!("rand.{seed}.{n}@example.test"), rng).await?;
        return Ok("append");
    }
    let has_move = conn.caps.has("MOVE");
    let has_uidplus = conn.caps.has("UIDPLUS");
    match roll {
        20..55 => {
            let set = raw::uid_set(&pick_uids(rng, &existing));
            let sign = if rng.chance(50) { "+" } else { "-" };
            let mut flags = random_flags(rng);
            if flags.is_empty() {
                flags = (*rng.pick(&FLAGS)).to_owned();
            }
            raw::run(
                &mut conn.session,
                &format!("UID STORE {set} {sign}FLAGS ({flags})"),
                |_| {},
            )
            .await?;
            Ok("flags")
        }
        55..70 => {
            let uid = *rng.pick(&existing);
            let target = loop {
                let t = wire(pick_folder(rng));
                if t != folder {
                    break t;
                }
            };
            if has_move {
                raw::run(
                    &mut conn.session,
                    &format!("UID MOVE {uid} {}", quote(&target)),
                    |_| {},
                )
                .await?;
            } else {
                raw::run(
                    &mut conn.session,
                    &format!("UID COPY {uid} {}", quote(&target)),
                    |_| {},
                )
                .await?;
                raw::run(
                    &mut conn.session,
                    &format!("UID STORE {uid} +FLAGS.SILENT (\\Deleted)"),
                    |_| {},
                )
                .await?;
                raw::run(&mut conn.session, "EXPUNGE", |_| {}).await?;
            }
            Ok("move")
        }
        70..85 => {
            let set = raw::uid_set(&pick_uids(rng, &existing));
            raw::run(
                &mut conn.session,
                &format!("UID STORE {set} +FLAGS.SILENT (\\Deleted)"),
                |_| {},
            )
            .await?;
            let expunge = if has_uidplus {
                format!("UID EXPUNGE {set}")
            } else {
                "EXPUNGE".to_owned()
            };
            raw::run(&mut conn.session, &expunge, |_| {}).await?;
            Ok("expunge")
        }
        85..93 => {
            let uid = *rng.pick(&existing);
            let target = loop {
                let t = wire(pick_folder(rng));
                if t != folder {
                    break t;
                }
            };
            raw::run(
                &mut conn.session,
                &format!("UID COPY {uid} {}", quote(&target)),
                |_| {},
            )
            .await?;
            Ok("copy")
        }
        _ => {
            let inbox = uids(conn, "INBOX").await?;
            if inbox.is_empty() {
                return Ok("storm-skipped");
            }
            let sign = if rng.chance(50) { "+" } else { "-" };
            raw::run(
                &mut conn.session,
                &format!("UID STORE 1:* {sign}FLAGS (\\Flagged)"),
                |_| {},
            )
            .await?;
            Ok("flag-storm")
        }
    }
}

async fn uidvalidity_reset(conn: &mut Connection, rng: &mut Rng, seed: u64) -> Result<(u32, u32)> {
    let before = examine_uidvalidity(conn, RESET).await?;
    ops::select(conn, "INBOX").await?;
    raw::run(
        &mut conn.session,
        &format!("DELETE {}", quote(&wire(RESET))),
        |_| {},
    )
    .await?;
    raw::run(
        &mut conn.session,
        &format!("CREATE {}", quote(&wire(RESET))),
        |_| {},
    )
    .await?;
    for i in 0..3 {
        append(
            conn,
            &wire(RESET),
            &format!("rand.{seed}.reset{i}@example.test"),
            rng,
        )
        .await?;
    }
    let after = examine_uidvalidity(conn, RESET).await?;
    ops::select(conn, "INBOX").await?;
    Ok((before, after))
}

/// Runs one seed end to end.
pub async fn run(config: &Config) -> Result<Report> {
    let mut account = testbed::account(config.server, &config.user)?;
    account.sync.batch = 200;
    account.sync.sabotage = config.sabotage;
    account.sync.list_status = config.list_status;
    let mut admin = testbed::account(config.server, &config.user)?;
    admin.connect = ConnectOptions {
        enable_qresync: false,
        send_id: false,
    };

    let mut rng = Rng::new(config.seed);
    let mut mutator = admin.connect().await.context("mutator connect")?;
    reset_account(&mut mutator, &FOLDERS).await?;
    for i in 0..config.initial_inbox {
        append(
            &mut mutator,
            "INBOX",
            &format!("rand.{}.init{i}@example.test", config.seed),
            &mut rng,
        )
        .await?;
    }
    for i in 0..config.initial_other {
        let folder = wire(FOLDERS[1 + rng.below(FOLDERS.len() - 1)]);
        append(
            &mut mutator,
            &folder,
            &format!("rand.{}.other{i}@example.test", config.seed),
            &mut rng,
        )
        .await?;
    }

    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let (stop_tx, stop_rx) = watch::channel(false);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (passes_tx, mut passes_rx) = watch::channel(None);
    let idle_task = tokio::spawn(idle::run_inbox(
        account.clone(),
        shared.clone(),
        Duration::from_secs(300),
        config.poll,
        events_tx.clone(),
        stop_rx.clone(),
    ));
    let mut poller = account.clone();
    if config.idle_owns_inbox {
        poller.sync.elsewhere = Some(sync::Elsewhere {
            mailbox: "INBOX",
            backstop: config.inbox_backstop,
        });
    }
    let poll_task = tokio::spawn(idle::run_poller(
        poller,
        shared.clone(),
        config.poll,
        passes_tx,
        events_tx,
        stop_rx,
    ));

    let mut report = Report {
        server: config.server.name,
        seed: config.seed,
        ..Report::default()
    };
    let reset_at = config.ops / 5 + rng.below(config.ops * 3 / 5 + 1);
    let started = Instant::now();
    for n in 0..config.ops {
        let kind = if n == reset_at {
            report.uidvalidity_reset =
                Some(uidvalidity_reset(&mut mutator, &mut rng, config.seed).await?);
            "uidvalidity-reset"
        } else {
            mutate(&mut mutator, &mut rng, config.seed, n)
                .await
                .with_context(|| format!("mutation {n}"))?
        };
        *report.ops.entry(kind).or_default() += 1;
        if rng.chance(10) {
            tokio::time::sleep(Duration::from_millis(rng.below(5) as u64)).await;
        }
    }
    // A last INBOX change, so the IDLE connection gets a notice after everything else.
    append(
        &mut mutator,
        "INBOX",
        &format!("rand.{}.final@example.test", config.seed),
        &mut rng,
    )
    .await?;
    *report.ops.entry("append").or_default() += 1;
    let quiet = Instant::now();
    report.mutation = started.elapsed();
    mutator.logout().await;

    // Quiescence: a poller pass that started after the last change, and an INBOX pass that
    // started after it or 5 s of IDLE silence (the pass that applied the last change may have
    // started before its APPEND reply reached the mutator). Anything the IDLE path missed then
    // shows up as INBOX divergence below.
    let deadline = quiet + Duration::from_secs(120);
    let mut inbox_done = false;
    let mut trace = Vec::new();
    loop {
        while let Ok(event) = events_rx.try_recv() {
            trace.push(format!(
                "{:.3}s {}",
                quiet.elapsed().as_secs_f64(),
                describe(&event, quiet)
            ));
            match event {
                Event::Notice(_) => report.idle_notices += 1,
                Event::Disconnected(_) => report.disconnects += 1,
                Event::Synced { started, .. } if started > quiet => inbox_done = true,
                _ => {}
            }
        }
        let idle_settled = inbox_done || quiet.elapsed() > Duration::from_secs(5);
        if idle_settled && (*passes_rx.borrow()).is_some_and(|started: Instant| started > quiet) {
            break;
        }
        if Instant::now() > deadline {
            let poller = (*passes_rx.borrow())
                .map(|t| format!("{:.3}s", t.saturating_duration_since(quiet).as_secs_f64()));
            bail!(
                "no INBOX and account pass completed within 120 s after the last mutation \
                 (inbox pass after: {inbox_done}; last poller pass started at {poller:?}); events after the last mutation: {:?}",
                trace.iter().rev().take(20).collect::<Vec<_>>()
            );
        }
        let _ = tokio::time::timeout(Duration::from_millis(50), passes_rx.changed()).await;
    }
    report.quiescence = quiet.elapsed();
    stop_tx.send(true)?;
    report.idle_stats = idle_task.await?;
    report.poller_stats = poll_task.await?;
    while let Ok(event) = events_rx.try_recv() {
        match event {
            Event::Notice(_) => report.idle_notices += 1,
            Event::Disconnected(_) => report.disconnects += 1,
            _ => {}
        }
    }

    let mut verifier = admin.connect().await?;
    let server = sync::server_snapshot(&mut verifier).await?;
    verifier.logout().await;
    let local = shared.store().snapshot()?;
    report.server_messages = server.values().map(|f| f.messages.len()).sum();
    report.local_messages = local.values().map(|f| f.messages.len()).sum();
    report.divergences = sync::diff(&server, &local);
    Ok(report)
}

fn describe(event: &Event, quiet: Instant) -> String {
    match event {
        Event::Connected => "connected".into(),
        Event::Idling => "idling".into(),
        Event::Notice(_) => "notice".into(),
        Event::Synced { started, .. } => {
            format!(
                "synced (started {:+.3}s)",
                started.saturating_duration_since(quiet).as_secs_f64()
            )
        }
        Event::Disconnected(error) => format!("disconnected: {error}"),
    }
}
