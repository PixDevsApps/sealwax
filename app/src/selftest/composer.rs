//! Self-test commands for the composer window (M6: ML-067, ML-071, ML-072, ML-079), against the
//! app's own composer on the phase A stand-in backend. Keys and typing go through Broadway as
//! real key presses to the composer window (`broadway.rs`); `composer-insert` types through the
//! editor's helper instead. Toolbar buttons are clicked with `emit_clicked` (Broadway's clicks
//! aim at the main window's surface). Every name, address and message is fictional; the
//! canary listens on 127.0.0.1 only.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `composer-open new\|reply\|reply-all\|forward\|server-draft` | Opens a composer for that request (messages are the stand-in's) |
//! | `composer-open mailto URI`, `quick-reply TEXT`, `draft N` | The same with what the request carries |
//! | `composer-wait` | Waits until the newest composer's body is loaded |
//! | `composer-pick N` | Later commands act on composer N (0 the oldest); the newest by default |
//! | `composer-insert TEXT` | Types TEXT at the caret through the helper (`insertText`) |
//! | `composer-type TEXT` | Types TEXT as key presses, a character at a time |
//! | `composer-key KEY [COUNT]` | Presses KEY (a GTK accelerator) COUNT times |
//! | `composer-focus body\|to\|cc\|bcc\|subject` | Puts the keyboard focus there |
//! | `composer-body HTML` | Replaces the body (as a paste of the editor's own content would) |
//! | `composer-caret start\|end\|TEXT` | Puts the caret there (before TEXT) |
//! | `composer-select TEXT` | Selects TEXT in the body |
//! | `composer-click NAME` | Clicks the button NAME (`composer-bold`, `composer-send`, a chip's remove…) |
//! | `composer-action NAME` | Activates `win.NAME` on the composer window |
//! | `composer-style TAG` | Picks a paragraph style (`p`, `h1`, `h2`, `h3`, `pre`) |
//! | `composer-link URL\|- apply\|remove\|cancel` | Answers the open link dialog |
//! | `composer-discard-answer discard\|cancel` | Answers the open discard dialog |
//! | `composer-clipboard html\|text\|image\|hostile TEXT` | Puts that on the clipboard (`hostile` builds S7's paste with the canary's URLs) |
//! | `composer-canary` | Starts a local HTTP canary on 127.0.0.1 |
//! | `composer-make-file PATH BYTES` | Writes a file of BYTES bytes under `$TMP` |
//! | `composer-attach PATH…` | Attach File… with the chooser answering PATH… |
//! | `composer-drop body\|window PATH…` | Takes PATH… as files dropped over the body or elsewhere |
//! | `composer-drop-image NAME` | Drops a small PNG named NAME inside the page (the helper's drop path) |
//! | `composer-remove-attachment N` | Clicks attachment card N's × |
//! | `composer-suggestion N` | Clicks suggestion N |
//! | `composer-limit BYTES\|none` | The stand-in's submission limit |
//! | `composer-identity LABEL NAME ADDRESS` | An identity every account can send from |
//! | `composer-from N` | Picks sender N on the From chip |
//! | `composer-close` | Closes the composer window as the user would |
//! | `composer-edit-for MS EVERY` | Types a character through the helper every EVERY ms for MS ms |
//! | `composer-measure-attach PATH` | Attaches PATH and reports how long the GTK thread went without a turn |
//! | `composer-typing N` | Types N characters as key presses and reports key → change (Broadway's; informational) |
//! | `composer-screenshot PATH` | Renders the composer window to PATH |
//! | `assert-composer count N` | Open composer windows |
//! | `assert-composer loaded\|spell BOOL` | The body is loaded; spell checking is on |
//! | `assert-composer title\|subject\|from\|style TEXT` | The header's title, the subject, the sender's address, the style menu |
//! | `assert-composer chips FIELD A,B\|none` | The valid recipients of `to`, `cc` or `bcc` |
//! | `assert-composer invalid FIELD N` | Error chips in FIELD |
//! | `assert-composer entry FIELD TEXT` | What FIELD's entry holds |
//! | `assert-composer html-has\|html-lacks TEXT` | The serialised body contains TEXT, or not |
//! | `assert-composer pressed NAME BOOL` | Toolbar button NAME shows as pressed |
//! | `assert-composer enabled ACTION BOOL` | `win.ACTION` is enabled |
//! | `assert-composer shown NAME BOOL` | Widget NAME is on screen |
//! | `assert-composer focus body\|to\|cc\|bcc\|subject` | Where the keyboard focus is |
//! | `assert-composer suggestions N`, `selected N` | Suggestions shown, the selected one |
//! | `assert-composer attachments N`, `attachment N name\|size TEXT` | The footer's cards |
//! | `assert-composer parts N` | Inline images in the draft |
//! | `assert-composer warning TEXT\|none` | The size warning contains TEXT |
//! | `assert-composer saved BOOL` | "Draft saved HH:MM" shows |
//! | `assert-composer saves N` | At least N saves so far |
//! | `assert-composer save-lag MS` | Every edit was followed by a save within MS ms |
//! | `assert-composer sent\|discarded\|closed N` | What the stand-in recorded |
//! | `assert-composer last-sent FIELD TEXT` | The last sent draft's to, cc, bcc, subject, from or html contains TEXT |
//! | `assert-composer ran NAMES\|none` | The commands the window ran since the last check |
//! | `assert-composer canary N`, `refused N`, `remote N` | Canary hits; what the view refused; non-local loads |
//! | `assert-composer cid-served N` | At least N `cid:` images served |
//! | `assert-composer undo\|redo BOOL` | WebKit has something to undo or redo |
//! | `assert-composer toast TEXT` | A toast containing TEXT shows |
//! | `assert-composer dialog link\|discard BOOL` | That dialog is open |

