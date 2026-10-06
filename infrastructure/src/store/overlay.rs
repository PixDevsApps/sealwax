//! The op queue's overlay (ADR-015): what the user sees of a message is the server's flags with
//! its pending ops applied, in FIFO order. A pending move or expunge takes the message out of its
//! folder until the server has done it; so does `\Deleted`, whoever set it. Pure functions, used
//! by the writer and by `check`.

use sealwax_domain::model::{Flags, FolderId};

/// Set in `messages.flags` while a pending move or expunge takes a message out of its folder.
/// It is outside `Flags`' bits, so no row ever shows it.
pub(crate) const HIDDEN: u32 = 256;
/// Rows with either bit are in no view and no count. The partial indexes spell the masks out:
/// 264 for shown rows, 265 for shown unread ones, 268 = 4 for shown starred ones.
pub(crate) const UNSHOWN: u32 = HIDDEN | Flags::DELETED.bits();
/// The unread count a folder shows (of `folders`): its shown unread rows, plus the server's
/// unread below its floor unless a pending "mark folder read" covers them.
pub(crate) const UNREAD_SQL: &str = "unread_count + CASE WHEN EXISTS (SELECT 1 FROM ops \
    WHERE ops.folder_id = folders.id AND ops.done = 0 AND ops.kind = 3) THEN 0 \
    ELSE below_unread END";

/// `kind` in the `ops` table.
pub(crate) const FLAGS: i64 = 0;
pub(crate) const MOVE: i64 = 1;
pub(crate) const EXPUNGE: i64 = 2;
/// A folder marked read: the server's unread messages below its floor (`uid` holds the floor).
pub(crate) const READ_BELOW: i64 = 3;

/// `kind` in the `op_batches` table.
pub(crate) const BATCH_TRIAGE: i64 = 0;
pub(crate) const BATCH_FOLDER_READ: i64 = 1;
pub(crate) const BATCH_UNDO: i64 = 2;

/// Whether a row with these flags is listed and counted.
pub(crate) fn shown(flags: u32) -> bool {
    flags & UNSHOWN == 0
}

/// What a row adds to its folder's total and unread counts.
pub(crate) fn counted(flags: u32) -> (i64, i64) {
    (
        i64::from(shown(flags)),
        i64::from(flags & (UNSHOWN | Flags::SEEN.bits()) == 0),
    )
}

/// One pending change of one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Flags { add: u32, remove: u32 },
    Move { to: FolderId, add: u32, remove: u32 },
    Expunge,
}

impl Change {
    /// The change an `ops` row describes; `None` for a folder's read-below op.
    pub(crate) fn from_row(kind: i64, target: Option<i64>, add: u32, remove: u32) -> Option<Self> {
        match kind {
            FLAGS => Some(Change::Flags { add, remove }),
            MOVE => Some(Change::Move {
                to: FolderId(target?),
                add,
                remove,
            }),
            EXPUNGE => Some(Change::Expunge),
            _ => None,
        }
    }
}

/// Where a run of changes leaves a message that starts in `from`, and the flags it adds and
/// removes overall (a later change wins).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Folded {
    pub(crate) from: FolderId,
    pub(crate) at: FolderId,
    pub(crate) add: u32,
    pub(crate) remove: u32,
    pub(crate) expunge: bool,
}

/// The server action a run of changes comes to, as the op runner performs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Action {
    Flags {
        add: u32,
        remove: u32,
    },
    /// `+FLAGS add` and `-FLAGS remove` where the message is, then a move to `to`.
    Move {
        to: FolderId,
        add: u32,
        remove: u32,
    },
    Expunge,
}

pub(crate) fn fold(from: FolderId, changes: impl IntoIterator<Item = Change>) -> Folded {
    let mut folded = Folded {
        from,
        at: from,
        add: 0,
        remove: 0,
        expunge: false,
    };
    for change in changes {
        let (add, remove) = match change {
            Change::Flags { add, remove } => (add, remove),
            Change::Move { to, add, remove } => {
                folded.at = to;
                (add, remove)
            }
            Change::Expunge => {
                folded.expunge = true;
                break;
            }
        };
        folded.add = (folded.add | add) & !remove;
        folded.remove = (folded.remove | remove) & !add;
    }
    folded
}

impl Folded {
    /// Whether the message ends up outside the folder it started in, or deleted.
    pub(crate) fn leaves(&self) -> bool {
        self.expunge || self.at != self.from
    }

    /// `flags` with the run's flag changes; other bits are kept.
    pub(crate) fn apply(&self, flags: u32) -> u32 {
        (flags | self.add) & !self.remove
    }

    /// The flags a message shows: `server` (its server flags, read below the floor when `read`)
    /// with the run applied, hidden when the run takes it away.
    pub(crate) fn shown(&self, server: u32, read: bool) -> u32 {
        let mut flags = server & !HIDDEN;
        if read {
            flags |= Flags::SEEN.bits();
        }
        flags = self.apply(flags);
        if self.leaves() {
            flags |= HIDDEN;
        }
        flags
    }

