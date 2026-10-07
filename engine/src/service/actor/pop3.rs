//! A POP3 account's actor (M8: ML-087, ML-088). POP3 has no IDLE and no folders, so the actor
//! polls: a pass at its start, on `Reconnect`, on `CheckMail` (Refresh) and every `pop3_poll`.
//! A pass logs in, lists the maildrop (UIDL and LIST), downloads what the store hasn't recorded
//! newest first into the account's local Inbox, whole up to the client's size cap (above it,
//! its header only, and it stays on the server), a few messages per transaction with their
//! UIDLs, then marks for deletion what the account's policy says (only messages the store
//! recorded whole) and ends with QUIT, which alone makes the server delete them. Messages that
//! share a UIDL are left alone. A cut connection deletes nothing and leaves the message it cut
//! for the next pass, which downloads it again; none is stored twice. Mail downloaded before
//! the account's first complete pass isn't new mail; later mail goes out as `Event::NewMail` as
//! each batch commits. Downloaded mail is unread unless its header says otherwise.
//!
//! The outbox runs beside the passes on the same task, so a long first download never holds
//! up a send: the IMAP actor's sends (`outbox::Submission`), with each Sent copy filed into the
//! local Sent folder. Drafts never go to a server: each draft's copy is filed into the local
//! Drafts folder when it is due, as an IMAP account's goes to its server's. The account's ops
//! are done at once in the store; the raw messages they leave unreferenced are deleted after
//! each pass.

use super::ops::{instant_at, unix_ms};
use super::outbox::{Submission, Unbuilt, build_drafts_copy};
use super::{
    Control, Handle, Interrupt, Jitter, Signal, Status, backoff_delay, discard, halts, interrupted,
    race,
};
use crate::api::{Event, SyncStatus};
use crate::imap::Progress;
use crate::pop3::{self, Busy, Credentials, Expire, Listing, Mailbox, Pop3Error};
use crate::service::{Shared, password, tls};
use sealwax_domain::account::{AccountSettings, AuthMethod, LoginFailure, Pop3Policy, Receive};
use sealwax_domain::message;
use sealwax_domain::model::{AccountId, Flags, FolderId, MessageId};
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{
    BodyState, DraftUpload, DueSentCopy, Failure, NewMessage, Pop3Download, Pop3PassDone, Pop3Uidl,
    SentCopyDone,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, watch};

#[cfg(test)]
mod tests;

/// Messages stored per transaction, at most.
const BATCH: usize = 25;
/// A batch is stored once its first message has waited this long.
const BATCH_WAIT: Duration = Duration::from_millis(500);
const DAY: i64 = 86_400;

/// What the server's CAPA said at the login that the passes heed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Offers {
    /// LOGIN-DELAY: the least time between logins.
    pub login_delay: Option<Duration>,
    /// EXPIRE: how long the server keeps mail once it was retrieved.
    pub expire: Option<Expire>,
}

/// Why a login didn't open the maildrop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refused {
    /// The account's status says why.
    Login(LoginFailure),
    /// The server can't open the maildrop now (another app holds it, LOGIN-DELAY, a temporary
    /// problem): tried again later, without the user.
    Busy(Busy),
}

/// How the actor reaches the account's server: the POP3 client, or a test's mailbox.
pub(super) trait Connect: Send + Sync + 'static {
    type Mailbox: Mailbox + 'static;

    /// Connects and logs in as the account.
    fn connect(
        &self,
        shared: &Shared,
        settings: &AccountSettings,
    ) -> impl Future<Output = Result<(Self::Mailbox, Offers), Refused>> + Send;
}

/// The POP3 client, with the account's password: POP3 accounts are password accounts in 1.0
/// (Microsoft's POP3 would need a sign-in scope of its own).
pub(super) struct Network;

impl Connect for Network {
    type Mailbox = pop3::Session;

    async fn connect(
        &self,
        shared: &Shared,
        settings: &AccountSettings,
    ) -> Result<(pop3::Session, Offers), Refused> {
        if let AuthMethod::OAuth(_) = settings.auth {
            tracing::info!("a POP3 account signs in with a password");
            return Err(Refused::Login(LoginFailure::Other));
        }
        let password = password(shared, settings).await.map_err(Refused::Login)?;
        let password = std::str::from_utf8(&password)
            .map_err(|_| Refused::Login(LoginFailure::WrongPassword))?;
        let credentials = Credentials::Password {
            username: &settings.username,
            password,
        };
        let tls = tls(shared).map_err(Refused::Login)?;
        match pop3::connect(&settings.incoming, credentials, tls).await {
            Ok(session) => {
                let caps = session.capabilities();
                let offers = Offers {
                    login_delay: caps.login_delay,
                    expire: caps.expire,
                };
                Ok((session, offers))
            }
            Err(Pop3Error::Busy(busy)) => Err(Refused::Busy(busy)),
            Err(error) => Err(Refused::Login(error.failure())),
        }
    }
}

