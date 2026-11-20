//! The unread count for other programs (1.5.0, issue #17; `sealwax_infrastructure::unread`):
//! whenever the folders' counts change, the unread mail in each account's Inbox, the folders
//! under it and the user's own folders goes to `unread.json` in Sealwax's runtime folder, with
//! each of those folders that has some, off the GTK thread and only when it changed; the file
//! goes when Sealwax quits. This runs with or without a window (background mode).

use crate::application::AppState;
use crate::mailbox::folder_key;
use crate::window::{FolderRole, SidebarFolder, SidebarModel};
use gtk4::{gio, glib};
use sealwax_domain::address;
use sealwax_domain::model::FolderRole as StoreRole;
use sealwax_infrastructure::unread::{self, AccountUnread, FolderUnread, UnreadCount};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

thread_local! {
    /// What the file holds now, so an unchanged count isn't written again.
    static WRITTEN: RefCell<Option<UnreadCount>> = const { RefCell::new(None) };
}

/// Keeps the file up to date from now on.
pub(crate) fn install(state: &Rc<AppState>) {
    let Some(path) = state.config().dirs.unread_file() else {
        tracing::debug!("no runtime folder; the unread count isn't shared");
        return;
    };
    let weak = Rc::downgrade(state);
    state.mailbox().connect_folders_changed(move || {
        if let Some(state) = weak.upgrade() {
            update(&state, path.clone());
        }
    });
}

/// The counts for the file (1.6.3, ADR-029 amendment): an account's Inbox and every folder under
/// it, and its own folders (the sidebar's "More" and what is under them), each with its unread
/// mail; not Sent, Drafts, Archive, Junk, Trash or the folders under them, whose unread mail
/// isn't waiting to be read, nor the folders in `skip` (`\All`, which holds the others' mail
/// again). An account without a label of its own is "Account 2", not its address.
pub(crate) fn count(
    model: &SidebarModel,
    addresses: &[String],
    skip: &HashSet<String>,
) -> UnreadCount {
    let accounts: Vec<AccountUnread> = model
        .accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            let name = if addresses
                .iter()
                .any(|known| address::same(known, &account.name))
            {
                format!("Account {}", index + 1)
            } else {
                account.name.clone()
            };
            let mut folders = Vec::new();
            let counted = account
                .folders
                .iter()
                .filter(|folder| folder.role == Some(FolderRole::Inbox))
                .chain(&account.more);
            for folder in counted {
                collect(folder, "", skip, &mut folders);
            }
            AccountUnread {
                name,
                unread: folders
                    .iter()
                    .fold(0, |sum: u32, folder| sum.saturating_add(folder.unread)),
                folders,
            }
        })
        .collect();
    UnreadCount {
        total: accounts
            .iter()
            .fold(0, |sum: u32, account| sum.saturating_add(account.unread)),
        accounts,
    }
}

/// `folder` and the folders under it, depth first, those with unread mail into `out`.
fn collect(
    folder: &SidebarFolder,
    parent: &str,
    skip: &HashSet<String>,
    out: &mut Vec<FolderUnread>,
) {
    if skip.contains(&folder.key) {
        return;
    }
    if folder.count > 0 {
        out.push(FolderUnread {
            key: folder.key.clone(),
            name: folder.title().to_owned(),
            parent: parent.to_owned(),
            unread: folder.count,
        });
    }
    let path = if parent.is_empty() {
        folder.title().to_owned()
    } else {
        format!("{parent}/{}", folder.title())
    };
    for child in &folder.children {
        collect(child, &path, skip, out);
    }
}

fn update(state: &AppState, path: PathBuf) {
    let mailbox = state.mailbox();
    let addresses: Vec<String> = mailbox
        .accounts()
        .into_iter()
        .map(|account| account.address)
        .collect();
    let skip: HashSet<String> = mailbox
        .folders()
        .into_iter()
        .filter(|folder| folder.role == StoreRole::All)
        .map(|folder| folder_key(folder.id))
        .collect();
    let count = count(&mailbox.sidebar_model(), &addresses, &skip);
    let changed = WRITTEN.with(|written| {
        let mut written = written.borrow_mut();
        (written.as_ref() != Some(&count)).then(|| written.replace(count.clone()))
    });
    if changed.is_none() {
        return;
    }
    glib::spawn_future_local(async move {
        let pid = std::process::id();
        let written = gio::spawn_blocking(move || unread::write(&path, &count, pid)).await;
        if let Ok(Err(error)) = written {
            tracing::warn!(%error, "the unread count can't be shared");
            WRITTEN.with(|written| written.replace(None));
        }
    });
}

