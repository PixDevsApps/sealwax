//! The folder list and the first sync's state (M2: ML-036, ML-037).

use super::tests::scratch;
use super::*;
use sealwax_domain::model::{FolderRole, Headers};

fn listed(
    account: AccountId,
    name: &str,
    role: FolderRole,
    counts: Option<(u32, u32)>,
) -> FolderSpec {
    FolderSpec {
        attributes: vec!["\\HasNoChildren".into()],
        server_counts: counts.map(|(total, unread)| ServerCounts { total, unread }),
        ..FolderSpec::local(account, name, role)
    }
}

fn message(folder: FolderId, uid: u32, seen: bool) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        flags: if seen { Flags::SEEN } else { Flags::NONE },
        date: 1_790_000_000 + i64::from(uid),
        headers: Headers {
            subject: format!("Message {uid}"),
            message_id: Some(format!("m{uid}@example.test")),
            ..Headers::default()
        },
        ..NewMessage::default()
    }
}

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

fn counts(store: &Store, folder: FolderId) -> (u32, u32) {
    let folders = store.folders().recv_blocking().unwrap().value;
    let found = folders.iter().find(|f| f.id == folder).unwrap();
    (found.total, found.unread)
}

#[test]
fn a_folder_list_lands_in_one_commit_with_server_counts_for_new_folders() {
    let (_dir, store, account) = open();
    let events = store.subscribe();
    let mut container = listed(account, "Prosjekter", FolderRole::Other, Some((9, 9)));
    container.selectable = false;
    container.attributes = vec!["\\Noselect".into(), "\\HasChildren".into()];
    let ids = store
        .upsert_folders(vec![
            listed(account, "INBOX", FolderRole::Inbox, Some((120, 7))),
            listed(account, "Søppelpost", FolderRole::Junk, Some((3, 3))),
            container,
            listed(account, "Arkiv", FolderRole::Archive, None),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    assert_eq!(ids.len(), 4);
    let event = events.recv_blocking().unwrap();
    assert!(event.folders_changed);
    assert_eq!(
        event.folders,
        vec![
            FolderCounts {
                folder: ids[0],
                total: 120,
                unread: 7
            },
            FolderCounts {
                folder: ids[1],
                total: 3,
                unread: 3
            },
        ]
    );
    assert!(events.try_recv().is_err(), "one commit for the whole list");
    assert_eq!(counts(&store, ids[0]), (120, 7));
    assert_eq!(
        counts(&store, ids[2]),
        (0, 0),
        "a \\Noselect folder has no counts"
    );
    let container = store.folder_sync(ids[2]).recv_blocking().unwrap().unwrap();
    assert!(!container.selectable);
    assert_eq!(container.state, SyncState::New);
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());

    let again = store
        .upsert_folders(vec![listed(
            account,
            "INBOX",
            FolderRole::Inbox,
            Some((121, 8)),
        )])
        .recv_blocking()
        .unwrap()
        .value;
    assert_eq!(again, vec![ids[0]], "upserted by wire name");
    assert_eq!(counts(&store, ids[0]), (121, 8));
    store.close().unwrap();
}

#[test]
fn the_first_sync_resumes_from_its_floor_and_ends_ready() {
    let (_dir, store, account) = open();
    let inbox = store
        .upsert_folders(vec![listed(
            account,
            "INBOX",
            FolderRole::Inbox,
            Some((4, 2)),
        )])
        .recv_blocking()
        .unwrap()
        .value[0];
    let floor = |store: &Store, uid| {
        store
            .first_sync_step(inbox, FirstSyncStep::Floor(uid))
            .recv_blocking()
    };
    assert!(matches!(
        floor(&store, 3),
        Err(StoreError::SyncState(folder)) if folder == inbox
    ));
    store
        .first_sync_step(
            inbox,
            FirstSyncStep::Start {
                uidvalidity: 77,
                top: 11,
                modseq: 5,
            },
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(
        counts(&store, inbox),
        (0, 0),
        "the store's own counts from now on"
    );
    store
        .ingest(vec![message(inbox, 10, false), message(inbox, 7, true)])
        .recv_blocking()
        .unwrap();
    floor(&store, 7).unwrap();
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!(state.uidvalidity, Some(77));
    assert_eq!(
        state.state,
        SyncState::Initial {
            top: 11,
            modseq: 5,
            floor: 7
        }
    );
    assert_eq!((state.messages, state.name_raw.as_str()), (2, "INBOX"));

    let again = store
        .ingest(vec![message(inbox, 7, true), message(inbox, 2, false)])
        .recv_blocking()
        .unwrap();
    assert_eq!(again.ids[0], None, "a refetched UID is skipped");
    floor(&store, 1).unwrap();
    store
        .first_sync_step(inbox, FirstSyncStep::Finish)
        .recv_blocking()
        .unwrap();
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!(state.state, SyncState::Ready);
    assert_eq!((state.uidnext, state.highestmodseq), (11, 5));
    assert_eq!((state.floor, state.messages), (Some(1), 3));
    assert_eq!(counts(&store, inbox), (3, 2));
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn a_uidvalidity_reset_removes_the_messages_and_starts_over() {
    let (_dir, store, account) = open();
    let inbox = store
        .upsert_folders(vec![listed(account, "INBOX", FolderRole::Inbox, None)])
        .recv_blocking()
        .unwrap()
        .value[0];
    store
        .first_sync_step(
            inbox,
            FirstSyncStep::Start {
                uidvalidity: 1,
                top: 3,
                modseq: 0,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .ingest(vec![message(inbox, 1, false), message(inbox, 2, false)])
        .recv_blocking()
        .unwrap();
    let removed = store.reset_folder(inbox, 2).recv_blocking().unwrap();
    assert_eq!(removed.removed.len(), 2);
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!(state.state, SyncState::New);
    assert_eq!((state.uidvalidity, state.messages), (Some(2), 0));
    assert_eq!(counts(&store, inbox), (0, 0));
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}