    /// What the server must do, or `None` when the run comes to nothing (A→B→A, or flags that
    /// cancel out).
    pub(crate) fn action(&self) -> Option<Action> {
        if self.expunge {
            Some(Action::Expunge)
        } else if self.at != self.from {
            Some(Action::Move {
                to: self.at,
                add: self.add,
                remove: self.remove,
            })
        } else if self.add != 0 || self.remove != 0 {
            Some(Action::Flags {
                add: self.add,
                remove: self.remove,
            })
        } else {
            None
        }
    }
}

/// The change that puts back what `change` did to a message whose flags were `prior` and which
/// was in `prior_folder`: the flags it set that weren't set before are removed, those it removed
/// that were set are added, and a move goes back. `None` for an expunge.
pub(crate) fn inverse(change: Change, prior: u32, prior_folder: FolderId) -> Option<Change> {
    let (add, remove) = match change {
        Change::Flags { add, remove } | Change::Move { add, remove, .. } => {
            (remove & prior, add & !prior)
        }
        Change::Expunge => return None,
    };
    Some(match change {
        Change::Move { .. } => Change::Move {
            to: prior_folder,
            add,
            remove,
        },
        _ => Change::Flags { add, remove },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: FolderId = FolderId(1);
    const B: FolderId = FolderId(2);
    const C: FolderId = FolderId(3);
    const SEEN: u32 = 1;
    const FLAGGED: u32 = 4;
    const JUNK: u32 = 64;
    const NOT_JUNK: u32 = 128;

    fn flags(add: u32, remove: u32) -> Change {
        Change::Flags { add, remove }
    }

    fn moved(to: FolderId, add: u32, remove: u32) -> Change {
        Change::Move { to, add, remove }
    }

    #[test]
    fn flag_changes_merge_and_the_later_one_wins() {
        let run = fold(A, [flags(SEEN, 0), flags(FLAGGED, 0), flags(0, SEEN)]);
        assert_eq!((run.add, run.remove), (FLAGGED, SEEN));
        assert_eq!(
            run.action(),
            Some(Action::Flags {
                add: FLAGGED,
                remove: SEEN
            })
        );
        assert_eq!(run.shown(SEEN, false), FLAGGED);
        assert!(!run.leaves());
    }

    #[test]
    fn move_chains_collapse() {
        let run = fold(A, [moved(B, 0, 0), moved(C, 0, 0)]);
        assert_eq!(
            run.action(),
            Some(Action::Move {
                to: C,
                add: 0,
                remove: 0
            })
        );
        assert_eq!(run.shown(0, false), HIDDEN);
        let back = fold(A, [moved(B, 0, 0), moved(A, 0, 0)]);
        assert_eq!(back.action(), None, "A→B→A is nothing");
        assert_eq!(back.shown(SEEN, false), SEEN);
        let junk = fold(A, [moved(B, JUNK, NOT_JUNK), moved(A, NOT_JUNK, JUNK)]);
        assert_eq!(
            junk.action(),
            Some(Action::Flags {
                add: NOT_JUNK,
                remove: JUNK
            }),
            "junk and back leaves the keywords of the last"
        );
    }

    #[test]
    fn flags_before_a_move_travel_with_it_and_an_expunge_ends_the_run() {
        let run = fold(A, [flags(SEEN, 0), moved(B, 0, 0)]);
        assert_eq!(
            run.action(),
            Some(Action::Move {
                to: B,
                add: SEEN,
                remove: 0
            })
        );
        let gone = fold(A, [moved(B, 0, 0), Change::Expunge, flags(SEEN, 0)]);
        assert_eq!(gone.action(), Some(Action::Expunge));
        assert_eq!(gone.add, 0, "nothing after the expunge counts");
    }

    #[test]
    fn hidden_and_deleted_rows_are_not_counted() {
        assert_eq!(counted(0), (1, 1));
        assert_eq!(counted(SEEN), (1, 0));
        assert_eq!(counted(HIDDEN), (0, 0));
        assert_eq!(counted(Flags::DELETED.bits()), (0, 0));
        assert_eq!(
            fold(A, []).shown(HIDDEN | SEEN, true),
            SEEN,
            "the bit is never the server's"
        );
        assert_eq!(fold(A, []).shown(0, true), SEEN, "read below the floor");
    }

    #[test]
    fn inverses_put_back_only_what_changed() {
        assert_eq!(inverse(flags(SEEN, 0), 0, A), Some(flags(0, SEEN)));
        assert_eq!(inverse(flags(SEEN, 0), SEEN, A), Some(flags(0, 0)));
        assert_eq!(
            inverse(moved(B, JUNK, NOT_JUNK), NOT_JUNK, A),
            Some(moved(A, NOT_JUNK, JUNK))
        );
        assert_eq!(inverse(Change::Expunge, 0, A), None);
    }
}
