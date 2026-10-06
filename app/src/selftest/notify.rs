//! Self-test commands for new-mail notifications (ML-083), with a stand-in notification server
//! on the wrapper's private bus (`server.rs`, never the desktop's) and the engine's `NewMail`
//! played by the fake engine (`engine-attach` first). Message names are the script's own; every
//! sender, subject and text is fictional. Notifications are numbered as the stand-in received
//! them, from 1; `last` is the newest.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `notify-server [plain]` | Starts the stand-in with omarchy-shell's capabilities; `plain` without `body-markup` |
//! | `notify-mail NAME [ACCOUNT:]ROLE SUBJECT [read] [from=NAME] [ago=MINUTES]` | A fictional message in the store, unread unless `read` |
//! | `notify-arrive NAME…` | `Event::NewMail` for the named messages, one per folder |
//! | `notify-click N default\|LABEL` | The stand-in clicks notification N: its default action, or the button labelled LABEL |
//! | `notify-expire N` | Notification N leaves the screen by itself |
//! | `assert-notify-count N` | The stand-in received N notifications |
//! | `assert-notify N FIELD VALUE` | Notification N's `app-name`, `summary`, `body`, `actions` (labels, `default` first), `urgency`, `category`, `desktop-entry` (`app-id`: the run's), `image`, `icon`, `expire`, `target` (the message its buttons name, or `none`), `replaces` (`none`, or the notification it replaced) |
//! | `assert-notify-closed N BOOL` | Whether notification N was closed, by Sealwax or by a click |
//! | `assert-notify-open N` | Notifications not closed |
//! | `assert-notified NAME alone\|counted\|none` | How Sealwax shows the message: its own notification, in the count, or not at all |
//! | `assert-notify-withdrawn NAME` | Sealwax withdrew the message's own notification |
//! | `assert-notify-settings on\|off sender\|count` | `[notifications]` as Sealwax applies it |
//! | `assert-notify-opened NAME [had-window\|new-window]` | The last click selected NAME in the list and the reader painted it (with the window there before the click, or built by it); prints the times |
//! | `assert-message NAME read\|unread\|gone` | The message as the store holds it |

mod server;

use super::{Harness, StepResult, blocking, equal, number, setup};
use crate::notify;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::{Flags, FolderId, MessageId};
use sealwax_engine::api::Event;
use sealwax_infrastructure::ingest::{self, RawMessage};
use server::StandIn;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("notify-server", 0, 1),
    ("notify-mail", 3, 6),
    ("notify-arrive", 1, usize::MAX),
    ("notify-click", 2, 2),
    ("notify-expire", 1, 1),
    ("assert-notify-count", 1, 1),
    ("assert-notify", 3, 3),
    ("assert-notify-closed", 2, 2),
    ("assert-notify-open", 1, 1),
    ("assert-notified", 2, 2),
    ("assert-notify-withdrawn", 1, 1),
    ("assert-notify-settings", 2, 2),
    ("assert-notify-opened", 1, 2),
    ("assert-message", 2, 2),
];

