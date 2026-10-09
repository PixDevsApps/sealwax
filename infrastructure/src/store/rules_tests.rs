//! What an account's rules queue (ADR-020): moves and flags with their overlay, copies as ops of
//! their own that run before what follows, a copy refused as a copy, Undo leaving copies made,
//! and local accounts leaving copies to the caller.

use super::ops_tests::{
    NOW, clean, counts, due, held_ids, ingest_local, local, server, server_flags, shown, ten,
};
use super::*;

fn rules(store: &Store, messages: Vec<RuleChanges>) -> Queued {
    store.queue_rules(messages).recv_blocking().unwrap()
}

fn done(store: &Store, group: &OpGroup, new_uids: Vec<(u32, u32)>) {
    store
        .start_ops(group.ops.clone(), Some(100))
        .recv_blocking()
        .unwrap();
    store
        .ops_done(OpsDone {
            ops: group.ops.clone(),
            new_uids,
            ..OpsDone::default()
        })
        .recv_blocking()
        .unwrap();
}

#[test]
fn moves_flags_and_copies_queue_as_one_batch_and_copies_run_first() {
    let fx = server();
    let ids = ten(&fx);
    let queued = rules(
        &fx.store,
        vec![
            RuleChanges {
                message: ids[0],
                changes: vec![
                    RuleChange::Copy { to: fx.projects },
                    RuleChange::Move {
                        to: fx.archive,
                        add: Flags::SEEN,
                        remove: Flags::NONE,
                    },
                ],
            },
            RuleChanges {
                message: ids[2],
                changes: vec![RuleChange::Flags {
                    add: Flags::FLAGGED,
                    remove: Flags::NONE,
                }],
            },
            RuleChanges {
                message: ids[4],
                changes: vec![RuleChange::Move {
                    to: fx.inbox,
                    add: Flags::NONE,
                    remove: Flags::NONE,
                }],
            },
        ],
    );
    assert_eq!(queued.applied, 2);
    assert_eq!(queued.not_applied, vec![(NotApplied::NoTarget, 1)]);
    assert_eq!(queued.accounts, vec![fx.account]);
    assert!(queued.batch.is_some());
    // The overlay: the moved message has left the Inbox, the starred one shows its star.
    assert_eq!(counts(&fx.store, fx.inbox), (9, 4));
    assert!(shown(&fx.store, ids[2]).unwrap().contains(Flags::FLAGGED));

    let first = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(
        first.len(),
        2,
        "the copy and the star; the move waits for the copy"
    );
    let copy = first
        .iter()
        .find(|group| matches!(group.action, OpAction::Copy { .. }))
        .unwrap();
    assert_eq!(
        copy.action,
        OpAction::Copy {
            to: fx.projects,
            to_name: "Projects".into(),
            to_uidvalidity: Some(7),
        }
    );
    assert_eq!(copy.folder, fx.inbox);
    assert_eq!(copy.messages.len(), 1);
    for group in &first {
        done(&fx.store, group, Vec::new());
    }
    assert_eq!(
        held_ids(&fx.store, fx.inbox).get(&1),
        Some(&ids[0]),
        "a copy leaves the message where it is"
    );
    assert!(
        held_ids(&fx.store, fx.projects).is_empty(),
        "its sync fetches the copy"
    );

    let second = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(second.len(), 1);
    assert!(matches!(
        second[0].action,
        OpAction::Move { to, add, .. } if to == fx.archive && add == Flags::SEEN
    ));
    done(&fx.store, &second[0], vec![(1, 100)]);
    assert_eq!(held_ids(&fx.store, fx.archive).get(&100), Some(&ids[0]));
    assert_eq!(server_flags(&fx.store, fx.archive)[&100], Flags::SEEN);
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_refused_copy_is_reported_as_a_copy_and_what_follows_still_runs() {
    let fx = server();
    let ids = ten(&fx);
    rules(
        &fx.store,
        vec![RuleChanges {
            message: ids[1],
            changes: vec![
                RuleChange::Copy { to: fx.projects },
                RuleChange::Flags {
                    add: Flags::FLAGGED,
                    remove: Flags::NONE,
                },
            ],
        }],
    );
    let groups = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(groups.len(), 1);
    let failed = fx
        .store
        .ops_failed(groups[0].ops.clone(), Failure::Permanent)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        failed.refused,
        vec![OpsRefusal {
            account: fx.account,
            change: RefusedChange::Copy,
            messages: 1,
        }]
    );
    let next = due(&fx.store, fx.account, NOW).groups;
    assert_eq!(next.len(), 1);
    assert!(matches!(next[0].action, OpAction::Flags { .. }));
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn undo_leaves_copies_made_and_puts_the_message_back() {
    let fx = server();
    let ids = ten(&fx);
    let queued = rules(
        &fx.store,
        vec![RuleChanges {
            message: ids[0],
            changes: vec![
                RuleChange::Copy { to: fx.projects },
                RuleChange::Move {
                    to: fx.archive,
                    add: Flags::NONE,
                    remove: Flags::NONE,
                },
            ],
        }],
    );
    let copy = due(&fx.store, fx.account, NOW).groups.remove(0);
    done(&fx.store, &copy, Vec::new());
    let undone = fx
        .store
        .undo(queued.batch.unwrap())
        .recv_blocking()
        .unwrap();
    assert_eq!(undone.restored, 1, "the move hadn't started");
    assert!(
        undone.lost.is_empty(),
        "the copy made stays, and isn't lost"
    );
    assert_eq!(counts(&fx.store, fx.inbox), (10, 5));
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn a_message_already_on_its_way_is_left_alone() {
    let fx = server();
    let ids = ten(&fx);
    let archive = RuleChange::Move {
        to: fx.archive,
        add: Flags::NONE,
        remove: Flags::NONE,
    };
    let once = vec![RuleChanges {
        message: ids[0],
        changes: vec![archive],
    }];
    assert_eq!(rules(&fx.store, once.clone()).applied, 1);
    let again = rules(&fx.store, once);
    assert_eq!(again.applied, 0);
    assert_eq!(again.not_applied, vec![(NotApplied::Gone, 1)]);
    assert_eq!(again.batch, None);
    clean(&fx.store);
    fx.store.close().unwrap();
}

#[test]
fn local_accounts_apply_at_once_and_leave_copies_to_the_caller() {
    let fx = local();
    let ids = ingest_local(&fx);
    let queued = rules(
        &fx.store,
        vec![RuleChanges {
            message: ids[0],
            changes: vec![
                RuleChange::Copy { to: fx.projects },
                RuleChange::Move {
                    to: fx.archive,
                    add: Flags::SEEN,
                    remove: Flags::NONE,
                },
            ],
        }],
    );
    assert_eq!(queued.applied, 1);
    assert_eq!(queued.not_applied, vec![(NotApplied::NoTarget, 1)]);
    assert!(queued.accounts.is_empty(), "nothing for an op runner");
    assert_eq!(
        held_ids(&fx.store, fx.archive).values().next(),
        Some(&ids[0])
    );
    assert_eq!(shown(&fx.store, ids[0]), Some(Flags::SEEN));
    assert!(held_ids(&fx.store, fx.projects).is_empty());
    assert_eq!(fx.store.pending_ops(fx.account).recv_blocking().unwrap(), 0);
    clean(&fx.store);
    fx.store.close().unwrap();
}
