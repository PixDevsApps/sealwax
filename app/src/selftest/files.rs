//! Self-test commands for files and remote content (M5: ML-058, ML-061, ML-063), against the
//! app's own reader and store. Samples (`reader::samples`, the files samples included) go into
//! the first account's Inbox whole, or as their text parts (`files-add-text`, as a large
//! message is stored), and the fake engine (`setup.rs`) answers `FetchPart` and
//! `FetchOriginal` with the sample's own bytes. The reader's hooks take the would-be launches
//! and answer the file choosers with scratch paths under `$TMP`; nothing reaches the desktop.
//! Buttons take real clicks through Broadway (docs/m4/triage-app.md). Every name, address and
//! message is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `files-add NAME…` | Stores the named samples whole in the first account's Inbox, the first one newest |
//! | `files-add-text NAME` | Stores a sample as a large message's text parts: its attachments empty, their sections recorded, `BodyState::Text` |
//! | `files-hooks` | Opens attachments under `$TMP/files-open` and records them; Save answers `$TMP/files-saved/<name>`, Download all `$TMP/files-folder` |
//! | `files-answer save\|folder PATH\|cancel` | The next answers of the Save or the folder chooser |
//! | `files-click N open\|save\|cancel\|retry` | A real click on card N's button (scrolled into view first) |
//! | `files-click download-all\|show-images\|always\|hide\|original-save\|original-save-rest\|original-retry\|original-close` | A real click on that button of the shown message or the Show Original dialog |
//! | `files-print PATH` | Print through the registry, into a PDF at PATH; waits until it is written |
//! | `assert-files message NAME` | The reader shows the sample NAME |
//! | `assert-files cards N` | The shown message has N attachment cards |
//! | `assert-files card N name\|size\|danger\|open\|busy\|failure\|warning VALUE` | What card N shows (`failure`, `warning`: its text contains VALUE; `failure none`) |
//! | `assert-files opened N` | N attachments were handed to the opener |
//! | `assert-files opened-file NAME SAMPLE INDEX` | The last one: a 0600 file named NAME in a 0700 folder holding the sample's attachment INDEX |
//! | `assert-files saved PATH SAMPLE INDEX` | PATH holds the sample's attachment INDEX |
//! | `assert-files eml PATH SAMPLE` | PATH holds the sample's message byte for byte |
//! | `assert-files folder N` | The Download all folder holds N files |
//! | `assert-files missing PATH` | Nothing was written at PATH |
//! | `assert-files banner\|note BOOL` | The shown message's banner, or its "always shown" line |
//! | `assert-files remote-allowed BOOL` | The shown HTML body loads remote images |
//! | `assert-files sender ADDRESS BOOL` | The reader's list holds ADDRESS |
//! | `assert-files stored-sender ADDRESS BOOL` | The store's list holds ADDRESS |
//! | `assert-files toggle NAME BOOL` | The window action NAME's state (a toggle) |
//! | `assert-files original open\|cut BOOL`, `assert-files original contains\|lacks\|status TEXT` | The Show Original dialog |
//! | `assert-files pdf PATH TEXT` | PATH is a PDF with at least one page whose text (`pdftotext`) contains TEXT |

use super::script::boolean;
use super::{Harness, StepResult, blocking, equal, number};
use crate::reader::{FileChoice, Reader, original_dialog, samples};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::message;
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{BodyState, BodyUpgrade, NewMessage};
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("files-add", 1, usize::MAX),
    ("files-add-text", 1, 1),
    ("files-hooks", 0, 0),
    ("files-answer", 2, 2),
    ("files-click", 1, 2),
    ("files-print", 1, 1),
    ("assert-files", 2, 4),
];

const WAIT: Duration = Duration::from_secs(15);

/// A sample the script stored, with its whole message.
struct Added {
    name: String,
    id: MessageId,
    raw: Vec<u8>,
}

#[derive(Default)]
struct Hooks {
    opened: Rc<RefCell<Vec<PathBuf>>>,
    save: Rc<RefCell<Option<Option<PathBuf>>>>,
    folder: Rc<RefCell<Option<Option<PathBuf>>>>,
}

