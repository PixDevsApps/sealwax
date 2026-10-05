//! Body downloads for the app (ML-044): `Command::FetchBodies` answered with one
//! `Event::BodyFetched` per message. A message already queued or in flight isn't asked for twice;
//! each account has one worker that logs in with the account's stored secret, takes the most
//! wanted messages first, a folder at a time, and logs out once its queue is empty. The disk cap
//! is enforced when `Command::Storage` arrives and after every batch. Phase B moves the fetches
//! onto the account actor's connection.

use super::{Shared, log_in_with_password, log_in_with_token};
use crate::api::{BodyError, Event, SyncStatus};
use crate::bodies::enforce_cap;
use crate::imap::bodies::{Sink, fetch_bodies};
use crate::imap::{self, Connection};
use sealwax_domain::account::{AccountSettings, AuthMethod, LoginFailure};
use sealwax_domain::config::Storage;
use sealwax_domain::model::{AccountId, FolderId, MessageId};
use sealwax_infrastructure::accounts as files;
use sealwax_infrastructure::store::{BodyState, BodyTarget};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

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
    if !running {
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

/// The next batch of `account`. `None` ends the worker, under the same lock that `enqueue`
/// takes, so a message queued meanwhile starts a new one.
fn next_batch(shared: &Shared, account: AccountId) -> Option<(FolderId, Vec<BodyTarget>)> {
    let mut state = shared.bodies.state();
    let batch = take_batch(state.queues.get_mut(&account)?);
    if batch.is_none() {
        state.queues.remove(&account);
        state.workers.remove(&account);
    }
    batch
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

/// The account's settings from `accounts.toml`, through its key in the store; `None` for a
/// local account or one the file no longer lists.
async fn settings(shared: &Shared, account: AccountId) -> Option<AccountSettings> {
    let accounts = shared.store.accounts().recv().await.ok()?.value;
    let key = accounts
        .into_iter()
        .find(|known| known.id == account)?
        .key?;
    let path = shared.paths.settings.clone();
    let listed = tokio::task::spawn_blocking(move || files::read(&path))
        .await
        .ok()?
        .ok()?;
    listed.into_iter().find(|settings| settings.key == key)
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

/// One account's downloads, until its queue is empty.
async fn worker(shared: Arc<Shared>, account: AccountId) {
    let Some(settings) = settings(&shared, account).await else {
        tracing::info!(
            account = account.0,
            "bodies asked for an account with no server"
        );
        fail_all(&shared, account, BodyError::Other);
        return;
    };
    let mut connection: Option<Connection> = None;
    while let Some((folder, batch)) = next_batch(&shared, account) {
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
            let fetched = fetch_bodies(conn, sink(&shared), &sync, &left, now(), |id, result| {
                answered.push(id);
                answer(&shared, id, result);
            })
            .await;
            left.retain(|target| !answered.contains(&target.message));
            let Err(error) = fetched else {
                continue;
            };
            let connection_lost = matches!(&error, imap::bodies::BodyFetchError::Imap(failure)
                if failure.is_connection());
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
