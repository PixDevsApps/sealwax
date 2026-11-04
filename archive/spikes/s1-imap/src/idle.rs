//! The IDLE connection on INBOX and the polling connection for the rest. A notice from the
//! server only wakes a state-based pass; its content is never trusted (async-imap may also have
//! dropped some of it).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_imap::extensions::idle::IdleResponse;
use rustls::ClientConfig;
use tokio::sync::{mpsc, watch};

use crate::net::{self, ConnectOptions, Connection, Endpoint, Login};
use crate::raw::ImapError;
use crate::sync::{self, Shared, Stats, SyncOptions};

#[derive(Clone)]
pub struct Account {
    pub endpoint: Endpoint,
    pub tls: Arc<ClientConfig>,
    pub login: Login,
    pub connect: ConnectOptions,
    pub sync: SyncOptions,
}

impl Account {
    pub async fn connect(&self) -> Result<Connection, ImapError> {
        net::connect(&self.endpoint, self.tls.clone(), &self.login, self.connect).await
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Connected,
    Idling,
    Notice(Instant),
    /// A pass finished and was committed.
    Synced {
        started: Instant,
        finished: Instant,
    },
    Disconnected(String),
}

/// Bound for IDLE's start and DONE: async-imap's `init` waits for a continuation and ignores a
/// tagged NO, and `done` has no deadline of its own.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

pub enum Wake {
    Notice,
    Timeout,
    Stopped,
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

/// One IDLE round trip: IDLE, wait for anything, DONE.
pub async fn idle(
    conn: Connection,
    max: Duration,
    stop: &mut watch::Receiver<bool>,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<(Connection, Wake), ImapError> {
    let Connection {
        session,
        caps,
        qresync,
        auth,
        selected,
    } = conn;
    let mut handle = session.idle();
    tokio::time::timeout(COMMAND_TIMEOUT, handle.init())
        .await
        .map_err(|_| ImapError::Timeout)??;
    let _ = events.send(Event::Idling);
    let wake = {
        let (wait, _interrupt) = handle.wait_with_timeout(max);
        tokio::select! {
            result = wait => match result? {
                IdleResponse::NewData(_) => Wake::Notice,
                IdleResponse::Timeout => Wake::Timeout,
                // async-imap reports the end of the stream (EOF) as ManualInterrupt; we never
                // interrupt through the StopSource, so this is a lost connection.
                IdleResponse::ManualInterrupt => return Err(ImapError::ConnectionLost),
            },
            _ = stopped(stop) => Wake::Stopped,
        }
    };
    if matches!(wake, Wake::Notice) {
        let _ = events.send(Event::Notice(Instant::now()));
    }
    let session = tokio::time::timeout(COMMAND_TIMEOUT, handle.done())
        .await
        .map_err(|_| ImapError::Timeout)??;
    Ok((
        Connection {
            session,
            caps,
            qresync,
            auth,
            selected,
        },
        wake,
    ))
}

/// Keeps INBOX current: pass, IDLE, pass on every wake, reconnecting with backoff. Without
/// IDLE it polls every `poll`.
pub async fn run_inbox(
    account: Account,
    shared: Arc<Shared>,
    max_idle: Duration,
    poll: Duration,
    events: mpsc::UnboundedSender<Event>,
    mut stop: watch::Receiver<bool>,
) -> Stats {
    let mut stats = Stats::default();
    let mut backoff = Duration::from_millis(50);
    'connection: while !*stop.borrow() {
        let mut conn = match account.connect().await {
            Ok(conn) => conn,
            Err(error) => {
                let _ = events.send(Event::Disconnected(error.to_string()));
                if sleep_or_stop(backoff, &mut stop).await {
                    break;
                }
                backoff = (backoff * 2).min(Duration::from_secs(2));
                continue;
            }
        };
        backoff = Duration::from_millis(50);
        let _ = events.send(Event::Connected);
        loop {
            let inbox = match inbox_id(&mut conn, &shared, &mut stats).await {
                Ok(id) => id,
                Err(error) => {
                    let _ = events.send(Event::Disconnected(error.to_string()));
                    continue 'connection;
                }
            };
            let started = Instant::now();
            if let Err(error) =
                sync::sync_folder(&mut conn, &shared, inbox, &account.sync, &mut stats).await
            {
                let _ = events.send(Event::Disconnected(error.to_string()));
                continue 'connection;
            }
            let _ = events.send(Event::Synced {
                started,
                finished: Instant::now(),
            });
            if *stop.borrow() {
                conn.logout().await;
                break 'connection;
            }
            if !conn.caps.has("IDLE") {
                if sleep_or_stop(poll, &mut stop).await {
                    conn.logout().await;
                    break 'connection;
                }
                continue;
            }
            // Changes made after the pass's EXAMINE and before IDLE starts are not reported by
            // every server once IDLE begins (Stalwart 0.16 doesn't); NOOP reports them, so a
            // NOOP that shows a change means another pass instead of IDLE.
            match changed_since_last_command(&mut conn).await {
                Ok(true) => {
                    stats.noop_changes += 1;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    let _ = events.send(Event::Disconnected(error.to_string()));
                    continue 'connection;
                }
            }
            match idle(conn, max_idle, &mut stop, &events).await {
                Ok((c, Wake::Stopped)) => {
                    c.logout().await;
                    break 'connection;
                }
                Ok((c, _)) => conn = c,
                Err(error) => {
                    let _ = events.send(Event::Disconnected(error.to_string()));
                    continue 'connection;
                }
            }
        }
    }
    stats
}

async fn changed_since_last_command(conn: &mut Connection) -> Result<bool, ImapError> {
    use async_imap::imap_proto::{MailboxDatum, Response};
    let mut changed = false;
    crate::raw::run(&mut conn.session, "NOOP", |r| {
        if matches!(
            r,
            Response::MailboxData(MailboxDatum::Exists(_))
                | Response::Expunge(_)
                | Response::Fetch(..)
                | Response::Vanished { .. }
        ) {
            changed = true;
        }
    })
    .await?;
    Ok(changed)
}

async fn inbox_id(
    conn: &mut Connection,
    shared: &Shared,
    stats: &mut Stats,
) -> anyhow::Result<i64> {
    if let Some(row) = shared.store().mailbox_by_raw("INBOX")? {
        return Ok(row.id);
    }
    sync::sync_list(conn, shared, stats).await?;
    shared
        .store()
        .mailbox_by_raw("INBOX")?
        .map(|r| r.id)
        .ok_or_else(|| anyhow::anyhow!("no INBOX"))
}

/// Full account passes (LIST + every mailbox) every `interval`, reconnecting on errors.
/// Publishes the start time of every completed pass.
pub async fn run_poller(
    account: Account,
    shared: Arc<Shared>,
    interval: Duration,
    completed: watch::Sender<Option<Instant>>,
    events: mpsc::UnboundedSender<Event>,
    mut stop: watch::Receiver<bool>,
) -> Stats {
    let mut stats = Stats::default();
    'connection: while !*stop.borrow() {
        let mut conn = match account.connect().await {
            Ok(conn) => conn,
            Err(error) => {
                let _ = events.send(Event::Disconnected(error.to_string()));
                if sleep_or_stop(Duration::from_millis(100), &mut stop).await {
                    break;
                }
                continue;
            }
        };
        loop {
            let started = Instant::now();
            if let Err(error) =
                sync::sync_account(&mut conn, &shared, &account.sync, &mut stats).await
            {
                let _ = events.send(Event::Disconnected(error.to_string()));
                continue 'connection;
            }
            let _ = completed.send(Some(started));
            if sleep_or_stop(interval, &mut stop).await {
                conn.logout().await;
                break 'connection;
            }
        }
    }
    stats
}