use super::script::boolean;
use super::{Harness, StepResult, broadway, equal, number};
use crate::composer::{self, ComposeRequest, Composer};
use gtk4 as gtk;
use gtk4::glib::translate::IntoGlib;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::compose::ReplyKind;
use sealwax_domain::model::{DraftId, MessageId};
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("composer-open", 1, 2),
    ("composer-wait", 0, 0),
    ("composer-pick", 1, 1),
    ("composer-insert", 1, 1),
    ("composer-type", 1, 1),
    ("composer-key", 1, 2),
    ("composer-focus", 1, 1),
    ("composer-body", 1, 1),
    ("composer-caret", 1, 1),
    ("composer-select", 1, 1),
    ("composer-click", 1, 1),
    ("composer-action", 1, 1),
    ("composer-style", 1, 1),
    ("composer-link", 2, 2),
    ("composer-discard-answer", 1, 1),
    ("composer-clipboard", 1, 2),
    ("composer-canary", 0, 0),
    ("composer-make-file", 2, 2),
    ("composer-attach", 1, usize::MAX),
    ("composer-drop", 2, usize::MAX),
    ("composer-drop-image", 1, 1),
    ("composer-remove-attachment", 1, 1),
    ("composer-suggestion", 1, 1),
    ("composer-limit", 1, 1),
    ("composer-identity", 3, 3),
    ("composer-from", 1, 1),
    ("composer-close", 0, 0),
    ("composer-save-all", 0, 0),
    ("composer-edit-for", 2, 2),
    ("composer-measure-attach", 1, 1),
    ("composer-typing", 1, 1),
    ("composer-screenshot", 1, 1),
    ("assert-composer", 1, 4),
];

const WAIT: Duration = Duration::from_secs(15);

/// A 4×3 PNG, for pasted and dropped images.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x08, 0x02, 0x00, 0x00, 0x00, 0xb4, 0x0d, 0xb1,
    0xfe, 0x00, 0x00, 0x00, 0x15, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x88, 0x19, 0x18, 0xa0, 0x34, 0x03, 0x58, 0x04, 0x00, 0x76, 0x2e, 0x0b, 0xf5, 0x9d, 0xc6,
    0x81, 0x89, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// The window whose key and button releases are counted, and the counter.
type Watched = (glib::WeakRef<gtk::Window>, Rc<Cell<u64>>);

struct Canary {
    port: u16,
    hits: Arc<Mutex<Vec<String>>>,
}

