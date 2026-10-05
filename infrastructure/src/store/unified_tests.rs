//! Several accounts and All Inboxes (ML-047): the unified inbox merges every account's inbox by
//! date under each filter chip, each account's folders keep only its own mail, a detached
//! account's mail leaves it, and accounts that sync at once keep the open views exact.

use super::tests::{Model, scratch};
use super::*;
use sealwax_domain::account::AccountKey;
use sealwax_domain::model::{Address, FolderRole, Headers};
use std::collections::{BTreeMap, BTreeSet};

const FILTERS: [ViewFilter; 4] = [
    ViewFilter::All,
    ViewFilter::Unread,
    ViewFilter::Starred,
    ViewFilter::Attachments,
];
const NAMES: [&str; 3] = ["Work", "Home", "Club"];

struct Mailbox {
    id: AccountId,
    inbox: FolderId,
    archive: FolderId,
    sent: FolderId,
}

fn new_account(name: &str) -> NewAccount {
    NewAccount {
        name: name.to_owned(),
        address: format!("{}@example.test", name.to_lowercase()),
    }
}

fn folders(store: &Store, id: AccountId) -> Mailbox {
    let folder = |name: &str, role: FolderRole| {
        store
            .upsert_folder(FolderSpec::local(id, name, role))
            .recv_blocking()
            .unwrap()
            .value
    };
    Mailbox {
        id,
        inbox: folder("INBOX", FolderRole::Inbox),
        archive: folder("Archive", FolderRole::Archive),
        sent: folder("Sent", FolderRole::Sent),
    }
}

