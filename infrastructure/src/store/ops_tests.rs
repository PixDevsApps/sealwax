//! The op queue's store side (ML-051, ML-053): ops and overlays written together, exact change
//! events, the undo window and inverse ops, what the runner reads and records, sync next to
//! pending ops, UIDVALIDITY re-targeting, hidden `\Deleted` messages and local accounts.

use super::tests::{Model, scratch, whole};
use super::*;
use rusqlite::Connection;
use sealwax_domain::account::AccountKey;
use sealwax_domain::model::{Address, FolderRole, Headers};
use sealwax_domain::triage::Triage;
use std::collections::BTreeMap;

pub(super) const UIDVALIDITY: u32 = 7;
/// Unix ms.
pub(super) const NOW: i64 = 1_790_000_000_000;
pub(super) const WINDOW: i64 = 8_000;

pub(super) struct Fx {
    pub(super) _dir: tempfile::TempDir,
    pub(super) store: Store,
    pub(super) account: AccountId,
    pub(super) inbox: FolderId,
    pub(super) archive: FolderId,
    pub(super) trash: FolderId,
    pub(super) junk: FolderId,
    pub(super) projects: FolderId,
}

fn listed(account: AccountId, name: &str, role: FolderRole) -> FolderSpec {
    FolderSpec {
        attributes: vec!["\\HasNoChildren".into()],
        ..FolderSpec::local(account, name, role)
    }
}

pub(super) fn folder_list(account: AccountId) -> Vec<FolderSpec> {
    vec![
        listed(account, "INBOX", FolderRole::Inbox),
        listed(account, "Archive", FolderRole::Archive),
        listed(account, "Trash", FolderRole::Trash),
        listed(account, "Junk", FolderRole::Junk),
        listed(account, "Projects", FolderRole::Other),
    ]
}

