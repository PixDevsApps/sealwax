//! Conversations (ML-046): Outlook's `Thread-Index`, never the subject alone, Message-ID
//! dedupe, what removals do, and a property test that ingests random corpora in any order and
//! in batches of any size, with removals, against a reference model of the alias graph.

use super::tests::scratch;
use super::*;
use crate::ingest::{RawMessage, ingest_raw};
use proptest::prelude::*;
use rusqlite::Connection;
use sealwax_domain::model::{Address, FolderRole, Headers, ThreadId, ThreadIndex};
use std::collections::{BTreeMap, BTreeSet};

fn open(dir: &Path) -> Store {
    Store::open(&dir.join("store.sqlite"), StoreOptions::default()).unwrap()
}

fn account(store: &Store, name: &str) -> AccountId {
    store
        .add_account(NewAccount {
            name: name.to_owned(),
            address: format!("{}@example.test", name.to_lowercase()),
        })
        .recv_blocking()
        .unwrap()
        .value
}

fn folder(store: &Store, account: AccountId, name: &str, role: FolderRole) -> FolderId {
    store
        .upsert_folder(FolderSpec::local(account, name, role))
        .recv_blocking()
        .unwrap()
        .value
}

fn message(folder: FolderId, uid: u32, message_id: Option<&str>, subject: &str) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        date: 1_000 + i64::from(uid),
        headers: Headers {
            from: vec![Address::new("Kari Nordmann", "kari@example.test")],
            subject: subject.to_owned(),
            message_id: message_id.map(str::to_owned),
            ..Headers::default()
        },
        snippet: format!("snippet {uid}"),
        search_text: format!("text {uid}"),
        size: 100,
        ..NewMessage::default()
    }
}

