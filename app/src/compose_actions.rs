//! The main window's compose commands (M6: ML-068, ML-069, ML-074): Compose, Reply, Reply All,
//! Forward and Edit Draft from the list, the reader, the palette and their keys, and what the
//! reader asks for itself (the cards' buttons, the quick reply's expansion). Every one opens a
//! window through `composer::open`. The quick reply's Send goes through [`quick_send`], the seam
//! phase B wires to the outbox.
//!
//! - **Compose** writes from the account the list shows; in All Inboxes from the selected
//!   message's account, else the first account (`ComposeRequest::New { account: None }`).
//! - **Reply, Reply All, Reply to Sender and Forward** act on one message: in the list the
//!   selected one; once the reader shows its conversation, the message the user is on there
//!   ([`Reader::current_message`]). They don't apply to several messages, nor to a draft. Reply
//!   answers the sender, or everyone with `[reader] reply = "all"` ([`reply_kind`], issue #8);
//!   Reply to Sender always the sender. The quick reply answers as Reply does.
//! - **Edit Draft** opens the selected message of a Drafts folder.

use crate::actions::ActionSet;
use crate::application::{AppState, SealwaxApplication};
use crate::composer::backend::Refusal;
use crate::composer::{self, ComposeRequest};
use crate::list::{MessageList, Selected};
use crate::outbox;
use crate::reader::Reader;
use crate::triage::Target;
use crate::window::Window;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::actions::ActionId;
use sealwax_domain::compose::{Draft, ReplyKind};
use sealwax_domain::config::DefaultReply;
use sealwax_domain::model::{AccountId, DraftId, Folder, FolderId, FolderRole, MessageId};
use sealwax_infrastructure::store::ViewScope;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module handles.
pub const COMMANDS: [ActionId; 6] = [
    ActionId::Compose,
    ActionId::Reply,
    ActionId::ReplyAll,
    ActionId::ReplySender,
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

/// The reader's own requests open composer windows too; its quick reply sends, and keeps its
/// text as a draft, through the composer's backend (`composer::backend`).
pub fn connect_reader(reader: &Reader, window: &Window) {
    let target = window.window().downgrade();
    reader.connect_compose(move |request| {
        if let Some(window) = target.upgrade() {
            open(&window, request);
        }
    });
    let Some(app) = window
        .window()
        .application()
        .and_downcast::<SealwaxApplication>()
    else {
        return;
    };
    let weak = app.downgrade();
    reader.set_quick_send(move |message, text| {
        let app = weak.upgrade();
        Box::pin(async move {
            match app {
                Some(app) => send_quick(&app, message, text).await,
                None => false,
            }
        })
    });
    let keeper = Rc::new(QuickDrafts {
        app: app.downgrade(),
        kept: RefCell::new(HashMap::new()),
    });
    let saver = keeper.clone();
    reader.connect_quick_text(move |message, text| saver.changed(message, text));
    reader.set_quick_finder(move |message| keeper.find(message));
}

/// Reply, Reply All, Reply to Sender and Forward apply to one selected message that isn't a
/// draft; Edit Draft to one in a Drafts folder. `targets` are the selected messages whose rows
/// are loaded.
pub fn refresh(actions: &ActionSet, list: &MessageList, targets: &[Target], folders: &[Folder]) {
    let one = matches!(list.reported(), Selected::One(_));
    let draft = one
        && targets
            .first()
            .and_then(|target| folders.iter().find(|folder| folder.id == target.folder))
            .is_some_and(|folder| folder.role == FolderRole::Drafts);
    for id in [
        ActionId::Reply,
        ActionId::ReplyAll,
        ActionId::ReplySender,
        ActionId::Forward,
    ] {
        actions.set_applies(id, one && !draft);
    }
    actions.set_applies(ActionId::EditDraft, draft);
}

/// Opening a message of a Drafts folder (Enter, `o`, a double click) edits it in the composer
/// rather than showing it (the user's choice at the M6 gate); true when it did.
pub fn open_if_draft(state: &AppState, window: &Window, message: MessageId) -> bool {
    let Some(list) = state.list() else {
        return false;
    };
    let folders = state.mailbox().folders();
    let in_drafts = list
        .selected_targets()
        .ok()
        .and_then(|targets| targets.into_iter().find(|target| target.id == message))
        .and_then(|target| folders.iter().find(|folder| folder.id == target.folder))
        .is_some_and(|folder| folder.role == FolderRole::Drafts);
    if in_drafts {
        open(window.window(), ComposeRequest::ServerDraft(message));
    }
    in_drafts
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
        ActionId::Reply => reply_kind(state),
        ActionId::ReplyAll => ReplyKind::ReplyAll,
        ActionId::Forward => ReplyKind::Forward,
        _ => ReplyKind::Reply,
    };
    let message = message_for(&list, state.reader().as_ref())?;
    Some(ComposeRequest::Reply { message, kind })
}

