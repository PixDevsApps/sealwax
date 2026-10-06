//! The per-account sync actor (ML-040, ML-042, ML-045), in ADR-006's layout as S1 proved it.
//!
//! - The **command connection** runs an account pass on a schedule (every synced folder's
//!   STATUS, a pass where it changed), with INBOX left to the IDLE connection and its STATUS as
//!   the backstop; a pass over the folder the user has open, at once and then more often; older
//!   mail on request. Servers without IDLE get INBOX polled here. Before all of them it runs
//!   the ops that are due (`ops`): after connecting, on `Command::OpsQueued`, and when the next
//!   undo window or retry ends; then the outbox and the Drafts folder (`outbox`): after
//!   connecting, on `Command::OutboxQueued` and `Command::DraftsChanged`, and when the next send,
//!   Sent copy, upload or deletion comes due.
//! - The **IDLE connection** keeps INBOX current: a pass, NOOP (another pass when it reported a
//!   change, S1's Stalwart gap), IDLE until the server says anything or the restart interval,
//!   and a pass on every wake.
//! - Both reconnect with jittered exponential backoff, NOOP a connection that sat unused before
//!   using it again, and drop everything at once on `Command::Reconnect`. A refused login or a
//!   certificate that no longer verifies stops the retries until `Reconnect` or a new password
//!   or sign-in (which restarts the actor).
//!
//! Passes over one folder never overlap ([`FolderLocks`]). Statuses go out as they change;
//! progress at most every `progress_every`. New mail goes out as `Event::NewMail` the moment a
//! pass's step commits it (ML-083), except what the account's own moves put there ([`Moved`]),
//! and never from older mail.

use super::{Shared, bodies, log_in_with_password, log_in_with_token};
use crate::api::{Event, SyncStatus};
use crate::imap::write::Mark;
use crate::imap::{
    self, Arrived, ConnectOptions, Connection, Elsewhere, FolderLocks, Idled, Progress, Stop,
    SyncError, SyncOptions, SyncReport,
};
use crate::smtp::submit::Timeouts;
use sealwax_domain::account::{AccountSettings, AuthMethod, LoginFailure};
use sealwax_domain::model::{AccountId, BlobKey, DraftId, FolderId, FolderRole};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::watch;

mod ops;
mod outbox;

/// How often and how patiently the actors sync. The defaults are proposals for the user.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    pub options: SyncOptions,
    /// An account pass: every synced folder's STATUS, and a pass where it changed.
    pub account_pass: Duration,
    /// INBOX on servers without IDLE.
    pub inbox_poll: Duration,
    /// The folder the user has open.
    pub opened_poll: Duration,
    /// IDLE is ended and issued again after this long (RFC 2177: before 29 minutes).
    pub idle_restart: Duration,
    /// A connection unused this long gets a NOOP before it is used again.
    pub reuse_check: Duration,
    /// How long that NOOP may take before the connection counts as dead.
    pub noop_timeout: Duration,
    /// The backoff after the second failure in a row; it doubles up to `backoff_max`.
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// `SyncProgress` at most this often per account.
    pub progress_every: Duration,
    /// How long a body download connection stays open without work.
    pub body_linger: Duration,
    /// How long LOGOUT may take when the engine stops.
    pub logout_wait: Duration,
    /// How every sync and body connection is prepared (QRESYNC off reaches the CONDSTORE-only
    /// path on servers that offer both).
    pub connect: ConnectOptions,
    /// How long the outbox's SMTP steps may take.
    pub submission: Timeouts,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            options: SyncOptions::default(),
            account_pass: Duration::from_secs(120),
            inbox_poll: Duration::from_secs(60),
            opened_poll: Duration::from_secs(30),
            idle_restart: Duration::from_secs(25 * 60),
            reuse_check: Duration::from_secs(30),
            noop_timeout: Duration::from_secs(10),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(300),
            progress_every: Duration::from_millis(100),
            body_linger: Duration::from_secs(60),
            logout_wait: Duration::from_millis(300),
            connect: ConnectOptions::default(),
            submission: Timeouts::default(),
        }
    }
}