/// Unix seconds.
fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// Whether `policy` deletes from the server, at `now`, a message the store downloaded whole at
/// `downloaded` (Unix seconds): every one when the account doesn't leave mail on the server;
/// else those downloaded `delete_after_days` ago or longer; else none.
pub(super) fn deletes(policy: Pop3Policy, now: i64, downloaded: i64) -> bool {
    if !policy.leave_on_server {
        return true;
    }
    policy
        .delete_after_days
        .is_some_and(|days| downloaded <= now.saturating_sub(i64::from(days) * DAY))
}

/// UIDLs the server lists for more than one message: those messages are left alone, never
/// downloaded nor deleted, as no UIDL tells them apart.
pub(super) fn shared_uidls(listing: &[Listing]) -> HashSet<&str> {
    let mut seen = HashSet::new();
    listing
        .iter()
        .filter(|entry| !seen.insert(entry.uidl.as_str()))
        .map(|entry| entry.uidl.as_str())
        .collect()
}

/// The maildrop's messages the store hasn't recorded, newest (highest number) first, without
/// those whose UIDL another message shares.
pub(super) fn to_download<'a>(listing: &'a [Listing], known: &HashSet<&str>) -> Vec<&'a Listing> {
    let shared = shared_uidls(listing);
    let mut new: Vec<&Listing> = listing
        .iter()
        .filter(|entry| {
            !known.contains(entry.uidl.as_str()) && !shared.contains(entry.uidl.as_str())
        })
        .collect();
    new.sort_by_key(|entry| std::cmp::Reverse(entry.number));
    new
}

/// Whether the message's header says it was read: an mbox `Status:` field with `R`. Sealwax's
/// read state is its own otherwise: what the server does at QUIT (Dovecot marks downloaded mail
/// read, Stalwart doesn't) never shows.
pub(super) fn read_by_header(raw: &[u8]) -> bool {
    raw.split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            let colon = line.iter().position(|byte| *byte == b':')?;
            line[..colon]
                .eq_ignore_ascii_case(b"status")
                .then_some(&line[colon + 1..])
        })
        .any(|value| value.contains(&b'R'))
}

impl Handle {
    /// A POP3 account's actor (M8).
    pub fn start_pop3(shared: &Arc<Shared>, settings: AccountSettings) -> Handle {
        start_with(shared, settings, Network)
    }
}

/// A POP3 actor reaching its server through `connector`.
pub(super) fn start_with<C: Connect>(
    shared: &Arc<Shared>,
    settings: AccountSettings,
    connector: C,
) -> Handle {
    let (control, controls) = async_channel::unbounded();
    let (signal, signals) = watch::channel(Signal::default());
    let account = Arc::new(OnceLock::new());
    let task = tokio::spawn(run(
        shared.clone(),
        settings.clone(),
        account.clone(),
        controls,
        signals,
        connector,
    ));
    Handle {
        settings,
        account,
        control,
        signal,
        task,
    }
}

async fn run<C: Connect>(
    shared: Arc<Shared>,
    settings: AccountSettings,
    cell: Arc<OnceLock<AccountId>>,
    controls: async_channel::Receiver<Control>,
    signals: watch::Receiver<Signal>,
    connector: C,
) {
    let Receive::Pop3(policy) = settings.receive else {
        return;
    };
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
    let local = match shared.store.pop3_account(account).recv().await {
        Ok(local) => local,
        Err(error) => {
            tracing::warn!(%error, "a POP3 account's local folders couldn't be made");
            return;
        }
    };
    let _ = cell.set(account);
    let status = Arc::new(Status::new(shared.clone(), account));
    status.set(SyncStatus::Syncing);
    let settings = Arc::new(settings);
    let wake = Arc::new(Notify::new());
    let drafts = Arc::new(Notify::new());
    let incoming = Arc::new(Mutex::new(SyncStatus::Synced));
    let mut passes = Passes {
        jitter: Jitter::new(account),
        shared: shared.clone(),
        settings: settings.clone(),
        policy,
        account,
        inbox: local.inbox,
        first_pass: local.first_pass,
        status: status.clone(),
        signals: signals.clone(),
        controls,
        connector,
        failures: 0,
        halted: false,
        next_pass: Instant::now(),
        wake: wake.clone(),
        drafts: drafts.clone(),
        incoming: incoming.clone(),
        login_delay: None,
        last_login: None,
        expire_told: false,
        too_large: HashSet::new(),
    };
    let mut outbox = Outbox {
        submission: Submission::new(shared.clone(), settings, account, status, signals.clone()),
        shared,
        account,
        signals,
        wake,
        drafts,
        drafts_now: true,
        next_drafts: None,
        incoming,
    };
    tokio::join!(passes.run(), outbox.run());
}