thread_local! {
    static ADDED: RefCell<Vec<Added>> = const { RefCell::new(Vec::new()) };
    static HOOKS: RefCell<Option<Rc<Hooks>>> = const { RefCell::new(None) };
}

pub fn handles(command: &str) -> bool {
    command.starts_with("files-") || command == "assert-files"
}

/// The bytes of part `part` of a message the script stored, for the fake engine's
/// `part-fetched ok`.
pub fn part_bytes(message: MessageId, part: u32) -> Option<Vec<u8>> {
    ADDED.with(|added| {
        added
            .borrow()
            .iter()
            .find(|added| added.id == message)
            .and_then(|added| message::part(&added.raw, part))
            .map(|part| part.data)
    })
}

/// A message the script stored, whole, for the fake engine's `original-fetched ok`.
pub fn original_bytes(message: MessageId) -> Option<Vec<u8>> {
    ADDED.with(|added| {
        added
            .borrow()
            .iter()
            .find(|added| added.id == message)
            .map(|added| added.raw.clone())
    })
}

fn sample_raw(name: &str, date: i64) -> Result<Vec<u8>, String> {
    samples::files_draft(name, date)
        .map(|draft| draft.render())
        .ok_or_else(|| format!("no sample {name}"))
}

fn added_raw(name: &str) -> Result<Vec<u8>, String> {
    ADDED
        .with(|added| {
            added
                .borrow()
                .iter()
                .find(|added| added.name == name)
                .map(|added| added.raw.clone())
        })
        .ok_or_else(|| format!("files-add {name} first"))
}

/// The decoded bytes of attachment `index` (0 for the first) of a stored sample.
fn attachment_bytes(name: &str, index: &str) -> Result<Vec<u8>, String> {
    let raw = added_raw(name)?;
    let index = number(index)?;
    let parsed = message::parse(&raw);
    let attachment = parsed
        .attachments
        .get(index)
        .ok_or_else(|| format!("{name} has no attachment {index}"))?;
    message::part(&raw, attachment.part)
        .map(|part| part.data)
        .ok_or_else(|| format!("{name}'s attachment {index} isn't readable"))
}

fn reader(harness: &Harness) -> Result<Reader, String> {
    harness
        .state
        .reader()
        .ok_or_else(|| "the reader isn't installed".to_owned())
}

fn hooks() -> Result<Rc<Hooks>, String> {
    HOOKS
        .with(|hooks| hooks.borrow().clone())
        .ok_or_else(|| "run files-hooks first".to_owned())
}

fn scratch(harness: &Harness, name: &str) -> PathBuf {
    harness.root.join(name)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "files-add" => add(harness, args).await,
        "files-add-text" => add_text(harness, arg(0)).await,
        "files-hooks" => install_hooks(harness),
        "files-answer" => {
            let answer = match arg(1) {
                "cancel" => None,
                path => Some(PathBuf::from(path)),
            };
            let hooks = hooks()?;
            match arg(0) {
                "save" => hooks.save.replace(Some(answer)),
                "folder" => hooks.folder.replace(Some(answer)),
                other => return Err(format!("use save or folder, not {other}")),
            };
            Ok(())
        }
        "files-click" => click(harness, args).await,
        "files-print" => print(harness, Path::new(arg(0))).await,
        "assert-files" => check(harness, args).await,
        other => Err(format!("unknown command {other}")),
    }
}

