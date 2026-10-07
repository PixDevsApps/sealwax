//! At-most-once delivery (ADR-015) as a property test: random interleavings of queueing, Undo
//! send, time passing, the account's actor sending what is due (accepted, a transient failure
//! before DATA or a 4xx after it, a permanent refusal, the connection cut after DATA, Sealwax
//! stopped while the message is built, before or after DATA or before the answer is recorded),
//! reopening the store with the engine's recovery, the user's answers to uncertain sends, the
//! Sent copy, edits and discards.
//!
//! The model counts the user's authorizations: queueing a draft and "send again" give one;
//! Undo, "it was sent", "back to a draft", a permanent refusal and a stop while the message is
//! built (it comes back as a failed draft) withdraw it; data that may
//! have reached the server uses it up, while a definite refusal (4xx, 5xx) doesn't. The actor
//! transmits only after the store answers `Started`, and every transmission asserts an
//! authorization is held: no path sends a message twice without the user. After every step the
//! outbox's states and the due sends equal the model's and `check()` is clean.

use super::compose_tests::{draft, key, server};
use super::ops_tests::NOW;
use super::*;
use proptest::prelude::*;
use sealwax_domain::model::DraftId;
use std::collections::BTreeMap;
use std::path::PathBuf;

const DRAFTS: usize = 3;
const WINDOW: i64 = 7_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    Editable,
    Queued { not_before: i64 },
    Sending,
    Uncertain,
    Sent,
    Gone,
}

impl Expected {
    fn state(self) -> Option<SendState> {
        match self {
            Expected::Queued { .. } => Some(SendState::Queued),
            Expected::Sending => Some(SendState::Sending),
            Expected::Uncertain => Some(SendState::Uncertain),
            Expected::Sent => Some(SendState::Sent),
            Expected::Editable | Expected::Gone => None,
        }
    }
}

#[derive(Clone, Debug)]
enum Step {
    Queue {
        draft: u8,
        window: bool,
    },
    Undo {
        draft: u8,
    },
    Tick {
        ms: u16,
    },
    /// One outcome per due send, in order: 0 a stop before the start, 1 a transient failure
    /// before DATA, 2 a permanent one before DATA, 3 accepted, 4 a 4xx reply to the data, 5 a
    /// 5xx, 6 the connection cut after DATA, 7 a stop after the start before DATA, 8 a stop
    /// after DATA, 9 accepted and a stop before it is recorded, 10 a stop while its message is
    /// built (a send with recorded bytes builds nothing: a stop before the start).
    Engine {
        outcomes: Vec<u8>,
    },
    Reopen,
    Resolve {
        draft: u8,
        how: u8,
    },
    Copy {
        draft: u8,
        done: bool,
    },
    Edit {
        draft: u8,
    },
    Discard {
        draft: u8,
    },
}

fn step() -> impl Strategy<Value = Step> {
    let draft = || 0..DRAFTS as u8;
    prop_oneof![
        4 => (draft(), any::<bool>()).prop_map(|(draft, window)| Step::Queue { draft, window }),
        2 => draft().prop_map(|draft| Step::Undo { draft }),
        3 => (0..12_000u16).prop_map(|ms| Step::Tick { ms }),
        4 => proptest::collection::vec(0..11u8, 1..4).prop_map(|outcomes| Step::Engine { outcomes }),
        1 => Just(Step::Reopen),
        2 => (draft(), 0..3u8).prop_map(|(draft, how)| Step::Resolve { draft, how }),
        1 => (draft(), any::<bool>()).prop_map(|(draft, done)| Step::Copy { draft, done }),
        1 => draft().prop_map(|draft| Step::Edit { draft }),
        1 => draft().prop_map(|draft| Step::Discard { draft }),
    ]
}

struct World {
    _dir: tempfile::TempDir,
    path: PathBuf,
    store: Option<Store>,
    account: AccountId,
    now: i64,
    ids: [Option<DraftId>; DRAFTS],
    expected: [Expected; DRAFTS],
    /// The user asked for a send that no transmission may have delivered since.
    authorized: [bool; DRAFTS],
    /// Transmissions whose data may have reached the server.
    delivered: [u32; DRAFTS],
    /// Queues and "send again".
    grants: [u32; DRAFTS],
    built: u8,
}