/// What ended a pass's session early.
enum Failed {
    /// The server or the connection.
    Server(Pop3Error),
    /// The store couldn't read or record.
    Store,
}

/// The passes: the account's downloads and the server's deletions.
struct Passes<C: Connect> {
    shared: Arc<Shared>,
    settings: Arc<AccountSettings>,
    policy: Pop3Policy,
    account: AccountId,
    inbox: FolderId,
    /// When the account's first complete pass ended; before it, downloads aren't new mail.
    first_pass: Option<i64>,
    status: Arc<Status>,
    signals: watch::Receiver<Signal>,
    controls: async_channel::Receiver<Control>,
    connector: C,
    jitter: Jitter,
    failures: u32,
    /// A refused login or a changed certificate: no pass until `Reconnect`.
    halted: bool,
    next_pass: Instant,
    /// Wakes the outbox.
    wake: Arc<Notify>,
    /// Wakes the outbox's drafts.
    drafts: Arc<Notify>,
    /// What the account shows from its incoming side: the outbox's status once a submission
    /// login works again.
    incoming: Arc<Mutex<SyncStatus>>,
    /// The server's LOGIN-DELAY, once a login said it: no login comes sooner after another.
    login_delay: Option<Duration>,
    /// When the last login was tried.
    last_login: Option<Instant>,
    /// The server's EXPIRE was logged for this policy.
    expire_told: bool,
    /// UIDLs whose data outgrew the size cap while it came, closing the session: their header
    /// is fetched instead from then on.
    too_large: HashSet<String>,
}

impl<C: Connect> Passes<C> {
    fn generation(&self) -> u64 {
        self.signals.borrow().reconnects
    }

