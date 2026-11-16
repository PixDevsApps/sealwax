//! The unread count for other programs (1.5.0, issue #17; `sealwax_infrastructure::unread`):
//! whenever the folders' counts change, All Inboxes' number and each account's go to
//! `unread.json` in Sealwax's runtime folder, off the GTK thread and only when they changed; the
//! file goes when Sealwax quits. This runs with or without a window (background mode).

use crate::application::AppState;
use crate::window::SidebarModel;
use gtk4::{gio, glib};
use sealwax_domain::address;
use sealwax_infrastructure::unread::{self, UnreadCount};
use std::cell::RefCell;
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

/// The counts as the sidebar shows them; an account without a label of its own is "Account 2",
/// not its address.
pub(crate) fn count(model: &SidebarModel, addresses: &[String]) -> UnreadCount {
    let accounts = model
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
            (name, account.inbox().map_or(0, |inbox| inbox.count))
        })
        .collect();
    UnreadCount {
        total: model.all_inboxes_unread(),
        accounts,
    }
}

fn update(state: &AppState, path: PathBuf) {
    let mailbox = state.mailbox();
    let addresses: Vec<String> = mailbox
        .accounts()
        .into_iter()
        .map(|account| account.address)
        .collect();
    let count = count(&mailbox.sidebar_model(), &addresses);
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
    use crate::window::{AccountStatus, FolderRole, SidebarAccount, SidebarFolder};

    fn account(name: &str, inbox: u32) -> SidebarAccount {
        SidebarAccount {
            key: name.to_owned(),
            name: name.to_owned(),
            color: None,
            collapsed: false,
            more_open: None,
            status: AccountStatus::Synced,
            folders: vec![SidebarFolder {
                key: format!("{name}-inbox"),
                name: "INBOX".to_owned(),
                role: Some(FolderRole::Inbox),
                count: inbox,
                children: Vec::new(),
                folded: false,
            }],
            more: Vec::new(),
        }
    }

    #[test]
    fn the_count_is_all_inboxes_and_never_names_an_address() {
        let model = SidebarModel {
            accounts: vec![account("Jobb", 9), account("Kari@Example.test", 3)],
            preferred: None,
        };
        let count = count(&model, &["kari@example.test".to_owned()]);
        assert_eq!(
            count,
            UnreadCount {
                total: 12,
                accounts: vec![("Jobb".to_owned(), 9), ("Account 2".to_owned(), 3)],
            }
        );
    }
}
