//! POP3 accounts (M8, schema v11): the local folders, messages stored with their UIDLs once and
//! in one transaction, the end of a pass, ops done at once with Undo, sending with the Sent copy
//! filed locally and no Drafts-folder copies, the disk cap, removal and the v11 migration.

use super::body_tests::{raw, upgrade};
use super::compose_tests::{draft, prepared, save};
use super::ops_tests::{NOW, WINDOW, counts, held_ids, message};
use super::tests::scratch;
use super::*;
use rusqlite::Connection;
use sealwax_domain::account::AccountKey;
use sealwax_domain::compose::{DraftSource, ReplyKind};
use sealwax_domain::model::FolderRole;
use sealwax_domain::triage::Triage;

/// Unix seconds.
const AT: i64 = 1_790_000_000;

struct Pop {
    dir: tempfile::TempDir,
    store: Store,
    account: AccountId,
    inbox: FolderId,
}

impl Pop {
    fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("store.sqlite")
    }

    fn folder(&self, role: FolderRole) -> FolderId {
        roles(&self.store, self.account)
            .into_iter()
            .find(|(_, found)| *found == role)
            .unwrap()
            .0
    }
}

fn roles(store: &Store, account: AccountId) -> Vec<(FolderId, FolderRole)> {
    store
        .account_folders(account)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|folder| (folder.folder, folder.role))
        .collect()
}

