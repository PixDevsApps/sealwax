//! Folder management (ADR-020, M9): a rename in place keeps every message, the sync state and
//! the folders' ids; a delete takes the folders' messages with them; both refuse another
//! account's folders; a POP3 account's local folders take the same changes. Every name is
//! fictional.

use super::ops_tests::message;
use super::tests::scratch;
use super::*;
use sealwax_domain::account::AccountKey;
use sealwax_domain::model::{Flags, FolderRole};

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

fn listed(account: AccountId, name: &str, attributes: &[&str]) -> FolderSpec {
    FolderSpec {
        attributes: attributes.iter().map(|a| a.to_string()).collect(),
        ..FolderSpec::local(account, name, FolderRole::Other)
    }
}

fn folders(store: &Store, account: AccountId) -> Vec<Folder> {
    store
        .folders()
        .recv_blocking()
        .unwrap()
        .value
        .into_iter()
        .filter(|folder| folder.account == account)
        .collect()
}

fn named(store: &Store, account: AccountId, name: &str) -> Option<Folder> {
    folders(store, account)
        .into_iter()
        .find(|folder| folder.name == name)
}

fn rename(folder: FolderId, name: &str) -> FolderRename {
    FolderRename {
        folder,
        name: name.to_owned(),
        name_raw: name.to_owned(),
    }
}

#[test]
fn folders_carry_their_delimiter_and_whether_they_take_subfolders() {
    let (_dir, store, account) = open();
    store
        .upsert_folders(vec![
            listed(account, "Kunder", &["\\HasChildren"]),
            listed(account, "Arkiv 2019", &["\\NoInferiors"]),
            FolderSpec {
                delimiter: Some('.'),
                ..listed(account, "INBOX.Kunder", &[])
            },
            FolderSpec {
                delimiter: None,
                ..listed(account, "Flat", &[])
            },
        ])
        .recv_blocking()
        .unwrap();
    let kunder = named(&store, account, "Kunder").unwrap();
    assert_eq!((kunder.delimiter, kunder.inferiors), (Some('/'), true));
    let old = named(&store, account, "Arkiv 2019").unwrap();
    assert!(!old.inferiors, "\\Noinferiors in any case");
    assert_eq!(
        named(&store, account, "INBOX.Kunder").unwrap().delimiter,
        Some('.')
    );
    assert_eq!(named(&store, account, "Flat").unwrap().delimiter, None);
}

