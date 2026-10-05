//! Body downloads for the app (ML-044): `Command::FetchBodies` answered with one
//! `Event::BodyFetched` per message. A message already queued or in flight isn't asked for twice;
//! each account has one worker with a connection of its own beside the account's sync actor
//! (whose settings it uses and whose reconnects it follows), taking the most wanted messages
//! first, a folder at a time. Its connection stays open `body_linger` after the queue empties,
//! and gets a NOOP before it is used again after sitting idle. The disk cap is enforced when
//! `Command::Storage` arrives and after every batch; after a pass that stored new mail the
//! actor queues the background prefetch behind the reader's requests.

use super::actor::Signal;
use super::{Shared, log_in_with_password, log_in_with_token};
use crate::api::{BodyError, Event, SyncStatus};
use crate::bodies::{enforce_cap, prefetch_ids};
use crate::imap::bodies::{BodyFetchError, Sink, fetch_bodies};
use crate::imap::{Connection, ImapError};
use sealwax_domain::account::{AccountSettings, AuthMethod, LoginFailure};
use sealwax_domain::config::Storage;
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_infrastructure::store::{BodyState, BodyTarget};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, watch};

/// Messages of one folder fetched per batch.
const BATCH: usize = 20;

#[derive(Default)]
struct State {
    /// Every message asked for and not answered yet, and whether someone waits for its
    /// `BodyFetched` (the background prefetch doesn't).
    asked: HashMap<MessageId, bool>,
    /// Per account, the messages waiting, most wanted first.
    queues: HashMap<AccountId, VecDeque<BodyTarget>>,
    workers: HashMap<AccountId, tokio::task::AbortHandle>,
    /// Wakes a worker that waits for more work with its connection open.
    wakers: HashMap<AccountId, Arc<Notify>>,
    storage: Option<Storage>,
}

#[derive(Default)]
pub(super) struct Bodies {
    state: Mutex<State>,
    /// Blob writes with their upgrades, and evictions with their deletions, one at a time.
    writes: tokio::sync::Mutex<()>,
}

