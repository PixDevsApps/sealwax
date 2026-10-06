//! The composer (M6, ML-067): a window of its own for each draft (the user's choice at M6's
//! start, 2026-10-06), so Hyprland tiles it beside the main window and a `mailto:` link never
//! takes over what the reader shows. [`open`] is the one way in: the main window's Compose,
//! Reply, Reply All, Forward and Edit Draft, the quick reply's expansion, `mailto:` links, and
//! drafts reopened after a restart or an Undo send.

use crate::application::SealwaxApplication;
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::mailto::Mailto;
use sealwax_domain::model::{AccountId, DraftId, MessageId};
use std::cell::RefCell;
use std::fmt;

/// What a composer window starts from.
#[derive(Clone)]
pub enum ComposeRequest {
    /// A new message from `account`: the one the list shows, or the first account.
    New { account: Option<AccountId> },
    /// A reply, reply all or forward of a stored message.
    Reply { message: MessageId, kind: ReplyKind },
    /// A reply to `message` that starts with what the quick reply under its conversation holds
    /// (ML-068): plain text, typed by the user.
    QuickReply { message: MessageId, text: String },
    /// A `mailto:` link (PRD C8): its safe fields only (`domain::mailto`).
    Mailto(Mailto),
    /// A draft the store keeps: reopened after a restart, after Undo send, or after a send
    /// failed.
    Draft(DraftId),
    /// A message in a Drafts folder, written in Sealwax or elsewhere (Edit Draft).
    ServerDraft(MessageId),
}

/// The request's kind only: what it carries is the user's mail.
impl fmt::Debug for ComposeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComposeRequest::New { account } => write!(f, "New({account:?})"),
            ComposeRequest::Reply { message, kind } => write!(f, "Reply({message}, {kind:?})"),
            ComposeRequest::QuickReply { message, .. } => write!(f, "QuickReply({message})"),
            ComposeRequest::Mailto(_) => f.write_str("Mailto"),
            ComposeRequest::Draft(draft) => write!(f, "Draft({draft})"),
            ComposeRequest::ServerDraft(message) => write!(f, "ServerDraft({message})"),
        }
    }
}

thread_local! {
    static REQUESTS: RefCell<Vec<ComposeRequest>> = const { RefCell::new(Vec::new()) };
}

/// Opens a composer window for `request`. Until the composer is built (M6's phase A) the
/// request is only kept, for [`requests`].
pub fn open(_app: &SealwaxApplication, request: ComposeRequest) {
    tracing::info!(kind = ?request, "compose requested");
    REQUESTS.with(|requests| requests.borrow_mut().push(request));
}

/// Every request [`open`] was given in this process, oldest first; the self-tests read it.
pub fn requests() -> Vec<ComposeRequest> {
    REQUESTS.with(|requests| requests.borrow().clone())
}
