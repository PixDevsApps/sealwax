//! The composer (M6, ML-067): a window of its own for each draft (the user's choice at M6's
//! start, 2026-10-06), so Hyprland tiles it beside the main window and a `mailto:` link never
//! takes over what the reader shows. [`open`] is the one way in: the main window's Compose,
//! Reply, Reply All, Forward and Edit Draft, the quick reply's expansion, `mailto:` links, and
//! drafts reopened after a restart or an Undo send.
//!
//! Each window is an application window: it keeps Sealwax running while it is open, also
//! after the main window closed. Everything it needs from outside goes through
//! [`backend::Backend`]: phase A's [`backend::StandIn`] until phase B sets the real one with
//! [`set_backend`]. Quit saves every open draft first ([`save_all`]).

pub mod attachments;
pub mod backend;
pub mod editor;
mod js;
pub mod recipients;
pub mod window;

use crate::application::SealwaxApplication;
use backend::{Backend, StandIn};
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::mailto::Mailto;
use sealwax_domain::model::{AccountId, DraftId, MessageId};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
pub use window::{Composer, Ending};

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

/// How long Quit waits for the open drafts to be saved before it goes anyway.
pub const SAVE_ON_QUIT: Duration = Duration::from_secs(3);

thread_local! {
    static REQUESTS: RefCell<Vec<ComposeRequest>> = const { RefCell::new(Vec::new()) };
    static OPEN: RefCell<Vec<Composer>> = const { RefCell::new(Vec::new()) };
    static BACKEND: RefCell<Option<Rc<dyn Backend>>> = const { RefCell::new(None) };
    static STAND_IN: RefCell<Option<Rc<StandIn>>> = const { RefCell::new(None) };
    static CHOOSER: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    static KEYS_FOLLOWED: Cell<bool> = const { Cell::new(false) };
}

/// Opens a composer window for `request`. The window shows at once; the backend fills it.
pub fn open(app: &SealwaxApplication, request: ComposeRequest) {
    tracing::info!(kind = ?request, "compose requested");
    REQUESTS.with(|requests| requests.borrow_mut().push(request.clone()));
    follow_keys(app);
    let backend = backend(app);
    let composer = Composer::open(app, backend, request);
    OPEN.with(|open| open.borrow_mut().push(composer));
}

/// Every request [`open`] was given in this process, oldest first; the self-tests read it.
pub fn requests() -> Vec<ComposeRequest> {
    REQUESTS.with(|requests| requests.borrow().clone())
}

/// The composer windows open now, oldest first.
pub fn composers() -> Vec<Composer> {
    OPEN.with(|open| open.borrow().clone())
}

/// Whether a composer window is open.
pub fn is_open() -> bool {
    OPEN.with(|open| !open.borrow().is_empty())
}

/// Makes `backend` the one new composer windows use (phase B: the store and the engine).
pub fn set_backend(backend: Rc<dyn Backend>) {
    BACKEND.with(|current| current.replace(Some(backend)));
}

/// The phase A stand-in, when it is the backend in use.
pub fn stand_in() -> Option<Rc<StandIn>> {
    STAND_IN.with(|stand_in| stand_in.borrow().clone())
}

fn backend(app: &SealwaxApplication) -> Rc<dyn Backend> {
    if let Some(backend) = BACKEND.with(|current| current.borrow().clone()) {
        return backend;
    }
    let stand_in = StandIn::new(&app.state());
    STAND_IN.with(|current| current.replace(Some(stand_in.clone())));
    let backend: Rc<dyn Backend> = stand_in;
    BACKEND.with(|current| current.replace(Some(backend.clone())));
    backend
}

/// Self-tests: the next Attach File… takes `paths` instead of asking the file chooser.
pub fn answer_chooser(paths: Vec<PathBuf>) {
    CHOOSER.with(|chooser| chooser.replace(Some(paths)));
}

fn chooser_answer() -> Option<Vec<PathBuf>> {
    CHOOSER.with(|chooser| chooser.take())
}

/// Forgets a window that was destroyed.
fn forget(window: &libadwaita::ApplicationWindow) {
    OPEN.with(|open| {
        open.borrow_mut()
            .retain(|composer| composer.window() != window)
    });
}

/// Saves every open draft and lets the windows go; Quit waits for it (at most
/// [`SAVE_ON_QUIT`]) before it destroys them.
pub async fn save_all() {
    let composers = composers();
    let saving = async move {
        for composer in composers {
            composer.save_and_release().await;
        }
    };
    let saved = crate::mailbox::with_timeout(saving, SAVE_ON_QUIT).await;
    if saved.is_none() {
        tracing::warn!("composer: Quit stopped waiting for the drafts to be saved");
    }
}

/// `keys.toml` changes reach every open composer window.
fn follow_keys(app: &SealwaxApplication) {
    if KEYS_FOLLOWED.with(|followed| followed.replace(true)) {
        return;
    }
    let Some(prefs) = app.state().preferences() else {
        KEYS_FOLLOWED.with(|followed| followed.set(false));
        return;
    };
    let weak = Rc::downgrade(&prefs);
    prefs.connect_changed(move |update| {
        if !update.keymap_changed {
            return;
        }
        if let Some(prefs) = weak.upgrade() {
            let keymap = prefs.keymap();
            for composer in composers() {
                composer.update_keys(&keymap);
            }
        }
    });
}
