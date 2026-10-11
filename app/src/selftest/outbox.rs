//! Self-test commands for the outbox in the main window (M6 phase B, ML-076) and the quick
//! reply's drafts: drafts queued in the app's store as the composer's backend queues them,
//! the engine's steps and events played by the fake engine (`engine-attach` first), the toasts'
//! buttons, and the stand-in backend's record of the quick reply's drafts. Draft names are the
//! script's own; message names are the conversation samples'. Every name, address and text is
//! fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `outbox-draft NAME ACCOUNT` | Saves a draft from ACCOUNT (`triage-account`'s, which has a server) in the store, closed |
//! | `outbox-queue NAME` | Queues it for the undo window (`queue_send`) and calls `outbox::queued`, as the backend's send does |
//! | `outbox-quick NAME ACCOUNT MESSAGE TEXT` | A reply to MESSAGE from ACCOUNT, sent from the quick reply with TEXT: saved, marked as the quick reply's, queued |
//! | `outbox-engine start\|sent\|uncertain NAME [stopped\|no-reply\|unreadable]` | The engine's step on it: `send_started`; `send_done` and `Event::Sent`; `send_uncertain` and `Event::SendUncertain` with why (`stopped` when none is named) |
//! | `outbox-engine failed NAME ERROR` | `send_failed` (permanent) and `Event::SendFailed`: `recipients-refused` (two), `too-large`, `refused`, `needs-sign-in`, `other` |
//! | `outbox-expect KIND [KEY=VALUE]` | The app sent KIND to the engine (`outbox-queued`, `ops-queued`, `drafts-changed`) within a second; other commands are passed over |
//! | `toast-click LABEL` | Clicks the toast button labelled LABEL (`Undo`, `Open`) |
//! | `quick-store-draft MESSAGE TEXT` | A closed reply draft to MESSAGE whose body is TEXT, in the store, as an earlier session's quick reply left it |
//! | `assert-outbox NAME queued\|sending\|uncertain\|sent\|draft\|gone` | Where the draft stands in the store |
//! | `assert-outbox-entries N` | The outbox as the window last read it holds N entries |
//! | `stand-in-mark` | Notes the time, for `assert-stand-in saved-between` |
//! | `assert-stand-in sent\|discarded\|closed N` | The stand-in backend's count |
//! | `assert-stand-in saved-between MIN MAX` | A save came MIN to MAX ms after `stand-in-mark`; prints every save's time since |
//! | `assert-stand-in last-sent TEXT` | The stand-in's last sent draft starts with TEXT's lines |
//! | `assert-stand-in quick-draft MESSAGE TEXT\|none` | The stand-in's newest kept reply draft to MESSAGE: its body is TEXT (closed), or it was discarded |
//! | `assert-compose-draft NAME` | The latest `composer::open` request is `Draft(NAME)` |

use super::{Harness, StepResult, blocking, equal, number, setup, triage};
use crate::composer::{self, ComposeRequest};
use crate::outbox;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::compose::text::to_html;
use sealwax_domain::compose::{Draft, DraftSource, ReplyKind};
use sealwax_domain::model::{Address, DraftId};
use sealwax_engine::api::{Event, SendError, UncertainReason};
use sealwax_infrastructure::store::{
    DraftSaved, Prepared, SaveDraft, SendFailed, SendFailure, SendQueued, SendState,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("outbox-draft", 2, 2),
    ("outbox-queue", 1, 1),
    ("outbox-quick", 4, 4),
    ("outbox-engine", 2, 3),
    ("outbox-expect", 1, 2),
    ("toast-click", 1, 1),
    ("quick-store-draft", 2, 2),
    ("assert-outbox", 2, 2),
    ("assert-outbox-entries", 1, 1),
    ("assert-stand-in", 2, 3),
    ("stand-in-mark", 0, 0),
    ("assert-compose-draft", 1, 1),
];

