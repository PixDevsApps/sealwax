//! Fault injection: the proxy cuts the connection mid-FETCH, mid-IDLE and mid-APPEND; after
//! reconnecting and re-syncing nothing may be lost or duplicated.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, watch};

use crate::idle::{self, Account, Event};
use crate::net::{ConnectOptions, Endpoint, Security};
use crate::ops;
use crate::proxy::{Proxy, Rule};
use crate::random::{append, reset_account};
use crate::raw::{self, ImapError};
use crate::rng::Rng;
use crate::store::Store;
use crate::sync::{self, Shared, Stats};
use crate::testbed::{self, Server};

#[derive(Debug)]
pub struct Outcome {
    pub name: &'static str,
    pub passed: bool,
    pub notes: Vec<String>,
}

struct Bench {
    proxy: Proxy,
    direct: Account,
    proxied: Account,
}

impl Bench {
    async fn new(server: Server, user: &str, batch: usize) -> Result<Bench> {
        let upstream: SocketAddr = format!("127.0.0.1:{}", server.tls_port).parse()?;
        let (cert, key) = testbed::cert_paths();
        let proxy =
            Proxy::start(upstream, crate::tls::server(&cert, &key)?, testbed::tls()?).await?;
        let mut direct = testbed::account(server, user)?;
        direct.connect = ConnectOptions {
            enable_qresync: false,
            send_id: false,
        };
        let mut proxied = testbed::account(server, user)?;
        proxied.endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: proxy.addr.port(),
            server_name: "localhost".into(),
            security: Security::Tls,
        };
        proxied.sync.batch = batch;
        Ok(Bench {
            proxy,
            direct,
            proxied,
        })
    }

    async fn compare(&self, shared: &Shared) -> Result<Vec<String>> {
        let mut verifier = self.direct.connect().await?;
        let server = sync::server_snapshot(&mut verifier).await?;
        verifier.logout().await;
        let local = shared.store().snapshot()?;
        Ok(sync::diff(&server, &local))
    }

    /// Server-side copies, counted by fetching Message-IDs (independent of SEARCH support).
    async fn copies_since(&self, folder: &str, since: u32, message_id: &str) -> Result<usize> {
        let mut conn = self.direct.connect().await?;
        let found = ops::find_since(&mut conn, folder, since, message_id)
            .await?
            .len();
        conn.logout().await;
        Ok(found)
    }
}

fn connection_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ImapError>()
        .is_some_and(ImapError::is_connection)
}

fn local_copies(shared: &Shared, message_id: &str) -> Result<usize> {
    let snapshot = shared.store().snapshot()?;
    Ok(snapshot
        .values()
        .flat_map(|f| f.messages.values())
        .filter(|(_, _, id)| id.as_deref() == Some(message_id))
        .count())
}

/// Seeds `count` messages into the fault user's empty INBOX.
async fn prepare(bench: &Bench, count: usize, seed: u64) -> Result<()> {
    let mut conn = bench.direct.connect().await?;
    reset_account(&mut conn, &["INBOX", "Arkiv"]).await?;
    let mut rng = Rng::new(seed);
    for i in 0..count {
        append(
            &mut conn,
            "INBOX",
            &format!("fault.{seed}.{i}@example.test"),
            &mut rng,
        )
        .await?;
    }
    conn.logout().await;
    Ok(())
}

/// Cut inside the second batch of a first sync; the retry must resume from the committed
/// floor and end equal to the server.
pub async fn mid_fetch_initial(server: Server) -> Result<Outcome> {
    let bench = Bench::new(server, "fault", 500).await?;
    prepare(&bench, 1200, 11).await?;
    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let mut notes = Vec::new();

    bench.proxy.arm(Rule::ServerAfter {
        needle: b" FETCH (".to_vec(),
        nth: 700,
        extra: 37,
    });
    let mut conn = bench.proxied.connect().await?;
    let mut stats = Stats::default();
    let first = sync::sync_account(&mut conn, &shared, &bench.proxied.sync, &mut stats).await;
    let cut = matches!(&first, Err(e) if connection_error(e));
    notes.push(format!(
        "first attempt failed with a connection error: {cut} ({:?})",
        first.err().map(|e| e.to_string())
    ));
    let committed = shared.store().total_messages()?;
    notes.push(format!("messages committed before the cut: {committed}"));

    let mut conn = bench.proxied.connect().await?;
    let mut retry = Stats::default();
    sync::sync_account(&mut conn, &shared, &bench.proxied.sync, &mut retry).await?;
    conn.logout().await;
    notes.push(format!(
        "headers fetched by the retry: {} (resumed, not refetched)",
        retry.headers_fetched
    ));
    let divergences = bench.compare(&shared).await?;
    notes.push(format!("divergences after retry: {}", divergences.len()));
    notes.extend(divergences.iter().take(5).cloned());
    let passed = cut
        && bench.proxy.fired() == 1
        && committed == 500
        && retry.headers_fetched == 700
        && divergences.is_empty();
    Ok(Outcome {
        name: "mid-FETCH (first sync)",
        passed,
        notes,
    })
}