/// A Thread-Index conversation; `n` makes its GUID.
fn conversation(n: u8) -> ThreadIndex {
    let mut bytes = [n; 22];
    bytes[..6].copy_from_slice(&[1, 0xd9, 0x3c, 0x5a, 0x10, 0x22]);
    ThreadIndex(bytes)
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

/// Every row of the given folders, by id.
fn rows(store: &Store, folders: &[FolderId]) -> BTreeMap<MessageId, MessageSummary> {
    folders
        .iter()
        .flat_map(|folder| {
            store
                .page(ViewSpec::folder(*folder), 0, u32::MAX)
                .recv_blocking()
                .unwrap()
                .rows
        })
        .map(|row| (row.id, row))
        .collect()
}

fn thread(rows: &BTreeMap<MessageId, MessageSummary>, id: MessageId) -> (ThreadId, u32) {
    let row = &rows[&id];
    (row.thread, row.thread_count)
}

fn clean(store: &Store) {
    assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
}

#[test]
fn thread_index_joins_outlook_replies_without_references() {
    let dir = scratch();
    let store = open(dir.path());
    let work = account(&store, "Work");
    let inbox = folder(&store, work, "INBOX", FolderRole::Inbox);
    let sent = folder(&store, work, "Sent", FolderRole::Sent);

    let mut root = message(inbox, 1, Some("root@example.test"), "Budsjett");
    root.headers.thread_index = Some(conversation(7));
    let mut reply = message(sent, 1, Some("reply@example.test"), "SV: Budsjett");
    reply.headers.thread_index = Some(conversation(7));
    let mut answer = message(inbox, 2, Some("answer@example.test"), "SV: SV: Budsjett");
    answer.headers.thread_index = Some(conversation(7));
    let namesake = message(inbox, 3, Some("namesake@example.test"), "Budsjett");
    let mut other = message(inbox, 4, Some("other@example.test"), "Budsjett");
    other.headers.thread_index = Some(conversation(8));
    // The answer arrives first: a first sync goes newest first.
    let ids = ingest(&store, vec![answer, reply, namesake, other]);
    let root = ingest(&store, vec![root])[0];
    let all = rows(&store, &[inbox, sent]);
    let (outlook, size) = thread(&all, root);
    assert_eq!(size, 3);
    assert_eq!(thread(&all, ids[0]), (outlook, 3));
    assert_eq!(thread(&all, ids[1]), (outlook, 3));
    assert_ne!(
        thread(&all, ids[2]).0,
        outlook,
        "a subject alone joins nothing"
    );
    assert_ne!(thread(&all, ids[3]).0, outlook, "another Thread-Index");
    assert_ne!(thread(&all, ids[3]).0, thread(&all, ids[2]).0);

    // A reply from a client without Thread-Index joins through References, and a message with
    // the other conversation's Thread-Index and a reference to this one merges the two.
    let mut plain = message(inbox, 5, Some("plain@example.test"), "Re: Budsjett");
    plain.headers.in_reply_to = vec!["answer@example.test".into()];
    let mut bridge = message(inbox, 6, Some("bridge@example.test"), "Re: Budsjett");
    bridge.headers.references = vec!["reply@example.test".into()];
    bridge.headers.thread_index = Some(conversation(8));
    let events = store.subscribe();
    let later = ingest(&store, vec![plain, bridge]);
    let all = rows(&store, &[inbox, sent]);
    let (merged, size) = thread(&all, later[1]);
    assert_eq!(size, 6);
    for id in [root, ids[0], ids[1], ids[3], later[0]] {
        assert_eq!(thread(&all, id), (merged, 6));
    }
    assert_eq!(thread(&all, ids[2]).1, 1);
    let merges: Vec<ThreadMerge> = std::iter::from_fn(|| events.try_recv().ok())
        .flat_map(|event| event.merged_threads.clone())
        .collect();
    assert_eq!(merges.len(), 1, "{merges:?}");
    assert_eq!(merges[0].into, merged);

    let detail = store
        .message(ids[1])
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(detail.headers.thread_index, Some(conversation(7)));
    let detail = store
        .message(ids[2])
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(detail.headers.thread_index, None);
    clean(&store);
    store.close().unwrap();
}

/// A raw Outlook message: Thread-Index and Thread-Topic, no In-Reply-To or References.
fn outlook(n: u32, index: &str) -> Vec<u8> {
    format!(
        "From: Ola Nordmann <ola@example.test>\r\nTo: kari@example.test\r\n\
         Subject: SV: Prosjektplan\r\nDate: Mon, 05 Oct 2026 10:{n:02}:00 +0200\r\n\
         Message-ID: <outlook-{n}@example.test>\r\nThread-Topic: Prosjektplan\r\n\
         Thread-Index: {index}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHei {n}\r\n"
    )
    .into_bytes()
}

#[test]
fn raw_messages_bring_their_thread_index_into_the_store() {
    let dir = scratch();
    let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
    let store = Store::open(
        &dir.path().join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let work = account(&store, "Work");
    let inbox = folder(&store, work, "INBOX", FolderRole::Inbox);
    let raws = [
        outlook(1, "Adk8WhAiEBESExQVFhcYGRobHB0eHw=="),
        outlook(2, "Adk8WhAiEBESExQVFhcYGRobHB0eHwAAEjRW"),
        outlook(3, "Adk8WhAiEBESExQVFhcYGRobHB0eHwAAEjRWAAECAwQ="),
        outlook(4, "Adk8WhAiEBESExQVFhcYGRobHB0eHwEC"),
    ];
    let messages = raws
        .into_iter()
        .rev()
        .map(|raw| RawMessage {
            folder: inbox,
            raw,
            ..RawMessage::default()
        })
        .collect();
    let ids: Vec<MessageId> = ingest_raw(&store, &blobs, messages)
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect();
    let all = rows(&store, &[inbox]);
    let (conversation, size) = thread(&all, ids[3]);
    assert_eq!(size, 3);
    assert_eq!(thread(&all, ids[2]), (conversation, 3));
    assert_eq!(thread(&all, ids[1]), (conversation, 3));
    assert_eq!(
        thread(&all, ids[0]).1,
        1,
        "a malformed Thread-Index joins nothing"
    );
    let detail = store
        .message(ids[1])
        .recv_blocking()
        .unwrap()
        .value
        .unwrap();
    assert_eq!(
        detail.headers.thread_index.map(|index| index.to_hex()),
        Some("01d93c5a1022101112131415161718191a1b1c1d1e1f".to_owned())
    );
    clean(&store);
    store.close().unwrap();
}

#[test]
fn the_same_subject_never_joins_a_conversation() {
    let dir = scratch();
    let store = open(dir.path());
    let home = account(&store, "Home");
    let inbox = folder(&store, home, "INBOX", FolderRole::Inbox);
    let mut messages: Vec<NewMessage> = (1..=4)
        .map(|uid| {
            message(
                inbox,
                uid,
                Some(&format!("faktura-{uid}@example.test")),
                "Faktura",
            )
        })
        .collect();
    messages.push(message(inbox, 5, Some("re@example.test"), "Re: Faktura"));
    messages.push(message(inbox, 6, None, "Faktura"));
    messages.push(message(inbox, 7, None, "Faktura"));
    let ids = ingest(&store, messages);
    let all = rows(&store, &[inbox]);
    let threads: BTreeSet<ThreadId> = ids.iter().map(|id| thread(&all, *id).0).collect();
    assert_eq!(threads.len(), ids.len());
    assert!(all.values().all(|row| row.thread_count == 1));
    clean(&store);
    store.close().unwrap();
}

#[test]
fn a_message_id_counts_once_across_folders_and_copies() {
    let dir = scratch();
    let store = open(dir.path());
    let work = account(&store, "Work");
    let inbox = folder(&store, work, "INBOX", FolderRole::Inbox);
    let archive = folder(&store, work, "Archive", FolderRole::Archive);
    let home = account(&store, "Home");
    let home_inbox = folder(&store, home, "INBOX", FolderRole::Inbox);

    let mut reply = message(inbox, 3, Some("reply@example.test"), "Re: Møte");
    reply.headers.in_reply_to = vec!["meeting@example.test".into()];
    let mut anonymous = message(inbox, 4, None, "Re: Møte");
    anonymous.headers.references = vec!["meeting@example.test".into()];
    let ids = ingest(
        &store,
        vec![
            message(inbox, 1, Some("meeting@example.test"), "Møte"),
            message(inbox, 2, Some("meeting@example.test"), "Møte"),
            message(archive, 1, Some("meeting@example.test"), "Møte"),
            reply,
            anonymous,
            message(home_inbox, 1, Some("meeting@example.test"), "Møte"),
        ],
    );
    let all = rows(&store, &[inbox, archive, home_inbox]);
    let (meeting, size) = thread(&all, ids[0]);
    assert_eq!(size, 3, "two Message-IDs and one message without one");
    for id in &ids[1..5] {
        assert_eq!(thread(&all, *id), (meeting, 3));
    }
    let (elsewhere, size) = thread(&all, ids[5]);
    assert_ne!(elsewhere, meeting, "accounts never share a conversation");
    assert_eq!(size, 1);
    let folders = store.folders().recv_blocking().unwrap().value;
    let total = |id: FolderId| folders.iter().find(|f| f.id == id).unwrap().total;
    assert_eq!((total(inbox), total(archive)), (4, 1));

    store.remove(vec![ids[1], ids[2]]).recv_blocking().unwrap();
    assert_eq!(thread(&rows(&store, &[inbox]), ids[0]), (meeting, 3));
    store.remove(vec![ids[0]]).recv_blocking().unwrap();
    assert_eq!(thread(&rows(&store, &[inbox]), ids[3]), (meeting, 2));
    clean(&store);
    store.close().unwrap();
}

#[test]
fn removals_keep_a_merged_conversation_whole_until_it_is_empty() {
    let dir = scratch();
    let store = open(dir.path());
    let work = account(&store, "Work");
    let inbox = folder(&store, work, "INBOX", FolderRole::Inbox);
    let archive = folder(&store, work, "Archive", FolderRole::Archive);
    let mut bridge = message(inbox, 3, Some("bridge@example.test"), "Re: Plan");
    bridge.headers.references = vec!["a@example.test".into(), "b@example.test".into()];
    let ids = ingest(
        &store,
        vec![
            message(inbox, 1, Some("a@example.test"), "Plan"),
            message(inbox, 2, Some("b@example.test"), "Plan B"),
            bridge.clone(),
        ],
    );
    let (merged, size) = thread(&rows(&store, &[inbox]), ids[0]);
    assert_eq!(size, 3);

    // The bridge goes (an expunge): the conversation stays merged, and its aliases stay, so
    // the bridge filed again elsewhere (a move) joins it again.
    store.remove(vec![ids[2]]).recv_blocking().unwrap();
    let all = rows(&store, &[inbox]);
    assert_eq!(thread(&all, ids[0]), (merged, 2));
    assert_eq!(thread(&all, ids[1]), (merged, 2));
    bridge.folder = archive;
    bridge.uid = Some(1);
    let moved = ingest(&store, vec![bridge])[0];
    assert_eq!(thread(&rows(&store, &[archive]), moved), (merged, 3));

    // Once its last message is gone, the conversation and its aliases go with it.
    store
        .remove(vec![ids[0], ids[1], moved])
        .recv_blocking()
        .unwrap();
    let mut late = message(inbox, 4, Some("late@example.test"), "Re: Plan");
    late.headers.in_reply_to = vec!["a@example.test".into()];
    let mut later = message(inbox, 5, Some("later@example.test"), "Re: Plan B");
    later.headers.in_reply_to = vec!["b@example.test".into()];
    let fresh = ingest(&store, vec![late, later]);
    let all = rows(&store, &[inbox]);
    assert_eq!(thread(&all, fresh[0]).1, 1);
    assert_eq!(thread(&all, fresh[1]).1, 1);
    assert_ne!(thread(&all, fresh[0]).0, thread(&all, fresh[1]).0);
    clean(&store);
    store.close().unwrap();
}

#[test]
fn the_check_finds_aliases_that_point_elsewhere_or_are_missing() {
    let dir = scratch();
    let store = open(dir.path());
    let work = account(&store, "Work");
    let inbox = folder(&store, work, "INBOX", FolderRole::Inbox);
    let mut reply = message(inbox, 2, Some("reply@example.test"), "Re: Tur");
    reply.headers.references = vec!["trip@example.test".into()];
    reply.headers.thread_index = Some(conversation(3));
    ingest(
        &store,
        vec![
            message(inbox, 1, Some("trip@example.test"), "Tur"),
            reply,
            message(inbox, 3, Some("alone@example.test"), "Annet"),
        ],
    );
    clean(&store);
    let conn = Connection::open(store.path()).unwrap();
    let alone: i64 = conn
        .query_row(
            "SELECT thread_id FROM messages WHERE message_id = 'alone@example.test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let thread_index_alias = format!("{}{}", writer::THREAD_INDEX_ALIAS, conversation(3).to_hex());
    // Each key, whether its alias is deleted or pointed at another conversation, and how many
    // messages then disagree.
    for (key, missing, disagree) in [
        ("trip@example.test", false, 2),
        (thread_index_alias.as_str(), false, 1),
        ("reply@example.test", true, 1),
    ] {
        let (account, thread): (i64, i64) = conn
            .query_row(
                "SELECT account_id, thread_id FROM thread_aliases WHERE message_id = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        if missing {
            conn.execute("DELETE FROM thread_aliases WHERE message_id = ?1", [key])
        } else {
            conn.execute(
                "UPDATE thread_aliases SET thread_id = ?2 WHERE message_id = ?1",
                rusqlite::params![key, alone],
            )
        }
        .unwrap();
        assert_eq!(
            store.check().recv_blocking().unwrap(),
            [format!("threading aliases: {disagree} disagree")],
            "{key}"
        );
        conn.execute(
            "INSERT OR REPLACE INTO thread_aliases(account_id, message_id, thread_id) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![account, key, thread],
        )
        .unwrap();
        clean(&store);
    }
    drop(conn);
    store.close().unwrap();
}

/// One message of a generated corpus. Ids index small pools, so keys collide often.
#[derive(Clone, Debug)]
struct Spec {
    account: usize,
    folder: usize,
    message_id: Option<u8>,
    in_reply_to: Option<u8>,
    references: Vec<u8>,
    thread_index: Option<u8>,
    date: i64,
}

/// What happens next: the next `n` messages of the shuffled corpus are ingested in one call,
/// with up to `again` live messages sent again, which the store must skip; `n` live messages
/// are removed, picked from `pick` on (an expunge); or up to `n` removed messages are filed
/// again in their account's other folder (a move).
#[derive(Clone, Debug)]
enum Step {
    Ingest { n: usize, again: usize },
    Remove { n: usize, pick: usize },
    Restore { n: usize },
}

const ACCOUNTS: usize = 3;
const FOLDERS: usize = 2;

fn spec() -> impl Strategy<Value = Spec> {
    (
        0..ACCOUNTS,
        0..FOLDERS,
        proptest::option::weighted(0.8, 0..10u8),
        proptest::option::weighted(0.4, 0..10u8),
        proptest::collection::vec(0..10u8, 0..4),
        proptest::option::weighted(0.25, 0..3u8),
        0..50i64,
    )
        .prop_map(
            |(account, folder, message_id, in_reply_to, references, thread_index, date)| Spec {
                account,
                folder,
                message_id,
                in_reply_to,
                references,
                thread_index,
                date,
            },
        )
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (1..9usize, 0..3usize).prop_map(|(n, again)| Step::Ingest { n, again }),
        2 => (1..4usize, 0..64usize).prop_map(|(n, pick)| Step::Remove { n, pick }),
        1 => (1..3usize).prop_map(|n| Step::Restore { n }),
    ]
}

/// A corpus, the order it arrives in, and the steps.
fn scenario() -> impl Strategy<Value = (Vec<Spec>, Vec<usize>, Vec<Step>)> {
    proptest::collection::vec(spec(), 1..40).prop_flat_map(|specs| {
        let order = Just((0..specs.len()).collect::<Vec<_>>()).prop_shuffle();
        let steps = proptest::collection::vec(step(), 1..24);
        (Just(specs), order, steps)
    })
}

impl Spec {
    fn new_message(&self, folder: FolderId, uid: u32) -> NewMessage {
        let id = |n: u8| format!("k{n}@example.test");
        let mut m = message(folder, uid, None, "Samme emne");
        m.date = self.date;
        m.headers.message_id = self.message_id.map(id);
        m.headers.in_reply_to = self.in_reply_to.map(id).into_iter().collect();
        m.headers.references = self.references.iter().copied().map(id).collect();
        m.headers.thread_index = self.thread_index.map(|n| conversation(n + 1));
        m
    }

    /// The alias keys the store files for this message, in the model's terms.
    fn keys(&self) -> BTreeSet<String> {
        let mut keys: BTreeSet<String> = self
            .message_id
            .iter()
            .chain(&self.in_reply_to)
            .chain(&self.references)
            .map(|n| format!("id {n}"))
            .collect();
        keys.extend(self.thread_index.map(|n| format!("index {n}")));
        keys
    }
}

/// The reference model: per account, the connected components of the alias graph over the
/// messages stored so far. A component whose last message goes is forgotten with its keys.
#[derive(Default)]
struct Model {
    components: Vec<(usize, BTreeSet<String>, BTreeSet<usize>)>,
}

impl Model {
    fn insert(&mut self, specs: &[Spec], message: usize) {
        let spec = &specs[message];
        let keys = spec.keys();
        let (linked, rest): (Vec<_>, Vec<_>) = self
            .components
            .drain(..)
            .partition(|c| c.0 == spec.account && !c.1.is_disjoint(&keys));
        self.components = rest;
        let mut joined = (spec.account, keys, BTreeSet::from([message]));
        for (_, keys, members) in linked {
            joined.1.extend(keys);
            joined.2.extend(members);
        }
        self.components.push(joined);
    }

    fn remove(&mut self, message: usize) {
        for component in &mut self.components {
            component.2.remove(&message);
        }
        self.components.retain(|component| !component.2.is_empty());
    }

    /// The conversations as sets of messages, each with its size: Message-IDs once, messages
    /// without one each.
    fn conversations(&self, specs: &[Spec]) -> BTreeSet<(BTreeSet<usize>, u32)> {
        self.components
            .iter()
            .map(|(_, _, members)| {
                let ids: BTreeSet<u8> = members
                    .iter()
                    .filter_map(|m| specs[*m].message_id)
                    .collect();
                let anonymous = members
                    .iter()
                    .filter(|m| specs[**m].message_id.is_none())
                    .count();
                (members.clone(), (ids.len() + anonymous) as u32)
            })
            .collect()
    }
}

/// 64 cases in CI; `PROPTEST_CASES` asks for more.
fn config() -> ProptestConfig {
    let config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_some() {
        config
    } else {
        ProptestConfig {
            cases: 64,
            ..config
        }
    }
}

/// Where a message is stored: its folder and UID.
type At = (FolderId, u32);

fn run_scenario(specs: &[Spec], order: &[usize], steps: &[Step]) -> Result<(), TestCaseError> {
    let dir = scratch();
    let store = open(dir.path());
    let folders: Vec<[FolderId; FOLDERS]> = (0..ACCOUNTS)
        .map(|n| {
            let account = account(&store, &format!("Account{n}"));
            [
                folder(&store, account, "INBOX", FolderRole::Inbox),
                folder(&store, account, "Archive", FolderRole::Archive),
            ]
        })
        .collect();
    let home = |m: usize| (folders[specs[m].account][specs[m].folder], m as u32 + 1);
    let mut model = Model::default();
    let mut next = 0;
    // Live messages: their stored id and where they are.
    let mut live: BTreeMap<usize, (MessageId, At)> = BTreeMap::new();
    let mut removed: Vec<usize> = Vec::new();
    let mut restores = 0;
    let rest = Step::Ingest {
        n: order.len(),
        again: 0,
    };
    for step in steps.iter().chain([&rest]) {
        // The messages to store, where, and the live ones sent again.
        let (batch, homes, again) = match *step {
            Step::Ingest { n, again } => {
                let batch: Vec<usize> = order[next..(next + n).min(order.len())].to_vec();
                next += batch.len();
                let homes: Vec<At> = batch.iter().map(|m| home(*m)).collect();
                let again: Vec<(usize, At)> = live
                    .iter()
                    .take(again)
                    .map(|(m, (_, at))| (*m, *at))
                    .collect();
                (batch, homes, again)
            }
            Step::Remove { n, pick } => {
                let gone: Vec<usize> = live
                    .keys()
                    .copied()
                    .cycle()
                    .skip(pick)
                    .take(n.min(live.len()))
                    .collect();
                let ids: Vec<MessageId> = gone.iter().map(|m| live[m].0).collect();
                store.remove(ids).recv_blocking().unwrap();
                for m in gone {
                    live.remove(&m);
                    model.remove(m);
                    removed.push(m);
                }
                continue;
            }
            Step::Restore { n } => {
                restores += 1;
                let batch: Vec<usize> = removed.drain(..n.min(removed.len())).collect();
                let homes: Vec<At> = batch
                    .iter()
                    .map(|m| {
                        let other = folders[specs[*m].account][1 - specs[*m].folder];
                        (other, restores * 1_000 + *m as u32 + 1)
                    })
                    .collect();
                (batch, homes, Vec::new())
            }
        };
        let messages = batch
            .iter()
            .copied()
            .zip(homes.iter().copied())
            .chain(again)
            .map(|(m, (folder, uid))| specs[m].new_message(folder, uid))
            .collect();
        let reply = store.ingest(messages).recv_blocking().unwrap();
        prop_assert!(
            reply.ids[batch.len()..].iter().all(Option::is_none),
            "sent again: skipped"
        );
        for ((m, at), id) in batch.iter().zip(homes).zip(&reply.ids) {
            prop_assert!(id.is_some());
            live.insert(*m, (id.unwrap(), at));
            model.insert(specs, *m);
        }
    }
    prop_assert_eq!(next, order.len());
    let by_id: BTreeMap<MessageId, usize> = live.iter().map(|(m, (id, _))| (*id, *m)).collect();
    let mut stored: BTreeMap<ThreadId, (BTreeSet<usize>, BTreeSet<u32>)> = BTreeMap::new();
    let all_folders: Vec<FolderId> = folders.iter().flatten().copied().collect();
    for row in rows(&store, &all_folders).into_values() {
        let entry = stored.entry(row.thread).or_default();
        entry.0.insert(by_id[&row.id]);
        entry.1.insert(row.thread_count);
    }
    prop_assert_eq!(
        by_id.len(),
        stored.values().map(|entry| entry.0.len()).sum::<usize>()
    );
    let mut found = BTreeSet::new();
    for (members, sizes) in stored.into_values() {
        prop_assert_eq!(sizes.len(), 1, "one size per conversation");
        found.insert((members, sizes.into_iter().next().unwrap()));
    }
    prop_assert_eq!(found, model.conversations(specs));
    prop_assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
    store.close().unwrap();
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn conversations_match_the_alias_graph_in_any_order(
        (specs, order, steps) in scenario()
    ) {
        run_scenario(&specs, &order, &steps)?;
    }
}
