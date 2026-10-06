use super::*;
use crate::ingest::{RawMessage, ingest_raw};
use rusqlite::Connection;
use sealwax_domain::model::{Address, Attachment, BodyKind, FolderRole, Headers, ThreadId};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A scratch directory under the workspace's `target/`, on the real disk (`/tmp` is tmpfs here,
/// where fsync costs nothing).
pub(crate) fn scratch() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("store-")
        .tempdir_in(base)
        .unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    account: AccountId,
    inbox: FolderId,
    archive: FolderId,
}

fn add_account(store: &Store, name: &str) -> AccountId {
    store
        .add_account(NewAccount {
            name: name.into(),
            address: format!("{}@example.test", name.to_lowercase()),
        })
        .recv_blocking()
        .unwrap()
        .value
}

fn add_folder(store: &Store, account: AccountId, name: &str, role: FolderRole) -> FolderId {
    store
        .upsert_folder(FolderSpec::local(account, name, role))
        .recv_blocking()
        .unwrap()
        .value
}

fn fixture() -> Fixture {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = add_account(&store, "Test");
    let inbox = add_folder(&store, account, "INBOX", FolderRole::Inbox);
    let archive = add_folder(&store, account, "Archive", FolderRole::Archive);
    Fixture {
        _dir: dir,
        store,
        account,
        inbox,
        archive,
    }
}

fn message(folder: FolderId, key: &str, date: i64) -> NewMessage {
    NewMessage {
        folder,
        date,
        headers: Headers {
            from: vec![Address::new(
                format!("Sender {key}"),
                format!("{key}@example.test"),
            )],
            subject: format!("Subject {key}"),
            date: Some(date),
            message_id: Some(format!("{key}@example.test")),
            ..Headers::default()
        },
        snippet: format!("Snippet {key}"),
        search_text: format!("body of {key}"),
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

pub(super) fn whole(store: &Store, spec: ViewSpec) -> Page {
    store.page(spec, 0, u32::MAX).recv_blocking().unwrap()
}

fn ids(rows: &[MessageSummary]) -> Vec<MessageId> {
    rows.iter().map(|row| row.id).collect()
}

/// SplitMix64.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(super) fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[test]
fn migrates_once_and_refuses_a_newer_schema() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    Store::open(&path, StoreOptions::default())
        .unwrap()
        .close()
        .unwrap();
    Store::open(&path, StoreOptions::default())
        .unwrap()
        .close()
        .unwrap();
    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let migrations: i64 = conn
        .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        (version, journal.as_str(), migrations),
        (SCHEMA_VERSION, "wal", SCHEMA_VERSION)
    );
    drop(conn);

    let future = dir.path().join("future.sqlite");
    let conn = Connection::open(&future).unwrap();
    conn.execute_batch(
        "PRAGMA user_version=10; CREATE TABLE later(x); INSERT INTO later VALUES (1);",
    )
    .unwrap();
    drop(conn);
    let before = fs::read(&future).unwrap();
    let error = Store::open(&future, StoreOptions::default()).err().unwrap();
    assert!(
        matches!(
            error,
            StoreError::NewerSchema {
                found: 10,
                supported: SCHEMA_VERSION
            }
        ),
        "{error}"
    );
    assert_eq!(fs::read(&future).unwrap(), before);
    let mut wal = future.as_os_str().to_owned();
    wal.push("-wal");
    assert!(!Path::new(&wal).exists());
}

#[test]
fn store_files_are_private() {
    let dir = scratch();
    let path = dir.path().join("data/sealwax/store.sqlite");
    let store = Store::open_in_background(path.clone(), StoreOptions::default())
        .recv_blocking()
        .unwrap();
    add_account(&store, "Test");
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        assert_eq!(mode(Path::new(&file)), 0o600, "{suffix}");
    }
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    store.close().unwrap();
}

#[test]
fn close_leaves_an_empty_wal() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let account = add_account(&store, "Test");
    let inbox = add_folder(&store, account, "INBOX", FolderRole::Inbox);
    ingest(
        &store,
        (0..300)
            .map(|n| message(inbox, &format!("w{n}"), n))
            .collect(),
    );
    whole(&store, ViewSpec::folder(inbox));
    store.close().unwrap();
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let size = fs::metadata(Path::new(&wal)).map_or(0, |meta| meta.len());
    assert_eq!(size, 0);
}