/// Whom Reply and the quick reply answer: the sender, or everyone with `[reader] reply = "all"`
/// (issue #8).
pub fn reply_kind(state: &AppState) -> ReplyKind {
    match state.preferences().map(|prefs| prefs.config().reader.reply) {
        Some(DefaultReply::All) => ReplyKind::ReplyAll,
        _ => ReplyKind::Reply,
    }
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
    /// Every quick reply Send was asked for in this process, oldest first.
    static QUICK_SENDS: RefCell<Vec<(MessageId, String)>> = const { RefCell::new(Vec::new()) };
}

/// Every quick reply Send was asked for in this process, oldest first; the self-tests read it.
pub fn quick_sends() -> Vec<(MessageId, String)> {
    QUICK_SENDS.with(|sends| sends.borrow().clone())
}

/// The quick reply's Send (M6 phase B): the reply prepared by the compose rules
/// (`ComposeRequest::QuickReply`), saved, and handed to the backend's send, which queues it with
/// Undo send (`outbox::queued`). `false` keeps the text in the box: the window says why. Only the
/// message's id is logged, never the text.
async fn send_quick(app: &SealwaxApplication, message: MessageId, text: String) -> bool {
    tracing::info!(id = message.0, "quick reply send requested");
    QUICK_SENDS.with(|sends| sends.borrow_mut().push((message, text.clone())));
    let backend = composer::backend(app);
    let refuse = |why: &str| {
        let toast = adw::Toast::builder()
            .title(format!("The reply wasn't sent. {why}"))
            .use_markup(false)
            .timeout(outbox::FAILED_SHOWN.as_secs() as u32)
            .build();
        app.state().show_toast(&toast, false);
        false
    };
    let request = ComposeRequest::QuickReply {
        message,
        text: text.clone(),
        kind: reply_kind(&app.state()),
    };
    let prepared = match backend.prepare(request).await {
        Ok(prepared) => prepared,
        Err(why) => return refuse(&why),
    };
    if let Err(why) = backend.save(prepared.id, prepared.draft.clone()).await {
        let _ = backend.discard(prepared.id).await;
        return refuse(&why);
    }
    outbox::from_quick_reply(prepared.id, message, text);
    match backend.send(prepared.id, prepared.draft).await {
        Ok(()) => true,
        Err(Refusal(why)) => {
            outbox::forget_quick(prepared.id);
            let _ = backend.discard(prepared.id).await;
            refuse(&why)
        }
    }
}

/// How long the quick reply's text rests before it is saved as a draft, and the most it waits
/// while the typing goes on (PRD C5: at most 5 s behind the typing).
pub const QUICK_SAVE_IDLE: Duration = Duration::from_secs(2);
pub const QUICK_SAVE_AT_MOST: Duration = Duration::from_secs(5);

/// One message's quick reply as a draft.
#[derive(Default)]
struct Kept {
    /// The draft as it was last saved; `None` until the first save.
    draft: Option<(DraftId, Draft)>,
    text: String,
    saved: String,
    timer: Option<glib::SourceId>,
    first_unsaved: Option<Instant>,
    busy: bool,
}

/// Keeps every quick reply's text as a draft through the composer's backend: prepared once as
/// a reply to its message (`ComposeRequest::QuickReply` with no text yet), saved with the text
/// as its body (the quote comes back when it is sent or opened in a composer, from the compose
/// rules), closed so no composer window reopens it, and discarded when the box empties. After a
/// restart the reader finds it again through the store's newest closed reply draft to the
/// message ([`QuickDrafts::find`]).
struct QuickDrafts {
    app: glib::WeakRef<SealwaxApplication>,
    kept: RefCell<HashMap<MessageId, Kept>>,
}

impl QuickDrafts {
    fn changed(self: &Rc<Self>, message: MessageId, text: &str) {
        let mut kept = self.kept.borrow_mut();
        let entry = kept.entry(message).or_default();
        entry.text = text.to_owned();
        if let Some(timer) = entry.timer.take() {
            timer.remove();
        }
        if entry.text == entry.saved && !entry.busy {
            entry.first_unsaved = None;
            return;
        }
        let now = Instant::now();
        let first = *entry.first_unsaved.get_or_insert(now);
        let delay =
            QUICK_SAVE_IDLE.min((first + QUICK_SAVE_AT_MOST).saturating_duration_since(now));
        let weak = Rc::downgrade(self);
        entry.timer = Some(glib::timeout_add_local_once(delay, move || {
            if let Some(keeper) = weak.upgrade() {
                keeper.flush(message);
            }
        }));
    }