/// Cut inside the CHANGEDSINCE (or full flag scan) FETCH of an incremental pass; nothing of the
/// partial response may be committed, and the retry must converge.
pub async fn mid_fetch_incremental(server: Server) -> Result<Outcome> {
    let bench = Bench::new(server, "fault", 500).await?;
    prepare(&bench, 1200, 12).await?;
    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let mut notes = Vec::new();
    let mut stats = Stats::default();
    let mut conn = bench.proxied.connect().await?;
    sync::sync_account(&mut conn, &shared, &bench.proxied.sync, &mut stats).await?;
    conn.logout().await;
    let before = shared.store().snapshot()?;

    let mut direct = bench.direct.connect().await?;
    ops::select(&mut direct, "INBOX").await?;
    raw::run(
        &mut direct.session,
        "UID STORE 1:* +FLAGS.SILENT (Kuttet)",
        |_| {},
    )
    .await?;
    direct.logout().await;

    bench.proxy.arm(Rule::ServerAfter {
        needle: b" FETCH (".to_vec(),
        nth: 300,
        extra: 10,
    });
    let mut conn = bench.proxied.connect().await?;
    let first = sync::sync_account(&mut conn, &shared, &bench.proxied.sync, &mut stats).await;
    let cut = matches!(&first, Err(e) if connection_error(e));
    let unchanged = shared.store().snapshot()? == before;
    notes.push(format!(
        "cut mid-FETCH: {cut}; store untouched by the partial response: {unchanged}"
    ));

    let mut conn = bench.proxied.connect().await?;
    sync::sync_account(&mut conn, &shared, &bench.proxied.sync, &mut stats).await?;
    conn.logout().await;
    let divergences = bench.compare(&shared).await?;
    notes.push(format!("divergences after retry: {}", divergences.len()));
    notes.extend(divergences.iter().take(5).cloned());
    Ok(Outcome {
        name: "mid-FETCH (incremental)",
        passed: cut && unchanged && divergences.is_empty(),
        notes,
    })
}

/// Cut while the INBOX connection is in IDLE, change the mailbox from another connection, and
/// wait for the IDLE task to reconnect and converge.
pub async fn mid_idle(server: Server) -> Result<Outcome> {
    let bench = Bench::new(server, "fault", 500).await?;
    prepare(&bench, 200, 13).await?;
    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let (stop_tx, stop_rx) = watch::channel(false);
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let task = tokio::spawn(idle::run_inbox(
        bench.proxied.clone(),
        shared.clone(),
        Duration::from_secs(300),
        Duration::from_millis(500),
        events_tx,
        stop_rx,
    ));
    let mut notes = Vec::new();
    wait_for(&mut events, Duration::from_secs(30), |e| {
        matches!(e, Event::Idling)
    })
    .await?;
    let cut_at = Instant::now();
    let killed = bench.proxy.cut_all();
    notes.push(format!("connections cut while idling: {killed}"));

    let mut direct = bench.direct.connect().await?;
    let mut rng = Rng::new(14);
    for i in 0..5 {
        append(
            &mut direct,
            "INBOX",
            &format!("fault.idle.{i}@example.test"),
            &mut rng,
        )
        .await?;
    }
    ops::select(&mut direct, "INBOX").await?;
    raw::run(
        &mut direct.session,
        "UID STORE 1:10 +FLAGS (\\Flagged)",
        |_| {},
    )
    .await?;
    raw::run(
        &mut direct.session,
        "UID STORE 11:13 +FLAGS.SILENT (\\Deleted)",
        |_| {},
    )
    .await?;
    raw::run(&mut direct.session, "EXPUNGE", |_| {}).await?;
    direct.logout().await;

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut disconnects = 0;
    let mut converged = None;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
            Ok(Some(Event::Disconnected(_))) => disconnects += 1,
            Ok(Some(Event::Synced { .. })) | Err(_) => {
                if bench
                    .compare(&shared)
                    .await?
                    .iter()
                    .all(|d| !d.starts_with("INBOX"))
                {
                    converged = Some(cut_at.elapsed());
                    break;
                }
            }
            Ok(None) => break,
            Ok(Some(_)) => {}
        }
    }
    stop_tx.send(true)?;
    task.await?;
    notes.push(format!("disconnects seen by the IDLE task: {disconnects}"));
    notes.push(format!("INBOX converged after the cut in: {converged:?}"));
    let divergences: Vec<String> = bench
        .compare(&shared)
        .await?
        .into_iter()
        .filter(|d| d.starts_with("INBOX"))
        .collect();
    notes.extend(divergences.iter().take(5).cloned());
    Ok(Outcome {
        name: "mid-IDLE",
        passed: killed >= 1 && converged.is_some() && divergences.is_empty(),
        notes,
    })
}