#[test]
fn pages_counts_sections_and_filters() {
    let f = fixture();
    let mut messages = Vec::new();
    for n in 0..300i64 {
        let mut m = message(f.inbox, &format!("i{n}"), 1_000_000 + (n / 3) * 60);
        if n % 3 != 0 {
            m.flags = Flags::SEEN;
        }
        if n % 7 == 0 {
            m.flags = m.flags | Flags::FLAGGED;
        }
        m.has_attachments = n % 5 == 0;
        messages.push(m);
    }
    for n in 0..50 {
        messages.push(message(f.archive, &format!("a{n}"), 1_000_000 + n * 7));
    }
    let stored = ingest(&f.store, messages.clone());
    let mut expected: Vec<(i64, MessageId, &NewMessage)> = stored
        .iter()
        .zip(&messages)
        .filter(|(_, m)| m.folder == f.inbox)
        .map(|(id, m)| (m.date, *id, m))
        .collect();
    expected.sort_by_key(|e| std::cmp::Reverse((e.0, e.1)));

    let folders = f.store.folders().recv_blocking().unwrap().value;
    let inbox = folders.iter().find(|folder| folder.id == f.inbox).unwrap();
    assert_eq!(
        (inbox.total, inbox.unread, inbox.role),
        (300, 100, FolderRole::Inbox)
    );
    let archive = folders
        .iter()
        .find(|folder| folder.id == f.archive)
        .unwrap();
    assert_eq!((archive.total, archive.unread), (50, 50));

    let starts = [1_000_000 + 90 * 60, 1_000_000 + 30 * 60];
    let opened = f
        .store
        .open_view(OpenView {
            spec: ViewSpec::folder(f.inbox),
            first_page: 30,
            section_starts: starts.to_vec(),
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(opened.total, 300);
    assert_eq!(
        ids(&opened.rows),
        expected.iter().take(30).map(|e| e.1).collect::<Vec<_>>()
    );
    let since = |start: i64| expected.iter().filter(|e| e.0 >= start).count() as u32;
    assert_eq!(opened.section_counts, [since(starts[0]), since(starts[1])]);

    for offset in [0, 37, 149, 150, 151, 260, 290, 299, 300, 450] {
        let page = f
            .store
            .page(ViewSpec::folder(f.inbox), offset, 25)
            .recv_blocking()
            .unwrap();
        let want: Vec<MessageId> = expected
            .iter()
            .skip(offset as usize)
            .take(25)
            .map(|e| e.1)
            .collect();
        assert_eq!(ids(&page.rows), want, "offset {offset}");
        assert_eq!(page.offset, offset);
    }

    for filter in [
        ViewFilter::Unread,
        ViewFilter::Starred,
        ViewFilter::Attachments,
    ] {
        let keep = |m: &NewMessage| match filter {
            ViewFilter::Unread => !m.flags.is_seen(),
            ViewFilter::Starred => m.flags.is_flagged(),
            _ => m.has_attachments,
        };
        let spec = ViewSpec::folder(f.inbox).with_filter(filter);
        let want: Vec<MessageId> = expected.iter().filter(|e| keep(e.2)).map(|e| e.1).collect();
        let opened = f
            .store
            .open_view(OpenView {
                spec,
                first_page: 10,
                section_starts: Vec::new(),
            })
            .recv_blocking()
            .unwrap();
        assert_eq!(opened.total as usize, want.len(), "{filter:?}");
        assert_eq!(ids(&whole(&f.store, spec).rows), want, "{filter:?}");
        let deep = f
            .store
            .page(spec, opened.total - 5, 10)
            .recv_blocking()
            .unwrap();
        assert_eq!(ids(&deep.rows), want[want.len() - 5..], "{filter:?}");
    }

    let unified = whole(&f.store, ViewSpec::unified_inbox());
    assert_eq!(
        ids(&unified.rows),
        expected.iter().map(|e| e.1).collect::<Vec<_>>()
    );
    assert_eq!(
        f.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn unified_inbox_merges_accounts_in_date_order() {
    let f = fixture();
    let other = add_account(&f.store, "Other");
    let other_inbox = add_folder(&f.store, other, "INBOX", FolderRole::Inbox);
    let mut rng = Rng(11);
    let mut messages = Vec::new();
    for n in 0..240 {
        let folder = [f.inbox, other_inbox, f.archive][n % 3];
        messages.push(message(
            folder,
            &format!("u{n}"),
            5_000 + rng.below(400) as i64,
        ));
    }
    let stored = ingest(&f.store, messages.clone());
    let mut expected: Vec<(i64, MessageId)> = stored
        .iter()
        .zip(&messages)
        .filter(|(_, m)| m.folder != f.archive)
        .map(|(id, m)| (m.date, *id))
        .collect();
    expected.sort_by(|a, b| b.cmp(a));
    let expected: Vec<MessageId> = expected.into_iter().map(|e| e.1).collect();
    let spec = ViewSpec::unified_inbox();
    for offset in [0, 1, 50, 79, 80, 81, 120, 159, 160] {
        let page = f.store.page(spec, offset, 13).recv_blocking().unwrap();
        let want: Vec<MessageId> = expected
            .iter()
            .skip(offset as usize)
            .take(13)
            .copied()
            .collect();
        assert_eq!(ids(&page.rows), want, "offset {offset}");
    }
    let opened = f
        .store
        .open_view(OpenView {
            spec,
            first_page: 5,
            section_starts: vec![5_200],
        })
        .recv_blocking()
        .unwrap();
    assert_eq!(opened.total, 160);
    let newer = stored
        .iter()
        .zip(&messages)
        .filter(|(_, m)| m.folder != f.archive && m.date >= 5_200)
        .count() as u32;
    assert_eq!(opened.section_counts, [newer]);
}

#[test]
fn threads_join_late_parents_merge_and_count_each_message_id_once() {
    let f = fixture();
    let events = f.store.subscribe();
    let mut reply = message(f.inbox, "reply", 200);
    reply.headers.in_reply_to = vec!["root@example.test".into()];
    reply.headers.references = vec!["root@example.test".into()];
    let mut stray = message(f.inbox, "stray", 300);
    stray.headers.in_reply_to = vec!["lost@example.test".into()];
    let first = ingest(&f.store, vec![reply, message(f.inbox, "root", 100), stray]);
    let summaries = whole(&f.store, ViewSpec::folder(f.inbox)).rows;
    let thread_of = |id: MessageId, rows: &[MessageSummary]| {
        rows.iter().find(|row| row.id == id).unwrap().thread
    };
    assert_eq!(
        thread_of(first[0], &summaries),
        thread_of(first[1], &summaries)
    );
    assert_ne!(
        thread_of(first[2], &summaries),
        thread_of(first[1], &summaries)
    );

    let mut bridge = message(f.inbox, "bridge", 400);
    bridge.headers.references = vec!["root@example.test".into(), "lost@example.test".into()];
    let bridge = ingest(&f.store, vec![bridge])[0];
    let mut copy = message(f.archive, "root", 100);
    copy.headers.subject = "Subject root".into();
    let copy = ingest(&f.store, vec![copy])[0];

    let mut merges = Vec::new();
    while let Ok(event) = events.try_recv() {
        merges.extend(event.merged_threads.clone());
    }
    assert_eq!(merges.len(), 1, "{merges:?}");
    let inbox = whole(&f.store, ViewSpec::folder(f.inbox)).rows;
    let thread = thread_of(bridge, &inbox);
    assert_eq!(merges[0].into, thread);
    assert!(inbox.iter().all(|row| row.thread == thread));
    assert!(inbox.iter().all(|row| row.thread_count == 4), "{inbox:?}");
    let archived = whole(&f.store, ViewSpec::folder(f.archive)).rows;
    assert_eq!(
        (archived[0].id, archived[0].thread, archived[0].thread_count),
        (copy, thread, 4)
    );
    let detail = f
        .store
        .message(copy)
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(detail.summary.thread, thread);
    assert_eq!(
        f.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );

    let removed = f
        .store
        .remove(vec![copy, first[2]])
        .recv_blocking()
        .unwrap();
    assert_eq!(removed.removed.len(), 2);
    let inbox = whole(&f.store, ViewSpec::folder(f.inbox)).rows;
    assert!(inbox.iter().all(|row| row.thread_count == 3));
    assert_eq!(
        f.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
    let _ = f.account;
}

/// A list model as the GTK side would keep it, fed only by change events.
pub(crate) struct Model {
    pub(crate) view: ViewHandle,
    pub(crate) rows: Vec<MessageSummary>,
    pub(crate) version: StoreVersion,
}

impl Model {
    pub(crate) fn apply(&mut self, store: &Store, event: &ChangeEvent) {
        assert_eq!(
            event.version.0,
            self.version.0 + 1,
            "events arrive in order"
        );
        self.version = event.version;
        for change in event
            .views
            .iter()
            .filter(|change| change.view == self.view.id())
        {
            match &change.diff {
                ViewDiff::Splice {
                    removed,
                    inserted,
                    changed,
                    total,
                } => {
                    assert!(removed.windows(2).all(|pair| pair[0] > pair[1]));
                    for position in removed {
                        self.rows.remove(*position as usize);
                    }
                    assert!(
                        inserted
                            .windows(2)
                            .all(|pair| pair[0].position < pair[1].position)
                    );
                    for item in inserted {
                        self.rows.insert(item.position as usize, item.row.clone());
                    }
                    for item in changed {
                        assert_eq!(self.rows[item.position as usize].id, item.row.id);
                        self.rows[item.position as usize] = item.row.clone();
                    }
                    assert_eq!(self.rows.len(), *total as usize);
                }
                ViewDiff::Reload { total } => {
                    let page = whole(store, self.view.spec());
                    assert_eq!(page.version, event.version);
                    assert_eq!(page.rows.len(), *total as usize);
                    self.rows = page.rows;
                }
            }
        }
        let merged: BTreeMap<ThreadId, ThreadId> = event
            .merged_threads
            .iter()
            .map(|merge| (merge.from, merge.into))
            .collect();
        let counts: BTreeMap<ThreadId, u32> = event
            .threads
            .iter()
            .map(|count| (count.thread, count.count))
            .collect();
        for row in &mut self.rows {
            if let Some(into) = merged.get(&row.thread) {
                row.thread = *into;
            }
            if let Some(count) = counts.get(&row.thread) {
                row.thread_count = *count;
            }
        }
    }
}

#[test]
fn change_events_keep_list_models_exact() {
    let f = fixture();
    let other = add_account(&f.store, "Other");
    let other_inbox = add_folder(&f.store, other, "INBOX", FolderRole::Inbox);
    let folders = [f.inbox, f.inbox, other_inbox, f.archive];
    let mut rng = Rng(2026);
    let mut serial = 0u64;
    let mut fresh = |rng: &mut Rng, count: usize| -> Vec<NewMessage> {
        (0..count)
            .map(|_| {
                serial += 1;
                let folder = folders[rng.below(folders.len() as u64) as usize];
                let mut m = message(
                    folder,
                    &format!("r{serial}"),
                    10_000 + rng.below(600) as i64,
                );
                if rng.below(3) > 0 {
                    m.flags = Flags::SEEN;
                }
                if rng.below(6) == 0 {
                    m.flags = m.flags | Flags::FLAGGED;
                }
                m.has_attachments = rng.below(4) == 0;
                if serial > 3 && rng.below(3) == 0 {
                    let parent = 1 + rng.below(serial - 1);
                    m.headers.in_reply_to = vec![format!("r{parent}@example.test")];
                }
                m
            })
            .collect()
    };
    let mut live: Vec<MessageId> = ingest(&f.store, fresh(&mut rng, 120));

    let events = f.store.subscribe();
    let specs = [
        ViewSpec::folder(f.inbox),
        ViewSpec::folder(f.inbox).with_filter(ViewFilter::Unread),
        ViewSpec::folder(f.inbox).with_filter(ViewFilter::Starred),
        ViewSpec::folder(f.inbox).with_filter(ViewFilter::Attachments),
        ViewSpec::unified_inbox(),
        ViewSpec::unified_inbox().with_filter(ViewFilter::Unread),
        ViewSpec::folder(f.archive),
    ];
    let mut models: Vec<Model> = specs
        .iter()
        .map(|spec| {
            let opened = f
                .store
                .open_view(OpenView {
                    spec: *spec,
                    first_page: u32::MAX,
                    section_starts: Vec::new(),
                })
                .recv_blocking()
                .unwrap();
            assert_eq!(opened.rows.len(), opened.total as usize);
            Model {
                view: opened.view,
                rows: opened.rows,
                version: opened.version,
            }
        })
        .collect();

    let mut splices = 0;
    let mut reloads = 0;
    for step in 0..160 {
        let version = match rng.below(10) {
            _ if step % 40 == 39 => {
                let reply = f.store.ingest(fresh(&mut rng, 90)).recv_blocking().unwrap();
                live.extend(reply.ids.iter().flatten());
                reply.version
            }
            0..=2 => {
                let count = 1 + rng.below(3) as usize;
                let reply = f
                    .store
                    .ingest(fresh(&mut rng, count))
                    .recv_blocking()
                    .unwrap();
                live.extend(reply.ids.iter().flatten());
                reply.version
            }
            3..=6 => {
                let picks: Vec<MessageId> = (0..1 + rng.below(5))
                    .map(|_| live[rng.below(live.len() as u64) as usize])
                    .collect();
                let (add, remove) = match rng.below(4) {
                    0 => (Flags::SEEN, Flags::NONE),
                    1 => (Flags::NONE, Flags::SEEN),
                    2 => (Flags::FLAGGED, Flags::NONE),
                    _ => (Flags::NONE, Flags::FLAGGED | Flags::SEEN),
                };
                f.store
                    .set_flags(picks, add, remove)
                    .recv_blocking()
                    .unwrap()
            }
            _ => {
                let picks: Vec<MessageId> = (0..1 + rng.below(3))
                    .map(|_| live[rng.below(live.len() as u64) as usize])
                    .collect();
                live.retain(|id| !picks.contains(id));
                f.store.remove(picks).recv_blocking().unwrap().version
            }
        };
        while models[0].version < version {
            let event = events.recv_blocking().unwrap();
            for change in &event.views {
                match change.diff {
                    ViewDiff::Splice { .. } => splices += 1,
                    ViewDiff::Reload { .. } => reloads += 1,
                }
            }
            for model in &mut models {
                model.apply(&f.store, &event);
            }
        }
        for model in &models {
            let page = whole(&f.store, model.view.spec());
            assert_eq!(page.version, model.version, "step {step}");
            assert_eq!(
                page.rows,
                model.rows,
                "step {step}, {:?}",
                model.view.spec()
            );
        }
    }
    assert!(
        splices > 100 && reloads > 0,
        "{splices} splices, {reloads} reloads"
    );
    assert_eq!(
        f.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn flag_changes_update_counts_and_skip_no_ops() {
    let f = fixture();
    let stored = ingest(
        &f.store,
        (0..10)
            .map(|n| message(f.inbox, &format!("f{n}"), 100 + n))
            .collect(),
    );
    let events = f.store.subscribe();
    let version = f
        .store
        .set_flags(
            stored[..4].to_vec(),
            Flags::SEEN | Flags::FLAGGED,
            Flags::NONE,
        )
        .recv_blocking()
        .unwrap();
    let event = events.recv_blocking().unwrap();
    assert_eq!(event.version, version);
    assert_eq!(event.changed, stored[..4]);
    assert_eq!(
        event.folders,
        [FolderCounts {
            folder: f.inbox,
            total: 10,
            unread: 6
        }]
    );
    let again = f
        .store
        .set_flags(stored[..4].to_vec(), Flags::SEEN, Flags::NONE)
        .recv_blocking()
        .unwrap();
    assert_eq!(again, version);
    assert!(events.try_recv().is_err(), "no event without a change");
    let starred = whole(
        &f.store,
        ViewSpec::folder(f.inbox).with_filter(ViewFilter::Starred),
    );
    assert_eq!(starred.rows.len(), 4);
    assert!(
        starred
            .rows
            .iter()
            .all(|row| row.flags.is_seen() && row.flags.is_flagged())
    );
}

#[test]
fn removal_reports_orphaned_blobs() {
    let f = fixture();
    let key = BlobKey([7; 32]);
    let mut first = message(f.inbox, "same", 10);
    first.blob = Some(key);
    first.uid = Some(1);
    let mut second = message(f.archive, "same", 10);
    second.blob = Some(key);
    second.uid = Some(1);
    let stored = ingest(&f.store, vec![first, second]);
    let removed = f.store.remove(vec![stored[0]]).recv_blocking().unwrap();
    assert!(removed.orphaned_blobs.is_empty());
    let removed = f
        .store
        .remove(vec![stored[1], stored[1]])
        .recv_blocking()
        .unwrap();
    assert_eq!(removed.removed, [stored[1]]);
    assert_eq!(removed.orphaned_blobs, [key]);
    let nothing = f.store.remove(vec![stored[1]]).recv_blocking().unwrap();
    assert_eq!(nothing.version, removed.version);
    assert_eq!(
        f.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn skips_known_uids_and_unknown_folders() {
    let f = fixture();
    let mut m = message(f.inbox, "uid", 10);
    m.uid = Some(42);
    let first = f.store.ingest(vec![m.clone()]).recv_blocking().unwrap();
    let again = f.store.ingest(vec![m]).recv_blocking().unwrap();
    assert!(first.ids[0].is_some());
    assert_eq!(again.ids, [None]);
    assert_eq!(again.version, first.version);
    let error = f
        .store
        .ingest(vec![message(FolderId(999), "x", 1)])
        .recv_blocking()
        .unwrap_err();
    assert!(matches!(error, StoreError::UnknownFolder(FolderId(999))));
    let empty = f.store.ingest(Vec::new()).recv_blocking().unwrap();
    assert_eq!(empty.version, first.version);
}

#[test]
fn search_finds_norwegian_cjk_prefixes_and_phrases() {
    let f = fixture();
    let bodies = [
        (f.inbox, "Vi tar med blåbærsyltetøy til hytta på fredag."),
        (f.inbox, "关于下周的会议安排，请查看附件。"),
        (f.inbox, "プロジェクトの進捗について報告します。"),
        (f.archive, "Prosjektplanen er oppdatert etter møtet."),
        (f.inbox, "The project update is ready for review."),
    ];
    let messages: Vec<NewMessage> = bodies
        .iter()
        .enumerate()
        .map(|(n, (folder, body))| {
            let mut m = message(*folder, &format!("s{n}"), 1_000 + n as i64);
            m.search_text = (*body).to_owned();
            m
        })
        .collect();
    let stored = ingest(&f.store, messages);
    let find = |text: &str, folder: Option<FolderId>| -> Vec<MessageId> {
        ids(&f
            .store
            .search(text, folder, 50)
            .recv_blocking()
            .unwrap()
            .value)
    };
    assert_eq!(find("blåbærsyltetøy", None), [stored[0]]);
    assert_eq!(find("blabaersyltetoy", None), Vec::<MessageId>::new());
    assert_eq!(find("会议", None), [stored[1]]);
    assert_eq!(find("進捗", None), [stored[2]]);
    assert_eq!(find("prosjekt*", None), [stored[3]]);
    assert_eq!(find("prosjekt*", Some(f.inbox)), Vec::<MessageId>::new());
    assert_eq!(find("\"project update\"", None), [stored[4]]);
    assert_eq!(find("\"update project\"", None), Vec::<MessageId>::new());
    assert_eq!(find("sender s3", None), [stored[3]]);
    assert_eq!(find("body", None), Vec::<MessageId>::new());
    assert_eq!(find("subject", None).len(), 5);
    assert_eq!(find("subject", None)[0], stored[4], "newest first");
    assert_eq!(find("  ", None), Vec::<MessageId>::new());
    f.store.remove(vec![stored[0]]).recv_blocking().unwrap();
    assert_eq!(find("blåbærsyltetøy", None), Vec::<MessageId>::new());
    assert_eq!(
        f.store.check().recv_blocking().unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn details_round_trip() {
    let f = fixture();
    let mut m = message(f.inbox, "detail", 500);
    m.headers.to = vec![
        Address::new("Ola Nordmann", "ola@example.test"),
        Address::new("", "kari@example.test"),
    ];
    m.headers.cc = vec![Address::new("王伟", "wang.wei@example.test")];
    m.headers.reply_to = vec![Address::new("Svar", "svar@example.test")];
    m.headers.in_reply_to = vec!["parent@example.test".into()];
    m.headers.references = vec!["root@example.test".into(), "parent@example.test".into()];
    m.headers.list_id = Some("liste.example.test".into());
    m.keywords = vec!["$Label1".into(), "Prioritert".into()];
    m.body_kind = BodyKind::Both;
    m.text_parts = vec![1];
    m.html_parts = vec![2];
    m.attachments = vec![
        Attachment {
            part: 4,
            name: None,
            size: 8,
            mime_type: "image/png".into(),
            content_id: Some("logo@example.test".into()),
            inline: true,
        },
        Attachment {
            part: 5,
            name: Some("faktura for møte.pdf".into()),
            size: 27,
            mime_type: "application/pdf".into(),
            content_id: None,
            inline: false,
        },
    ];
    m.has_attachments = true;
    m.blob = Some(BlobKey([3; 32]));
    m.body_state = BodyState::Full;
    m.size = 4_321;
    m.flags = Flags::SEEN | Flags::ANSWERED;
    let id = ingest(&f.store, vec![m.clone()])[0];
    let detail = f.store.message(id).recv_blocking().unwrap().value.unwrap();
    assert_eq!(detail.headers, m.headers);
    assert_eq!(detail.keywords, m.keywords);
    assert_eq!(detail.body_kind, BodyKind::Both);
    assert_eq!((detail.text_parts, detail.html_parts), (vec![1], vec![2]));
    assert_eq!(detail.attachments, m.attachments);
    assert_eq!(detail.blob, m.blob);
    assert_eq!(detail.size, 4_321);
    assert_eq!(detail.summary.flags, m.flags);
    assert!(detail.summary.has_attachments);
    assert_eq!(detail.summary.snippet, "Snippet detail");
    assert!(
        f.store
            .message(MessageId(9_999))
            .recv_blocking()
            .unwrap()
            .value
            .is_none()
    );
    assert!(f.store.raw_message(id).recv_blocking().unwrap().is_none());
}

#[test]
fn versions_rise_with_each_visible_commit() {
    let f = fixture();
    let before = whole(&f.store, ViewSpec::folder(f.inbox)).version;
    let ingested = f
        .store
        .ingest(
            (0..450)
                .map(|n| message(f.inbox, &format!("v{n}"), n))
                .collect(),
        )
        .recv_blocking()
        .unwrap();
    assert_eq!(ingested.batches, 3, "batches of at most 200");
    assert_eq!(ingested.version.0, before.0 + 3);
    let page = whole(&f.store, ViewSpec::folder(f.inbox));
    assert_eq!((page.version, page.rows.len()), (ingested.version, 450));
    let accounts = f.store.accounts().recv_blocking().unwrap();
    assert_eq!(accounts.version, ingested.version);
    assert_eq!(accounts.value.len(), 1);
}

#[test]
fn checkpoints_run_on_the_maintenance_thread() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let store = Store::open(
        &path,
        StoreOptions {
            checkpoint_frames: 20,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let account = add_account(&store, "Test");
    let inbox = add_folder(&store, account, "INBOX", FolderRole::Inbox);
    for round in 0..5 {
        ingest(
            &store,
            (0..100)
                .map(|n| message(inbox, &format!("c{round}-{n}"), n))
                .collect(),
        );
    }
    let conn = Connection::open(&path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let restarts = loop {
        let restarts: Option<i64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'wal_restarts'",
                [],
                |row| row.get(0),
            )
            .ok();
        if restarts.is_some() || Instant::now() > deadline {
            break restarts;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(restarts.unwrap_or(0) >= 1, "{restarts:?}");
    store.close().unwrap();
}

const CRASH_DIR: &str = "SEALWAX_STORE_CRASH_DIR";
const CRASH_START: &str = "SEALWAX_STORE_CRASH_START";

fn crash_message(n: u64) -> Vec<u8> {
    let parent = if n.is_multiple_of(3) { n - 1 } else { n };
    format!(
        "From: Avsender {n} <a{n}@example.test>\r\nTo: b@example.test\r\n\
         Subject: Krasj {n}\r\nDate: Mon, 05 Oct 2026 09:00:00 +0000\r\n\
         Message-ID: <crash-{n}@example.test>\r\nReferences: <crash-{parent}@example.test>\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nTekst {n} {}\r\n",
        "blåbær ".repeat((n % 50) as usize)
    )
    .into_bytes()
}

/// The child half of `kill_during_ingest_leaves_a_consistent_store`: ingests, flags and removes
/// until killed, printing each committed message number.
#[test]
#[ignore = "runs as a child process of kill_during_ingest_leaves_a_consistent_store"]
fn crash_child() {
    let Some(dir) = std::env::var_os(CRASH_DIR).map(PathBuf::from) else {
        return;
    };
    let mut n: u64 = std::env::var(CRASH_START).unwrap().parse().unwrap();
    let blobs = BlobStore::open(&dir.join("blobs")).unwrap();
    let store = Store::open(
        &dir.join("store.sqlite"),
        StoreOptions {
            checkpoint_frames: 64,
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let accounts = store.accounts().recv_blocking().unwrap().value;
    let account = match accounts.first() {
        Some(account) => account.id,
        None => add_account(&store, "Crash"),
    };
    let inbox = add_folder(&store, account, "INBOX", FolderRole::Inbox);
    let archive = add_folder(&store, account, "Archive", FolderRole::Archive);
    let mut out = std::io::stdout().lock();
    loop {
        let batch: Vec<RawMessage> = (0..48)
            .map(|_| {
                n += 1;
                RawMessage {
                    folder: if n.is_multiple_of(5) { archive } else { inbox },
                    flags: if n.is_multiple_of(2) {
                        Flags::SEEN
                    } else {
                        Flags::NONE
                    },
                    raw: crash_message(n),
                    ..RawMessage::default()
                }
            })
            .collect();
        let stored: Vec<MessageId> = ingest_raw(&store, &blobs, batch)
            .unwrap()
            .ids
            .into_iter()
            .flatten()
            .collect();
        writeln!(out, "committed {n}").unwrap();
        out.flush().unwrap();
        store
            .set_flags(stored[..12].to_vec(), Flags::FLAGGED, Flags::SEEN)
            .recv_blocking()
            .unwrap();
        store
            .remove(stored[12..15].to_vec())
            .recv_blocking()
            .unwrap();
        let body = "Teksten er lang nok til at indeksen får noe å gjøre. ".repeat(40);
        let bulk: Vec<NewMessage> = (0..300)
            .map(|k| {
                let mut m = message(inbox, &format!("bulk-{n}-{k}"), 1_000 + k);
                m.search_text = body.clone();
                m.snippet = "Kort tekst".into();
                m
            })
            .collect();
        store.ingest(bulk).recv_blocking().unwrap();
    }
}

#[test]
fn full_text_lost_to_a_crash_is_rebuilt_from_the_blobs() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
    let options = StoreOptions {
        blobs: Some(blobs.clone()),
        ..StoreOptions::default()
    };
    let store = Store::open(&path, options.clone()).unwrap();
    let account = add_account(&store, "Test");
    let inbox = add_folder(&store, account, "INBOX", FolderRole::Inbox);
    let filler = "fyll ".repeat(80);
    let messages: Vec<RawMessage> = (0..30)
        .map(|n| RawMessage {
            folder: inbox,
            raw: format!(
                "From: a@example.test\r\nSubject: Melding {n}\r\nMessage-ID: <fts-{n}@example.test>\r\n\
                 \r\n{filler}\r\nhalebenet {n}\r\n"
            )
            .into_bytes(),
            ..RawMessage::default()
        })
        .collect();
    ingest_raw(&store, &blobs, messages).unwrap();
    store.close().unwrap();

    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DELETE FROM message_fts WHERE rowid > (SELECT max(id) - 12 FROM messages);
         UPDATE meta SET value = (SELECT max(id) - 12 FROM messages) WHERE key = 'fts_through';",
    )
    .unwrap();
    assert_eq!(unindexed(&conn), 12);
    let store = Store::open(&path, options).unwrap();
    store.ingest(Vec::new()).recv_blocking().unwrap();
    assert_eq!(unindexed(&conn), 0);
    let found = store
        .search("halebenet", None, 100)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        found.value.len(),
        30,
        "the words past the snippet came from the blobs"
    );
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
}

fn unindexed(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM messages m \
         WHERE NOT EXISTS (SELECT 1 FROM message_fts WHERE rowid = m.id)",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn kill_during_ingest_leaves_a_consistent_store() {
    let dir = scratch();
    let exe = std::env::current_exe().unwrap();
    let mut repaired = 0;
    for round in 0..6u64 {
        let mut child = Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "store::tests::crash_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CRASH_DIR, dir.path())
            .env(CRASH_START, (round * 1_000_000).to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut last = None;
        while last.is_none_or(|n: u64| n < round * 1_000_000 + 96) {
            let line = lines
                .next()
                .expect("the child stopped before committing")
                .unwrap();
            if let Some(n) = line.strip_prefix("committed ") {
                last = Some(n.parse().unwrap());
            }
        }
        std::thread::sleep(Duration::from_millis(3 + round * 13 % 50));
        child.kill().unwrap();
        child.wait().unwrap();
        let last = last.unwrap();

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
        let survived: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE message_id = ?1)",
                [format!("crash-{last}@example.test")],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            survived,
            "round {round}: message {last} was reported committed"
        );
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        let keys: Vec<Vec<u8>> = conn
            .prepare("SELECT blob_key FROM messages WHERE blob_key IS NOT NULL")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        repaired += unindexed(&conn);
        drop(conn);

        let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
        for key in &keys {
            let key = BlobKey::from_slice(key).unwrap();
            assert_eq!(
                BlobStore::key(&blobs.get(&key).unwrap()),
                key,
                "round {round}"
            );
        }
        let store = Store::open(
            &path,
            StoreOptions {
                blobs: Some(blobs),
                ..StoreOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            store.check().recv_blocking().unwrap(),
            Vec::<String>::new(),
            "round {round}"
        );
        let total: u32 = store
            .folders()
            .recv_blocking()
            .unwrap()
            .value
            .iter()
            .map(|folder| folder.total)
            .sum();
        assert_eq!(i64::from(total), rows);
        store.ingest(Vec::new()).recv_blocking().unwrap();
        let conn = Connection::open(&path).unwrap();
        assert_eq!(unindexed(&conn), 0, "round {round}");
        let with_word: i64 = conn
            .query_row(
                "SELECT count(*) FROM messages WHERE message_id LIKE 'crash-%' \
                 AND CAST(substr(message_id, 7) AS INTEGER) % 50 <> 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let found = store
            .search("blåbær", None, u32::MAX)
            .recv_blocking()
            .unwrap();
        assert_eq!(found.value.len() as i64, with_word, "round {round}");
        store.close().unwrap();
    }
    eprintln!("kill -9 test: {repaired} messages re-indexed after kills");
}