/// Removes the file as Sealwax quits.
pub(crate) fn clear(state: &AppState) {
    if let Some(path) = state.config().dirs.unread_file()
        && WRITTEN
            .with(|written| written.borrow_mut().take())
            .is_some()
        && let Err(error) = unread::remove(&path)
    {
        tracing::warn!(%error, "the unread count's file can't be removed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{AccountStatus, SidebarAccount};

    fn folder(key: &str, name: &str, role: Option<FolderRole>, count: u32) -> SidebarFolder {
        SidebarFolder {
            key: key.to_owned(),
            name: name.to_owned(),
            role,
            count,
            children: Vec::new(),
            folded: false,
        }
    }

    fn under(mut parent: SidebarFolder, children: Vec<SidebarFolder>) -> SidebarFolder {
        parent.children = children;
        parent
    }

    fn account(
        name: &str,
        folders: Vec<SidebarFolder>,
        more: Vec<SidebarFolder>,
    ) -> SidebarAccount {
        SidebarAccount {
            key: name.to_owned(),
            name: name.to_owned(),
            color: None,
            collapsed: false,
            more_open: None,
            status: AccountStatus::Synced,
            folders,
            more,
        }
    }

    fn unread(key: &str, name: &str, parent: &str, unread: u32) -> FolderUnread {
        FolderUnread {
            key: key.to_owned(),
            name: name.to_owned(),
            parent: parent.to_owned(),
            unread,
        }
    }

    #[test]
    fn the_inbox_its_subfolders_and_own_folders_count_by_folder() {
        // The owner's sidebar (2026-11-21): 1 in the Inbox and 1 each in three of its subfolders,
        // which the bar didn't count.
        let inbox = under(
            folder("j-inbox", "INBOX", Some(FolderRole::Inbox), 1),
            vec![
                folder("j-ai", "AI", None, 0),
                folder("j-bank", "Bank", None, 1),
                under(
                    folder("j-foto", "Foto", None, 1),
                    vec![folder("j-2026", "2026", None, 4)],
                ),
                folder("j-handel", "Netthandel", None, 1),
            ],
        );
        let jobb = account(
            "Jobb",
            vec![
                inbox,
                folder("j-drafts", "Drafts", Some(FolderRole::Drafts), 2),
                folder("j-sent", "Sent", Some(FolderRole::Sent), 1),
                under(
                    folder("j-archive", "Archive", Some(FolderRole::Archive), 5),
                    vec![folder("j-old", "2024", None, 3)],
                ),
                folder("j-junk", "Junk", Some(FolderRole::Junk), 6),
                under(
                    folder("j-trash", "Trash", Some(FolderRole::Trash), 7),
                    vec![folder("j-gone", "Gamle", None, 1)],
                ),
            ],
            vec![
                under(
                    folder("j-kunder", "Kunder", None, 2),
                    vec![folder("j-acme", "Acme", None, 1)],
                ),
                folder("j-all", "[Gmail]/All Mail", None, 40),
            ],
        );
        let other = account(
            "Kari@Example.test",
            vec![folder("k-inbox", "INBOX", Some(FolderRole::Inbox), 3)],
            Vec::new(),
        );
        let model = SidebarModel {
            accounts: vec![jobb, other],
            preferred: None,
        };
        let skip = HashSet::from(["j-all".to_owned()]);
        let count = count(&model, &["kari@example.test".to_owned()], &skip);
        assert_eq!(
            count,
            UnreadCount {
                total: 14,
                accounts: vec![
                    AccountUnread {
                        name: "Jobb".to_owned(),
                        unread: 11,
                        folders: vec![
                            unread("j-inbox", "Inbox", "", 1),
                            unread("j-bank", "Bank", "Inbox", 1),
                            unread("j-foto", "Foto", "Inbox", 1),
                            unread("j-2026", "2026", "Inbox/Foto", 4),
                            unread("j-handel", "Netthandel", "Inbox", 1),
                            unread("j-kunder", "Kunder", "", 2),
                            unread("j-acme", "Acme", "Kunder", 1),
                        ],
                    },
                    AccountUnread {
                        name: "Account 2".to_owned(),
                        unread: 3,
                        folders: vec![unread("k-inbox", "Inbox", "", 3)],
                    },
                ],
            }
        );
    }

    #[test]
    fn an_account_without_unread_mail_lists_no_folder() {
        let model = SidebarModel {
            accounts: vec![account(
                "Home",
                vec![folder("h-inbox", "INBOX", Some(FolderRole::Inbox), 0)],
                vec![folder("h-kvitteringer", "Kvitteringer", None, 0)],
            )],
            preferred: None,
        };
        let count = count(&model, &[], &HashSet::new());
        assert_eq!(count.total, 0);
        assert_eq!(count.accounts[0].unread, 0);
        assert!(count.accounts[0].folders.is_empty());
    }
}
