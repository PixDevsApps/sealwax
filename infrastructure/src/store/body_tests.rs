//! Downloaded bodies (ML-044): a header-only row upgraded in place, the full-text row replaced,
//! orphans after a crash between the blob and the commit, the disk cap's eviction, and the
//! bodies table following the rows.

use super::tests::scratch;
use super::*;
use rusqlite::Connection;
use sealwax_domain::account::AccountKey;
use sealwax_domain::message;
use sealwax_domain::model::FolderRole;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

const T0: i64 = 1_790_000_000;

/// A message whose text runs well past the first sync's 1 KB preview, ending in a word only
/// the whole body has, with a PDF attached.
pub(super) fn raw(n: u32) -> Vec<u8> {
    let filler = "fjord kyst skjærgård lyngheia tindeklatring \r\n".repeat(60);
    format!(
        "From: Avsender {n} <avsender{n}@example.test>\r\nTo: mottaker@example.test\r\n\
         Subject: Melding {n}\r\nDate: Mon, 05 Oct 2026 09:00:00 +0000\r\n\
         Message-ID: <body-{n}@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"b{n}\"\r\n\r\n\
         --b{n}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Hei nummer {n}.\r\n{filler}gulspurv{n} zebrafink\r\n\
         --b{n}\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\n\
         Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b{n}--\r\n"
    )
    .into_bytes()
}

/// The row the first sync stores: headers and the preview, no blob.
fn header_only(folder: FolderId, uid: u32, date: i64) -> NewMessage {
    let parsed = message::parse(&raw(uid));
    NewMessage {
        folder,
        uid: Some(uid),
        date,
        headers: parsed.headers,
        snippet: parsed.snippet,
        body_kind: parsed.body_kind,
        has_attachments: false,
        size: parsed.size,
        body_state: BodyState::Partial,
        search_text: message::truncate(&parsed.body_text, 1024).to_owned(),
        ..NewMessage::default()
    }
}

/// The upgrade the engine builds for a whole message it stored as `key`.
pub(super) fn upgrade(
    message: MessageId,
    uid: u32,
    key: BlobKey,
    raw: &[u8],
    used: i64,
) -> BodyUpgrade {
    let parsed = message::parse(raw);
    BodyUpgrade {
        message,
        uid,
        blob: key,
        blob_size: raw.len() as u64,
        body_state: BodyState::Full,
        body_kind: parsed.body_kind,
        has_attachments: parsed.has_attachments(),
        snippet: parsed.snippet.clone(),
        text_parts: parsed.text_parts.clone(),
        html_parts: parsed.html_parts.clone(),
        sections: parsed
            .attachments
            .iter()
            .map(|attachment| (attachment.part, "2".to_owned()))
            .collect(),
        attachments: parsed.attachments,
        search_text: parsed.body_text,
        used,
    }
}

struct Bodies {
    dir: tempfile::TempDir,
    store: Store,
    blobs: BlobStore,
    account: AccountId,
    inbox: FolderId,
}

