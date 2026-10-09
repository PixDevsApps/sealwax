//! The op queue against a model server, as a property test: random interleavings of triage,
//! Undo, the op runner's outcomes (done with or without COPYUID, transient and permanent
//! failures, a crash between an action and its record), changes by another client (flags,
//! expunges, moves, new mail), UIDVALIDITY changes and reopening the store. After every step:
//! what is shown equals the server's flags in the store with the pending ops applied (folded
//! here, independently of the store's code), every open view equals a fresh read, and
//! `Store::check` finds nothing; after every full sync the store's server columns equal the
//! model server. At the end the runner drains the queue and the store equals the server.

use super::ops_tests::{NOW, UIDVALIDITY, server};
use super::tests::{Model, whole};
use super::*;
use proptest::prelude::*;
use rusqlite::{Connection, OpenFlags};
use sealwax_domain::model::{Address, Headers};
use sealwax_domain::triage::Triage;
use std::collections::BTreeMap;

const WINDOW: i64 = 8_000;
const HIDDEN: u32 = 256;

/// A pending op as the `ops` table holds it: kind, target, flags added and removed.
type PendingOp = (i64, Option<i64>, u32, u32);

/// The model server's folders, in the order of `ops_tests::folder_list`.
const FOLDERS: usize = 5;
const INBOX: usize = 0;

#[derive(Clone, Debug, Default)]
struct ServerFolder {
    uidvalidity: u32,
    uidnext: u32,
    /// UID to flags and the message's number (its Message-ID).
    messages: BTreeMap<u32, (Flags, u32)>,
}

impl ServerFolder {
    fn add(&mut self, flags: Flags, number: u32) -> u32 {
        let uid = self.uidnext;
        self.uidnext += 1;
        self.messages.insert(uid, (flags, number));
        uid
    }
}

struct World {
    folders: Vec<ServerFolder>,
    ids: [FolderId; FOLDERS],
    uidplus: bool,
    copyuid: bool,
    next_number: u32,
}

impl World {
    fn index(&self, folder: FolderId) -> usize {
        self.ids.iter().position(|id| *id == folder).unwrap()
    }
}

fn new_message(folder: FolderId, uid: u32, flags: Flags, number: u32) -> NewMessage {
    NewMessage {
        folder,
        uid: Some(uid),
        flags,
        date: 1_790_000_000 + i64::from(number) * 60,
        headers: Headers {
            from: vec![Address::new("Sender", format!("s{number}@example.test"))],
            subject: format!("Message {number}"),
            message_id: Some(format!("p{number}@example.test")),
            ..Headers::default()
        },
        snippet: format!("Snippet {number}"),
        search_text: format!("body {number}"),
        has_attachments: number.is_multiple_of(4),
        size: 100,
        ..NewMessage::default()
    }
}

#[derive(Clone, Debug)]
enum Step {
    Queue {
        action: u8,
        picks: Vec<u8>,
        window: bool,
    },
    /// Queue and Undo at once: every view must be as before.
    QueueUndo {
        action: u8,
        picks: Vec<u8>,
    },
    Undo {
        back: u8,
    },
    Tick {
        ms: u16,
    },
    /// One outcome per due group: 0–4 done, 5 transient, 6 permanent, 7 a crash after the
    /// action ran, 8 a crash before it ran.
    Run {
        outcomes: Vec<u8>,
    },
    Other {
        kind: u8,
        pick: u8,
    },
    Sync,
    Reset {
        folder: u8,
    },
    Reopen,
}

fn step() -> impl Strategy<Value = Step> {
    let picks = || proptest::collection::vec(any::<u8>(), 1..4);
    prop_oneof![
        5 => (0..10u8, picks(), any::<bool>())
            .prop_map(|(action, picks, window)| Step::Queue { action, picks, window }),
        1 => (0..10u8, picks()).prop_map(|(action, picks)| Step::QueueUndo { action, picks }),
        2 => (0..4u8).prop_map(|back| Step::Undo { back }),
        2 => (0..12_000u16).prop_map(|ms| Step::Tick { ms }),
        4 => proptest::collection::vec(0..9u8, 1..6).prop_map(|outcomes| Step::Run { outcomes }),
        2 => (0..4u8, any::<u8>()).prop_map(|(kind, pick)| Step::Other { kind, pick }),
        2 => Just(Step::Sync),
        1 => (0..FOLDERS as u8).prop_map(|folder| Step::Reset { folder }),
        1 => Just(Step::Reopen),
    ]
}

