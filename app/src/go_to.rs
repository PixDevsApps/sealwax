//! Go to Inbox, Starred, Sent and Drafts (`g` then `i`, `s`, `t`, `d`) and Back to List
//! (Escape, `u` in the reader): the keymap's navigation commands, built in M9 (ML-092) with
//! the keys the registry already had. A go-to shows the account's folder of that role: the
//! account the list shows, in All Inboxes the selected message's, else the first account's;
//! Go to Inbox from All Inboxes stays there, and Go to Starred turns on the Starred chip, as
//! there is no Starred folder. Back to List puts the keyboard focus in the list, showing it
//! first when one pane fits.

use crate::application::AppState;
use crate::compose_actions::compose_account;
use crate::mailbox::folder_key;
use crate::window::{ALL_INBOXES, Pane};
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::{Folder, FolderId, FolderRole};
use sealwax_infrastructure::store::{ViewFilter, ViewScope};
use std::rc::Rc;

/// The commands this module runs.
pub const COMMANDS: [ActionId; 5] = [
    ActionId::GoToInbox,
    ActionId::GoToStarred,
    ActionId::GoToSent,
    ActionId::GoToDrafts,
    ActionId::BackToList,
];

/// Where a go-to command leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    AllInboxes,
    Folder(FolderId),
}

/// The folder of `role` for the list showing `scope`, with `selected` the selected message's
/// folder: the shown account's, else the selected message's, else the first account's that
/// has one. Inbox from All Inboxes stays in All Inboxes.
pub fn destination(
    role: FolderRole,
    scope: Option<ViewScope>,
    selected: Option<FolderId>,
    folders: &[Folder],
) -> Option<Destination> {
    if role == FolderRole::Inbox && scope == Some(ViewScope::UnifiedInbox) {
        return Some(Destination::AllInboxes);
    }
    let account = compose_account(scope, selected, folders);
    let mut candidates: Vec<&Folder> = folders
        .iter()
        .filter(|folder| folder.role == role && folder.selectable)
        .collect();
    candidates.sort_by_key(|folder| (folder.account.0, folder.id.0));
    candidates
        .iter()
        .find(|folder| Some(folder.account) == account)
        .or_else(|| candidates.first())
        .map(|folder| Destination::Folder(folder.id))
}

/// Connects the five commands on the main window's action set.
pub fn install(state: &Rc<AppState>, actions: &crate::actions::ActionSet) {
    for (id, role) in [
        (ActionId::GoToInbox, FolderRole::Inbox),
        (ActionId::GoToSent, FolderRole::Sent),
        (ActionId::GoToDrafts, FolderRole::Drafts),
    ] {
        let weak = Rc::downgrade(state);
        actions.connect(id, move || {
            if let Some(state) = weak.upgrade() {
                go_to(&state, role);
            }
        });
    }
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::GoToStarred, move || {
        if let Some(state) = weak.upgrade()
            && let (Some(window), Some(list)) = (state.window(), state.list())
        {
            list.set_filter(ViewFilter::Starred);
            window.show_pane(Pane::List);
            list.list_view().grab_focus();
        }
    });
    let weak = Rc::downgrade(state);
    actions.connect(ActionId::BackToList, move || {
        if let Some(state) = weak.upgrade() {
            back_to_list(&state);
        }
    });
}

fn go_to(state: &Rc<AppState>, role: FolderRole) {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    let selected = list
        .selected_targets()
        .ok()
        .and_then(|targets| targets.first().map(|target| target.folder));
    let key = match destination(role, list.scope(), selected, &state.mailbox().folders()) {
        Some(Destination::AllInboxes) => ALL_INBOXES.to_owned(),
        Some(Destination::Folder(folder)) => folder_key(folder),
        None => return,
    };
    // The folder itself, as picking it in the sidebar shows it: not only its starred mail.
    list.set_filter(ViewFilter::All);
    window.select_folder(&key);
    list.list_view().grab_focus();
}

/// Leaves the reader for the list: the select mode ends as Escape ends it there, and the
/// list, shown first when one pane fits, takes the keyboard focus.
fn back_to_list(state: &Rc<AppState>) {
    let (Some(window), Some(list)) = (state.window(), state.list()) else {
        return;
    };
    if list.is_select_mode() {
        list.leave_select_mode();
    }
    window.show_pane(Pane::List);
    list.list_view().grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::AccountId;

    fn folder(id: i64, account: i64, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: format!("folder {id}"),
            role,
            selectable: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn go_to_leads_to_the_shown_accounts_folder() {
        let folders = [
            folder(1, 1, FolderRole::Inbox),
            folder(2, 1, FolderRole::Sent),
            folder(3, 2, FolderRole::Inbox),
            folder(4, 2, FolderRole::Sent),
            folder(5, 2, FolderRole::Drafts),
        ];
        let second = Some(ViewScope::Folder(FolderId(3)));
        assert_eq!(
            destination(FolderRole::Sent, second, None, &folders),
            Some(Destination::Folder(FolderId(4)))
        );
        assert_eq!(
            destination(
                FolderRole::Inbox,
                Some(ViewScope::Folder(FolderId(4))),
                None,
                &folders
            ),
            Some(Destination::Folder(FolderId(3)))
        );
        // The first account has no Drafts: the first account that has one.
        let first = Some(ViewScope::Folder(FolderId(1)));
        assert_eq!(
            destination(FolderRole::Drafts, first, None, &folders),
            Some(Destination::Folder(FolderId(5)))
        );
        let all = Some(ViewScope::UnifiedInbox);
        assert_eq!(
            destination(FolderRole::Inbox, all, Some(FolderId(3)), &folders),
            Some(Destination::AllInboxes)
        );
        assert_eq!(
            destination(FolderRole::Sent, all, Some(FolderId(3)), &folders),
            Some(Destination::Folder(FolderId(4)))
        );
        assert_eq!(
            destination(FolderRole::Sent, all, None, &folders),
            Some(Destination::Folder(FolderId(2)))
        );
        assert_eq!(destination(FolderRole::Junk, first, None, &folders), None);
    }
}