#[test]
fn a_rename_keeps_the_messages_and_the_sync_state_in_place() {
    let (_dir, store, account) = open();
    let ids = store
        .upsert_folders(vec![
            listed(account, "Kunder", &[]),
            listed(account, "Kunder/Acme", &[]),
            listed(account, "Kunderservice", &[]),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    let (kunder, acme, service) = (ids[0], ids[1], ids[2]);
    store
        .first_sync_step(
            acme,
            FirstSyncStep::Start {
                uidvalidity: 41,
                top: 4,
                modseq: 1,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .ingest((1..=3).map(|uid| message(acme, uid, Flags::NONE)).collect())
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(acme, FirstSyncStep::Finish { below: 0 })
        .recv_blocking()
        .unwrap();
    let before = store.folder_sync(acme).recv_blocking().unwrap().unwrap();
    let events = store.subscribe();
    let version = store
        .rename_folders(
            account,
            vec![
                FolderRename {
                    folder: kunder,
                    name: "Klienter æøå".into(),
                    name_raw: "Klienter &AOYA+ADl-".into(),
                },
                FolderRename {
                    folder: acme,
                    name: "Klienter æøå/Acme".into(),
                    name_raw: "Klienter &AOYA+ADl-/Acme".into(),
                },
            ],
        )
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    assert!(event.folders_changed);
    assert_eq!(event.version, version);
    let after = store.folder_sync(acme).recv_blocking().unwrap().unwrap();
    assert_eq!(after.name_raw, "Klienter &AOYA+ADl-/Acme");
    assert_eq!(
        (
            after.uidvalidity,
            after.uidnext,
            after.state,
            after.messages
        ),
        (
            before.uidvalidity,
            before.uidnext,
            before.state,
            before.messages
        )
    );
    assert_eq!(after.messages, 3);
    assert_eq!(
        store.folder_messages(acme).recv_blocking().unwrap().len(),
        3
    );
    let shown = named(&store, account, "Klienter æøå/Acme").unwrap();
    assert_eq!((shown.id, shown.total), (acme, 3));
    assert_eq!(named(&store, account, "Klienter æøå").unwrap().id, kunder);
    assert_eq!(
        named(&store, account, "Kunderservice").unwrap().id,
        service,
        "a name that only starts the same stays"
    );
    assert!(named(&store, account, "Kunder").is_none());
}

#[test]
fn names_can_change_places_and_a_taken_name_refuses_the_whole_rename() {
    let (_dir, store, account) = open();
    let ids = store
        .upsert_folders(vec![
            listed(account, "A", &[]),
            listed(account, "B", &[]),
            listed(account, "C", &[]),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    store
        .rename_folders(account, vec![rename(ids[0], "B"), rename(ids[1], "A")])
        .recv_blocking()
        .unwrap();
    assert_eq!(named(&store, account, "B").unwrap().id, ids[0]);
    assert_eq!(named(&store, account, "A").unwrap().id, ids[1]);
    let taken = store
        .rename_folders(account, vec![rename(ids[2], "D"), rename(ids[0], "A")])
        .recv_blocking();
    assert!(matches!(taken, Err(StoreError::Sqlite(_))), "{taken:?}");
    assert_eq!(
        named(&store, account, "C").unwrap().id,
        ids[2],
        "nothing of a refused rename stays"
    );
    let other = store
        .add_account(NewAccount {
            name: "Other".into(),
            address: "other@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let theirs = store
        .rename_folders(other, vec![rename(ids[2], "Mine")])
        .recv_blocking();
    assert!(
        matches!(theirs, Err(StoreError::UnknownFolder(folder)) if folder == ids[2]),
        "{theirs:?}"
    );
}

#[test]
fn a_delete_takes_the_folders_messages_with_them() {
    let (_dir, store, account) = open();
    let ids = store
        .upsert_folders(vec![
            FolderSpec::local(account, "INBOX", FolderRole::Inbox),
            listed(account, "Kunder", &[]),
            listed(account, "Kunder/Acme", &[]),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    let (inbox, kunder, acme) = (ids[0], ids[1], ids[2]);
    store
        .ingest(vec![
            message(inbox, 1, Flags::NONE),
            message(kunder, 2, Flags::NONE),
            message(acme, 3, Flags::SEEN),
            message(acme, 4, Flags::NONE),
        ])
        .recv_blocking()
        .unwrap();
    let other = store
        .add_account(NewAccount {
            name: "Other".into(),
            address: "other@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let theirs = store.delete_folders(other, vec![acme]).recv_blocking();
    assert!(
        matches!(theirs, Err(StoreError::UnknownFolder(_))),
        "{theirs:?}"
    );
    let events = store.subscribe();
    let deleted = store
        .delete_folders(account, vec![acme, kunder, acme])
        .recv_blocking()
        .unwrap();
    assert_eq!((deleted.removed_folders, deleted.removed_messages), (2, 3));
    let event = events.recv_blocking().unwrap();
    assert!(event.folders_changed);
    assert_eq!(event.removed.len(), 3);
    let left: Vec<String> = folders(&store, account)
        .into_iter()
        .map(|folder| folder.name)
        .collect();
    assert_eq!(left, ["INBOX"]);
    assert_eq!(
        store.folder_messages(inbox).recv_blocking().unwrap().len(),
        1
    );
}

#[test]
fn a_pop3_accounts_local_folders_are_renamed_and_deleted_too() {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .ensure_account(
            AccountKey("pop3-folders".into()),
            NewAccount {
                name: "Pop".into(),
                address: "pop@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    store.pop3_account(account).recv_blocking().unwrap();
    let kunder = store
        .upsert_folder(FolderSpec::local(account, "Kunder", FolderRole::Other))
        .recv_blocking()
        .unwrap()
        .value;
    let acme = store
        .upsert_folder(FolderSpec::local(account, "Kunder/Acme", FolderRole::Other))
        .recv_blocking()
        .unwrap()
        .value;
    store
        .ingest(vec![NewMessage {
            uid: None,
            ..message(acme, 1, Flags::NONE)
        }])
        .recv_blocking()
        .unwrap();
    store
        .rename_folders(
            account,
            vec![rename(kunder, "Klienter"), rename(acme, "Klienter/Acme")],
        )
        .recv_blocking()
        .unwrap();
    let renamed = named(&store, account, "Klienter/Acme").unwrap();
    assert_eq!((renamed.id, renamed.total), (acme, 1));
    let deleted = store
        .delete_folders(account, vec![acme, kunder])
        .recv_blocking()
        .unwrap();
    assert_eq!((deleted.removed_folders, deleted.removed_messages), (2, 1));
    assert_eq!(folders(&store, account).len(), POP3_FOLDERS.len());
    assert!(
        store.is_pop3_account(account).recv_blocking().unwrap(),
        "still a POP3 account"
    );
}