fn scenario() -> impl Strategy<Value = (u32, bool, bool, Vec<Step>)> {
    (
        4..14u32,
        any::<bool>(),
        any::<bool>(),
        proptest::collection::vec(step(), 1..36),
    )
}

/// 32 cases in CI; `PROPTEST_CASES` asks for more.
fn config() -> ProptestConfig {
    let config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_some() {
        config
    } else {
        ProptestConfig {
            cases: 32,
            ..config
        }
    }
}

fn action(code: u8, ids: &[FolderId; FOLDERS]) -> Triage {
    match code {
        0 => Triage::Read(true),
        1 => Triage::Read(false),
        2 => Triage::Star(true),
        3 => Triage::Star(false),
        4 => Triage::Archive,
        5 => Triage::Delete,
        6 => Triage::Junk,
        7 => Triage::MoveTo(ids[4]),
        8 => Triage::MoveTo(ids[INBOX]),
        _ => Triage::MoveTo(ids[1]),
    }
}

struct Harness {
    path: std::path::PathBuf,
    store: Store,
    account: AccountId,
    world: World,
    now: i64,
    batches: Vec<BatchId>,
    views: Vec<Model>,
    events: async_channel::Receiver<std::sync::Arc<ChangeEvent>>,
}

impl Harness {
    fn specs(&self) -> Vec<ViewSpec> {
        let ids = self.world.ids;
        vec![
            ViewSpec::folder(ids[INBOX]),
            ViewSpec::folder(ids[INBOX]).with_filter(ViewFilter::Unread),
            ViewSpec::folder(ids[INBOX]).with_filter(ViewFilter::Starred),
            ViewSpec::folder(ids[1]),
            ViewSpec::folder(ids[2]).with_filter(ViewFilter::Attachments),
            ViewSpec::folder(ids[3]),
            ViewSpec::folder(ids[4]),
            ViewSpec::unified_inbox(),
        ]
    }

    fn open_views(&mut self) {
        self.events = self.store.subscribe();
        self.views = self
            .specs()
            .into_iter()
            .map(|spec| {
                let opened = self
                    .store
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
            })
            .collect();
    }

    fn reopen(&mut self) {
        self.views.clear();
        let store = Store::open(&self.path, StoreOptions::default()).unwrap();
        std::mem::replace(&mut self.store, store).close().unwrap();
        self.open_views();
    }

    /// What every view shows: ids and flags.
    fn shown(&self) -> Vec<Vec<(MessageId, Flags)>> {
        self.views
            .iter()
            .map(|model| model.rows.iter().map(|r| (r.id, r.flags)).collect())
            .collect()
    }

    /// Every stored message of the account.
    fn stored(&self) -> Vec<MessageId> {
        self.world
            .ids
            .iter()
            .flat_map(|folder| self.store.folder_messages(*folder).recv_blocking().unwrap())
            .map(|stored| stored.id)
            .collect()
    }