/// A POP3 account (it has a key, as `accounts.toml` lists it) with its local folders.
fn pop() -> Pop {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .ensure_account(
            AccountKey("pop3-test".into()),
            NewAccount {
                name: "Pop".into(),
                address: "pop@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    let made = store.pop3_account(account).recv_blocking().unwrap();
    Pop {
        dir,
        store,
        account,
        inbox: made.inbox,
    }
}

/// Message `n` as the server's UIDL `u<n>` downloads it, into the Inbox.
fn download(inbox: FolderId, n: u32) -> Pop3Download {
    Pop3Download {
        uidl: format!("u{n}"),
        message: NewMessage {
            uid: None,
            blob: Some(BlobKey([n as u8; 32])),
            body_state: BodyState::Full,
            ..message(inbox, n, Flags::NONE)
        },
        whole: true,
    }
}

fn stored(store: &Store, account: AccountId, downloads: Vec<Pop3Download>) -> Pop3Stored {
    store
        .pop3_store(account, downloads, AT)
        .recv_blocking()
        .unwrap()
}

fn uidls(store: &Store, account: AccountId) -> Vec<(String, Option<MessageId>, i64, Option<i64>)> {
    store
        .pop3_uidls(account)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|uidl| (uidl.uidl, uidl.message, uidl.downloaded, uidl.deleted))
        .collect()
}

fn queue(store: &Store, action: Triage, messages: &[MessageId]) -> Queued {
    store
        .queue_triage(QueueTriage {
            action,
            messages: messages.to_vec(),
            not_before: NOW + WINDOW,
        })
        .recv_blocking()
        .unwrap()
}

fn flags(store: &Store, id: MessageId) -> Option<Flags> {
    store
        .message(id)
        .recv_blocking()
        .unwrap()
        .value
        .map(|detail| detail.summary.flags)
}

fn clean(store: &Store) {
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

#[test]
fn a_pop3_account_gets_its_local_folders_once() {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .ensure_account(
            AccountKey("pop3-test".into()),
            NewAccount {
                name: "Pop".into(),
                address: "pop@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    let events = store.subscribe();
    let made = store.pop3_account(account).recv_blocking().unwrap();
    assert!(made.created);
    assert_eq!(made.first_pass, None);
    let event = events.try_recv().unwrap();
    assert!(event.folders_changed);
    let found: Vec<FolderRole> = roles(&store, account)
        .into_iter()
        .map(|(_, role)| role)
        .collect();
    assert_eq!(
        found,
        [
            FolderRole::Inbox,
            FolderRole::Sent,
            FolderRole::Drafts,
            FolderRole::Junk,
            FolderRole::Trash,
            FolderRole::Archive
        ]
    );
    let names: Vec<String> = store
        .folders()
        .recv_blocking()
        .unwrap()
        .value
        .into_iter()
        .map(|folder| folder.name)
        .collect();
    assert_eq!(
        names,
        ["INBOX", "Sent", "Drafts", "Junk", "Trash", "Archive"]
    );
    // Nothing older to load from a server, actor or not.
    for folder in store.account_folders(account).recv_blocking().unwrap() {
        assert!(folder.complete());
    }
    assert!(store.is_pop3_account(account).recv_blocking().unwrap());
    assert!(
        !store
            .is_pop3_account(AccountId(999))
            .recv_blocking()
            .unwrap()
    );
    // Again (every start of its actor): nothing new, no event.
    let again = store.pop3_account(account).recv_blocking().unwrap();
    assert_eq!((again.inbox, again.created), (made.inbox, false));
    assert!(events.try_recv().is_err());
    assert_eq!(roles(&store, account).len(), 6);
    assert!(matches!(
        store.pop3_account(AccountId(999)).recv_blocking(),
        Err(StoreError::UnknownAccount(AccountId(999)))
    ));
    clean(&store);
}

#[test]
fn downloads_are_stored_with_their_uidls_once() {
    let pop = pop();
    let events = pop.store.subscribe();
    let first = stored(
        &pop.store,
        pop.account,
        (1..=3).map(|n| download(pop.inbox, n)).collect(),
    );
    assert!(first.ids.iter().all(Option::is_some));
    let event = events.try_recv().unwrap();
    assert_eq!(event.inserted.len(), 3);
    assert_eq!(counts(&pop.store, pop.inbox), (3, 3));
    let recorded = uidls(&pop.store, pop.account);
    assert_eq!(
        recorded,
        (1..=3)
            .map(|n| (format!("u{n}"), first.ids[n - 1], AT, None))
            .collect::<Vec<_>>()
    );
    // The full text is indexed.
    let found = pop
        .store
        .search("message2", None, 10)
        .recv_blocking()
        .unwrap();
    assert_eq!(found.value.len(), 1);

    // The same UIDLs again (a pass after a crash that lost the reply): nothing more.
    let again = stored(
        &pop.store,
        pop.account,
        (1..=3).map(|n| download(pop.inbox, n)).collect(),
    );
    assert_eq!(again.ids, [None, None, None]);
    assert_eq!(again.version, first.version);
    assert!(events.try_recv().is_err());
    // The same UIDL twice in one batch is stored once.
    let twice = stored(
        &pop.store,
        pop.account,
        vec![download(pop.inbox, 4), download(pop.inbox, 4)],
    );
    assert!(twice.ids[0].is_some() && twice.ids[1].is_none());
    // Another UIDL with the same bytes: recorded, naming the row the Inbox holds.
    let copy = Pop3Download {
        uidl: "u1-again".into(),
        ..download(pop.inbox, 1)
    };
    let same = stored(&pop.store, pop.account, vec![copy]);
    assert_eq!(same.ids, [None]);
    let recorded = uidls(&pop.store, pop.account);
    assert_eq!(recorded.len(), 5);
    assert_eq!(recorded[1].0, "u1-again");
    assert_eq!(recorded[1].1, first.ids[0]);
    assert_eq!(held_ids(&pop.store, pop.inbox).len(), 4);
    // Local UIDs, one after another.
    assert_eq!(
        held_ids(&pop.store, pop.inbox)
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    // Only into the account's own folders, and only for a POP3 account.
    let other = pop
        .store
        .add_account(NewAccount {
            name: "Local".into(),
            address: "local@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let elsewhere = pop
        .store
        .upsert_folder(FolderSpec::local(other, "INBOX", FolderRole::Inbox))
        .recv_blocking()
        .unwrap()
        .value;
    assert!(matches!(
        pop.store
            .pop3_store(pop.account, vec![download(elsewhere, 9)], AT)
            .recv_blocking(),
        Err(StoreError::UnknownFolder(_))
    ));
    assert!(matches!(
        pop.store
            .pop3_store(other, vec![download(elsewhere, 9)], AT)
            .recv_blocking(),
        Err(StoreError::NotPop3(_))
    ));
    clean(&pop.store);
}

#[test]
fn a_failed_transaction_stores_neither_a_message_nor_its_uidl() {
    let pop = pop();
    let path = pop.path();
    // A fault planted in the database aborts the transaction at the second UIDL, after the
    // first message and UIDL were written in it: a crash mid-transaction rolls back the same way.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fault BEFORE INSERT ON pop3_uidls WHEN NEW.uidl = 'u2' \
         BEGIN SELECT RAISE(ABORT, 'planted'); END;",
    )
    .unwrap();
    drop(conn);
    let failed = pop
        .store
        .pop3_store(
            pop.account,
            vec![download(pop.inbox, 1), download(pop.inbox, 2)],
            AT,
        )
        .recv_blocking();
    assert!(failed.is_err());
    assert!(uidls(&pop.store, pop.account).is_empty());
    assert!(held_ids(&pop.store, pop.inbox).is_empty());
    assert_eq!(counts(&pop.store, pop.inbox), (0, 0));
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TRIGGER fault;").unwrap();
    drop(conn);
    // The next pass downloads both again, and they are stored once.
    let again = stored(
        &pop.store,
        pop.account,
        vec![download(pop.inbox, 1), download(pop.inbox, 2)],
    );
    assert!(again.ids.iter().all(Option::is_some));
    // What a commit stored survives a restart with its UIDL.
    let Pop {
        store,
        account,
        inbox,
        dir,
    } = pop;
    store.close().unwrap();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let recorded = uidls(&store, account);
    assert_eq!(recorded.len(), 2);
    let held: Vec<MessageId> = held_ids(&store, inbox).into_values().collect();
    assert_eq!(
        recorded
            .iter()
            .map(|uidl| uidl.1.unwrap())
            .collect::<Vec<_>>(),
        held
    );
    clean(&store);
}

#[test]
fn the_end_of_a_pass_records_the_first_pass_deletions_and_forgets() {
    let pop = pop();
    stored(
        &pop.store,
        pop.account,
        (1..=4).map(|n| download(pop.inbox, n)).collect(),
    );
    let first_pass = |store: &Store| store.pop3_account(pop.account).recv_blocking().unwrap();
    // A pass cut short: no first pass yet.
    pop.store
        .pop3_pass_done(
            pop.account,
            Pop3PassDone {
                complete: false,
                at: AT + 10,
                ..Pop3PassDone::default()
            },
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(first_pass(&pop.store).first_pass, None);
    let done = |deleted: &[&str], forget: &[&str], at: i64| {
        pop.store
            .pop3_pass_done(
                pop.account,
                Pop3PassDone {
                    complete: true,
                    deleted: deleted.iter().map(|uidl| (*uidl).to_owned()).collect(),
                    forget: forget.iter().map(|uidl| (*uidl).to_owned()).collect(),
                    at,
                },
            )
            .recv_blocking()
            .unwrap();
    };
    done(&["u1", "u2"], &[], AT + 20);
    assert_eq!(first_pass(&pop.store).first_pass, Some(AT + 20));
    done(&["u2"], &["u3"], AT + 30);
    assert_eq!(first_pass(&pop.store).first_pass, Some(AT + 20), "kept");
    let recorded = uidls(&pop.store, pop.account);
    let deleted: Vec<(&str, Option<i64>)> = recorded
        .iter()
        .map(|uidl| (uidl.0.as_str(), uidl.3))
        .collect();
    // u2's first deletion stays; u3 isn't forgotten, since Sealwax never deleted it.
    assert_eq!(
        deleted,
        [
            ("u1", Some(AT + 20)),
            ("u2", Some(AT + 20)),
            ("u3", None),
            ("u4", None)
        ]
    );
    done(&[], &["u1", "u4"], AT + 40);
    let left: Vec<String> = uidls(&pop.store, pop.account)
        .into_iter()
        .map(|uidl| uidl.0)
        .collect();
    assert_eq!(left, ["u2", "u3", "u4"]);
    // Forgetting a UIDL leaves its message alone.
    assert_eq!(held_ids(&pop.store, pop.inbox).len(), 4);
    clean(&pop.store);
}

#[test]
fn a_pop3_accounts_triage_is_done_at_once_with_undo() {
    let pop = pop();
    let ids: Vec<MessageId> = stored(
        &pop.store,
        pop.account,
        (1..=6).map(|n| download(pop.inbox, n)).collect(),
    )
    .ids
    .into_iter()
    .map(Option::unwrap)
    .collect();
    let archive = pop.folder(FolderRole::Archive);
    let trash = pop.folder(FolderRole::Trash);
    let junk = pop.folder(FolderRole::Junk);

    let read = queue(&pop.store, Triage::Read(true), &ids[..2]);
    assert!(read.accounts.is_empty(), "nothing for an op runner");
    assert_eq!(flags(&pop.store, ids[0]), Some(Flags::SEEN));
    let archived = queue(&pop.store, Triage::Archive, &ids[2..4]);
    assert_eq!(held_ids(&pop.store, archive).len(), 2);
    queue(&pop.store, Triage::Junk, &ids[4..5]);
    assert_eq!(held_ids(&pop.store, junk).len(), 1);
    assert_eq!(
        pop.store.pending_ops(pop.account).recv_blocking().unwrap(),
        0
    );
    let due = pop
        .store
        .due_ops(pop.account, NOW + 60_000)
        .recv_blocking()
        .unwrap();
    assert!(due.groups.is_empty());

    // Undo puts the archived messages back in the Inbox, as they were.
    let undone = pop
        .store
        .undo(archived.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.reverted, 2);
    assert!(held_ids(&pop.store, archive).is_empty());
    assert_eq!(held_ids(&pop.store, pop.inbox).len(), 5);
    // A moved message keeps its UIDL.
    let recorded = uidls(&pop.store, pop.account);
    assert_eq!(recorded[4].1, Some(ids[4]));

    // Mark Folder as Read: at once.
    let all_read = pop
        .store
        .mark_folder_read(pop.inbox, NOW + WINDOW)
        .recv_blocking()
        .unwrap();
    assert!(all_read.accounts.is_empty());
    assert_eq!(counts(&pop.store, pop.inbox), (5, 0));

    // Deleted from the Trash for good: the row goes, its UIDL stays (never downloaded again).
    queue(&pop.store, Triage::Delete, &ids[5..6]);
    assert_eq!(held_ids(&pop.store, trash).len(), 1);
    let gone = queue(&pop.store, Triage::Delete, &ids[5..6]);
    assert_eq!(gone.applied, 1);
    assert!(held_ids(&pop.store, trash).is_empty());
    let recorded = uidls(&pop.store, pop.account);
    assert_eq!((recorded[5].0.as_str(), recorded[5].1), ("u6", None));
    clean(&pop.store);
}

#[test]
fn a_pop3_account_sends_and_files_its_sent_copy_locally() {
    let pop = pop();
    let store = &pop.store;
    let source = stored(store, pop.account, vec![download(pop.inbox, 1)]).ids[0].unwrap();
    let sent = pop.folder(FolderRole::Sent);
    let mut reply = draft(pop.account, 1);
    reply.source = Some(DraftSource {
        message: source,
        kind: ReplyKind::Reply,
    });
    let id = save(store, None, reply, true);
    // No server holds its drafts: nothing to delete there.
    let drafts = store
        .due_drafts(pop.account, NOW + 3_600_000)
        .recv_blocking()
        .unwrap();
    assert!(drafts.deletions.is_empty());

    let queued = store.queue_send(id, NOW + 7_000).recv_blocking().unwrap();
    assert!(matches!(queued, SendQueued::Queued { .. }), "{queued:?}");
    let due = store
        .due_sends(pop.account, NOW + 7_000)
        .recv_blocking()
        .unwrap();
    assert_eq!(due.sends.len(), 1);
    assert_eq!(
        store
            .send_started(id, prepared(1), NOW + 7_100)
            .recv_blocking()
            .unwrap(),
        SendStarted::Started
    );
    let done = store.send_done(id, NOW + 7_200).recv_blocking().unwrap();
    assert_eq!(done.state, Some(SendState::Sent));
    assert!(!done.copies_to_delete, "no Drafts-folder copies");
    assert!(!done.ops_queued, "the reply's \\Answered is set at once");
    assert_eq!(flags(store, source), Some(Flags::ANSWERED));

    // The Sent copy is due for the local Sent folder: filed there, then done.
    let due = store
        .due_sends(pop.account, NOW + 7_300)
        .recv_blocking()
        .unwrap();
    assert_eq!(due.sent_copies.len(), 1);
    let copy = &due.sent_copies[0];
    assert_eq!(copy.folder.as_ref().map(|folder| folder.folder), Some(sent));
    assert_eq!(copy.blob, key_of(&prepared(1)));
    let filed = NewMessage {
        folder: sent,
        uid: None,
        flags: Flags::SEEN,
        blob: Some(copy.blob),
        body_state: BodyState::Full,
        ..message(sent, 50, Flags::SEEN)
    };
    let ingested = store.ingest(vec![filed.clone()]).recv_blocking().unwrap();
    assert!(ingested.ids[0].is_some());
    // Filed again after a crash before the record: the Sent folder holds it already.
    let again = store.ingest(vec![filed]).recv_blocking().unwrap();
    assert_eq!(again.ids, [None]);
    let recorded = store
        .sent_copy_done(id, SentCopyDone::Filed)
        .recv_blocking()
        .unwrap();
    assert!(recorded.applied);
    assert!(
        !recorded.orphaned_blobs.contains(&copy.blob),
        "the filed copy keeps its bytes"
    );
    assert!(store.draft(id).recv_blocking().unwrap().is_none());
    assert_eq!(counts(store, sent), (1, 0));
    assert!(store.outbox(None).recv_blocking().unwrap().is_empty());

    // A discarded draft without a copy has nothing to delete.
    let other = save(store, None, draft(pop.account, 2), false);
    let discarded = store.discard_draft(other).recv_blocking().unwrap();
    assert!(discarded.found && !discarded.server_copy);
    clean(store);
}

/// The copy of `draft` the actor files, as `blob`, into the Drafts folder.
fn file_copy(pop: &Pop, drafts: FolderId, message_id: &str, blob: u8) -> MessageId {
    let mut copy = message(
        drafts,
        60 + u32::from(blob),
        Flags::DRAFT.union(Flags::SEEN),
    );
    copy.uid = None;
    copy.headers.message_id = Some(message_id.to_owned());
    copy.blob = Some(BlobKey([blob; 32]));
    copy.body_state = BodyState::Full;
    pop.store.ingest(vec![copy]).recv_blocking().unwrap().ids[0].unwrap()
}

#[test]
fn a_pop3_drafts_copies_are_rows_of_its_local_drafts_folder() {
    let pop = pop();
    let store = &pop.store;
    let drafts = pop.folder(FolderRole::Drafts);
    let mut first = draft(pop.account, 1);
    first.message_id = "local-draft@example.test".into();
    let id = save(store, None, first.clone(), false);
    // Due for the local Drafts folder once it went quiet, with nothing to replace on a server.
    let due = store
        .due_drafts(pop.account, NOW + CLOSED_DRAFT_QUIET_MS)
        .recv_blocking()
        .unwrap();
    assert_eq!(due.uploads.len(), 1);
    let upload = &due.uploads[0];
    assert_eq!(upload.folder.folder, drafts);
    assert_eq!((upload.retry_from, upload.replaces), (None, None));

    let row = file_copy(&pop, drafts, &first.message_id, 201);
    let filed = store
        .draft_filed(pop.account, id, upload.revision, BlobKey([201; 32]))
        .recv_blocking()
        .unwrap();
    assert!(filed.applied && filed.orphaned_blobs.is_empty());
    assert_eq!(
        store.draft_for_message(row).recv_blocking().unwrap(),
        Some(id)
    );
    let due = store
        .due_drafts(pop.account, NOW + 3_600_000)
        .recv_blocking()
        .unwrap();
    assert!(due.uploads.is_empty(), "copied");

    // A new revision's copy replaces the old one at once.
    save(store, Some(id), first.clone(), false);
    let due = store
        .due_drafts(pop.account, NOW + 3_600_000)
        .recv_blocking()
        .unwrap();
    assert_eq!(due.uploads[0].revision, 2);
    let newer = file_copy(&pop, drafts, &first.message_id, 202);
    let filed = store
        .draft_filed(pop.account, id, 2, BlobKey([202; 32]))
        .recv_blocking()
        .unwrap();
    assert!(filed.applied);
    assert_eq!(filed.orphaned_blobs, [BlobKey([201; 32])]);
    assert_eq!(
        held_ids(store, drafts).into_values().collect::<Vec<_>>(),
        [newer]
    );

    // Sent: its copy goes at once, and its bytes are handed out for deletion.
    store.queue_send(id, NOW).recv_blocking().unwrap();
    store
        .send_started(id, prepared(3), NOW + 100)
        .recv_blocking()
        .unwrap();
    let done = store.send_done(id, NOW + 200).recv_blocking().unwrap();
    assert!(!done.ops_queued);
    assert!(held_ids(store, drafts).is_empty());
    let taken = store.take_blob_deletions().recv_blocking().unwrap();
    assert!(taken.contains(&BlobKey([202; 32])), "{taken:?}");

    // A copy filed after its draft was discarded goes itself.
    let late = save(store, None, draft(pop.account, 4), false);
    store.discard_draft(late).recv_blocking().unwrap();
    file_copy(&pop, drafts, "d4@example.test", 204);
    let filed = store
        .draft_filed(pop.account, late, 1, BlobKey([204; 32]))
        .recv_blocking()
        .unwrap();
    assert!(!filed.applied);
    assert_eq!(filed.orphaned_blobs, [BlobKey([204; 32])]);
    assert!(held_ids(store, drafts).is_empty());
    // Discarding a draft deletes its copy at once.
    let kept = save(store, None, draft(pop.account, 5), false);
    let copy = file_copy(&pop, drafts, "d5@example.test", 205);
    store
        .draft_filed(pop.account, kept, 1, BlobKey([205; 32]))
        .recv_blocking()
        .unwrap();
    assert_eq!(
        held_ids(store, drafts).into_values().collect::<Vec<_>>(),
        [copy]
    );
    store.discard_draft(kept).recv_blocking().unwrap();
    assert!(held_ids(store, drafts).is_empty());
    // Only for a POP3 account.
    let other = store
        .add_account(NewAccount {
            name: "Local".into(),
            address: "local@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    assert!(matches!(
        store
            .draft_filed(other, kept, 1, BlobKey([205; 32]))
            .recv_blocking(),
        Err(StoreError::NotPop3(_))
    ));
    clean(store);
}

#[test]
fn blobs_that_ops_done_at_once_leave_are_handed_out_once() {
    let pop = pop();
    let store = &pop.store;
    let ids: Vec<MessageId> = stored(
        store,
        pop.account,
        (1..=2).map(|n| download(pop.inbox, n)).collect(),
    )
    .ids
    .into_iter()
    .map(Option::unwrap)
    .collect();
    // Message 2's bytes are also in the Archive (another copy of the same message).
    let archive = pop.folder(FolderRole::Archive);
    let mut copy = download(archive, 2).message;
    copy.folder = archive;
    store.ingest(vec![copy]).recv_blocking().unwrap();
    assert!(
        store
            .take_blob_deletions()
            .recv_blocking()
            .unwrap()
            .is_empty()
    );
    queue(store, Triage::Delete, &ids);
    queue(store, Triage::Delete, &ids);
    let taken = store.take_blob_deletions().recv_blocking().unwrap();
    assert_eq!(
        taken,
        [BlobKey([1; 32])],
        "the Archive's copy keeps its bytes"
    );
    assert!(
        store
            .take_blob_deletions()
            .recv_blocking()
            .unwrap()
            .is_empty()
    );
    clean(store);
}

fn key_of(prepared: &Prepared) -> BlobKey {
    prepared.sent_copy_blob.unwrap_or(prepared.message_blob)
}

#[test]
fn an_imap_folder_list_never_replaces_a_pop3_accounts_folders() {
    let pop = pop();
    stored(&pop.store, pop.account, vec![download(pop.inbox, 1)]);
    let replaced = pop
        .store
        .replace_folders(
            pop.account,
            vec![FolderSpec::local(pop.account, "INBOX", FolderRole::Inbox)],
        )
        .recv_blocking();
    assert!(matches!(replaced, Err(StoreError::NotPop3(_))));
    assert_eq!(roles(&pop.store, pop.account).len(), 6);
    assert_eq!(held_ids(&pop.store, pop.inbox).len(), 1);
}

#[test]
fn the_disk_cap_never_evicts_a_pop3_accounts_mail() {
    let pop = pop();
    let ids = stored(
        &pop.store,
        pop.account,
        (1..=3).map(|n| download(pop.inbox, n)).collect(),
    )
    .ids;
    let usage = pop.store.body_usage().recv_blocking().unwrap();
    assert_eq!(usage, BodyUsage::default(), "downloaded mail isn't a cache");
    let evicted = pop
        .store
        .evict_bodies(0, Vec::new())
        .recv_blocking()
        .unwrap();
    assert!(evicted.messages.is_empty() && evicted.blobs.is_empty());
    for id in ids.into_iter().flatten() {
        let target = &pop.store.body_targets(vec![id]).recv_blocking().unwrap()[0];
        assert_eq!(target.body_state, BodyState::Full);
        assert!(target.blob.is_some());
    }
    let candidates = pop.store.body_candidates(0, 10).recv_blocking().unwrap();
    assert!(candidates.is_empty(), "nothing to prefetch");
}

/// Found in the security review (ML-093): blobs are named by their content, so an IMAP
/// account's cached body can be the very file a POP3 message keeps (the same mailbox added
/// both ways). Evicting the cached body took the POP3 message's only copy with it.
#[test]
fn the_disk_cap_never_evicts_a_blob_a_pop3_message_shares() {
    let pop = pop();
    let shared = BlobKey([1; 32]);
    let kept = stored(&pop.store, pop.account, vec![download(pop.inbox, 1)]).ids[0].unwrap();
    let imap = pop
        .store
        .ensure_account(
            AccountKey("imap-test".into()),
            NewAccount {
                name: "Imap".into(),
                address: "imap@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    let inbox = pop
        .store
        .upsert_folder(FolderSpec::local(imap, "INBOX", FolderRole::Inbox))
        .recv_blocking()
        .unwrap()
        .value;
    let cached = pop
        .store
        .ingest(vec![NewMessage {
            body_state: BodyState::Partial,
            ..message(inbox, 7, Flags::NONE)
        }])
        .recv_blocking()
        .unwrap()
        .ids[0]
        .unwrap();
    pop.store
        .upgrade_body(upgrade(cached, 7, shared, &raw(7), AT))
        .recv_blocking()
        .unwrap();
    assert!(pop.store.body_usage().recv_blocking().unwrap().bytes > 0);
    let evicted = pop
        .store
        .evict_bodies(0, Vec::new())
        .recv_blocking()
        .unwrap();
    assert!(evicted.blobs.is_empty(), "{evicted:?}");
    let target = &pop.store.body_targets(vec![kept]).recv_blocking().unwrap()[0];
    assert_eq!(target.blob, Some(shared));
}

#[test]
fn removing_a_pop3_account_takes_its_folders_mail_and_uidls() {
    let pop = pop();
    stored(
        &pop.store,
        pop.account,
        (1..=3).map(|n| download(pop.inbox, n)).collect(),
    );
    let removed = pop
        .store
        .remove_account(pop.account)
        .recv_blocking()
        .unwrap();
    assert!(removed.found);
    assert_eq!((removed.folders, removed.messages), (6, 3));
    let mut orphaned = removed.orphaned_blobs;
    orphaned.sort();
    assert_eq!(
        orphaned,
        [BlobKey([1; 32]), BlobKey([2; 32]), BlobKey([3; 32])]
    );
    assert!(uidls(&pop.store, pop.account).is_empty());
    let path = pop.path();
    pop.store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let left: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM pop3_accounts), (SELECT count(*) FROM pop3_uidls)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(left, (0, 0));
}

#[test]
fn v11_migrates_a_v10_store_and_a_newer_one_is_refused() {
    let pop = pop();
    let ids = stored(&pop.store, pop.account, vec![download(pop.inbox, 1)]).ids;
    let path = pop.path();
    let (account, inbox) = (pop.account, pop.inbox);
    pop.store.close().unwrap();
    // Back to v10: what M7 left on disk, with mail in it.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "ALTER TABLE outbox DROP COLUMN building; ALTER TABLE drafts DROP COLUMN upload_building; \
         DROP TABLE blob_deletions; DROP TABLE pop3_uidls; DROP TABLE pop3_accounts; \
         DELETE FROM schema_migrations WHERE version >= 11; PRAGMA user_version = 10;",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(
        held_ids(&store, inbox)
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [ids[0].unwrap()]
    );
    assert!(uidls(&store, account).is_empty());
    // The account is no POP3 account until its actor says so again.
    let made = store.pop3_account(account).recv_blocking().unwrap();
    assert!(made.created);
    assert_eq!(made.inbox, inbox);
    clean(&store);
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 12);
    // A store from a newer build is refused and left as it is.
    conn.execute_batch("PRAGMA user_version = 13;").unwrap();
    drop(conn);
    assert!(matches!(
        Store::open(&path, StoreOptions::default()),
        Err(StoreError::NewerSchema {
            found: 13,
            supported: 12
        })
    ));
    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 13);
}
