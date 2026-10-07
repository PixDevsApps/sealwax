//! The outbox in the main window (M6 phase B, ML-076): the toast with Undo that follows a draft
//! queued to send, from a composer window or the quick reply, and what the window says about a
//! send that failed or may have gone out.
//!
//! - **Queued** ([`queued`]): a toast for the undo window (triage's 7 s) with Undo; the send
//!   becomes the last action, so `z` and Ctrl+Z take it back too while it can be (ADR-015: only
//!   the last action). Undo asks the store (`undo_send`): back to a draft, it reopens in a
//!   composer window, or its text goes back to the quick reply it came from; too late, it says
//!   it is being sent.
//! - **Sent**: a short toast. **Failed for good**: a toast for 10 s, like refused changes, with
//!   Open (the store kept the draft and why). **May have gone out**: the window's banner, whose
//!   Choose… asks the user to send it again, file it as sent, or take it back to a draft.
//! - The offline banner's "N waiting" counts the sends waiting in the outbox too.
//!
//! The outbox is read off the GTK thread when the store says it changed. Nothing here logs a
//! recipient, a subject or a text: drafts are named by id.

use crate::application::{AppState, SealwaxApplication};
use crate::composer::{self, ComposeRequest};
use crate::engine_link::Subscription;
use crate::triage;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::model::{AccountId, BlobKey, DraftId, MessageId};
use sealwax_engine::api::{Command, Event, SendError};
use sealwax_infrastructure::store::{OutboxEntry, Resolution, SendRecorded, SendState, SendUndone};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

/// How long a failed send's toast shows (as refused changes').
pub const FAILED_SHOWN: Duration = Duration::from_secs(10);
/// How long "Message sent" shows.
pub const SENT_SHOWN: Duration = Duration::from_secs(4);

/// The queued toast's text (accepted at the M6 gate).
pub const QUEUED_TEXT: &str = "Message queued";
pub const SENT_TEXT: &str = "Message sent";
pub const UNDONE_TEXT: &str = "Sending undone";
pub const UNDONE_QUICK_TEXT: &str = "Sending undone: the reply is back under its conversation";

/// What the uncertain-send dialog's answers do, by response id.
pub const ANSWERS: [(&str, &str); 3] = [
    ("back-to-draft", "Back to Drafts"),
    ("was-sent", "It Was Sent"),
    ("send-again", "Send Again"),
];

#[derive(Default)]
struct Outbox {
    state: RefCell<Weak<AppState>>,
    /// The send Undo takes back while it is the last action.
    last: Cell<Option<DraftId>>,
    expiry: RefCell<Option<glib::SourceId>>,
    /// Sends that came from the quick reply: the message they answer and the text, for Undo.
    from_quick: RefCell<HashMap<DraftId, (MessageId, String)>>,
    /// The outbox as last read.
    entries: RefCell<Vec<OutboxEntry>>,
    reading: Cell<bool>,
    read_again: Cell<bool>,
    events: RefCell<Option<Subscription>>,
}

thread_local! {
    static OUTBOX: Rc<Outbox> = Rc::default();
}

fn shared() -> Rc<Outbox> {
    OUTBOX.with(Rc::clone)
}

fn now_ms() -> i64 {
    glib::real_time() / 1000
}

fn app() -> Option<SealwaxApplication> {
    gio::Application::default().and_downcast::<SealwaxApplication>()
}

/// Follows the engine's sends and the store's outbox, once at startup.
pub fn install(state: &Rc<AppState>) {
    let outbox = shared();
    outbox.state.replace(Rc::downgrade(state));
    let weak = Rc::downgrade(state);
    let subscription = state.engine().subscribe(move |event| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        match event {
            Event::Sent { draft, .. } => sent(&state, *draft),
            Event::SendFailed {
                account,
                draft,
                error,
            } => failed(&state, *account, *draft, error),
            Event::SendUncertain { draft, .. } => {
                tracing::info!(draft = draft.0, "a send may have gone out");
                settle(&state, *draft);
                read(&state);
            }
            _ => {}
        }
    });
    outbox.events.replace(Some(subscription));
    let weak = Rc::downgrade(state);
    state.mailbox().connect_event(move |event| {
        if event.outbox.is_empty() {
            return;
        }
        if let Some(state) = weak.upgrade() {
            read(&state);
        }
    });
    let ready = state.mailbox().wait_ready();
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        ready.await;
        if let Some(state) = weak.upgrade() {
            read(&state);
        }
    });
}

