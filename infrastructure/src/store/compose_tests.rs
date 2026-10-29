//! Drafts, the outbox, the Sent copy, Drafts-folder copies and contacts (M6, schema v9): saves
//! and reopening, blobs held by drafts and the outbox, the undo window, every step of a send,
//! uncertain sends after a restart, the source message's flag, uploads that replace their copy
//! and hide stale ones, contacts ranked for suggestions, and the v9 migration with mail.

use super::ops_tests::{NOW, UIDVALIDITY, arrive, held_ids, message, ready};
use super::tests::{scratch, whole};
use super::*;
use rusqlite::Connection;
use sealwax_domain::account::AccountKey;
use sealwax_domain::compose::{Draft, DraftPart, DraftSource, ReplyKind};
use sealwax_domain::model::{Address, DraftId, FolderRole, Headers};
use std::path::PathBuf;

pub(super) struct Cx {
    pub(super) dir: tempfile::TempDir,
    pub(super) store: Store,
    pub(super) account: AccountId,
    pub(super) inbox: FolderId,
    pub(super) drafts: FolderId,
    pub(super) sent: FolderId,
}

impl Cx {
    pub(super) fn path(&self) -> PathBuf {
        self.dir.path().join("store.sqlite")
    }
}

/// A server account with a synced, empty INBOX, Drafts and Sent.
pub(super) fn server() -> Cx {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .ensure_account(
            AccountKey("compose-test".into()),
            NewAccount {
                name: "Compose".into(),
                address: "me@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    let ids = store
        .upsert_folders(vec![
            FolderSpec::local(account, "INBOX", FolderRole::Inbox),
            FolderSpec::local(account, "Drafts", FolderRole::Drafts),
            FolderSpec::local(account, "Sent", FolderRole::Sent),
        ])
        .recv_blocking()
        .unwrap()
        .value;
    for id in &ids {
        ready(&store, *id, UIDVALIDITY);
    }
    Cx {
        dir,
        store,
        account,
        inbox: ids[0],
        drafts: ids[1],
        sent: ids[2],
    }
}

pub(super) fn key(n: u8) -> BlobKey {
    BlobKey([n; 32])
}

pub(super) fn draft(account: AccountId, n: u32) -> Draft {
    Draft {
        account,
        from: Address::new("Me", "me@example.test"),
        to: vec![Address::new("Kari", "kari@example.test")],
        bcc: vec![Address::new("", "hidden@example.test")],
        subject: format!("Draft {n}"),
        html: format!("<p>Draft {n}</p>"),
        message_id: format!("d{n}@example.test"),
        parts: vec![DraftPart {
            name: "plan.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 3,
            blob: key(n as u8),
            content_id: None,
        }],
        ..Draft::default()
    }
}

pub(super) fn save(store: &Store, id: Option<DraftId>, draft: Draft, open: bool) -> DraftId {
    match store
        .save_draft(SaveDraft {
            id,
            draft,
            open,
            now: NOW,
        })
        .recv_blocking()
        .unwrap()
    {
        DraftSaved::Saved { id, .. } => id,
        other => panic!("not saved: {other:?}"),
    }
}

pub(super) fn prepared(n: u8) -> Prepared {
    Prepared {
        message_blob: key(100 + n),
        sent_copy_blob: Some(key(150 + n)),
        size: 1_000,
        envelope_from: "me@example.test".into(),
        envelope_to: vec!["kari@example.test".into(), "hidden@example.test".into()],
    }
}

fn state(store: &Store, id: DraftId) -> Option<SendState> {
    store.draft(id).recv_blocking().unwrap().unwrap().send
}

fn due(store: &Store, account: AccountId, now: i64) -> Vec<DraftId> {
    store
        .due_sends(account, now)
        .recv_blocking()
        .unwrap()
        .sends
        .iter()
        .map(|send| send.draft)
        .collect()
}

fn outbox_changes(events: &async_channel::Receiver<Arc<ChangeEvent>>) -> Vec<OutboxChange> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.extend(event.outbox.iter().copied());
    }
    out
}