fn open_at(dir: tempfile::TempDir) -> Bodies {
    let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
    let store = Store::open(
        &dir.path().join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let account = store
        .ensure_account(
            AccountKey("bodies".into()),
            NewAccount {
                name: "Bodies".into(),
                address: "bodies@example.test".into(),
            },
        )
        .recv_blocking()
        .unwrap()
        .value;
    let inbox = store
        .upsert_folder(FolderSpec::local(account, "INBOX", FolderRole::Inbox))
        .recv_blocking()
        .unwrap()
        .value;
    Bodies {
        dir,
        store,
        blobs,
        account,
        inbox,
    }
}

fn open() -> Bodies {
    open_at(scratch())
}

impl Bodies {
    fn ingest(&self, messages: Vec<NewMessage>) -> Vec<MessageId> {
        self.store
            .ingest(messages)
            .recv_blocking()
            .unwrap()
            .ids
            .into_iter()
            .map(Option::unwrap)
            .collect()
    }

    /// Stores message `uid`'s raw bytes and upgrades its row, as the engine does.
    fn download(&self, id: MessageId, uid: u32, used: i64) -> Upgraded {
        let raw = raw(uid);
        let key = self.blobs.put(&raw).unwrap();
        self.store
            .upgrade_body(upgrade(id, uid, key, &raw, used))
            .recv_blocking()
            .unwrap()
    }

    fn detail(&self, id: MessageId) -> MessageDetail {
        self.store
            .message(id)
            .recv_blocking()
            .unwrap()
            .value
            .unwrap()
    }

    fn found(&self, text: &str) -> Vec<MessageId> {
        self.store
            .search(text, None, 100)
            .recv_blocking()
            .unwrap()
            .value
            .iter()
            .map(|row| row.id)
            .collect()
    }

    fn usage(&self) -> BodyUsage {
        self.store.body_usage().recv_blocking().unwrap()
    }
}

fn view(store: &Store, spec: ViewSpec) -> ViewOpened {
    store
        .open_view(OpenView {
            spec,
            first_page: 100,
            section_starts: Vec::new(),
        })
        .recv_blocking()
        .unwrap()
}

#[test]
fn an_upgrade_keeps_the_id_and_place_and_announces_exactly_that_row() {
    let bodies = open();
    let ids = bodies.ingest(
        (1..=5)
            .map(|uid| header_only(bodies.inbox, uid, T0 + i64::from(uid)))
            .collect(),
    );
    let folder_view = view(&bodies.store, ViewSpec::folder(bodies.inbox));
    let clip_view = view(
        &bodies.store,
        ViewSpec::folder(bodies.inbox).with_filter(ViewFilter::Attachments),
    );
    assert_eq!(clip_view.total, 0);
    let before = bodies.detail(ids[2]);
    assert_eq!(before.blob, None);
    assert!(bodies.found("zebrafink").is_empty(), "past the preview");
    assert_eq!(bodies.found("gulspurv3"), Vec::<MessageId>::new());
    let events = bodies.store.subscribe();

    let upgraded = bodies.download(ids[2], 3, T0);
    assert!(upgraded.found);
    assert!(upgraded.orphaned_blobs.is_empty());
    assert_eq!(upgraded.version.0, folder_view.version.0 + 1);

    let event = events.try_recv().unwrap();
    assert!(events.try_recv().is_err(), "one commit, one event");
    assert_eq!(event.version, upgraded.version);
    assert_eq!(event.changed, [ids[2]]);
    assert!(event.inserted.is_empty() && event.removed.is_empty());
    assert!(event.folders.is_empty() && event.threads.is_empty());
    let diff = |view: &ViewOpened| {
        event
            .views
            .iter()
            .find(|change| change.view == view.view.id())
            .map(|change| change.diff.clone())
    };
    let ViewDiff::Splice {
        removed,
        inserted,
        changed,
        total,
    } = diff(&folder_view).unwrap()
    else {
        panic!("a splice");
    };
    assert!(removed.is_empty() && inserted.is_empty());
    assert_eq!(total, 5);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].position, 2, "newest first: uid 3 is third");
    assert_eq!(changed[0].row.id, ids[2]);
    assert!(changed[0].row.has_attachments);
    let ViewDiff::Splice {
        inserted, total, ..
    } = diff(&clip_view).unwrap()
    else {
        panic!("a splice");
    };
    assert_eq!(total, 1);
    assert_eq!(inserted[0].row.id, ids[2], "the clip view gains the row");

    let rows = bodies
        .store
        .page(ViewSpec::folder(bodies.inbox), 0, 10)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        rows.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        ids.iter().rev().copied().collect::<Vec<_>>()
    );
    let after = bodies.detail(ids[2]);
    assert_eq!(after.summary.thread, before.summary.thread);
    assert!(after.blob.is_some());
    assert_eq!(after.text_parts.len(), 1);
    assert_eq!(after.attachments.len(), 1);
    assert_eq!(after.attachments[0].name.as_deref(), Some("report.pdf"));
    assert_eq!(
        bodies.store.raw_message(ids[2]).recv_blocking().unwrap(),
        Some(raw(3))
    );
    assert_eq!(bodies.found("zebrafink"), [ids[2]]);
    assert_eq!(bodies.found("gulspurv3"), [ids[2]]);
    assert_eq!(
        bodies.found("Melding"),
        ids.iter().rev().copied().collect::<Vec<_>>()
    );
    let source = bodies
        .store
        .part_source(ids[2], after.attachments[0].part)
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert_eq!((source.uid, source.section.as_str()), (3, "2"));
    assert_eq!(source.mime_type, "application/pdf");
    assert_eq!(
        bodies.usage(),
        BodyUsage {
            bodies: 1,
            bytes: raw(3).len() as u64
        }
    );
    let targets = bodies
        .store
        .body_targets(vec![ids[2], MessageId(999), ids[0]])
        .recv_blocking()
        .unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].body_state, BodyState::Full);
    assert_eq!(
        (targets[1].uid, targets[1].body_state),
        (1, BodyState::Partial)
    );
    assert_eq!(targets[1].blob, None);
    assert_eq!(targets[1].account, bodies.account);
    assert_eq!(
        bodies.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
    bodies.store.close().unwrap();
}