/// Reads the outbox again, at most one read at a time, and updates the banner.
fn read(state: &Rc<AppState>) {
    let outbox = shared();
    if outbox.reading.replace(true) {
        outbox.read_again.set(true);
        return;
    }
    let Some(reply) = state.mailbox().store().map(|store| store.outbox(None)) else {
        outbox.reading.set(false);
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let entries = reply.recv().await;
        let outbox = shared();
        outbox.reading.set(false);
        match entries {
            Ok(entries) => {
                outbox.entries.replace(entries);
            }
            Err(error) => tracing::warn!(%error, "the outbox couldn't be read"),
        }
        let Some(state) = weak.upgrade() else {
            return;
        };
        if outbox.read_again.replace(false) {
            read(&state);
        }
        state.update_banner();
    });
}

/// Sends waiting for a server (queued or being sent), for the offline banner's count.
pub fn waiting_sends() -> u32 {
    shared()
        .entries
        .borrow()
        .iter()
        .filter(|entry| matches!(entry.state, SendState::Queued | SendState::Sending))
        .count() as u32
}

/// The sends that may have gone out, oldest first, with their accounts.
pub fn uncertain() -> Vec<(DraftId, AccountId)> {
    shared()
        .entries
        .borrow()
        .iter()
        .filter(|entry| entry.state == SendState::Uncertain)
        .map(|entry| (entry.draft, entry.account))
        .collect()
}

/// The outbox as last read, for checks.
pub fn entries() -> Vec<OutboxEntry> {
    shared().entries.borrow().clone()
}

/// A send from the quick reply that answers `message` with `text`: Undo puts the text back.
pub fn from_quick_reply(draft: DraftId, message: MessageId, text: String) {
    shared()
        .from_quick
        .borrow_mut()
        .insert(draft, (message, text));
}

/// The send left the outbox's hands: it no longer goes back to the quick reply, nor is it the
/// one Undo takes back.
fn settle(state: &AppState, draft: DraftId) {
    let outbox = shared();
    outbox.from_quick.borrow_mut().remove(&draft);
    if outbox.last.get() == Some(draft) {
        outbox.last.set(None);
        state.refresh_commands();
    }
}

/// A refused send of the quick reply: forget it.
pub fn forget_quick(draft: DraftId) {
    shared().from_quick.borrow_mut().remove(&draft);
}

/// `draft` was queued to send with the undo window (the backend's `send` calls it after the
/// store's `queue_send` and `Command::OutboxQueued`): a toast with Undo for the window, and the
/// send becomes the last action.
pub fn queued(state: &AppState, draft: DraftId, account: AccountId) {
    tracing::info!(draft = draft.0, account = account.0, "send queued");
    let outbox = shared();
    if let Some(triage) = state.triage() {
        triage.forget_last();
    }
    outbox.last.set(Some(draft));
    let window = state
        .triage()
        .map_or(triage::UNDO_WINDOW, |triage| triage.window());
    let expiry = glib::timeout_add_local_once(window, move || {
        let outbox = shared();
        outbox.expiry.replace(None);
        if outbox.last.get() == Some(draft) {
            outbox.last.set(None);
            if let Some(state) = outbox.state.borrow().upgrade() {
                state.refresh_commands();
            }
        }
    });
    if let Some(old) = outbox.expiry.replace(Some(expiry)) {
        old.remove();
    }
    let toast = toast(QUEUED_TEXT, window);
    toast.set_button_label(Some("Undo"));
    toast.connect_button_clicked(move |_| {
        if let Some(state) = shared().state.borrow().upgrade() {
            undo(&state, draft);
        }
    });
    state.show_toast(&toast, true);
    state.refresh_commands();
}