/// The wait before the next attempt after `failures` failures in a row: none after the first
/// (a dropped connection is tried again at once), then `backoff_min` doubling to `backoff_max`,
/// each drawn between half and all of it (`unit` in 0..1) so accounts don't retry in step.
pub fn backoff_delay(config: &SyncConfig, failures: u32, unit: f64) -> Duration {
    if failures <= 1 {
        return Duration::ZERO;
    }
    let doubled = config
        .backoff_min
        .saturating_mul(1u32 << (failures - 2).min(30));
    let ceiling = doubled.min(config.backoff_max);
    ceiling.mul_f64(0.5 + 0.5 * unit.clamp(0.0, 1.0))
}

/// What the engine tells an actor between passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    /// The folder the user opened, when it is this account's; `None` when it is another's.
    Opened(Option<FolderId>),
    /// `LoadOlder` for one of this account's folders.
    Older(FolderId),
    /// `OpsQueued`: the store has new ops for this account.
    Ops,
    /// `OutboxQueued`: a send was queued or undone.
    Outbox,
    /// `DraftsChanged`: a draft was saved, closed or discarded.
    Drafts,
}

/// Reconnects asked for so far, and whether to stop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Signal {
    pub reconnects: u64,
    pub stop: Option<Stop>,
}

/// What interrupted network work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interrupt {
    Reconnect,
    Stop(Stop),
}

/// A running actor, as the service keeps it.
pub(super) struct Handle {
    pub settings: AccountSettings,
    pub account: Arc<OnceLock<AccountId>>,
    pub control: async_channel::Sender<Control>,
    pub signal: watch::Sender<Signal>,
    pub task: tokio::task::JoinHandle<()>,
}

impl Handle {
    pub fn start(shared: &Arc<Shared>, settings: AccountSettings) -> Handle {
        let (control, controls) = async_channel::unbounded();
        let (signal, signals) = watch::channel(Signal::default());
        let account = Arc::new(OnceLock::new());
        let task = tokio::spawn(run(
            shared.clone(),
            settings.clone(),
            account.clone(),
            controls,
            signals,
        ));
        Handle {
            settings,
            account,
            control,
            signal,
            task,
        }
    }

    pub fn account(&self) -> Option<AccountId> {
        self.account.get().copied()
    }

    pub fn reconnect(&self) {
        self.signal.send_modify(|signal| signal.reconnects += 1);
    }

    pub fn stop(&self, stop: Stop) {
        self.signal.send_modify(|signal| signal.stop = Some(stop));
    }
}

/// Resolves when a reconnect newer than `seen`, or a stop, has been asked for.
async fn interrupted(mut signals: watch::Receiver<Signal>, seen: u64) -> Interrupt {
    loop {
        let signal = *signals.borrow_and_update();
        if let Some(stop) = signal.stop {
            return Interrupt::Stop(stop);
        }
        if signal.reconnects != seen {
            return Interrupt::Reconnect;
        }
        if signals.changed().await.is_err() {
            return Interrupt::Stop(Stop::Drop);
        }
    }
}

/// `work`, unless a reconnect or stop comes first.
async fn race<T>(
    signals: &watch::Receiver<Signal>,
    seen: u64,
    work: impl Future<Output = T>,
) -> Result<T, Interrupt> {
    tokio::select! {
        biased;
        interrupt = interrupted(signals.clone(), seen) => Err(interrupt),
        done = work => Ok(done),
    }
}

/// How long a move whose target UID is unknown keeps its messages out of the target's new mail.
const MOVED_KEPT: Duration = Duration::from_secs(600);

/// Message-IDs moved into each folder, with when their move started.
type MovedInto = HashMap<FolderId, Vec<(String, Instant)>>;

/// Messages the account's op runner moved into a folder without learning their new UIDs (no
/// COPYUID and no Message-ID match, or an attempt cut before its outcome), by target folder and
/// Message-ID: the target's sync stores them as new rows, and they aren't new mail. Shared by
/// the op runner and [`forward_new_mail`]; holds Message-IDs, so it has no `Debug`.
#[derive(Clone, Default)]
pub(super) struct Moved(Arc<Mutex<MovedInto>>);

impl Moved {
    fn lock(&self) -> std::sync::MutexGuard<'_, MovedInto> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A move into `target` is about to run (again, for a retry).
    pub(super) fn start(&self, target: FolderId, message_ids: impl Iterator<Item = String>) {
        let now = Instant::now();
        let mut moved = self.lock();
        let waiting = moved.entry(target).or_default();
        for message_id in message_ids {
            match waiting.iter_mut().find(|(id, _)| *id == message_id) {
                Some((_, at)) => *at = now,
                None => waiting.push((message_id, now)),
            }
        }
    }