#[test]
fn a_message_gone_before_its_upgrade_leaves_its_blob_to_delete() {
    let bodies = open();
    let ids = bodies.ingest(vec![
        header_only(bodies.inbox, 1, T0),
        header_only(bodies.inbox, 2, T0),
    ]);
    let wrong_uid = bodies.download(ids[0], 2, T0);
    assert!(!wrong_uid.found);
    assert_eq!(wrong_uid.orphaned_blobs, [BlobStore::key(&raw(2))]);
    bodies.store.remove(vec![ids[1]]).recv_blocking().unwrap();
    let gone = bodies.download(ids[1], 2, T0);
    assert!(!gone.found);
    assert_eq!(gone.orphaned_blobs, [BlobStore::key(&raw(2))]);
    assert_eq!(bodies.usage(), BodyUsage::default());
    assert_eq!(bodies.detail(ids[0]).blob, None);
    bodies.store.close().unwrap();
}

#[test]
fn bodies_leave_with_the_last_message_that_holds_them() {
    let bodies = open();
    let archive = bodies
        .store
        .upsert_folder(FolderSpec::local(
            bodies.account,
            "Arkiv",
            FolderRole::Archive,
        ))
        .recv_blocking()
        .unwrap()
        .value;
    let inbox = bodies.ingest(vec![
        header_only(bodies.inbox, 1, T0),
        header_only(bodies.inbox, 2, T0),
    ]);
    let copy = bodies.ingest(vec![header_only(archive, 1, T0)]);
    bodies.download(inbox[0], 1, T0);
    bodies.download(copy[0], 1, T0);
    bodies.download(inbox[1], 2, T0);
    assert_eq!(bodies.usage().bodies, 2, "the copy shares its body");
    let removed = bodies.store.remove(vec![inbox[0]]).recv_blocking().unwrap();
    assert!(removed.orphaned_blobs.is_empty());
    assert_eq!(bodies.usage().bodies, 2);
    let removed = bodies.store.remove(vec![copy[0]]).recv_blocking().unwrap();
    assert_eq!(removed.orphaned_blobs, [BlobStore::key(&raw(1))]);
    assert_eq!(bodies.usage().bodies, 1);
    bodies
        .store
        .reset_folder(bodies.inbox, 7)
        .recv_blocking()
        .unwrap();
    assert_eq!(bodies.usage(), BodyUsage::default(), "a UIDVALIDITY reset");

    let again = bodies.ingest(vec![header_only(bodies.inbox, 3, T0)]);
    bodies.download(again[0], 3, T0);
    assert_eq!(bodies.usage().bodies, 1);
    let removed = bodies
        .store
        .remove_account(bodies.account)
        .recv_blocking()
        .unwrap();
    assert_eq!(removed.orphaned_blobs, [BlobStore::key(&raw(3))]);
    assert_eq!(
        bodies.usage(),
        BodyUsage::default(),
        "the cascade of a removal"
    );
    bodies.store.close().unwrap();
}