fn toast(text: &str, shown: Duration) -> adw::Toast {
    adw::Toast::builder()
        .title(text)
        .use_markup(false)
        .timeout(shown.as_millis().div_ceil(1000).max(1) as u32)
        .priority(adw::ToastPriority::High)
        .build()
}

/// Whether Undo would take back a send: the last action is one still in its window.
pub fn can_undo() -> bool {
    shared().last.get().is_some()
}

/// Another action became the last one: the send's Undo no longer works.
pub fn forget_last() {
    shared().last.set(None);
}

/// Undo (`z`, Ctrl+Z, the palette): takes back the last send if the last action is one;
/// `false` leaves Undo to triage.
pub fn undo_last(state: &AppState) -> bool {
    match shared().last.get() {
        Some(draft) => {
            undo(state, draft);
            true
        }
        None => false,
    }
}

/// Takes `draft` back from the outbox, if it is still the last action.
fn undo(state: &AppState, draft: DraftId) {
    let outbox = shared();
    if outbox.last.get() != Some(draft) {
        return;
    }
    outbox.last.set(None);
    state.refresh_commands();
    let Some(reply) = state.mailbox().store().map(|store| store.undo_send(draft)) else {
        return;
    };
    let weak = outbox.state.borrow().clone();
    glib::spawn_future_local(async move {
        let answer = reply.recv().await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let text = match answer {
            Ok(SendUndone::Undone {
                account,
                orphaned_blobs,
                ..
            }) => {
                tracing::info!(draft = draft.0, "send undone");
                remove_blobs(&state, orphaned_blobs);
                wake(&state, Command::OutboxQueued { account });
                reopen(&state, draft)
            }
            Ok(SendUndone::TooLate(SendState::Sent)) => {
                "Too late to undo: the message was sent".to_owned()
            }
            Ok(SendUndone::TooLate(SendState::Uncertain)) => {
                "Too late to undo: the message may have been sent".to_owned()
            }
            Ok(SendUndone::TooLate(_)) => "Too late to undo: the message is being sent".to_owned(),
            Ok(SendUndone::NotQueued) => "The message wasn't waiting to be sent".to_owned(),
            Err(error) => {
                tracing::warn!(%error, "a send couldn't be undone");
                "Sending couldn't be undone".to_owned()
            }
        };
        state.show_toast(&toast(&text, Duration::from_secs(4)), true);
    });
}

/// An undone send goes back where it was written: its text to the quick reply it came from
/// (its draft, with the quote, is discarded: the quick reply keeps its own), or its draft to a
/// composer window. Returns the toast's text.
fn reopen(state: &Rc<AppState>, draft: DraftId) -> String {
    let quick = shared().from_quick.borrow_mut().remove(&draft);
    match quick {
        Some((message, text)) => {
            if let Some(reply) = state
                .mailbox()
                .store()
                .map(|store| store.discard_draft(draft))
            {
                let weak = Rc::downgrade(state);
                glib::spawn_future_local(async move {
                    if let (Ok(discarded), Some(state)) = (reply.recv().await, weak.upgrade()) {
                        remove_blobs(&state, discarded.orphaned_blobs);
                        if let (true, Some(account)) = (discarded.server_copy, discarded.account) {
                            wake(&state, Command::DraftsChanged { account });
                        }
                    }
                });
            }
            if let Some(reader) = state.reader() {
                reader.restore_quick_reply(message, text);
            }
            UNDONE_QUICK_TEXT.to_owned()
        }
        None => {
            if let Some(app) = app() {
                composer::open(&app, ComposeRequest::Draft(draft));
            }
            UNDONE_TEXT.to_owned()
        }
    }
}