    fn flush(self: &Rc<Self>, message: MessageId) {
        let (text, draft) = {
            let mut kept = self.kept.borrow_mut();
            let Some(entry) = kept.get_mut(&message) else {
                return;
            };
            entry.timer = None;
            if entry.busy {
                return;
            }
            entry.busy = true;
            (entry.text.clone(), entry.draft.clone())
        };
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let keeper = self.clone();
        glib::spawn_future_local(async move {
            let saved = keep(&app, message, &text, draft).await;
            let again = {
                let mut kept = keeper.kept.borrow_mut();
                let Some(entry) = kept.get_mut(&message) else {
                    return;
                };
                entry.busy = false;
                if let Ok(draft) = saved {
                    entry.draft = draft;
                    entry.saved = text;
                    entry.first_unsaved = None;
                }
                (entry.text != entry.saved).then(|| entry.text.clone())
            };
            if let Some(text) = again {
                keeper.changed(message, &text);
            }
        });
    }

    /// What the quick reply held for `message` before this session: the text of its draft,
    /// when the draft's body is the quick reply's plain text.
    fn find(self: &Rc<Self>, message: MessageId) -> crate::reader::Local<Option<String>> {
        let known = self.kept.borrow().contains_key(&message);
        let reply = self.app.upgrade().filter(|_| !known).and_then(|app| {
            app.state()
                .mailbox()
                .store()
                .map(|store| store.reply_draft(message))
        });
        let keeper = self.clone();
        Box::pin(async move {
            let stored = reply?.recv().await.ok().flatten()?;
            let text = typed_text(&stored.draft.html)?;
            let mut kept = keeper.kept.borrow_mut();
            let entry = kept.entry(message).or_default();
            if entry.draft.is_none() && entry.text.is_empty() {
                entry.draft = Some((stored.id, stored.draft));
                entry.text = text.clone();
                entry.saved = text.clone();
            }
            Some(text)
        })
    }
}

/// Saves `text` as the quick reply's draft for `message`; empty, discards it. The draft after
/// it, or `Err` when the backend refused (the text stays in memory and is saved again on the
/// next change).
async fn keep(
    app: &SealwaxApplication,
    message: MessageId,
    text: &str,
    draft: Option<(DraftId, Draft)>,
) -> Result<Option<(DraftId, Draft)>, ()> {
    let backend = composer::backend(app);
    if text.trim().is_empty() {
        if let Some((id, _)) = draft {
            backend.discard(id).await.map_err(|_| ())?;
        }
        return Ok(None);
    }
    let (id, mut body) = match draft {
        Some(draft) => draft,
        None => {
            let request = ComposeRequest::QuickReply {
                message,
                text: String::new(),
                kind: reply_kind(&app.state()),
            };
            let prepared = backend.prepare(request).await.map_err(|_| {
                tracing::warn!(id = message.0, "the quick reply's draft couldn't be made");
            })?;
            (prepared.id, prepared.draft)
        }
    };
    body.html = sealwax_domain::compose::text::to_html(text);
    backend.save(id, body.clone()).await.map_err(|_| {
        tracing::warn!(id = message.0, "the quick reply's draft couldn't be saved");
    })?;
    backend.close(id);
    Ok(Some((id, body)))
}

/// The plain text of a body that is nothing but the editor's lines (`compose::text::to_html`'s
/// output); `None` for anything else (formatting, a quote, a signature).
pub fn typed_text(html: &str) -> Option<String> {
    let mut lines = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let inner = rest.strip_prefix("<div>")?;
        let end = inner.find("</div>")?;
        let line = &inner[..end];
        rest = &inner[end + "</div>".len()..];
        if line == "<br>" {
            lines.push(String::new());
            continue;
        }
        if line.contains('<') {
            return None;
        }
        lines.push(
            line.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&nbsp;", " ")
                .replace("&amp;", "&"),
        );
    }
    let text = lines.join("\n");
    (sealwax_domain::compose::text::to_html(&text) == html).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, account: i64, role: FolderRole) -> Folder {
        Folder {
            id: FolderId(id),
            account: AccountId(account),
            name: format!("{role:?}"),
            delimiter: Some('/'),
            role,
            selectable: true,
            inferiors: true,
            total: 0,
            unread: 0,
        }
    }

    #[test]
    fn a_quick_replys_draft_gives_its_text_back_and_nothing_else() {
        use sealwax_domain::compose::text::to_html;
        for text in [
            "See you Friday!",
            "Two lines\n\nand a gap",
            "  indented <b> & \"quoted\" 'text'",
            "æøå 😀",
        ] {
            assert_eq!(typed_text(&to_html(text)).as_deref(), Some(text), "{text}");
        }
        assert_eq!(typed_text("<div><b>bold</b></div>"), None);
        assert_eq!(
            typed_text("<div>Hi</div><div><br></div><blockquote type=\"cite\">x</blockquote>"),
            None
        );
        assert_eq!(typed_text("<p>Hi</p>"), None);
        assert_eq!(typed_text("<div>a</div>trailing"), None);
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
