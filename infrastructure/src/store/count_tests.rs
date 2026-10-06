//! The server's counts for a folder that keeps only its newest mail (the M3 gate, 2026-10-06):
//! the store's own counts plus what STATUS leaves below the floor, in `folders()` and in change
//! events, with the clamps, stale STATUS ignored, older mail moving counts across the floor and
//! a v5 store migrating.

use super::tests::scratch;
use super::*;
use rusqlite::Connection;
use sealwax_domain::model::{Address, FolderRole, Headers};

fn open() -> (tempfile::TempDir, Store, AccountId) {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Test".into(),
            address: "test@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    (dir, store, account)
}

fn inbox(store: &Store, account: AccountId) -> FolderId {
    store
        .upsert_folders(vec![FolderSpec::local(account, "INBOX", FolderRole::Inbox)])
        .recv_blocking()
        .unwrap()
        .value[0]
}

/// Every third message unread.
fn message(folder: FolderId, uid: u32) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        flags: if uid.is_multiple_of(3) {
            Flags::NONE
        } else {
            Flags::SEEN
        },
        date: 1_790_000_000 + i64::from(uid) * 60,
        headers: Headers {
            from: vec![Address::new("Sender", "s@example.test")],
            subject: format!("Message {uid}"),
            message_id: Some(format!("m{uid}@example.test")),
            ..Headers::default()
        },
        snippet: format!("Snippet {uid}"),
        search_text: format!("body of {uid}"),
        size: 100,
        ..NewMessage::default()
    }
}

fn unread(uids: std::ops::RangeInclusive<u32>) -> u32 {
    uids.filter(|uid| uid.is_multiple_of(3)).count() as u32
}

const UIDVALIDITY: u32 = 7;
const TOP: u32 = 1_001;
const MODSEQ: u64 = 50;