    /// These moved messages were re-homed under their new UIDs: the store holds them already.
    pub(super) fn settled<'a>(&self, target: FolderId, message_ids: impl Iterator<Item = &'a str>) {
        let mut moved = self.lock();
        if let Some(waiting) = moved.get_mut(&target) {
            for message_id in message_ids {
                if let Some(at) = waiting.iter().position(|(id, _)| id == message_id) {
                    waiting.swap_remove(at);
                }
            }
            if waiting.is_empty() {
                moved.remove(&target);
            }
        }
    }

    /// Whether moves into `folder` wait to be recognised; forgets those past [`MOVED_KEPT`].
    fn waiting(&self, folder: FolderId) -> bool {
        let mut moved = self.lock();
        moved.retain(|_, waiting| {
            waiting.retain(|(_, at)| at.elapsed() < MOVED_KEPT);
            !waiting.is_empty()
        });
        moved.contains_key(&folder)
    }

    /// Whether a message with `message_id` was moved into `folder`, forgetting it if so.
    fn take(&self, folder: FolderId, message_id: &str) -> bool {
        let mut moved = self.lock();
        let Some(waiting) = moved.get_mut(&folder) else {
            return false;
        };
        let Some(at) = waiting.iter().position(|(id, _)| id == message_id) else {
            return false;
        };
        waiting.swap_remove(at);
        if waiting.is_empty() {
            moved.remove(&folder);
        }
        true
    }
}

/// Hands each step's new mail to the app as `Event::NewMail`, without the messages the
/// account's own moves put there, until every pass's end of the channel is gone.
async fn forward_new_mail(
    shared: Arc<Shared>,
    account: AccountId,
    arrivals: async_channel::Receiver<Arrived>,
    moved: Moved,
) {
    while let Ok(Arrived { folder, messages }) = arrivals.recv().await {
        let mut kept = Vec::with_capacity(messages.len());
        if moved.waiting(folder) {
            for id in messages {
                let message_id = shared
                    .store
                    .message(id)
                    .recv()
                    .await
                    .ok()
                    .and_then(|found| found.value)
                    .and_then(|detail| detail.headers.message_id);
                if !message_id.is_some_and(|message_id| moved.take(folder, &message_id)) {
                    kept.push(id);
                }
            }
        } else {
            kept = messages;
        }
        if kept.is_empty() {
            continue;
        }
        tracing::debug!(account = account.0, %folder, messages = kept.len(), "new mail");
        shared.send(Event::NewMail {
            account,
            folder,
            messages: kept,
        });
    }
}

/// A small xorshift for the backoff's jitter; the tests reproduce delays with `backoff_delay`.
struct Jitter(u64);

impl Jitter {
    fn new(account: AccountId) -> Jitter {
        let clock = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |elapsed| elapsed.subsec_nanos() as u64);
        Jitter((clock ^ (account.0 as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)) | 1)
    }

    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The account's statuses as the app last heard them, sent only when they change.
struct Status {
    shared: Arc<Shared>,
    account: AccountId,
    shown: Mutex<Option<SyncStatus>>,
    progress: Mutex<Option<Instant>>,
}

impl Status {
    fn set(&self, status: SyncStatus) {
        let mut shown = self.shown.lock().unwrap_or_else(PoisonError::into_inner);
        if *shown != Some(status) {
            *shown = Some(status);
            self.shared.send(Event::AccountStatus {
                account: self.account,
                status,
            });
        }
    }

    fn current(&self) -> Option<SyncStatus> {
        *self.shown.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `SyncProgress` at most every `every`, and always the last batch's.
    fn progress(&self, folder: FolderId, progress: Progress, every: Duration) {
        let mut last = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        let due = last.is_none_or(|sent| sent.elapsed() >= every);
        if due || progress.fetched >= progress.total {
            *last = Some(Instant::now());
            // The app shows the account as syncing on progress.
            *self.shown.lock().unwrap_or_else(PoisonError::into_inner) = Some(SyncStatus::Syncing);
            self.shared.send(Event::SyncProgress {
                account: self.account,
                folder,
                fetched: progress.fetched,
                total: progress.total,
            });
        }
    }
}

async fn log_in(shared: &Shared, settings: &AccountSettings) -> Result<Connection, LoginFailure> {
    match settings.auth {
        AuthMethod::Password => log_in_with_password(shared, settings).await,
        AuthMethod::OAuth(_) => log_in_with_token(shared, settings).await,
    }
}

/// Deletes raw messages no row refers to any more.
pub(super) async fn discard(shared: &Shared, orphaned: Vec<BlobKey>) {
    if orphaned.is_empty() {
        return;
    }
    let blobs = shared.blobs.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for key in &orphaned {
            if let Err(error) = blobs.remove(key) {
                tracing::warn!(%error, "a raw message no row refers to couldn't be deleted");
            }
        }
    })
    .await;
}