impl Bodies {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stops every worker; their connections close with them.
    pub(super) fn stop(&self) {
        for (_, worker) in self.state().workers.drain() {
            worker.abort();
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn sink(shared: &Shared) -> Sink<'_> {
    Sink {
        store: &shared.store,
        blobs: &shared.blobs,
        writes: &shared.bodies.writes,
    }
}

/// Answers `message` once: its `BodyFetched` when someone waits for it.
fn answer(shared: &Shared, message: MessageId, result: Result<(), BodyError>) {
    let announce = shared.bodies.state().asked.remove(&message);
    if announce == Some(true) {
        shared.send(Event::BodyFetched { message, result });
    }
}

/// Queues the bodies of `messages`, the first most wanted, ahead of everything queued before.
/// `announce` is false for the background prefetch, whose messages answer nothing and queue
/// behind everything else; asking for one of them again announces it and moves it to the front.
pub(super) fn request(shared: &Arc<Shared>, messages: Vec<MessageId>, announce: bool) {
    let fresh: Vec<MessageId> = {
        let mut state = shared.bodies.state();
        let mut fresh = Vec::new();
        for message in messages {
            match state.asked.get_mut(&message) {
                Some(announced) => {
                    *announced |= announce;
                    if announce {
                        promote(&mut state, message);
                    }
                }
                None => {
                    state.asked.insert(message, announce);
                    fresh.push(message);
                }
            }
        }
        fresh
    };
    if fresh.is_empty() {
        return;
    }
    let shared = shared.clone();
    tokio::spawn(async move {
        let targets = match shared.store.body_targets(fresh.clone()).recv().await {
            Ok(targets) => targets,
            Err(error) => {
                tracing::warn!(%error, "the messages to download couldn't be read");
                for message in fresh {
                    answer(&shared, message, Err(BodyError::Store));
                }
                return;
            }
        };
        for message in fresh
            .iter()
            .filter(|message| targets.iter().all(|target| target.message != **message))
        {
            answer(&shared, *message, Err(BodyError::Gone));
        }
        let mut queued: HashMap<AccountId, Vec<BodyTarget>> = HashMap::new();
        for target in targets {
            let stored = matches!(target.body_state, BodyState::Full | BodyState::Text)
                && target.blob.is_some_and(|key| shared.blobs.contains(&key));
            if stored {
                answer(&shared, target.message, Ok(()));
            } else {
                queued.entry(target.account).or_default().push(target);
            }
        }
        for (account, targets) in queued {
            enqueue(&shared, account, targets, announce);
        }
    });
}

/// Moves a queued message to the front of its account's queue.
fn promote(state: &mut State, message: MessageId) {
    for queue in state.queues.values_mut() {
        if let Some(index) = queue.iter().position(|target| target.message == message) {
            if let Some(target) = queue.remove(index) {
                queue.push_front(target);
            }
            return;
        }
    }
}

/// Adds `targets` in their order: at the front when someone waits for them, else at the back.
fn queue_in(queue: &mut VecDeque<BodyTarget>, targets: Vec<BodyTarget>, front: bool) {
    if front {
        for target in targets.into_iter().rev() {
            queue.push_front(target);
        }
    } else {
        queue.extend(targets);
    }
}

fn enqueue(shared: &Arc<Shared>, account: AccountId, targets: Vec<BodyTarget>, front: bool) {
    let mut state = shared.bodies.state();
    queue_in(state.queues.entry(account).or_default(), targets, front);
    let running = state
        .workers
        .get(&account)
        .is_some_and(|worker| !worker.is_finished());
    if running {
        state.wakers.entry(account).or_default().notify_one();
    } else {
        let task = tokio::spawn(worker(shared.clone(), account));
        state.workers.insert(account, task.abort_handle());
    }
}

/// The most wanted message and up to [`BATCH`] - 1 more of its folder, in queue order.
fn take_batch(queue: &mut VecDeque<BodyTarget>) -> Option<(FolderId, Vec<BodyTarget>)> {
    let folder = queue.front()?.folder;
    let mut batch = Vec::new();
    let mut index = 0;
    while index < queue.len() && batch.len() < BATCH {
        if queue[index].folder == folder {
            batch.extend(queue.remove(index));
        } else {
            index += 1;
        }
    }
    Some((folder, batch))
}

/// The next batch of `account`, if any.
fn next_batch(shared: &Shared, account: AccountId) -> Option<(FolderId, Vec<BodyTarget>)> {
    take_batch(shared.bodies.state().queues.get_mut(&account)?)
}

/// Ends the worker if its queue is still empty, under the same lock that `enqueue` takes, so a
/// message queued meanwhile either finds it waiting or starts a new one.
fn retire(shared: &Shared, account: AccountId) -> bool {
    let mut state = shared.bodies.state();
    if state
        .queues
        .get(&account)
        .is_some_and(|queue| !queue.is_empty())
    {
        return false;
    }
    state.queues.remove(&account);
    state.workers.remove(&account);
    state.wakers.remove(&account);
    true
}

/// The account's waker, for a worker waiting with its connection open.
fn waker(shared: &Shared, account: AccountId) -> Arc<Notify> {
    shared
        .bodies
        .state()
        .wakers
        .entry(account)
        .or_default()
        .clone()
}

/// Resolves once the actor asks for a reconnect after `seen`, or goes away.
async fn reconnected(mut signals: watch::Receiver<Signal>, seen: u64) {
    loop {
        let signal = *signals.borrow_and_update();
        if signal.reconnects != seen || signal.stop.is_some() {
            return;
        }
        if signals.changed().await.is_err() {
            return;
        }
    }
}

/// Answers everything `account` has queued with `error`, and ends its worker.
fn fail_all(shared: &Shared, account: AccountId, error: BodyError) {
    let waiting: Vec<MessageId> = {
        let mut state = shared.bodies.state();
        state.workers.remove(&account);
        state
            .queues
            .remove(&account)
            .map(|queue| queue.into_iter().map(|target| target.message).collect())
            .unwrap_or_default()
    };
    for message in waiting {
        answer(shared, message, Err(error));
    }
}

/// What a refused or failed login tells the reader.
fn login_error(failure: LoginFailure) -> BodyError {
    match SyncStatus::from_failure(failure) {
        SyncStatus::NeedsSignIn(_) => BodyError::NeedsSignIn,
        SyncStatus::Offline(LoginFailure::Other) => BodyError::Other,
        _ => BodyError::Offline,
    }
}

async fn log_in(shared: &Shared, settings: &AccountSettings) -> Result<Connection, BodyError> {
    let connection = match settings.auth {
        AuthMethod::Password => log_in_with_password(shared, settings).await,
        AuthMethod::OAuth(_) => log_in_with_token(shared, settings).await,
    };
    connection.map_err(|failure| {
        tracing::info!(?failure, "the body download's login failed");
        login_error(failure)
    })
}

/// One account's downloads, until its queue has been empty for `body_linger`.
async fn worker(shared: Arc<Shared>, account: AccountId) {
    let Some(settings) = shared.actor_settings(account) else {
        tracing::info!(
            account = account.0,
            "bodies asked for an account with no server"
        );
        fail_all(&shared, account, BodyError::Other);
        return;
    };
    let signals = shared.actor_signal(account);
    let mut seen = signals
        .as_ref()
        .map_or(0, |signals| signals.borrow().reconnects);
    let mut connection: Option<Connection> = None;
    let mut used = Instant::now();
    loop {
        let Some((folder, batch)) = next_batch(&shared, account) else {
            let woken =
                tokio::time::timeout(shared.sync.body_linger, waker(&shared, account).notified())
                    .await;
            if woken.is_err() && retire(&shared, account) {
                break;
            }
            continue;
        };
        if let Some(signals) = &signals {
            let now = signals.borrow().reconnects;
            if now != seen {
                connection = None;
                seen = now;
            }
        }
        if let Some(conn) = connection.as_mut()
            && used.elapsed() >= shared.sync.reuse_check
        {
            let alive = tokio::time::timeout(shared.sync.noop_timeout, conn.noop()).await;
            if !matches!(alive, Ok(Ok(_))) {
                connection = None;
            }
        }
        let Some(sync) = shared.store.folder_sync(folder).recv().await.ok().flatten() else {
            for target in &batch {
                answer(&shared, target.message, Err(BodyError::Gone));
            }
            continue;
        };
        let fetched_now: Vec<MessageId> = batch.iter().map(|target| target.message).collect();
        let mut left = batch;
        let mut attempts = 0;
        while !left.is_empty() {
            attempts += 1;
            let conn = match connection.as_mut() {
                Some(conn) => conn,
                None => match log_in(&shared, &settings).await {
                    Ok(conn) => connection.insert(conn),
                    Err(error) => {
                        for target in &left {
                            answer(&shared, target.message, Err(error));
                        }
                        fail_all(&shared, account, error);
                        return;
                    }
                },
            };
            let mut answered = Vec::new();
            let fetch = fetch_bodies(conn, sink(&shared), &sync, &left, now(), |id, result| {
                answered.push(id);
                answer(&shared, id, result);
            });
            // A reconnect (resume, network back) abandons a fetch on a connection that may be
            // dead; the batch is tried once more on a new one.
            let fetched = match &signals {
                Some(signals) => tokio::select! {
                    fetched = fetch => fetched,
                    () = reconnected(signals.clone(), seen) => {
                        seen = signals.borrow().reconnects;
                        Err(BodyFetchError::Imap(ImapError::ConnectionLost))
                    }
                },
                None => fetch.await,
            };
            used = Instant::now();
            left.retain(|target| !answered.contains(&target.message));
            let Err(error) = fetched else {
                continue;
            };
            let connection_lost =
                matches!(&error, BodyFetchError::Imap(failure) if failure.is_connection());
            if connection_lost {
                connection = None;
            }
            if !connection_lost || attempts >= 2 {
                tracing::info!(%error, "bodies couldn't be downloaded");
                for target in left.drain(..) {
                    answer(&shared, target.message, Err(error.body_error()));
                }
            }
        }
        cap(&shared, &fetched_now).await;
    }
    if let Some(connection) = connection {
        connection.logout().await;
    }
}

/// Enforces the disk cap once the settings are known, sparing `keep`.
async fn cap(shared: &Shared, keep: &[MessageId]) {
    let limit = shared
        .bodies
        .state()
        .storage
        .and_then(|storage| storage.cache_size);
    if let Some(limit) = limit
        && let Err(error) = enforce_cap(sink(shared), limit, keep).await
    {
        tracing::warn!(%error, "the disk cap couldn't be enforced");
    }
}

/// Queues the background prefetch (recent Inbox mail) behind the reader's requests, once the
/// `[storage]` settings are known.
pub(super) fn prefetch(shared: &Arc<Shared>) {
    let Some(storage) = shared.bodies.state().storage else {
        return;
    };
    let shared = shared.clone();
    tokio::spawn(async move {
        match prefetch_ids(&shared.store, &storage, now()).await {
            Ok(ids) if !ids.is_empty() => request(&shared, ids, false),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "the prefetch couldn't be planned"),
        }
    });
}

/// New `[storage]` settings: the cap is enforced at once.
pub(super) fn set_storage(shared: &Arc<Shared>, storage: Storage) {
    let changed = shared.bodies.state().storage.replace(storage) != Some(storage);
    if changed {
        let shared = shared.clone();
        tokio::spawn(async move { cap(&shared, &[]).await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(message: i64, folder: i64) -> BodyTarget {
        BodyTarget {
            message: MessageId(message),
            account: AccountId(1),
            folder: FolderId(folder),
            uid: message as u32,
            size: 100,
            body_state: BodyState::Partial,
            blob: None,
        }
    }

    fn messages(queue: &VecDeque<BodyTarget>) -> Vec<i64> {
        queue.iter().map(|target| target.message.0).collect()
    }

    #[test]
    fn the_reader_goes_first_and_the_prefetch_last() {
        let mut queue = VecDeque::new();
        queue_in(&mut queue, vec![target(1, 1), target(2, 1)], false);
        queue_in(&mut queue, vec![target(3, 1), target(4, 1)], true);
        queue_in(&mut queue, vec![target(5, 1)], false);
        queue_in(&mut queue, vec![target(6, 1)], true);
        assert_eq!(messages(&queue), [6, 3, 4, 1, 2, 5]);
        let mut state = State::default();
        state.queues.insert(AccountId(1), queue);
        promote(&mut state, MessageId(5));
        assert_eq!(messages(&state.queues[&AccountId(1)]), [5, 6, 3, 4, 1, 2]);
    }

    #[test]
    fn a_batch_is_the_first_message_and_more_of_its_folder() {
        let mut queue: VecDeque<BodyTarget> = [(1, 7), (2, 8), (3, 7)]
            .into_iter()
            .map(|(message, folder)| target(message, folder))
            .chain((10..40).map(|message| target(message, 7)))
            .collect();
        let (folder, batch) = take_batch(&mut queue).unwrap();
        assert_eq!(folder, FolderId(7));
        assert_eq!(batch.len(), BATCH);
        assert_eq!(
            batch
                .iter()
                .take(3)
                .map(|t| t.message.0)
                .collect::<Vec<_>>(),
            [1, 3, 10]
        );
        assert_eq!(
            queue.front().map(|t| t.message.0),
            Some(2),
            "the other folder waits"
        );
        let (folder, batch) = take_batch(&mut queue).unwrap();
        assert_eq!((folder, batch.len()), (FolderId(8), 1));
        assert_eq!(take_batch(&mut queue).unwrap().1.len(), 30 + 2 - BATCH);
        assert!(take_batch(&mut queue).is_none());
    }
}
