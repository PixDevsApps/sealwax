//! The reader's read of a conversation (ML-057): order, copies, the Trash and Junk, rows the
//! overlay hides, merges and messages that arrive later, through the store's own threading.

use super::ops_tests::{arrive, server};
use super::tests::scratch;
use super::*;
use sealwax_domain::model::{Address, FolderRole, Headers};
use sealwax_domain::triage::Triage;

fn open(dir: &Path) -> Store {
    Store::open(&dir.join("store.sqlite"), StoreOptions::default()).unwrap()
}

struct Local {
    _dir: tempfile::TempDir,
    store: Store,
    inbox: FolderId,
    archive: FolderId,
    sent: FolderId,
    trash: FolderId,
    junk: FolderId,
}

fn local() -> Local {
    let dir = scratch();
    let store = open(dir.path());
    let account = store
        .add_account(NewAccount {
            name: "Home".into(),
            address: "kari@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let folder = |name: &str, role| {
        store
            .upsert_folder(FolderSpec::local(account, name, role))
            .recv_blocking()
            .unwrap()
            .value
    };
    let inbox = folder("INBOX", FolderRole::Inbox);
    let archive = folder("Archive", FolderRole::Archive);
    let sent = folder("Sent", FolderRole::Sent);
    let trash = folder("Trash", FolderRole::Trash);
    let junk = folder("Junk", FolderRole::Junk);
    Local {
        _dir: dir,
        store,
        inbox,
        archive,
        sent,
        trash,
        junk,
    }
}

/// A message with Message-ID `<id@example.test>` answering `parent`, at `minute`.
fn mail(folder: FolderId, uid: u32, id: &str, parent: Option<&str>, minute: i64) -> NewMessage {
    let parent = parent.map(|parent| format!("{parent}@example.test"));
    NewMessage {
        folder,
        uid: Some(uid),
        date: 1_790_000_000 + minute * 60,
        headers: Headers {
            from: vec![Address::new("Ola Nordmann", "ola@example.test")],
            subject: format!("Re: Hyttetur ({id})"),
            message_id: Some(format!("{id}@example.test")),
            in_reply_to: parent.iter().cloned().collect(),
            references: parent.into_iter().collect(),
            ..Headers::default()
        },
        snippet: format!("snippet {id}"),
        search_text: format!("text {id}"),
        size: 100,
        ..NewMessage::default()
    }
}

fn ingest(store: &Store, messages: Vec<NewMessage>) -> Vec<MessageId> {
    store
        .ingest(messages)
        .recv_blocking()
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

fn conversation(store: &Store, id: MessageId) -> Conversation {
    store
        .conversation(id)
        .recv_blocking()
        .unwrap()
        .value
        .unwrap()
}

fn shown(conversation: &Conversation) -> Vec<MessageId> {
    conversation
        .messages
        .iter()
        .map(|message| message.summary.id)
        .collect()
}

#[test]
fn oldest_first_by_date_then_id_with_each_folders_role() {
    let fx = local();
    // Arrival order isn't date order: a first sync goes newest first.
    let ids = ingest(
        &fx.store,
        vec![
            mail(fx.inbox, 3, "c", Some("b"), 30),
            mail(fx.sent, 1, "b", Some("a"), 20),
            mail(fx.inbox, 1, "a", None, 10),
            mail(fx.inbox, 2, "tie", Some("a"), 30),
            mail(fx.inbox, 4, "alone", None, 15),
        ],
    );
    let (c, b, a, tie, alone) = (ids[0], ids[1], ids[2], ids[3], ids[4]);
    let found = conversation(&fx.store, b);
    assert_eq!(found.selected, b);
    assert_eq!(shown(&found), [a, b, c.min(tie), c.max(tie)]);
    assert_eq!(found.messages[1].role, FolderRole::Sent);
    assert_eq!(found.messages[0].role, FolderRole::Inbox);
    assert_eq!(found.messages[0].summary.snippet, "snippet a");
    assert!(found.left_out.is_empty());
    assert!(
        found
            .messages
            .iter()
            .all(|m| m.summary.thread == found.thread)
    );
    assert_eq!(shown(&conversation(&fx.store, alone)), [alone]);
    assert!(
        fx.store
            .conversation(MessageId(9_999))
            .recv_blocking()
            .unwrap()
            .value
            .is_none()
    );
    fx.store.close().unwrap();
}

#[test]
fn copies_show_once_the_selected_copy_first_then_by_role() {
    let fx = local();
    let ids = ingest(
        &fx.store,
        vec![
            mail(fx.inbox, 1, "root", None, 10),
            mail(fx.archive, 1, "root", None, 10),
            mail(fx.sent, 1, "mine", Some("root"), 20),
            mail(fx.inbox, 2, "mine", Some("root"), 21),
        ],
    );
    let (inbox_root, archive_root, sent_mine, inbox_mine) = (ids[0], ids[1], ids[2], ids[3]);
    let found = conversation(&fx.store, inbox_mine);
    assert_eq!(shown(&found), [inbox_root, inbox_mine]);
    assert_eq!(found.messages[0].copies, [archive_root]);
    assert_eq!(found.messages[1].copies, [sent_mine]);
    // Selected in the Archive, that copy shows; the Inbox copy of the reply wins over Sent.
    let found = conversation(&fx.store, archive_root);
    assert_eq!(shown(&found), [archive_root, inbox_mine]);
    assert_eq!(found.messages[0].copies, [inbox_root]);
    let found = conversation(&fx.store, sent_mine);
    assert_eq!(shown(&found), [inbox_root, sent_mine]);
    assert_eq!(found.members().count(), 4);
    fx.store.close().unwrap();
}

#[test]
fn the_trash_and_junk_show_only_when_the_selected_message_is_there() {
    let fx = local();
    let ids = ingest(
        &fx.store,
        vec![
            mail(fx.inbox, 1, "root", None, 10),
            mail(fx.trash, 1, "binned", Some("root"), 20),
            mail(fx.junk, 1, "spammy", Some("root"), 30),
            mail(fx.inbox, 2, "last", Some("root"), 40),
            mail(fx.trash, 2, "root", None, 10),
        ],
    );
    let (root, binned, spammy, last, root_in_trash) = (ids[0], ids[1], ids[2], ids[3], ids[4]);
    let found = conversation(&fx.store, last);
    assert_eq!(shown(&found), [root, last]);
    assert_eq!(found.left_out, [binned, spammy, root_in_trash]);
    for selected in [binned, spammy] {
        let found = conversation(&fx.store, selected);
        assert_eq!(shown(&found), [root, binned, spammy, last], "{selected:?}");
        assert_eq!(found.messages[0].copies, [root_in_trash]);
    }
    // The selected copy in the Trash shows in place of the Inbox's.
    let found = conversation(&fx.store, root_in_trash);
    assert_eq!(shown(&found), [root_in_trash, binned, spammy, last]);
    fx.store.close().unwrap();
}

#[test]
fn rows_the_overlay_hides_stay_out_and_flags_are_the_overlays() {
    let fx = server();
    let ids = arrive(
        &fx.store,
        fx.inbox,
        vec![
            mail(fx.inbox, 1, "root", None, 10),
            mail(fx.inbox, 2, "moved", Some("root"), 20),
            mail(fx.inbox, 3, "deleted", Some("root"), 30),
            mail(fx.inbox, 4, "starred", Some("root"), 40),
        ],
    );
    let (root, moved, deleted, starred) = (ids[0], ids[1], ids[2], ids[3]);
    // A pending archive (inside its undo window) hides the row; so does `\Deleted`.
    fx.store
        .queue_triage(QueueTriage {
            action: Triage::Archive,
            messages: vec![moved],
            not_before: i64::MAX,
        })
        .recv_blocking()
        .unwrap();
    fx.store
        .set_flags(vec![deleted], Flags::DELETED, Flags::NONE)
        .recv_blocking()
        .unwrap();
    fx.store
        .queue_triage(QueueTriage {
            action: Triage::Star(true),
            messages: vec![starred],
            not_before: 0,
        })
        .recv_blocking()
        .unwrap();
    let found = conversation(&fx.store, root);
    assert_eq!(shown(&found), [root, starred]);
    assert_eq!(found.left_out, [moved, deleted]);
    assert!(found.messages[1].summary.flags.is_flagged());
    // The selected message shows even while hidden: the reader keeps it until the list moves.
    let found = conversation(&fx.store, moved);
    assert_eq!(shown(&found), [root, moved, starred]);
    assert_eq!(found.left_out, [deleted]);
    fx.store.close().unwrap();
}

#[test]
fn a_merge_joins_two_conversations_and_a_late_parent_joins_its_replies() {
    let fx = local();
    let ids = ingest(
        &fx.store,
        vec![
            mail(fx.inbox, 1, "a2", Some("a1"), 20),
            mail(fx.inbox, 2, "b1", None, 15),
            mail(fx.inbox, 3, "b2", Some("b1"), 25),
        ],
    );
    let (a2, b1, b2) = (ids[0], ids[1], ids[2]);
    assert_eq!(shown(&conversation(&fx.store, a2)), [a2]);
    assert_eq!(shown(&conversation(&fx.store, b1)), [b1, b2]);
    // The parent arrives after its reply.
    let a1 = ingest(&fx.store, vec![mail(fx.inbox, 4, "a1", None, 10)])[0];
    assert_eq!(shown(&conversation(&fx.store, a2)), [a1, a2]);
    // A reply that references both merges them, and the change event says so.
    let events = fx.store.subscribe();
    let mut bridge = mail(fx.inbox, 5, "bridge", Some("a2"), 30);
    bridge.headers.references.push("b2@example.test".into());
    let bridge = ingest(&fx.store, vec![bridge])[0];
    let found = conversation(&fx.store, b1);
    assert_eq!(shown(&found), [a1, b1, a2, b2, bridge]);
    assert_eq!(conversation(&fx.store, a1).thread, found.thread);
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.inserted, [bridge]);
    assert!(
        event
            .threads
            .iter()
            .any(|count| count.thread == found.thread)
            && event
                .merged_threads
                .iter()
                .all(|merge| merge.into == found.thread),
        "{event:?}"
    );
    fx.store.close().unwrap();
}