#[test]
fn the_cap_evicts_the_bodies_used_longest_ago() {
    let bodies = open();
    let ids = bodies.ingest(
        (1..=4)
            .map(|uid| header_only(bodies.inbox, uid, T0))
            .collect(),
    );
    for (n, id) in ids.iter().enumerate() {
        bodies.download(*id, n as u32 + 1, T0 + n as i64 * 100);
    }
    let size = |uid: u32| raw(uid).len() as u64;
    let total: u64 = (1..=4).map(size).sum();
    assert_eq!(bodies.usage().bytes, total);
    bodies
        .store
        .touch_body(ids[0], T0 + 1_000)
        .recv_blocking()
        .unwrap();
    bodies
        .store
        .touch_body(ids[2], T0 + 210)
        .recv_blocking()
        .unwrap();
    let snippet = bodies.detail(ids[2]).summary.snippet;
    let events = bodies.store.subscribe();

    let nothing = bodies
        .store
        .evict_bodies(total, Vec::new())
        .recv_blocking()
        .unwrap();
    assert_eq!(
        nothing,
        Evicted {
            bytes: total,
            ..Evicted::default()
        }
    );

    // Used order: uid 2 (T0+100), uid 3 (T0+200; the touch 10 s later is within the slack),
    // uid 4 (T0+300), uid 1 (touched at T0+1000). Uid 2 is kept, so 3 goes first.
    let limit = total - size(3) / 2;
    let evicted = bodies
        .store
        .evict_bodies(limit, vec![ids[1]])
        .recv_blocking()
        .unwrap();
    assert_eq!(evicted.messages, [ids[2]]);
    assert_eq!(evicted.blobs, [BlobStore::key(&raw(3))]);
    assert_eq!(evicted.bytes, total - size(3));
    assert!(!evicted.more);
    assert!(events.try_recv().is_err(), "no list changes, no event");

    let gone = bodies.detail(ids[2]);
    assert_eq!(gone.blob, None);
    assert!(gone.text_parts.is_empty() && gone.attachments.is_empty());
    assert_eq!(gone.summary.snippet, snippet);
    assert!(gone.summary.has_attachments, "the row keeps its clip");
    assert_eq!(bodies.found("gulspurv3"), [ids[2]], "the search text stays");
    let targets = bodies
        .store
        .body_targets(vec![ids[2]])
        .recv_blocking()
        .unwrap();
    assert_eq!(targets[0].body_state, BodyState::Partial);

    let evicted = bodies
        .store
        .evict_bodies(0, vec![ids[0]])
        .recv_blocking()
        .unwrap();
    assert_eq!(evicted.messages, [ids[1], ids[3]]);
    assert_eq!(evicted.bytes, size(1), "the kept body stays above the cap");
    assert!(!evicted.more);
    assert_eq!(bodies.usage().bodies, 1);
    for key in &evicted.blobs {
        assert!(bodies.blobs.remove(key).unwrap());
    }
    let redownloaded = bodies.download(ids[2], 3, T0 + 2_000);
    assert!(redownloaded.found);
    assert_eq!(bodies.usage().bodies, 2);
    assert_eq!(
        bodies.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
    bodies.store.close().unwrap();
}

#[test]
fn prefetch_candidates_are_recent_inbox_mail_of_server_accounts() {
    let bodies = open();
    let sent = bodies
        .store
        .upsert_folder(FolderSpec::local(bodies.account, "Sendt", FolderRole::Sent))
        .recv_blocking()
        .unwrap()
        .value;
    let local = bodies
        .store
        .add_account(NewAccount {
            name: "Local".into(),
            address: "local@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    let local_inbox = bodies
        .store
        .upsert_folder(FolderSpec::local(local, "INBOX", FolderRole::Inbox))
        .recv_blocking()
        .unwrap()
        .value;
    let ids = bodies.ingest(vec![
        header_only(bodies.inbox, 1, T0 - 100),
        header_only(bodies.inbox, 2, T0),
        header_only(bodies.inbox, 3, T0 - 10 * 86_400),
        header_only(bodies.inbox, 4, T0 - 50),
        header_only(sent, 5, T0),
        header_only(local_inbox, 6, T0),
    ]);
    bodies.download(ids[3], 4, T0);
    let candidates = bodies
        .store
        .body_candidates(T0 - 86_400, 10)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        candidates.iter().map(|c| c.message).collect::<Vec<_>>(),
        [ids[1], ids[0]]
    );
    assert!(
        candidates
            .iter()
            .all(|c| c.role == FolderRole::Inbox && c.account == bodies.account)
    );
    assert_eq!(candidates[0].size, raw(2).len() as u64);
    let limited = bodies.store.body_candidates(0, 1).recv_blocking().unwrap();
    assert_eq!(limited.len(), 1);
    bodies.store.close().unwrap();
}

const CHILD_DIR: &str = "SEALWAX_BODY_CRASH_DIR";

/// Run by `kill_between_blob_and_commit_leaves_orphans_and_no_broken_row`: downloads bodies in a
/// loop, printing each blob it stored and each upgrade committed, and waiting between the two
/// for every tenth message, where the parent kills it.
#[test]
#[ignore = "run by kill_between_blob_and_commit_leaves_orphans_and_no_broken_row"]
fn body_crash_child() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let blobs = BlobStore::open(&dir.join("blobs")).unwrap();
    let store = Store::open(
        &dir.join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let targets = store
        .body_targets((1..=400).map(MessageId).collect())
        .recv_blocking()
        .unwrap();
    for target in targets.into_iter().filter(|target| target.blob.is_none()) {
        let raw = raw(target.uid);
        let fresh = !blobs.contains(&BlobStore::key(&raw));
        let key = blobs.put(&raw).unwrap();
        println!("stored {}", target.uid);
        if fresh && target.uid.is_multiple_of(10) {
            std::thread::sleep(Duration::from_secs(5));
        }
        store
            .upgrade_body(upgrade(target.message, target.uid, key, &raw, T0))
            .recv_blocking()
            .unwrap();
        println!("committed {}", target.uid);
    }
    println!("done");
}

#[test]
fn kill_between_blob_and_commit_leaves_orphans_and_no_broken_row() {
    let bodies = open();
    bodies.ingest(
        (1..=400)
            .map(|uid| header_only(bodies.inbox, uid, T0))
            .collect(),
    );
    let Bodies { dir, store, .. } = bodies;
    store.close().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut killed_at: Vec<u32> = Vec::new();
    let mut committed = Vec::new();
    for round in 0..4u64 {
        let mut child = Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "store::body_tests::body_crash_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_DIR, dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let paused = loop {
            let line = lines
                .next()
                .expect("the child stopped before pausing")
                .unwrap();
            if let Some(uid) = line.strip_prefix("committed ") {
                committed.push(uid.parse::<u32>().unwrap());
            } else if let Some(uid) = line.strip_prefix("stored ") {
                let uid: u32 = uid.parse().unwrap();
                if uid.is_multiple_of(10) && !killed_at.contains(&uid) {
                    break uid;
                }
            }
        };
        child.kill().unwrap();
        child.wait().unwrap();
        killed_at.push(paused);

        let path = dir.path().join("store.sqlite");
        let conn = Connection::open(&path).unwrap();
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok", "round {round}");
        conn.execute(
            "INSERT INTO message_fts(message_fts, rank) VALUES ('integrity-check', 0)",
            [],
        )
        .unwrap();
        let rows: Vec<(u32, Option<Vec<u8>>, i64)> = conn
            .prepare("SELECT uid, blob_key, body_state FROM messages")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 400);
        let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
        for (uid, key, state) in &rows {
            match key {
                Some(key) => {
                    let key = BlobKey::from_slice(key).unwrap();
                    assert_eq!(*state, BodyState::Full.code(), "round {round}, uid {uid}");
                    assert_eq!(blobs.get(&key).unwrap(), raw(*uid), "round {round}");
                }
                None => assert_eq!(*state, BodyState::Partial.code(), "round {round}"),
            }
        }
        for uid in &committed {
            assert!(
                rows.iter()
                    .any(|(stored, key, _)| stored == uid && key.is_some()),
                "round {round}: uid {uid} was reported committed"
            );
        }
        let (_, key, state) = rows.iter().find(|(uid, _, _)| *uid == paused).unwrap();
        assert_eq!(
            (key.as_ref(), *state),
            (None, BodyState::Partial.code()),
            "round {round}: killed before its commit, the row is as it was"
        );
        assert!(
            blobs.contains(&BlobStore::key(&raw(paused))),
            "round {round}: its blob stays behind as an orphan"
        );
        let bodies_rows: i64 = conn
            .query_row("SELECT count(*) FROM bodies", [], |row| row.get(0))
            .unwrap();
        let with_body = rows.iter().filter(|(_, key, _)| key.is_some()).count() as i64;
        assert_eq!(bodies_rows, with_body, "round {round}");
        assert_eq!(
            blobs.disk_usage().unwrap().files as i64,
            with_body + 1,
            "round {round}: one orphan, no missing blob"
        );
        drop(conn);
        let store = Store::open(
            &path,
            StoreOptions {
                blobs: Some(blobs),
                ..StoreOptions::default()
            },
        )
        .unwrap();
        assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
        store.close().unwrap();
    }
    assert_eq!(killed_at, [10, 20, 30, 40]);
    assert!(committed.len() >= 36, "{} committed", committed.len());
}
