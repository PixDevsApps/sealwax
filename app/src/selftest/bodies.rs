//! The reader's body downloads in the self-test (ML-044): synthetic messages (the reader's
//! samples) stored in the app's store as headers and a preview, as the first sync leaves them;
//! the app's own reader asks the fake engine (`engine-attach`, `setup.rs`) for their bodies, and
//! the script stores a body as the engine would and answers with `engine-reply body-fetched`.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `bodies-add NAME…` | Stores the named samples (`reader::samples`) in the first account's Inbox as headers and a preview, the first one newest |
//! | `bodies-store NAME` | What the engine does with a download: the sample's raw message into the blob store, then the store's upgrade of its row, whose change event the reader follows |
//! | `assert-bodies-requested NAME…` | The last `FetchBodies` the fake engine took asked for these samples, in order |
//! | `assert-body STATE` | The app's reader shows `downloading`, `download-failed`, `not-downloaded`, `plain`, `html-themed`, `html-paper`, `empty`, `missing` or `none` |
//! | `assert-body-text TEXT` | The app's reader's body (plain text or the HTML's text) contains TEXT |

use super::{Harness, StepResult, blocking, equal};
use crate::reader::samples;
use gtk4::glib;
use sealwax_domain::message;
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_infrastructure::store::{BodyState, BodyUpgrade, NewMessage};
use std::cell::RefCell;

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("bodies-add", 1, usize::MAX),
    ("bodies-store", 1, 1),
    ("assert-bodies-requested", 1, usize::MAX),
    ("assert-body", 1, 1),
    ("assert-body-text", 1, 1),
];

/// The first sync's preview of a body, in bytes.
const PREVIEW: usize = 1024;

thread_local! {
    /// The samples `bodies-add` stored, by name.
    static ADDED: RefCell<Vec<(String, MessageId)>> = const { RefCell::new(Vec::new()) };
}

pub fn handles(command: &str) -> bool {
    command.starts_with("bodies-")
        || matches!(
            command,
            "assert-bodies-requested" | "assert-body" | "assert-body-text"
        )
}

fn added(name: &str) -> Result<MessageId, String> {
    ADDED
        .with(|added| {
            added
                .borrow()
                .iter()
                .find(|(added, _)| added == name)
                .map(|(_, id)| *id)
        })
        .ok_or_else(|| format!("bodies-add {name} first"))
}

fn sample(name: &str) -> Result<Vec<u8>, String> {
    samples::messages(glib::real_time() / 1_000_000)
        .into_iter()
        .find(|(sample, _)| *sample == name)
        .map(|(_, raw)| raw)
        .ok_or_else(|| format!("no sample {name}; there are {:?}", samples::NAMES))
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    match command {
        "bodies-add" => add(harness, args).await,
        "bodies-store" => store(harness, &args[0]).await,
        "assert-bodies-requested" => {
            let wanted = args
                .iter()
                .map(|name| added(name))
                .collect::<Result<Vec<_>, _>>()?;
            equal("bodies asked for", super::setup::last_requested(), wanted)
        }
        "assert-body" => {
            let reader = harness.state.reader().ok_or("the reader isn't installed")?;
            let state = reader.state();
            equal(
                "the reader's body",
                super::reader::body_name(state.body.as_ref()),
                args[0].as_str(),
            )
        }
        "assert-body-text" => {
            let reader = harness.state.reader().ok_or("the reader isn't installed")?;
            let text = if reader.web_view().is_some() {
                reader
                    .eval("return document.body ? document.body.innerText : '';")
                    .await?
                    .to_str()
                    .to_string()
            } else {
                super::reader::plain_text(&reader)
            };
            if text.contains(args[0].as_str()) {
                Ok(())
            } else {
                Err(format!("the body lacks {:?}", args[0]))
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// `bodies-add NAME…`: header-only rows, the first newest.
async fn add(harness: &Harness, names: &[String]) -> StepResult {
    let mailbox = harness.state.mailbox();
    let folder = mailbox
        .folders()
        .into_iter()
        .find(|folder| folder.role == FolderRole::Inbox)
        .ok_or("no Inbox; run mail-account first")?;
    let now = glib::real_time() / 1_000_000;
    let mut rows = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let raw = sample(name)?;
        let parsed = message::parse(&raw);
        let has_attachments = parsed.has_attachments();
        rows.push(NewMessage {
            folder: folder.id,
            date: now - 60 * index as i64,
            headers: parsed.headers,
            snippet: parsed.snippet,
            body_kind: parsed.body_kind,
            has_attachments,
            size: parsed.size,
            body_state: BodyState::Partial,
            search_text: message::truncate(&parsed.body_text, PREVIEW).to_owned(),
            ..NewMessage::default()
        });
    }
    let reply = mailbox.store().ok_or("the store is not open")?.ingest(rows);
    let ingested = reply.recv().await.map_err(|error| error.to_string())?;
    for (name, id) in names.iter().zip(ingested.ids) {
        let id = id.ok_or_else(|| format!("the store already held {name}"))?;
        ADDED.with(|added| added.borrow_mut().push((name.clone(), id)));
    }
    super::mail::settled(harness).await
}

/// `bodies-store NAME`: the engine's part of a download, through the store's real upgrade.
async fn store(harness: &Harness, name: &str) -> StepResult {
    let id = added(name)?;
    let raw = sample(name)?;
    let mailbox = harness.state.mailbox();
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .body_targets(vec![id]);
    let target = reply
        .recv()
        .await
        .map_err(|error| error.to_string())?
        .pop()
        .ok_or_else(|| format!("{name} is gone"))?;
    let upgrade = blocking(move || -> Result<BodyUpgrade, String> {
        let key = blobs.put(&raw).map_err(|error| error.to_string())?;
        let parsed = message::parse(&raw);
        Ok(BodyUpgrade {
            message: target.message,
            uid: target.uid,
            blob: key,
            blob_size: raw.len() as u64,
            body_state: BodyState::Full,
            body_kind: parsed.body_kind,
            has_attachments: parsed.has_attachments(),
            snippet: parsed.snippet.clone(),
            text_parts: parsed.text_parts.clone(),
            html_parts: parsed.html_parts.clone(),
            sections: Vec::new(),
            attachments: parsed.attachments,
            search_text: parsed.body_text,
            used: glib::real_time() / 1_000_000,
        })
    })
    .await??;
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .upgrade_body(upgrade);
    let upgraded = reply.recv().await.map_err(|error| error.to_string())?;
    equal("the row was upgraded", upgraded.found, true)?;
    super::mail::settled(harness).await
}