/// `files-add NAME…`: whole messages through `ingest_raw`, the first newest.
async fn add(harness: &Harness, names: &[String]) -> StepResult {
    let mailbox = harness.state.mailbox();
    let folder = mailbox
        .folders()
        .into_iter()
        .find(|folder| folder.role == FolderRole::Inbox)
        .ok_or("no Inbox; run mail-account first")?;
    let store = mailbox.shared_store().ok_or("the store is not open")?;
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let now = glib::real_time() / 1_000_000;
    let mut raws = Vec::new();
    for (index, name) in names.iter().enumerate() {
        raws.push(sample_raw(name, now - 60 * index as i64)?);
    }
    let messages: Vec<RawMessage> = raws
        .iter()
        .map(|raw| RawMessage {
            folder: folder.id,
            raw: raw.clone(),
            ..RawMessage::default()
        })
        .collect();
    let ingested = blocking(move || ingest::ingest_raw(&store, &blobs, messages))
        .await?
        .map_err(|error| error.to_string())?;
    for ((name, raw), id) in names.iter().zip(raws).zip(ingested.ids) {
        let id = id.ok_or_else(|| format!("the store already held {name}"))?;
        ADDED.with(|added| {
            added.borrow_mut().push(Added {
                name: name.clone(),
                id,
                raw,
            });
        });
    }
    super::mail::settled(harness).await
}

/// `files-add-text NAME`: what a download of a large message leaves: the headers, then the
/// message rebuilt with its attachments empty, its sections recorded and the whole message's
/// sizes, `BodyState::Text`.
async fn add_text(harness: &Harness, name: &str) -> StepResult {
    let mailbox = harness.state.mailbox();
    let folder = mailbox
        .folders()
        .into_iter()
        .find(|folder| folder.role == FolderRole::Inbox)
        .ok_or("no Inbox; run mail-account first")?;
    let now = glib::real_time() / 1_000_000;
    let mut draft = samples::files_draft(name, now).ok_or_else(|| format!("no sample {name}"))?;
    let raw = draft.render();
    for attachment in &mut draft.attachments {
        attachment.data.clear();
    }
    let rebuilt = draft.render();
    let parsed = message::parse(&raw);
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .ingest(vec![NewMessage {
            folder: folder.id,
            date: now,
            headers: parsed.headers.clone(),
            snippet: parsed.snippet.clone(),
            body_kind: parsed.body_kind,
            has_attachments: parsed.has_attachments(),
            size: raw.len() as u64,
            body_state: BodyState::Partial,
            search_text: parsed.body_text.clone(),
            ..NewMessage::default()
        }]);
    let id = reply
        .recv()
        .await
        .map_err(|error| error.to_string())?
        .ids
        .pop()
        .flatten()
        .ok_or_else(|| format!("the store already held {name}"))?;
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .body_targets(vec![id]);
    let target = reply
        .recv()
        .await
        .map_err(|error| error.to_string())?
        .pop()
        .ok_or("the row is gone")?;
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let for_worker = rebuilt.clone();
    let key = blocking(move || blobs.put(&for_worker))
        .await?
        .map_err(|error| error.to_string())?;
    let sections = sealwax_engine::imap::bodies::part_sections(&rebuilt);
    let rebuilt_parsed = message::parse(&rebuilt);
    let upgrade = BodyUpgrade {
        message: id,
        uid: target.uid,
        blob: key,
        blob_size: rebuilt.len() as u64,
        body_state: BodyState::Text,
        body_kind: rebuilt_parsed.body_kind,
        has_attachments: true,
        snippet: rebuilt_parsed.snippet.clone(),
        text_parts: rebuilt_parsed.text_parts.clone(),
        html_parts: rebuilt_parsed.html_parts.clone(),
        sections: parsed
            .attachments
            .iter()
            .filter_map(|attachment| {
                sections
                    .get(&attachment.part)
                    .map(|section| (attachment.part, section.clone()))
            })
            .collect(),
        attachments: parsed.attachments.clone(),
        search_text: rebuilt_parsed.body_text,
        used: now,
    };
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .upgrade_body(upgrade);
    let upgraded = reply.recv().await.map_err(|error| error.to_string())?;
    equal("the row was upgraded", upgraded.found, true)?;
    ADDED.with(|added| {
        added.borrow_mut().push(Added {
            name: name.to_owned(),
            id,
            raw,
        });
    });
    super::mail::settled(harness).await
}