async fn wait_for(
    events: &mut mpsc::UnboundedReceiver<Event>,
    timeout: Duration,
    wanted: impl Fn(&Event) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, events.recv()).await {
            Ok(Some(event)) if wanted(&event) => return Ok(()),
            Ok(Some(_)) => {}
            _ => bail!("event did not arrive in time"),
        }
    }
}

fn big_message(message_id: &str) -> Vec<u8> {
    let mut message = format!(
        "From: <sender@example.test>\r\nTo: <fault@example.test>\r\nSubject: Large synthetic message\r\n\
         Date: Sun, 4 Oct 2026 12:00:00 +0000\r\nMessage-ID: <{message_id}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n"
    );
    for i in 0..4000 {
        message.push_str(&format!(
            "line {i:05} of synthetic filler text for the fault test\r\n"
        ));
    }
    message.into_bytes()
}

/// How a retry finds out whether the earlier attempt took effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// `UID SEARCH HEADER Message-ID` (ADR-006 as drafted).
    Search,
    /// Message-IDs of the UIDs at or above the UIDNEXT recorded before the first attempt.
    Window,
    /// No check: the control that shows what the check prevents.
    Blind,
}

/// The op-queue APPEND path under a cut: the first attempt fails, the retry checks with
/// `strategy` before appending again.
async fn append_scenario(
    server: Server,
    rule: Rule,
    name: &'static str,
    strategy: Strategy,
) -> Result<Outcome> {
    let bench = Bench::new(server, "fault", 500).await?;
    prepare(&bench, 20, 15).await?;
    let message_id = format!("fault.append.{}@example.test", rand_suffix());
    let message = big_message(&message_id);
    let mut notes = Vec::new();

    let mut conn = bench.proxied.connect().await?;
    let before = ops::uidnext(&mut conn, "INBOX").await?;
    bench.proxy.arm(rule);
    let first = match strategy {
        Strategy::Window => {
            ops::append_op(
                &mut conn,
                "INBOX",
                "\\Seen",
                &message,
                &message_id,
                before,
                0,
            )
            .await
        }
        _ => ops::append_once(&mut conn, "INBOX", "\\Seen", &message, &message_id, 0).await,
    };
    let cut = matches!(&first, Err(e) if e.is_connection());
    let after_cut = bench.copies_since("INBOX", before, &message_id).await?;
    notes.push(format!(
        "first attempt cut: {cut}; copies on the server after the cut: {after_cut}"
    ));

    let mut conn = bench.proxied.connect().await?;
    let (uid, sent) = match strategy {
        Strategy::Search => {
            ops::append_once(&mut conn, "INBOX", "\\Seen", &message, &message_id, 1).await?
        }
        Strategy::Window => {
            ops::append_op(
                &mut conn,
                "INBOX",
                "\\Seen",
                &message,
                &message_id,
                before,
                1,
            )
            .await?
        }
        Strategy::Blind => {
            ops::append_once(&mut conn, "INBOX", "\\Seen", &message, &message_id, 0).await?
        }
    };
    notes.push(format!(
        "retry sent an APPEND: {sent}; new UID known: {}",
        uid.is_some()
    ));
    let on_server = bench.copies_since("INBOX", before, &message_id).await?;

    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let mut stats = Stats::default();
    sync::sync_account(&mut conn, &shared, &bench.proxied.sync, &mut stats).await?;
    conn.logout().await;
    let local = local_copies(&shared, &message_id)?;
    let divergences = bench.compare(&shared).await?;
    notes.push(format!(
        "copies on the server: {on_server}; in the cache: {local}; divergences: {}",
        divergences.len()
    ));
    let expected = if strategy == Strategy::Blind && after_cut == 1 {
        2
    } else {
        1
    };
    let passed = cut && on_server == expected && local == expected && divergences.is_empty();
    Ok(Outcome {
        name,
        passed,
        notes,
    })
}

fn rand_suffix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

pub async fn mid_append_literal(server: Server, strategy: Strategy) -> Result<Outcome> {
    let name = match strategy {
        Strategy::Search => "mid-APPEND, literal half sent (retry: SEARCH HEADER Message-ID)",
        Strategy::Window => "mid-APPEND, literal half sent (retry: UIDNEXT window)",
        Strategy::Blind => "control: mid-APPEND, literal half sent (blind retry)",
    };
    append_scenario(
        server,
        Rule::ClientAfter {
            needle: b"APPEND".to_vec(),
            nth: 1,
            extra: 100_000,
        },
        name,
        strategy,
    )
    .await
}