#[test]
fn a_draft_saves_reads_back_reopens_and_bumps_its_revision() {
    let cx = server();
    let events = cx.store.subscribe();
    let mut first = draft(cx.account, 1);
    first.cc = vec![Address::new("Åse Bråten", "ase@example.test")];
    first.references = vec!["a@example.test".into(), "b@example.test".into()];
    first.in_reply_to = Some("b@example.test".into());
    let id = save(&cx.store, None, first.clone(), true);
    let stored = cx.store.draft(id).recv_blocking().unwrap().unwrap();
    assert_eq!(stored.draft, first);
    assert_eq!((stored.revision, stored.open, stored.send), (1, true, None));
    assert_eq!(cx.store.open_drafts().recv_blocking().unwrap(), [id]);

    let mut second = first.clone();
    second.html = "x".repeat(200_000);
    second.parts.push(DraftPart {
        name: "inline.png".into(),
        mime_type: "image/png".into(),
        size: 9,
        blob: key(9),
        content_id: Some("img1".into()),
    });
    let saved = cx
        .store
        .save_draft(SaveDraft {
            id: Some(id),
            draft: second.clone(),
            open: true,
            now: NOW + 5_000,
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(saved, DraftSaved::Saved { id, revision: 2 });
    let stored = cx.store.draft(id).recv_blocking().unwrap().unwrap();
    assert_eq!(stored.draft, second);
    assert_eq!((stored.saved, stored.created), (NOW + 5_000, NOW));
    assert!(events.try_recv().is_err(), "draft saves send no event");

    // Closing keeps the draft; it no longer reopens.
    cx.store.set_draft_open(id, false).recv_blocking().unwrap();
    assert!(cx.store.open_drafts().recv_blocking().unwrap().is_empty());
    assert!(cx.store.draft(id).recv_blocking().unwrap().is_some());

    // Reopening the store keeps everything.
    let path = cx.path();
    cx.store.close().unwrap();
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(
        store.draft(id).recv_blocking().unwrap().unwrap().draft,
        second
    );
    let discarded = store.discard_draft(id).recv_blocking().unwrap();
    assert!(discarded.found && !discarded.server_copy);
    let mut orphaned = discarded.orphaned_blobs;
    orphaned.sort();
    assert_eq!(orphaned, [key(1), key(9)]);
    assert!(store.draft(id).recv_blocking().unwrap().is_none());
    assert_eq!(
        store
            .save_draft(SaveDraft {
                id: Some(id),
                draft: second,
                open: true,
                now: NOW,
            })
            .recv_blocking()
            .unwrap(),
        DraftSaved::Gone
    );
    assert!(store.check().recv_blocking().unwrap().is_empty());
    store.close().unwrap();
}

#[test]
fn blobs_of_drafts_and_the_outbox_are_never_evicted_or_orphaned() {
    let cx = server();
    let store = &cx.store;
    // Two messages whose downloaded bodies share their bytes with a draft's part and with a
    // built message in the outbox (an .eml attached, a Sent copy synced back).
    let ids = arrive(
        store,
        cx.inbox,
        vec![
            message(cx.inbox, 1, Flags::NONE),
            message(cx.inbox, 2, Flags::NONE),
        ],
    );
    for (id, uid, blob) in [(ids[0], 1, key(1)), (ids[1], 2, key(101))] {
        let upgraded = store
            .upgrade_body(BodyUpgrade {
                message: id,
                uid,
                blob,
                blob_size: 1_000,
                body_state: BodyState::Full,
                body_kind: Default::default(),
                has_attachments: false,
                snippet: String::new(),
                text_parts: Vec::new(),
                html_parts: Vec::new(),
                attachments: Vec::new(),
                sections: Vec::new(),
                search_text: String::new(),
                used: 1,
            })
            .recv_blocking()
            .unwrap();
        assert!(upgraded.found);
    }
    let held = save(store, None, draft(cx.account, 1), true);
    let sending = save(store, None, draft(cx.account, 2), false);
    assert!(matches!(
        store.queue_send(sending, NOW).recv_blocking().unwrap(),
        SendQueued::Queued { .. }
    ));
    assert_eq!(
        store
            .send_started(sending, prepared(1), NOW)
            .recv_blocking()
            .unwrap(),
        SendStarted::Started
    );
    // A part dropped by a later save stays held until the draft ends.
    let mut without = draft(cx.account, 1);
    without.parts.clear();
    save(store, Some(held), without, true);

    let evicted = store.evict_bodies(0, Vec::new()).recv_blocking().unwrap();
    assert!(evicted.blobs.is_empty(), "{:?}", evicted.blobs);
    assert_eq!(evicted.bytes, 2_000);
    let removed = store.remove(ids.clone()).recv_blocking().unwrap();
    assert!(removed.orphaned_blobs.is_empty());

    // Ending the draft and the send gives them back.
    let discarded = store.discard_draft(held).recv_blocking().unwrap();
    assert_eq!(discarded.orphaned_blobs, [key(1)]);
    store.send_done(sending, NOW).recv_blocking().unwrap();
    let finished = store
        .sent_copy_done(sending, SentCopyDone::ServerSaves)
        .recv_blocking()
        .unwrap();
    let mut orphaned = finished.orphaned_blobs;
    orphaned.sort();
    assert_eq!(orphaned, [key(2), key(101), key(151)]);
    assert!(store.check().recv_blocking().unwrap().is_empty());
}

#[test]
fn removing_an_account_returns_its_drafts_blobs_unless_another_holds_them() {
    let cx = server();
    let other = cx
        .store
        .ensure_account(
            AccountKey("other".into()),
            NewAccount {
                name: "Other".into(),
                address: "other@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    save(&cx.store, None, draft(cx.account, 1), true);
    save(&cx.store, None, draft(cx.account, 2), true);
    save(&cx.store, None, draft(other, 2), true);
    let removed = cx.store.remove_account(cx.account).recv_blocking().unwrap();
    assert_eq!(removed.orphaned_blobs, [key(1)]);
    assert_eq!(cx.store.open_drafts().recv_blocking().unwrap().len(), 1);
}

#[test]
fn undo_send_inside_the_window_takes_it_back_and_nothing_starts() {
    let cx = server();
    let store = &cx.store;
    let events = store.subscribe();
    let id = save(store, None, draft(cx.account, 1), true);
    let queued = store.queue_send(id, NOW + 7_000).recv_blocking().unwrap();
    assert!(matches!(queued, SendQueued::Queued { revision: 1, .. }));
    assert_eq!(state(store, id), Some(SendState::Queued));
    assert!(store.open_drafts().recv_blocking().unwrap().is_empty());
    // Not editable while queued.
    assert_eq!(
        store
            .save_draft(SaveDraft {
                id: Some(id),
                draft: draft(cx.account, 1),
                open: true,
                now: NOW,
            })
            .recv_blocking()
            .unwrap(),
        DraftSaved::NotEditable {
            id,
            state: SendState::Queued
        }
    );
    let early = store
        .due_sends(cx.account, NOW + 6_999)
        .recv_blocking()
        .unwrap();
    assert!(early.sends.is_empty());
    assert_eq!(early.next, Some(NOW + 7_000));
    assert!(matches!(
        store.undo_send(id).recv_blocking().unwrap(),
        SendUndone::Undone { .. }
    ));
    assert_eq!(state(store, id), None);
    assert_eq!(store.open_drafts().recv_blocking().unwrap(), [id]);
    assert!(due(store, cx.account, NOW + 60_000).is_empty());
    assert_eq!(
        store.undo_send(id).recv_blocking().unwrap(),
        SendUndone::NotQueued
    );
    let changes = outbox_changes(&events);
    assert_eq!(
        changes.iter().map(|c| c.state).collect::<Vec<_>>(),
        [Some(SendState::Queued), None]
    );

    // Once started, Undo is too late; a due list taken before Undo starts nothing.
    store.queue_send(id, NOW).recv_blocking().unwrap();
    let due_now = due(store, cx.account, NOW);
    assert_eq!(due_now, [id]);
    store.undo_send(id).recv_blocking().unwrap();
    assert_eq!(
        store
            .send_started(id, prepared(1), NOW)
            .recv_blocking()
            .unwrap(),
        SendStarted::NotQueued(None)
    );
    store.queue_send(id, NOW).recv_blocking().unwrap();
    assert_eq!(
        store
            .send_started(id, prepared(1), NOW)
            .recv_blocking()
            .unwrap(),
        SendStarted::Started
    );
    assert_eq!(
        store.undo_send(id).recv_blocking().unwrap(),
        SendUndone::TooLate(SendState::Sending)
    );
    assert_eq!(
        store.discard_draft(id).recv_blocking().unwrap().refused,
        Some(SendState::Sending)
    );
}

#[test]
fn local_accounts_cant_send() {
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
    let id = save(&store, None, draft(account, 1), true);
    assert_eq!(
        store.queue_send(id, NOW).recv_blocking().unwrap(),
        SendQueued::LocalAccount
    );
    assert!(
        store
            .due_drafts(account, NOW + 3_600_000)
            .recv_blocking()
            .unwrap()
            .uploads
            .is_empty()
    );
}

#[test]
fn a_send_records_every_step_and_flags_its_source() {
    for (kind, flag) in [
        (ReplyKind::Reply, Flags::ANSWERED),
        (ReplyKind::ReplyAll, Flags::ANSWERED),
        (ReplyKind::Forward, Flags::FORWARDED),
    ] {
        let cx = server();
        let store = &cx.store;
        let events = store.subscribe();
        let source = arrive(store, cx.inbox, vec![message(cx.inbox, 4, Flags::SEEN)])[0];
        let mut reply = draft(cx.account, 1);
        reply.source = Some(DraftSource {
            message: source,
            kind,
        });
        let id = save(store, None, reply, true);
        store.queue_send(id, NOW).recv_blocking().unwrap();
        let due_now = store.due_sends(cx.account, NOW).recv_blocking().unwrap();
        assert_eq!(due_now.sends.len(), 1);
        assert_eq!(due_now.sends[0].prepared, None);
        assert_eq!(due_now.sends[0].message.subject, "Draft 1");
        assert_eq!(
            store
                .send_started(id, prepared(1), NOW)
                .recv_blocking()
                .unwrap(),
            SendStarted::Started
        );
        assert!(due(store, cx.account, NOW + 60_000).is_empty());
        let recorded = store.send_done(id, NOW + 1).recv_blocking().unwrap();
        assert!(recorded.applied && recorded.ops_queued && !recorded.copies_to_delete);
        assert_eq!(recorded.state, Some(SendState::Sent));
        // The source shows its flag at once and the op runner has it to do.
        let shown = store
            .message(source)
            .recv_blocking()
            .unwrap()
            .value
            .unwrap();
        assert!(shown.summary.flags.contains(flag));
        let ops = store.due_ops(cx.account, NOW + 2).recv_blocking().unwrap();
        assert_eq!(ops.groups.len(), 1);
        assert_eq!(
            ops.groups[0].action,
            OpAction::Flags {
                add: flag,
                remove: Flags::NONE
            }
        );
        // The Sent copy: due, started with the folder's mark, due again from the mark.
        let copies = store
            .due_sends(cx.account, NOW + 2)
            .recv_blocking()
            .unwrap();
        assert_eq!(copies.sent_copies.len(), 1);
        let copy = &copies.sent_copies[0];
        assert_eq!(copy.blob, key(151));
        assert_eq!(copy.message_id, "d1@example.test");
        assert_eq!(copy.folder.as_ref().unwrap().folder, cx.sent);
        assert_eq!(copy.retry_from, None);
        store
            .sent_copy_started(id, cx.sent, UIDVALIDITY, 5)
            .recv_blocking()
            .unwrap();
        store
            .sent_copy_failed(id, NOW + 10_000)
            .recv_blocking()
            .unwrap();
        let later = store
            .due_sends(cx.account, NOW + 3)
            .recv_blocking()
            .unwrap();
        assert!(later.sent_copies.is_empty());
        assert_eq!(later.next, Some(NOW + 10_000));
        store
            .sent_copy_started(id, cx.sent, UIDVALIDITY, 6)
            .recv_blocking()
            .unwrap();
        let retry = store
            .due_sends(cx.account, NOW + 10_000)
            .recv_blocking()
            .unwrap();
        assert_eq!(retry.sent_copies[0].retry_from, Some(5));
        assert_eq!(retry.sent_copies[0].attempts, 2);
        let finished = store
            .sent_copy_done(id, SentCopyDone::Appended { uid: Some(5) })
            .recv_blocking()
            .unwrap();
        assert!(finished.applied && finished.state.is_none());
        assert!(store.draft(id).recv_blocking().unwrap().is_none());
        assert!(store.outbox(None).recv_blocking().unwrap().is_empty());
        let states: Vec<Option<SendState>> =
            outbox_changes(&events).iter().map(|c| c.state).collect();
        assert_eq!(
            states,
            [
                Some(SendState::Queued),
                Some(SendState::Sending),
                Some(SendState::Sent),
                None
            ]
        );
        assert!(store.check().recv_blocking().unwrap().is_empty());
    }
}

#[test]
fn transient_failures_retry_the_same_bytes_and_permanent_ones_come_back() {
    let cx = server();
    let store = &cx.store;
    let events = store.subscribe();
    let id = save(store, None, draft(cx.account, 1), true);
    store.queue_send(id, NOW).recv_blocking().unwrap();
    // Before DATA: nothing recorded, tried again later.
    let failed = store
        .send_failed(
            id,
            SendFailed::Transient {
                retry_at: NOW + 30_000,
            },
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(failed.state, Some(SendState::Queued));
    assert!(due(store, cx.account, NOW + 29_999).is_empty());
    let retry = store
        .due_sends(cx.account, NOW + 30_000)
        .recv_blocking()
        .unwrap();
    assert_eq!(retry.sends[0].prepared, None);
    // A 4xx reply to the data: the retry must send the recorded bytes.
    store
        .send_started(id, prepared(1), NOW + 30_000)
        .recv_blocking()
        .unwrap();
    store
        .send_failed(
            id,
            SendFailed::Transient {
                retry_at: NOW + 60_000,
            },
        )
        .recv_blocking()
        .unwrap();
    let retry = store
        .due_sends(cx.account, NOW + 60_000)
        .recv_blocking()
        .unwrap();
    assert_eq!(retry.sends[0].prepared, Some(prepared(1)));
    assert_eq!(retry.sends[0].attempts, 1);
    assert_eq!(
        store
            .send_started(id, prepared(2), NOW + 60_000)
            .recv_blocking()
            .unwrap(),
        SendStarted::Mismatch
    );
    assert_eq!(
        store
            .send_started(id, prepared(1), NOW + 60_000)
            .recv_blocking()
            .unwrap(),
        SendStarted::Started
    );
    // Refused for good: an editable draft again, with the reason.
    let refused = store
        .send_failed(id, SendFailed::Permanent(SendFailure::RecipientsRefused))
        .recv_blocking()
        .unwrap();
    assert!(refused.applied && refused.state.is_none());
    let mut orphaned = refused.orphaned_blobs;
    orphaned.sort();
    assert_eq!(orphaned, [key(101), key(151)]);
    let stored = store.draft(id).recv_blocking().unwrap().unwrap();
    assert_eq!(stored.send, None);
    assert_eq!(stored.failure, Some(SendFailure::RecipientsRefused));
    let last = *outbox_changes(&events).last().unwrap();
    assert_eq!(
        (last.state, last.failure),
        (None, Some(SendFailure::RecipientsRefused))
    );
    // Queued again, the failure clears.
    store.queue_send(id, NOW).recv_blocking().unwrap();
    assert_eq!(
        store.draft(id).recv_blocking().unwrap().unwrap().failure,
        None
    );
}

#[test]
fn a_send_left_sending_is_uncertain_after_a_restart_and_waits_for_the_user() {
    let cx = server();
    let path = cx.path();
    let ids: Vec<DraftId> = (1..=3)
        .map(|n| save(&cx.store, None, draft(cx.account, n), true))
        .collect();
    for (n, id) in ids.iter().enumerate() {
        cx.store.queue_send(*id, NOW).recv_blocking().unwrap();
        cx.store
            .send_started(*id, prepared(n as u8), NOW)
            .recv_blocking()
            .unwrap();
    }
    // Stopped hard while all three were sending.
    drop(cx.store);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert!(due(&store, cx.account, NOW + 60_000).is_empty());
    let recovered = store.recover_sends().recv_blocking().unwrap();
    assert_eq!(recovered.len(), 3);
    assert!(
        recovered
            .iter()
            .all(|change| change.state == Some(SendState::Uncertain))
    );
    assert!(store.recover_sends().recv_blocking().unwrap().is_empty());
    // The window names each by its subject and when it went (issue #2).
    let outbox = store.outbox(None).recv_blocking().unwrap();
    assert_eq!(outbox.len(), 3);
    for entry in &outbox {
        let saved = store.draft(entry.draft).recv_blocking().unwrap().unwrap();
        assert_eq!(entry.subject, saved.draft.subject);
        assert_eq!(entry.started, Some(NOW));
        assert!(!format!("{entry:?}").contains(&entry.subject));
    }
    assert!(due(&store, cx.account, NOW + 3_600_000).is_empty());
    assert_eq!(
        store.undo_send(ids[0]).recv_blocking().unwrap(),
        SendUndone::TooLate(SendState::Uncertain)
    );
    assert!(
        !store
            .send_failed(ids[0], SendFailed::Transient { retry_at: NOW })
            .recv_blocking()
            .unwrap()
            .applied
    );
    // Send again: the same bytes.
    store
        .resolve_uncertain(ids[0], Resolution::SendAgain { not_before: NOW }, NOW)
        .recv_blocking()
        .unwrap();
    let again = store.due_sends(cx.account, NOW).recv_blocking().unwrap();
    assert_eq!(again.sends.len(), 1);
    assert_eq!(again.sends[0].prepared, Some(prepared(0)));
    // It was sent: recorded as sent, its Sent copy due.
    let was_sent = store
        .resolve_uncertain(ids[1], Resolution::WasSent, NOW)
        .recv_blocking()
        .unwrap();
    assert_eq!(was_sent.state, Some(SendState::Sent));
    // Back to a draft: editable, marked open to reopen.
    let back = store
        .resolve_uncertain(ids[2], Resolution::BackToDraft, NOW)
        .recv_blocking()
        .unwrap();
    assert!(back.applied);
    assert_eq!(state(&store, ids[2]), None);
    assert_eq!(store.open_drafts().recv_blocking().unwrap(), [ids[2]]);
    assert!(store.check().recv_blocking().unwrap().is_empty());
}

/// A message whose build stopped Sealwax (M9, docs/m9/depth.md) isn't built again by itself at
/// the next start: the send comes back as a draft that failed, never due, nothing sent; one
/// whose build returned goes on as usual.
#[test]
fn a_send_left_being_built_fails_at_the_next_start_and_keeps_its_draft() {
    let cx = server();
    let path = cx.path();
    let ids: Vec<DraftId> = (1..=3)
        .map(|n| save(&cx.store, None, draft(cx.account, n), false))
        .collect();
    for id in &ids {
        cx.store.queue_send(*id, NOW).recv_blocking().unwrap();
    }
    for id in &ids[..2] {
        assert!(cx.store.send_building(*id, NOW).recv_blocking().unwrap());
    }
    // The second's build returned (a refusal or a transient failure would follow).
    cx.store.send_built(ids[1]).recv_blocking().unwrap();
    // Sealwax died while the first's was being built.
    drop(cx.store);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let recovered = store.recover_sends().recv_blocking().unwrap();
    assert_eq!(
        recovered,
        [OutboxChange {
            account: cx.account,
            draft: ids[0],
            state: None,
            failure: Some(SendFailure::Interrupted),
        }]
    );
    assert!(store.recover_sends().recv_blocking().unwrap().is_empty());
    assert_eq!(due(&store, cx.account, NOW + 3_600_000), ids[1..]);
    let back = store.draft(ids[0]).recv_blocking().unwrap().unwrap();
    assert_eq!(
        (back.send, back.failure, back.open),
        (None, Some(SendFailure::Interrupted), false)
    );
    assert_eq!(back.draft, draft(cx.account, 1));
    // The user's draft to edit, discard or send again: sent again, it is built again.
    save(&store, Some(ids[0]), draft(cx.account, 1), true);
    store.queue_send(ids[0], NOW).recv_blocking().unwrap();
    assert_eq!(due(&store, cx.account, NOW).len(), 3);
    assert_eq!(
        store
            .draft(ids[0])
            .recv_blocking()
            .unwrap()
            .unwrap()
            .failure,
        None
    );
    // Undone meanwhile: nothing to build.
    store.undo_send(ids[2]).recv_blocking().unwrap();
    assert!(!store.send_building(ids[2], NOW).recv_blocking().unwrap());
    assert!(store.check().recv_blocking().unwrap().is_empty());
}

/// The same for a draft's Drafts-folder copy: left being built, it isn't built again at the next
/// start until the draft changes.
#[test]
fn a_drafts_copy_left_being_built_waits_for_the_draft_to_change() {
    let cx = server();
    let path = cx.path();
    let id = save(&cx.store, None, draft(cx.account, 1), false);
    let later = NOW + 60_000;
    let uploads = |store: &Store| -> Vec<DraftId> {
        store
            .due_drafts(cx.account, later)
            .recv_blocking()
            .unwrap()
            .uploads
            .iter()
            .map(|upload| upload.draft)
            .collect()
    };
    assert_eq!(uploads(&cx.store), [id]);
    assert!(cx.store.draft_building(id, 1).recv_blocking().unwrap());
    drop(cx.store);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert!(store.recover_sends().recv_blocking().unwrap().is_empty());
    assert!(uploads(&store).is_empty());
    save(&store, Some(id), draft(cx.account, 1), false);
    assert_eq!(uploads(&store), [id]);
    // A build that returned leaves nothing behind.
    assert!(store.draft_building(id, 2).recv_blocking().unwrap());
    store.draft_built(id).recv_blocking().unwrap();
    store.recover_sends().recv_blocking().unwrap();
    assert_eq!(uploads(&store), [id]);
}

/// v12 adds the markers to a v11 store with a queued send, which stays due.
#[test]
fn v12_migrates_a_v11_store_with_a_queued_send() {
    let cx = server();
    let path = cx.path();
    let id = save(&cx.store, None, draft(cx.account, 1), false);
    cx.store.queue_send(id, NOW).recv_blocking().unwrap();
    cx.store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(super::schema::UNDO_V13).unwrap();
    conn.execute_batch(
        "ALTER TABLE outbox DROP COLUMN building; ALTER TABLE drafts DROP COLUMN upload_building; \
         DELETE FROM schema_migrations WHERE version >= 12; PRAGMA user_version = 11;",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert!(store.recover_sends().recv_blocking().unwrap().is_empty());
    assert_eq!(due(&store, cx.account, NOW), [id]);
    assert!(store.send_building(id, NOW).recv_blocking().unwrap());
    assert!(store.check().recv_blocking().unwrap().is_empty());
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

fn drafts_message(folder: FolderId, uid: u32, message_id: &str) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        flags: Flags::SEEN | Flags::DRAFT,
        date: 1_790_000_000 + i64::from(uid),
        headers: Headers {
            from: vec![Address::new("Me", "me@example.test")],
            to: vec![Address::new("Kari", "kari@example.test")],
            subject: "Draft".into(),
            message_id: Some(message_id.into()),
            ..Headers::default()
        },
        size: 100,
        ..NewMessage::default()
    }
}

#[test]
fn drafts_upload_when_quiet_or_closed_and_replace_their_copy() {
    let cx = server();
    let store = &cx.store;
    let id = save(store, None, draft(cx.account, 1), true);
    let early = store
        .due_drafts(cx.account, NOW + 1_000)
        .recv_blocking()
        .unwrap();
    assert!(early.uploads.is_empty());
    assert_eq!(early.next, Some(NOW + DRAFT_QUIET_MS));
    let first = store
        .due_drafts(cx.account, NOW + DRAFT_QUIET_MS)
        .recv_blocking()
        .unwrap();
    assert_eq!(first.uploads.len(), 1);
    let upload = &first.uploads[0];
    assert_eq!((upload.revision, upload.folder.folder), (1, cx.drafts));
    assert_eq!((upload.retry_from, upload.replaces), (None, None));
    store
        .draft_upload_started(id, 1, cx.drafts, UIDVALIDITY, 10)
        .recv_blocking()
        .unwrap();
    store
        .draft_upload_done(id, 1, Some(10))
        .recv_blocking()
        .unwrap();
    assert!(
        store
            .due_drafts(cx.account, NOW + 3_600_000)
            .recv_blocking()
            .unwrap()
            .uploads
            .is_empty()
    );

    // Changed and closed: due once it has been quiet a little while (the quick reply keeps its
    // draft closed while the user types); an attempt cut short is retried from its mark.
    let mut edited = draft(cx.account, 1);
    edited.subject = "Edited".into();
    save(store, Some(id), edited, false);
    let closed = store.due_drafts(cx.account, NOW).recv_blocking().unwrap();
    assert!(closed.uploads.is_empty());
    assert_eq!(closed.next, Some(NOW + CLOSED_DRAFT_QUIET_MS));
    let quiet = NOW + CLOSED_DRAFT_QUIET_MS;
    let second = store.due_drafts(cx.account, quiet).recv_blocking().unwrap();
    assert_eq!(second.uploads.len(), 1);
    store
        .draft_upload_started(id, 2, cx.drafts, UIDVALIDITY, 11)
        .recv_blocking()
        .unwrap();
    let retry = store.due_drafts(cx.account, quiet).recv_blocking().unwrap();
    assert_eq!(retry.uploads[0].retry_from, Some(11));
    assert_eq!(retry.uploads[0].attempts, 1);
    assert_eq!(retry.uploads[0].replaces.unwrap().uid, Some(10));
    store
        .draft_upload_started(id, 2, cx.drafts, UIDVALIDITY, 12)
        .recv_blocking()
        .unwrap();
    let done = store
        .draft_upload_done(id, 2, Some(12))
        .recv_blocking()
        .unwrap();
    assert!(done.applied && !done.ops_queued);
    // The old copy isn't in the store: the actor deletes it by UID.
    let deletions = store.due_drafts(cx.account, NOW).recv_blocking().unwrap();
    assert_eq!(deletions.deletions.len(), 1);
    assert_eq!(deletions.deletions[0].target, DeletionTarget::Uid(10));
    assert_eq!(deletions.deletions[0].folder, cx.drafts);

    // Sync brings both copies before the deletion ran: the stale one never shows.
    let rows = arrive(
        store,
        cx.drafts,
        vec![
            drafts_message(cx.drafts, 10, "d1@example.test"),
            drafts_message(cx.drafts, 12, "d1@example.test"),
        ],
    );
    let shown = whole(store, ViewSpec::folder(cx.drafts));
    assert_eq!(
        shown.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        [rows[1]]
    );
    let ops = store.due_ops(cx.account, NOW).recv_blocking().unwrap();
    assert_eq!(ops.groups.len(), 1);
    assert_eq!(ops.groups[0].action, OpAction::Expunge);
    assert_eq!(ops.groups[0].messages[0].uid, 10);
    assert!(
        store
            .due_drafts(cx.account, NOW)
            .recv_blocking()
            .unwrap()
            .deletions
            .is_empty()
    );
    // The synced copy is the local draft.
    assert_eq!(
        store.draft_for_message(rows[1]).recv_blocking().unwrap(),
        Some(id)
    );
    assert_eq!(
        store.draft_for_message(rows[0]).recv_blocking().unwrap(),
        Some(id)
    );
    let inbox = arrive(store, cx.inbox, vec![message(cx.inbox, 3, Flags::NONE)]);
    assert_eq!(
        store.draft_for_message(inbox[0]).recv_blocking().unwrap(),
        None
    );

    // A third upload replaces the synced copy through the op queue: hidden at once.
    let mut third = draft(cx.account, 1);
    third.subject = "Third".into();
    save(store, Some(id), third, false);
    store
        .draft_upload_started(id, 3, cx.drafts, UIDVALIDITY, 13)
        .recv_blocking()
        .unwrap();
    let done = store
        .draft_upload_done(id, 3, Some(13))
        .recv_blocking()
        .unwrap();
    assert!(done.ops_queued);
    assert!(whole(store, ViewSpec::folder(cx.drafts)).rows.is_empty());
    assert!(store.check().recv_blocking().unwrap().is_empty());
}

#[test]
fn discarded_and_sent_drafts_delete_every_copy() {
    let cx = server();
    let store = &cx.store;
    // Discarded with a synced copy and an upload under way.
    let discarded = save(store, None, draft(cx.account, 1), false);
    store
        .draft_upload_started(discarded, 1, cx.drafts, UIDVALIDITY, 10)
        .recv_blocking()
        .unwrap();
    store
        .draft_upload_done(discarded, 1, Some(10))
        .recv_blocking()
        .unwrap();
    let mut edited = draft(cx.account, 1);
    edited.subject = "Edited".into();
    save(store, Some(discarded), edited, false);
    store
        .draft_upload_started(discarded, 2, cx.drafts, UIDVALIDITY, 11)
        .recv_blocking()
        .unwrap();
    arrive(
        store,
        cx.drafts,
        vec![drafts_message(cx.drafts, 10, "d1@example.test")],
    );
    let gone = store.discard_draft(discarded).recv_blocking().unwrap();
    assert!(gone.server_copy);
    assert!(whole(store, ViewSpec::folder(cx.drafts)).rows.is_empty());
    let due_drafts = store.due_drafts(cx.account, NOW).recv_blocking().unwrap();
    assert_eq!(
        due_drafts
            .deletions
            .iter()
            .map(|d| d.target.clone())
            .collect::<Vec<_>>(),
        [DeletionTarget::Search {
            message_id: "d1@example.test".into(),
            from_uid: 11,
            below_uid: None
        }]
    );
    // The upload lands after the discard: nothing to record, its copy is found by the search,
    // and hidden if sync brings it first.
    assert!(
        !store
            .draft_upload_done(discarded, 2, Some(11))
            .recv_blocking()
            .unwrap()
            .applied
    );
    arrive(
        store,
        cx.drafts,
        vec![drafts_message(cx.drafts, 11, "d1@example.test")],
    );
    assert!(whole(store, ViewSpec::folder(cx.drafts)).rows.is_empty());
    let ids: Vec<DeletionId> = due_drafts.deletions.iter().map(|d| d.id).collect();
    store.draft_deletions_done(ids).recv_blocking().unwrap();

    // Sent with a copy: deleted in the transaction that records the send.
    let sent = save(store, None, draft(cx.account, 2), false);
    store
        .draft_upload_started(sent, 1, cx.drafts, UIDVALIDITY, 20)
        .recv_blocking()
        .unwrap();
    store
        .draft_upload_done(sent, 1, Some(20))
        .recv_blocking()
        .unwrap();
    store.queue_send(sent, NOW).recv_blocking().unwrap();
    assert!(
        store
            .due_drafts(cx.account, NOW + 3_600_000)
            .recv_blocking()
            .unwrap()
            .uploads
            .is_empty(),
        "a queued draft isn't uploaded"
    );
    store
        .send_started(sent, prepared(2), NOW)
        .recv_blocking()
        .unwrap();
    let recorded = store.send_done(sent, NOW).recv_blocking().unwrap();
    assert!(recorded.copies_to_delete);
    let due_drafts = store.due_drafts(cx.account, NOW).recv_blocking().unwrap();
    assert_eq!(due_drafts.deletions.len(), 1);
    assert_eq!(due_drafts.deletions[0].target, DeletionTarget::Uid(20));
    assert!(store.check().recv_blocking().unwrap().is_empty());
}

fn mail(folder: FolderId, uid: u32, from: Address, to: Vec<Address>, id: &str) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        date: 1_790_000_000 + i64::from(uid) * 60,
        headers: Headers {
            from: vec![from],
            to,
            subject: format!("Mail {uid}"),
            message_id: Some(id.into()),
            ..Headers::default()
        },
        size: 100,
        ..NewMessage::default()
    }
}

fn suggest(store: &Store, account: AccountId, prefix: &str) -> Vec<(String, u32, u32)> {
    store
        .recipients(account, prefix, 10)
        .recv_blocking()
        .unwrap()
        .into_iter()
        .map(|contact| (contact.address, contact.seen, contact.written))
        .collect()
}

pub(super) fn contacts_mail(cx: &Cx) {
    let me = || Address::new("Me", "me@example.test");
    let kari = || Address::new("Kari Nordmann", "kari@example.test");
    let karl = || Address::new("Karl Berg", "karl.berg@example.test");
    let ase = || Address::new("Åse Bråten", "ase@braten.example.test");
    let news = || Address::new("News", "news@example.test");
    let mut inbox = vec![
        mail(cx.inbox, 1, kari(), vec![me()], "c1@example.test"),
        mail(cx.inbox, 2, ase(), vec![me(), karl()], "c2@example.test"),
        mail(cx.inbox, 3, karl(), vec![me()], "c3@example.test"),
    ];
    for uid in 4..10 {
        inbox.push(mail(
            cx.inbox,
            uid,
            news(),
            vec![me()],
            &format!("n{uid}@example.test"),
        ));
    }
    arrive(&cx.store, cx.inbox, inbox);
    // The same message filed twice (a Gmail label), and the user's own mail.
    arrive(
        &cx.store,
        cx.drafts,
        vec![mail(cx.drafts, 1, kari(), vec![me()], "c1@example.test")],
    );
    arrive(
        &cx.store,
        cx.sent,
        vec![
            mail(cx.sent, 1, me(), vec![ase()], "s1@example.test"),
            mail(cx.sent, 2, me(), vec![ase(), kari()], "s2@example.test"),
        ],
    );
}

#[test]
fn contacts_rank_the_users_correspondents_first_and_match_prefixes() {
    let cx = server();
    contacts_mail(&cx);
    let store = &cx.store;
    assert_eq!(
        suggest(store, cx.account, "kar"),
        [
            ("kari@example.test".to_owned(), 2, 1),
            ("karl.berg@example.test".to_owned(), 2, 0)
        ]
    );
    // The domain, a name word, diacritics folded, any case.
    assert_eq!(
        suggest(store, cx.account, "BRATEN")[0].0,
        "ase@braten.example.test"
    );
    assert_eq!(
        suggest(store, cx.account, "Bråt")[0].0,
        "ase@braten.example.test"
    );
    assert_eq!(
        suggest(store, cx.account, "nordm")[0].0,
        "kari@example.test"
    );
    // Written to most first, then seen most.
    assert_eq!(
        suggest(store, cx.account, "example.test")
            .into_iter()
            .map(|(address, _, _)| address)
            .collect::<Vec<_>>(),
        [
            "kari@example.test",
            "news@example.test",
            "karl.berg@example.test"
        ]
    );
    assert_eq!(
        suggest(store, cx.account, "ase")[0],
        ("ase@braten.example.test".into(), 3, 2)
    );
    assert!(
        suggest(store, cx.account, "me@").is_empty(),
        "the own address is left out"
    );
    assert!(suggest(store, cx.account, "").is_empty());
    assert!(suggest(store, cx.account, "zz").is_empty());
    let found = store
        .recipients(cx.account, "kari", 1)
        .recv_blocking()
        .unwrap();
    assert_eq!(found[0].name, "Kari Nordmann");

    store.remove_account(cx.account).recv_blocking().unwrap();
    let conn = Connection::open(cx.path()).unwrap();
    let left: i64 = conn
        .query_row(
            "SELECT (SELECT count(*) FROM contacts) + (SELECT count(*) FROM contact_terms)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn v9_migrates_a_v8_store_with_mail_and_fills_the_contacts() {
    let cx = server();
    contacts_mail(&cx);
    let path = cx.path();
    let (account, inbox) = (cx.account, cx.inbox);
    cx.store.close().unwrap();
    let rows = |conn: &Connection| -> Vec<(i64, String, String, i64, i64, i64)> {
        conn.prepare(
            "SELECT account_id, address, name, seen, written, last FROM contacts \
             ORDER BY account_id, address",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    let conn = Connection::open(&path).unwrap();
    let kept = rows(&conn);
    assert_eq!(kept.len(), 4);
    // Back to v8: what M5 left on disk, with mail in it.
    conn.execute_batch(super::schema::UNDO_V13).unwrap();
    conn.execute_batch(
        "DROP TABLE blob_deletions; DROP TABLE pop3_uidls; DROP TABLE pop3_accounts; \
         DROP TABLE contact_terms; DROP TABLE contacts; DROP TABLE draft_deletions; \
         DROP TABLE outbox; DROP TABLE draft_blobs; DROP TABLE draft_parts; DROP TABLE drafts; \
         DELETE FROM schema_migrations WHERE version >= 9; PRAGMA user_version = 8;",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(
        suggest(&store, account, "kar"),
        [
            ("kari@example.test".to_owned(), 2, 1),
            ("karl.berg@example.test".to_owned(), 2, 0)
        ]
    );
    assert!(store.check().recv_blocking().unwrap().is_empty());
    let id = save(&store, None, draft(account, 1), true);
    assert!(store.draft(id).recv_blocking().unwrap().is_some());
    store.close().unwrap();
    let conn = Connection::open(&path).unwrap();
    assert_eq!(rows(&conn), kept, "the backfill equals what ingest kept");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(held_ids(&store, inbox).len(), 9);
}

#[test]
fn a_quick_reply_finds_its_closed_reply_draft_again() {
    let cx = server();
    let store = &cx.store;
    let source = arrive(store, cx.inbox, vec![message(cx.inbox, 7, Flags::SEEN)])[0];
    let reply = |kind| Draft {
        source: Some(DraftSource {
            message: source,
            kind,
        }),
        ..draft(cx.account, 1)
    };
    let found = |store: &Store| {
        store
            .reply_draft(source)
            .recv_blocking()
            .unwrap()
            .map(|stored| stored.id)
    };
    // Open in a composer window, a forward, or no source: not the quick reply's.
    let open = save(store, None, reply(ReplyKind::Reply), true);
    save(store, None, reply(ReplyKind::Forward), false);
    save(store, None, draft(cx.account, 2), false);
    assert_eq!(found(store), None);
    // Closed replies: the newest; one in the outbox doesn't count.
    let older = save(store, None, reply(ReplyKind::Reply), false);
    assert_eq!(found(store), Some(older));
    let newer = save(store, None, reply(ReplyKind::Reply), false);
    assert_eq!(found(store), Some(newer));
    store
        .queue_send(newer, NOW + 7_000)
        .recv_blocking()
        .unwrap();
    assert_eq!(found(store), Some(older));
    // Closing the composer's draft makes it a candidate; the newest saved (then the highest id)
    // still wins.
    store.set_draft_open(open, false).recv_blocking().unwrap();
    assert_eq!(found(store), Some(older));
    store.undo_send(newer).recv_blocking().unwrap();
    store.set_draft_open(newer, false).recv_blocking().unwrap();
    assert_eq!(found(store), Some(newer));
}
