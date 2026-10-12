//! Self-test commands for replying and composing from the main window (M6: ML-068, ML-069,
//! ML-074, ML-078): what `composer::open` was asked for, the cards' buttons, the quick reply
//! and its Send seam, and Make Sealwax the Default Mail App… in a sandboxed XDG home. Message
//! names are the conversation samples' (`trip-6`), or `selected` for the list's selection.
//! Every name, address and text is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `reply-sandbox` | Before the app starts: `HOME`, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` point into the run's scratch directory (`$TMP/home`), so GIO reads and writes the run's own `mimeapps.list` and desktop entries |
//! | `reply-install-entry` | Writes a desktop entry for this run's app-id into the sandbox and waits until GIO lists it |
//! | `reply-click-card MESSAGE reply\|reply-all\|forward\|edit-draft` | A real click on that button of MESSAGE's card |
//! | `assert-compose-requests N` | `composer::open` was asked N times in this run |
//! | `assert-compose-last KIND [ARG] [ARG] [ARG]` | The latest request: `new ACCOUNT\|none`, `reply MESSAGE reply\|reply-all\|forward`, `quick-reply MESSAGE TEXT [reply\|reply-all]`, `server-draft MESSAGE`, `mailto TO` |
//! | `assert-quick-reply FIELD VALUE` | The quick reply on screen: `shown BOOL`, `target MESSAGE`, `label TEXT`, `text TEXT`, `focused BOOL`, `send BOOL` (its Send sensitive) |
//! | `assert-quick-sends N` | The quick reply's Send took N replies |
//! | `assert-quick-send-last MESSAGE TEXT` | The latest one Send took |
//! | `assert-mailto-handler ID\|ours\|not-ours\|none` | The handler GIO reports for `x-scheme-handler/mailto` (`ours`: this run's desktop entry); prints the sandbox's `mimeapps.list` |
//! | `assert-default-app-dialog heading\|body TEXT` | The default-mail-app dialog on screen: its heading, or text its body holds |

use super::script::{Step, boolean};
use super::{Harness, StepResult, equal, number, triage};
use crate::compose_actions;
use crate::composer::{self, ComposeRequest};
use crate::default_handler::{self, MAILTO_TYPE};
use crate::reader::Reader;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::model::MessageId;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// The commands and their argument counts, for the script parser.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("reply-sandbox", 0, 0),
    ("reply-install-entry", 0, 0),
    ("reply-click-card", 2, 2),
    ("assert-compose-requests", 1, 1),
    ("assert-compose-last", 1, 4),
    ("assert-quick-reply", 2, 2),
    ("assert-quick-sends", 1, 1),
    ("assert-quick-send-last", 2, 2),
    ("assert-mailto-handler", 1, 1),
    ("assert-default-app-dialog", 2, 2),
];

const WAIT: Duration = Duration::from_secs(10);

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

/// `reply-sandbox`, before GTK or any thread starts: the home folder and GIO's per-user
/// configuration and data (`mimeapps.list`, `applications/`) are the run's own, under
/// `root/home`.
pub fn before_start(steps: &[Step], root: &Path) -> Result<(), String> {
    if !steps.iter().any(|step| step.command == "reply-sandbox") {
        return Ok(());
    }
    let home = root.join("home");
    let config = home.join(".config");
    let data = home.join(".local/share");
    for dir in [&config, &data.join("applications")] {
        fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    // SAFETY: the self-test calls this before GTK, the Broadway client or any worker starts, so
    // no other thread reads the environment.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_DATA_HOME", &data);
    }
    Ok(())
}

fn reader(harness: &Harness) -> Result<Reader, String> {
    harness
        .state
        .reader()
        .ok_or_else(|| "there is no reader".to_owned())
}

/// A conversation sample's message, or `selected`.
fn message(harness: &Harness, name: &str) -> Result<MessageId, String> {
    if name == "selected" {
        return super::mail::list(harness)?
            .selected()
            .ok_or_else(|| "no message is selected".to_owned());
    }
    super::conversation::id(name)
}

fn name_of(harness: &Harness, id: MessageId) -> String {
    let selected = super::mail::list(harness)
        .ok()
        .and_then(|list| list.selected());
    match super::conversation::name(id) {
        Some(name) => name,
        None if selected == Some(id) => "selected".to_owned(),
        None => id.0.to_string(),
    }
}