    async fn run(&mut self) {
        loop {
            let seen = self.generation();
            if !self.halted
                && Instant::now() >= self.next_pass
                && let Err(interrupt) = self.pass(seen).await
            {
                if self.interrupted(interrupt) {
                    return;
                }
                continue;
            }
            let due = (!self.halted).then_some(self.next_pass);
            let sleep = async {
                match due {
                    Some(due) => tokio::time::sleep_until(due.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                interrupt = interrupted(self.signals.clone(), seen) => {
                    if self.interrupted(interrupt) {
                        return;
                    }
                }
                control = self.controls.recv() => match control {
                    Ok(control) => self.control(control),
                    Err(_) => return,
                },
                () = sleep => {}
            }
        }
    }

    /// A reconnect tries again now, backoff and halt forgotten. True when the actor ends.
    fn interrupted(&mut self, interrupt: Interrupt) -> bool {
        match interrupt {
            Interrupt::Reconnect => {
                self.failures = 0;
                self.halted = false;
                self.next_pass = self.earliest_login();
                self.status.set(SyncStatus::Syncing);
                false
            }
            Interrupt::Stop(_) => true,
        }
    }

    fn control(&mut self, control: Control) {
        match control {
            // Local folders: nothing to sync, nothing older on a server, no op for a server.
            Control::Opened(_) | Control::Ops => {}
            Control::Drafts => self.drafts.notify_one(),
            Control::CheckMail => self.next_pass = self.earliest_login(),
            Control::Older(folder) => self.shared.send(Event::OlderLoaded {
                folder,
                fetched: 0,
                complete: true,
            }),
            Control::Outbox => self.wake.notify_one(),
        }
    }

    /// Now, or once the server's LOGIN-DELAY since the last login has passed.
    fn earliest_login(&self) -> Instant {
        let now = Instant::now();
        match (self.last_login, self.login_delay) {
            (Some(last), Some(delay)) => (last + delay).max(now),
            _ => now,
        }
    }

    fn set_incoming(&self, status: SyncStatus) {
        *self.incoming.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    /// Logging in failed: the account says why, and tries again after the backoff, or not
    /// until `Reconnect` when the user must act.
    fn failed(&mut self, failure: LoginFailure) {
        let status = SyncStatus::from_failure(failure);
        tracing::info!(
            account = self.account.0,
            ?failure,
            "the POP3 account couldn't connect"
        );
        self.status.set(status);
        self.set_incoming(status);
        if halts(status) {
            self.halted = true;
            return;
        }
        self.retry_later(false, None);
    }

    /// Tried again after the backoff; a connection lost mid-pass is made again at once the first
    /// time, a failed login never. No login comes sooner than the server's LOGIN-DELAY, or
    /// `least`.
    fn retry_later(&mut self, at_once: bool, least: Option<Duration>) {
        self.failures += 1;
        let failures = if at_once {
            self.failures
        } else {
            self.failures.max(2)
        };
        let unit = self.jitter.unit();
        let wait = backoff_delay(&self.shared.sync, failures, unit)
            .max(self.login_delay.unwrap_or_default())
            .max(least.unwrap_or_default());
        self.next_pass = Instant::now() + wait;
    }

    /// The server can't open the maildrop now: the status stays, and the login waits.
    fn busy(&mut self, busy: Busy) {
        tracing::info!(account = self.account.0, ?busy, "the POP3 maildrop is busy");
        let least = match busy {
            Busy::LoginDelay(delay) => {
                if delay.is_some() {
                    self.login_delay = delay;
                }
                delay
            }
            Busy::InUse | Busy::Temporary => None,
        };
        self.retry_later(false, least);
    }

    /// What the login's CAPA said: LOGIN-DELAY spaces the logins; an EXPIRE that keeps
    /// retrieved mail for less than the policy leaves it on the server is logged once.
    fn offers(&mut self, offers: Offers) {
        if offers.login_delay.is_some() {
            self.login_delay = offers.login_delay;
        }
        let Some(Expire::Days(days)) = offers.expire else {
            return;
        };
        let kept = self.policy.leave_on_server
            && self
                .policy
                .delete_after_days
                .is_none_or(|after| after > days);
        if kept && !self.expire_told {
            self.expire_told = true;
            tracing::info!(
                account = self.account.0,
                days,
                "the POP3 server deletes retrieved mail itself (EXPIRE): leave on server can't hold"
            );
        }
    }

    async fn pass(&mut self, seen: u64) -> Result<(), Interrupt> {
        self.last_login = Some(Instant::now());
        let connected = race(
            &self.signals,
            seen,
            self.connector.connect(&self.shared, &self.settings),
        )
        .await?;
        let mailbox = match connected {
            Ok((mailbox, offers)) => {
                self.offers(offers);
                mailbox
            }
            Err(Refused::Login(failure)) => {
                self.failed(failure);
                return Ok(());
            }
            Err(Refused::Busy(busy)) => {
                self.busy(busy);
                return Ok(());
            }
        };
        match self.session(mailbox, seen).await? {
            Ok(()) => {
                self.failures = 0;
                self.next_pass = Instant::now()
                    + self
                        .shared
                        .sync
                        .pop3_poll
                        .max(self.login_delay.unwrap_or_default());
                self.set_incoming(SyncStatus::Synced);
                self.status
                    .set(self.status.smtp().unwrap_or(SyncStatus::Synced));
                match self.shared.store.take_blob_deletions().recv().await {
                    Ok(orphaned) => discard(&self.shared, orphaned).await,
                    Err(error) => {
                        tracing::warn!(%error, "raw messages left by local ops couldn't be read");
                    }
                }
            }
            Err(Failed::Server(error)) if error.is_connection() => {
                tracing::info!(account = self.account.0, %error, "the POP3 connection was lost");
                self.retry_later(true, None);
            }
            Err(Failed::Server(Pop3Error::Busy(busy))) => self.busy(busy),
            Err(Failed::Server(error)) => {
                tracing::warn!(account = self.account.0, %error, "a POP3 pass failed");
                self.failed(LoginFailure::Other);
            }
            Err(Failed::Store) => self.retry_later(false, None),
        }
        Ok(())
    }

    /// One session: the listing, the downloads, the deletions and QUIT. An interrupt drops the
    /// connection without QUIT, so the server deletes nothing.
    async fn session(
        &mut self,
        mut mailbox: C::Mailbox,
        seen: u64,
    ) -> Result<Result<(), Failed>, Interrupt> {
        let listing = match race(&self.signals, seen, mailbox.listing()).await? {
            Ok(listing) => listing,
            Err(error) => return Ok(Err(Failed::Server(error))),
        };
        let known = match self.shared.store.pop3_uidls(self.account).recv().await {
            Ok(known) => known,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "the downloaded UIDLs couldn't be read");
                return Ok(Err(Failed::Store));
            }
        };
        let mut downloaded: HashMap<String, (i64, bool)> = known
            .iter()
            .map(|uidl| (uidl.uidl.clone(), (uidl.downloaded, uidl.whole)))
            .collect();
        let mut downloads = Downloads::new(self, listing.len());
        let complete = {
            let held: HashSet<&str> = downloaded.keys().map(String::as_str).collect();
            let new = to_download(&listing, &held);
            downloads.total = u32::try_from(new.len()).unwrap_or(u32::MAX);
            let fetched = downloads.fetch(self, &mut mailbox, &new, seen).await;
            self.too_large
                .extend(std::mem::take(&mut downloads.too_large));
            let stored = downloads.flush(self).await;
            match (fetched, stored) {
                (Err(interrupt), _) => return Err(interrupt),
                (Ok(Err(failed)), _) | (Ok(Ok(_)), Err(failed)) => return Ok(Err(failed)),
                (Ok(Ok(complete)), Ok(())) => complete,
            }
        };
        downloaded.extend(downloads.stored);
        let now = unix_seconds();
        let shared = shared_uidls(&listing);
        let mut marked = Vec::new();
        for entry in &listing {
            let Some((at, whole)) = downloaded.get(&entry.uidl) else {
                continue;
            };
            if !whole || shared.contains(entry.uidl.as_str()) || !deletes(self.policy, now, *at) {
                continue;
            }
            match race(&self.signals, seen, mailbox.delete(entry.number)).await? {
                Ok(()) => marked.push(entry.uidl.clone()),
                Err(Pop3Error::Err) => {}
                Err(error) => return Ok(Err(Failed::Server(error))),
            }
        }
        // A QUIT without its answer may still have deleted what was marked (on the testbed it
        // always had): recorded as deleted all the same, the next listing tells.
        if let Err(error) = race(&self.signals, seen, mailbox.quit()).await? {
            tracing::info!(account = self.account.0, %error, "QUIT went unanswered or was refused");
        }
        let deleted = marked;
        let listed: HashSet<&str> = listing.iter().map(|entry| entry.uidl.as_str()).collect();
        let forget: Vec<String> = known
            .iter()
            .filter(|uidl| uidl.deleted.is_some() && !listed.contains(uidl.uidl.as_str()))
            .map(|uidl: &Pop3Uidl| uidl.uidl.clone())
            .collect();
        if !deleted.is_empty() {
            tracing::info!(
                account = self.account.0,
                messages = deleted.len(),
                "deleted from the POP3 server by the account's policy"
            );
        }
        let done = Pop3PassDone {
            complete,
            deleted,
            forget,
            at: now,
        };
        if let Err(error) = self
            .shared
            .store
            .pop3_pass_done(self.account, done)
            .recv()
            .await
        {
            tracing::warn!(account = self.account.0, %error, "a POP3 pass's end couldn't be recorded");
        } else if complete && self.first_pass.is_none() {
            self.first_pass = Some(now);
        }
        Ok(Ok(()))
    }
}

/// One pass's downloads: stored a batch at a time, with progress.
struct Downloads {
    batch: Vec<Pop3Download>,
    since: Option<Instant>,
    /// UIDLs stored by this pass, with when and whether whole.
    stored: Vec<(String, (i64, bool))>,
    /// UIDLs whose data outgrew the size cap while it came (the session is closed).
    too_large: Vec<String>,
    fetched: u32,
    total: u32,
    /// Mail stored now is new mail (the account's first complete pass is behind it).
    new_mail: bool,
}

impl Downloads {
    fn new<C: Connect>(passes: &Passes<C>, listed: usize) -> Downloads {
        Downloads {
            batch: Vec::new(),
            since: None,
            stored: Vec::with_capacity(listed.min(BATCH)),
            too_large: Vec::new(),
            fetched: 0,
            total: 0,
            new_mail: passes.first_pass.is_some(),
        }
    }