/// `files-hooks`: the opener and the choosers answer with scratch paths.
fn install_hooks(harness: &Harness) -> StepResult {
    let reader = reader(harness)?;
    let hooks = Rc::new(Hooks::default());
    let opened = hooks.opened.clone();
    reader.set_file_opener(&scratch(harness, "files-open"), move |path| {
        opened.borrow_mut().push(path.to_owned());
    });
    let saved = scratch(harness, "files-saved");
    let folder = scratch(harness, "files-folder");
    for dir in [&saved, &folder] {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let save = hooks.save.clone();
    let pick = hooks.folder.clone();
    reader.set_file_chooser(move |choice| match choice {
        FileChoice::Save { suggested } => save
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Some(saved.join(suggested))),
        FileChoice::Folder => pick
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Some(folder.clone())),
    });
    HOOKS.with(|current| current.replace(Some(hooks)));
    Ok(())
}

/// Brings `widget` into the reader column's view, then clicks it for real.
async fn click_widget(harness: &Harness, widget: &gtk::Widget) -> StepResult {
    if let Some(viewport) = widget
        .ancestor(gtk::Viewport::static_type())
        .and_downcast::<gtk::Viewport>()
    {
        viewport.scroll_to(widget, None);
        harness.wait_frames(2).await?;
    }
    let button = widget
        .downcast_ref::<gtk::Button>()
        .ok_or_else(|| format!("{} isn't a button", widget.widget_name()))?;
    super::triage::click_button(harness, button).await
}

/// The first widget named `name` under `root`.
fn find(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(next) = child {
        if let Some(found) = find(&next, name) {
            return Some(found);
        }
        child = next.next_sibling();
    }
    None
}

async fn click(harness: &Harness, args: &[String]) -> StepResult {
    let reader = reader(harness)?;
    let widget = match (args[0].as_str(), args.get(1).map(String::as_str)) {
        (index, Some(which)) => {
            let name = match which {
                "open" => "reader-attachment-open",
                "save" => "reader-attachment-save",
                "cancel" => "reader-attachment-cancel",
                "retry" => "reader-attachment-retry",
                other => return Err(format!("use open, save, cancel or retry, not {other}")),
            };
            reader
                .attachment_button(number(index)?, name)
                .ok_or_else(|| format!("card {index} has no {which} button"))?
        }
        (which, None) if which.starts_with("original-") => {
            let open = original_dialog().ok_or("Show Original isn't open")?;
            match which {
                "original-save" => open.save.clone().upcast(),
                "original-save-rest" => open.save_rest.clone().upcast(),
                "original-retry" => open.retry.clone().upcast(),
                "original-close" => {
                    adw::prelude::AdwDialogExt::close(&open.dialog);
                    harness.wait_until_settled().await;
                    return Ok(());
                }
                other => return Err(format!("no button {other}")),
            }
        }
        (which, None) => {
            let name = match which {
                "download-all" => "reader-download-all",
                "show-images" => "reader-show-images",
                "always" => "reader-always-show-images",
                "hide" => "reader-hide-images",
                other => return Err(format!("no button {other}")),
            };
            let page = reader
                .widget()
                .first_child()
                .ok_or("the reader shows nothing")?;
            find(&page, name).ok_or_else(|| format!("the shown message has no {name}"))?
        }
    };
    click_widget(harness, &widget).await
}