impl World {
    fn new() -> World {
        let cx = server();
        let path = cx.path();
        let mut world = World {
            _dir: cx.dir,
            path,
            store: Some(cx.store),
            account: cx.account,
            now: NOW,
            ids: [None; DRAFTS],
            expected: [Expected::Gone; DRAFTS],
            authorized: [false; DRAFTS],
            delivered: [0; DRAFTS],
            grants: [0; DRAFTS],
            built: 0,
        };
        for slot in 0..DRAFTS {
            world.create(slot);
        }
        world
    }

    fn store(&self) -> &Store {
        self.store.as_ref().unwrap()
    }

    fn create(&mut self, slot: usize) {
        let saved = self
            .store()
            .save_draft(SaveDraft {
                id: None,
                draft: draft(self.account, slot as u32 + 1),
                open: true,
                now: self.now,
            })
            .recv_blocking()
            .unwrap();
        let DraftSaved::Saved { id, .. } = saved else {
            panic!("{saved:?}");
        };
        self.ids[slot] = Some(id);
        self.expected[slot] = Expected::Editable;
    }

    fn slot(&self, id: DraftId) -> usize {
        self.ids.iter().position(|slot| *slot == Some(id)).unwrap()
    }

    /// Stopped hard and started again: the engine's start makes every send left sending
    /// uncertain, and the one whose message was being built (`building`) a failed draft.
    fn reopen(&mut self, building: Option<usize>) {
        drop(self.store.take());
        let store = Store::open(&self.path, StoreOptions::default()).unwrap();
        let recovered = store.recover_sends().recv_blocking().unwrap();
        self.store = Some(store);
        let mut expected: Vec<(DraftId, Option<SendFailure>)> = (0..DRAFTS)
            .filter(|slot| self.expected[*slot] == Expected::Sending)
            .filter_map(|slot| Some((self.ids[slot]?, None)))
            .chain(
                building.and_then(|slot| Some((self.ids[slot]?, Some(SendFailure::Interrupted)))),
            )
            .collect();
        let mut got: Vec<(DraftId, Option<SendFailure>)> = recovered
            .iter()
            .map(|change| (change.draft, change.failure))
            .collect();
        expected.sort_by_key(|(id, _)| *id);
        got.sort_by_key(|(id, _)| *id);
        assert_eq!(got, expected);
        for slot in 0..DRAFTS {
            if self.expected[slot] == Expected::Sending {
                self.expected[slot] = Expected::Uncertain;
            }
        }
        if let Some(slot) = building {
            self.expected[slot] = Expected::Editable;
            self.authorized[slot] = false;
        }
    }

    /// The actor's DATA: only with an authorization held.
    fn transmit(&mut self, slot: usize, may_deliver: bool) {
        assert!(
            self.authorized[slot],
            "draft {slot} transmitted without the user's authorization"
        );
        if may_deliver {
            self.authorized[slot] = false;
            self.delivered[slot] += 1;
        }
    }

    fn prepared(&mut self) -> Prepared {
        self.built = self.built.wrapping_add(1);
        Prepared {
            message_blob: key(200u8.wrapping_add(self.built)),
            sent_copy_blob: None,
            size: 10,
            envelope_from: "me@example.test".into(),
            envelope_to: vec!["kari@example.test".into()],
        }
    }

    fn apply(&mut self, step: &Step) {
        match *step {
            Step::Queue { draft, window } => self.queue(draft as usize, window),
            Step::Undo { draft } => self.undo(draft as usize),
            Step::Tick { ms } => self.now += i64::from(ms),
            Step::Engine { ref outcomes } => self.engine(outcomes),
            Step::Reopen => self.reopen(None),
            Step::Resolve { draft, how } => self.resolve(draft as usize, how),
            Step::Copy { draft, done } => self.copy(draft as usize, done),
            Step::Edit { draft } => self.edit(draft as usize),
            Step::Discard { draft } => self.discard(draft as usize),
        }
    }

    fn queue(&mut self, slot: usize, window: bool) {
        let Some(id) = self.ids[slot] else {
            return;
        };
        let not_before = self.now + if window { WINDOW } else { 0 };
        let queued = self
            .store()
            .queue_send(id, not_before)
            .recv_blocking()
            .unwrap();
        match self.expected[slot] {
            Expected::Editable => {
                assert!(matches!(queued, SendQueued::Queued { .. }), "{queued:?}");
                self.expected[slot] = Expected::Queued { not_before };
                self.authorized[slot] = true;
                self.grants[slot] += 1;
            }
            other => assert_eq!(queued, SendQueued::NotEditable(other.state().unwrap())),
        }
    }