    /// Downloads `new` in order, storing a batch whenever it is full or has waited. Ok(true)
    /// when every message was downloaded or can never be (too large).
    async fn fetch<C: Connect>(
        &mut self,
        passes: &Passes<C>,
        mailbox: &mut C::Mailbox,
        new: &[&Listing],
        seen: u64,
    ) -> Result<Result<bool, Failed>, Interrupt> {
        let mut complete = true;
        let every = passes.shared.sync.progress_every;
        for entry in new {
            let retrieved = if passes.too_large.contains(&entry.uidl) {
                Err(Pop3Error::TooLarge(entry.size))
            } else {
                race(&passes.signals, seen, mailbox.retrieve(entry.number)).await?
            };
            let (message, whole) = match retrieved {
                Ok(bytes) => match prepare(passes, bytes).await {
                    Some(message) => (message, true),
                    None => return Ok(Err(Failed::Store)),
                },
                Err(Pop3Error::TooLarge(size)) => {
                    match race(&passes.signals, seen, mailbox.header(entry.number)).await? {
                        Ok(header) => (header_only(passes.inbox, &header, entry.size), false),
                        Err(error) if error.is_connection() => {
                            // The data outgrew the cap while it came, which closed the session.
                            self.too_large.push(entry.uidl.clone());
                            return Ok(Err(Failed::Server(error)));
                        }
                        Err(_) => {
                            tracing::info!(
                                account = passes.account.0,
                                size,
                                "a message too large to download stays on the server"
                            );
                            continue;
                        }
                    }
                }
                Err(Pop3Error::Err | Pop3Error::Busy(_)) => {
                    complete = false;
                    continue;
                }
                Err(error) => return Ok(Err(Failed::Server(error))),
            };
            self.batch.push(Pop3Download {
                uidl: entry.uidl.clone(),
                message,
                whole,
            });
            self.since.get_or_insert_with(Instant::now);
            self.fetched += 1;
            passes.status.progress(
                passes.inbox,
                Progress {
                    fetched: self.fetched,
                    total: self.total,
                },
                every,
            );
            if (self.batch.len() >= BATCH
                || self.since.is_some_and(|at| at.elapsed() >= BATCH_WAIT))
                && let Err(failed) = self.flush(passes).await
            {
                return Ok(Err(failed));
            }
        }
        Ok(Ok(complete))
    }

