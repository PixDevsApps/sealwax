//! The sync engine's store side (ML-041, ML-043): a folder's changes by UID with exact change
//! events, the folder list made to match the server's, and the reads the passes compare with.

use super::tests::{Model, Rng, scratch, whole};
use super::*;
use rusqlite::Connection;
use sealwax_domain::model::{Address, FolderRole, Headers};
use std::collections::BTreeMap;

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

fn listed(account: AccountId, name: &str, role: FolderRole) -> FolderSpec {
    FolderSpec {
        attributes: vec!["\\HasNoChildren".into()],
        ..FolderSpec::local(account, name, role)
    }
}

fn message(folder: FolderId, uid: u32, flags: Flags, parent: Option<u32>) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        flags,
        date: 1_790_000_000 + i64::from(uid % 97) * 60,
        headers: Headers {
            from: vec![Address::new(
                format!("Sender {uid}"),
                format!("s{uid}@example.test"),
            )],
            subject: format!("Message {uid}"),
            message_id: Some(format!("m{folder}.{uid}@example.test")),
            in_reply_to: parent
                .map(|parent| vec![format!("m{folder}.{parent}@example.test")])
                .unwrap_or_default(),
            ..Headers::default()
        },
        snippet: format!("Snippet {uid}"),
        search_text: format!("body of {uid}"),
        has_attachments: uid.is_multiple_of(5),
        size: 100,
        ..NewMessage::default()
    }
}

