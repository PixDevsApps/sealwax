//! Measurements against the testbed: first header sync, no-change re-sync and IDLE latency.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, watch};

use crate::idle::{self, Account, Event};
use crate::net::{ConnectOptions, Endpoint, Security};
use crate::proxy::Proxy;
use crate::raw;
use crate::rng::Rng;
use crate::store::Store;
use crate::sync::{self, Shared, Stats};
use crate::synth;
use crate::testbed::{self, Server};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The testbed account, optionally behind a proxy that adds `rtt` of round-trip delay to the
/// IMAP traffic. Keep the returned proxy alive while the account is used.
pub async fn account(
    server: Server,
    user: &str,
    rtt: Duration,
) -> Result<(Account, Option<Proxy>)> {
    let mut account = testbed::account(server, user)?;
    if rtt.is_zero() {
        return Ok((account, None));
    }
    let (cert, key) = testbed::cert_paths();
    let upstream = format!("127.0.0.1:{}", server.tls_port).parse()?;
    let proxy = Proxy::start_with_latency(
        upstream,
        crate::tls::server(&cert, &key)?,
        testbed::tls()?,
        rtt / 2,
    )
    .await?;
    account.endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port: proxy.addr.port(),
        server_name: "localhost".into(),
        security: Security::Tls,
    };
    Ok((account, Some(proxy)))
}

#[derive(Debug)]
pub struct FirstSync {
    pub connect_ms: f64,
    pub list_ms: f64,
    pub inbox_ms: f64,
    pub total_ms: f64,
    pub messages: u32,
    pub batches: u64,
    pub db_bytes: u64,
    /// IMAP bytes from the server, when measured through the proxy.
    pub bytes_received: Option<u64>,
}

/// A first sync of `user`'s INBOX into an empty store: connect + LIST + INBOX headers.
pub async fn first_sync(
    server: Server,
    user: &str,
    batch: usize,
    rtt: Duration,
) -> Result<FirstSync> {
    let (mut account, proxy) = account(server, user, rtt).await?;
    account.sync.batch = batch;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cache.db");
    let shared = Shared::new(Store::open(&path)?);
    let mut stats = Stats::default();
    let t0 = Instant::now();
    let mut conn = account.connect().await?;
    let t1 = Instant::now();
    sync::sync_list(&mut conn, &shared, &mut stats).await?;
    let t2 = Instant::now();
    let inbox = shared.store().mailbox_by_raw("INBOX")?.context("INBOX")?.id;
    sync::sync_folder(&mut conn, &shared, inbox, &account.sync, &mut stats).await?;
    let t3 = Instant::now();
    conn.logout().await;
    let messages = shared.store().count(inbox)?;
    drop(shared);
    let db_bytes = std::fs::metadata(&path)?.len()
        + std::fs::metadata(dir.path().join("cache.db-wal"))
            .map(|m| m.len())
            .unwrap_or(0);
    Ok(FirstSync {
        connect_ms: ms(t1 - t0),
        list_ms: ms(t2 - t1),
        inbox_ms: ms(t3 - t2),
        total_ms: ms(t3 - t0),
        messages,
        batches: stats.initial_batches,
        db_bytes,
        bytes_received: proxy.map(|p| p.bytes_to_client()),
    })
}

/// The header FETCH alone: EXAMINE, UID SEARCH ALL and the batched UID FETCH, decoded into
/// owned rows but not stored, for comparison with the io-imap prototype.
pub async fn fetch_only(server: Server, user: &str, batch: usize) -> Result<(usize, f64)> {
    let account = testbed::account(server, user)?;
    let mut conn = account.connect().await?;
    let mut view = sync::examine(&mut conn, "INBOX").await?;
    let t = Instant::now();
    let mut uids = sync::uid_search(&mut conn, "ALL", &mut view).await?;
    uids.reverse();
    let mut fetched = 0;
    for chunk in uids.chunks(batch) {
        let wanted: std::collections::HashSet<u32> = chunk.iter().copied().collect();
        fetched += sync::fetch_headers(
            &mut conn,
            &crate::raw::uid_set(chunk),
            |u| wanted.contains(&u),
            &mut view,
        )
        .await?
        .len();
    }
    let elapsed = ms(t.elapsed());
    conn.logout().await;
    Ok((fetched, elapsed))
}

#[derive(Debug, Default)]
pub struct Resync {
    /// One pass over INBOX on an open connection.
    pub inbox_ms: Vec<f64>,
    /// LIST plus a pass over every mailbox on an open connection.
    pub account_ms: Vec<f64>,
    /// Connect + login + the account pass.
    pub reconnect_ms: Vec<f64>,
    pub fast_path: u64,
    pub status_skips: u64,
    pub full_flag_scans: u64,
    pub messages: u64,
    pub mailboxes: usize,
}