    /// Stores the batch with its UIDLs in one transaction; new mail goes out once it commits.
    async fn flush<C: Connect>(&mut self, passes: &Passes<C>) -> Result<(), Failed> {
        self.since = None;
        if self.batch.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.batch);
        let uidls: Vec<(String, bool)> = batch
            .iter()
            .map(|download| (download.uidl.clone(), download.whole))
            .collect();
        let at = unix_seconds();
        let stored = passes
            .shared
            .store
            .pop3_store(passes.account, batch, at)
            .recv()
            .await
            .map_err(|error| {
                tracing::warn!(account = passes.account.0, %error, "downloaded mail couldn't be stored");
                Failed::Store
            })?;
        self.stored
            .extend(uidls.into_iter().map(|(uidl, whole)| (uidl, (at, whole))));
        let messages: Vec<MessageId> = stored.ids.into_iter().flatten().collect();
        if self.new_mail && !messages.is_empty() {
            tracing::debug!(
                account = passes.account.0,
                messages = messages.len(),
                "new mail"
            );
            passes.shared.send(Event::NewMail {
                account: passes.account,
                folder: passes.inbox,
                messages,
            });
        }
        Ok(())
    }
}

/// The header of a message too large to download, as a row of the Inbox without a body: its
/// size is the server's, and the message stays there.
fn header_only(inbox: FolderId, header: &[u8], size: u64) -> NewMessage {
    let parsed = message::parse(header);
    NewMessage {
        folder: inbox,
        uid: None,
        flags: if read_by_header(header) {
            Flags::SEEN
        } else {
            Flags::NONE
        },
        date: parsed.headers.date.unwrap_or_else(unix_seconds),
        headers: parsed.headers,
        size,
        body_state: BodyState::Headers,
        ..NewMessage::default()
    }
}