thread_local! {
    static PICKED: Cell<Option<usize>> = const { Cell::new(None) };
    static CANARY: RefCell<Option<Canary>> = const { RefCell::new(None) };
    static RAN_SEEN: Cell<usize> = const { Cell::new(0) };
    static RELEASES: RefCell<Option<Watched>> = const { RefCell::new(None) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

fn composer() -> Result<Composer, String> {
    let composers = composer::composers();
    let picked = PICKED.with(Cell::get);
    let found = match picked {
        Some(index) => composers.get(index).cloned(),
        None => composers.last().cloned(),
    };
    found.ok_or_else(|| "there is no composer window".to_owned())
}

fn stand_in() -> Result<Rc<composer::backend::StandIn>, String> {
    composer::stand_in().ok_or_else(|| "the stand-in backend isn't in use".to_owned())
}

fn setup_stand_in(harness: &Harness) -> Result<Rc<composer::backend::StandIn>, String> {
    composer::ensure_stand_in(&harness.app)
        .ok_or_else(|| "the stand-in backend isn't in use".to_owned())
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "composer-open" => {
            let message = MessageId(1);
            let request = match (arg(0), args.get(1).map(String::as_str)) {
                ("new", None) => ComposeRequest::New { account: None },
                ("reply", None) => ComposeRequest::Reply {
                    message,
                    kind: ReplyKind::Reply,
                },
                ("reply-all", None) => ComposeRequest::Reply {
                    message,
                    kind: ReplyKind::ReplyAll,
                },
                ("forward", None) => ComposeRequest::Reply {
                    message,
                    kind: ReplyKind::Forward,
                },
                ("server-draft", None) => ComposeRequest::ServerDraft(message),
                ("quick-reply", Some(text)) => ComposeRequest::QuickReply {
                    message,
                    text: text.to_owned(),
                },
                ("mailto", Some(uri)) => ComposeRequest::Mailto(
                    sealwax_domain::mailto::parse(uri).map_err(|error| error.to_string())?,
                ),
                ("draft", Some(id)) => ComposeRequest::Draft(DraftId(number(id)? as i64)),
                _ => return Err("composer-open: unknown request".to_owned()),
            };
            PICKED.with(|picked| picked.set(None));
            composer::open(&harness.app, request);
            harness.wait_until_settled().await;
            Ok(())
        }
        "composer-wait" => {
            let started = Instant::now();
            loop {
                if let Ok(composer) = composer()
                    && composer.is_loaded()
                {
                    break;
                }
                if started.elapsed() > WAIT {
                    return Err("the composer's body never loaded".to_owned());
                }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            settle(harness).await;
            Ok(())
        }
        "composer-pick" => {
            let index = number(arg(0))?;
            PICKED.with(|picked| picked.set(Some(index)));
            Ok(())
        }
        "composer-insert" => {
            composer()?.editor().insert_text(arg(0)).await;
            settle(harness).await;
            Ok(())
        }
        "composer-type" => {
            let composer = composer()?;
            let watch = releases(&composer);
            for character in arg(0).chars() {
                let keysym = gdk::unicode_to_keyval(u32::from(character));
                broadway::key(keysym, 0).map_err(|error| error.to_string())?;
            }
            delivered(watch, arg(0).chars().count() as u64).await;
            settle(harness).await;
            Ok(())
        }
        "composer-key" => {
            let composer = composer()?;
            let times = args.get(1).map_or(Ok(1), |count| number(count))?;
            for _ in 0..times {
                let watch = releases(&composer);
                press(arg(0))?;
                delivered(watch, 1).await;
            }
            settle(harness).await;
            Ok(())
        }
        "composer-focus" => {
            let composer = composer()?;
            composer.window().present();
            focus_target(&composer, arg(0))?.grab_focus();
            settle(harness).await;
            Ok(())
        }
        "composer-body" => {
            let composer = composer()?;
            composer.editor().set_content(arg(0)).await;
            settle(harness).await;
            Ok(())
        }
        "composer-caret" => {
            let composer = composer()?;
            let script = match arg(0) {
                "start" => "sealwaxComposer.caretAtStart()".to_owned(),
                "end" => "sealwaxComposer.caretAtEnd()".to_owned(),
                text => format!("sealwaxComposer.caretAt({}, 0)", literal(text)),
            };
            run_true(&composer, &script).await?;
            composer.editor().view().grab_focus();
            settle(harness).await;
            Ok(())
        }
        "composer-select" => {
            let composer = composer()?;
            run_true(
                &composer,
                &format!("sealwaxComposer.selectText({})", literal(arg(0))),
            )
            .await?;
            composer.editor().view().grab_focus();
            settle(harness).await;
            Ok(())
        }
        "composer-click" => {
            let composer = composer()?;
            let widget = composer
                .widget(arg(0))
                .ok_or_else(|| format!("no widget named {}", arg(0)))?;
            if let Some(menu) = widget.downcast_ref::<gtk::MenuButton>() {
                menu.popup();
            } else if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if !button.is_sensitive() {
                    return Err(format!("{} is insensitive", arg(0)));
                }
                button.emit_clicked();
            } else {
                return Err(format!("{} is not a button", arg(0)));
            }
            settle(harness).await;
            Ok(())
        }
        "composer-action" => {
            let composer = composer()?;
            WidgetExt::activate_action(composer.window(), &format!("win.{}", arg(0)), None)
                .map_err(|_| format!("no action win.{}", arg(0)))?;
            settle(harness).await;
            Ok(())
        }
        "composer-style" => {
            let composer = composer()?;
            WidgetExt::activate_action(
                composer.window(),
                "composer.style",
                Some(&arg(0).to_variant()),
            )
            .map_err(|_| "no style action".to_owned())?;
            settle(harness).await;
            Ok(())
        }
        "composer-link" => {
            let dialog = dialog(&composer()?, "composer-link-dialog")?;
            if arg(0) != "-" {
                let entry = dialog
                    .extra_child()
                    .and_downcast::<gtk::Entry>()
                    .ok_or("the link dialog has no entry")?;
                entry.set_text(arg(0));
            }
            respond(&dialog, arg(1))?;
            settle(harness).await;
            Ok(())
        }
        "composer-discard-answer" => {
            let dialog = dialog(&composer()?, "composer-discard-dialog")?;
            respond(&dialog, arg(0))?;
            settle(harness).await;
            Ok(())
        }
        "composer-clipboard" => set_clipboard(arg(0), args.get(1).map(String::as_str)),
        "composer-canary" => start_canary(),
        "composer-make-file" => {
            let path = scratch(harness, arg(0))?;
            let size = number(arg(1))?;
            let bytes: Vec<u8> = (0..size).map(|index| (index * 31 % 251) as u8).collect();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(&path, bytes).map_err(|error| error.to_string())
        }
        "composer-attach" => {
            let composer = composer()?;
            let paths = args
                .iter()
                .map(|path| scratch(harness, path))
                .collect::<Result<Vec<_>, _>>()?;
            composer::answer_chooser(paths);
            WidgetExt::activate_action(composer.window(), "win.attach-file", None)
                .map_err(|_| "no attach action".to_owned())?;
            settle(harness).await;
            Ok(())
        }
        "composer-drop" => {
            let composer = composer()?;
            let paths = args[1..]
                .iter()
                .map(|path| scratch(harness, path))
                .collect::<Result<Vec<_>, _>>()?;
            composer.drop_files(paths, arg(0) == "body");
            settle(harness).await;
            Ok(())
        }
        "composer-drop-image" => {
            let composer = composer()?;
            let data = glib::base64_encode(TINY_PNG);
            run_true(
                &composer,
                &format!(
                    "sealwaxComposer.testDrop({}, 'image/png', {})",
                    literal(arg(0)),
                    literal(&data)
                ),
            )
            .await?;
            settle(harness).await;
            Ok(())
        }
        "composer-remove-attachment" => {
            let composer = composer()?;
            let card = composer
                .tray()
                .cards()
                .get(number(arg(0))?)
                .cloned()
                .ok_or("no such attachment")?;
            let remove = card
                .last_child()
                .and_downcast::<gtk::Button>()
                .ok_or("the card has no remove button")?;
            remove.emit_clicked();
            settle(harness).await;
            Ok(())
        }
        "composer-suggestion" => {
            let composer = composer()?;
            let index = number(arg(0))?;
            let field = ["to", "cc", "bcc"]
                .into_iter()
                .filter_map(|name| composer.field(name))
                .find(|field| field.suggestions_open())
                .ok_or("no suggestions are open")?;
            let row = field
                .suggestion_rows()
                .get(index)
                .cloned()
                .ok_or("no such suggestion")?;
            row.emit_activate();
            settle(harness).await;
            Ok(())
        }
        "composer-limit" => {
            let limit = match arg(0) {
                "none" => None,
                bytes => Some(number(bytes)? as u64),
            };
            setup_stand_in(harness)?.set_limit(limit);
            Ok(())
        }
        "composer-identity" => {
            setup_stand_in(harness)?.add_identity(arg(0), arg(1), arg(2));
            Ok(())
        }
        "composer-from" => {
            composer()?.select_sender(number(arg(0))?);
            settle(harness).await;
            Ok(())
        }
        "composer-close" => {
            composer()?.window().close();
            settle(harness).await;
            Ok(())
        }
        "composer-save-all" => {
            composer::save_all().await;
            settle(harness).await;
            Ok(())
        }
        "composer-edit-for" => {
            let composer = composer()?;
            let total = Duration::from_millis(number(arg(0))? as u64);
            let every = Duration::from_millis(number(arg(1))? as u64);
            let started = Instant::now();
            let mut typed = 0;
            while started.elapsed() < total {
                composer.editor().insert_text("a").await;
                typed += 1;
                glib::timeout_future(every).await;
            }
            println!("  # typed {typed} characters over {} ms", total.as_millis());
            Ok(())
        }
        "composer-measure-attach" => measure_attach(harness, arg(0)).await,
        "composer-typing" => typing(harness, number(arg(0))?).await,
        "composer-screenshot" => {
            let composer = composer()?;
            let path = Path::new(arg(0));
            super::screenshot::capture(composer.window(), path)?;
            println!("  # {}", super::normalise(path).display());
            Ok(())
        }
        "assert-composer" => check(harness, args).await,
        other => Err(format!("unknown command {other}")),
    }
}

async fn settle(harness: &Harness) {
    harness.wait_until_settled().await;
    glib::timeout_future(Duration::from_millis(30)).await;
    harness.wait_until_settled().await;
}

fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

async fn run_true(composer: &Composer, script: &str) -> StepResult {
    let value = composer.editor().eval(script).await?;
    if value.to_boolean() {
        Ok(())
    } else {
        Err(format!("{script} found nothing"))
    }
}

fn scratch(harness: &Harness, path: &str) -> Result<PathBuf, String> {
    let path = super::normalise(Path::new(path));
    if path.starts_with(&harness.root) {
        Ok(path)
    } else {
        Err(format!(
            "{} is outside the scratch directory",
            path.display()
        ))
    }
}

fn focus_target(composer: &Composer, name: &str) -> Result<gtk::Widget, String> {
    Ok(match name {
        "body" => composer.editor().view().clone().upcast(),
        "subject" => composer.subject().clone().upcast(),
        field => composer
            .field(field)
            .ok_or_else(|| format!("no field {field}"))?
            .entry()
            .clone()
            .upcast(),
    })
}

fn focus_name(composer: &Composer) -> String {
    let Some(focus) = gtk::prelude::GtkWindowExt::focus(composer.window()) else {
        return "none".to_owned();
    };
    for name in ["body", "subject", "to", "cc", "bcc"] {
        if let Ok(target) = focus_target(composer, name)
            && (focus == target || focus.is_ancestor(&target))
        {
            return name.to_owned();
        }
    }
    format!("{} {}", focus.type_().name(), focus.widget_name())
}

fn dialog(composer: &Composer, name: &str) -> Result<adw::AlertDialog, String> {
    composer
        .window()
        .visible_dialog()
        .and_downcast::<adw::AlertDialog>()
        .filter(|dialog| dialog.widget_name() == name)
        .ok_or_else(|| format!("{name} is not open"))
}

fn respond(dialog: &adw::AlertDialog, response: &str) -> StepResult {
    if !dialog.has_response(response) {
        return Err(format!("the dialog has no response {response}"));
    }
    if !dialog.is_response_enabled(response) {
        return Err(format!("the dialog's {response} is disabled"));
    }
    dialog.emit_by_name::<()>("response", &[&response]);
    dialog.force_close();
    Ok(())
}

/// Presses `spec` (a GTK accelerator) with the keysym already chosen.
fn press(spec: &str) -> StepResult {
    let (keyval, mods) =
        gtk::accelerator_parse(spec).ok_or_else(|| format!("GTK can't parse the key {spec}"))?;
    let mut state = 0;
    let mut keyval = keyval;
    if mods.contains(gdk::ModifierType::SHIFT_MASK) {
        state |= broadway::SHIFT;
        keyval = keyval.to_upper();
    }
    if mods.contains(gdk::ModifierType::CONTROL_MASK) {
        state |= broadway::CONTROL;
    }
    if mods.contains(gdk::ModifierType::ALT_MASK) {
        state |= broadway::ALT;
    }
    broadway::key(keyval.into_glib(), state).map_err(|error| error.to_string())
}

/// The composer window's key and button releases, counted, and the count now.
fn releases(composer: &Composer) -> (Rc<Cell<u64>>, u64) {
    let window: gtk::Window = composer.window().clone().upcast();
    let counter = RELEASES.with(|releases| {
        let mut releases = releases.borrow_mut();
        match releases.as_ref() {
            Some((watched, counter)) if watched.upgrade().as_ref() == Some(&window) => {
                counter.clone()
            }
            _ => {
                let counter: Rc<Cell<u64>> = Rc::default();
                let legacy = gtk::EventControllerLegacy::new();
                legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
                let count = counter.clone();
                legacy.connect_event(move |_, event| {
                    if matches!(
                        event.event_type(),
                        gdk::EventType::KeyRelease | gdk::EventType::ButtonRelease
                    ) {
                        count.set(count.get() + 1);
                    }
                    glib::Propagation::Proceed
                });
                window.add_controller(legacy);
                releases.replace((window.downgrade(), counter.clone()));
                counter
            }
        }
    });
    let before = counter.get();
    (counter, before)
}

async fn delivered((counter, before): (Rc<Cell<u64>>, u64), count: u64) {
    let started = Instant::now();
    while counter.get() < before + count && started.elapsed() < Duration::from_secs(2) {
        glib::timeout_future(Duration::from_millis(2)).await;
    }
}

fn provider(mime: &str, data: &[u8]) -> gdk::ContentProvider {
    gdk::ContentProvider::for_bytes(mime, &glib::Bytes::from(data))
}

fn set_clipboard(kind: &str, text: Option<&str>) -> StepResult {
    let display = gdk::Display::default().ok_or("no display")?;
    let providers = match kind {
        "html" => vec![
            provider("text/html", text.unwrap_or_default().as_bytes()),
            provider("text/plain;charset=utf-8", b"plain fallback"),
        ],
        "text" => vec![provider(
            "text/plain;charset=utf-8",
            text.unwrap_or_default().as_bytes(),
        )],
        "image" => vec![provider("image/png", TINY_PNG)],
        "hostile" => {
            let port = CANARY
                .with(|canary| canary.borrow().as_ref().map(|canary| canary.port))
                .ok_or("start the canary first (composer-canary)")?;
            vec![
                provider("text/html", hostile_paste(port).as_bytes()),
                provider("text/plain;charset=utf-8", b"plain fallback"),
            ]
        }
        other => return Err(format!("no clipboard kind {other}")),
    };
    let union = gdk::ContentProvider::new_union(&providers);
    display
        .clipboard()
        .set_content(Some(&union))
        .map_err(|error| error.to_string())
}

fn start_canary() -> StepResult {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let log = hits.clone();
    std::thread::Builder::new()
        .name("sealwax-selftest-canary".into())
        .spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..read]);
                let path = head.split_whitespace().nth(1).unwrap_or("?").to_owned();
                if let Ok(mut hits) = log.lock() {
                    hits.push(path);
                }
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            }
        })
        .map_err(|error| error.to_string())?;
    println!("  # canary on 127.0.0.1:{port}");
    CANARY.with(|canary| canary.replace(Some(Canary { port, hits })));
    Ok(())
}