/// A request as the assertions name it; the user's text only as its length.
fn describe(harness: &Harness, request: &ComposeRequest) -> String {
    match request {
        ComposeRequest::New { account } => format!("new {account:?}"),
        ComposeRequest::Reply { message, kind } => {
            format!("reply {} {}", name_of(harness, *message), kind_name(*kind))
        }
        ComposeRequest::QuickReply {
            message,
            text,
            kind,
        } => format!(
            "quick-reply {} {} ({} characters)",
            name_of(harness, *message),
            kind_name(*kind),
            text.chars().count()
        ),
        ComposeRequest::Mailto(_) => "mailto".to_owned(),
        ComposeRequest::Draft(draft) => format!("draft {draft}"),
        ComposeRequest::ServerDraft(message) => {
            format!("server-draft {}", name_of(harness, *message))
        }
    }
}

fn kind_name(kind: ReplyKind) -> &'static str {
    match kind {
        ReplyKind::Reply => "reply",
        ReplyKind::ReplyAll => "reply-all",
        ReplyKind::Forward => "forward",
    }
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "reply-sandbox" => {
            let config = glib::user_config_dir();
            if config.starts_with(&harness.root) {
                println!("  # XDG_CONFIG_HOME is {}", config.display());
                Ok(())
            } else {
                Err(format!(
                    "GIO's configuration folder is {}, outside the run's scratch directory",
                    config.display()
                ))
            }
        }
        "reply-install-entry" => install_entry(harness).await,
        "reply-click-card" => click_card(harness, arg(0), arg(1)).await,
        "assert-compose-requests" => equal(
            "compose requests",
            composer::requests().len(),
            number(arg(0))?,
        ),
        "assert-compose-last" => compose_last(harness, args),
        "assert-quick-reply" => quick_reply(harness, arg(0), arg(1)),
        "assert-quick-sends" => equal(
            "quick replies sent",
            compose_actions::quick_sends().len(),
            number(arg(0))?,
        ),
        "assert-quick-send-last" => {
            let (message, text) = compose_actions::quick_sends()
                .pop()
                .ok_or("the quick reply sent nothing")?;
            equal("message answered", message, self::message(harness, arg(0))?)?;
            equal("text sent", text.as_str(), arg(1))
        }
        "assert-mailto-handler" => mailto_handler(harness, arg(0)),
        "assert-default-app-dialog" => {
            let dialog = harness
                .widget("default-mail-app")?
                .downcast::<adw::AlertDialog>()
                .map_err(|_| "default-mail-app isn't an alert dialog".to_owned())?;
            match arg(0) {
                "heading" => equal(
                    "heading",
                    dialog.heading().unwrap_or_default().as_str(),
                    arg(1),
                ),
                "body" if dialog.body().contains(arg(1)) => {
                    println!("  # {}", dialog.body().replace('\n', " "));
                    Ok(())
                }
                "body" => Err(format!("the body lacks {:?}: {}", arg(1), dialog.body())),
                other => Err(format!("use heading or body, not {other}")),
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn compose_last(harness: &Harness, args: &[String]) -> StepResult {
    let requests = composer::requests();
    let last = requests.last().ok_or("nothing was asked of the composer")?;
    let wanted = |index: usize| {
        args.get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("{} needs more arguments", args[0]))
    };
    let mismatch = || {
        Err(format!(
            "the latest request is {}, not {}",
            describe(harness, last),
            args.join(" ")
        ))
    };
    match (args[0].as_str(), last) {
        ("new", ComposeRequest::New { account }) => {
            let name = account.map_or_else(
                || "none".to_owned(),
                |account| {
                    harness
                        .state
                        .mailbox()
                        .accounts()
                        .into_iter()
                        .find(|candidate| candidate.id == account)
                        .map_or_else(|| account.0.to_string(), |found| found.name)
                },
            );
            equal("account", name.as_str(), wanted(1)?)
        }
        ("reply", ComposeRequest::Reply { message, kind }) => {
            equal("message", *message, self::message(harness, wanted(1)?)?)?;
            equal("kind", kind_name(*kind), wanted(2)?)
        }
        (
            "quick-reply",
            ComposeRequest::QuickReply {
                message,
                text,
                kind,
            },
        ) => {
            equal("message", *message, self::message(harness, wanted(1)?)?)?;
            equal("text", text.as_str(), wanted(2)?)?;
            match args.get(3) {
                Some(wanted) => equal("kind", kind_name(*kind), wanted.as_str()),
                None => Ok(()),
            }
        }
        ("server-draft", ComposeRequest::ServerDraft(message)) => {
            equal("message", *message, self::message(harness, wanted(1)?)?)
        }
        ("mailto", ComposeRequest::Mailto(mailto)) => {
            equal("to", mailto.to.join(",").as_str(), wanted(1)?)
        }
        _ => mismatch(),
    }
}

fn quick_reply(harness: &Harness, field: &str, value: &str) -> StepResult {
    let reader = reader(harness)?;
    let report = reader.quick_reply();
    if field == "shown" {
        return equal("a quick reply shown", report.is_some(), boolean(value)?);
    }
    let report = report.ok_or("no quick reply is shown")?;
    match field {
        "target" => equal("answers", report.target, message(harness, value)?),
        "label" => equal("label", report.label.as_str(), value),
        "text" => equal("text", report.text.as_str(), value),
        "focused" => equal("focus in the text", report.focused, boolean(value)?),
        "send" => equal("Send sensitive", report.send_sensitive, boolean(value)?),
        other => Err(format!("no field {other}")),
    }
}

/// The run's desktop entry, so GIO has a Sealwax to make the handler. It starts nothing:
/// `Exec` is `false`.
async fn install_entry(harness: &Harness) -> StepResult {
    let data = glib::user_data_dir();
    if !data.starts_with(&harness.root) {
        return Err("run reply-sandbox first: the data folder isn't the run's own".to_owned());
    }
    let id = default_handler::desktop_id(&harness.app_id);
    let path = data.join("applications").join(&id);
    fs::write(
        &path,
        "[Desktop Entry]\nType=Application\nName=Sealwax (self-test)\nExec=false %u\n\
         NoDisplay=true\nMimeType=x-scheme-handler/mailto;\n",
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    let started = Instant::now();
    loop {
        let app_id = harness.app_id.clone();
        let installed = gtk::gio::spawn_blocking(move || default_handler::probe(&app_id))
            .await
            .map_err(|_| "the probe failed".to_owned())?
            .installed;
        if installed {
            println!("  # {} lists {id}", data.join("applications").display());
            return Ok(());
        }
        if started.elapsed() > WAIT {
            return Err(format!("GIO never listed {id}"));
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}

/// The handler GIO reports, and the sandbox's `mimeapps.list` in the report.
fn mailto_handler(harness: &Harness, wanted: &str) -> StepResult {
    let list = glib::user_config_dir().join("mimeapps.list");
    if !list.starts_with(&harness.root) {
        return Err("run reply-sandbox first: mimeapps.list isn't the run's own".to_owned());
    }
    match fs::read_to_string(&list) {
        Ok(text) => {
            println!("  # {}:", list.display());
            for line in text.lines() {
                println!("  #   {line}");
            }
        }
        Err(_) => println!("  # {} doesn't exist", list.display()),
    }
    let current = default_handler::current().map_or_else(|| "none".to_owned(), |h| h.id);
    let ours = default_handler::desktop_id(&harness.app_id);
    println!("  # {MAILTO_TYPE}: {current}");
    match wanted {
        "ours" => equal("mailto: handler", current, ours),
        "not-ours" if current != ours => Ok(()),
        "not-ours" => Err(format!("the handler is {current}")),
        other => equal("mailto: handler", current.as_str(), other),
    }
}

/// A real click on one of MESSAGE's card buttons.
async fn click_card(harness: &Harness, name: &str, which: &str) -> StepResult {
    let id = message(harness, name)?;
    let reader = reader(harness)?;
    let index = reader
        .state()
        .expanded
        .iter()
        .position(|expanded| *expanded == id)
        .ok_or_else(|| format!("{name} isn't expanded"))?;
    let card = named(&reader.widget(), "reader-card")
        .get(index)
        .cloned()
        .ok_or_else(|| format!("{name}'s card isn't on screen"))?;
    let button = named(&card, &format!("reader-card-{which}"))
        .into_iter()
        .next()
        .and_downcast::<gtk::Button>()
        .ok_or_else(|| format!("{name}'s card has no {which} button"))?;
    if let Some(viewport) = button
        .ancestor(gtk::Viewport::static_type())
        .and_downcast::<gtk::Viewport>()
    {
        viewport.scroll_to(&button, None);
    }
    harness.wait_frames(2).await?;
    triage::click_button(harness, &button).await
}

/// Mapped widgets under `root` named `name`, in order.
fn named(root: &gtk::Widget, name: &str) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    super::collect(root, &mut |widget| {
        if widget.widget_name() == name && widget.is_mapped() {
            found.push(widget.clone());
        }
    });
    found
}