fn wake(state: &AppState, command: Command) {
    if state.engine().send(command).is_err() {
        tracing::debug!("no engine to tell about the outbox");
    }
}

/// Deletes blobs nothing references any more, off the GTK thread.
fn remove_blobs(state: &AppState, keys: Vec<BlobKey>) {
    if keys.is_empty() {
        return;
    }
    let Some(blobs) = state.mailbox().blobs() else {
        return;
    };
    gio::spawn_blocking(move || {
        for key in keys {
            if let Err(error) = blobs.remove(&key) {
                tracing::warn!(%error, "a blob nothing needs couldn't be removed");
            }
        }
    });
}

/// The server took `draft`.
fn sent(state: &Rc<AppState>, draft: DraftId) {
    tracing::info!(draft = draft.0, "sent");
    settle(state, draft);
    state.show_toast(&toast(SENT_TEXT, SENT_SHOWN), false);
    read(state);
}

/// What a failed send's toast says (never the recipients or the subject).
pub fn failure_text(error: &SendError) -> String {
    let why = match error {
        SendError::RecipientsRefused(refused) if refused.len() == 1 => {
            "the server refused 1 recipient".to_owned()
        }
        SendError::RecipientsRefused(refused) => {
            format!("the server refused {} recipients", refused.len())
        }
        SendError::TooLarge { limit: Some(limit) } => format!(
            "it is larger than the server takes ({})",
            glib::format_size(*limit)
        ),
        SendError::TooLarge { limit: None } => "it is larger than the server takes".to_owned(),
        SendError::Refused => "the server refused it".to_owned(),
        SendError::NeedsSignIn => "the account needs you to sign in again".to_owned(),
        SendError::Offline => "the mail server can't be reached".to_owned(),
        SendError::Interrupted => "Sealwax stopped while preparing it".to_owned(),
        SendError::Store | SendError::Other => String::new(),
    };
    if why.is_empty() {
        "Your message wasn't sent. It's back in your drafts.".to_owned()
    } else {
        format!("Your message wasn't sent: {why}. It's back in your drafts.")
    }
}

/// The server refused `draft` for good: the store put it back as a draft that says why.
fn failed(state: &Rc<AppState>, account: AccountId, draft: DraftId, error: &SendError) {
    tracing::info!(draft = draft.0, account = account.0, ?error, "send failed");
    settle(state, draft);
    let toast = toast(&failure_text(error), FAILED_SHOWN);
    toast.set_button_label(Some("Open"));
    toast.connect_button_clicked(move |_| {
        if let Some(app) = app() {
            composer::open(&app, ComposeRequest::Draft(draft));
        }
    });
    state.show_toast(&toast, false);
    read(state);
}

/// The banner's Choose… for the first send that may have gone out: send it again, file it as
/// sent, or take it back to a draft.
pub fn decide(state: &Rc<AppState>) {
    let Some((draft, account)) = uncertain().first().copied() else {
        return;
    };
    let Some(window) = state.window() else {
        return;
    };
    let name = state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|known| known.id == account)
        .map(|known| known.name)
        .unwrap_or_default();
    let dialog = adw::AlertDialog::builder()
        .heading("Was the message sent?")
        .body(decide_text(&name))
        .close_response("later")
        .default_response("later")
        .build();
    dialog.set_widget_name("uncertain-send");
    dialog.add_response("later", "Later");
    for (id, label) in ANSWERS {
        dialog.add_response(id, label);
    }
    dialog.set_response_appearance("send-again", adw::ResponseAppearance::Destructive);
    let weak = Rc::downgrade(state);
    dialog.connect_response(None, move |_, response| {
        let resolution = match response {
            "send-again" => Resolution::SendAgain {
                not_before: now_ms(),
            },
            "was-sent" => Resolution::WasSent,
            "back-to-draft" => Resolution::BackToDraft,
            _ => return,
        };
        if let Some(state) = weak.upgrade() {
            resolve(&state, draft, account, resolution);
        }
    });
    dialog.present(Some(window.window()));
}

