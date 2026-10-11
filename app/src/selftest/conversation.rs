//! The conversation view's self-test commands (ML-057), on the app's own list and reader:
//! sample conversations (`reader::samples::conversation`) filed into the app's store, shown by
//! selecting a row, driven by real clicks through Broadway, the window's actions and
//! programmatic scrolling, and checked through `Reader::state` and `Reader::bodies`. Every
//! message is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `conversation-account NAME ADDRESS` | An account at ADDRESS with the six role folders (`alex.berg@example.test` makes the samples "to me") |
//! | `conversation-add NAME [ACCOUNT]` | Files sample conversation NAME (`trip`, `letters`, `long`, `typical`, `links`) in the account's Inbox, the user's own messages in its Sent |
//! | `conversation-show MESSAGE` | Selects MESSAGE's row (`trip-6`, …) and waits until the reader shows its conversation |
//! | `conversation-click-row N` | A real click on the N-th collapsed row (from 0), scrolled into view first; waits for its card |
//! | `conversation-click-header MESSAGE` | A real click on MESSAGE's card header; waits for its row |
//! | `conversation-click-fold` | A real click on the first "N earlier messages" row |
//! | `conversation-scroll top\|bottom\|FRACTION` | Scrolls the reader's column and waits until the bodies on screen are drawn |
//! | `conversation-scroll-through STEP` | Scrolls from the top to the bottom STEP px at a time: at every step at most `MAX_LIVE_VIEWS` views; once settled, no blank body on screen and every body as tall as first measured |
//! | `conversation-reply NAME` | A reply to conversation NAME's latest message arrives, unread |
//! | `conversation-delete MESSAGE` | MESSAGE goes to the Trash (a local account applies it at once) |
//! | `conversation-remember MESSAGE` | Notes where MESSAGE starts in the column, for `assert-conversation kept` |
//! | `assert-conversation FIELD VALUE…` | `messages`, `expanded` (a count, or names joined by commas), `collapsed`, `cards` (expanded cards built), `folds`, `folded`, `html-bodies`, `measured` (HTML bodies with a height), `live-bodies`, `live-at-most`, `top-of` MESSAGE (its card or row starts in the column's view), `at-top` (the column isn't scrolled), `blank`, `read` MESSAGE `true\|false`, `selected` MESSAGE, `kept` (the remembered message starts where it did, within 2 px), `labelled` (rows and headers say what they are), `criticals` N, `order` MESSAGE,… (they start in that order down the column), `quick-above`/`quick-below` MESSAGE (the quick reply starts above or below it) |

use super::{Harness, StepResult, blocking, equal, mail, number, script, triage};
use crate::reader::{BodyReport, MAX_LIVE_VIEWS, Reader, samples};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_domain::triage::Triage;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{FolderSpec, NewAccount, QueueTriage};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The commands and their argument counts, for the script parser.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("conversation-account", 2, 2),
    ("conversation-add", 1, 2),
    ("conversation-show", 1, 1),
    ("conversation-click-row", 1, 1),
    ("conversation-click-header", 1, 1),
    ("conversation-click-fold", 0, 0),
    ("conversation-scroll", 1, 1),
    ("conversation-scroll-through", 1, 1),
    ("conversation-reply", 1, 1),
    ("conversation-delete", 1, 1),
    ("conversation-remember", 1, 1),
    ("assert-conversation", 1, 3),
];

const WAIT: Duration = Duration::from_secs(10);

thread_local! {
    /// Every sample message filed, by name.
    static NAMES: RefCell<HashMap<String, MessageId>> = RefCell::new(HashMap::new());
    /// Messages in each conversation filed, by its name, and replies added since.
    static SIZES: RefCell<HashMap<String, (usize, usize)>> = RefCell::new(HashMap::new());
    /// Gtk and GObject criticals since the first command.
    static CRITICALS: Cell<Option<u32>> = const { Cell::new(None) };
    /// A message and where it started in the column when `conversation-remember` ran.
    static REMEMBERED: Cell<Option<(MessageId, f64)>> = const { Cell::new(None) };
}

pub fn handles(command: &str) -> bool {
    command.starts_with("conversation-") || command == "assert-conversation"
}

