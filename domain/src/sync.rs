//! Sync policy (ML-041, ML-043): which folders the engine keeps current by itself, and how far
//! back a folder's first sync reaches. Pure functions; the engine applies them.

use crate::model::FolderRole;

/// What the account pass does with a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Kept current: first sync, then a pass whenever STATUS shows a change.
    Sync,
    /// Only the server's STATUS counts, until the user opens it.
    Counts,
    /// Nothing: the folder can't hold messages.
    Skip,
}

/// What the policy knows about a folder.
#[derive(Clone, Copy, Debug)]
pub struct Facts<'a> {
    pub role: FolderRole,
    /// LIST attributes as the server sent them.
    pub attributes: &'a [String],
    pub selectable: bool,
    /// The folder was synced before (it has a UIDVALIDITY), for example because the user opened
    /// it.
    pub synced_before: bool,
}

/// The roles that sync without being opened.
pub const SYNCED_ROLES: [FolderRole; 6] = [
    FolderRole::Inbox,
    FolderRole::Sent,
    FolderRole::Drafts,
    FolderRole::Archive,
    FolderRole::Junk,
    FolderRole::Trash,
];

/// INBOX and the role folders sync by themselves, and so does every folder synced before
/// (opened by the user once). Other folders show their STATUS counts until opened. A folder with
/// `\All` (Gmail's All Mail, which holds every other folder's messages again) never syncs by
/// itself, even after it was opened.
pub fn plan(facts: Facts<'_>) -> Plan {
    let all = facts.role == FolderRole::All
        || facts
            .attributes
            .iter()
            .any(|attribute| attribute.eq_ignore_ascii_case("\\All"));
    if !facts.selectable {
        Plan::Skip
    } else if all {
        Plan::Counts
    } else if SYNCED_ROLES.contains(&facts.role) || facts.synced_before {
        Plan::Sync
    } else {
        Plan::Counts
    }
}

/// How far back a folder's first sync reaches (ML-043). The first sync keeps the newest
/// messages, UID-contiguous from a floor up: everything that arrived within `days`, capped at
/// `max` messages, and at least `min` however old, so a folder with little recent mail isn't
/// empty. Older mail loads on request, `older` messages at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub days: u32,
    pub max: u32,
    pub min: u32,
}

impl Window {
    /// The proposal: 90 days, at most 5,000 and at least 200 messages per folder.
    pub const DEFAULT: Window = Window {
        days: 90,
        max: 5_000,
        min: 200,
    };

    /// No limit: the whole folder.
    pub const ALL: Window = Window {
        days: u32::MAX,
        max: u32::MAX,
        min: u32::MAX,
    };

    /// Keeps every one of `count` messages whatever their age.
    pub fn keeps_all(self, count: usize) -> bool {
        self.min as usize >= count
    }

    /// The oldest UID worth asking SEARCH SINCE about: the `max`-th newest.
    pub fn cap(self, uids: &[u32]) -> Option<u32> {
        let keep = (self.max as usize).min(uids.len());
        uids.len()
            .checked_sub(keep)
            .and_then(|first| uids.get(first))
            .copied()
    }

    /// Where the window starts (Unix seconds), counted back from `now`.
    pub fn since(self, now: i64) -> i64 {
        now.saturating_sub(i64::from(self.days).saturating_mul(86_400))
    }

    /// The lowest UID to keep. `uids` are the folder's UIDs below `top` in ascending order, and
    /// `first_recent` the lowest of them that arrived within `days`. Everything from it up is
    /// kept, then the count is held between `min` and `max`. An empty folder's floor is `top`.
    pub fn floor(self, uids: &[u32], first_recent: Option<u32>, top: u32) -> u32 {
        let total = uids.len();
        let recent =
            first_recent.map_or(0, |first| total - uids.partition_point(|&uid| uid < first));
        let keep = recent
            .min(self.max as usize)
            .max(self.min as usize)
            .min(total);
        match keep {
            0 => top,
            keep => uids[total - keep],
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Window::DEFAULT
    }
}

/// Older messages fetched per `LoadOlder` (the proposal).
pub const OLDER_BATCH: u32 = 200;

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(role: FolderRole, attributes: &[String], synced_before: bool) -> Facts<'_> {
        Facts {
            role,
            attributes,
            selectable: true,
            synced_before,
        }
    }

    #[test]
    fn role_folders_and_folders_synced_before_sync_by_themselves() {
        for role in SYNCED_ROLES {
            assert_eq!(plan(facts(role, &[], false)), Plan::Sync, "{role:?}");
        }
        assert_eq!(plan(facts(FolderRole::Other, &[], false)), Plan::Counts);
        assert_eq!(plan(facts(FolderRole::Other, &[], true)), Plan::Sync);
        let flagged = ["\\Flagged".to_owned()];
        assert_eq!(
            plan(facts(FolderRole::Other, &flagged, false)),
            Plan::Counts
        );
    }

    #[test]
    fn all_mail_never_syncs_by_itself_and_containers_never() {
        assert_eq!(plan(facts(FolderRole::All, &[], true)), Plan::Counts);
        let all = ["\\HasNoChildren".to_owned(), "\\ALL".to_owned()];
        assert_eq!(plan(facts(FolderRole::Archive, &all, true)), Plan::Counts);
        let container = Facts {
            selectable: false,
            ..facts(FolderRole::Inbox, &[], true)
        };
        assert_eq!(plan(container), Plan::Skip);
    }

    #[test]
    fn the_window_keeps_recent_mail_between_min_and_max() {
        let window = Window {
            days: 90,
            max: 5,
            min: 2,
        };
        let uids = [3, 4, 7, 8, 9, 12, 15, 20];
        assert_eq!(window.floor(&uids, Some(8), 21), 8, "recent mail: 8 and up");
        assert_eq!(
            window.floor(&uids, Some(4), 21),
            8,
            "capped at the 5 newest"
        );
        assert_eq!(window.floor(&uids, Some(20), 21), 15, "at least 2");
        assert_eq!(
            window.floor(&uids, None, 21),
            15,
            "nothing recent: the 2 newest"
        );
        assert_eq!(
            window.floor(&uids, Some(10), 21),
            12,
            "UID-contiguous from the first"
        );
        assert_eq!(window.floor(&[], None, 21), 21, "an empty folder");
        assert_eq!(window.floor(&[5], None, 6), 5);
        assert_eq!(window.cap(&uids), Some(8));
        assert_eq!(window.cap(&[]), None);
    }

    #[test]
    fn the_whole_folder_and_the_default() {
        let uids: Vec<u32> = (1..=10_000).collect();
        assert!(Window::ALL.keeps_all(uids.len()));
        assert_eq!(Window::ALL.floor(&uids, None, 10_001), 1);
        assert!(Window::ALL.since(1_790_000_000) < 0);
        assert!(!Window::DEFAULT.keeps_all(uids.len()));
        assert!(Window::DEFAULT.keeps_all(200));
        assert_eq!(Window::DEFAULT.floor(&uids, Some(1), 10_001), 5_001);
        assert_eq!(Window::DEFAULT.floor(&uids, None, 10_001), 9_801);
        assert_eq!(Window::DEFAULT.since(90 * 86_400 + 5), 5);
        assert_eq!(Window::default(), Window::DEFAULT);
    }
}
