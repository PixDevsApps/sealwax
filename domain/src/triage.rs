//! What each mail action means (ML-052, ADR-015): the change an action makes to messages of one
//! folder, from the roles of the account's folders. The app plans with it and enables its
//! commands by it, the store queues the plan with its overlay, and the engine runs it. No
//! provider names: an account without an Archive folder archives to its `\All` folder, which is
//! how archiving works where folders are labels.

use crate::model::{Flags, FolderId, FolderRole};

/// A mail action on selected messages, as the user asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Triage {
    /// Mark as read (`true`) or unread.
    Read(bool),
    /// Star (`\Flagged`) or unstar.
    Star(bool),
    Archive,
    Delete,
    /// Move to Junk; in Junk, back to the Inbox as not junk.
    Junk,
    /// Move to another folder of the same account.
    MoveTo(FolderId),
}

/// One of an account's folders, as planning needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderInfo {
    pub id: FolderId,
    pub role: FolderRole,
    /// `\Noselect` folders hold no messages and take none.
    pub selectable: bool,
}

/// What an action does to messages that share a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Add and remove flags in place (`+FLAGS` and `-FLAGS`, never a full replace).
    Flags { add: Flags, remove: Flags },
    /// Set `add` and clear `remove` where the server keeps them, then move to `to`.
    Move {
        to: FolderId,
        add: Flags,
        remove: Flags,
    },
    /// Delete for good: `\Deleted`, then `UID EXPUNGE` where the server has UIDPLUS; otherwise
    /// the message stays flagged `\Deleted` and hidden (ADR-015, S1 amendment).
    Expunge,
}

impl Plan {
    /// Whether the messages leave the folder they are in.
    pub fn leaves_folder(self) -> bool {
        matches!(self, Plan::Move { .. } | Plan::Expunge)
    }
}

/// Why an action doesn't apply to messages in a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The account has no folder for it (no Archive or `\All` folder, no Junk folder), or the
    /// folder named is not one of the account's folders that take messages.
    NoTarget,
    /// The messages are already where the action would put them.
    AlreadyThere,
}

/// The change `action` makes to messages in `source`, one of `folders` (the account's).
pub fn plan(
    action: Triage,
    source: FolderInfo,
    folders: &[FolderInfo],
) -> Result<Plan, Unavailable> {
    let target = |role: FolderRole| {
        folders
            .iter()
            .find(|folder| folder.role == role && folder.selectable)
    };
    let to = |folder: &FolderInfo, add: Flags, remove: Flags| {
        if folder.id == source.id {
            Err(Unavailable::AlreadyThere)
        } else {
            Ok(Plan::Move {
                to: folder.id,
                add,
                remove,
            })
        }
    };
    match action {
        Triage::Read(true) => Ok(flags(Flags::SEEN, Flags::NONE)),
        Triage::Read(false) => Ok(flags(Flags::NONE, Flags::SEEN)),
        Triage::Star(true) => Ok(flags(Flags::FLAGGED, Flags::NONE)),
        Triage::Star(false) => Ok(flags(Flags::NONE, Flags::FLAGGED)),
        Triage::Archive => {
            if source.role == FolderRole::Archive {
                return Err(Unavailable::AlreadyThere);
            }
            let archive = target(FolderRole::Archive)
                .or_else(|| target(FolderRole::All))
                .ok_or(Unavailable::NoTarget)?;
            to(archive, Flags::NONE, Flags::NONE)
        }
        Triage::Delete => match target(FolderRole::Trash) {
            _ if source.role == FolderRole::Trash => Ok(Plan::Expunge),
            Some(trash) => to(trash, Flags::NONE, Flags::NONE),
            None => Ok(Plan::Expunge),
        },
        Triage::Junk if source.role == FolderRole::Junk => {
            let inbox = target(FolderRole::Inbox).ok_or(Unavailable::NoTarget)?;
            to(inbox, Flags::NOT_JUNK, Flags::JUNK)
        }
        Triage::Junk => {
            let junk = target(FolderRole::Junk).ok_or(Unavailable::NoTarget)?;
            to(junk, Flags::JUNK, Flags::NOT_JUNK)
        }
        Triage::MoveTo(id) => {
            let folder = folders
                .iter()
                .find(|folder| folder.id == id && folder.selectable)
                .ok_or(Unavailable::NoTarget)?;
            to(folder, Flags::NONE, Flags::NONE)
        }
    }
}