/// Stops the retries (a refused login, a bad certificate) until something changes.
fn halts(status: SyncStatus) -> bool {
    matches!(
        status,
        SyncStatus::NeedsSignIn(_) | SyncStatus::CertificateChanged
    )
}

async fn run(
    shared: Arc<Shared>,
    settings: AccountSettings,
    cell: Arc<OnceLock<AccountId>>,
    controls: async_channel::Receiver<Control>,
    signals: watch::Receiver<Signal>,
) {
    let account = match shared
        .store
        .account_by_key(settings.key.clone())
        .recv()
        .await
    {
        Ok(found) => match found.value {
            Some(account) => account.id,
            None => return,
        },
        Err(error) => {
            tracing::warn!(%error, "the account's store row couldn't be read");
            return;
        }
    };
    let _ = cell.set(account);
    let status = Arc::new(Status {
        shared: shared.clone(),
        account,
        shown: Mutex::new(None),
        progress: Mutex::new(None),
    });
    status.set(SyncStatus::Syncing);
    let (arrivals, arrived) = async_channel::unbounded();
    let moved = Moved::default();
    tokio::spawn(forward_new_mail(
        shared.clone(),
        account,
        arrived,
        moved.clone(),
    ));
    let mut poller = Poller {
        jitter: Jitter::new(account),
        shared,
        settings: Arc::new(settings),
        account,
        status,
        locks: FolderLocks::default(),
        signals,
        controls,
        conn: None,
        used: Instant::now(),
        failures: 0,
        retry_at: Instant::now(),
        halted: false,
        inbox: watch::channel(None).0,
        idle: None,
        opened: None,
        opened_now: false,
        older: VecDeque::new(),
        next_account: Instant::now(),
        next_inbox: Instant::now(),
        next_opened: Instant::now(),
        ops_now: true,
        next_ops: None,
        quota: false,
        outbox_now: true,
        next_outbox: None,
        drafts_now: true,
        next_drafts: None,
        smtp_failures: 0,
        smtp_status: None,
        sent_marks: HashMap::new(),
        arrivals,
        moved,
    };
    poller.run().await;
}

struct Poller {
    shared: Arc<Shared>,
    settings: Arc<AccountSettings>,
    account: AccountId,
    status: Arc<Status>,
    locks: FolderLocks,
    signals: watch::Receiver<Signal>,
    controls: async_channel::Receiver<Control>,
    conn: Option<Connection>,
    /// When the connection was last used.
    used: Instant,
    failures: u32,
    retry_at: Instant,
    halted: bool,
    jitter: Jitter,
    /// INBOX's id, for the IDLE connection.
    inbox: watch::Sender<Option<FolderId>>,
    idle: Option<tokio::task::JoinHandle<()>>,
    opened: Option<FolderId>,
    opened_now: bool,
    older: VecDeque<FolderId>,
    next_account: Instant,
    next_inbox: Instant,
    next_opened: Instant,
    /// Run the due ops at the next chance: after connecting, or on `OpsQueued`.
    ops_now: bool,
    /// When the next op becomes due (an undo window or a retry ends).
    next_ops: Option<Instant>,
    /// The server said the mailbox is full; the account shows it until ops get through.
    quota: bool,
    /// Run the due sends and Sent copies at the next chance.
    outbox_now: bool,
    /// When the next send or Sent copy becomes due.
    next_outbox: Option<Instant>,
    /// Run the due Drafts-folder uploads and deletions at the next chance.
    drafts_now: bool,
    next_drafts: Option<Instant>,
    /// Submission failures in a row, for the backoff.
    smtp_failures: u32,
    /// The submission server refused the login: the account shows it until a login works.
    smtp_status: Option<SyncStatus>,
    /// The Sent folder's mark taken before each send's data went out, until its Sent copy is
    /// done: a server that saves its own copy puts it above.
    sent_marks: HashMap<DraftId, Mark>,
    /// Where the passes put new mail ([`forward_new_mail`] reads it).
    arrivals: async_channel::Sender<Arrived>,
    /// What the op runner moved without learning the new UIDs.
    moved: Moved,
}