    fn undo(&mut self, slot: usize) {
        let Some(id) = self.ids[slot] else {
            return;
        };
        let undone = self.store().undo_send(id).recv_blocking().unwrap();
        match self.expected[slot] {
            Expected::Queued { .. } => {
                assert!(matches!(undone, SendUndone::Undone { .. }), "{undone:?}");
                self.expected[slot] = Expected::Editable;
                self.authorized[slot] = false;
            }
            Expected::Editable => assert_eq!(undone, SendUndone::NotQueued),
            other => assert_eq!(undone, SendUndone::TooLate(other.state().unwrap())),
        }
    }

    fn engine(&mut self, outcomes: &[u8]) {
        let due = self
            .store()
            .due_sends(self.account, self.now)
            .recv_blocking()
            .unwrap();
        for (index, send) in due.sends.into_iter().enumerate() {
            let slot = self.slot(send.draft);
            let id = send.draft;
            let outcome = outcomes[index % outcomes.len()];
            if send.prepared.is_none() && outcome != 0 {
                // The actor builds the message, recording that first.
                let building = self
                    .store()
                    .send_building(id, self.now)
                    .recv_blocking()
                    .unwrap();
                assert!(building);
                if outcome == 10 {
                    self.reopen(Some(slot));
                    return;
                }
                self.store().send_built(id).recv_blocking().unwrap();
            }
            match outcome {
                0 | 10 => {
                    self.reopen(None);
                    return;
                }
                1 => {
                    let retry_at = self.now + 1_000;
                    let failed = self
                        .store()
                        .send_failed(id, SendFailed::Transient { retry_at })
                        .recv_blocking()
                        .unwrap();
                    assert!(failed.applied);
                    self.expected[slot] = Expected::Queued {
                        not_before: retry_at,
                    };
                    continue;
                }
                2 => {
                    let failed = self
                        .store()
                        .send_failed(id, SendFailed::Permanent(SendFailure::RecipientsRefused))
                        .recv_blocking()
                        .unwrap();
                    assert!(failed.applied);
                    self.expected[slot] = Expected::Editable;
                    self.authorized[slot] = false;
                    continue;
                }
                _ => {}
            }
            let prepared = match send.prepared {
                Some(prepared) => prepared,
                None => self.prepared(),
            };
            let started = self
                .store()
                .send_started(id, prepared, self.now)
                .recv_blocking()
                .unwrap();
            assert_eq!(started, SendStarted::Started);
            self.expected[slot] = Expected::Sending;
            match outcome {
                3 => {
                    self.transmit(slot, true);
                    let done = self
                        .store()
                        .send_done(id, self.now)
                        .recv_blocking()
                        .unwrap();
                    assert!(done.applied);
                    self.expected[slot] = Expected::Sent;
                }
                4 => {
                    self.transmit(slot, false);
                    let retry_at = self.now + 500;
                    self.store()
                        .send_failed(id, SendFailed::Transient { retry_at })
                        .recv_blocking()
                        .unwrap();
                    self.expected[slot] = Expected::Queued {
                        not_before: retry_at,
                    };
                }
                5 => {
                    self.transmit(slot, false);
                    self.store()
                        .send_failed(id, SendFailed::Permanent(SendFailure::Refused))
                        .recv_blocking()
                        .unwrap();
                    self.expected[slot] = Expected::Editable;
                    self.authorized[slot] = false;
                }
                6 => {
                    self.transmit(slot, true);
                    let uncertain = self.store().send_uncertain(id).recv_blocking().unwrap();
                    assert!(uncertain.applied);
                    self.expected[slot] = Expected::Uncertain;
                }
                7 => {
                    self.reopen(None);
                    return;
                }
                8 | 9 => {
                    self.transmit(slot, true);
                    self.reopen(None);
                    return;
                }
                other => unreachable!("outcome {other}"),
            }
        }
    }

    fn resolve(&mut self, slot: usize, how: u8) {
        let Some(id) = self.ids[slot] else {
            return;
        };
        let resolution = match how {
            0 => Resolution::SendAgain {
                not_before: self.now,
            },
            1 => Resolution::WasSent,
            _ => Resolution::BackToDraft,
        };
        let resolved = self
            .store()
            .resolve_uncertain(id, resolution, self.now)
            .recv_blocking()
            .unwrap();
        if self.expected[slot] != Expected::Uncertain {
            assert!(!resolved.applied);
            return;
        }
        assert!(resolved.applied);
        match resolution {
            Resolution::SendAgain { not_before } => {
                self.expected[slot] = Expected::Queued { not_before };
                self.authorized[slot] = true;
                self.grants[slot] += 1;
            }
            Resolution::WasSent => {
                self.expected[slot] = Expected::Sent;
                self.authorized[slot] = false;
            }
            Resolution::BackToDraft => {
                self.expected[slot] = Expected::Editable;
                self.authorized[slot] = false;
            }
        }
    }