/// Counts GTK's and GObject's criticals from now on, still writing them as GLib does.
fn count_criticals() {
    if CRITICALS.with(Cell::get).is_some() {
        return;
    }
    CRITICALS.with(|count| count.set(Some(0)));
    for domain in ["Gtk", "GLib-GObject"] {
        glib::log_set_handler(
            Some(domain),
            glib::LogLevels::LEVEL_CRITICAL,
            false,
            false,
            |domain, level, message| {
                CRITICALS.with(|count| count.set(Some(count.get().unwrap_or(0) + 1)));
                glib::log_default_handler(domain, level, Some(message));
            },
        );
    }
}

fn reader(harness: &Harness) -> Result<Reader, String> {
    harness
        .state
        .reader()
        .ok_or_else(|| "there is no reader".to_owned())
}

pub(super) fn id(name: &str) -> Result<MessageId, String> {
    NAMES
        .with(|names| names.borrow().get(name).copied())
        .ok_or_else(|| format!("no message {name}; file it with conversation-add"))
}

/// The sample name of message `id`, if a sample filed it.
pub(super) fn name(id: MessageId) -> Option<String> {
    NAMES.with(|names| {
        names
            .borrow()
            .iter()
            .find(|(_, candidate)| **candidate == id)
            .map(|(name, _)| name.clone())
    })
}

fn name_of(id: MessageId) -> String {
    NAMES.with(|names| {
        names
            .borrow()
            .iter()
            .find(|(_, candidate)| **candidate == id)
            .map_or_else(|| id.0.to_string(), |(name, _)| name.clone())
    })
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    count_criticals();
    let arg = |index: usize| args[index].as_str();
    match command {
        "conversation-account" => account(harness, arg(0), arg(1)).await,
        "conversation-add" => add(harness, arg(0), args.get(1).map(String::as_str)).await,
        "conversation-show" => show(harness, arg(0)).await,
        "conversation-click-row" => click_row(harness, number(arg(0))?).await,
        "conversation-click-header" => click_header(harness, arg(0)).await,
        "conversation-click-fold" => click_fold(harness).await,
        "conversation-scroll" => scroll(harness, arg(0)).await,
        "conversation-scroll-through" => scroll_through(harness, number(arg(0))? as f64).await,
        "conversation-reply" => reply(harness, arg(0)).await,
        "conversation-delete" => delete(harness, arg(0)).await,
        "conversation-remember" => {
            let id = id(arg(0))?;
            let top = reader(harness)?
                .message_top(id)
                .ok_or_else(|| format!("{} isn't shown", arg(0)))?;
            REMEMBERED.with(|remembered| remembered.set(Some((id, top))));
            Ok(())
        }
        "assert-conversation" => check(harness, args).await,
        other => Err(format!("unknown command {other}")),
    }
}