fn flags(add: Flags, remove: Flags) -> Plan {
    Plan::Flags { add, remove }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INBOX: FolderInfo = folder(1, FolderRole::Inbox);
    const ARCHIVE: FolderInfo = folder(2, FolderRole::Archive);
    const TRASH: FolderInfo = folder(3, FolderRole::Trash);
    const JUNK: FolderInfo = folder(4, FolderRole::Junk);
    const ALL: FolderInfo = folder(5, FolderRole::All);
    const PROJECTS: FolderInfo = folder(6, FolderRole::Other);
    const CONTAINER: FolderInfo = FolderInfo {
        selectable: false,
        ..folder(7, FolderRole::Other)
    };

    const fn folder(id: i64, role: FolderRole) -> FolderInfo {
        FolderInfo {
            id: FolderId(id),
            role,
            selectable: true,
        }
    }

    fn moved(to: FolderInfo) -> Result<Plan, Unavailable> {
        Ok(Plan::Move {
            to: to.id,
            add: Flags::NONE,
            remove: Flags::NONE,
        })
    }

    #[test]
    fn flags_change_in_place_in_any_folder() {
        let all = [INBOX, ARCHIVE, TRASH, JUNK, PROJECTS];
        for source in all {
            assert_eq!(
                plan(Triage::Read(true), source, &all),
                Ok(flags(Flags::SEEN, Flags::NONE))
            );
            assert_eq!(
                plan(Triage::Read(false), source, &all),
                Ok(flags(Flags::NONE, Flags::SEEN))
            );
            assert_eq!(
                plan(Triage::Star(true), source, &all),
                Ok(flags(Flags::FLAGGED, Flags::NONE))
            );
            assert_eq!(
                plan(Triage::Star(false), source, &all),
                Ok(flags(Flags::NONE, Flags::FLAGGED))
            );
        }
    }

    #[test]
    fn archive_goes_to_the_archive_folder_else_to_all_mail() {
        let both = [INBOX, ARCHIVE, ALL, TRASH];
        assert_eq!(plan(Triage::Archive, INBOX, &both), moved(ARCHIVE));
        assert_eq!(plan(Triage::Archive, TRASH, &both), moved(ARCHIVE));
        assert_eq!(
            plan(Triage::Archive, ARCHIVE, &both),
            Err(Unavailable::AlreadyThere)
        );
        let labels = [INBOX, ALL, TRASH, JUNK];
        assert_eq!(plan(Triage::Archive, INBOX, &labels), moved(ALL));
        assert_eq!(
            plan(Triage::Archive, ALL, &labels),
            Err(Unavailable::AlreadyThere)
        );
        assert_eq!(
            plan(Triage::Archive, INBOX, &[INBOX, TRASH]),
            Err(Unavailable::NoTarget)
        );
    }

    #[test]
    fn delete_goes_to_trash_and_from_trash_for_good() {
        let all = [INBOX, TRASH, JUNK, ALL];
        assert_eq!(plan(Triage::Delete, INBOX, &all), moved(TRASH));
        assert_eq!(plan(Triage::Delete, JUNK, &all), moved(TRASH));
        assert_eq!(plan(Triage::Delete, ALL, &all), moved(TRASH));
        assert_eq!(plan(Triage::Delete, TRASH, &all), Ok(Plan::Expunge));
        assert_eq!(plan(Triage::Delete, INBOX, &[INBOX]), Ok(Plan::Expunge));
    }

    #[test]
    fn junk_and_not_junk_set_the_keywords() {
        let all = [INBOX, JUNK, PROJECTS];
        assert_eq!(
            plan(Triage::Junk, PROJECTS, &all),
            Ok(Plan::Move {
                to: JUNK.id,
                add: Flags::JUNK,
                remove: Flags::NOT_JUNK,
            })
        );
        assert_eq!(
            plan(Triage::Junk, JUNK, &all),
            Ok(Plan::Move {
                to: INBOX.id,
                add: Flags::NOT_JUNK,
                remove: Flags::JUNK,
            })
        );
        assert_eq!(
            plan(Triage::Junk, INBOX, &[INBOX]),
            Err(Unavailable::NoTarget)
        );
    }

    #[test]
    fn move_to_takes_only_another_selectable_folder_of_the_account() {
        let all = [INBOX, PROJECTS, CONTAINER];
        assert_eq!(
            plan(Triage::MoveTo(PROJECTS.id), INBOX, &all),
            moved(PROJECTS)
        );
        assert_eq!(
            plan(Triage::MoveTo(INBOX.id), INBOX, &all),
            Err(Unavailable::AlreadyThere)
        );
        assert_eq!(
            plan(Triage::MoveTo(CONTAINER.id), INBOX, &all),
            Err(Unavailable::NoTarget)
        );
        assert_eq!(
            plan(Triage::MoveTo(FolderId(99)), INBOX, &all),
            Err(Unavailable::NoTarget)
        );
    }

    #[test]
    fn a_container_never_receives_role_moves() {
        let trash = FolderInfo {
            selectable: false,
            ..TRASH
        };
        assert_eq!(
            plan(Triage::Delete, INBOX, &[INBOX, trash]),
            Ok(Plan::Expunge)
        );
    }

    #[test]
    fn only_moves_and_expunges_leave_the_folder() {
        assert!(Plan::Expunge.leaves_folder());
        assert!(moved(TRASH).unwrap().leaves_folder());
        assert!(!flags(Flags::SEEN, Flags::NONE).leaves_folder());
    }
}