/// The dialog's text: what happened, how to tell, and what each answer does.
pub fn decide_text(account: &str) -> String {
    let from = if account.is_empty() {
        String::new()
    } else {
        format!(" from {account}")
    };
    format!(
        "Sealwax stopped while it was sending a message{from}, so the server may have it. Look \
         in your Sent folder, or ask a recipient.\n\nSend Again sends the same message, which \
         its recipients may then get twice. It Was Sent files it as sent. Back to Drafts opens \
         it, to change or send it later."
    )
}

fn resolve(state: &Rc<AppState>, draft: DraftId, account: AccountId, resolution: Resolution) {
    let Some(reply) = state
        .mailbox()
        .store()
        .map(|store| store.resolve_uncertain(draft, resolution, now_ms()))
    else {
        return;
    };
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let answer = reply.recv().await;
        let Some(state) = weak.upgrade() else {
            return;
        };
        let recorded: SendRecorded = match answer {
            Ok(recorded) => recorded,
            Err(error) => {
                tracing::warn!(%error, "an uncertain send couldn't be resolved");
                state.show_toast(
                    &toast(
                        "Sealwax couldn't record your answer",
                        Duration::from_secs(4),
                    ),
                    false,
                );
                return;
            }
        };
        tracing::info!(
            draft = draft.0,
            applied = recorded.applied,
            "uncertain send resolved"
        );
        remove_blobs(&state, recorded.orphaned_blobs.clone());
        if !recorded.applied {
            state.show_toast(
                &toast(
                    "That message was already dealt with",
                    Duration::from_secs(4),
                ),
                false,
            );
            read(&state);
            return;
        }
        let text = match resolution {
            Resolution::SendAgain { .. } => {
                wake(&state, Command::OutboxQueued { account });
                Some("Sending it again")
            }
            Resolution::WasSent => {
                wake(&state, Command::OutboxQueued { account });
                Some("Filed as sent")
            }
            Resolution::BackToDraft => {
                if let Some(app) = app() {
                    composer::open(&app, ComposeRequest::Draft(draft));
                }
                None
            }
        };
        if recorded.ops_queued {
            wake(&state, Command::OpsQueued { account });
        }
        if recorded.copies_to_delete {
            wake(&state, Command::DraftsChanged { account });
        }
        if let Some(text) = text {
            state.show_toast(&toast(text, Duration::from_secs(4)), false);
        }
        read(&state);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_send_says_why_without_its_recipients() {
        let refused = SendError::RecipientsRefused(vec![
            "kari@example.test".into(),
            "ola@example.test".into(),
        ]);
        let text = failure_text(&refused);
        assert_eq!(
            text,
            "Your message wasn't sent: the server refused 2 recipients. It's back in your drafts."
        );
        assert!(!text.contains("example.test"));
        assert_eq!(
            failure_text(&SendError::RecipientsRefused(vec!["a@example.test".into()])),
            "Your message wasn't sent: the server refused 1 recipient. It's back in your drafts."
        );
        // GLib may put a no-break space between the number and the unit.
        assert!(
            failure_text(&SendError::TooLarge {
                limit: Some(26_214_400)
            })
            .contains("(26.2")
        );
        assert!(failure_text(&SendError::TooLarge { limit: None }).contains("larger"));
        assert_eq!(
            failure_text(&SendError::Other),
            "Your message wasn't sent. It's back in your drafts."
        );
        assert_eq!(
            failure_text(&SendError::Interrupted),
            "Your message wasn't sent: Sealwax stopped while preparing it. It's back in your drafts."
        );
    }

    #[test]
    fn the_uncertain_question_names_the_account_and_each_answer() {
        let text = decide_text("Work");
        assert!(text.starts_with("Sealwax stopped while it was sending a message from Work,"));
        for (_, label) in ANSWERS {
            assert!(text.contains(label), "{label}");
        }
        assert!(decide_text("").starts_with("Sealwax stopped while it was sending a message,"));
    }
}