/// A downloaded message, its bytes in the blob store and parsed for the Inbox: unread unless
/// its header says otherwise, sorted by its Date header, or by when it was downloaded without
/// one.
async fn prepare<C: Connect>(passes: &Passes<C>, bytes: Vec<u8>) -> Option<NewMessage> {
    let blobs = passes.shared.blobs.clone();
    let raw = RawMessage {
        folder: passes.inbox,
        uid: None,
        flags: if read_by_header(&bytes) {
            Flags::SEEN
        } else {
            Flags::NONE
        },
        keywords: Vec::new(),
        internal_date: None,
        raw: bytes,
    };
    let prepared = tokio::task::spawn_blocking(move || ingest::prepare(&blobs, vec![raw], 1))
        .await
        .ok()?;
    match prepared {
        Ok(prepared) => prepared.messages.into_iter().next().map(|mut message| {
            if message.headers.date.is_none() {
                message.date = unix_seconds();
            }
            message
        }),
        Err(error) => {
            tracing::warn!(account = passes.account.0, %error, "a downloaded message couldn't be kept");
            None
        }
    }
}

/// The outbox beside the passes: the account's sends, their Sent copies filed locally, and its
/// drafts' copies filed into the local Drafts folder.
struct Outbox {
    submission: Submission,
    shared: Arc<Shared>,
    account: AccountId,
    signals: watch::Receiver<Signal>,
    wake: Arc<Notify>,
    drafts: Arc<Notify>,
    /// File the drafts' copies that are due at the next chance.
    drafts_now: bool,
    /// When the next draft's copy becomes due.
    next_drafts: Option<Instant>,
    incoming: Arc<Mutex<SyncStatus>>,
}

impl Outbox {
    async fn run(&mut self) {
        loop {
            let seen = self.signals.borrow().reconnects;
            if self.submission.is_due() {
                // A send done makes its Sent copy due at once.
                match self.round(seen).await {
                    Ok(()) => continue,
                    Err(Interrupt::Reconnect) => {
                        self.submission.reset();
                        continue;
                    }
                    Err(Interrupt::Stop(_)) => return,
                }
            }
            if self.drafts_now || self.next_drafts.is_some_and(|due| due <= Instant::now()) {
                self.file_drafts().await;
                continue;
            }
            let next = match (self.submission.next, self.next_drafts) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let sleep = async {
                match next {
                    Some(due) => tokio::time::sleep_until(due.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                interrupt = interrupted(self.signals.clone(), seen) => match interrupt {
                    Interrupt::Reconnect => self.submission.reset(),
                    Interrupt::Stop(_) => return,
                },
                () = self.wake.notified() => self.submission.now = true,
                () = self.drafts.notified() => self.drafts_now = true,
                () = sleep => {}
            }
        }
    }

    async fn round(&mut self, seen: u64) -> Result<(), Interrupt> {
        let Some(due) = self.submission.due().await else {
            return Ok(());
        };
        if !due.sends.is_empty() {
            let base = *self.incoming.lock().unwrap_or_else(PoisonError::into_inner);
            let sent = self.submission.send_all(due.sends, None, base, seen).await;
            // A POP3 account's ops are done at once, and it has no Drafts-folder copies.
            self.submission.ops_queued = false;
            self.submission.copies_to_delete = false;
            sent?;
        }
        for copy in due.sent_copies {
            self.file(copy).await;
        }
        Ok(())
    }

    /// Files a sent message's Sent copy into the local Sent folder. A crash between the filing
    /// and its record files it again into a folder that holds those bytes already: nothing.
    async fn file(&mut self, copy: DueSentCopy) {
        let Some(folder) = copy.folder.as_ref().map(|folder| folder.folder) else {
            self.copy_done(&copy, SentCopyDone::GaveUp).await;
            return;
        };
        let blobs = self.shared.blobs.clone();
        let key = copy.blob;
        let prepared = tokio::task::spawn_blocking(move || {
            let raw = blobs.get(&key)?;
            ingest::prepare(
                &blobs,
                vec![RawMessage {
                    folder,
                    uid: None,
                    flags: Flags::SEEN,
                    keywords: Vec::new(),
                    internal_date: None,
                    raw,
                }],
                1,
            )
        })
        .await;
        let messages = match prepared {
            Ok(Ok(prepared)) => prepared.messages,
            _ => {
                tracing::warn!(
                    account = self.account.0,
                    "a Sent copy's bytes couldn't be read"
                );
                self.copy_done(&copy, SentCopyDone::GaveUp).await;
                return;
            }
        };
        match self.shared.store.ingest(messages).recv().await {
            Ok(_) => self.copy_done(&copy, SentCopyDone::Filed).await,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "a Sent copy couldn't be filed");
                let retry = self.shared.sync.backoff_min.max(Duration::from_secs(1));
                let retry_at = unix_ms() + retry.as_millis() as i64;
                let _ = self
                    .shared
                    .store
                    .sent_copy_failed(copy.draft, retry_at)
                    .recv()
                    .await;
                self.submission.retry_by(Instant::now() + retry);
            }
        }
    }