/// What woke the command connection's loop.
enum Woke {
    Control(Control),
    Interrupt(Interrupt),
    Due,
}

impl Poller {
    fn config(&self) -> &SyncConfig {
        &self.shared.sync
    }

    fn generation(&self) -> u64 {
        self.signals.borrow().reconnects
    }

    fn idling(&self) -> bool {
        self.idle.as_ref().is_some_and(|task| !task.is_finished())
    }

    async fn run(&mut self) {
        loop {
            let seen = self.generation();
            if self.conn.is_none() && !self.halted && Instant::now() >= self.retry_at {
                match race(&self.signals, seen, log_in(&self.shared, &self.settings)).await {
                    Ok(Ok(conn)) => {
                        self.conn = Some(conn);
                        self.used = Instant::now();
                        self.next_account = Instant::now();
                        self.ops_now = true;
                        self.outbox_now = true;
                        self.drafts_now = true;
                    }
                    Ok(Err(failure)) => self.failed(failure),
                    Err(interrupt) => {
                        if self.interrupted(interrupt).await {
                            return;
                        }
                        continue;
                    }
                }
            }
            if self.conn.is_some() {
                match self.work(seen).await {
                    Ok(()) => {}
                    Err(interrupt) => {
                        if self.interrupted(interrupt).await {
                            return;
                        }
                        continue;
                    }
                }
            }
            if let Some(woke) = self.wait(seen).await {
                match woke {
                    Woke::Control(control) => self.control(control).await,
                    Woke::Interrupt(interrupt) => {
                        if self.interrupted(interrupt).await {
                            return;
                        }
                    }
                    Woke::Due => {}
                }
            }
        }
    }

    /// A reconnect drops the connection and tries again now, backoff and halt forgotten; a stop
    /// logs out and ends the IDLE connection too. True when the actor ends.
    async fn interrupted(&mut self, interrupt: Interrupt) -> bool {
        match interrupt {
            Interrupt::Reconnect => {
                self.conn = None;
                self.failures = 0;
                self.halted = false;
                self.retry_at = Instant::now();
                self.status.set(SyncStatus::Syncing);
                false
            }
            Interrupt::Stop(stop) => {
                if let (Some(conn), Stop::Logout(wait)) = (self.conn.take(), stop) {
                    conn.logout_within(wait).await;
                }
                if let Some(mut idle) = self.idle.take() {
                    let wait = match stop {
                        Stop::Logout(wait) => wait * 3,
                        Stop::Drop => Duration::from_millis(50),
                    };
                    if tokio::time::timeout(wait, &mut idle).await.is_err() {
                        idle.abort();
                    }
                }
                true
            }
        }
    }

    fn failed(&mut self, failure: LoginFailure) {
        let status = SyncStatus::from_failure(failure);
        tracing::info!(
            account = self.account.0,
            ?failure,
            "the account couldn't connect"
        );
        self.status.set(status);
        if halts(status) {
            self.halted = true;
            return;
        }
        self.failures += 1;
        let unit = self.jitter.unit();
        self.retry_at = Instant::now() + backoff_delay(self.config(), self.failures, unit);
    }

    /// The connection broke: it is made again at once the first time, after the backoff when
    /// that fails too. The status says offline only once connecting fails.
    fn lost(&mut self) {
        self.conn = None;
        self.failures += 1;
        let unit = self.jitter.unit();
        self.retry_at = Instant::now() + backoff_delay(self.config(), self.failures, unit);
    }

    /// A pass ended with an error: a lost connection is made again; anything else waits for
    /// the next pass.
    fn pass_failed(&mut self, error: &SyncError) {
        match error {
            SyncError::Imap(imap) if imap.is_connection() => {
                tracing::info!(account = self.account.0, %error, "the command connection was lost");
                self.lost();
            }
            _ => tracing::warn!(account = self.account.0, %error, "a pass failed"),
        }
    }