    fn copy(&mut self, slot: usize, done: bool) {
        let Some(id) = self.ids[slot] else {
            return;
        };
        let sent = self.expected[slot] == Expected::Sent;
        if done {
            let finished = self
                .store()
                .sent_copy_done(id, SentCopyDone::Appended { uid: None })
                .recv_blocking()
                .unwrap();
            assert_eq!(finished.applied, sent);
            if sent {
                self.expected[slot] = Expected::Gone;
                self.ids[slot] = None;
            }
        } else {
            let failed = self
                .store()
                .sent_copy_failed(id, self.now + 1_000)
                .recv_blocking()
                .unwrap();
            assert_eq!(failed.applied, sent);
        }
    }

    fn edit(&mut self, slot: usize) {
        let Some(id) = self.ids[slot] else {
            self.create(slot);
            return;
        };
        let saved = self
            .store()
            .save_draft(SaveDraft {
                id: Some(id),
                draft: draft(self.account, slot as u32 + 1),
                open: true,
                now: self.now,
            })
            .recv_blocking()
            .unwrap();
        match self.expected[slot] {
            Expected::Editable => assert!(matches!(saved, DraftSaved::Saved { .. })),
            other => assert_eq!(
                saved,
                DraftSaved::NotEditable {
                    id,
                    state: other.state().unwrap()
                }
            ),
        }
    }

    fn discard(&mut self, slot: usize) {
        let Some(id) = self.ids[slot] else {
            return;
        };
        let discarded = self.store().discard_draft(id).recv_blocking().unwrap();
        match self.expected[slot] {
            Expected::Editable | Expected::Queued { .. } => {
                assert!(discarded.found && discarded.refused.is_none());
                self.expected[slot] = Expected::Gone;
                self.ids[slot] = None;
                self.authorized[slot] = false;
            }
            other => assert_eq!(discarded.refused, other.state()),
        }
    }

    fn check(&self) {
        let store = self.store();
        let outbox: BTreeMap<DraftId, SendState> = store
            .outbox(None)
            .recv_blocking()
            .unwrap()
            .into_iter()
            .map(|entry| (entry.draft, entry.state))
            .collect();
        let expected: BTreeMap<DraftId, SendState> = (0..DRAFTS)
            .filter_map(|slot| Some((self.ids[slot]?, self.expected[slot].state()?)))
            .collect();
        assert_eq!(outbox, expected);
        for slot in 0..DRAFTS {
            if let Some(id) = self.ids[slot] {
                let stored = store.draft(id).recv_blocking().unwrap();
                assert_eq!(stored.unwrap().send, self.expected[slot].state());
            }
        }
        let mut due: Vec<DraftId> = store
            .due_sends(self.account, self.now)
            .recv_blocking()
            .unwrap()
            .sends
            .iter()
            .map(|send| send.draft)
            .collect();
        let mut want: Vec<DraftId> = (0..DRAFTS)
            .filter(|slot| {
                matches!(self.expected[*slot],
                    Expected::Queued { not_before } if not_before <= self.now)
            })
            .filter_map(|slot| self.ids[slot])
            .collect();
        due.sort();
        want.sort();
        assert_eq!(due, want);
        assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
        for slot in 0..DRAFTS {
            assert!(self.delivered[slot] <= self.grants[slot]);
        }
    }
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

proptest! {
    #![proptest_config(config())]

    #[test]
    fn no_path_sends_a_message_twice_without_the_user(
        steps in proptest::collection::vec(step(), 1..60)
    ) {
        let mut world = World::new();
        for step in &steps {
            world.apply(step);
            world.check();
        }
        // The actor drains what is due; the user says what was sent.
        for _ in 0..20 {
            world.now += 60_000;
            world.engine(&[3]);
            for slot in 0..DRAFTS {
                world.resolve(slot, 1);
                world.copy(slot, true);
            }
            world.check();
        }
        for slot in 0..DRAFTS {
            prop_assert!(
                matches!(world.expected[slot], Expected::Editable | Expected::Gone),
                "{:?}", world.expected
            );
        }
        world.store.take().unwrap().close().unwrap();
    }
}