/// An account named `name` at `address` (the samples write to `alex.berg@example.test`, so
/// their messages say "to me" there), with the six role folders.
async fn account(harness: &Harness, name: &str, address: &str) -> StepResult {
    let mailbox = harness.state.mailbox();
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .add_account(NewAccount {
            name: name.to_owned(),
            address: address.to_owned(),
        });
    let account = reply.recv().await.map_err(|error| error.to_string())?.value;
    for (role, folder) in [
        (FolderRole::Inbox, "INBOX"),
        (FolderRole::Drafts, "Drafts"),
        (FolderRole::Sent, "Sent"),
        (FolderRole::Archive, "Archive"),
        (FolderRole::Junk, "Junk"),
        (FolderRole::Trash, "Trash"),
    ] {
        let reply = mailbox
            .store()
            .ok_or("the store is not open")?
            .upsert_folder(FolderSpec::local(account, folder, role));
        reply.recv().await.map_err(|error| error.to_string())?;
    }
    let started = Instant::now();
    while !mailbox
        .folders()
        .iter()
        .any(|folder| folder.account == account)
    {
        if started.elapsed() > WAIT {
            return Err("the sidebar never got the new account's folders".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    mail::settled(harness).await
}

async fn add(harness: &Harness, name: &str, account: Option<&str>) -> StepResult {
    if !samples::CONVERSATIONS.contains(&name) {
        return Err(format!(
            "no conversation {name}; there are {:?}",
            samples::CONVERSATIONS
        ));
    }
    let spec =
        |role: &str| account.map_or_else(|| role.to_owned(), |account| format!("{account}:{role}"));
    let inbox = mail::find_folder(harness, &spec("inbox"))?.id;
    let sent = mail::find_folder(harness, &spec("sent"))?.id;
    let mailbox = harness.state.mailbox();
    let store = mailbox.shared_store().ok_or("the store is not open")?;
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let messages = samples::conversation(name, glib::real_time() / 1_000_000);
    let count = messages.len();
    let filed = blocking(move || {
        samples::fill_conversation(&store, &blobs, inbox, sent, messages)
            .map_err(|error| error.to_string())
    })
    .await??;
    NAMES.with(|names| names.borrow_mut().extend(filed.ids.iter().cloned()));
    SIZES.with(|sizes| sizes.borrow_mut().insert(name.to_owned(), (count, 0)));
    println!("  # {name}: {} messages filed", filed.ids.len());
    mail::settled(harness).await
}

async fn show(harness: &Harness, name: &str) -> StepResult {
    let id = id(name)?;
    let list = mail::list(harness)?;
    let started = Instant::now();
    let position = loop {
        if let Some((position, _)) = list.position(id) {
            break position;
        }
        if started.elapsed() > WAIT {
            return Err(format!("{name} isn't in the list"));
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    };
    list.select_position(position - 1);
    let reader = reader(harness)?;
    while reader.message() != Some(id) {
        if started.elapsed() > WAIT {
            return Err(format!("the reader never showed {name}'s conversation"));
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    settle(harness, &reader).await?;
    let state = reader.state();
    println!(
        "  # {name}: {} messages, {} expanded, {} rows, {} folded",
        state.messages,
        state.expanded.len(),
        state.collapsed.len(),
        state.folded
    );
    Ok(())
}

/// Waits a frame, then until every HTML body on screen holds a drawn view (5 s at most).
async fn settle(harness: &Harness, reader: &Reader) -> StepResult {
    harness.wait_frames(2).await?;
    let started = Instant::now();
    loop {
        let blank = blank(reader);
        if blank.is_empty() {
            return harness.wait_frames(1).await;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let details: Vec<String> = reader
                .bodies()
                .into_iter()
                .filter(|body| blank.contains(&body.id))
                .map(|body| {
                    format!(
                        "{} at {:.0}–{:.0} px: view {}, loaded {}, painted {}, measured {:?}, \
                         box {} px",
                        name_of(body.id),
                        body.top,
                        body.bottom,
                        body.live,
                        body.loaded,
                        body.painted,
                        body.measured,
                        body.height
                    )
                })
                .collect();
            return Err(format!(
                "bodies still blank on screen ({} live views): {}",
                reader.state().live_views,
                details.join("; ")
            ));
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
}

fn names(ids: &[MessageId]) -> String {
    ids.iter()
        .map(|id| name_of(*id))
        .collect::<Vec<_>>()
        .join(",")
}

fn column(reader: &Reader) -> Result<gtk::ScrolledWindow, String> {
    reader
        .widget()
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
        .ok_or_else(|| "the reader isn't in its column".to_owned())
}

/// HTML bodies on screen that show nothing yet: no view, or a view not loaded and drawn.
fn blank(reader: &Reader) -> Vec<MessageId> {
    let Ok(column) = column(reader) else {
        return Vec::new();
    };
    let height = f64::from(column.height());
    reader
        .bodies()
        .into_iter()
        .filter(|body| body.bottom > 0.0 && body.top < height)
        .filter(|body| !(body.live && body.loaded && body.painted))
        .map(|body| body.id)
        .collect()
}

/// The reader's widgets under `root` named `name`, in order.
fn named(root: &gtk::Widget, name: &str) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    super::collect(root, &mut |widget| {
        if widget.widget_name() == name && widget.is_mapped() {
            found.push(widget.clone());
        }
    });
    found
}

/// Scrolls `widget` into the column's view and clicks it for real.
async fn click(harness: &Harness, reader: &Reader, widget: &gtk::Widget) -> StepResult {
    if let Some(viewport) = widget
        .ancestor(gtk::Viewport::static_type())
        .and_downcast::<gtk::Viewport>()
    {
        viewport.scroll_to(widget, None);
    }
    settle(harness, reader).await?;
    let button = widget
        .downcast_ref::<gtk::Button>()
        .ok_or_else(|| format!("{} isn't a button", widget.widget_name()))?;
    triage::click_button(harness, button).await
}

async fn click_row(harness: &Harness, index: usize) -> StepResult {
    let reader = reader(harness)?;
    let rows = named(&reader.widget(), "reader-row");
    let row = rows
        .get(index)
        .ok_or_else(|| format!("there are {} collapsed rows", rows.len()))?
        .clone();
    let before = reader.state().expanded.len();
    click(harness, &reader, &row).await?;
    let started = Instant::now();
    while reader.state().expanded.len() == before || !rows_card_ready(&reader) {
        if started.elapsed() > WAIT {
            return Err("the row never expanded".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    settle(harness, &reader).await
}

/// Every expanded message shows its card, not its row.
fn rows_card_ready(reader: &Reader) -> bool {
    let state = reader.state();
    state.expanded.len() + state.collapsed.len() + state.folded == state.messages
        && named(&reader.widget(), "reader-card").len() == state.expanded.len()
}

async fn click_header(harness: &Harness, name: &str) -> StepResult {
    let id = id(name)?;
    let reader = reader(harness)?;
    let index = reader
        .state()
        .expanded
        .iter()
        .position(|expanded| *expanded == id)
        .ok_or_else(|| format!("{name} isn't expanded"))?;
    let headers = named(&reader.widget(), "reader-card-header");
    let header = headers
        .get(index)
        .ok_or_else(|| format!("{name}'s card isn't on screen"))?
        .clone();
    click(harness, &reader, &header).await?;
    harness.wait_frames(1).await
}

async fn click_fold(harness: &Harness) -> StepResult {
    let reader = reader(harness)?;
    let fold = named(&reader.widget(), "reader-fold")
        .into_iter()
        .next()
        .ok_or("there is no fold")?;
    let before = reader.state().folded;
    click(harness, &reader, &fold).await?;
    let after = reader.state().folded;
    println!("  # folded {before} → {after}");
    if after < before {
        Ok(())
    } else {
        Err("the fold revealed nothing".to_owned())
    }
}

async fn scroll(harness: &Harness, to: &str) -> StepResult {
    let reader = reader(harness)?;
    let adjustment = column(&reader)?.vadjustment();
    let end = (adjustment.upper() - adjustment.page_size()).max(0.0);
    let value = match to {
        "top" => 0.0,
        "bottom" => end,
        fraction => {
            end * fraction
                .parse::<f64>()
                .map_err(|_| format!("use top, bottom or a fraction, not {fraction}"))?
        }
    };
    adjustment.set_value(value);
    settle(harness, &reader).await
}

/// From the top to the bottom of the column, `step` px at a time.
async fn scroll_through(harness: &Harness, step: f64) -> StepResult {
    let reader = reader(harness)?;
    let adjustment = column(&reader)?.vadjustment();
    adjustment.set_value(0.0);
    settle(harness, &reader).await?;
    let mut first: HashMap<MessageId, i32> = HashMap::new();
    let (mut steps, mut most, mut problems) = (0, 0, Vec::new());
    loop {
        harness.wait_frames(1).await?;
        let live = reader.state().live_views;
        most = most.max(live);
        if live > MAX_LIVE_VIEWS {
            problems.push(format!("{live} live views at {:.0} px", adjustment.value()));
        }
        if let Err(error) = settle(harness, &reader).await {
            problems.push(format!("at {:.0} px: {error}", adjustment.value()));
        }
        for body in reader.bodies() {
            check_height(&body, &mut first, &mut problems);
        }
        steps += 1;
        let end = (adjustment.upper() - adjustment.page_size()).max(0.0);
        if adjustment.value() >= end - 0.5 {
            break;
        }
        adjustment.set_value((adjustment.value() + step).min(end));
    }
    println!(
        "  # {steps} steps to {:.0} px; at most {most} live views; bodies measured: {}",
        adjustment.value(),
        first.len()
    );
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// A body keeps the height it was first measured at, with or without its view.
fn check_height(
    body: &BodyReport,
    first: &mut HashMap<MessageId, i32>,
    problems: &mut Vec<String>,
) {
    let Some(measured) = body.measured else {
        return;
    };
    let kept = *first.entry(body.id).or_insert(measured);
    if (kept - measured).abs() > 1 || (body.height - measured).abs() > 1 {
        problems.push(format!(
            "{} first measured {kept} px, now {measured} px, its box {} px",
            name_of(body.id),
            body.height
        ));
    }
}

async fn reply(harness: &Harness, name: &str) -> StepResult {
    let (count, replies) = SIZES
        .with(|sizes| sizes.borrow().get(name).copied())
        .ok_or_else(|| format!("{name} wasn't filed"))?;
    let parent = if replies == 0 {
        format!("{name}-{count}@example.test")
    } else {
        format!("{name}-reply-{replies}@example.test")
    };
    let serial = replies + 1;
    let id = format!("{name}-reply-{serial}");
    let now = glib::real_time() / 1_000_000;
    let mut draft = samples::Draft::new(
        ("Tomas Berg", "tomas@example.test"),
        "Re: one more thing",
        now,
        &id,
    );
    draft.text = Some("One more thing: the bakery opens at eight on Saturdays.\n".into());
    let marker = format!("Message-ID: <{id}@example.test>\r\n");
    let raw = String::from_utf8_lossy(&draft.render())
        .replacen(
            &marker,
            &format!("{marker}In-Reply-To: <{parent}>\r\nReferences: <{parent}>\r\n"),
            1,
        )
        .into_bytes();
    let inbox = mail::find_folder(harness, "inbox")?.id;
    let mailbox = harness.state.mailbox();
    let store = mailbox.shared_store().ok_or("the store is not open")?;
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let ingested = blocking(move || {
        ingest::ingest_raw(
            &store,
            &blobs,
            vec![RawMessage {
                folder: inbox,
                raw,
                ..RawMessage::default()
            }],
        )
        .map_err(|error| error.to_string())
    })
    .await??;
    let stored = ingested
        .ids
        .first()
        .copied()
        .flatten()
        .ok_or("the store already held the reply")?;
    NAMES.with(|names| names.borrow_mut().insert(id.clone(), stored));
    SIZES.with(|sizes| sizes.borrow_mut().insert(name.to_owned(), (count, serial)));
    println!("  # {id} arrived");
    mail::settled(harness).await
}

async fn delete(harness: &Harness, name: &str) -> StepResult {
    let id = id(name)?;
    let reply = harness
        .state
        .mailbox()
        .store()
        .ok_or("the store is not open")?
        .queue_triage(QueueTriage {
            action: Triage::Delete,
            messages: vec![id],
            not_before: 0,
        });
    let queued = reply.recv().await.map_err(|error| error.to_string())?;
    if queued.applied != 1 {
        return Err(format!("{name} wasn't deleted: {queued:?}"));
    }
    mail::settled(harness).await
}

async fn check(harness: &Harness, args: &[String]) -> StepResult {
    let reader = reader(harness)?;
    let state = reader.state();
    let field = args[0].as_str();
    let value = |index: usize| {
        args.get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("assert-conversation {field} needs a value"))
    };
    match field {
        "messages" => equal("messages", state.messages, number(value(1)?)?),
        "expanded" => {
            let wanted = value(1)?;
            match wanted.parse::<usize>() {
                Ok(count) => equal("expanded", state.expanded.len(), count),
                Err(_) => equal("expanded", names(&state.expanded).as_str(), wanted),
            }
        }
        "collapsed" => equal("collapsed rows", state.collapsed.len(), number(value(1)?)?),
        "folds" => equal("folds", state.folds, number(value(1)?)?),
        "folded" => equal("folded messages", state.folded, number(value(1)?)?),
        "html-bodies" => equal("HTML bodies", state.html_bodies, number(value(1)?)?),
        "live-at-most" => {
            let limit = number(value(1)?)?;
            if state.live_views <= limit && state.live_bodies.len() <= limit {
                Ok(())
            } else {
                Err(format!(
                    "{} live views, {} bodies with one ({}), over {limit}",
                    state.live_views,
                    state.live_bodies.len(),
                    names(&state.live_bodies)
                ))
            }
        }
        "order" => {
            let mut tops = Vec::new();
            for name in value(1)?.split(',') {
                let top = reader
                    .message_top(id(name)?)
                    .ok_or_else(|| format!("{name} isn't in the conversation"))?;
                tops.push((name, top.round()));
            }
            if tops.windows(2).all(|pair| pair[0].1 < pair[1].1) {
                Ok(())
            } else {
                Err(format!("down the column: {tops:?}"))
            }
        }
        "quick-above" | "quick-below" => {
            let name = value(1)?;
            let message = reader
                .message_top(id(name)?)
                .ok_or_else(|| format!("{name} isn't in the conversation"))?;
            let column = column(&reader)?;
            let quick = harness.widget("quick-reply")?;
            let quick = quick
                .compute_point(&column, &gtk::graphene::Point::zero())
                .ok_or("the quick reply isn't in the column")?
                .y();
            let above = f64::from(quick) < message;
            equal("quick reply above", above, field == "quick-above")
        }
        "top-of" => {
            let name = value(1)?;
            let id = id(name)?;
            let column = column(&reader)?;
            let top = reader
                .message_top(id)
                .ok_or_else(|| format!("{name} isn't in the conversation"))?;
            let height = f64::from(column.height());
            println!(
                "  # {name} starts {top:.0} px down a {height:.0} px column scrolled {:.0} px",
                column.vadjustment().value()
            );
            equal(
                "starts in the column's view",
                (0.0..height).contains(&top),
                true,
            )
        }
        "live-bodies" => equal(
            "bodies holding a view",
            state.live_bodies.len(),
            number(value(1)?)?,
        ),
        "cards" => equal(
            "cards on the page",
            named(&reader.widget(), "reader-card").len(),
            number(value(1)?)?,
        ),
        "at-top" => {
            let at_top = column(&reader)?.vadjustment().value() < 0.5;
            equal("scrolled to the top", at_top, script::boolean(value(1)?)?)
        }
        "blank" => equal(
            "blank bodies on screen",
            blank(&reader).len(),
            number(value(1)?)?,
        ),
        "read" => {
            let id = id(value(1)?)?;
            let reply = harness
                .state
                .mailbox()
                .store()
                .ok_or("the store is not open")?
                .message(id);
            let detail = reply
                .recv()
                .await
                .map_err(|error| error.to_string())?
                .value
                .ok_or("the message is gone")?;
            equal(
                "read",
                detail.summary.flags.is_seen(),
                script::boolean(value(2)?)?,
            )
        }
        "measured" => equal(
            "HTML bodies measured",
            reader
                .bodies()
                .iter()
                .filter(|body| body.measured.is_some())
                .count(),
            number(value(1)?)?,
        ),
        "kept" => {
            let (id, before) = REMEMBERED
                .with(Cell::get)
                .ok_or("run conversation-remember first")?;
            let now = reader
                .message_top(id)
                .ok_or_else(|| format!("{} isn't shown", name_of(id)))?;
            println!(
                "  # {} started {before:.0} px down the column, now {now:.0} px",
                name_of(id)
            );
            equal(
                "kept its place (within 2 px)",
                (now - before).abs() <= 2.0,
                true,
            )
        }
        "selected" => equal(
            "the reader's message",
            reader.message().map(name_of),
            Some(value(1)?.to_owned()),
        ),
        "labelled" => {
            let root = reader.widget();
            let mut missing = Vec::new();
            for name in [
                "reader-row",
                "reader-card-header",
                "reader-fold",
                "reader-conversation",
            ] {
                for widget in named(&root, name) {
                    if !gtk::test_accessible_has_property(&widget, gtk::AccessibleProperty::Label) {
                        missing.push(format!("{name} has no label"));
                    }
                }
            }
            for name in ["reader-row", "reader-card-header"] {
                for widget in named(&root, name) {
                    if !gtk::test_accessible_has_state(&widget, gtk::AccessibleState::Expanded) {
                        missing.push(format!("{name} doesn't say whether it is expanded"));
                    }
                }
            }
            if missing.is_empty() {
                Ok(())
            } else {
                Err(missing.join("\n"))
            }
        }
        "criticals" => equal(
            "Gtk and GObject criticals",
            CRITICALS.with(Cell::get).unwrap_or(0),
            number(value(1)?)? as u32,
        ),
        other => Err(format!("assert-conversation knows no {other}")),
    }
}