pub async fn mid_append_reply(server: Server, strategy: Strategy) -> Result<Outcome> {
    let name = match strategy {
        Strategy::Search => "mid-APPEND, stored but reply lost (retry: SEARCH HEADER Message-ID)",
        Strategy::Window => "mid-APPEND, stored but reply lost (retry: UIDNEXT window)",
        Strategy::Blind => {
            "control: mid-APPEND, stored but reply lost (blind retry; expect a duplicate)"
        }
    };
    append_scenario(
        server,
        Rule::DropReplyAfterClient {
            needle: b"APPEND".to_vec(),
        },
        name,
        strategy,
    )
    .await
}

/// Lost reply to UID MOVE (or to the COPY of a COPY-based move without MOVE): the replay checks
/// the target first, so the message must end up exactly once, in the target.
pub async fn mid_move_reply(server: Server, strategy: Strategy) -> Result<Outcome> {
    let bench = Bench::new(server, "fault", 500).await?;
    prepare(&bench, 5, 16).await?;
    let mut notes = Vec::new();
    let message_id = "fault.16.2@example.test";
    let mut conn = bench.direct.connect().await?;
    let uid = ops::find_since(&mut conn, "INBOX", 1, message_id)
        .await?
        .first()
        .copied()
        .context("seeded message")?;
    let has_move = conn.caps.has("MOVE");
    let needle = if has_move {
        b"MOVE".to_vec()
    } else {
        b"COPY".to_vec()
    };
    conn.logout().await;

    let mut conn = bench.proxied.connect().await?;
    let before = ops::uidnext(&mut conn, "Arkiv").await?;
    bench.proxy.arm(Rule::DropReplyAfterClient { needle });
    let first = match strategy {
        Strategy::Window => {
            ops::move_op(&mut conn, "INBOX", uid, "Arkiv", message_id, before, 0).await
        }
        _ => ops::move_once(&mut conn, "INBOX", uid, "Arkiv", message_id, 0).await,
    };
    let cut = matches!(&first, Err(e) if e.is_connection());
    let mid_inbox = bench.copies_since("INBOX", 1, message_id).await?;
    let mid_archive = bench.copies_since("Arkiv", 1, message_id).await?;
    notes.push(format!(
        "first attempt cut: {cut}; after the cut INBOX {mid_inbox}, Arkiv {mid_archive}"
    ));
    let mut conn = bench.proxied.connect().await?;
    let replay = match strategy {
        Strategy::Window => {
            ops::move_op(&mut conn, "INBOX", uid, "Arkiv", message_id, before, 1).await
        }
        Strategy::Search => ops::move_once(&mut conn, "INBOX", uid, "Arkiv", message_id, 1).await,
        Strategy::Blind => ops::move_once(&mut conn, "INBOX", uid, "Arkiv", message_id, 0).await,
    };
    notes.push(format!(
        "replay result: {:?}",
        replay.as_ref().map(|u| u.is_some())
    ));
    let in_inbox = bench.copies_since("INBOX", 1, message_id).await?;
    let in_archive = bench.copies_since("Arkiv", 1, message_id).await?;
    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let mut stats = Stats::default();
    sync::sync_account(&mut conn, &shared, &bench.proxied.sync, &mut stats).await?;
    conn.logout().await;
    let divergences = bench.compare(&shared).await?;
    notes.push(format!(
        "copies: INBOX {in_inbox}, Arkiv {in_archive}; cache {}; divergences {}",
        local_copies(&shared, message_id)?,
        divergences.len()
    ));
    let expected_archive = if strategy == Strategy::Blind && !has_move {
        2
    } else {
        1
    };
    let passed = cut
        && replay.is_ok()
        && in_inbox == 0
        && in_archive == expected_archive
        && divergences.is_empty();
    let name = match strategy {
        Strategy::Search => "mid-MOVE, done but reply lost (retry: SEARCH HEADER Message-ID)",
        Strategy::Window => "mid-MOVE, done but reply lost (retry: UIDNEXT window)",
        Strategy::Blind => {
            "control: mid-MOVE, done but reply lost (blind replay; duplicates without MOVE)"
        }
    };
    Ok(Outcome {
        name,
        passed,
        notes,
    })
}

pub async fn all(server: Server) -> Vec<Result<Outcome>> {
    let mut out = vec![
        mid_fetch_initial(server).await,
        mid_fetch_incremental(server).await,
    ];
    if server != testbed::DOVECOT_REV1 {
        out.push(mid_idle(server).await);
    }
    for strategy in [Strategy::Search, Strategy::Window, Strategy::Blind] {
        out.push(mid_append_literal(server, strategy).await);
        out.push(mid_append_reply(server, strategy).await);
        out.push(mid_move_reply(server, strategy).await);
    }
    out
}