thread_local! {
    static STAND_IN: RefCell<Option<StandIn>> = const { RefCell::new(None) };
    /// The script's messages by name.
    static MESSAGES: RefCell<HashMap<String, (MessageId, FolderId)>> =
        RefCell::new(HashMap::new());
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

fn message(name: &str) -> Result<(MessageId, FolderId), String> {
    MESSAGES
        .with(|messages| messages.borrow().get(name).copied())
        .ok_or_else(|| format!("no message {name}; add it with notify-mail"))
}

fn name_of(id: MessageId) -> Option<String> {
    MESSAGES.with(|messages| {
        messages
            .borrow()
            .iter()
            .find(|(_, (found, _))| *found == id)
            .map(|(name, _)| name.clone())
    })
}

fn received() -> Result<Vec<server::Received>, String> {
    STAND_IN.with(|stand_in| {
        stand_in
            .borrow()
            .as_ref()
            .map(StandIn::received)
            .ok_or_else(|| "start the stand-in with notify-server first".to_owned())
    })
}

/// Notification `which` (1-based, or `last`).
fn nth(which: &str) -> Result<server::Received, String> {
    let all = received()?;
    let index = if which == "last" {
        all.len().checked_sub(1)
    } else {
        number(which)?.checked_sub(1)
    };
    index
        .and_then(|index| all.get(index).cloned())
        .ok_or_else(|| format!("there is no notification {which} ({} received)", all.len()))
}

/// The message a button's action names (`app.archive-message(int64 5)`).
fn target(received: &server::Received) -> Option<MessageId> {
    received.actions.iter().find_map(|(key, _)| {
        let (_, argument) = key.split_once('(')?;
        let argument = argument.strip_suffix(')')?;
        let number = argument.strip_prefix("int64 ").unwrap_or(argument);
        number.trim().parse().ok().map(MessageId)
    })
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "notify-server" => {
            let capabilities: Vec<&str> = match args.first().map(String::as_str) {
                None => server::CAPABILITIES.to_vec(),
                Some("plain") => server::CAPABILITIES
                    .into_iter()
                    .filter(|c| *c != "body-markup" && *c != "body-hyperlinks")
                    .collect(),
                Some(other) => return Err(format!("use plain or nothing, not {other}")),
            };
            let stand_in = StandIn::start(&capabilities)?;
            STAND_IN.with(|slot| slot.replace(Some(stand_in)));
            Ok(())
        }
        "notify-mail" => add_message(harness, args).await,
        "notify-arrive" => arrive(harness, args).await,
        "notify-click" => {
            let notification = nth(arg(0))?;
            let key = if arg(1) == "default" {
                "default".to_owned()
            } else {
                notification
                    .actions
                    .iter()
                    .find(|(_, label)| label == arg(1))
                    .map(|(key, _)| key.clone())
                    .ok_or_else(|| format!("notification {} has no button {}", arg(0), arg(1)))?
            };
            let clicked = STAND_IN.with(|stand_in| {
                stand_in
                    .borrow()
                    .as_ref()
                    .ok_or("no stand-in")?
                    .click(notification.id, &key)
            })?;
            println!(
                "  # clicked {:.0} ms after it arrived",
                clicked.duration_since(notification.at).as_secs_f64() * 1000.0
            );
            glib::timeout_future(std::time::Duration::from_millis(20)).await;
            harness.wait_until_settled().await;
            Ok(())
        }
        "notify-expire" => {
            let notification = nth(arg(0))?;
            STAND_IN.with(|stand_in| {
                stand_in
                    .borrow()
                    .as_ref()
                    .ok_or("no stand-in")?
                    .expire(notification.id)
            })
        }
        "assert-notify-count" => equal("notifications", received()?.len(), number(arg(0))?),
        "assert-notify" => {
            let wanted = match (arg(1), arg(2)) {
                ("desktop-entry", "app-id") => harness.app_id.as_str(),
                _ => arg(2),
            };
            field(arg(0), arg(1), wanted)
        }
        "assert-notify-closed" => equal(
            &format!("notification {} closed", arg(0)),
            nth(arg(0))?.closed.is_some(),
            super::script::boolean(arg(1))?,
        ),
        "assert-notify-open" => equal(
            "open notifications",
            received()?
                .iter()
                .filter(|received| received.closed.is_none())
                .map(|received| received.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            number(arg(0))?,
        ),
        "assert-notified" => {
            let (id, _) = message(arg(0))?;
            let shown = match notify::on_screen().get(&id) {
                Some(true) => "alone",
                Some(false) => "counted",
                None => "none",
            };
            equal(&format!("{} notified", arg(0)), shown, arg(1))
        }
        "assert-notify-withdrawn" => {
            let (id, _) = message(arg(0))?;
            let key = format!("message-{}", id.0);
            let withdrawn = notify::sent()
                .iter()
                .any(|sent| *sent == notify::Sent::Withdrawn(key.clone()));
            equal(&format!("{} withdrawn", arg(0)), withdrawn, true)
        }
        "assert-notify-settings" => {
            let settings = notify::settings();
            let enabled = if settings.enabled { "on" } else { "off" };
            let show = match settings.content {
                sealwax_domain::config::NotificationContent::SenderAndSubject => "sender",
                sealwax_domain::config::NotificationContent::Count => "count",
            };
            equal("[notifications]", (enabled, show), (arg(0), arg(1)))
        }
        "assert-notify-opened" => opened(harness, arg(0), args.get(1).map(String::as_str)),
        "assert-message" => {
            let (id, _) = message(arg(0))?;
            let store = harness
                .state
                .mailbox()
                .shared_store()
                .ok_or("the store is not open")?;
            let found = store
                .message(id)
                .recv()
                .await
                .map_err(|error| error.to_string())?
                .value;
            let state = match found {
                None => "gone",
                Some(detail) if detail.summary.flags.is_seen() => "read",
                Some(_) => "unread",
            };
            equal(&format!("{} in the store", arg(0)), state, arg(1))
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// `assert-notify N FIELD VALUE`.
fn field(which: &str, field: &str, wanted: &str) -> StepResult {
    let notification = nth(which)?;
    let actual = match field {
        "app-name" => notification.app_name.clone(),
        "summary" => notification.summary.clone(),
        "body" => notification.body.clone(),
        "actions" => notification
            .actions
            .iter()
            .map(|(key, label)| if key == "default" { key } else { label })
            .cloned()
            .collect::<Vec<_>>()
            .join(","),
        "urgency" => notification.hint("urgency").unwrap_or("none").to_owned(),
        "category" => notification.hint("category").unwrap_or("none").to_owned(),
        "desktop-entry" => notification
            .hint("desktop-entry")
            .unwrap_or("none")
            .to_owned(),
        "image" => notification.hint("image-path").unwrap_or("none").to_owned(),
        "icon" => notification.app_icon.clone(),
        "expire" => notification.expire_timeout.to_string(),
        "target" => target(&notification)
            .map(|id| name_of(id).unwrap_or_else(|| format!("message {}", id.0)))
            .unwrap_or_else(|| "none".to_owned()),
        "replaces" => {
            if notification.replaces == 0 {
                "none".to_owned()
            } else {
                notification.replaces.to_string()
            }
        }
        other => return Err(format!("no field {other}")),
    };
    equal(
        &format!("notification {which}'s {field}"),
        actual.as_str(),
        wanted,
    )
}

/// `notify-mail NAME [ACCOUNT:]ROLE SUBJECT [read] [from=NAME] [ago=MINUTES]`: through the real
/// ingest path, without the window (it may be closed).
async fn add_message(harness: &Harness, args: &[String]) -> StepResult {
    let name = args[0].clone();
    let folder = super::mail::find_folder(harness, &args[1])?;
    let mut fiction = super::mail::Fiction {
        subject: args[2].clone(),
        from: "Ingrid Solberg".to_owned(),
        text: "Hei! Dette er en melding fra selvtesten.".to_owned(),
        attachment: false,
        reply_to: None,
        html: false,
    };
    let mut flags = Flags::NONE;
    let mut ago = 0;
    for option in &args[3..] {
        match option.as_str() {
            "read" => flags = flags | Flags::SEEN,
            other => match other.split_once('=') {
                Some(("from", from)) => fiction.from = from.to_owned(),
                Some(("ago", minutes)) => ago = number(minutes)? as i64,
                _ => return Err(format!("unknown option {other}")),
            },
        }
    }
    let date = glib::real_time() / 1_000_000 - ago * 60;
    let serial = super::mail::ADDED.with(|added| {
        added.set(added.get() + 1);
        added.get()
    });
    let mailbox = harness.state.mailbox();
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let raw = RawMessage {
        folder: folder.id,
        flags,
        internal_date: Some(date),
        raw: super::mail::raw_message(&fiction, date, serial),
        ..RawMessage::default()
    };
    let prepared = blocking(move || ingest::prepare(&blobs, vec![raw], 1))
        .await?
        .map_err(|error| error.to_string())?;
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .ingest(prepared.messages);
    let ingested = reply.recv().await.map_err(|error| error.to_string())?;
    let id = ingested
        .ids
        .first()
        .copied()
        .flatten()
        .ok_or("the store already held that message")?;
    println!("  # {name} is message {}", id.0);
    MESSAGES.with(|messages| messages.borrow_mut().insert(name, (id, folder.id)));
    harness.wait_until_settled().await;
    Ok(())
}

/// `notify-arrive NAME…`: the engine's `NewMail`, one event per folder in the names' order.
async fn arrive(harness: &Harness, names: &[String]) -> StepResult {
    let mut events: Vec<(FolderId, Vec<MessageId>)> = Vec::new();
    for name in names {
        let (id, folder) = message(name)?;
        match events.iter_mut().find(|(found, _)| *found == folder) {
            Some((_, ids)) => ids.push(id),
            None => events.push((folder, vec![id])),
        }
    }
    for (folder, messages) in events {
        let account = harness
            .state
            .mailbox()
            .folder(folder)
            .ok_or("the message's folder is gone")?
            .account;
        setup::emit(
            harness,
            Event::NewMail {
                account,
                folder,
                messages,
            },
        )
        .await?;
    }
    // The app reads the messages and asks the server's capabilities before it notifies.
    let started = Instant::now();
    while started.elapsed() < std::time::Duration::from_millis(300) {
        glib::timeout_future(std::time::Duration::from_millis(20)).await;
    }
    harness.wait_until_settled().await;
    Ok(())
}

/// `assert-notify-opened NAME [had-window|new-window]`.
fn opened(harness: &Harness, name: &str, window: Option<&str>) -> StepResult {
    let (id, _) = message(name)?;
    let click = notify::clicks()
        .last()
        .copied()
        .ok_or("no notification was clicked")?;
    equal("the clicked message", click.message, id)?;
    let list = harness.state.list().ok_or("there is no list")?;
    equal("the list's selection", list.selected(), Some(id))?;
    let reader = harness.state.reader().ok_or("there is no reader")?;
    equal("the reader's message", reader.message(), Some(id))?;
    let selected = click.selected.ok_or("its row was never selected")?;
    let painted = click.painted.ok_or("it was never painted in the reader")?;
    if let Some(window) = window {
        let wanted = match window {
            "had-window" => true,
            "new-window" => false,
            other => return Err(format!("use had-window or new-window, not {other}")),
        };
        equal(
            "the window was there before the click",
            click.had_window,
            wanted,
        )?;
    }
    let ms = |at: Instant| at.duration_since(click.action).as_secs_f64() * 1000.0;
    println!(
        "  # action → selected {:.1} ms, → painted {:.1} ms (Broadway; had a window: {})",
        ms(selected),
        ms(painted),
        click.had_window
    );
    let shown = harness.window()?.window().is_visible();
    equal("the window is shown", shown, true)
}