    /// The next thing to do, or `None` when nothing is due before the next control message.
    async fn wait(&mut self, seen: u64) -> Option<Woke> {
        let now = Instant::now();
        let due = if self.conn.is_some() {
            let mut due = self.next_account;
            if !self.idling() {
                due = due.min(self.next_inbox);
            }
            if self.opened.is_some() {
                due = due.min(self.next_opened);
            }
            for next in [self.next_ops, self.next_outbox, self.next_drafts]
                .into_iter()
                .flatten()
            {
                due = due.min(next);
            }
            if self.opened_now
                || self.ops_now
                || self.outbox_now
                || self.drafts_now
                || !self.older.is_empty()
            {
                due = now;
            }
            Some(due)
        } else if self.halted {
            None
        } else {
            Some(self.retry_at)
        };
        if due.is_some_and(|due| due <= now) {
            return Some(Woke::Due);
        }
        let sleep = async {
            match due {
                Some(due) => tokio::time::sleep_until(due.into()).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            interrupt = interrupted(self.signals.clone(), seen) => Some(Woke::Interrupt(interrupt)),
            control = self.controls.recv() => control.ok().map(Woke::Control),
            _ = sleep => Some(Woke::Due),
        }
    }

    async fn control(&mut self, control: Control) {
        match control {
            Control::Opened(folder) => {
                self.opened = folder;
                self.opened_now = folder.is_some();
            }
            Control::Older(folder) => {
                if self.conn.is_some() {
                    self.older.push_back(folder);
                } else {
                    let complete = self
                        .shared
                        .store
                        .folder_sync(folder)
                        .recv()
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|state| state.complete());
                    self.shared.send(Event::OlderLoaded {
                        folder,
                        fetched: 0,
                        complete,
                    });
                }
            }
            Control::Ops => self.ops_now = true,
            Control::Outbox => self.outbox_now = true,
            Control::Drafts => self.drafts_now = true,
        }
    }