/// `files-print PATH`: Print through the registry into a PDF.
async fn print(harness: &Harness, path: &Path) -> StepResult {
    let reader = reader(harness)?;
    let _ = std::fs::remove_file(path);
    reader.set_print_target(Some(path.to_owned()));
    harness.activate("win.print")?;
    let started = Instant::now();
    glib::timeout_future(Duration::from_millis(50)).await;
    while reader.printing() {
        if started.elapsed() > WAIT {
            return Err("the print didn't finish".to_owned());
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    reader.set_print_target(None);
    if path.exists() {
        Ok(())
    } else {
        Err(format!("no PDF at {}", path.display()))
    }
}

fn mode(path: &Path) -> Result<u32, String> {
    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .map_err(|error| format!("{}: {error}", path.display()))
}

async fn check(harness: &Harness, args: &[String]) -> StepResult {
    let arg = |index: usize| -> Result<&str, String> {
        args.get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("assert-files {} needs more arguments", args[0]))
    };
    let reader = reader(harness)?;
    match args[0].as_str() {
        "message" => {
            let name = arg(1)?;
            let id = ADDED
                .with(|added| {
                    added
                        .borrow()
                        .iter()
                        .find(|added| added.name == name)
                        .map(|added| added.id)
                })
                .ok_or_else(|| format!("files-add {name} first"))?;
            equal("the message shown", reader.message(), Some(id))
        }
        "cards" => equal("cards", reader.attachment_cards().len(), number(arg(1)?)?),
        "card" => {
            let index = number(arg(1)?)?;
            let card = reader
                .attachment_card(index)
                .ok_or_else(|| format!("no card {index}"))?;
            let wanted = arg(3)?;
            match arg(2)? {
                "name" => equal("name", card.name.as_str(), wanted),
                "size" => equal("size", card.size.replace('\u{a0}', " ").as_str(), wanted),
                "warning" => match &card.warning {
                    Some(text) if text.contains(wanted) => Ok(()),
                    warning => Err(format!("warning is {warning:?}, wanted {wanted:?}")),
                },
                "danger" => equal("danger", card.danger, boolean(wanted)?),
                "open" => equal("open", card.open, boolean(wanted)?),
                "busy" => equal("busy", card.busy, boolean(wanted)?),
                "failure" => match (&card.failure, wanted) {
                    (None, "none") => Ok(()),
                    (Some(text), wanted) if wanted != "none" && text.contains(wanted) => Ok(()),
                    (failure, _) => Err(format!("failure is {failure:?}, wanted {wanted:?}")),
                },
                other => Err(format!("no card field {other}")),
            }
        }
        "opened" => equal("opened", hooks()?.opened.borrow().len(), number(arg(1)?)?),
        "opened-file" => {
            let path = hooks()?
                .opened
                .borrow()
                .last()
                .cloned()
                .ok_or("nothing was opened")?;
            equal(
                "the opened file's name",
                path.file_name().and_then(|name| name.to_str()),
                Some(arg(1)?),
            )?;
            let folder = path.parent().ok_or("the file has no folder")?;
            equal(
                "the folder's mode",
                format!("{:o}", mode(folder)?),
                "700".into(),
            )?;
            equal(
                "the file's mode",
                format!("{:o}", mode(&path)?),
                "600".into(),
            )?;
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            equal(
                "the opened bytes",
                bytes == attachment_bytes(arg(2)?, arg(3)?)?,
                true,
            )
        }
        "saved" => {
            let bytes = std::fs::read(arg(1)?)
                .map_err(|error| format!("{}: {error}", arg(1).unwrap_or_default()))?;
            equal(
                "the saved bytes",
                bytes == attachment_bytes(arg(2)?, arg(3)?)?,
                true,
            )?;
            equal("never executable", mode(Path::new(arg(1)?))? & 0o111, 0)
        }
        "eml" => {
            let bytes = std::fs::read(arg(1)?)
                .map_err(|error| format!("{}: {error}", arg(1).unwrap_or_default()))?;
            equal("the saved message", bytes == added_raw(arg(2)?)?, true)
        }
        "missing" => equal("the file is missing", !Path::new(arg(1)?).exists(), true),
        "folder" => {
            let folder = scratch(harness, "files-folder");
            let count = std::fs::read_dir(&folder)
                .map_err(|error| error.to_string())?
                .count();
            equal("files in the folder", count, number(arg(1)?)?)
        }
        "banner" => equal("banner", reader.state().banner, boolean(arg(1)?)?),
        "note" => {
            let shown = reader
                .widget()
                .first_child()
                .and_then(|page| find(&page, "reader-sender-note"))
                .is_some_and(|note| note.is_visible());
            equal("the sender's line", shown, boolean(arg(1)?)?)
        }
        "remote-allowed" => {
            let allowed = matches!(
                reader.state().body,
                Some(crate::reader::ShownBody::Html {
                    remote_allowed: true,
                    ..
                })
            );
            equal("remote images allowed", allowed, boolean(arg(1)?)?)
        }
        "sender" => {
            let started = Instant::now();
            while !reader.remote_senders_loaded() && started.elapsed() < WAIT {
                glib::timeout_future(Duration::from_millis(10)).await;
            }
            equal(
                "the reader's list",
                reader.remote_sender_allowed(arg(1)?),
                boolean(arg(2)?)?,
            )
        }
        "stored-sender" => {
            let reply = harness
                .state
                .mailbox()
                .store()
                .ok_or("the store is not open")?
                .remote_senders();
            let senders = reply.recv().await.map_err(|error| error.to_string())?;
            equal(
                "the store's list",
                senders
                    .iter()
                    .any(|sender| sender == arg(1).unwrap_or_default()),
                boolean(arg(2)?)?,
            )
        }
        "toggle" => {
            let window = harness.window()?;
            let action = window
                .window()
                .lookup_action(arg(1)?)
                .ok_or_else(|| format!("no action {}", arg(1).unwrap_or_default()))?;
            let on = action
                .state()
                .and_then(|state| state.get::<bool>())
                .ok_or("not a toggle")?;
            equal("the toggle", on, boolean(arg(2)?)?)
        }
        "original" => {
            let open = original_dialog();
            match arg(1)? {
                "open" => equal("Show Original is open", open.is_some(), boolean(arg(2)?)?),
                field => {
                    let open = open.ok_or("Show Original isn't open")?;
                    let buffer = open.text.buffer();
                    let text = buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .to_string();
                    match field {
                        "contains" => equal("has the text", text.contains(arg(2)?), true),
                        "lacks" => equal("lacks the text", !text.contains(arg(2)?), true),
                        "cut" => equal(
                            "the source is cut",
                            open.notice.parent().is_some_and(|row| row.is_visible()),
                            boolean(arg(2)?)?,
                        ),
                        "status" => {
                            let status = open.status.text().to_string();
                            if status.contains(arg(2)?) {
                                Ok(())
                            } else {
                                Err(format!("the status is {status:?}"))
                            }
                        }
                        "view" => {
                            equal("monospace", open.text.is_monospace(), true)?;
                            equal("read only", open.text.is_editable(), false)?;
                            equal(
                                "the source is shown",
                                open.stack.visible_child_name().as_deref(),
                                Some(arg(2)?),
                            )
                        }
                        other => Err(format!("no original field {other}")),
                    }
                }
            }
        }
        "pdf" => {
            let path = PathBuf::from(arg(1)?);
            let wanted = arg(2)?.to_owned();
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            equal("a PDF", bytes.starts_with(b"%PDF-"), true)?;
            let pages = pdf_pages(&bytes);
            if pages < 1 {
                return Err("the PDF has no page".to_owned());
            }
            let text = blocking(move || {
                std::process::Command::new("pdftotext")
                    .arg(&path)
                    .arg("-")
                    .output()
            })
            .await?
            .map_err(|error| format!("pdftotext (poppler) is needed for this check: {error}"))?;
            let text = String::from_utf8_lossy(&text.stdout);
            println!("  # {pages} pages");
            if text.contains(&wanted) {
                Ok(())
            } else {
                Err(format!("the PDF's text lacks {wanted:?}"))
            }
        }
        other => Err(format!("no assert-files {other}")),
    }
}

/// The page count of a PDF's page tree (`/Type /Pages … /Count N`); 0 when it has none.
fn pdf_pages(bytes: &[u8]) -> usize {
    let text = String::from_utf8_lossy(bytes);
    text.match_indices("/Type /Pages")
        .filter_map(|(at, _)| {
            let rest = &text[at..];
            let count = rest.find("/Count ")?;
            rest[count + "/Count ".len()..]
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .max()
        .unwrap_or(0)
}