/// Starts and finishes a first sync with nothing fetched, so the folder is `Ready`.
fn ready(store: &Store, folder: FolderId, top: u32) {
    store
        .first_sync_step(
            folder,
            FirstSyncStep::Start {
                uidvalidity: 7,
                top,
                modseq: 1,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(folder, FirstSyncStep::Finish { below: 0 })
        .recv_blocking()
        .unwrap();
}

fn model(store: &Store, spec: ViewSpec) -> Model {
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

fn sync(store: &Store, folder: FolderId, changes: FolderChanges) -> FolderSynced {
    store.sync_changes(folder, changes).recv_blocking().unwrap()
}

/// What the store holds of a folder: UID to flags and keywords.
fn held(store: &Store, folder: FolderId) -> BTreeMap<u32, (Flags, Vec<String>)> {
    store
        .folder_messages(folder)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|stored| (stored.uid, (stored.flags, stored.keywords)))
        .collect()
}

const KEYWORDS: [&str; 3] = ["$Label1", "Prioritert", "Storm"];

#[test]
fn sync_steps_keep_list_models_exact() {
    let (_dir, store, account) = open();
    let ids = store
        .upsert_folders(vec![
            listed(account, "INBOX", FolderRole::Inbox),
            listed(account, "Arkiv", FolderRole::Archive),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    let (inbox, archive) = (ids[0], ids[1]);
    ready(&store, inbox, 1);
    ready(&store, archive, 1);
    let events = store.subscribe();
    let specs = [
        ViewSpec::folder(inbox),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Unread),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Starred),
        ViewSpec::folder(inbox).with_filter(ViewFilter::Attachments),
        ViewSpec::unified_inbox(),
        ViewSpec::folder(archive),
    ];
    let mut models: Vec<Model> = specs.iter().map(|spec| model(&store, *spec)).collect();
    let mut server: BTreeMap<FolderId, BTreeMap<u32, (Flags, Vec<String>)>> = BTreeMap::new();
    let mut next = BTreeMap::from([(inbox, 1u32), (archive, 1u32)]);
    let mut rng = Rng(41);
    let (mut splices, mut reloads, mut keyword_only) = (0, 0, 0);
    for step in 0..200u64 {
        let folder = if rng.below(4) == 0 { archive } else { inbox };
        let held_now: Vec<u32> = server.entry(folder).or_default().keys().copied().collect();
        let mut changes = FolderChanges::default();
        let new = match rng.below(10) {
            _ if step % 50 == 49 => 90,
            0..=3 => 1 + rng.below(3) as u32,
            _ => 0,
        };
        for _ in 0..new {
            let uid = next[&folder];
            next.insert(folder, uid + 1 + rng.below(2) as u32);
            let mut flags = Flags::NONE;
            if rng.below(3) == 0 {
                flags = flags | Flags::SEEN;
            }
            if rng.below(5) == 0 {
                flags = flags | Flags::FLAGGED;
            }
            let parent = (!held_now.is_empty() && rng.below(3) == 0)
                .then(|| held_now[rng.below(held_now.len() as u64) as usize]);
            changes.new.push(message(folder, uid, flags, parent));
            server
                .entry(folder)
                .or_default()
                .insert(uid, (flags, Vec::new()));
        }
        if !held_now.is_empty() && rng.below(2) == 0 {
            for _ in 0..1 + rng.below(4) {
                let uid = held_now[rng.below(held_now.len() as u64) as usize];
                let (old, old_keywords) = server[&folder][&uid].clone();
                let (flags, keywords) = match rng.below(4) {
                    0 => {
                        keyword_only += 1;
                        let mut keywords = old_keywords.clone();
                        keywords.push(KEYWORDS[rng.below(3) as usize].to_owned());
                        keywords.sort();
                        keywords.dedup();
                        (old, keywords)
                    }
                    1 => (old, old_keywords.clone()),
                    2 => (old.union(Flags::SEEN), old_keywords.clone()),
                    _ => (
                        old.difference(Flags::SEEN).union(Flags::FLAGGED),
                        Vec::new(),
                    ),
                };
                changes.flags.push(FlagUpdate {
                    uid,
                    flags,
                    keywords: keywords.clone(),
                });
                server
                    .get_mut(&folder)
                    .unwrap()
                    .insert(uid, (flags, keywords));
            }
        }
        if !held_now.is_empty() && rng.below(3) == 0 {
            for _ in 0..1 + rng.below(3) {
                let uid = held_now[rng.below(held_now.len() as u64) as usize];
                changes.removed.push(uid);
                server.get_mut(&folder).unwrap().remove(&uid);
            }
        }
        changes.removed.push(999_999);
        changes.uidnext = Some(next[&folder]);
        changes.highestmodseq = Some(step);
        let synced = sync(&store, folder, changes);
        while models[0].version < synced.version {
            let event = events.recv_blocking().unwrap();
            for change in &event.views {
                match change.diff {
                    ViewDiff::Splice { .. } => splices += 1,
                    ViewDiff::Reload { .. } => reloads += 1,
                }
            }
            for model in &mut models {
                model.apply(&store, &event);
            }
        }
        for model in &models {
            let page = whole(&store, model.view.spec());
            assert_eq!(page.version, model.version, "step {step}");
            assert_eq!(
                page.rows,
                model.rows,
                "step {step}, {:?}",
                model.view.spec()
            );
        }
        assert_eq!(held(&store, folder), server[&folder], "step {step}");
        let state = store.folder_sync(folder).recv_blocking().unwrap().unwrap();
        assert_eq!((state.uidnext, state.highestmodseq), (next[&folder], step));
    }
    assert!(
        splices > 100 && reloads > 0 && keyword_only > 10,
        "{splices} splices, {reloads} reloads, {keyword_only} keyword changes"
    );
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn a_step_needs_a_ready_folder_and_state_alone_publishes_nothing() {
    let (_dir, store, account) = open();
    let inbox = store
        .upsert_folders(vec![listed(account, "INBOX", FolderRole::Inbox)])
        .recv_blocking()
        .unwrap()
        .value[0];
    let step = |changes| store.sync_changes(inbox, changes).recv_blocking();
    assert!(matches!(
        step(FolderChanges::default()),
        Err(StoreError::SyncState(folder)) if folder == inbox
    ));
    assert!(matches!(
        store
            .sync_changes(FolderId(999), FolderChanges::default())
            .recv_blocking(),
        Err(StoreError::UnknownFolder(_))
    ));
    store
        .first_sync_step(
            inbox,
            FirstSyncStep::Start {
                uidvalidity: 3,
                top: 50,
                modseq: 9,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .ingest(
            (40..50)
                .map(|uid| message(inbox, uid, Flags::SEEN, None))
                .collect(),
        )
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(inbox, FirstSyncStep::Floor(40))
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(inbox, FirstSyncStep::Finish { below: 25 })
        .recv_blocking()
        .unwrap();
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!(state.state, SyncState::Ready);
    assert_eq!(
        (state.floor, state.below, state.messages),
        (Some(40), 25, 10)
    );
    assert!(!state.complete());

    let events = store.subscribe();
    let before = store.folders().recv_blocking().unwrap().version;
    let synced = step(FolderChanges {
        uidnext: Some(45),
        highestmodseq: Some(12),
        flags: vec![FlagUpdate {
            uid: 41,
            flags: Flags::SEEN,
            keywords: Vec::new(),
        }],
        ..FolderChanges::default()
    })
    .unwrap();
    assert_eq!(synced.version, before, "nothing a list shows changed");
    assert_eq!((synced.inserted, synced.changed, synced.removed), (0, 0, 0));
    assert!(events.try_recv().is_err());
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!(
        (state.uidnext, state.highestmodseq),
        (50, 12),
        "UIDNEXT only rises"
    );

    let synced = step(FolderChanges {
        new: (30..40)
            .map(|uid| message(inbox, uid, Flags::NONE, None))
            .collect(),
        floor: Some(30),
        below: Some(15),
        ..FolderChanges::default()
    })
    .unwrap();
    assert_eq!(synced.inserted, 10);
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.inserted.len(), 10);
    assert_eq!(
        event.folders,
        vec![FolderCounts {
            folder: inbox,
            total: 20,
            unread: 10
        }]
    );
    let again = step(FolderChanges {
        new: vec![message(inbox, 35, Flags::NONE, None)],
        floor: Some(1),
        below: Some(0),
        ..FolderChanges::default()
    })
    .unwrap();
    assert_eq!(again.inserted, 0, "a UID the folder holds is skipped");
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!((state.floor, state.below, state.messages), (Some(1), 0, 20));
    assert!(state.complete());
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn a_reset_and_a_finish_with_nothing_below_set_the_floor() {
    let (_dir, store, account) = open();
    let inbox = store
        .upsert_folders(vec![listed(account, "INBOX", FolderRole::Inbox)])
        .recv_blocking()
        .unwrap()
        .value[0];
    store
        .first_sync_step(
            inbox,
            FirstSyncStep::Start {
                uidvalidity: 3,
                top: 9,
                modseq: 0,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(inbox, FirstSyncStep::Floor(4))
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(inbox, FirstSyncStep::Finish { below: 0 })
        .recv_blocking()
        .unwrap();
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!((state.floor, state.below), (Some(1), 0));
    store
        .sync_changes(
            inbox,
            FolderChanges {
                below: Some(4),
                floor: Some(6),
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    store.reset_folder(inbox, 4).recv_blocking().unwrap();
    let state = store.folder_sync(inbox).recv_blocking().unwrap().unwrap();
    assert_eq!(
        (state.state, state.floor, state.below, state.uidvalidity),
        (SyncState::New, None, 0, Some(4))
    );
    store.close().unwrap();
}

#[test]
fn a_folder_gone_from_the_server_leaves_with_its_messages() {
    let (_dir, store, account) = open();
    let other = store
        .add_account(NewAccount {
            name: "Other".into(),
            address: "other@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let ids = store
        .upsert_folders(vec![
            listed(account, "INBOX", FolderRole::Inbox),
            listed(account, "Gammel", FolderRole::Other),
            listed(account, "Arkiv", FolderRole::Archive),
            listed(other, "Gammel", FolderRole::Other),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    let (inbox, old, archive, foreign) = (ids[0], ids[1], ids[2], ids[3]);
    let blob = |n: u8| BlobKey([n; 32]);
    let mut messages = vec![
        message(inbox, 1, Flags::SEEN, None),
        message(old, 1, Flags::NONE, None),
        message(old, 2, Flags::SEEN, None),
        message(foreign, 1, Flags::NONE, None),
    ];
    messages[0].blob = Some(blob(1));
    messages[1].blob = Some(blob(1));
    messages[2].blob = Some(blob(2));
    messages[3].blob = Some(blob(3));
    // A reply in INBOX to a message in the folder that goes: one conversation across both.
    messages[0].headers.in_reply_to = vec![format!("m{old}.1@example.test")];
    let stored = store.ingest(messages).recv_blocking().unwrap();
    let doomed: Vec<MessageId> = stored.ids[1..3].iter().flatten().copied().collect();
    let events = store.subscribe();
    let mut gone_view = model(&store, ViewSpec::folder(old));
    let mut inbox_view = model(&store, ViewSpec::folder(inbox));
    let mut unified = model(&store, ViewSpec::unified_inbox());

    let replaced = store
        .replace_folders(
            account,
            vec![
                listed(account, "INBOX", FolderRole::Inbox),
                listed(account, "Arkiv", FolderRole::Archive),
                listed(account, "Ny", FolderRole::Other),
            ],
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(&replaced.ids[..2], &[inbox, archive]);
    assert_eq!(
        (replaced.removed_folders, replaced.removed_messages),
        (1, 2)
    );
    assert_eq!(
        replaced.orphaned_blobs,
        vec![blob(2)],
        "blob 1 is still INBOX's"
    );
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.version, replaced.version);
    assert!(event.folders_changed);
    assert_eq!(event.removed, doomed);
    assert_eq!(event.threads.len(), 2, "{:?}", event.threads);
    for model in [&mut gone_view, &mut inbox_view, &mut unified] {
        model.apply(&store, &event);
        let page = whole(&store, model.view.spec());
        assert_eq!(page.rows, model.rows, "{:?}", model.view.spec());
    }
    assert!(gone_view.rows.is_empty());
    assert_eq!(inbox_view.rows[0].thread_count, 1);
    let folders = store.account_folders(account).recv_blocking().unwrap();
    let names: Vec<&str> = folders.iter().map(|f| f.name_raw.as_str()).collect();
    assert_eq!(names, ["INBOX", "Arkiv", "Ny"]);
    assert_eq!(
        store.account_folders(other).recv_blocking().unwrap().len(),
        1
    );
    assert_eq!(
        held(&store, foreign).len(),
        1,
        "another account's folder stays"
    );
    assert!(matches!(
        store.folder_messages(old).recv_blocking(),
        Ok(found) if found.is_empty()
    ));
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

#[test]
fn a_v2_store_migrates_and_complete_folders_get_floor_one() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(schema::V1).unwrap();
    conn.execute_batch(
        "ALTER TABLE accounts ADD COLUMN key TEXT;
         ALTER TABLE accounts ADD COLUMN detached INTEGER NOT NULL DEFAULT 0;
         CREATE UNIQUE INDEX accounts_key ON accounts(key);
         INSERT INTO schema_migrations VALUES (1, 0), (2, 0); PRAGMA user_version = 2;
         INSERT INTO accounts(name, address) VALUES ('Fixture', 'fixture@example.test');
         INSERT INTO folders(account_id, name, name_raw, role, sync_state, uidvalidity, \
           uidnext, sync_floor) VALUES (1, 'INBOX', 'INBOX', 'inbox', 'ready', 5, 31, 3);
         INSERT INTO folders(account_id, name, name_raw, role) VALUES (1, 'Arkiv', 'Arkiv', 'archive');",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let folders = store.account_folders(AccountId(1)).recv_blocking().unwrap();
    assert_eq!(
        folders
            .iter()
            .map(|f| (f.state, f.floor, f.below))
            .collect::<Vec<_>>(),
        [(SyncState::Ready, Some(1), 0), (SyncState::New, None, 0)]
    );
    assert!(folders[0].complete() && !folders[1].complete());
    store.close().unwrap();
}

#[test]
fn the_same_folder_list_again_publishes_nothing_but_new_counts() {
    let (_dir, store, account) = open();
    let mut list = vec![
        listed(account, "INBOX", FolderRole::Inbox),
        listed(account, "Arkiv", FolderRole::Archive),
    ];
    list[1].server_counts = Some(ServerCounts {
        total: 4,
        unread: 1,
    });
    let first = store
        .replace_folders(account, list.clone())
        .recv_blocking()
        .unwrap();
    let events = store.subscribe();
    let unified = model(&store, ViewSpec::unified_inbox());
    let again = store
        .replace_folders(account, list.clone())
        .recv_blocking()
        .unwrap();
    assert_eq!((again.version, &again.ids), (first.version, &first.ids));
    assert!(events.try_recv().is_err(), "no event for the same list");
    list[1].server_counts = Some(ServerCounts {
        total: 5,
        unread: 2,
    });
    let counted = store
        .replace_folders(account, list.clone())
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.version, counted.version);
    assert!(!event.folders_changed && event.views.is_empty());
    assert_eq!(
        event.folders,
        vec![FolderCounts {
            folder: first.ids[1],
            total: 5,
            unread: 2
        }]
    );
    list[1].attributes.push("\\Archive".into());
    store
        .replace_folders(account, list)
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    assert!(event.folders_changed, "a changed attribute is a new list");
    assert_eq!(event.views.len(), 1, "All Inboxes reloads");
    drop(unified);
    store.close().unwrap();
}