    /// The folder pass of the sync engine, against the model: a UIDVALIDITY change resets the
    /// folder and syncs it again from scratch; otherwise new, changed and gone UIDs.
    fn sync(&mut self, index: usize) {
        let folder = self.world.ids[index];
        let theirs = self.world.folders[index].clone();
        let state = self
            .store
            .folder_sync(folder)
            .recv_blocking()
            .unwrap()
            .unwrap();
        if state.uidvalidity != Some(theirs.uidvalidity) {
            self.store
                .reset_folder(folder, theirs.uidvalidity)
                .recv_blocking()
                .unwrap();
            self.store
                .first_sync_step(
                    folder,
                    FirstSyncStep::Start {
                        uidvalidity: theirs.uidvalidity,
                        top: theirs.uidnext,
                        modseq: 1,
                    },
                )
                .recv_blocking()
                .unwrap();
            self.store
                .ingest(
                    theirs
                        .messages
                        .iter()
                        .rev()
                        .map(|(uid, (flags, number))| new_message(folder, *uid, *flags, *number))
                        .collect(),
                )
                .recv_blocking()
                .unwrap();
            self.store
                .first_sync_step(folder, FirstSyncStep::Floor(1))
                .recv_blocking()
                .unwrap();
            self.store
                .first_sync_step(folder, FirstSyncStep::Finish { below: 0 })
                .recv_blocking()
                .unwrap();
            return;
        }
        let ours: BTreeMap<u32, Flags> = self
            .store
            .folder_messages(folder)
            .recv_blocking()
            .unwrap()
            .into_iter()
            .map(|stored| (stored.uid, stored.flags))
            .collect();
        let changes = FolderChanges {
            new: theirs
                .messages
                .iter()
                .filter(|(uid, _)| !ours.contains_key(uid))
                .map(|(uid, (flags, number))| new_message(folder, *uid, *flags, *number))
                .collect(),
            flags: theirs
                .messages
                .iter()
                .filter(|(uid, (flags, _))| ours.get(uid).is_some_and(|ours| ours != flags))
                .map(|(uid, (flags, _))| FlagUpdate {
                    uid: *uid,
                    flags: *flags,
                    keywords: Vec::new(),
                })
                .collect(),
            removed: ours
                .keys()
                .filter(|uid| !theirs.messages.contains_key(uid))
                .copied()
                .collect(),
            uidnext: Some(theirs.uidnext),
            ..FolderChanges::default()
        };
        self.store
            .sync_changes(folder, changes)
            .recv_blocking()
            .unwrap();
    }

    fn sync_all(&mut self) {
        for index in 0..FOLDERS {
            self.sync(index);
        }
    }

    /// Carries a group out on the model server; replies with the record, or `None` when the
    /// server refuses the UIDVALIDITY.
    fn perform(&mut self, group: &OpGroup) -> Option<OpsDone> {
        let index = self.world.index(group.folder);
        if group.uidvalidity != Some(self.world.folders[index].uidvalidity) {
            return None;
        }
        let mut done = OpsDone {
            ops: group.ops.clone(),
            ..OpsDone::default()
        };
        match &group.action {
            OpAction::Flags { add, remove } => {
                for message in &group.messages {
                    if let Some((flags, _)) =
                        self.world.folders[index].messages.get_mut(&message.uid)
                    {
                        *flags = flags.union(*add).difference(*remove);
                    }
                }
            }
            OpAction::Move {
                to, add, remove, ..
            } => {
                let target = self.world.index(*to);
                for message in &group.messages {
                    let Some((flags, number)) =
                        self.world.folders[index].messages.remove(&message.uid)
                    else {
                        continue;
                    };
                    let uid = self.world.folders[target]
                        .add(flags.union(*add).difference(*remove), number);
                    if self.world.copyuid {
                        done.new_uids.push((message.uid, uid));
                    }
                }
                done.target_uidvalidity = Some(self.world.folders[target].uidvalidity);
            }
            OpAction::Expunge => {
                for message in &group.messages {
                    if self.world.uidplus {
                        self.world.folders[index].messages.remove(&message.uid);
                    } else if let Some((flags, _)) =
                        self.world.folders[index].messages.get_mut(&message.uid)
                    {
                        *flags = flags.union(Flags::DELETED);
                    }
                }
                done.flagged_only = !self.world.uidplus;
            }
            OpAction::ReadBelow { .. } => {}
            OpAction::Copy { to, .. } => {
                let target = self.world.index(*to);
                for message in &group.messages {
                    if let Some(&(flags, number)) =
                        self.world.folders[index].messages.get(&message.uid)
                    {
                        self.world.folders[target].add(flags, number);
                    }
                }
            }
        }
        Some(done)
    }

