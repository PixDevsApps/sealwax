//! Which message bodies Sealwax downloads (ML-044). A message up to [`WHOLE_LIMIT`] is fetched
//! whole; a larger one as its text parts, its attachments left on the server until asked for.
//! After a sync, recent Inbox messages are downloaded in the background ([`prefetch`]) within
//! part of the disk cap, so they open without waiting. Pure rules: the engine fetches, the
//! store keeps and evicts.

use crate::config::Storage;
use crate::model::{FolderRole, MessageId};

const MIB: u64 = 1024 * 1024;
const DAY: i64 = 86_400;

/// Messages up to this size (RFC822.SIZE) are downloaded whole.
pub const WHOLE_LIMIT: u64 = 10 * MIB;

/// Background downloads may fill at most this share of the disk cap, in percent, so the bodies
/// the user opened aren't pushed out by ones nobody has read.
pub const PREFETCH_SHARE: u64 = 50;

/// What the background prefetch downloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefetch {
    /// Inbox messages that arrived within this many days; 0 turns prefetching off.
    pub days: u32,
    /// Larger messages wait until they are opened.
    pub max_size: u64,
}

impl Prefetch {
    pub fn new(storage: &Storage) -> Self {
        Self {
            days: storage.prefetch_days,
            max_size: WHOLE_LIMIT,
        }
    }

    /// The oldest arrival time prefetched at `now`; `None` when prefetching is off.
    pub fn since(&self, now: i64) -> Option<i64> {
        (self.days > 0).then(|| now - i64::from(self.days) * DAY)
    }
}

/// A message the prefetch may download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub message: MessageId,
    pub role: FolderRole,
    /// When it arrived (Unix seconds).
    pub date: i64,
    /// RFC822.SIZE.
    pub size: u64,
    /// The store holds its body already.
    pub downloaded: bool,
}

/// What the downloaded bodies take and may take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUse {
    pub used: u64,
    /// The disk cap; `None` for none.
    pub limit: Option<u64>,
}

/// The messages to download in the background, newest first: Inbox messages from the policy's
/// window, each within its size, while the downloaded bodies stay within
/// [`PREFETCH_SHARE`] of the cap.
pub fn prefetch(
    policy: &Prefetch,
    now: i64,
    cache: CacheUse,
    candidates: &[Candidate],
) -> Vec<MessageId> {
    let Some(since) = policy.since(now) else {
        return Vec::new();
    };
    let mut wanted: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| {
            candidate.role == FolderRole::Inbox
                && !candidate.downloaded
                && candidate.date >= since
                && candidate.size <= policy.max_size
        })
        .collect();
    wanted.sort_by(|a, b| b.date.cmp(&a.date).then(b.message.cmp(&a.message)));
    let budget = cache
        .limit
        .map(|limit| (limit / 100 * PREFETCH_SHARE).saturating_sub(cache.used));
    let mut taken = 0u64;
    let mut out = Vec::new();
    for candidate in wanted {
        if budget.is_some_and(|budget| taken + candidate.size > budget) {
            break;
        }
        taken += candidate.size;
        out.push(candidate.message);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000;

    fn candidate(id: i64, days_ago: i64, size: u64) -> Candidate {
        Candidate {
            message: MessageId(id),
            role: FolderRole::Inbox,
            date: NOW - days_ago * DAY,
            size,
            downloaded: false,
        }
    }

    fn policy(days: u32) -> Prefetch {
        Prefetch::new(&Storage {
            prefetch_days: days,
            ..Storage::default()
        })
    }

    #[test]
    fn recent_inbox_messages_come_newest_first() {
        let candidates = [
            candidate(1, 20, 1000),
            candidate(2, 1, 1000),
            candidate(3, 13, 1000),
            Candidate {
                role: FolderRole::Sent,
                ..candidate(4, 0, 1000)
            },
            Candidate {
                downloaded: true,
                ..candidate(5, 0, 1000)
            },
            candidate(6, 0, WHOLE_LIMIT + 1),
            candidate(7, 0, WHOLE_LIMIT),
        ];
        let picked = prefetch(&policy(14), NOW, CacheUse::default(), &candidates);
        assert_eq!(picked, [MessageId(7), MessageId(2), MessageId(3)]);
    }

    #[test]
    fn zero_days_turns_it_off() {
        let candidates = [candidate(1, 0, 10)];
        assert!(prefetch(&policy(0), NOW, CacheUse::default(), &candidates).is_empty());
        assert_eq!(policy(0).since(NOW), None);
        assert_eq!(policy(2).since(NOW), Some(NOW - 2 * DAY));
    }

    #[test]
    fn prefetching_fills_at_most_half_the_cap() {
        let candidates: Vec<Candidate> = (1..=10).map(|n| candidate(n, n, 10 * MIB)).collect();
        let cache = CacheUse {
            used: 0,
            limit: Some(100 * MIB),
        };
        let picked = prefetch(&policy(30), NOW, cache, &candidates);
        assert_eq!(picked.len(), 5, "{picked:?}");
        assert_eq!(picked[0], MessageId(1));
        let fuller = CacheUse {
            used: 45 * MIB,
            ..cache
        };
        assert!(prefetch(&policy(30), NOW, fuller, &candidates).is_empty());
        let unlimited = CacheUse {
            used: 1 << 40,
            limit: None,
        };
        assert_eq!(prefetch(&policy(30), NOW, unlimited, &candidates).len(), 10);
    }
}