/// No-change re-syncs after a completed first sync of the whole account.
pub async fn resync(server: Server, user: &str, runs: usize, rtt: Duration) -> Result<Resync> {
    let (account, _proxy) = account(server, user, rtt).await?;
    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let mut warmup = Stats::default();
    let mut conn = account.connect().await?;
    sync::sync_account(&mut conn, &shared, &account.sync, &mut warmup).await?;
    sync::sync_account(&mut conn, &shared, &account.sync, &mut warmup).await?;
    let inbox = shared.store().mailbox_by_raw("INBOX")?.context("INBOX")?.id;
    let messages = shared.store().total_messages()?;
    let mailboxes = shared.store().mailboxes()?.len();
    let mut out = Resync {
        messages,
        mailboxes,
        ..Resync::default()
    };
    let mut stats = Stats::default();
    for _ in 0..runs {
        let t = Instant::now();
        sync::sync_folder(&mut conn, &shared, inbox, &account.sync, &mut stats).await?;
        out.inbox_ms.push(ms(t.elapsed()));
        let t = Instant::now();
        sync::sync_account(&mut conn, &shared, &account.sync, &mut stats).await?;
        out.account_ms.push(ms(t.elapsed()));
    }
    conn.logout().await;
    for _ in 0..runs {
        let t = Instant::now();
        let mut conn = account.connect().await?;
        sync::sync_account(&mut conn, &shared, &account.sync, &mut stats).await?;
        out.reconnect_ms.push(ms(t.elapsed()));
        conn.logout().await;
    }
    out.fast_path = stats.fast_path;
    out.status_skips = stats.status_skips;
    out.full_flag_scans = stats.full_flag_scans;
    if stats.headers_fetched > 0 || stats.flag_updates > 0 || stats.deletes > 0 {
        bail!("a no-change re-sync changed the cache: {stats:?}");
    }
    Ok(out)
}

#[derive(Debug)]
pub struct IdleSample {
    /// APPEND's tagged OK at the other client -> IDLE notice on the sync connection.
    pub notice_ms: Option<f64>,
    /// APPEND's tagged OK -> the message committed to the cache.
    pub visible_ms: f64,
}

/// New-mail latency over IDLE: another connection appends to INBOX, and the clock stops when
/// the message is in the cache.
pub async fn idle_latency(server: Server, user: &str, samples: usize) -> Result<Vec<IdleSample>> {
    let account = testbed::account(server, user)?;
    let mut writer_account = testbed::account(server, user)?;
    writer_account.connect = ConnectOptions {
        enable_qresync: false,
        send_id: false,
    };
    let dir = tempfile::tempdir()?;
    let shared = Shared::new(Store::open(&dir.path().join("cache.db"))?);
    let (stop_tx, stop_rx) = watch::channel(false);
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let task = tokio::spawn(idle::run_inbox(
        account,
        shared.clone(),
        Duration::from_secs(300),
        Duration::from_secs(1),
        events_tx,
        stop_rx,
    ));
    let mut writer = writer_account.connect().await?;
    let mut rng = Rng::new(99);
    let run = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    );
    let mut out = Vec::new();
    let mut idling = false;
    for i in 0..samples {
        while !idling {
            match tokio::time::timeout(Duration::from_secs(30), events.recv()).await {
                Ok(Some(Event::Idling)) => idling = true,
                Ok(Some(_)) => {}
                _ => bail!("the IDLE connection never reached IDLE"),
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        while events.try_recv().is_ok() {}
        let message_id = format!("idle.{run}.{i}@example.test");
        let message = synth::message(&message_id, &mut rng);
        let literal_plus = writer.caps.has("LITERAL+");
        raw::append(
            &mut writer.session,
            "INBOX",
            "",
            &message,
            literal_plus,
            |_| {},
        )
        .await?;
        let appended = Instant::now();
        let mut notice = None;
        let deadline = appended + Duration::from_secs(10);
        idling = false;
        let visible = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(Event::Notice(at))) => notice = Some(at),
                Ok(Some(Event::Synced { finished, .. })) => {
                    if shared.store().has_message_id(&message_id)? {
                        break finished;
                    }
                }
                Ok(Some(Event::Idling)) => idling = true,
                Ok(Some(_)) => {}
                _ => bail!("sample {i}: the message did not reach the cache within 10 s"),
            }
        };
        out.push(IdleSample {
            notice_ms: notice.map(|n| ms(n.saturating_duration_since(appended))),
            visible_ms: ms(visible.saturating_duration_since(appended)),
        });
    }
    writer.logout().await;
    stop_tx.send(true)?;
    task.await?;
    Ok(out)
}

pub fn summary(values: &[f64]) -> String {
    if values.is_empty() {
        return "n=0".into();
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    format!(
        "n={} min={:.1} p50={:.1} p95={:.1} max={:.1} ms",
        sorted.len(),
        sorted[0],
        pick(0.5),
        pick(0.95),
        sorted[sorted.len() - 1]
    )
}
