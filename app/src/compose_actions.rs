//! The main window's compose commands (M6: ML-068, ML-069, ML-074): Compose, Reply, Reply All,
//! Forward and Edit Draft from the list, the reader, the palette and their keys, and what the
//! reader asks for itself (the cards' buttons, the quick reply's expansion). Every one opens a
//! window through `composer::open`. The quick reply's Send goes through [`quick_send`], the seam
//! phase B wires to the outbox.
//!
//! - **Compose** writes from the account the list shows; in All Inboxes from the selected
//!   message's account, else the first account (`ComposeRequest::New { account: None }`).
//! - **Reply, Reply All and Forward** act on one message: in the list the selected one; once the
//!   reader shows its conversation, the message the user is on there
//!   ([`Reader::current_message`]). They don't apply to several messages, nor to a draft.
//! - **Edit Draft** opens the selected message of a Drafts folder.

use crate::actions::ActionSet;
use crate::application::{AppState, SealwaxApplication};
use crate::composer::{self, ComposeRequest};
use crate::list::{MessageList, Selected};
use crate::reader::Reader;
use crate::triage::Target;
use crate::window::Window;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::actions::ActionId;
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::model::{AccountId, Folder, FolderId, FolderRole, MessageId};
use sealwax_infrastructure::store::ViewScope;
use std::cell::RefCell;
use std::rc::Rc;

/// The commands this module handles.
pub const COMMANDS: [ActionId; 5] = [
    ActionId::Compose,
    ActionId::Reply,
    ActionId::ReplyAll,
    ActionId::Forward,
    ActionId::EditDraft,
];

/// Connects the compose commands to the window's actions.
pub fn install(state: &Rc<AppState>, window: &Window, actions: &ActionSet) {
    for id in COMMANDS {
        let weak = Rc::downgrade(state);
        let target = window.window().downgrade();
        actions.connect(id, move || {
            if let (Some(state), Some(window)) = (weak.upgrade(), target.upgrade())
                && let Some(request) = request_for(&state, id)
            {
                open(&window, request);
            }
        });
    }
}

/// The reader's own requests open composer windows too, and its quick reply sends through
/// [`quick_send`].
pub fn connect_reader(reader: &Reader, window: &Window) {
    let target = window.window().downgrade();
    reader.connect_compose(move |request| {
        if let Some(window) = target.upgrade() {
            open(&window, request);
        }
    });
    reader.set_quick_send(quick_send);
}

/// Reply, Reply All and Forward apply to one selected message that isn't a draft; Edit Draft
/// to one in a Drafts folder. `targets` are the selected messages whose rows are loaded.
pub fn refresh(actions: &ActionSet, list: &MessageList, targets: &[Target], folders: &[Folder]) {
    let one = matches!(list.reported(), Selected::One(_));
    let draft = one
        && targets
            .first()
            .and_then(|target| folders.iter().find(|folder| folder.id == target.folder))
            .is_some_and(|folder| folder.role == FolderRole::Drafts);
    for id in [ActionId::Reply, ActionId::ReplyAll, ActionId::Forward] {
        actions.set_applies(id, one && !draft);
    }
    actions.set_applies(ActionId::EditDraft, draft);
}

fn open(window: &adw::ApplicationWindow, request: ComposeRequest) {
    if let Some(app) = window.application().and_downcast::<SealwaxApplication>() {
        composer::open(&app, request);
    }
}

/// What command `id` asks the composer for now; `None` when it doesn't apply.
fn request_for(state: &AppState, id: ActionId) -> Option<ComposeRequest> {
    let list = state.list()?;
    let kind = match id {
        ActionId::Compose => {
            let selected = list
                .selected_targets()
                .ok()
                .and_then(|targets| targets.first().map(|target| target.folder));
            let account = compose_account(list.scope(), selected, &state.mailbox().folders());
            return Some(ComposeRequest::New { account });
        }
        ActionId::EditDraft => {
            return match list.reported() {
                Selected::One(message) => Some(ComposeRequest::ServerDraft(message)),
                _ => None,
            };
        }
        ActionId::ReplyAll => ReplyKind::ReplyAll,
        ActionId::Forward => ReplyKind::Forward,
        _ => ReplyKind::Reply,
    };
    let message = message_for(&list, state.reader().as_ref())?;
    Some(ComposeRequest::Reply { message, kind })
}

/// The message Reply, Reply All and Forward act on: the selected one, or, once the reader shows
/// its conversation, the one the user is on there.
fn message_for(list: &MessageList, reader: Option<&Reader>) -> Option<MessageId> {
    let Selected::One(selected) = list.reported() else {
        return None;
    };
    match reader {
        Some(reader) if reader.message() == Some(selected) => reader.current_message(),
        _ => Some(selected),
    }
}

/// The account a new message is written from: the one whose folder the list shows; in All
/// Inboxes the selected message's (`selected` is its folder); `None` leaves it to the composer,
/// which takes the first account.
pub fn compose_account(
    scope: Option<ViewScope>,
    selected: Option<FolderId>,
    folders: &[Folder],
) -> Option<AccountId> {
    let account_of = |id: FolderId| {
        folders
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.account)
    };
    match scope {
        Some(ViewScope::Folder(folder)) => account_of(folder),
        _ => selected.and_then(account_of),
    }
}

thread_local! {
    /// Every quick reply [`quick_send`] took in this process, oldest first.
    static QUICK_SENDS: RefCell<Vec<(MessageId, String)>> = const { RefCell::new(Vec::new()) };
}

/// The quick reply's Send. Phase A's stand-in keeps the request for the self-tests and takes
/// it, so the box empties; phase B plans the reply by the compose rules, saves it as a draft
/// and queues it with Undo send. Only the message's id is logged, never the text.
pub fn quick_send(message: MessageId, text: &str) -> bool {
    tracing::info!(message = message.0, "quick reply send requested");
    QUICK_SENDS.with(|sends| sends.borrow_mut().push((message, text.to_owned())));
    true
}

/// Every quick reply sent in this process, oldest first; the self-tests read it.
pub fn quick_sends() -> Vec<(MessageId, String)> {
    QUICK_SENDS.with(|sends| sends.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, account: i64, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: format!("{role:?}"),
            role,
            selectable: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn compose_writes_from_the_account_shown() {
        let folders = [
            folder(1, 1, FolderRole::Inbox),
            folder(2, 1, FolderRole::Drafts),
            folder(3, 2, FolderRole::Inbox),
        ];
        let one = Some(ViewScope::Folder(FolderId(3)));
        assert_eq!(compose_account(one, None, &folders), Some(AccountId(2)));
        assert_eq!(
            compose_account(one, Some(FolderId(1)), &folders),
            Some(AccountId(2))
        );
        let all = Some(ViewScope::UnifiedInbox);
        assert_eq!(
            compose_account(all, Some(FolderId(3)), &folders),
            Some(AccountId(2))
        );
        assert_eq!(compose_account(all, None, &folders), None);
        assert_eq!(compose_account(None, None, &folders), None);
        let gone = Some(ViewScope::Folder(FolderId(9)));
        assert_eq!(compose_account(gone, None, &folders), None);
    }
}