    /// Files the copies of the drafts that are due, and sets the timer for the next.
    async fn file_drafts(&mut self) {
        self.drafts_now = false;
        let now = unix_ms();
        let due = match self.shared.store.due_drafts(self.account, now).recv().await {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "due drafts couldn't be read");
                self.next_drafts = Some(Instant::now() + self.shared.sync.backoff_max);
                return;
            }
        };
        self.next_drafts = due.next.map(|at| instant_at(at, now));
        for upload in due.uploads {
            self.file_draft(upload).await;
        }
    }

    /// One draft's copy, `\Draft \Seen` in the local Drafts folder; the copy it replaces goes
    /// when the store records it.
    async fn file_draft(&mut self, upload: DraftUpload) {
        let blobs = self.shared.blobs.clone();
        let draft = upload.message.clone();
        let folder = upload.folder.folder;
        let date = unix_seconds();
        let built = tokio::task::spawn_blocking(move || {
            let raw = build_drafts_copy(&blobs, &draft, date)?;
            let message = RawMessage {
                folder,
                uid: None,
                flags: Flags::DRAFT.union(Flags::SEEN),
                keywords: Vec::new(),
                internal_date: None,
                raw,
            };
            ingest::prepare(&blobs, vec![message], 1).map_err(|_| Unbuilt::Blobs)
        })
        .await
        .unwrap_or(Err(Unbuilt::Blobs));
        let message = match built {
            Ok(prepared) => prepared.messages.into_iter().next(),
            Err(Unbuilt::Draft(error)) => {
                tracing::info!(
                    account = self.account.0,
                    ?error,
                    "a draft's copy couldn't be built"
                );
                self.draft_failed(&upload, Failure::Permanent).await;
                return;
            }
            Err(Unbuilt::Blobs) => None,
        };
        let Some((message, blob)) = message.and_then(|message| {
            let blob = message.blob?;
            Some((message, blob))
        }) else {
            tracing::warn!(account = self.account.0, "a draft's copy couldn't be kept");
            self.draft_failed(&upload, self.later()).await;
            return;
        };
        if let Err(error) = self.shared.store.ingest(vec![message]).recv().await {
            tracing::warn!(account = self.account.0, %error, "a draft's copy couldn't be filed");
            self.draft_failed(&upload, self.later()).await;
            return;
        }
        match self
            .shared
            .store
            .draft_filed(self.account, upload.draft, upload.revision, blob)
            .recv()
            .await
        {
            Ok(filed) => discard(&self.shared, filed.orphaned_blobs).await,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "a draft's copy couldn't be recorded");
                self.draft_failed(&upload, self.later()).await;
            }
        }
    }

    /// A transient failure, tried again after the longest backoff.
    fn later(&self) -> Failure {
        let wait = self.shared.sync.backoff_max;
        Failure::Transient {
            retry_at: Some(unix_ms() + wait.as_millis() as i64),
        }
    }

    async fn draft_failed(&mut self, upload: &DraftUpload, failure: Failure) {
        if let Err(error) = self
            .shared
            .store
            .draft_upload_failed(upload.draft, failure)
            .recv()
            .await
        {
            tracing::warn!(account = self.account.0, %error, "a draft's failed copy couldn't be recorded");
        }
        if let Failure::Transient { .. } = failure {
            let retry = Instant::now() + self.shared.sync.backoff_max;
            self.next_drafts = Some(self.next_drafts.map_or(retry, |next| next.min(retry)));
        }
    }

    async fn copy_done(&mut self, copy: &DueSentCopy, done: SentCopyDone) {
        tracing::debug!(account = self.account.0, ?done, "Sent copy done");
        match self
            .shared
            .store
            .sent_copy_done(copy.draft, done)
            .recv()
            .await
        {
            Ok(recorded) => discard(&self.shared, recorded.orphaned_blobs).await,
            Err(error) => {
                tracing::warn!(account = self.account.0, %error, "a Sent copy couldn't be recorded");
            }
        }
    }
}