    /// Whatever is due, on a connection checked with NOOP when it sat unused.
    async fn work(&mut self, seen: u64) -> Result<(), Interrupt> {
        if self.used.elapsed() >= self.config().reuse_check {
            let wait = self.config().noop_timeout;
            let Some(conn) = self.conn.as_mut() else {
                return Ok(());
            };
            let alive = race(&self.signals, seen, tokio::time::timeout(wait, conn.noop())).await?;
            self.used = Instant::now();
            if !matches!(alive, Ok(Ok(_))) {
                tracing::info!(
                    account = self.account.0,
                    "a quiet connection didn't answer NOOP"
                );
                self.lost();
                return Ok(());
            }
        }
        if self.ops_now || self.next_ops.is_some_and(|due| due <= Instant::now()) {
            self.run_ops(seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        if self.outbox_now || self.next_outbox.is_some_and(|due| due <= Instant::now()) {
            self.run_outbox(seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        if self.drafts_now || self.next_drafts.is_some_and(|due| due <= Instant::now()) {
            self.run_drafts(seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        while let Some(folder) = self.older.pop_front() {
            self.load_older(folder, seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        let now = Instant::now();
        if self.opened_now || self.opened.is_some() && now >= self.next_opened {
            self.opened_now = false;
            if let Some(folder) = self.opened {
                self.next_opened = now + self.config().opened_poll;
                let under_idle = self.idling() && *self.inbox.borrow() == Some(folder);
                if !under_idle {
                    self.folder_pass(folder, seen).await?;
                }
                if self.conn.is_none() {
                    return Ok(());
                }
            }
        }
        if Instant::now() >= self.next_account {
            self.account_pass(seen).await?;
            if self.conn.is_none() {
                return Ok(());
            }
        }
        if !self.idling() && Instant::now() >= self.next_inbox {
            self.next_inbox = Instant::now() + self.config().inbox_poll;
            let inbox = *self.inbox.borrow();
            if let Some(inbox) = inbox {
                self.folder_pass(inbox, seen).await?;
            }
        }
        Ok(())
    }

    fn options(&self) -> SyncOptions {
        let mut options = self.config().options.clone();
        options.arrivals = Some(self.arrivals.clone());
        if self.idling() {
            options.elsewhere = (*self.inbox.borrow()).map(|folder| Elsewhere {
                folder,
                backstop: true,
            });
        }
        options
    }

    async fn account_pass(&mut self, seen: u64) -> Result<(), Interrupt> {
        let options = self.options();
        let every = self.config().progress_every;
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        let status = self.status.clone();
        let passed = race(
            &self.signals,
            seen,
            imap::sync_account(
                conn,
                &self.shared.store,
                self.account,
                &options,
                &self.locks,
                |folder, progress| status.progress(folder, progress, every),
            ),
        )
        .await?;
        self.used = Instant::now();
        self.next_account = Instant::now() + self.config().account_pass;
        match passed {
            Ok(report) => {
                self.failures = 0;
                let stored = report.stats.stored;
                self.done(report).await;
                self.status.set(self.healthy());
                self.find_inbox().await;
                if stored > 0 {
                    bodies::prefetch(&self.shared);
                }
                self.start_idle();
            }
            Err(error) => self.pass_failed(&error),
        }
        Ok(())
    }

    async fn folder_pass(&mut self, folder: FolderId, seen: u64) -> Result<(), Interrupt> {
        let options = self.options();
        let every = self.config().progress_every;
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        let status = self.status.clone();
        let passed = race(
            &self.signals,
            seen,
            imap::sync_folder(
                conn,
                &self.shared.store,
                folder,
                &options,
                &self.locks,
                |progress| status.progress(folder, progress, every),
            ),
        )
        .await?;
        self.used = Instant::now();
        match passed {
            Ok(report) => {
                self.done(report).await;
                if self.status.current() == Some(SyncStatus::Syncing) {
                    self.status.set(self.healthy());
                }
            }
            Err(SyncError::Imap(imap::ImapError::No(_)) | SyncError::NotSelectable) => {}
            Err(error) => self.pass_failed(&error),
        }
        Ok(())
    }

    async fn load_older(&mut self, folder: FolderId, seen: u64) -> Result<(), Interrupt> {
        let options = SyncOptions {
            arrivals: None,
            ..self.options()
        };
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };
        let loaded = race(
            &self.signals,
            seen,
            imap::load_older(conn, &self.shared.store, folder, &options, &self.locks),
        )
        .await?;
        self.used = Instant::now();
        let (fetched, complete) = match loaded {
            Ok(older) => {
                let answer = (older.fetched, older.complete);
                self.done(older.report).await;
                answer
            }
            Err(error) => {
                self.pass_failed(&error);
                (0, false)
            }
        };
        self.shared.send(Event::OlderLoaded {
            folder,
            fetched,
            complete,
        });
        Ok(())
    }

    async fn done(&self, report: SyncReport) {
        discard(&self.shared, report.orphaned_blobs).await;
    }

    async fn find_inbox(&self) {
        let inbox = self
            .shared
            .store
            .account_folders(self.account)
            .recv()
            .await
            .ok()
            .and_then(|folders| {
                folders
                    .into_iter()
                    .find(|folder| folder.role == FolderRole::Inbox && folder.selectable)
            })
            .map(|folder| folder.folder);
        self.inbox.send_if_modified(|known| {
            let changed = *known != inbox;
            *known = inbox;
            changed
        });
    }

    /// The IDLE connection, once INBOX is known and the server offers IDLE.
    fn start_idle(&mut self) {
        let offered = self
            .conn
            .as_ref()
            .is_some_and(|conn| conn.caps().has("IDLE"));
        if !offered || self.idling() || self.inbox.borrow().is_none() {
            return;
        }
        self.idle = Some(tokio::spawn(idle(
            self.shared.clone(),
            self.settings.clone(),
            self.account,
            self.status.clone(),
            self.locks.clone(),
            self.inbox.subscribe(),
            self.signals.clone(),
            self.arrivals.clone(),
        )));
    }
}

/// The IDLE connection: INBOX current within a moment of every change the server announces.
#[allow(clippy::too_many_arguments)]
async fn idle(
    shared: Arc<Shared>,
    settings: Arc<AccountSettings>,
    account: AccountId,
    status: Arc<Status>,
    locks: FolderLocks,
    inbox: watch::Receiver<Option<FolderId>>,
    signals: watch::Receiver<Signal>,
    arrivals: async_channel::Sender<Arrived>,
) {
    let config = shared.sync.clone();
    let options = SyncOptions {
        arrivals: Some(arrivals),
        ..config.options.clone()
    };
    let mut jitter = Jitter::new(account);
    let mut failures = 0u32;
    loop {
        if failures > 0 {
            let seen = signals.borrow().reconnects;
            let delay = backoff_delay(&config, failures, jitter.unit());
            match race(&signals, seen, tokio::time::sleep(delay)).await {
                Ok(()) => {}
                Err(Interrupt::Reconnect) => failures = 0,
                Err(Interrupt::Stop(_)) => return,
            }
        }
        let seen = signals.borrow().reconnects;
        let conn = match race(&signals, seen, log_in(&shared, &settings)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(failure)) => {
                if halts(SyncStatus::from_failure(failure)) {
                    // The command connection reports it; wait for a reconnect or the end.
                    match interrupted(signals.clone(), seen).await {
                        Interrupt::Reconnect => continue,
                        Interrupt::Stop(_) => return,
                    }
                }
                failures += 1;
                continue;
            }
            Err(Interrupt::Reconnect) => continue,
            Err(Interrupt::Stop(_)) => return,
        };
        let watch = Idling {
            shared: &shared,
            config: &config,
            options: &options,
            account,
            status: &status,
            locks: &locks,
            inbox: &inbox,
            signals: &signals,
            seen,
        };
        match watch.keep_current(conn).await {
            Kept::Lost => failures += 1,
            Kept::Reconnect => failures = 0,
            Kept::Stopped => return,
            Kept::Logout(conn, wait) => {
                conn.logout_within(wait).await;
                return;
            }
        }
    }
}

/// How the IDLE connection's loop ended.
enum Kept {
    Lost,
    Reconnect,
    Stopped,
    Logout(Box<Connection>, Duration),
}

/// What the IDLE connection's loop works with.
struct Idling<'a> {
    shared: &'a Arc<Shared>,
    config: &'a SyncConfig,
    /// The configuration's, with the account's new mail sink.
    options: &'a SyncOptions,
    account: AccountId,
    status: &'a Arc<Status>,
    locks: &'a FolderLocks,
    inbox: &'a watch::Receiver<Option<FolderId>>,
    signals: &'a watch::Receiver<Signal>,
    seen: u64,
}

impl Idling<'_> {
    fn interrupted(interrupt: Interrupt, conn: Connection) -> Kept {
        match interrupt {
            Interrupt::Reconnect => Kept::Reconnect,
            Interrupt::Stop(Stop::Drop) => Kept::Stopped,
            Interrupt::Stop(Stop::Logout(wait)) => Kept::Logout(Box::new(conn), wait),
        }
    }

    /// A pass, NOOP (another pass if it reported a change), IDLE; again on every wake.
    async fn keep_current(&self, mut conn: Connection) -> Kept {
        loop {
            let Some(folder) = *self.inbox.borrow() else {
                return Kept::Stopped;
            };
            let every = self.config.progress_every;
            let status = self.status;
            let passed = race(
                self.signals,
                self.seen,
                imap::sync_folder(
                    &mut conn,
                    &self.shared.store,
                    folder,
                    self.options,
                    self.locks,
                    |progress| status.progress(folder, progress, every),
                ),
            )
            .await;
            match passed {
                Ok(Ok(report)) => {
                    let stored = report.stats.stored;
                    discard(self.shared, report.orphaned_blobs).await;
                    // Progress showed the account syncing; the pass is over.
                    if self.status.current() == Some(SyncStatus::Syncing) {
                        self.status.set(SyncStatus::Synced);
                    }
                    if stored > 0 {
                        bodies::prefetch(self.shared);
                    }
                }
                Ok(Err(error)) => {
                    tracing::info!(account = self.account.0, %error, "the IDLE connection's pass failed");
                    return Kept::Lost;
                }
                Err(interrupt) => return Self::interrupted(interrupt, conn),
            }
            match race(self.signals, self.seen, conn.noop()).await {
                Ok(Ok(true)) => continue,
                Ok(Ok(false)) => {}
                Ok(Err(_)) => return Kept::Lost,
                Err(interrupt) => return Self::interrupted(interrupt, conn),
            }
            let signals = self.signals.clone();
            let seen = self.seen;
            let stop = async move {
                match interrupted(signals, seen).await {
                    Interrupt::Reconnect => Stop::Drop,
                    Interrupt::Stop(stop) => stop,
                }
            };
            match conn.idle(self.config.idle_restart, stop).await {
                Ok(Idled::Woke(back, _)) => conn = *back,
                Ok(Idled::Stopped) => {
                    return match self.signals.borrow().stop {
                        Some(_) => Kept::Stopped,
                        None => Kept::Reconnect,
                    };
                }
                Err(error) => {
                    tracing::info!(account = self.account.0, %error, "IDLE ended without the server");
                    return Kept::Lost;
                }
            }
        }
    }
}