thread_local! {
    /// The script's drafts by name, with their accounts.
    static DRAFTS: RefCell<HashMap<String, (DraftId, sealwax_domain::model::AccountId)>> =
        RefCell::new(HashMap::new());
    static MARK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

fn now_ms() -> i64 {
    glib::real_time() / 1000
}

fn draft(name: &str) -> Result<(DraftId, sealwax_domain::model::AccountId), String> {
    DRAFTS
        .with(|drafts| drafts.borrow().get(name).copied())
        .ok_or_else(|| format!("no draft {name}; save it with outbox-draft"))
}

fn store(
    harness: &Harness,
) -> Result<std::sync::Arc<sealwax_infrastructure::store::Store>, String> {
    harness
        .state
        .mailbox()
        .shared_store()
        .ok_or_else(|| "the store is not open".to_owned())
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "outbox-draft" => save_draft(harness, arg(0), arg(1)).await,
        "outbox-queue" => queue(harness, arg(0)).await,
        "outbox-quick" => {
            save_draft(harness, arg(0), arg(1)).await?;
            let (id, _) = draft(arg(0))?;
            let message = super::conversation::id(arg(2))?;
            outbox::from_quick_reply(id, message, arg(3).to_owned());
            queue(harness, arg(0)).await
        }
        "outbox-engine" => engine(harness, args).await,
        "outbox-expect" => expect(harness, arg(0), args.get(1).map(String::as_str)).await,
        "toast-click" => {
            let button = toast_button(harness, arg(0))?;
            triage::click_button(harness, &button).await
        }
        "quick-store-draft" => store_quick_draft(harness, arg(0), arg(1)).await,
        "assert-outbox" => {
            let (id, _) = draft(arg(0))?;
            let stored = store(harness)?
                .draft(id)
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let state = match stored {
                None => "gone",
                Some(stored) => match stored.send {
                    None => "draft",
                    Some(SendState::Queued) => "queued",
                    Some(SendState::Sending) => "sending",
                    Some(SendState::Uncertain) => "uncertain",
                    Some(SendState::Sent) => "sent",
                },
            };
            equal(&format!("{}'s state", arg(0)), state, arg(1))
        }
        "assert-outbox-entries" => {
            equal("outbox entries", outbox::entries().len(), number(arg(0))?)
        }
        "assert-stand-in" => stand_in(harness, args),
        "stand-in-mark" => {
            MARK.with(|mark| mark.set(Some(Instant::now())));
            Ok(())
        }
        "assert-compose-draft" => {
            let (id, _) = draft(arg(0))?;
            match composer::requests().last() {
                Some(ComposeRequest::Draft(opened)) => equal("draft opened", *opened, id),
                other => Err(format!("the latest request is {other:?}, not a draft")),
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// A fictional draft from the account named `account`, saved closed in the store.
async fn save_draft(harness: &Harness, name: &str, account: &str) -> StepResult {
    let account = harness
        .state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|known| known.name == account)
        .ok_or_else(|| format!("no account {account}"))?;
    let draft = Draft {
        account: account.id,
        from: Address::new(account.name.clone(), account.address.clone()),
        to: vec![
            Address::new("Kari Nordmann", "kari@example.test"),
            Address::new("Ola Nordmann", "ola@example.test"),
        ],
        subject: format!("Notes {name}"),
        html: to_html("A fictional message from the self-test."),
        message_id: format!("{name}.{}@example.test", glib::uuid_string_random()),
        ..Draft::default()
    };
    let saved = store(harness)?
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: now_ms(),
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    let DraftSaved::Saved { id, .. } = saved else {
        return Err(format!("the draft wasn't saved: {saved:?}"));
    };
    println!("  # {name} is draft {}", id.0);
    DRAFTS.with(|drafts| {
        drafts
            .borrow_mut()
            .insert(name.to_owned(), (id, account.id))
    });
    Ok(())
}

/// What the backend's send does: the store's queue for the undo window, then the window's toast.
async fn queue(harness: &Harness, name: &str) -> StepResult {
    let (id, _) = draft(name)?;
    let window = harness
        .state
        .triage()
        .map_or(crate::triage::UNDO_WINDOW, |triage| triage.window());
    let queued = store(harness)?
        .queue_send(id, now_ms() + window.as_millis() as i64)
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    let SendQueued::Queued { account, .. } = queued else {
        return Err(format!("not queued: {queued:?}"));
    };
    outbox::queued(&harness.state, id, account);
    harness.wait_until_settled().await;
    Ok(())
}

fn send_error(name: &str) -> Result<(SendError, SendFailure), String> {
    Ok(match name {
        "recipients-refused" => (
            SendError::RecipientsRefused(vec![
                "kari@example.test".into(),
                "ola@example.test".into(),
            ]),
            SendFailure::RecipientsRefused,
        ),
        "too-large" => (
            SendError::TooLarge {
                limit: Some(26_214_400),
            },
            SendFailure::TooLarge,
        ),
        "refused" => (SendError::Refused, SendFailure::Refused),
        "needs-sign-in" => (SendError::NeedsSignIn, SendFailure::NeedsSignIn),
        "other" => (SendError::Other, SendFailure::Other),
        "interrupted" => (SendError::Interrupted, SendFailure::Interrupted),
        other => return Err(format!("no send error {other}")),
    })
}

/// The engine's part, played on the store and through the fake engine's events.
async fn engine(harness: &Harness, args: &[String]) -> StepResult {
    let (id, account) = draft(&args[1])?;
    let store = store(harness)?;
    let applied = |recorded: Result<sealwax_infrastructure::store::SendRecorded, _>| {
        let recorded = recorded
            .map_err(|error: sealwax_infrastructure::store::StoreError| error.to_string())?;
        if recorded.applied {
            Ok(())
        } else {
            Err(format!("the store didn't apply it: {recorded:?}"))
        }
    };
    match args[0].as_str() {
        "start" => {
            // A send started before (sent again after it was uncertain) sends its recorded bytes.
            let due = store
                .due_sends(account, now_ms())
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            let recorded = due
                .sends
                .into_iter()
                .find(|send| send.draft == id)
                .and_then(|send| send.prepared);
            let prepared = match recorded {
                Some(prepared) => prepared,
                None => {
                    let blobs = harness
                        .state
                        .mailbox()
                        .blobs()
                        .ok_or("the blob store is not open")?;
                    let message_blob = blocking(move || {
                        blobs
                            .put(b"Subject: A fictional message\r\n\r\nFrom the self-test.\r\n")
                            .map_err(|error| error.to_string())
                    })
                    .await??;
                    Prepared {
                        message_blob,
                        sent_copy_blob: None,
                        size: 60,
                        envelope_from: "work@example.test".into(),
                        envelope_to: vec!["kari@example.test".into(), "ola@example.test".into()],
                    }
                }
            };
            let started = store
                .send_started(id, prepared, now_ms())
                .recv()
                .await
                .map_err(|error| error.to_string())?;
            println!("  # started: {started:?}");
            Ok(())
        }
        "sent" => {
            applied(store.send_done(id, now_ms()).recv().await)?;
            setup::emit(harness, Event::Sent { account, draft: id }).await
        }
        "uncertain" => {
            let reason = match args.get(2).map(String::as_str) {
                None | Some("stopped") => UncertainReason::Stopped,
                Some("no-reply") => UncertainReason::NoReply,
                Some("unreadable") => UncertainReason::UnreadableReply,
                Some(other) => return Err(format!("no reason {other}")),
            };
            applied(store.send_uncertain(id).recv().await)?;
            setup::emit(
                harness,
                Event::SendUncertain {
                    account,
                    draft: id,
                    reason,
                },
            )
            .await
        }
        "failed" => {
            let name = args.get(2).ok_or("failed takes an error")?;
            let (error, failure) = send_error(name)?;
            applied(
                store
                    .send_failed(id, SendFailed::Permanent(failure))
                    .recv()
                    .await,
            )?;
            setup::emit(
                harness,
                Event::SendFailed {
                    account,
                    draft: id,
                    error,
                },
            )
            .await
        }
        other => Err(format!("use start, sent, failed or uncertain, not {other}")),
    }
}

/// Waits up to a second for the app to send `kind` to the engine; other commands are passed
/// over (and printed).
async fn expect(harness: &Harness, kind: &str, field: Option<&str>) -> StepResult {
    let started = Instant::now();
    loop {
        while let Some(command) = setup::next_command()? {
            let matched = command.split_whitespace().next() == Some(kind)
                && field.is_none_or(|field| command.split_whitespace().any(|part| part == field));
            if matched {
                println!("  # {command}");
                return Ok(());
            }
            println!("  # passed over: {command}");
        }
        if started.elapsed() > Duration::from_secs(1) {
            return Err(format!("the app sent no {kind}"));
        }
        glib::timeout_future(Duration::from_millis(20)).await;
        harness.wait_until_settled().await;
    }
}

fn toast_button(harness: &Harness, label: &str) -> Result<gtk::Button, String> {
    let toast = glib::Type::from_name("AdwToastWidget").ok_or("no toast is shown")?;
    let mut found = None;
    super::collect(harness.window()?.window().upcast_ref(), &mut |widget| {
        if found.is_none()
            && let Some(button) = widget.downcast_ref::<gtk::Button>()
            && button.is_mapped()
            && button.ancestor(toast).is_some()
            && button.label().as_deref() == Some(label)
        {
            found = Some(button.clone());
        }
    });
    found.ok_or_else(|| format!("no toast offers {label}"))
}

/// A closed reply draft to `message` with `text` as its body, in the store: what an earlier
/// session's quick reply left.
async fn store_quick_draft(harness: &Harness, message: &str, text: &str) -> StepResult {
    let id = super::conversation::id(message)?;
    let account = harness
        .state
        .mailbox()
        .accounts()
        .into_iter()
        .next()
        .ok_or("there is no account")?;
    let draft = Draft {
        account: account.id,
        from: Address::new(account.name.clone(), account.address.clone()),
        to: vec![Address::new("Tomas Berg", "tomas@example.test")],
        subject: "Re: Cabin trip in November?".into(),
        html: to_html(text),
        message_id: format!("quick.{}@example.test", glib::uuid_string_random()),
        source: Some(DraftSource {
            message: id,
            kind: ReplyKind::Reply,
        }),
        ..Draft::default()
    };
    let saved = store(harness)?
        .save_draft(SaveDraft {
            id: None,
            draft,
            open: false,
            now: now_ms(),
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    println!("  # {saved:?}");
    Ok(())
}

fn stand_in(harness: &Harness, args: &[String]) -> StepResult {
    let stand_in =
        composer::ensure_stand_in(&harness.app).ok_or("the backend isn't the stand-in")?;
    let arg = |index: usize| args.get(index).map(String::as_str).unwrap_or_default();
    match arg(0) {
        "sent" => equal("sent", stand_in.sent().len(), number(arg(1))?),
        "discarded" => equal("discarded", stand_in.discarded().len(), number(arg(1))?),
        "closed" => equal("closed", stand_in.closed().len(), number(arg(1))?),
        "saved-between" => {
            let mark = MARK
                .with(std::cell::Cell::get)
                .ok_or("run stand-in-mark first")?;
            let (low, high) = (number(arg(1))? as u128, number(arg(2))? as u128);
            let since: Vec<u128> = stand_in
                .saves()
                .iter()
                .filter(|(_, at)| *at >= mark)
                .map(|(_, at)| at.duration_since(mark).as_millis())
                .collect();
            println!("  # saves after the mark, ms: {since:?}");
            if since.iter().any(|ms| (low..=high).contains(ms)) {
                Ok(())
            } else {
                Err(format!("no save {low}–{high} ms after the mark"))
            }
        }
        "last-sent" => {
            let (_, sent) = stand_in.sent().pop().ok_or("nothing was sent")?;
            let typed = to_html(arg(1));
            if sent.html.starts_with(&typed) {
                Ok(())
            } else {
                Err(format!(
                    "the sent draft's body doesn't start with the text ({} bytes)",
                    sent.html.len()
                ))
            }
        }
        "quick-draft" => {
            let message = super::conversation::id(arg(1))?;
            let wanted = arg(2);
            let kept = stand_in.saves().iter().rev().map(|(id, _)| *id).find(|id| {
                stand_in.draft(*id).is_some_and(|draft| {
                    draft.source.is_some_and(|source| source.message == message)
                }) || stand_in.discarded().contains(id)
            });
            let Some(id) = kept else {
                return equal("a kept draft", "none", wanted);
            };
            match stand_in.draft(id) {
                None => equal("the kept draft", "none", wanted),
                Some(draft) => {
                    equal(
                        "the kept draft's body",
                        draft.html.as_str(),
                        to_html(wanted).as_str(),
                    )?;
                    if stand_in.closed().contains(&id) {
                        Ok(())
                    } else {
                        Err("the kept draft wasn't closed".to_owned())
                    }
                }
            }
        }
        other => Err(format!(
            "use sent, discarded, closed, last-sent or quick-draft, not {other}"
        )),
    }
}