fn canary_hits() -> usize {
    CANARY.with(|canary| {
        canary
            .borrow()
            .as_ref()
            .and_then(|canary| canary.hits.lock().ok().map(|hits| hits.len()))
            .unwrap_or(0)
    })
}

/// S7's hostile clipboard: a Google Docs wrapper, Word's markup, a web page's scripts, frames,
/// forms, SVG, media, styles and remote references, all pointing at the canary.
fn hostile_paste(port: u16) -> String {
    let url = |path: &str| format!("http://127.0.0.1:{port}/paste/{path}");
    let png = glib::base64_encode(TINY_PNG);
    format!(
        r#"<html><head><meta charset="utf-8"><title>clip</title><style>p {{ color: red; }}</style>
<link rel="stylesheet" href="{link}"></head><body>
<!--[if gte mso 9]><xml><o:OfficeDocumentSettings></o:OfficeDocumentSettings></xml><![endif]-->
<b style="font-weight:normal;" id="docs-internal-guid-1234"><p dir="ltr" style="line-height:1.38;margin-top:0pt"><span style="font-size:11pt;font-family:Arial;color:#000000;font-weight:700;">Bold from Docs</span> <span style="font-style:italic;">italic from Docs</span></p></b>
<p class="MsoNormal" style="mso-margin-top-alt:auto">Word paragraph<o:p></o:p></p>
<script>document.title = "pasted-script";</script>
<img src="{img}" onerror="alert(1)" width="10" alt="remote">
<img src="data:image/png;base64,{png}" alt="tiny">
<img src="cid:someone-elses-part@example.test" alt="foreign">
<p><a href="javascript:alert(1)">bad link</a> and <a href="https://example.org/ok" onclick="steal()" target="_blank" rel="noopener">good link</a></p>
<iframe src="{iframe}"></iframe>
<form action="{form}"><input name="q"><button>go</button></form>
<div style="background:url({background})" onmouseover="steal()">styled div</div>
<svg onload="alert(1)"><circle r="5"/></svg>
<table background="{table}"><tr><td colspan="2" style="border:1px solid">cell</td></tr></table>
<ul><li>list item</li></ul>
<p><span style="text-decoration: underline line-through">under-strike</span> <font face="Comic Sans MS" color="red">font tag</font></p>
<video src="{video}" poster="{poster}"></video>
</body></html>"#,
        link = url("stylesheet.css"),
        img = url("img.png"),
        iframe = url("iframe"),
        form = url("form"),
        background = url("background.png"),
        table = url("table.png"),
        video = url("video.mp4"),
        poster = url("poster.png"),
    )
}