/// Three keyed accounts, as `accounts.toml` would list them.
fn three(store: &Store) -> Vec<Mailbox> {
    NAMES
        .iter()
        .map(|name| {
            let id = store
                .ensure_account(AccountKey(name.to_lowercase()), new_account(name))
                .recv_blocking()
                .unwrap()
                .value;
            folders(store, id)
        })
        .collect()
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// A message with random flags and attachments, dated in a narrow range so dates tie across
/// accounts.
fn message(rng: &mut Rng, folder: FolderId, uid: u32) -> NewMessage {
    let key = format!("f{}-u{uid}", folder.0);
    let mut flags = if rng.below(10) < 3 {
        Flags::NONE
    } else {
        Flags::SEEN
    };
    if rng.below(8) == 0 {
        flags = flags | Flags::FLAGGED;
    }
    NewMessage {
        folder,
        uid: Some(uid),
        flags,
        date: 50_000 + rng.below(400) as i64,
        headers: Headers {
            from: vec![Address::new("Kari", "kari@example.test")],
            subject: format!("Subject {key}"),
            message_id: Some(format!("{key}@example.test")),
            ..Headers::default()
        },
        has_attachments: rng.below(5) == 0,
        snippet: format!("snippet {key}"),
        search_text: format!("text {key}"),
        size: 100,
        ..NewMessage::default()
    }
}

fn ingest(store: &Store, messages: &[NewMessage]) -> Vec<MessageId> {
    store
        .ingest(messages.to_vec())
        .recv_blocking()
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

/// The stored messages a view lists, newest first by date, then id: `(date, id)`.
fn expected(
    stored: &BTreeMap<MessageId, NewMessage>,
    folders: &[FolderId],
    filter: ViewFilter,
) -> Vec<(i64, MessageId)> {
    let mut rows: Vec<(i64, MessageId)> = stored
        .iter()
        .filter(|(_, m)| {
            folders.contains(&m.folder) && views::passes(filter, m.flags.bits(), m.has_attachments)
        })
        .map(|(id, m)| (m.date, *id))
        .collect();
    rows.sort_by(|a, b| b.cmp(a));
    rows
}

fn ids_of(rows: &[(i64, MessageId)]) -> Vec<MessageId> {
    rows.iter().map(|row| row.1).collect()
}

fn listed(store: &Store, spec: ViewSpec, offset: u32, limit: u32) -> Vec<MessageId> {
    store
        .page(spec, offset, limit)
        .recv_blocking()
        .unwrap()
        .rows
        .iter()
        .map(|row| row.id)
        .collect()
}

fn open_model(store: &Store, spec: ViewSpec) -> Model {
    let opened = store
        .open_view(OpenView {
            spec,
            first_page: u32::MAX,
            section_starts: Vec::new(),
        })
        .recv_blocking()
        .unwrap();
    Model {
        view: opened.view,
        rows: opened.rows,
        version: opened.version,
    }
}

/// Applies every event up to `version` to the models.
fn catch_up(
    store: &Store,
    events: &async_channel::Receiver<Arc<ChangeEvent>>,
    models: &mut [Model],
    version: StoreVersion,
) -> Vec<Arc<ChangeEvent>> {
    let mut seen = Vec::new();
    while models.iter().any(|model| model.version < version) {
        let event = events.recv_blocking().unwrap();
        for model in models.iter_mut() {
            model.apply(store, &event);
        }
        seen.push(event);
    }
    seen
}

#[test]
fn all_inboxes_merges_three_accounts_by_date_under_every_filter() {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let accounts = three(&store);
    let mut rng = Rng(47);
    let mut stored = BTreeMap::new();
    // Batches interleaved across accounts, as three first syncs would arrive.
    for round in 0..5 {
        for account in &accounts {
            let batch: Vec<NewMessage> = (0..40)
                .map(|n| {
                    let folder = match rng.below(10) {
                        0..=5 => account.inbox,
                        6..=7 => account.archive,
                        _ => account.sent,
                    };
                    message(&mut rng, folder, round * 100 + n + 1)
                })
                .collect();
            stored.extend(ingest(&store, &batch).into_iter().zip(batch));
        }
    }
    let inboxes: Vec<FolderId> = accounts.iter().map(|account| account.inbox).collect();
    let starts = [50_300, 50_100];
    for filter in FILTERS {
        let spec = ViewSpec::unified_inbox().with_filter(filter);
        let want = expected(&stored, &inboxes, filter);
        let total = want.len() as u32;
        assert!(total > 40, "{filter:?}: {total}");
        let opened = store
            .open_view(OpenView {
                spec,
                first_page: 25,
                section_starts: starts.to_vec(),
            })
            .recv_blocking()
            .unwrap();
        assert_eq!(opened.total, total, "{filter:?}");
        assert_eq!(
            opened.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            ids_of(&want[..25]),
            "{filter:?}"
        );
        let since = |start: i64| want.iter().filter(|row| row.0 >= start).count() as u32;
        assert_eq!(opened.section_counts, [since(starts[0]), since(starts[1])]);
        assert_eq!(
            listed(&store, spec, 0, u32::MAX),
            ids_of(&want),
            "{filter:?}"
        );
        let half = total / 2;
        for offset in [1, 24, half - 1, half, half + 1, total - 30, total - 1] {
            let end = (offset as usize + 17).min(want.len());
            assert_eq!(
                listed(&store, spec, offset, 17),
                ids_of(&want[offset as usize..end]),
                "{filter:?} at {offset}"
            );
        }
        let accounts_listed: BTreeSet<AccountId> = opened
            .rows
            .iter()
            .map(|row| {
                accounts
                    .iter()
                    .find(|account| account.inbox == row.folder)
                    .unwrap()
                    .id
            })
            .collect();
        assert_eq!(accounts_listed.len(), 3, "{filter:?}: every account shows");
    }
    // Each account's folders list only its own mail, under every filter.
    for account in &accounts {
        for folder in [account.inbox, account.archive, account.sent] {
            for filter in FILTERS {
                let spec = ViewSpec::folder(folder).with_filter(filter);
                assert_eq!(
                    listed(&store, spec, 0, u32::MAX),
                    ids_of(&expected(&stored, &[folder], filter)),
                    "{folder:?} {filter:?}"
                );
            }
        }
    }
    let counts = store.folders().recv_blocking().unwrap().value;
    for folder in &counts {
        let mine: Vec<&NewMessage> = stored.values().filter(|m| m.folder == folder.id).collect();
        let unread = mine.iter().filter(|m| !m.flags.is_seen()).count() as u32;
        assert_eq!((folder.total, folder.unread), (mine.len() as u32, unread));
    }
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

/// Asserts that the models of the unified views, one per filter in `FILTERS` order, list
/// exactly the stored messages of `inboxes`.
fn unified_models_list(
    models: &[Model],
    stored: &BTreeMap<MessageId, NewMessage>,
    inboxes: &[FolderId],
) {
    for (model, filter) in models.iter().zip(FILTERS) {
        let rows: Vec<MessageId> = model.rows.iter().map(|row| row.id).collect();
        assert_eq!(
            rows,
            ids_of(&expected(stored, inboxes, filter)),
            "{filter:?}"
        );
    }
}

#[test]
fn a_detached_account_leaves_all_inboxes_and_comes_back() {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let accounts = three(&store);
    let mut rng = Rng(9);
    let mut stored = BTreeMap::new();
    for account in &accounts {
        let batch: Vec<NewMessage> = (1..=30)
            .map(|uid| message(&mut rng, account.inbox, uid))
            .collect();
        stored.extend(ingest(&store, &batch).into_iter().zip(batch));
    }
    let events = store.subscribe();
    let mut models: Vec<Model> = FILTERS
        .iter()
        .map(|filter| open_model(&store, ViewSpec::unified_inbox().with_filter(*filter)))
        .collect();
    let keyed = |names: &[&str]| -> Vec<(AccountKey, NewAccount)> {
        names
            .iter()
            .map(|name| (AccountKey(name.to_lowercase()), new_account(name)))
            .collect()
    };
    let inboxes = |names: &[&str]| -> Vec<FolderId> {
        NAMES
            .iter()
            .zip(&accounts)
            .filter(|(name, _)| names.contains(name))
            .map(|(_, account)| account.inbox)
            .collect()
    };
    // Home's entry goes from accounts.toml: its mail leaves every unified view at once.
    let detached = store
        .reconcile_accounts(keyed(&["Work", "Club"]))
        .recv_blocking()
        .unwrap();
    let seen = catch_up(&store, &events, &mut models, detached.version);
    let reloads = seen
        .iter()
        .flat_map(|event| &event.views)
        .filter(|change| matches!(change.diff, ViewDiff::Reload { .. }))
        .count();
    assert_eq!(reloads, FILTERS.len());
    unified_models_list(&models, &stored, &inboxes(&["Work", "Club"]));

    // Mail that still reaches the detached account stays out of All Inboxes; its own folder
    // keeps it.
    let late = message(&mut rng, accounts[1].inbox, 99);
    let id = ingest(&store, std::slice::from_ref(&late))[0];
    stored.insert(id, late);
    let version = store.accounts().recv_blocking().unwrap().version;
    let seen = catch_up(&store, &events, &mut models, version);
    assert!(seen.iter().all(|event| event.views.is_empty()));
    unified_models_list(&models, &stored, &inboxes(&["Work", "Club"]));
    assert_eq!(
        listed(&store, ViewSpec::folder(accounts[1].inbox), 0, u32::MAX).len(),
        31
    );

    // The entry comes back with the same key: the same account and its mail.
    let back = store
        .reconcile_accounts(keyed(&NAMES))
        .recv_blocking()
        .unwrap();
    assert_eq!(back.value[1], accounts[1].id);
    catch_up(&store, &events, &mut models, back.version);
    unified_models_list(&models, &stored, &inboxes(&NAMES));
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn accounts_syncing_at_once_keep_all_inboxes_exact() {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let accounts = three(&store);
    let events = store.subscribe();
    let mut specs: Vec<ViewSpec> = FILTERS
        .iter()
        .map(|filter| ViewSpec::unified_inbox().with_filter(*filter))
        .collect();
    specs.extend(
        accounts
            .iter()
            .map(|account| ViewSpec::folder(account.inbox)),
    );
    let mut models: Vec<Model> = specs.iter().map(|spec| open_model(&store, *spec)).collect();

    // Each account's sync on its own thread: header batches, flag changes and expunges, with
    // replies threading under earlier messages of the same account. Every commit touches at
    // most 25 rows, so the views get splices, never reloads.
    std::thread::scope(|scope| {
        for (n, account) in accounts.iter().enumerate() {
            let store = &store;
            scope.spawn(move || {
                let mut rng = Rng(100 + n as u64);
                let mut live: Vec<MessageId> = Vec::new();
                let mut uid = 0;
                for round in 0..12 {
                    let batch: Vec<NewMessage> = (0..1 + rng.below(25))
                        .map(|_| {
                            uid += 1;
                            let folder = if rng.below(4) == 0 {
                                account.archive
                            } else {
                                account.inbox
                            };
                            let mut m = message(&mut rng, folder, uid);
                            if uid > 3 && rng.below(3) == 0 {
                                let parent = 1 + rng.below(u64::from(uid) - 1);
                                m.headers.in_reply_to =
                                    vec![format!("f{}-u{parent}@example.test", account.inbox.0)];
                            }
                            m
                        })
                        .collect();
                    live.extend(ingest(store, &batch));
                    let picks: Vec<MessageId> = (0..1 + rng.below(10))
                        .map(|_| live[rng.below(live.len() as u64) as usize])
                        .collect();
                    let (add, remove) = match rng.below(3) {
                        0 => (Flags::SEEN, Flags::NONE),
                        1 => (Flags::FLAGGED, Flags::SEEN),
                        _ => (Flags::NONE, Flags::FLAGGED),
                    };
                    store.set_flags(picks, add, remove).recv_blocking().unwrap();
                    if round % 3 == 2 {
                        let gone: Vec<MessageId> = (0..(1 + rng.below(5)).min(live.len() as u64))
                            .map(|_| live.swap_remove(rng.below(live.len() as u64) as usize))
                            .collect();
                        store.remove(gone).recv_blocking().unwrap();
                    }
                }
            });
        }
    });
    let mut versions = Vec::new();
    while let Ok(event) = events.try_recv() {
        assert!(
            event
                .views
                .iter()
                .all(|change| matches!(change.diff, ViewDiff::Splice { .. })),
            "no reloads"
        );
        for model in &mut models {
            model.apply(&store, &event);
        }
        versions.push(event.version);
    }
    assert!(versions.len() >= 36, "{} commits", versions.len());
    for model in &models {
        let page = store
            .page(model.view.spec(), 0, u32::MAX)
            .recv_blocking()
            .unwrap();
        assert_eq!(page.version, model.version);
        assert_eq!(page.rows, model.rows, "{:?}", model.view.spec());
    }
    let unified = &models[0].rows;
    assert!(
        unified
            .windows(2)
            .all(|pair| (pair[0].date, pair[0].id) > (pair[1].date, pair[1].id))
    );
    let inboxes: BTreeSet<FolderId> = unified.iter().map(|row| row.folder).collect();
    assert_eq!(inboxes.len(), 3);
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}