/// A first sync that kept UIDs 801–1000 (67 unread) and left 800 below the floor.
fn windowed(store: &Store, folder: FolderId) {
    store
        .first_sync_step(
            folder,
            FirstSyncStep::Start {
                uidvalidity: UIDVALIDITY,
                top: TOP,
                modseq: MODSEQ,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .ingest((801..=1_000).map(|uid| message(folder, uid)).collect())
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(folder, FirstSyncStep::Floor(801))
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(folder, FirstSyncStep::Finish { below: 800 })
        .recv_blocking()
        .unwrap();
}

fn status(total: u32, unread: u32) -> ServerStatus {
    ServerStatus {
        uidvalidity: UIDVALIDITY,
        uidnext: TOP,
        highestmodseq: Some(MODSEQ),
        counts: ServerCounts { total, unread },
    }
}

fn shown(store: &Store, folder: FolderId) -> (u32, u32) {
    let folders = store.folders().recv_blocking().unwrap().value;
    let found = folders.iter().find(|f| f.id == folder).unwrap();
    (found.total, found.unread)
}

fn counts(folder: FolderId, (total, unread): (u32, u32)) -> Vec<FolderCounts> {
    vec![FolderCounts {
        folder,
        total,
        unread,
    }]
}

fn count_below(store: &Store, folder: FolderId, status: ServerStatus) -> bool {
    store
        .count_below(folder, status)
        .recv_blocking()
        .unwrap()
        .value
}

#[test]
fn a_windowed_folder_shows_the_servers_counts() {
    let (_dir, store, account) = open();
    let folder = inbox(&store, account);
    let events = store.subscribe();
    windowed(&store, folder);
    let kept = unread(801..=1_000);
    assert_eq!(kept, 67);
    let finished = std::iter::from_fn(|| events.try_recv().ok())
        .last()
        .unwrap();
    assert_eq!(
        finished.folders,
        counts(folder, (1_000, kept)),
        "the finish adds what it left below"
    );
    assert_eq!(shown(&store, folder), (1_000, kept));

    let before = store.folders().recv_blocking().unwrap().version;
    let counted = store
        .count_below(folder, status(1_000, 333))
        .recv_blocking()
        .unwrap();
    assert!(counted.value);
    assert!(counted.version > before);
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.version, counted.version);
    assert_eq!(
        *event,
        ChangeEvent {
            version: counted.version,
            folders: counts(folder, (1_000, 333)),
            ..ChangeEvent::default()
        },
        "the counts and nothing else"
    );
    assert!(events.try_recv().is_err());
    assert_eq!(shown(&store, folder), (1_000, 333));
    let state = store.folder_sync(folder).recv_blocking().unwrap().unwrap();
    assert_eq!((state.below, state.messages), (800, 200));

    let again = store
        .count_below(folder, status(1_000, 333))
        .recv_blocking()
        .unwrap();
    assert_eq!((again.version, again.value), (counted.version, false));
    assert!(
        events.try_recv().is_err(),
        "the same STATUS publishes nothing"
    );

    // Another client deleted three old messages and read ten.
    assert!(count_below(&store, folder, status(997, 323)));
    assert_eq!(
        events.recv_blocking().unwrap().folders,
        counts(folder, (997, 323))
    );
    let state = store.folder_sync(folder).recv_blocking().unwrap().unwrap();
    assert_eq!(state.below, 797, "the skip compares MESSAGES with it");
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn the_counts_below_are_clamped() {
    let (_dir, store, account) = open();
    let folder = inbox(&store, account);
    windowed(&store, folder);
    let kept = unread(801..=1_000);
    for (server, expected) in [
        ((150, 10), (200, kept)),
        ((1_000, 40), (1_000, kept)),
        ((1_000, 990), (1_000, kept + 800)),
        ((1_200, 500), (1_200, 500)),
    ] {
        count_below(&store, folder, status(server.0, server.1));
        assert_eq!(shown(&store, folder), expected, "STATUS {server:?}");
        assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    }
    store.close().unwrap();
}

#[test]
fn a_status_older_than_the_store_is_ignored() {
    let (_dir, store, account) = open();
    let folder = inbox(&store, account);
    windowed(&store, folder);
    let kept = unread(801..=1_000);
    let events = store.subscribe();
    let stale = [
        ServerStatus {
            uidnext: TOP - 1,
            ..status(999, 300)
        },
        ServerStatus {
            uidvalidity: UIDVALIDITY + 1,
            ..status(999, 300)
        },
        ServerStatus {
            highestmodseq: Some(MODSEQ - 1),
            ..status(999, 300)
        },
    ];
    for status in stale {
        assert!(!count_below(&store, folder, status), "{status:?}");
    }
    assert!(events.try_recv().is_err());
    assert_eq!(shown(&store, folder), (1_000, kept));
    let newer = ServerStatus {
        uidnext: TOP + 5,
        highestmodseq: None,
        ..status(1_005, 300)
    };
    assert!(
        count_below(&store, folder, newer),
        "a server ahead of the store, or without CONDSTORE"
    );
    assert_eq!(shown(&store, folder), (1_005, 300));
    assert!(matches!(
        store
            .count_below(FolderId(999), status(1, 1))
            .recv_blocking(),
        Err(StoreError::UnknownFolder(_))
    ));
    store.close().unwrap();
}

#[test]
fn complete_and_new_folders_keep_their_counts() {
    let (_dir, store, account) = open();
    let ids = store
        .upsert_folders(vec![
            FolderSpec::local(account, "INBOX", FolderRole::Inbox),
            FolderSpec {
                server_counts: Some(ServerCounts {
                    total: 40,
                    unread: 4,
                }),
                ..FolderSpec::local(account, "Arkiv", FolderRole::Archive)
            },
        ])
        .recv_blocking()
        .unwrap()
        .value;
    let (complete, new) = (ids[0], ids[1]);
    store
        .first_sync_step(
            complete,
            FirstSyncStep::Start {
                uidvalidity: UIDVALIDITY,
                top: 31,
                modseq: MODSEQ,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .ingest((1..=30).map(|uid| message(complete, uid)).collect())
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(complete, FirstSyncStep::Finish { below: 0 })
        .recv_blocking()
        .unwrap();
    let state = store
        .folder_sync(complete)
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert!(state.complete());
    let events = store.subscribe();
    let server = ServerStatus {
        uidnext: 31,
        ..status(45, 20)
    };
    assert!(!count_below(&store, complete, server));
    assert!(!count_below(&store, new, server));
    assert!(events.try_recv().is_err());
    assert_eq!(shown(&store, complete), (30, unread(1..=30)));
    assert_eq!(shown(&store, new), (40, 4), "STATUS counts until it syncs");
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn older_mail_moves_counts_across_the_floor() {
    let (_dir, store, account) = open();
    let folder = inbox(&store, account);
    windowed(&store, folder);
    count_below(&store, folder, status(1_000, 333));
    let events = store.subscribe();
    let older = |from: u32, to: u32, floor: u32, below: u32| FolderChanges {
        new: (from..=to).map(|uid| message(folder, uid)).collect(),
        floor: Some(floor),
        below: Some(below),
        ..FolderChanges::default()
    };

    let synced = store
        .sync_changes(folder, older(601, 800, 601, 600))
        .recv_blocking()
        .unwrap();
    assert_eq!(synced.inserted, 200);
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.inserted.len(), 200);
    assert_eq!(
        event.folders,
        counts(folder, (1_000, 333)),
        "200 older messages, 66 of them unread, move from below the floor into the store"
    );
    assert_eq!(unread(601..=800), 66);
    assert_eq!(shown(&store, folder), (1_000, 333));

    // Fewer unread below the floor than the next batch holds: the rest is clamped at 0.
    assert!(count_below(
        &store,
        folder,
        status(1_000, unread(601..=1_000) + 10)
    ));
    events.recv_blocking().unwrap();
    store
        .sync_changes(folder, older(401, 600, 401, 400))
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    let stored_unread = unread(401..=1_000);
    assert_eq!(event.folders, counts(folder, (1_000, stored_unread)));

    // The last of it: nothing is left below.
    store
        .sync_changes(folder, older(1, 400, 1, 0))
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.folders, counts(folder, (1_000, unread(1..=1_000))));
    let state = store.folder_sync(folder).recv_blocking().unwrap().unwrap();
    assert!(state.complete());
    assert!(
        !count_below(&store, folder, status(1_000, 500)),
        "a complete folder counts its own"
    );
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn a_recount_keeps_the_unread_below_within_it() {
    let (_dir, store, account) = open();
    let folder = inbox(&store, account);
    windowed(&store, folder);
    count_below(&store, folder, status(1_000, 333));
    let events = store.subscribe();
    let recount = |below: u32| FolderChanges {
        below: Some(below),
        ..FolderChanges::default()
    };
    let synced = store
        .sync_changes(folder, recount(800))
        .recv_blocking()
        .unwrap();
    assert!(
        events.try_recv().is_err(),
        "the same count publishes nothing"
    );
    let synced_again = store
        .sync_changes(folder, recount(100))
        .recv_blocking()
        .unwrap();
    assert!(synced_again.version > synced.version);
    let event = events.recv_blocking().unwrap();
    assert_eq!(
        *event,
        ChangeEvent {
            version: synced_again.version,
            folders: counts(folder, (300, 167)),
            ..ChangeEvent::default()
        }
    );
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());

    // A new UIDVALIDITY empties the folder, and its counts go with it.
    let removed = store
        .reset_folder(folder, UIDVALIDITY + 1)
        .recv_blocking()
        .unwrap();
    let last = std::iter::from_fn(|| events.try_recv().ok())
        .last()
        .unwrap();
    assert_eq!(last.version, removed.version);
    assert_eq!(last.folders, counts(folder, (0, 0)));
    assert_eq!(shown(&store, folder), (0, 0));
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn a_v5_store_migrates_with_nothing_unread_below() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    for (index, sql) in schema::MIGRATIONS[..5].iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            [index as i64 + 1],
        )
        .unwrap();
    }
    conn.execute_batch(
        "PRAGMA user_version = 5;
         INSERT INTO accounts(name, address) VALUES ('Fixture', 'fixture@example.test');
         INSERT INTO folders(account_id, name, name_raw, role, sync_state, uidvalidity, \
           uidnext, sync_floor, below_floor) \
           VALUES (1, 'INBOX', 'INBOX', 'inbox', 'ready', 7, 1001, 801, 800);
         INSERT INTO folders(account_id, name, name_raw, role, sync_state, uidvalidity, \
           uidnext, sync_floor) VALUES (1, 'Arkiv', 'Arkiv', 'archive', 'ready', 3, 9, 1);",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let folders = store.folders().recv_blocking().unwrap().value;
    assert_eq!(
        folders
            .iter()
            .map(|f| (f.name.as_str(), f.total, f.unread))
            .collect::<Vec<_>>(),
        [("INBOX", 800, 0), ("Arkiv", 0, 0)]
    );
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 10);
}