/// `composer-measure-attach PATH`: attaches PATH and records the longest time the GTK thread
/// went without running a 1 ms timer, until the card shows the file's size.
async fn measure_attach(harness: &Harness, path: &str) -> StepResult {
    let composer = composer()?;
    let path = scratch(harness, path)?;
    let size = std::fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .len();
    let before = composer.tray().parts().len();
    let gap = Rc::new(Cell::new(Duration::ZERO));
    let last = Rc::new(Cell::new(Instant::now()));
    let ticker = {
        let gap = gap.clone();
        let last = last.clone();
        glib::timeout_add_local(Duration::from_millis(1), move || {
            let now = Instant::now();
            gap.set(gap.get().max(now - last.get()));
            last.set(now);
            glib::ControlFlow::Continue
        })
    };
    let started = Instant::now();
    composer::answer_chooser(vec![path]);
    WidgetExt::activate_action(composer.window(), "win.attach-file", None)
        .map_err(|_| "no attach action".to_owned())?;
    let mut done = false;
    while started.elapsed() < WAIT {
        if composer.tray().parts().len() > before {
            done = true;
            break;
        }
        glib::timeout_future(Duration::from_millis(1)).await;
    }
    let took = started.elapsed();
    ticker.remove();
    if !done {
        return Err("the attachment never arrived".to_owned());
    }
    println!(
        "  # attached {} in {:.1} ms; the longest the GTK thread went without a turn: {:.1} ms",
        glib::format_size(size),
        took.as_secs_f64() * 1000.0,
        gap.get().as_secs_f64() * 1000.0
    );
    if gap.get() > Duration::from_millis(250) {
        return Err(format!(
            "the window froze for {:.0} ms while attaching",
            gap.get().as_secs_f64() * 1000.0
        ));
    }
    settle(harness).await;
    Ok(())
}

