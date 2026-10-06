//! Keyed accounts, detaching, the v1 → v2 migration and account removal (ML-034, ML-038).

use super::tests::scratch;
use super::*;
use crate::ingest::{RawMessage, ingest_raw};
use rusqlite::Connection;
use sealwax_domain::model::FolderRole;
use std::collections::BTreeSet;

fn key(text: &str) -> AccountKey {
    AccountKey(text.to_owned())
}

fn new_account(name: &str) -> NewAccount {
    NewAccount {
        name: name.to_owned(),
        address: format!("{}@example.test", name.to_lowercase()),
    }
}

fn open(dir: &Path) -> (Store, BlobStore) {
    let blobs = BlobStore::open(&dir.join("blobs")).unwrap();
    let store = Store::open(
        &dir.join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    (store, blobs)
}

fn folder(store: &Store, account: AccountId, name: &str, role: FolderRole) -> FolderId {
    store
        .upsert_folder(FolderSpec::local(account, name, role))
        .recv_blocking()
        .unwrap()
        .value
}

/// A message with `word` in its body, threaded under `parent` when given.
fn raw(n: u32, word: &str, parent: Option<u32>) -> Vec<u8> {
    let mut text = format!(
        "From: Sender {n} <sender{n}@example.test>\r\nTo: someone@example.test\r\n\
         Subject: Message {n}\r\nDate: Mon, 05 Oct 2026 09:{:02}:00 +0000\r\n\
         Message-ID: <m{n}@example.test>\r\n",
        n % 60
    );
    if let Some(parent) = parent {
        text.push_str(&format!(
            "In-Reply-To: <m{parent}@example.test>\r\nReferences: <m{parent}@example.test>\r\n"
        ));
    }
    text.push_str(&format!(
        "Content-Type: text/plain; charset=utf-8\r\n\r\nBody {n} with {word}.\r\n"
    ));
    text.into_bytes()
}

fn ingest(store: &Store, blobs: &BlobStore, folder: FolderId, raws: Vec<Vec<u8>>) {
    let messages = raws
        .into_iter()
        .map(|raw| RawMessage {
            folder,
            raw,
            ..RawMessage::default()
        })
        .collect();
    ingest_raw(store, blobs, messages).unwrap();
}

fn names(store: &Store) -> Vec<(String, Option<String>)> {
    store
        .accounts()
        .recv_blocking()
        .unwrap()
        .value
        .into_iter()
        .map(|account| (account.name, account.key.map(|key| key.0)))
        .collect()
}

fn unified_total(store: &Store) -> u32 {
    store
        .open_view(OpenView {
            spec: ViewSpec::unified_inbox(),
            first_page: 0,
            section_starts: Vec::new(),
        })
        .recv_blocking()
        .unwrap()
        .total
}

fn search(store: &Store, word: &str) -> usize {
    store
        .search(word, None, 1_000)
        .recv_blocking()
        .unwrap()
        .value
        .len()
}

/// Events until one with a version at least `version`.
fn events_until(
    events: &async_channel::Receiver<Arc<ChangeEvent>>,
    version: StoreVersion,
) -> Vec<Arc<ChangeEvent>> {
    let mut seen = Vec::new();
    loop {
        let event = events.recv_blocking().unwrap();
        let done = event.version >= version;
        seen.push(event);
        if done {
            return seen;
        }
    }
}

#[test]
fn a_v1_store_migrates_and_keeps_its_accounts_keyless() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(schema::V1).unwrap();
    conn.execute_batch(
        "INSERT INTO schema_migrations VALUES (1, 0); PRAGMA user_version = 1;
         INSERT INTO accounts(name, address) VALUES ('Fixture', 'fixture@example.test');
         INSERT INTO folders(account_id, name, name_raw, role) VALUES (1, 'INBOX', 'INBOX', 'inbox');",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(names(&store), [("Fixture".to_owned(), None)]);
    assert_eq!(unified_total(&store), 0);
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn keyed_accounts_follow_accounts_toml() {
    let dir = scratch();
    let (store, blobs) = open(dir.path());
    let local = store
        .add_account(new_account("Local"))
        .recv_blocking()
        .unwrap()
        .value;
    let work = store
        .ensure_account(key("work"), new_account("Work"))
        .recv_blocking()
        .unwrap();
    let again = store
        .ensure_account(key("work"), new_account("Work"))
        .recv_blocking()
        .unwrap();
    assert_eq!(again.value, work.value);
    assert_eq!(
        again.version, work.version,
        "nothing changed, nothing committed"
    );
    let renamed = store
        .ensure_account(key("work"), new_account("Job"))
        .recv_blocking()
        .unwrap();
    assert_eq!(renamed.value, work.value);
    assert!(renamed.version > work.version);
    let work = work.value;
    let found = store
        .account_by_key(key("work"))
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    assert_eq!((found.id, found.name.as_str()), (work, "Job"));
    assert_eq!(
        store
            .account_by_key(key("none"))
            .recv_blocking()
            .unwrap()
            .value,
        None
    );

    let work_inbox = folder(&store, work, "INBOX", FolderRole::Inbox);
    let local_inbox = folder(&store, local, "INBOX", FolderRole::Inbox);
    ingest(
        &store,
        &blobs,
        work_inbox,
        (1..=3).map(|n| raw(n, "alpha", None)).collect(),
    );
    ingest(
        &store,
        &blobs,
        local_inbox,
        (10..=11).map(|n| raw(n, "beta", None)).collect(),
    );
    assert_eq!(unified_total(&store), 5);

    // The user deletes the work entry by hand and adds a home account.
    let events = store.subscribe();
    let opened = store
        .open_view(OpenView {
            spec: ViewSpec::unified_inbox(),
            first_page: 10,
            section_starts: Vec::new(),
        })
        .recv_blocking()
        .unwrap();
    let ids = store
        .reconcile_accounts(vec![(key("home"), new_account("Home"))])
        .recv_blocking()
        .unwrap();
    let event = events_until(&events, ids.version).pop().unwrap();
    assert!(event.folders_changed);
    assert_eq!(
        event.views,
        [ViewChange {
            view: opened.view.id(),
            diff: ViewDiff::Reload { total: 2 }
        }]
    );
    assert_eq!(
        names(&store),
        [
            ("Local".to_owned(), None),
            ("Home".to_owned(), Some("home".to_owned()))
        ]
    );
    assert_eq!(unified_total(&store), 2);
    assert!(
        store
            .account_by_key(key("work"))
            .recv_blocking()
            .unwrap()
            .value
            .is_some(),
        "a detached account keeps its rows"
    );
    assert_eq!(
        search(&store, "alpha"),
        0,
        "a search of every folder leaves a detached account out"
    );
    let in_folder = store.search("alpha", Some(work_inbox), 10).recv_blocking();
    assert_eq!(in_folder.unwrap().value.len(), 3);

    // The entry comes back: the same account, with its mail.
    let back = store
        .reconcile_accounts(vec![
            (key("work"), new_account("Job")),
            (key("home"), new_account("Home")),
        ])
        .recv_blocking()
        .unwrap();
    assert_eq!(back.value[0], work);
    assert_eq!(back.value[1], ids.value[0]);
    assert_eq!(unified_total(&store), 5);
    assert_eq!(search(&store, "alpha"), 3);
    let same = store
        .reconcile_accounts(vec![
            (key("work"), new_account("Job")),
            (key("home"), new_account("Home")),
        ])
        .recv_blocking()
        .unwrap();
    assert_eq!(same.version, back.version);
    assert!(
        store
            .ensure_account(key("other"), new_account("Home"))
            .recv_blocking()
            .is_ok(),
        "the store doesn't police addresses; accounts.toml does"
    );
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

/// Counts of an account's rows in every table that holds them.
fn rows_of(path: &Path, account: AccountId) -> [i64; 5] {
    let conn = Connection::open(path).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [account.0], |row| row.get(0)).unwrap() };
    [
        count("SELECT count(*) FROM accounts WHERE id = ?1"),
        count("SELECT count(*) FROM folders WHERE account_id = ?1"),
        count("SELECT count(*) FROM threads WHERE account_id = ?1"),
        count("SELECT count(*) FROM thread_aliases WHERE account_id = ?1"),
        count(
            "SELECT count(*) FROM message_parts p JOIN messages m ON m.id = p.message \
             JOIN folders f ON f.id = m.folder_id WHERE f.account_id = ?1",
        ),
    ]
}

#[test]
fn removing_an_account_deletes_what_it_owns() {
    let dir = scratch();
    let (store, blobs) = open(dir.path());
    let gone = store
        .ensure_account(key("gone"), new_account("Gone"))
        .recv_blocking()
        .unwrap()
        .value;
    let kept = store
        .ensure_account(key("kept"), new_account("Kept"))
        .recv_blocking()
        .unwrap()
        .value;
    let gone_inbox = folder(&store, gone, "INBOX", FolderRole::Inbox);
    let gone_archive = folder(&store, gone, "Archive", FolderRole::Archive);
    let kept_inbox = folder(&store, kept, "INBOX", FolderRole::Inbox);
    // Message 5 is in both accounts: its blob must stay.
    ingest(
        &store,
        &blobs,
        gone_inbox,
        (1..=20)
            .map(|n| raw(n, "alpha", (n % 3 == 0).then_some(n - 1)))
            .collect(),
    );
    ingest(
        &store,
        &blobs,
        gone_archive,
        vec![raw(1, "alpha", None), raw(21, "alpha", None)],
    );
    ingest(
        &store,
        &blobs,
        kept_inbox,
        vec![
            raw(5, "alpha", None),
            raw(30, "beta", None),
            raw(31, "beta", None),
        ],
    );
    let shared = BlobStore::key(&raw(5, "alpha", None));
    let blobs_before = blobs.disk_usage().unwrap().files;
    assert_eq!(blobs_before, 23);
    let path = dir.path().join("store.sqlite");
    assert!(rows_of(&path, gone).iter().all(|count| *count > 0));

    let view = |spec| {
        store
            .open_view(OpenView {
                spec,
                first_page: 10,
                section_starts: Vec::new(),
            })
            .recv_blocking()
            .unwrap()
    };
    let gone_view = view(ViewSpec::folder(gone_inbox));
    let unified = view(ViewSpec::unified_inbox());
    let kept_view = view(ViewSpec::folder(kept_inbox));
    let events = store.subscribe();
    let removed = store.remove_account(gone).recv_blocking().unwrap();
    assert!(removed.found);
    assert_eq!((removed.folders, removed.messages), (2, 22));
    let orphaned: BTreeSet<_> = removed.orphaned_blobs.iter().copied().collect();
    assert_eq!(
        orphaned.len(),
        20,
        "21 distinct blobs, one shared with the kept account"
    );
    assert!(!orphaned.contains(&shared));

    let event = events_until(&events, removed.version).pop().unwrap();
    assert!(event.folders_changed);
    assert_eq!(event.removed.len(), 22);
    let mut reloads: Vec<_> = event
        .views
        .iter()
        .map(|change| (change.view, change.diff.clone()))
        .collect();
    reloads.sort_by_key(|(view, _)| *view);
    assert_eq!(
        reloads,
        [
            (gone_view.view.id(), ViewDiff::Reload { total: 0 }),
            (unified.view.id(), ViewDiff::Reload { total: 3 }),
        ]
    );
    drop(kept_view);

    assert_eq!(rows_of(&path, gone), [0; 5]);
    assert_eq!(
        search(&store, "alpha"),
        1,
        "only the kept copy of message 5"
    );
    assert_eq!(search(&store, "beta"), 2);
    assert_eq!(
        names(&store),
        [("Kept".to_owned(), Some("kept".to_owned()))]
    );
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    for key in &removed.orphaned_blobs {
        assert!(blobs.remove(key).unwrap());
    }
    assert_eq!(blobs.disk_usage().unwrap().files, 3);
    assert!(blobs.contains(&shared));

    let again = store.remove_account(gone).recv_blocking().unwrap();
    assert!(!again.found);
    assert_eq!(again.version, removed.version);
    // New mail still reaches the kept account's folders.
    ingest(&store, &blobs, kept_inbox, vec![raw(40, "gamma", None)]);
    assert_eq!(search(&store, "gamma"), 1);
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES ('integrity-check')")
        .unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}