/// Starts and finishes a first sync with nothing fetched, so the folder is `Ready`.
pub(super) fn ready(store: &Store, folder: FolderId, uidvalidity: u32) {
    store
        .first_sync_step(
            folder,
            FirstSyncStep::Start {
                uidvalidity,
                top: 1,
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

/// A server account (it has a key) with five synced, empty folders.
pub(super) fn server() -> Fx {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .ensure_account(
            AccountKey("ops-test".into()),
            NewAccount {
                name: "Ops".into(),
                address: "ops@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    let ids = store
        .upsert_folders(folder_list(account))
        .recv_blocking()
        .unwrap()
        .value;
    for id in &ids {
        ready(&store, *id, UIDVALIDITY);
    }
    Fx {
        _dir: dir,
        store,
        account,
        inbox: ids[0],
        archive: ids[1],
        trash: ids[2],
        junk: ids[3],
        projects: ids[4],
    }
}

/// A local account (no key): fixtures and self-tests.
pub(super) fn local() -> Fx {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Local".into(),
            address: "local@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let ids: Vec<FolderId> = folder_list(account)
        .into_iter()
        .map(|spec| {
            store
                .upsert_folder(FolderSpec {
                    attributes: Vec::new(),
                    ..spec
                })
                .recv_blocking()
                .unwrap()
                .value
        })
        .collect();
    Fx {
        _dir: dir,
        store,
        account,
        inbox: ids[0],
        archive: ids[1],
        trash: ids[2],
        junk: ids[3],
        projects: ids[4],
    }
}

/// The message with `uid`, its Message-ID the same wherever it is.
pub(super) fn message(folder: FolderId, uid: u32, flags: Flags) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        flags,
        date: 1_790_000_000 + i64::from(uid) * 60,
        headers: Headers {
            from: vec![Address::new(
                format!("Sender {uid}"),
                format!("s{uid}@example.test"),
            )],
            subject: format!("Message {uid}"),
            message_id: Some(format!("m{uid}@example.test")),
            ..Headers::default()
        },
        snippet: format!("Snippet {uid}"),
        search_text: format!("body of message{uid}"),
        has_attachments: uid.is_multiple_of(3),
        size: 100,
        ..NewMessage::default()
    }
}

/// New mail from the server; replies with the stored ids in the order given.
pub(super) fn arrive(store: &Store, folder: FolderId, messages: Vec<NewMessage>) -> Vec<MessageId> {
    let uids: Vec<u32> = messages.iter().map(|m| m.uid.unwrap()).collect();
    let top = uids.iter().max().copied().unwrap_or(0) + 1;
    store
        .sync_changes(
            folder,
            FolderChanges {
                new: messages,
                uidnext: Some(top),
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    let held = held_ids(store, folder);
    uids.iter().map(|uid| held[uid]).collect()
}

pub(super) fn held_ids(store: &Store, folder: FolderId) -> BTreeMap<u32, MessageId> {
    store
        .folder_messages(folder)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|stored| (stored.uid, stored.id))
        .collect()
}

/// The server's flags of a folder's messages, by UID.
pub(super) fn server_flags(store: &Store, folder: FolderId) -> BTreeMap<u32, Flags> {
    store
        .folder_messages(folder)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|stored| (stored.uid, stored.flags))
        .collect()
}

/// What the reader shows of a message.
pub(super) fn shown(store: &Store, id: MessageId) -> Option<Flags> {
    store
        .message(id)
        .recv_blocking()
        .unwrap()
        .value
        .map(|detail| detail.summary.flags)
}

pub(super) fn counts(store: &Store, folder: FolderId) -> (u32, u32) {
    let folders = store.folders().recv_blocking().unwrap().value;
    let found = folders.iter().find(|f| f.id == folder).unwrap();
    (found.total, found.unread)
}

fn listed_ids(store: &Store, spec: ViewSpec) -> Vec<MessageId> {
    whole(store, spec).rows.iter().map(|row| row.id).collect()
}

fn queue(store: &Store, action: Triage, messages: &[MessageId], not_before: i64) -> Queued {
    store
        .queue_triage(QueueTriage {
            action,
            messages: messages.to_vec(),
            not_before,
        })
        .recv_blocking()
        .unwrap()
}

pub(super) fn due(store: &Store, account: AccountId, now: i64) -> DueOps {
    store.due_ops(account, now).recv_blocking().unwrap()
}

/// Starts and records every group due at `now` as done, the moves with the UIDs `next_uid`
/// hands out.
fn run_all(fx: &Fx, now: i64, next_uid: &mut u32) -> Vec<OpGroup> {
    let groups = due(&fx.store, fx.account, now).groups;
    for group in &groups {
        fx.store
            .start_ops(group.ops.clone(), Some(*next_uid))
            .recv_blocking()
            .unwrap();
        let new_uids = match group.action {
            OpAction::Move { .. } => group
                .messages
                .iter()
                .map(|message| {
                    *next_uid += 1;
                    (message.uid, *next_uid - 1)
                })
                .collect(),
            _ => Vec::new(),
        };
        fx.store
            .ops_done(OpsDone {
                ops: group.ops.clone(),
                new_uids,
                ..OpsDone::default()
            })
            .recv_blocking()
            .unwrap();
    }
    groups
}

pub(super) fn clean(store: &Store) {
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

/// Open views fed by change events, compared with fresh reads after every write.
pub(super) struct Views {
    events: async_channel::Receiver<std::sync::Arc<ChangeEvent>>,
    models: Vec<Model>,
}

impl Views {
    pub(super) fn open(store: &Store, specs: &[ViewSpec]) -> Views {
        let events = store.subscribe();
        let models = specs
            .iter()
            .map(|spec| {
                let opened = store
                    .open_view(OpenView {
                        spec: *spec,
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
            })
            .collect();
        Views { events, models }
    }

    /// Applies every event so far; every view must equal a fresh read. Replies with the events.
    pub(super) fn settle(&mut self, store: &Store) -> Vec<std::sync::Arc<ChangeEvent>> {
        let mut seen = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            for model in &mut self.models {
                if event.version > model.version {
                    model.apply(store, &event);
                }
            }
            seen.push(event);
        }
        for model in &self.models {
            let fresh = whole(store, model.view.spec());
            assert_eq!(
                model
                    .rows
                    .iter()
                    .map(|r| (r.id, r.flags))
                    .collect::<Vec<_>>(),
                fresh
                    .rows
                    .iter()
                    .map(|r| (r.id, r.flags))
                    .collect::<Vec<_>>(),
                "{:?} follows its events",
                model.view.spec()
            );
        }
        seen
    }
}

fn all_views(fx: &Fx) -> Vec<ViewSpec> {
    let mut specs = Vec::new();
    for folder in [fx.inbox, fx.archive, fx.trash, fx.junk, fx.projects] {
        for filter in [
            ViewFilter::All,
            ViewFilter::Unread,
            ViewFilter::Starred,
            ViewFilter::Attachments,
        ] {
            specs.push(ViewSpec::folder(folder).with_filter(filter));
        }
    }
    specs.push(ViewSpec::unified_inbox());
    specs.push(ViewSpec::unified_inbox().with_filter(ViewFilter::Unread));
    specs
}

pub(super) fn ten(fx: &Fx) -> Vec<MessageId> {
    arrive(
        &fx.store,
        fx.inbox,
        (1..=10)
            .map(|uid| {
                let flags = if uid % 2 == 0 {
                    Flags::SEEN
                } else {
                    Flags::NONE
                };
                message(fx.inbox, uid, flags)
            })
            .collect(),
    )
}

#[test]
fn marking_read_shows_at_once_and_keeps_the_servers_flags() {
    let fx = server();
    let ids = ten(&fx);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    assert_eq!(counts(&fx.store, fx.inbox), (10, 5));
    let queued = queue(&fx.store, Triage::Read(true), &ids[..4], NOW);
    assert_eq!(queued.applied, 4);
    assert_eq!(queued.accounts, vec![fx.account]);
    assert!(queued.batch.is_some());
    let events = views.settle(&fx.store);
    assert_eq!(events.len(), 1, "one transaction, one event");
    let event = &events[0];
    assert_eq!(event.version, queued.version);
    assert_eq!(
        event.changed,
        vec![ids[0], ids[2]],
        "only the unread ones change"
    );
    assert_eq!(
        event.folders,
        vec![FolderCounts {
            folder: fx.inbox,
            total: 10,
            unread: 3
        }]
    );
    assert_eq!(counts(&fx.store, fx.inbox), (10, 3));
    assert_eq!(shown(&fx.store, ids[0]), Some(Flags::SEEN));
    // The sync engine still sees the server's flags.
    assert_eq!(server_flags(&fx.store, fx.inbox)[&1], Flags::NONE);
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 2);
    clean(&fx.store);

    // Already read: applied, no op, no event.
    let again = queue(&fx.store, Triage::Read(true), &ids[..2], NOW);
    assert_eq!((again.applied, again.batch), (2, None));
    assert_eq!(again.version, queued.version);
    assert!(views.settle(&fx.store).is_empty());
    fx.store.close().unwrap();
}

#[test]
fn archive_hides_at_once_from_every_view_count_and_search() {
    let fx = server();
    let ids = ten(&fx);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    let queued = queue(&fx.store, Triage::Archive, &ids[2..5], NOW + WINDOW);
    assert_eq!(queued.applied, 3);
    let events = views.settle(&fx.store);
    assert_eq!(events.len(), 1);
    assert_eq!(counts(&fx.store, fx.inbox), (7, 3));
    assert_eq!(
        counts(&fx.store, fx.archive),
        (0, 0),
        "counted once its row is there"
    );
    let inbox = listed_ids(&fx.store, ViewSpec::folder(fx.inbox));
    assert!(ids[2..5].iter().all(|id| !inbox.contains(id)));
    assert!(listed_ids(&fx.store, ViewSpec::folder(fx.archive)).is_empty());
    let found = fx
        .store
        .search("message3", None, 10)
        .recv_blocking()
        .unwrap();
    assert!(found.value.is_empty(), "hidden rows are not found");
    assert_eq!(
        fx.store
            .search("message6", Some(fx.inbox), 10)
            .recv_blocking()
            .unwrap()
            .value
            .len(),
        1
    );
    // The reader can still show it; the server still has it in INBOX.
    assert!(shown(&fx.store, ids[2]).is_some());
    assert_eq!(server_flags(&fx.store, fx.inbox).len(), 10);
    let state = fx
        .store
        .folder_sync(fx.inbox)
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert_eq!(
        state.messages, 10,
        "the sync engine counts what the server holds"
    );
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn undo_within_the_window_cancels_locally_and_puts_rows_back() {
    let fx = server();
    let ids = ten(&fx);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    let before = listed_ids(&fx.store, ViewSpec::folder(fx.inbox));
    let read = queue(&fx.store, Triage::Read(true), &ids[..1], NOW);
    let archived = queue(&fx.store, Triage::Archive, &ids[..5], NOW + WINDOW);
    views.settle(&fx.store);
    assert_eq!(
        due(&fx.store, fx.account, NOW).groups.len(),
        1,
        "only the read"
    );

    let undone = fx
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!((undone.restored, undone.reverted), (5, 0));
    assert!(undone.lost.is_empty());
    assert!(undone.accounts.is_empty(), "nothing new to run");
    let events = views.settle(&fx.store);
    assert_eq!(events.len(), 1);
    assert_eq!(listed_ids(&fx.store, ViewSpec::folder(fx.inbox)), before);
    assert_eq!(counts(&fx.store, fx.inbox), (10, 4), "the read stays");
    assert_eq!(shown(&fx.store, ids[0]), Some(Flags::SEEN));
    let due_now = due(&fx.store, fx.account, NOW + WINDOW);
    assert_eq!(due_now.groups.len(), 1);
    assert!(matches!(due_now.groups[0].action, OpAction::Flags { .. }));

    // Undoing twice does nothing.
    let again = fx
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!((again.restored, again.reverted), (0, 0));
    let read_undone = fx.store.undo(read.batch.unwrap()).recv_blocking().unwrap();
    assert_eq!(read_undone.restored, 1);
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.inbox), (10, 5));
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn due_ops_wait_for_the_window_and_group_into_uid_sets() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Archive, &[ids[4], ids[1]], NOW + WINDOW);
    queue(&fx.store, Triage::Star(true), &ids[6..8], NOW);
    let early = due(&fx.store, fx.account, NOW);
    assert_eq!(early.groups.len(), 1);
    let star = &early.groups[0];
    assert_eq!(
        star.action,
        OpAction::Flags {
            add: Flags::FLAGGED,
            remove: Flags::NONE
        }
    );
    assert_eq!(
        (star.folder, star.folder_name.as_str()),
        (fx.inbox, "INBOX")
    );
    assert_eq!(star.uidvalidity, Some(UIDVALIDITY));
    assert_eq!(
        star.messages.iter().map(|m| m.uid).collect::<Vec<_>>(),
        vec![7, 8]
    );
    assert!(!star.retry);
    assert_eq!(early.next, Some(NOW + WINDOW));

    let later = due(&fx.store, fx.account, NOW + WINDOW);
    assert_eq!(later.groups.len(), 2);
    let archive = later
        .groups
        .iter()
        .find(|group| matches!(group.action, OpAction::Move { .. }))
        .unwrap();
    assert_eq!(
        archive.action,
        OpAction::Move {
            to: fx.archive,
            to_name: "Archive".into(),
            to_uidvalidity: Some(UIDVALIDITY),
            add: Flags::NONE,
            remove: Flags::NONE,
        }
    );
    assert_eq!(
        archive
            .messages
            .iter()
            .map(|m| (m.uid, m.message_id.clone()))
            .collect::<Vec<_>>(),
        vec![
            (2, Some("m2@example.test".into())),
            (5, Some("m5@example.test".into()))
        ]
    );
    assert_eq!(later.next, None);
    fx.store.close().unwrap();
}

#[test]
fn flag_changes_merge_and_move_chains_collapse() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Read(true), &ids[..1], NOW);
    queue(&fx.store, Triage::Star(true), &ids[..1], NOW);
    // Archive, then Delete before the change event reached the list: Archive to Trash.
    queue(&fx.store, Triage::Archive, &ids[2..3], NOW);
    let deleted = queue(&fx.store, Triage::Delete, &ids[2..3], NOW);
    assert_eq!(deleted.applied, 1, "planned from where it is going");
    // To Projects and back again is nothing.
    queue(&fx.store, Triage::MoveTo(fx.projects), &ids[4..5], NOW);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    let back = queue(&fx.store, Triage::MoveTo(fx.inbox), &ids[4..5], NOW);
    assert_eq!(back.applied, 1);
    views.settle(&fx.store);
    assert!(listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&ids[4]));

    let groups = due(&fx.store, fx.account, NOW).groups;
    let actions: Vec<(OpAction, Vec<u32>, usize)> = groups
        .iter()
        .map(|g| {
            (
                g.action.clone(),
                g.messages.iter().map(|m| m.uid).collect(),
                g.ops.len(),
            )
        })
        .collect();
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert!(actions.contains(&(
        OpAction::Flags {
            add: Flags::SEEN | Flags::FLAGGED,
            remove: Flags::NONE
        },
        vec![1],
        2
    )));
    assert!(actions.contains(&(
        OpAction::Move {
            to: fx.trash,
            to_name: "Trash".into(),
            to_uidvalidity: Some(UIDVALIDITY),
            add: Flags::NONE,
            remove: Flags::NONE
        },
        vec![3],
        2
    )));
    assert_eq!(
        fx.store.pending_ops(fx.account).recv_blocking().unwrap(),
        4,
        "the round trip came to nothing and is done"
    );
    views.settle(&fx.store);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn done_ops_become_the_servers_state() {
    let fx = server();
    let ids = ten(&fx);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    queue(&fx.store, Triage::Read(true), &ids[..2], NOW);
    queue(&fx.store, Triage::Archive, &ids[3..5], NOW);
    views.settle(&fx.store);
    let before_archive = fx
        .store
        .message(ids[3])
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    let mut next_uid = 100;
    let groups = run_all(&fx, NOW, &mut next_uid);
    assert_eq!(groups.len(), 2);
    let events = views.settle(&fx.store);
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    // Flags written as the server's; no overlay left.
    assert_eq!(server_flags(&fx.store, fx.inbox)[&1], Flags::SEEN);
    assert_eq!(shown(&fx.store, ids[0]), Some(Flags::SEEN));
    // The moved rows re-homed under their new UIDs, same ids, in the archive's list.
    let archived = held_ids(&fx.store, fx.archive);
    assert_eq!(archived.len(), 2);
    assert_eq!(archived[&100], ids[3]);
    assert_eq!(archived[&101], ids[4]);
    assert!(!held_ids(&fx.store, fx.inbox).contains_key(&4));
    assert_eq!(
        listed_ids(&fx.store, ViewSpec::folder(fx.archive)),
        vec![ids[4], ids[3]]
    );
    assert_eq!(counts(&fx.store, fx.archive), (2, 1));
    assert_eq!(counts(&fx.store, fx.inbox), (8, 3));
    let after = fx
        .store
        .message(ids[3])
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(after.headers, before_archive.headers, "kept, not refetched");
    assert!(events.iter().any(|event| event.changed.contains(&ids[3])));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_move_without_a_new_uid_leaves_the_message_to_the_targets_sync() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Archive, &ids[..1], NOW);
    let group = due(&fx.store, fx.account, NOW).groups.remove(0);
    fx.store
        .start_ops(group.ops.clone(), Some(40))
        .recv_blocking()
        .unwrap();
    let recorded = fx
        .store
        .ops_done(OpsDone {
            ops: group.ops,
            ..OpsDone::default()
        })
        .recv_blocking()
        .unwrap();
    assert!(recorded.orphaned_blobs.is_empty());
    assert!(!held_ids(&fx.store, fx.inbox).contains_key(&1));
    assert!(held_ids(&fx.store, fx.archive).is_empty());
    arrive(
        &fx.store,
        fx.archive,
        vec![message(fx.archive, 40, Flags::NONE)],
    );
    assert_eq!(counts(&fx.store, fx.archive), (1, 1));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_move_the_targets_sync_stored_first_is_not_duplicated() {
    let fx = server();
    let ids = ten(&fx);
    let queued = queue(&fx.store, Triage::Archive, &ids[..1], NOW);
    let group = due(&fx.store, fx.account, NOW).groups.remove(0);
    fx.store
        .start_ops(group.ops.clone(), Some(40))
        .recv_blocking()
        .unwrap();
    // The archive's pass ran between the MOVE and its record.
    let copy = arrive(
        &fx.store,
        fx.archive,
        vec![message(fx.archive, 40, Flags::NONE)],
    )[0];
    let mut views = Views::open(&fx.store, &all_views(&fx));
    fx.store
        .ops_done(OpsDone {
            ops: group.ops,
            new_uids: vec![(1, 40)],
            target_uidvalidity: Some(UIDVALIDITY),
            ..OpsDone::default()
        })
        .recv_blocking()
        .unwrap();
    views.settle(&fx.store);
    assert_eq!(
        held_ids(&fx.store, fx.archive),
        BTreeMap::from([(40, copy)])
    );
    assert!(!held_ids(&fx.store, fx.inbox).contains_key(&1));
    assert!(shown(&fx.store, ids[0]).is_none());
    // Undo finds the copy.
    let undone = fx
        .store
        .undo(queued.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.reverted, 1);
    assert!(!listed_ids(&fx.store, ViewSpec::folder(fx.archive)).contains(&copy));
    let back = due(&fx.store, fx.account, NOW).groups.remove(0);
    assert_eq!(back.folder, fx.archive);
    assert!(matches!(back.action, OpAction::Move { to, .. } if to == fx.inbox));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn deleting_for_good_removes_the_row_or_leaves_it_flagged_and_hidden() {
    let fx = server();
    let ids = arrive(
        &fx.store,
        fx.trash,
        (1..=3)
            .map(|uid| message(fx.trash, uid, Flags::NONE))
            .collect(),
    );
    let mut views = Views::open(&fx.store, &all_views(&fx));
    let deleted = queue(&fx.store, Triage::Delete, &ids, NOW);
    assert_eq!(deleted.applied, 3);
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.trash), (0, 0));
    let groups = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].action, OpAction::Expunge);
    let (first, rest): (Vec<_>, Vec<_>) = groups[0]
        .messages
        .iter()
        .partition(|message| message.uid == 1);
    // UIDPLUS for the first: the row goes.
    fx.store
        .ops_done(OpsDone {
            ops: first[0].ops.clone(),
            ..OpsDone::default()
        })
        .recv_blocking()
        .unwrap();
    assert!(!held_ids(&fx.store, fx.trash).contains_key(&1));
    // No UIDPLUS for the others: flagged `\Deleted`, kept, hidden.
    fx.store
        .ops_done(OpsDone {
            ops: rest.iter().flat_map(|m| m.ops.clone()).collect(),
            flagged_only: true,
            ..OpsDone::default()
        })
        .recv_blocking()
        .unwrap();
    views.settle(&fx.store);
    assert_eq!(server_flags(&fx.store, fx.trash)[&2], Flags::DELETED);
    assert_eq!(counts(&fx.store, fx.trash), (0, 0));
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    // Undo: the expunged one is lost; the flagged ones get `-FLAGS \Deleted`.
    let undone = fx
        .store
        .undo(deleted.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.reverted, 2);
    assert_eq!(undone.lost, vec![(Lost::Deleted, 1)]);
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.trash), (2, 2));
    let back = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(
        back[0].action,
        OpAction::Flags {
            add: Flags::NONE,
            remove: Flags::DELETED
        }
    );
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn failures_keep_the_overlay_or_put_messages_back() {
    let fx = server();
    let ids = ten(&fx);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    queue(&fx.store, Triage::Archive, &ids[..2], NOW);
    queue(&fx.store, Triage::Star(true), &ids[5..6], NOW);
    views.settle(&fx.store);
    let groups = due(&fx.store, fx.account, NOW).groups;
    let all: Vec<OpId> = groups.iter().flat_map(|g| g.ops.clone()).collect();
    fx.store
        .start_ops(all.clone(), Some(30))
        .recv_blocking()
        .unwrap();
    let transient = fx
        .store
        .ops_failed(
            all.clone(),
            Failure::Transient {
                retry_at: Some(NOW + 1_000),
            },
        )
        .recv_blocking()
        .unwrap();
    assert!(transient.refused.is_empty());
    assert!(views.settle(&fx.store).is_empty(), "nothing shown changes");
    let waiting = due(&fx.store, fx.account, NOW);
    assert!(waiting.groups.is_empty());
    assert_eq!(waiting.next, Some(NOW + 1_000));
    let retried = due(&fx.store, fx.account, NOW + 1_000).groups;
    assert_eq!(retried.len(), 2);
    assert!(retried.iter().all(|g| g.retry && g.attempts == 1));
    let moved = retried
        .iter()
        .find(|g| matches!(g.action, OpAction::Move { .. }))
        .unwrap();
    assert_eq!(
        moved.target_uidnext,
        Some(30),
        "kept from the first attempt"
    );

    let refused = fx
        .store
        .ops_failed(all, Failure::Permanent)
        .recv_blocking()
        .unwrap();
    let mut kinds: Vec<(RefusedChange, u32)> = refused
        .refused
        .iter()
        .map(|r| {
            assert_eq!(r.account, fx.account);
            (r.change, r.messages)
        })
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        vec![(RefusedChange::Flags, 1), (RefusedChange::Move, 2)]
    );
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.inbox), (10, 5));
    assert!(listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&ids[0]));
    assert_eq!(shown(&fx.store, ids[5]), Some(Flags::SEEN));
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn undo_after_the_window_queues_inverse_ops_from_the_state_at_queue_time() {
    let fx = server();
    let ids = ten(&fx);
    // ids[1] is read already: marking it read changed nothing to put back.
    let read = queue(&fx.store, Triage::Read(true), &ids[..2], NOW);
    let starred = queue(&fx.store, Triage::Star(true), &ids[..1], NOW);
    let archived = queue(&fx.store, Triage::Archive, &ids[4..6], NOW);
    let mut next_uid = 200;
    run_all(&fx, NOW, &mut next_uid);
    let mut views = Views::open(&fx.store, &all_views(&fx));

    let undone = fx.store.undo(read.batch.unwrap()).recv_blocking().unwrap();
    assert_eq!((undone.restored, undone.reverted), (0, 1));
    assert_eq!(undone.accounts, vec![fx.account]);
    views.settle(&fx.store);
    assert_eq!(
        shown(&fx.store, ids[0]),
        Some(Flags::FLAGGED),
        "unread, still starred"
    );
    let back = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(back.len(), 1, "no window");
    assert_eq!(
        back[0].action,
        OpAction::Flags {
            add: Flags::NONE,
            remove: Flags::SEEN
        }
    );

    let undone = fx
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.reverted, 2);
    views.settle(&fx.store);
    assert!(listed_ids(&fx.store, ViewSpec::folder(fx.archive)).is_empty());
    assert!(!listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&ids[4]));
    run_all(&fx, NOW, &mut next_uid);
    views.settle(&fx.store);
    let inbox = listed_ids(&fx.store, ViewSpec::folder(fx.inbox));
    assert!(inbox.contains(&ids[4]) && inbox.contains(&ids[5]));
    assert_eq!(server_flags(&fx.store, fx.inbox)[&1], Flags::FLAGGED);
    let _ = starred;
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn undo_of_ops_in_flight_queues_behind_them() {
    let fx = server();
    let ids = ten(&fx);
    let archived = queue(&fx.store, Triage::Archive, &ids[..2], NOW);
    let group = due(&fx.store, fx.account, NOW).groups.remove(0);
    fx.store
        .start_ops(group.ops.clone(), Some(50))
        .recv_blocking()
        .unwrap();
    let undone = fx
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!((undone.restored, undone.reverted), (0, 2));
    assert!(listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&ids[0]));
    // The started move is due again on its own; the inverse waits behind it.
    let again = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(again.len(), 1);
    assert!(again[0].retry);
    assert_eq!(again[0].ops, group.ops);
    // The server refused it: the inverse comes to nothing.
    let (first, second) = (&group.messages[0], &group.messages[1]);
    fx.store
        .ops_failed(first.ops.clone(), Failure::Permanent)
        .recv_blocking()
        .unwrap();
    // The other one went through.
    fx.store
        .ops_done(OpsDone {
            ops: second.ops.clone(),
            new_uids: vec![(second.uid, 50)],
            ..OpsDone::default()
        })
        .recv_blocking()
        .unwrap();
    let next = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(next.len(), 1, "{next:?}");
    assert_eq!(next[0].folder, fx.archive);
    assert_eq!(next[0].messages[0].uid, 50);
    assert!(matches!(next[0].action, OpAction::Move { to, .. } if to == fx.inbox));
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 1);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn sync_writes_the_servers_flags_under_a_pending_overlay() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Read(true), &ids[..1], NOW);
    queue(&fx.store, Triage::Archive, &ids[2..3], NOW);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    // Another client stars both; the server still has them unread and in INBOX.
    let synced = fx
        .store
        .sync_changes(
            fx.inbox,
            FolderChanges {
                flags: vec![
                    FlagUpdate {
                        uid: 1,
                        flags: Flags::FLAGGED,
                        keywords: Vec::new(),
                    },
                    FlagUpdate {
                        uid: 3,
                        flags: Flags::FLAGGED,
                        keywords: Vec::new(),
                    },
                ],
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(synced.changed, 2);
    views.settle(&fx.store);
    assert_eq!(shown(&fx.store, ids[0]), Some(Flags::SEEN | Flags::FLAGGED));
    assert_eq!(server_flags(&fx.store, fx.inbox)[&1], Flags::FLAGGED);
    assert!(!listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&ids[2]));
    assert_eq!(counts(&fx.store, fx.inbox), (9, 3));
    // The same flags again change nothing.
    let quiet = fx
        .store
        .sync_changes(
            fx.inbox,
            FolderChanges {
                flags: vec![FlagUpdate {
                    uid: 1,
                    flags: Flags::FLAGGED,
                    keywords: Vec::new(),
                }],
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    assert_eq!((quiet.changed, quiet.version), (0, synced.version));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_message_that_vanishes_under_a_pending_move_is_done() {
    let fx = server();
    let ids = ten(&fx);
    let archived = queue(&fx.store, Triage::Archive, &ids[..2], NOW);
    queue(&fx.store, Triage::Read(true), &ids[4..5], NOW);
    // The MOVE ran, the record didn't (a crash): the next pass sees UIDs 1 and 2 gone, and 5
    // expunged by another client.
    let mut views = Views::open(&fx.store, &all_views(&fx));
    fx.store
        .sync_changes(
            fx.inbox,
            FolderChanges {
                removed: vec![1, 2, 5],
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    views.settle(&fx.store);
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    let after = due(&fx.store, fx.account, NOW);
    assert!(after.groups.is_empty());
    assert!(after.refused.is_empty(), "done, not refused");
    // Undo now needs the archive's copy, which the archive's sync brings.
    arrive(
        &fx.store,
        fx.archive,
        vec![NewMessage {
            uid: Some(60),
            ..message(fx.archive, 1, Flags::NONE)
        }],
    );
    let undone = fx
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.reverted, 1);
    assert_eq!(undone.lost, vec![(Lost::Unknown, 1)]);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn ops_behind_a_move_that_vanished_are_refused() {
    let fx = server();
    let ids = ten(&fx);
    let archived = queue(&fx.store, Triage::Archive, &ids[..1], NOW);
    let group = due(&fx.store, fx.account, NOW).groups.remove(0);
    fx.store.start_ops(group.ops, None).recv_blocking().unwrap();
    fx.store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    fx.store
        .sync_changes(
            fx.inbox,
            FolderChanges {
                removed: vec![1],
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    let after = due(&fx.store, fx.account, NOW);
    assert!(after.groups.is_empty());
    assert_eq!(
        after.refused,
        vec![OpsRefusal {
            account: fx.account,
            change: RefusedChange::Move,
            messages: 1
        }]
    );
    assert!(
        due(&fx.store, fx.account, NOW).refused.is_empty(),
        "taken once"
    );
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_folder_that_disappears_takes_its_ops_with_it() {
    let fx = server();
    let ids = ten(&fx);
    let projects = arrive(
        &fx.store,
        fx.projects,
        vec![message(fx.projects, 1, Flags::NONE)],
    );
    queue(
        &fx.store,
        Triage::MoveTo(fx.projects),
        &ids[..2],
        NOW + WINDOW,
    );
    queue(&fx.store, Triage::Star(true), &projects, NOW);
    queue(&fx.store, Triage::Read(true), &ids[2..3], NOW);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    let list: Vec<FolderSpec> = folder_list(fx.account)
        .into_iter()
        .filter(|spec| spec.name != "Projects")
        .collect();
    let replaced = fx
        .store
        .replace_folders(fx.account, list)
        .recv_blocking()
        .unwrap();
    assert_eq!(replaced.removed_folders, 1);
    let events = views.settle(&fx.store);
    assert_eq!(events.len(), 1);
    assert!(events[0].changed.contains(&ids[0]));
    let inbox = listed_ids(&fx.store, ViewSpec::folder(fx.inbox));
    assert!(
        inbox.contains(&ids[0]) && inbox.contains(&ids[1]),
        "back where they are"
    );
    assert_eq!(counts(&fx.store, fx.inbox), (10, 4));
    let mut refused = due(&fx.store, fx.account, NOW + WINDOW).refused;
    refused.sort_by_key(|r| r.change);
    assert_eq!(
        refused
            .iter()
            .map(|r| (r.change, r.messages))
            .collect::<Vec<_>>(),
        vec![(RefusedChange::Flags, 1), (RefusedChange::Move, 2)]
    );
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 1);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn ops_survive_a_uidvalidity_change_and_find_their_messages_again() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Star(true), &ids[..3], NOW);
    queue(&fx.store, Triage::Archive, &ids[3..4], NOW);
    // UID 5 has no Message-ID: it can't be found again.
    let anonymous = arrive(
        &fx.store,
        fx.inbox,
        vec![NewMessage {
            headers: Headers {
                message_id: None,
                ..message(fx.inbox, 11, Flags::NONE).headers
            },
            ..message(fx.inbox, 11, Flags::NONE)
        }],
    );
    queue(&fx.store, Triage::Read(true), &anonymous, NOW);
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 5);

    fx.store
        .reset_folder(fx.inbox, UIDVALIDITY + 1)
        .recv_blocking()
        .unwrap();
    assert!(held_ids(&fx.store, fx.inbox).is_empty());
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 4);
    let waiting = due(&fx.store, fx.account, NOW);
    assert!(waiting.groups.is_empty(), "waiting");
    assert_eq!(
        waiting.refused,
        vec![OpsRefusal {
            account: fx.account,
            change: RefusedChange::Flags,
            messages: 1
        }],
        "the read of the message without a Message-ID, at once"
    );
    clean(&fx.store);

    // The first sync under the new UIDVALIDITY renumbers: m1 is UID 101 now, m2 102 and m4 104;
    // m3 doesn't come back.
    let mut views = Views::open(&fx.store, &all_views(&fx));
    fx.store
        .first_sync_step(
            fx.inbox,
            FirstSyncStep::Start {
                uidvalidity: UIDVALIDITY + 1,
                top: 200,
                modseq: 1,
            },
        )
        .recv_blocking()
        .unwrap();
    let renumbered: Vec<NewMessage> = [1, 2, 4, 6]
        .into_iter()
        .map(|old| NewMessage {
            uid: Some(100 + old),
            ..message(fx.inbox, old, Flags::NONE)
        })
        .collect();
    fx.store.ingest(renumbered).recv_blocking().unwrap();
    views.settle(&fx.store);
    let held = held_ids(&fx.store, fx.inbox);
    assert_eq!(shown(&fx.store, held[&101]), Some(Flags::FLAGGED));
    assert_eq!(shown(&fx.store, held[&106]), Some(Flags::NONE));
    assert!(
        !listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&held[&104]),
        "the pending archive hides it again"
    );
    let groups = due(&fx.store, fx.account, NOW).groups;
    let star = groups
        .iter()
        .find(|g| matches!(g.action, OpAction::Flags { .. }))
        .unwrap();
    assert_eq!(star.uidvalidity, Some(UIDVALIDITY + 1));
    assert_eq!(
        star.messages.iter().map(|m| m.uid).collect::<Vec<_>>(),
        vec![101, 102]
    );
    clean(&fx.store);

    fx.store
        .first_sync_step(fx.inbox, FirstSyncStep::Finish { below: 0 })
        .recv_blocking()
        .unwrap();
    let after = due(&fx.store, fx.account, NOW);
    let mut refused: Vec<(RefusedChange, u32)> = after
        .refused
        .iter()
        .map(|r| (r.change, r.messages))
        .collect();
    refused.sort();
    assert_eq!(
        refused,
        vec![(RefusedChange::Flags, 1)],
        "m3's star, at the end of the first sync"
    );
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 3);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn deleted_messages_are_hidden_whoever_flagged_them() {
    let fx = server();
    let ids = ten(&fx);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    fx.store
        .sync_changes(
            fx.inbox,
            FolderChanges {
                flags: vec![FlagUpdate {
                    uid: 3,
                    flags: Flags::DELETED | Flags::FLAGGED,
                    keywords: Vec::new(),
                }],
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    views.settle(&fx.store);
    assert!(!listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&ids[2]));
    assert!(
        listed_ids(
            &fx.store,
            ViewSpec::folder(fx.inbox).with_filter(ViewFilter::Starred)
        )
        .is_empty()
    );
    assert_eq!(counts(&fx.store, fx.inbox), (9, 4));
    assert!(
        fx.store
            .search("message3", None, 10)
            .recv_blocking()
            .unwrap()
            .value
            .is_empty()
    );
    let state = fx
        .store
        .folder_sync(fx.inbox)
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert_eq!(state.messages, 10, "kept, so the store equals the server");
    // A message arriving `\Deleted` is hidden from the start.
    arrive(
        &fx.store,
        fx.inbox,
        vec![message(fx.inbox, 11, Flags::DELETED)],
    );
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.inbox), (9, 4));
    // Undeleted elsewhere: back.
    fx.store
        .set_flags(vec![ids[2]], Flags::NONE, Flags::DELETED)
        .recv_blocking()
        .unwrap();
    views.settle(&fx.store);
    assert!(listed_ids(&fx.store, ViewSpec::folder(fx.inbox)).contains(&ids[2]));
    assert_eq!(counts(&fx.store, fx.inbox), (10, 5));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn local_accounts_apply_at_once_and_undo_at_once() {
    let fx = local();
    let ids = ingest_local(&fx);
    let mut views = Views::open(&fx.store, &all_views(&fx));
    let read = queue(&fx.store, Triage::Read(true), &ids[..2], NOW + WINDOW);
    assert!(read.accounts.is_empty(), "nothing for an op runner");
    let archived = queue(&fx.store, Triage::Archive, &ids[2..4], NOW + WINDOW);
    views.settle(&fx.store);
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    assert_eq!(shown(&fx.store, ids[0]), Some(Flags::SEEN));
    assert_eq!(
        server_flags(&fx.store, fx.inbox)[&1],
        Flags::SEEN,
        "no server"
    );
    let archive = held_ids(&fx.store, fx.archive);
    assert_eq!(
        archive.values().copied().collect::<Vec<_>>(),
        vec![ids[2], ids[3]]
    );
    assert_eq!(archive.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(counts(&fx.store, fx.archive), (2, 2));

    let trashed = queue(&fx.store, Triage::Delete, &ids[4..5], NOW);
    let gone = queue(&fx.store, Triage::Delete, &ids[4..5], NOW);
    assert_eq!(gone.applied, 1, "from the Trash, for good");
    views.settle(&fx.store);
    assert!(shown(&fx.store, ids[4]).is_none());

    let undone = fx
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!((undone.restored, undone.reverted), (0, 2));
    assert!(undone.accounts.is_empty());
    views.settle(&fx.store);
    assert!(held_ids(&fx.store, fx.archive).is_empty());
    assert_eq!(held_ids(&fx.store, fx.inbox).len(), 9);
    let lost = fx.store.undo(gone.batch.unwrap()).recv_blocking().unwrap();
    assert_eq!(lost.lost, vec![(Lost::Deleted, 1)]);
    let _ = trashed;
    let undone = fx.store.undo(read.batch.unwrap()).recv_blocking().unwrap();
    assert_eq!(undone.reverted, 2);
    views.settle(&fx.store);
    assert_eq!(shown(&fx.store, ids[0]), Some(Flags::NONE));
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

pub(super) fn ingest_local(fx: &Fx) -> Vec<MessageId> {
    fx.store
        .ingest(
            (1..=10)
                .map(|n| NewMessage {
                    uid: None,
                    ..message(fx.inbox, n, Flags::NONE)
                })
                .collect(),
        )
        .recv_blocking()
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

#[test]
fn queue_reports_what_it_left_alone() {
    let fx = server();
    let ids = ten(&fx);
    let archived = arrive(
        &fx.store,
        fx.archive,
        vec![message(fx.archive, 1, Flags::NONE)],
    );
    let mut all = ids[..2].to_vec();
    all.extend(&archived);
    all.push(MessageId(99_999));
    let queued = queue(&fx.store, Triage::Archive, &all, NOW);
    assert_eq!(queued.applied, 2);
    assert_eq!(
        queued.not_applied,
        vec![(NotApplied::AlreadyThere, 1), (NotApplied::Gone, 1)]
    );
    // An expunge pending: gone for further actions.
    let trash = arrive(&fx.store, fx.trash, vec![message(fx.trash, 1, Flags::NONE)]);
    queue(&fx.store, Triage::Delete, &trash, NOW);
    let after = queue(&fx.store, Triage::Read(true), &trash, NOW);
    assert_eq!(after.not_applied, vec![(NotApplied::Gone, 1)]);
    // No Junk folder in this list of an account: nothing to junk to.
    fx.store
        .replace_folders(
            fx.account,
            folder_list(fx.account)
                .into_iter()
                .filter(|spec| spec.name != "Junk")
                .collect(),
        )
        .recv_blocking()
        .unwrap();
    let junk = queue(&fx.store, Triage::Junk, &ids[5..6], NOW);
    assert_eq!(junk.not_applied, vec![(NotApplied::NoTarget, 1)]);
    let _ = fx.junk;
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn junk_and_not_junk_carry_their_keywords() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Junk, &ids[..1], NOW);
    let groups = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(
        groups[0].action,
        OpAction::Move {
            to: fx.junk,
            to_name: "Junk".into(),
            to_uidvalidity: Some(UIDVALIDITY),
            add: Flags::JUNK,
            remove: Flags::NOT_JUNK,
        }
    );
    let mut next_uid = 70;
    run_all(&fx, NOW, &mut next_uid);
    assert_eq!(server_flags(&fx.store, fx.junk)[&70], Flags::JUNK);
    let in_junk = held_ids(&fx.store, fx.junk)[&70];
    queue(&fx.store, Triage::Junk, &[in_junk], NOW);
    run_all(&fx, NOW, &mut next_uid);
    assert_eq!(server_flags(&fx.store, fx.inbox)[&71], Flags::NOT_JUNK);
    clean(&fx.store);
    fx.store.close().unwrap();
}

/// A first sync that kept UIDs 801–1000 (every third unread) and left 800 below the floor with
/// 300 of them unread.
fn windowed(fx: &Fx) {
    let store = &fx.store;
    store
        .reset_folder(fx.inbox, UIDVALIDITY)
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(
            fx.inbox,
            FirstSyncStep::Start {
                uidvalidity: UIDVALIDITY,
                top: 1_001,
                modseq: 50,
            },
        )
        .recv_blocking()
        .unwrap();
    store
        .ingest(
            (801..=1_000)
                .map(|uid| {
                    let flags = if uid % 3 == 0 {
                        Flags::NONE
                    } else {
                        Flags::SEEN
                    };
                    message(fx.inbox, uid, flags)
                })
                .collect(),
        )
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(fx.inbox, FirstSyncStep::Floor(801))
        .recv_blocking()
        .unwrap();
    store
        .first_sync_step(fx.inbox, FirstSyncStep::Finish { below: 800 })
        .recv_blocking()
        .unwrap();
    store
        .count_below(
            fx.inbox,
            ServerStatus {
                uidvalidity: UIDVALIDITY,
                uidnext: 1_001,
                highestmodseq: Some(50),
                counts: ServerCounts {
                    total: 1_000,
                    unread: 367,
                },
            },
        )
        .recv_blocking()
        .unwrap();
}

#[test]
fn marking_a_folder_read_counts_below_the_floor_at_once() {
    let fx = server();
    windowed(&fx);
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 367));
    let mut views = Views::open(&fx.store, &all_views(&fx));
    let queued = fx
        .store
        .mark_folder_read(fx.inbox, NOW)
        .recv_blocking()
        .unwrap();
    assert_eq!(queued.applied, 67 + 300);
    let events = views.settle(&fx.store);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].folders,
        vec![FolderCounts {
            folder: fx.inbox,
            total: 1_000,
            unread: 0
        }]
    );
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 0));
    // A new STATUS doesn't bring the unread below back while the op is pending.
    fx.store
        .count_below(
            fx.inbox,
            ServerStatus {
                uidvalidity: UIDVALIDITY,
                uidnext: 1_001,
                highestmodseq: Some(51),
                counts: ServerCounts {
                    total: 1_000,
                    unread: 367,
                },
            },
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 0));
    let groups = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(groups.len(), 2);
    let below = groups
        .iter()
        .find(|g| matches!(g.action, OpAction::ReadBelow { .. }))
        .unwrap();
    assert_eq!(below.action, OpAction::ReadBelow { floor: 801 });
    assert!(below.messages.is_empty());
    let mut next_uid = 2_000;
    run_all(&fx, NOW, &mut next_uid);
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 0));
    let state = fx
        .store
        .folder_sync(fx.inbox)
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert_eq!(state.below, 800);
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);

    // Undone after it ran: the messages come back unread, the ones below the floor can't.
    let undone = fx
        .store
        .undo(queued.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.reverted, 67);
    assert_eq!(undone.lost, vec![(Lost::ReadBelow, 300)]);
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 67));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_folder_read_refused_or_undone_in_time_shows_the_unread_below_again() {
    let fx = server();
    windowed(&fx);
    let queued = fx
        .store
        .mark_folder_read(fx.inbox, NOW + WINDOW)
        .recv_blocking()
        .unwrap();
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 0));
    let undone = fx
        .store
        .undo(queued.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.restored, 67 + 300);
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 367));
    fx.store
        .mark_folder_read(fx.inbox, NOW)
        .recv_blocking()
        .unwrap();
    let groups = due(&fx.store, fx.account, NOW).groups;
    let ops: Vec<OpId> = groups.iter().flat_map(|g| g.ops.clone()).collect();
    let refused = fx
        .store
        .ops_failed(ops, Failure::Permanent)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        refused.refused,
        vec![OpsRefusal {
            account: fx.account,
            change: RefusedChange::FolderRead,
            messages: 367
        }]
    );
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 367));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn older_mail_loaded_under_a_pending_folder_read_shows_read() {
    let fx = server();
    windowed(&fx);
    fx.store
        .mark_folder_read(fx.inbox, NOW + WINDOW)
        .recv_blocking()
        .unwrap();
    let mut views = Views::open(&fx.store, &all_views(&fx));
    // UIDs 701–800 from below the floor, 33 of them unread on the server.
    fx.store
        .sync_changes(
            fx.inbox,
            FolderChanges {
                new: (701..=800)
                    .map(|uid| {
                        let flags = if uid % 3 == 0 {
                            Flags::NONE
                        } else {
                            Flags::SEEN
                        };
                        message(fx.inbox, uid, flags)
                    })
                    .collect(),
                floor: Some(701),
                below: Some(700),
                ..FolderChanges::default()
            },
        )
        .recv_blocking()
        .unwrap();
    views.settle(&fx.store);
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 0));
    let held = held_ids(&fx.store, fx.inbox);
    assert_eq!(shown(&fx.store, held[&702]), Some(Flags::SEEN));
    assert_eq!(server_flags(&fx.store, fx.inbox)[&702], Flags::NONE);
    clean(&fx.store);
    let mut next_uid = 3_000;
    run_all(&fx, NOW + WINDOW, &mut next_uid);
    views.settle(&fx.store);
    assert_eq!(
        server_flags(&fx.store, fx.inbox)[&702],
        Flags::SEEN,
        "read on the server once the folder read ran"
    );
    assert_eq!(counts(&fx.store, fx.inbox), (1_000, 0));
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn count_below_counts_rows_as_the_server_holds_them() {
    let fx = server();
    windowed(&fx);
    let held = held_ids(&fx.store, fx.inbox);
    // An unread one archived (pending), an unread one marked read (pending), and one the
    // server flagged `\Deleted`.
    queue(&fx.store, Triage::Archive, &[held[&999]], NOW);
    queue(&fx.store, Triage::Read(true), &[held[&996]], NOW);
    fx.store
        .set_flags(vec![held[&993]], Flags::DELETED, Flags::NONE)
        .recv_blocking()
        .unwrap();
    assert_eq!(counts(&fx.store, fx.inbox), (998, 364));
    // The same STATUS (one newer modseq) leaves the counts below as they were.
    let changed = fx
        .store
        .count_below(
            fx.inbox,
            ServerStatus {
                uidvalidity: UIDVALIDITY,
                uidnext: 1_001,
                highestmodseq: Some(52),
                counts: ServerCounts {
                    total: 1_000,
                    unread: 367,
                },
            },
        )
        .recv_blocking()
        .unwrap();
    assert!(!changed.value);
    assert_eq!(counts(&fx.store, fx.inbox), (998, 364));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn ops_started_before_a_crash_are_due_again_and_recording_twice_changes_nothing() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Archive, &ids[..2], NOW);
    queue(&fx.store, Triage::Read(true), &ids[2..3], NOW);
    let groups = due(&fx.store, fx.account, NOW).groups;
    let all: Vec<OpId> = groups.iter().flat_map(|g| g.ops.clone()).collect();
    fx.store.start_ops(all, Some(80)).recv_blocking().unwrap();
    // The process dies here; the store reopens.
    let path = fx.store.path().to_owned();
    fx.store.close().unwrap();
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(counts(&store, fx.inbox), (8, 3), "the overlay survives");
    let again = store
        .due_ops(fx.account, NOW)
        .recv_blocking()
        .unwrap()
        .groups;
    assert_eq!(again.len(), 2);
    assert!(again.iter().all(|g| g.retry));
    let moved = again
        .iter()
        .find(|g| matches!(g.action, OpAction::Move { .. }))
        .unwrap();
    assert_eq!(moved.target_uidnext, Some(80));
    let done = OpsDone {
        ops: moved.ops.clone(),
        new_uids: vec![(1, 80), (2, 81)],
        ..OpsDone::default()
    };
    let first = store.ops_done(done.clone()).recv_blocking().unwrap();
    let second = store.ops_done(done).recv_blocking().unwrap();
    assert_eq!(
        second.version, first.version,
        "the second record changes nothing"
    );
    assert_eq!(held_ids(&store, fx.archive).len(), 2);
    // A pass that saw the read done first: the record afterwards changes nothing visible.
    let read = again
        .iter()
        .find(|g| matches!(g.action, OpAction::Flags { .. }))
        .unwrap();
    store
        .set_flags(vec![ids[2]], Flags::SEEN, Flags::NONE)
        .recv_blocking()
        .unwrap();
    let version = store.accounts().recv_blocking().unwrap().version;
    let recorded = store
        .ops_done(OpsDone {
            ops: read.ops.clone(),
            ..OpsDone::default()
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(recorded.version, version);
    assert_eq!(store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&store);
    store.close().unwrap();
}

#[test]
fn removing_an_account_removes_its_ops() {
    let fx = server();
    let ids = ten(&fx);
    queue(&fx.store, Triage::Archive, &ids, NOW);
    fx.store.remove_account(fx.account).recv_blocking().unwrap();
    let conn = Connection::open(fx.store.path()).unwrap();
    let ops: i64 = conn
        .query_row("SELECT count(*) FROM ops", [], |row| row.get(0))
        .unwrap();
    assert_eq!(ops, 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_v6_store_migrates_hiding_deleted_messages() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    for (index, sql) in schema::MIGRATIONS[..6].iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            [index as i64 + 1],
        )
        .unwrap();
    }
    conn.execute_batch(
        "PRAGMA user_version = 6;
         INSERT INTO accounts(name, address) VALUES ('Fixture', 'fixture@example.test');
         INSERT INTO folders(account_id, name, name_raw, role, sync_state, uidvalidity, uidnext, \
           sync_floor, total_count, unread_count) \
           VALUES (1, 'INBOX', 'INBOX', 'inbox', 'ready', 7, 4, 1, 3, 2);
         INSERT INTO threads(account_id, subject, first_date, last_date, message_count) \
           VALUES (1, 'a', 1, 3, 3);
         INSERT INTO messages(folder_id, uid, thread_id, date, from_name, from_addr, subject, \
           snippet, flags, has_attachments, body_kind, size, body_state) VALUES \
           (1, 1, 1, 1, '', '', 'a', '', 0, 0, 0, 1, 0), \
           (1, 2, 1, 2, '', '', 'a', '', 8, 0, 0, 1, 0), \
           (1, 3, 1, 3, '', '', 'a', '', 1, 0, 0, 1, 0);",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(counts(&store, FolderId(1)), (2, 1));
    assert_eq!(whole(&store, ViewSpec::folder(FolderId(1))).rows.len(), 2);
    clean(&store);
    store.close().unwrap();
}