/// `composer-typing N`: N key presses into the body, each timed from the press to the
/// helper's input report. Broadway's delivery, not a real display's: informational.
async fn typing(harness: &Harness, count: usize) -> StepResult {
    let composer = composer()?;
    composer.editor().view().grab_focus();
    settle(harness).await;
    let mut times = Vec::new();
    for index in 0..count {
        let before = composer.edits().len();
        let started = Instant::now();
        let character = ['e', 'æ', 'ø', 'å', 'n'][index % 5];
        broadway::key(gdk::unicode_to_keyval(u32::from(character)), 0)
            .map_err(|error| error.to_string())?;
        while composer.edits().len() == before && started.elapsed() < Duration::from_secs(2) {
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    times.sort_by(f64::total_cmp);
    let at = |fraction: f64| times[((times.len() as f64 - 1.0) * fraction).round() as usize];
    println!(
        "  # key → input report through Broadway (informational): n={} p50 {:.1} ms p95 {:.1} ms max {:.1} ms",
        times.len(),
        at(0.5),
        at(0.95),
        at(1.0)
    );
    Ok(())
}

async fn check(harness: &Harness, args: &[String]) -> StepResult {
    let arg = |index: usize| args.get(index).map(String::as_str).unwrap_or_default();
    let what = arg(0);
    if what == "count" {
        return equal(
            "composer windows",
            composer::composers().len(),
            number(arg(1))?,
        );
    }
    if matches!(
        what,
        "sent" | "discarded" | "closed" | "saves" | "save-lag" | "last-sent" | "stored"
    ) {
        return check_backend(what, &args[1..]);
    }
    if what == "canary" {
        return equal("canary hits", canary_hits(), number(arg(1))?);
    }
    let composer = composer()?;
    match what {
        "loaded" => equal("loaded", composer.is_loaded(), boolean(arg(1))?),
        "theme" => {
            let theme = harness
                .state
                .theme()
                .ok_or("the theme follower isn't running")?
                .theme();
            let background =
                webkit6::prelude::WebViewExt::background_color(composer.editor().view());
            let wanted = theme.palette.background;
            let got = (
                (background.red() * 255.0).round() as u8,
                (background.green() * 255.0).round() as u8,
                (background.blue() * 255.0).round() as u8,
            );
            println!("  # the body's background {got:?}, the theme's {wanted}");
            equal("the body's background", got, (wanted.r, wanted.g, wanted.b))?;
            let page = composer
                .editor()
                .eval("getComputedStyle(document.documentElement).backgroundColor")
                .await?
                .to_str()
                .to_string();
            let css = format!("rgb({}, {}, {})", wanted.r, wanted.g, wanted.b);
            equal("the page's background", page.as_str(), css.as_str())
        }
        "spell" => {
            let languages = composer.editor().engine().spell_languages();
            println!("  # spell-checking languages WebKit accepted: {languages:?}");
            equal("spell checking", !languages.is_empty(), boolean(arg(1))?)
        }
        "eval" => {
            let value = composer.editor().eval(arg(1)).await?;
            equal(arg(1), value.to_str().as_str(), arg(2))
        }
        "title" => equal(
            "title",
            composer
                .widget("composer-title")
                .and_downcast::<gtk::Label>()
                .map(|label| label.label().to_string())
                .unwrap_or_else(|| composer.window().title().unwrap_or_default().to_string())
                .as_str(),
            arg(1),
        ),
        "subject" => equal("subject", composer.subject().text().as_str(), arg(1)),
        "from" => equal(
            "from",
            composer
                .sender()
                .map(|sender| sender.address.email)
                .unwrap_or_default()
                .as_str(),
            arg(1),
        ),
        "style" => equal("style", composer.style_label().as_str(), arg(1)),
        "chips" => {
            let field = composer.field(arg(1)).ok_or("no such field")?;
            let emails: Vec<String> = field
                .chip_addresses()
                .into_iter()
                .map(|address| address.email)
                .collect();
            let shown = if emails.is_empty() {
                "none".to_owned()
            } else {
                emails.join(",")
            };
            equal("recipients", shown.as_str(), arg(2))
        }
        "invalid" => {
            let field = composer.field(arg(1)).ok_or("no such field")?;
            equal("error chips", field.invalid(), number(arg(2))?)
        }
        "entry" => {
            let field = composer.field(arg(1)).ok_or("no such field")?;
            equal("entry", field.entry().text().as_str(), arg(2))
        }
        "html-has" | "html-lacks" => {
            let html = composer
                .draft()
                .await
                .map(|draft| draft.html)
                .unwrap_or_default();
            let wanted = what == "html-has";
            if html.contains(arg(1)) == wanted {
                Ok(())
            } else {
                Err(format!(
                    "the body {} {:?}: {}",
                    if wanted { "lacks" } else { "has" },
                    arg(1),
                    short(&html)
                ))
            }
        }
        "pressed" => {
            let widget = composer
                .widget(arg(1))
                .ok_or_else(|| format!("no widget named {}", arg(1)))?;
            equal(
                &format!("{} pressed", arg(1)),
                widget.has_css_class("sw-pressed"),
                boolean(arg(2))?,
            )
        }
        "enabled" => {
            let id = ActionId::ALL
                .into_iter()
                .find(|id| id.name() == arg(1))
                .ok_or_else(|| format!("no command {}", arg(1)))?;
            equal(
                &format!("{} enabled", arg(1)),
                composer.actions().is_enabled(id),
                boolean(arg(2))?,
            )
        }
        "shown" => {
            let shown = composer
                .widget(arg(1))
                .is_some_and(|widget| widget.is_visible() && widget.is_mapped());
            equal(&format!("{} shown", arg(1)), shown, boolean(arg(2))?)
        }
        "focus" => equal("focus", focus_name(&composer).as_str(), arg(1)),
        "suggestions" => {
            let count = ["to", "cc", "bcc"]
                .into_iter()
                .filter_map(|name| composer.field(name))
                .find(|field| field.suggestions_open())
                .map_or(0, |field| field.suggestions().len());
            equal("suggestions", count, number(arg(1))?)
        }
        "selected" => {
            let selected = ["to", "cc", "bcc"]
                .into_iter()
                .filter_map(|name| composer.field(name))
                .find(|field| field.suggestions_open())
                .and_then(|field| field.selected_suggestion());
            equal("selected suggestion", selected, Some(number(arg(1))?))
        }
        "attachments" => equal(
            "attachments",
            composer.tray().cards().len(),
            number(arg(1))?,
        ),
        "attachment" => {
            let index = number(arg(1))?;
            let parts = composer.tray().parts();
            let part = parts.get(index).ok_or("no such attachment")?;
            match arg(2) {
                value if value.starts_with("name=") => {
                    equal("name", part.name.as_str(), &value[5..])
                }
                value if value.starts_with("size=") => equal(
                    "size",
                    part.size,
                    value[5..].parse::<u64>().map_err(|e| e.to_string())?,
                ),
                value if value.starts_with("type=") => {
                    equal("type", part.mime_type.as_str(), &value[5..])
                }
                other => Err(format!("use name=, size= or type=, not {other}")),
            }
        }
        "parts" => {
            let parts = composer
                .draft()
                .await
                .map(|draft| {
                    draft
                        .parts
                        .iter()
                        .filter(|part| part.content_id.is_some())
                        .count()
                })
                .unwrap_or(0);
            equal("inline images", parts, number(arg(1))?)
        }
        "warning" => match (composer.warning(), arg(1)) {
            (None, "none") => Ok(()),
            (Some(warning), wanted) if wanted != "none" && warning.contains(wanted) => {
                println!("  # {warning}");
                Ok(())
            }
            (warning, wanted) => Err(format!("warning: got {warning:?}, wanted {wanted:?}")),
        },
        "saved" => equal(
            "Draft saved shows",
            composer
                .saved_label()
                .is_some_and(|label| label.starts_with("Draft saved ")),
            boolean(arg(1))?,
        ),
        "ran" => {
            let ran = composer.ran();
            let seen = RAN_SEEN.with(Cell::get).min(ran.len());
            RAN_SEEN.with(|cell| cell.set(ran.len()));
            let names: Vec<&str> = ran[seen..].iter().map(|id| id.name()).collect();
            let shown = if names.is_empty() {
                "none".to_owned()
            } else {
                names.join(",")
            };
            equal("commands run", shown.as_str(), arg(1))
        }
        "refused" | "remote" => {
            let (refused, remote) = composer.editor().refusals();
            let value = if what == "refused" { refused } else { remote };
            equal(what, value as usize, number(arg(1))?)
        }
        "cid-served" => {
            let (served, refused) = composer.editor().engine().cid_counts();
            println!("  # cid: served {served}, refused {refused}");
            if served as usize >= number(arg(1))? {
                Ok(())
            } else {
                Err(format!("{served} cid: images served"))
            }
        }
        "undo" | "redo" => {
            let (undo, redo) = composer.editor().history();
            equal(
                what,
                if what == "undo" { undo } else { redo },
                boolean(arg(1))?,
            )
        }
        "toast" => {
            let mut texts = Vec::new();
            if let Some(toast) = glib::Type::from_name("AdwToastWidget") {
                super::collect(composer.toasts().upcast_ref(), &mut |widget| {
                    if widget.type_() == toast && widget.is_mapped() {
                        super::collect(widget, &mut |inner| {
                            if let Some(label) = inner.downcast_ref::<gtk::Label>() {
                                texts.push(label.label().to_string());
                            }
                        });
                    }
                });
            }
            if texts.iter().any(|text| text.contains(arg(1))) {
                Ok(())
            } else {
                Err(format!("toasts shown: {texts:?}"))
            }
        }
        "dialog" => {
            let name = format!("composer-{}-dialog", arg(1));
            equal(
                &format!("{name} open"),
                dialog(&composer, &name).is_ok(),
                boolean(arg(2))?,
            )
        }
        other => Err(format!("unknown composer check {other}")),
    }
}

fn check_backend(what: &str, args: &[String]) -> StepResult {
    let stand_in = stand_in()?;
    let arg = |index: usize| args.get(index).map(String::as_str).unwrap_or_default();
    match what {
        "sent" => equal("sent", stand_in.sent().len(), number(arg(0))?),
        "discarded" => equal("discarded", stand_in.discarded().len(), number(arg(0))?),
        "closed" => equal("closed", stand_in.closed().len(), number(arg(0))?),
        "saves" => {
            let saves = stand_in.saves().len();
            println!("  # {saves} saves");
            if saves >= number(arg(0))? {
                Ok(())
            } else {
                Err(format!("only {saves} saves"))
            }
        }
        "save-lag" => {
            let composer = composer()?;
            let id = composer.id().ok_or("the draft has no id")?;
            let saves: Vec<Instant> = stand_in
                .saves()
                .into_iter()
                .filter(|(draft, _)| *draft == id)
                .map(|(_, at)| at)
                .collect();
            let limit = Duration::from_millis(number(arg(0))? as u64);
            let mut worst = Duration::ZERO;
            for edit in composer.edits() {
                let Some(save) = saves.iter().find(|save| **save >= edit) else {
                    return Err("an edit was never saved".to_owned());
                };
                worst = worst.max(*save - edit);
            }
            println!(
                "  # {} edits, {} saves; the longest an edit waited for a save: {:.0} ms",
                composer.edits().len(),
                saves.len(),
                worst.as_secs_f64() * 1000.0
            );
            if worst <= limit {
                Ok(())
            } else {
                Err(format!("an edit waited {} ms", worst.as_millis()))
            }
        }
        "last-sent" | "stored" => {
            let (field, wanted, draft) = if what == "stored" {
                let id = DraftId(number(arg(0))? as i64);
                let draft = stand_in
                    .draft(id)
                    .ok_or_else(|| format!("the stand-in keeps no draft {}", id.0))?;
                (arg(1), arg(2), draft)
            } else {
                let sent = stand_in.sent();
                let (_, draft) = sent.last().cloned().ok_or("nothing was sent")?;
                (arg(0), arg(1), draft)
            };
            let join = |list: &[sealwax_domain::model::Address]| {
                list.iter()
                    .map(|address| address.email.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let value = match field {
                "to" => join(&draft.to),
                "cc" => join(&draft.cc),
                "bcc" => join(&draft.bcc),
                "subject" => draft.subject.clone(),
                "from" => draft.from.email.clone(),
                "html" => draft.html.clone(),
                "parts" => draft.parts.len().to_string(),
                other => return Err(format!("no field {other}")),
            };
            if value.contains(wanted) {
                Ok(())
            } else {
                Err(format!("{field}: {:?} lacks {wanted:?}", short(&value)))
            }
        }
        _ => Err(format!("unknown composer check {what}")),
    }
}

fn short(text: &str) -> String {
    let mut out: String = text.chars().take(600).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}