    fn run(&mut self, outcomes: &[u8]) -> Result<(), TestCaseError> {
        let due = self
            .store
            .due_ops(self.account, self.now)
            .recv_blocking()
            .unwrap();
        for refusal in &due.refused {
            prop_assert_eq!(refusal.account, self.account);
            prop_assert!(refusal.messages > 0);
        }
        for (n, group) in due.groups.iter().enumerate() {
            prop_assert!(!group.ops.is_empty());
            prop_assert!(
                group
                    .messages
                    .windows(2)
                    .all(|pair| pair[0].uid < pair[1].uid)
            );
            let target_uidnext = match group.action {
                OpAction::Move { to, .. } => Some(self.world.folders[self.world.index(to)].uidnext),
                _ => None,
            };
            self.store
                .start_ops(group.ops.clone(), target_uidnext)
                .recv_blocking()
                .unwrap();
            match outcomes.get(n).copied().unwrap_or(0) {
                outcome @ (0..=4 | 7) => match self.perform(group) {
                    Some(done) if outcome != 7 => {
                        self.store.ops_done(done).recv_blocking().unwrap();
                    }
                    Some(_) => {
                        self.reopen();
                        return Ok(());
                    }
                    None => {
                        self.store
                            .ops_failed(group.ops.clone(), Failure::Transient { retry_at: None })
                            .recv_blocking()
                            .unwrap();
                    }
                },
                5 => {
                    self.store
                        .ops_failed(
                            group.ops.clone(),
                            Failure::Transient {
                                retry_at: Some(self.now + 500),
                            },
                        )
                        .recv_blocking()
                        .unwrap();
                }
                6 => {
                    let refused = self
                        .store
                        .ops_failed(group.ops.clone(), Failure::Permanent)
                        .recv_blocking()
                        .unwrap();
                    prop_assert!(!refused.refused.is_empty());
                }
                _ => {
                    self.reopen();
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn other(&mut self, kind: u8, pick: u8) {
        let all: Vec<(usize, u32)> = (0..FOLDERS)
            .flat_map(|index| {
                self.world.folders[index]
                    .messages
                    .keys()
                    .map(move |uid| (index, *uid))
            })
            .collect();
        if kind == 3 || all.is_empty() {
            let number = self.world.next_number;
            self.world.next_number += 1;
            self.world.folders[INBOX].add(Flags::NONE, number);
            return;
        }
        let (index, uid) = all[usize::from(pick) % all.len()];
        match kind {
            0 => {
                let (flags, _) = self.world.folders[index].messages.get_mut(&uid).unwrap();
                let flip = [Flags::SEEN, Flags::FLAGGED, Flags::DELETED][usize::from(pick) % 3];
                *flags = if flags.contains(flip) {
                    flags.difference(flip)
                } else {
                    flags.union(flip)
                };
            }
            1 => {
                self.world.folders[index].messages.remove(&uid);
            }
            _ => {
                let (flags, number) = self.world.folders[index].messages.remove(&uid).unwrap();
                let target = (index + 1 + usize::from(pick) % (FOLDERS - 1)) % FOLDERS;
                self.world.folders[target].add(flags, number);
            }
        }
    }

    /// Applies the change events so far: every view must equal a fresh read.
    fn settle(&mut self) -> Result<(), TestCaseError> {
        while let Ok(event) = self.events.try_recv() {
            for model in &mut self.views {
                if event.version > model.version {
                    model.apply(&self.store, &event);
                }
            }
        }
        for model in &self.views {
            let fresh = whole(&self.store, model.view.spec());
            prop_assert_eq!(
                model
                    .rows
                    .iter()
                    .map(|r| (r.id, r.flags))
                    .collect::<Vec<_>>(),
                fresh
                    .rows
                    .iter()
                    .map(|r| (r.id, r.flags))
                    .collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    /// The invariants every step keeps.
    fn invariants(&mut self) -> Result<(), TestCaseError> {
        self.settle()?;
        prop_assert_eq!(
            self.store.check().recv_blocking().unwrap(),
            Vec::<String>::new()
        );
        let conn =
            Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut pending: BTreeMap<i64, Vec<PendingOp>> = BTreeMap::new();
        let mut select = conn
            .prepare(
                "SELECT message, kind, target, add_flags, remove_flags FROM ops \
                 WHERE done = 0 AND message IS NOT NULL ORDER BY id",
            )
            .unwrap();
        let rows = select
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
                ))
            })
            .unwrap();
        for row in rows {
            let (message, op) = row.unwrap();
            pending.entry(message).or_default().push(op);
        }
        let mut messages = conn
            .prepare("SELECT id, folder_id, coalesce(server_flags, flags), flags FROM messages")
            .unwrap();
        let rows = messages
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            })
            .unwrap();
        for row in rows {
            let (id, folder, server, flags) = row.unwrap();
            prop_assert_eq!(server & HIDDEN, 0);
            let (mut expected, mut at, mut gone) = (server, folder, false);
            for (kind, target, add, remove) in pending.get(&id).into_iter().flatten() {
                match kind {
                    0 => expected = (expected | add) & !remove,
                    1 => {
                        expected = (expected | add) & !remove;
                        at = target.unwrap();
                    }
                    2 => {
                        gone = true;
                        break;
                    }
                    _ => {}
                }
            }
            if gone || at != folder {
                expected |= HIDDEN;
            }
            prop_assert_eq!(flags, expected, "message {}", id);
        }
        Ok(())
    }

    /// After a full sync the store's server columns are the model server's.
    fn equals_server(&self) -> Result<(), TestCaseError> {
        for (index, folder) in self.world.ids.iter().enumerate() {
            let ours: BTreeMap<u32, Flags> = self
                .store
                .folder_messages(*folder)
                .recv_blocking()
                .unwrap()
                .into_iter()
                .map(|stored| (stored.uid, stored.flags))
                .collect();
            let theirs: BTreeMap<u32, Flags> = self.world.folders[index]
                .messages
                .iter()
                .map(|(uid, (flags, _))| (*uid, *flags))
                .collect();
            prop_assert_eq!(ours, theirs, "folder {}", index);
        }
        Ok(())
    }
}

fn run_scenario(
    initial: u32,
    uidplus: bool,
    copyuid: bool,
    steps: &[Step],
) -> Result<(), TestCaseError> {
    let fx = server();
    let ids = [fx.inbox, fx.archive, fx.trash, fx.junk, fx.projects];
    let mut folders: Vec<ServerFolder> = (0..FOLDERS)
        .map(|_| ServerFolder {
            uidvalidity: UIDVALIDITY,
            uidnext: 1,
            messages: BTreeMap::new(),
        })
        .collect();
    for number in 0..initial {
        let flags = if number % 3 == 0 {
            Flags::SEEN
        } else {
            Flags::NONE
        };
        folders[INBOX].add(flags, number);
    }
    let path = fx.store.path().to_owned();
    let mut harness = Harness {
        path,
        store: fx.store,
        account: fx.account,
        world: World {
            folders,
            ids,
            uidplus,
            copyuid,
            next_number: initial,
        },
        now: NOW,
        batches: Vec::new(),
        views: Vec::new(),
        events: async_channel::unbounded().1,
    };
    harness.sync_all();
    harness.open_views();
    for step in steps {
        match step {
            Step::Queue {
                action: code,
                picks,
                window,
            } => {
                let stored = harness.stored();
                if stored.is_empty() {
                    continue;
                }
                let messages: Vec<MessageId> = picks
                    .iter()
                    .map(|pick| stored[usize::from(*pick) % stored.len()])
                    .collect();
                let before: BTreeMap<MessageId, (FolderId, Flags)> = harness.views[..1]
                    .iter()
                    .flat_map(|model| model.rows.iter().map(|r| (r.id, (r.folder, r.flags))))
                    .collect();
                let triage = action(*code, &ids);
                let not_before = if *window {
                    harness.now + WINDOW
                } else {
                    harness.now
                };
                let queued = harness
                    .store
                    .queue_triage(QueueTriage {
                        action: triage,
                        messages: messages.clone(),
                        not_before,
                    })
                    .recv_blocking()
                    .unwrap();
                harness.batches.extend(queued.batch);
                harness.settle()?;
                // What the user asked for shows at once in the Inbox's list.
                let after: BTreeMap<MessageId, Flags> = harness.views[0]
                    .rows
                    .iter()
                    .map(|r| (r.id, r.flags))
                    .collect();
                for id in &messages {
                    if !before.contains_key(id) {
                        continue;
                    }
                    match triage {
                        Triage::Read(read) => prop_assert_eq!(after[id].is_seen(), read),
                        Triage::Star(star) => prop_assert_eq!(after[id].is_flagged(), star),
                        Triage::Archive | Triage::Delete | Triage::Junk => {
                            prop_assert!(!after.contains_key(id));
                        }
                        Triage::MoveTo(to) => {
                            prop_assert_eq!(after.contains_key(id), to == ids[INBOX]);
                        }
                    }
                }
            }
            Step::QueueUndo {
                action: code,
                picks,
            } => {
                let stored = harness.stored();
                if stored.is_empty() {
                    continue;
                }
                harness.settle()?;
                let before = harness.shown();
                let messages = picks
                    .iter()
                    .map(|pick| stored[usize::from(*pick) % stored.len()])
                    .collect();
                let queued = harness
                    .store
                    .queue_triage(QueueTriage {
                        action: action(*code, &ids),
                        messages,
                        not_before: harness.now + WINDOW,
                    })
                    .recv_blocking()
                    .unwrap();
                if let Some(batch) = queued.batch {
                    let undone = harness.store.undo(batch).recv_blocking().unwrap();
                    prop_assert_eq!(undone.reverted, 0);
                    prop_assert!(undone.lost.is_empty());
                }
                harness.settle()?;
                prop_assert_eq!(harness.shown(), before, "a quick Undo puts everything back");
            }
            Step::Undo { back } => {
                if let Some(batch) = harness
                    .batches
                    .len()
                    .checked_sub(1 + usize::from(*back))
                    .map(|n| harness.batches[n])
                {
                    harness.store.undo(batch).recv_blocking().unwrap();
                }
            }
            Step::Tick { ms } => harness.now += i64::from(*ms),
            Step::Run { outcomes } => harness.run(outcomes)?,
            Step::Other { kind, pick } => harness.other(*kind, *pick),
            Step::Sync => {
                harness.sync_all();
                harness.invariants()?;
                harness.equals_server()?;
            }
            Step::Reset { folder } => {
                let theirs = &mut harness.world.folders[usize::from(*folder)];
                let messages: Vec<(Flags, u32)> = theirs.messages.values().copied().collect();
                theirs.uidvalidity += 100;
                theirs.uidnext = 1;
                theirs.messages.clear();
                for (flags, number) in messages {
                    theirs.add(flags, number);
                }
            }
            Step::Reopen => harness.reopen(),
        }
        harness.invariants()?;
    }
    // The runner drains the queue; the store ends equal to the server, nothing overlaid.
    harness.now += 3_600_000;
    for _ in 0..12 {
        harness.sync_all();
        let pending = harness
            .store
            .pending_ops(harness.account)
            .recv_blocking()
            .unwrap();
        if pending == 0 {
            break;
        }
        harness.run(&[])?;
        harness.invariants()?;
    }
    harness.sync_all();
    harness.invariants()?;
    harness.equals_server()?;
    prop_assert_eq!(
        harness
            .store
            .pending_ops(harness.account)
            .recv_blocking()
            .unwrap(),
        0
    );
    let conn =
        Connection::open_with_flags(&harness.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let overlaid: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE server_flags IS NOT NULL OR flags & 256 <> 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    prop_assert_eq!(overlaid, 0);
    harness.views.clear();
    harness.store.close().unwrap();
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn the_shown_state_is_the_servers_with_the_pending_ops(
        (initial, uidplus, copyuid, steps) in scenario()
    ) {
        run_scenario(initial, uidplus, copyuid, &steps)?;
    }
}
