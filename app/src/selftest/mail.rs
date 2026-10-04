//! Self-test commands for the message store and the message list (ML-026); the list's
//! measurements are in `perf.rs`. Fixture data comes from the built `sealwax-fixtures` binary,
//! run into the scratch store before the app opens it, so the app never depends on `devtools`.
//! Every message these commands make up is fictional, at `example.test`. The commands are listed
//! in docs/m1/list.md.

use super::script::Step;
use super::{Harness, StepResult, WAIT_TIMEOUT, blocking, equal, number};
use crate::list::{FILTERS, MessageList};
use crate::mailbox::folder_key;
use crate::window::ALL_INBOXES;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::{Flags, Folder, FolderId, FolderRole, MessageId};
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{FolderSpec, NewAccount, ViewScope};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    // Before the app starts; only before every other step but `desktop`.
    ("store-dir", 1, 1),
    ("fixtures", 1, usize::MAX),
    // The store, through the app's own store.
    ("mail-account", 1, 1),
    ("mail-add", 3, usize::MAX),
    ("mail-flag", 2, 2),
    ("mail-remove", 1, 1),
    ("mail-churn", 2, 2),
    // The list.
    ("folder", 1, 2),
    ("chip", 1, 1),
    ("search", 1, 1),
    ("search-escape", 0, 0),
    ("list-clock", 1, 1),
    ("list-select", 1, 1),
    ("list-key", 1, 1),
    ("list-scroll", 1, 1),
    ("list-scroll-end", 0, 0),
    ("list-wait", 0, 0),
    ("list-remember", 0, 0),
    // Assertions.
    ("assert-list-count", 1, 1),
    ("assert-list-row", 3, 3),
    ("assert-list-all", 2, 2),
    ("assert-list-sections", 1, 1),
    ("assert-list-headers", 1, 1),
    ("assert-list-selected", 1, 1),
    ("assert-list-remembered", 0, 0),
    ("assert-list-scrolled", 1, 1),
    ("assert-list-position", 1, 1),
    ("assert-list-growth", 1, 1),
    ("assert-list-top", 1, 1),
    ("assert-list-focus", 1, 1),
    ("assert-list-placeholders", 1, 1),
    ("assert-list-empty", 1, 1),
    ("assert-list-search", 2, 2),
    ("assert-list-matches-folder", 1, 1),
    ("assert-sidebar-count", 2, 2),
    ("assert-selection-reported", 1, 1),
    ("assert-action-enabled", 2, 2),
    ("assert-list-exact", 0, 0),
];

thread_local! {
    /// What each leading `fixtures` step printed, for its line in the report.
    static IMPORTS: RefCell<VecDeque<Vec<String>>> = const { RefCell::new(VecDeque::new()) };
    /// Messages the self-test added, numbered for their Message-IDs.
    pub(super) static ADDED: Cell<u32> = const { Cell::new(0) };
    /// The message `list-remember` noted.
    static REMEMBERED: Cell<Option<MessageId>> = const { Cell::new(None) };
    /// Every selection the list reported, for `assert-selection-reported`.
    static REPORTED: RefCell<Option<Rc<RefCell<Vec<MessageId>>>>> = const { RefCell::new(None) };
}

/// Runs the leading `store-dir` and `fixtures` steps before the app opens its store:
/// `store-dir PATH` makes PATH the data folder (store and blobs) instead of the scratch one;
/// `fixtures ARGS…` runs `sealwax-fixtures import --store … --blobs … ARGS…` into it (skipped when
/// `store-dir` names a folder that already holds a store).
pub fn before_start(steps: &[Step], dirs: &mut SealwaxDirs) -> Result<(), String> {
    let mut leading = true;
    let mut chosen = false;
    for step in steps {
        match step.command.as_str() {
            "desktop" => {}
            "store-dir" | "fixtures" if !leading => {
                return Err(format!(
                    "line {}: {} must come before the window's steps",
                    step.line, step.command
                ));
            }
            "store-dir" => {
                dirs.data = PathBuf::from(&step.args[0]);
                chosen = true;
            }
            "fixtures" => {
                let report = if chosen && dirs.store_file().exists() {
                    vec![format!(
                        "fixtures skipped: {} holds a store",
                        dirs.data.display()
                    )]
                } else {
                    import(&step.args, dirs)
                        .map_err(|error| format!("line {}: {error}", step.line))?
                };
                IMPORTS.with(|imports| imports.borrow_mut().push_back(report));
            }
            _ => leading = false,
        }
    }
    Ok(())
}

fn import(args: &[String], dirs: &SealwaxDirs) -> Result<Vec<String>, String> {
    let exe = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name("sealwax-fixtures");
    if !exe.exists() {
        return Err(format!(
            "{} is missing; build it with cargo build -p sealwax-fixtures",
            exe.display()
        ));
    }
    let started = Instant::now();
    let output = Command::new(&exe)
        .arg("import")
        .arg("--store")
        .arg(dirs.store_file())
        .arg("--blobs")
        .arg(dirs.blobs_dir())
        .args(args)
        .output()
        .map_err(|error| format!("{}: {error}", exe.display()))?;
    if !output.status.success() {
        return Err(format!(
            "sealwax-fixtures failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let mut report: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    report.push(format!(
        "import took {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    ));
    Ok(report)
}

/// Runs `command` when it is one of this module's; `None` otherwise.
pub(super) async fn run(harness: &Harness, command: &str, args: &[String]) -> Option<StepResult> {
    if let Some(result) = super::perf::run(harness, command, args).await {
        return Some(result);
    }
    let arg = |index: usize| args[index].as_str();
    let result = match command {
        "store-dir" => Ok(()),
        "fixtures" => {
            let report = IMPORTS.with(|imports| imports.borrow_mut().pop_front());
            for line in report.unwrap_or_default() {
                println!("  # {line}");
            }
            Ok(())
        }
        "mail-account" => add_account(harness, arg(0)).await,
        "mail-add" => add_message(harness, args).await,
        "mail-flag" => flag(harness, arg(0), arg(1)).await,
        "mail-remove" => remove(harness, arg(0)).await,
        "mail-churn" => match (number(arg(0)), arg(1).parse::<u64>()) {
            (Ok(count), Ok(seed)) => churn(harness, count, seed).await,
            _ => Err("mail-churn takes a count and a seed".to_owned()),
        },
        "folder" => select_folder(harness, arg(0), args.get(1).map(String::as_str)).await,
        "chip" => match FILTERS
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(arg(0)))
        {
            Some((filter, _)) => match list(harness) {
                Ok(list) => {
                    list.set_filter(*filter);
                    settled(harness).await
                }
                Err(error) => Err(error),
            },
            None => Err(format!("no chip {}", arg(0))),
        },
        "search" => match list(harness) {
            Ok(list) => {
                list.set_query(arg(0).to_owned());
                settled(harness).await
            }
            Err(error) => Err(error),
        },
        "search-escape" => match list(harness) {
            Ok(list) => {
                list.search_entry().emit_by_name::<()>("stop-search", &[]);
                settled(harness).await
            }
            Err(error) => Err(error),
        },
        "list-clock" => set_clock(harness, arg(0)).await,
        "list-select" => match (list(harness), number(arg(0))) {
            (Ok(list), Ok(position)) => {
                list.select_position(position as u32);
                settled(harness).await
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "list-key" => press(harness, arg(0)).await,
        "list-scroll" => match (list(harness), arg(0).parse::<f64>()) {
            (Ok(list), Ok(value)) => {
                list.scroller().vadjustment().set_value(value);
                settled(harness).await
            }
            (Err(error), _) => Err(error),
            (_, Err(_)) => Err(format!("expected pixels, got {}", arg(0))),
        },
        "list-scroll-end" => match list(harness) {
            Ok(list) => {
                let total = list.model().total();
                if total > 0 {
                    list.model().complete_growth();
                    list.list_view()
                        .scroll_to(total - 1, gtk::ListScrollFlags::NONE, None);
                }
                settled(harness).await
            }
            Err(error) => Err(error),
        },
        "list-wait" => settled(harness).await,
        "list-remember" => match list(harness) {
            Ok(list) => match list.selected() {
                Some(id) => {
                    REMEMBERED.with(|remembered| remembered.set(Some(id)));
                    Ok(())
                }
                None => Err("nothing is selected".to_owned()),
            },
            Err(error) => Err(error),
        },
        command
            if command.starts_with("assert-list-")
                || command == "assert-sidebar-count"
                || command == "assert-selection-reported"
                || command == "assert-action-enabled" =>
        {
            check(harness, command, args).await
        }
        _ => return None,
    };
    Some(result)
}

pub(super) fn list(harness: &Harness) -> Result<MessageList, String> {
    let list = harness
        .state
        .list()
        .ok_or_else(|| "there is no message list".to_owned())?;
    REPORTED.with(|reported| {
        if reported.borrow().is_none() {
            let selections = Rc::new(RefCell::new(Vec::new()));
            let sink = selections.clone();
            list.connect_message_selected(move |id| sink.borrow_mut().push(id));
            reported.replace(Some(selections));
        }
    });
    Ok(list)
}

/// Whether the list shows what it should, without anything left to load or grow.
/// Whether the list shows what it should, without anything left to load; it grows only while
/// it is on screen.
fn is_idle(list: &MessageList) -> bool {
    let model = list.model();
    let shown = list.list_view().is_mapped();
    !model.is_pending()
        && !model.is_searching()
        && (!shown || (!model.is_growing() && !list.is_growing()))
        && list.bound_rows().iter().all(|row| !row.placeholder)
}

/// Waits until the list is idle, painting frames.
pub(super) async fn settled(harness: &Harness) -> StepResult {
    let list = list(harness)?;
    let started = Instant::now();
    loop {
        if harness
            .window()
            .is_ok_and(|window| window.window().is_mapped())
        {
            harness.wait_frames(1).await?;
        } else {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        if is_idle(&list) {
            harness.wait_until_settled().await;
            if is_idle(&list) {
                return Ok(());
            }
        }
        if started.elapsed() > WAIT_TIMEOUT {
            return Err(format!(
                "the list didn't settle: pending {}, searching {}, growing {}, {} rows bound, {} placeholders",
                list.model().is_pending(),
                list.model().is_searching(),
                list.is_growing(),
                list.bound_rows().len(),
                list.bound_rows()
                    .iter()
                    .filter(|row| row.placeholder)
                    .count()
            ));
        }
    }
}

fn account_address(name: &str) -> String {
    let local: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("{local}@example.test")
}

/// `mail-account NAME`: an account with the six role folders, in the app's store.
async fn add_account(harness: &Harness, name: &str) -> StepResult {
    let mailbox = harness.state.mailbox();
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .add_account(NewAccount {
            name: name.to_owned(),
            address: account_address(name),
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
    while !mailbox.folders().iter().any(|f| f.account == account) {
        if started.elapsed() > WAIT_TIMEOUT {
            return Err("the sidebar never got the new account's folders".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    settled(harness).await
}

/// `[ACCOUNT:]ROLE`: a folder by its role, in the first account or the named one.
pub(super) fn find_folder(harness: &Harness, spec: &str) -> Result<Folder, String> {
    let mailbox = harness.state.mailbox();
    let (account, role) = match spec.split_once(':') {
        Some((account, role)) => (Some(account), role),
        None => (None, spec),
    };
    let accounts = mailbox.accounts();
    let account = match account {
        Some(name) => accounts.iter().find(|a| a.name == name),
        None => accounts.first(),
    }
    .ok_or_else(|| format!("no account for {spec}"))?;
    let role = FolderRole::parse(&role.to_ascii_lowercase());
    mailbox
        .folders()
        .into_iter()
        .find(|folder| folder.account == account.id && folder.role == role)
        .ok_or_else(|| format!("no folder {spec}"))
}

/// A date inside the named section (by the list's clock), or minutes, hours or days ago.
fn date_for(list: &MessageList, when: &str) -> Result<i64, String> {
    let thresholds = list.thresholds_now();
    let now = list.now_unix();
    let starts = thresholds.starts;
    let middle = |from: i64, to: i64, name: &str| {
        if from < to {
            Ok(from + (to - from) / 2)
        } else {
            Err(format!(
                "{name} is empty on this day; fix the day with list-clock"
            ))
        }
    };
    match when {
        "now" => Ok(now),
        "today" => Ok(starts[0] + (now - starts[0]).max(0) / 2),
        "yesterday" => middle(starts[1], starts[0], "Yesterday"),
        "this-week" => middle(starts[2], starts[1], "This week"),
        "this-month" => middle(starts[3], starts[2], "This month"),
        "older" => Ok(starts[3] - 10 * 86_400),
        relative => {
            let (amount, unit) = relative.split_at(relative.len().saturating_sub(1));
            let amount: i64 = amount
                .parse()
                .map_err(|_| format!("unknown date {relative}"))?;
            let seconds = match unit {
                "m" => 60,
                "h" => 3_600,
                "d" => 86_400,
                _ => return Err(format!("unknown date {relative}")),
            };
            Ok(now - amount * seconds)
        }
    }
}

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// An RFC 5322 date in UTC, with English names whatever the locale.
fn mail_date(unix: i64) -> String {
    match glib::DateTime::from_unix_utc(unix) {
        Ok(date) => format!(
            "{}, {:02} {} {} {:02}:{:02}:{:02} +0000",
            DAYS[(date.day_of_week() - 1).clamp(0, 6) as usize],
            date.day_of_month(),
            MONTHS[(date.month() - 1).clamp(0, 11) as usize],
            date.year(),
            date.hour(),
            date.minute(),
            date.second()
        ),
        Err(_) => "Thu, 01 Jan 1970 00:00:00 +0000".to_owned(),
    }
}

/// What a fictional message holds.
pub(super) struct Fiction {
    pub(super) subject: String,
    pub(super) from: String,
    pub(super) text: String,
    pub(super) attachment: bool,
    /// The self-test message this one answers, by its number.
    pub(super) reply_to: Option<u32>,
}

pub(super) fn raw_message(fiction: &Fiction, date: i64, serial: u32) -> Vec<u8> {
    let local: String = fiction
        .from
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    let mut text = format!(
        "From: {} <{local}@example.test>\r\nTo: Selftest <selftest@example.test>\r\n\
         Subject: {}\r\nDate: {}\r\nMessage-ID: <selftest-{serial}@example.test>\r\n\
         MIME-Version: 1.0\r\n",
        fiction.from,
        fiction.subject,
        mail_date(date)
    );
    if let Some(parent) = fiction.reply_to {
        text.push_str(&format!(
            "In-Reply-To: <selftest-{parent}@example.test>\r\n\
             References: <selftest-{parent}@example.test>\r\n"
        ));
    }
    if fiction.attachment {
        text.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"b{serial}\"\r\n\r\n--b{serial}\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n--b{serial}\r\n\
             Content-Type: text/csv; name=\"report.csv\"\r\n\
             Content-Disposition: attachment; filename=\"report.csv\"\r\n\r\n\
             month,total\r\nseptember,42\r\n--b{serial}--\r\n",
            fiction.text
        ));
    } else {
        text.push_str(&format!(
            "Content-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
            fiction.text
        ));
    }
    text.into_bytes()
}

/// Stores a fictional message through the real ingest path (blob store and parser off the GTK
/// thread, then the app's store writer), and returns its id.
async fn ingest_fiction(
    harness: &Harness,
    folder: FolderId,
    fiction: &Fiction,
    date: i64,
    flags: Flags,
) -> Result<MessageId, String> {
    let mailbox = harness.state.mailbox();
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let serial = ADDED.with(|added| {
        added.set(added.get() + 1);
        added.get()
    });
    let raw = RawMessage {
        folder,
        flags,
        internal_date: Some(date),
        raw: raw_message(fiction, date, serial),
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
    ingested
        .ids
        .first()
        .copied()
        .flatten()
        .ok_or_else(|| "the store already held that message".to_owned())
}

/// `mail-add [ACCOUNT:]ROLE WHEN SUBJECT [read] [starred] [attachment] [from=NAME] [text=WORDS]`.
async fn add_message(harness: &Harness, args: &[String]) -> StepResult {
    let list = list(harness)?;
    let folder = find_folder(harness, &args[0])?;
    let date = date_for(&list, &args[1])?;
    let mut fiction = Fiction {
        subject: args[2].clone(),
        from: "Ingrid Solberg".to_owned(),
        text: "Hei! Dette er en melding fra selvtesten.".to_owned(),
        attachment: false,
        reply_to: None,
    };
    let mut flags = Flags::NONE;
    for option in &args[3..] {
        match option.as_str() {
            "read" => flags = flags | Flags::SEEN,
            "starred" => flags = flags | Flags::FLAGGED,
            "attachment" => fiction.attachment = true,
            other => match other.split_once('=') {
                Some(("from", name)) => fiction.from = name.to_owned(),
                Some(("text", text)) => fiction.text = text.to_owned(),
                _ => return Err(format!("unknown option {other}")),
            },
        }
    }
    ingest_fiction(harness, folder.id, &fiction, date, flags).await?;
    settled(harness).await
}

fn row_id(list: &MessageList, position: &str) -> Result<MessageId, String> {
    let position = number(position)? as u32;
    list.model()
        .row_at(position)
        .map(|row| row.id)
        .ok_or_else(|| format!("row {position} isn't loaded"))
}

/// `mail-flag POSITION +read|-read|+starred|-starred`.
async fn flag(harness: &Harness, position: &str, change: &str) -> StepResult {
    let list = list(harness)?;
    let id = row_id(&list, position)?;
    let (add, remove) = match change {
        "+read" => (Flags::SEEN, Flags::NONE),
        "-read" => (Flags::NONE, Flags::SEEN),
        "+starred" => (Flags::FLAGGED, Flags::NONE),
        "-starred" => (Flags::NONE, Flags::FLAGGED),
        other => return Err(format!("unknown flag change {other}")),
    };
    let reply = harness
        .state
        .mailbox()
        .store()
        .ok_or("the store is not open")?
        .set_flags(vec![id], add, remove);
    reply.recv().await.map_err(|error| error.to_string())?;
    settled(harness).await
}

pub(super) async fn remove_ids(harness: &Harness, ids: Vec<MessageId>) -> StepResult {
    let mailbox = harness.state.mailbox();
    let reply = mailbox.store().ok_or("the store is not open")?.remove(ids);
    let removed = reply.recv().await.map_err(|error| error.to_string())?;
    if let Some(blobs) = mailbox.blobs() {
        let orphans = removed.orphaned_blobs;
        blocking(move || {
            for key in orphans {
                let _ = blobs.remove(&key);
            }
        })
        .await?;
    }
    Ok(())
}

/// `mail-remove POSITION`.
async fn remove(harness: &Harness, position: &str) -> StepResult {
    let list = list(harness)?;
    let id = row_id(&list, position)?;
    remove_ids(harness, vec![id]).await?;
    settled(harness).await
}

/// SplitMix64, for repeatable churn.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Stores fictional messages in one ingest: one commit, one change event.
async fn ingest_batch(
    harness: &Harness,
    messages: Vec<RawMessage>,
) -> Result<Vec<MessageId>, String> {
    let mailbox = harness.state.mailbox();
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let prepared = blocking(move || ingest::prepare(&blobs, messages, 4))
        .await?
        .map_err(|error| error.to_string())?;
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .ingest(prepared.messages);
    let ingested = reply.recv().await.map_err(|error| error.to_string())?;
    Ok(ingested.ids.into_iter().flatten().collect())
}

/// A random fictional message for `mail-churn`, dated within 60 days before `now`; a third
/// answer one of the earlier ones.
fn churn_message(rng: &mut Rng, answered: &mut Vec<u32>, folder: FolderId, now: i64) -> RawMessage {
    let serial = ADDED.with(|added| {
        added.set(added.get() + 1);
        added.get()
    });
    let date = now - rng.below(60 * 86_400) as i64;
    let reply_to = (!answered.is_empty() && rng.below(3) == 0)
        .then(|| answered[rng.below(answered.len() as u64) as usize]);
    answered.push(serial);
    let mut flags = Flags::NONE;
    if rng.below(2) == 0 {
        flags = flags | Flags::SEEN;
    }
    if rng.below(8) == 0 {
        flags = flags | Flags::FLAGGED;
    }
    let story = Fiction {
        subject: format!("Churn {serial}"),
        from: ["Ingrid Solberg", "Marius Dahl", "Leah Kim"][rng.below(3) as usize].to_owned(),
        text: "En melding fra selvtestens tilfeldige endringer.".to_owned(),
        attachment: rng.below(5) == 0,
        reply_to,
    };
    RawMessage {
        folder,
        flags,
        internal_date: Some(date),
        raw: raw_message(&story, date, serial),
        ..RawMessage::default()
    }
}

/// `mail-churn COUNT SEED`: COUNT random changes through the app's store while the list is
/// open, without waiting for the list in between: single messages at random dates (some deep in
/// the folder, some answering earlier ones), batches of 2 to 8 and, now and then, of 70 (the
/// store then tells open views to reload), flag changes and removals of loaded rows.
async fn churn(harness: &Harness, count: usize, seed: u64) -> StepResult {
    let list = list(harness)?;
    let folder = match list.scope() {
        Some(ViewScope::Folder(folder)) => folder,
        _ => find_folder(harness, "inbox")?.id,
    };
    let now = list.now_unix();
    let mut rng = Rng(seed);
    let mut answered: Vec<u32> = Vec::new();
    let (mut inserted, mut batches, mut reloads, mut flagged, mut removed) = (0, 0, 0, 0, 0);
    for _ in 0..count {
        let loaded: Vec<MessageId> = (0..list.model().total())
            .filter_map(|position| list.model().row_at(position).map(|row| row.id))
            .collect();
        let pick = rng.below(100);
        if (40..85).contains(&pick) && !loaded.is_empty() {
            let id = loaded[rng.below(loaded.len() as u64) as usize];
            if pick < 70 {
                let row = list
                    .model()
                    .position_of(id)
                    .and_then(|position| list.model().row_at(position));
                let flag = if rng.below(2) == 0 {
                    Flags::SEEN
                } else {
                    Flags::FLAGGED
                };
                let (add, remove) = if row.is_some_and(|row| !row.flags.contains(flag)) {
                    (flag, Flags::NONE)
                } else {
                    (Flags::NONE, flag)
                };
                let reply = harness
                    .state
                    .mailbox()
                    .store()
                    .ok_or("the store is not open")?
                    .set_flags(vec![id], add, remove);
                reply.recv().await.map_err(|error| error.to_string())?;
                flagged += 1;
            } else {
                remove_ids(harness, vec![id]).await?;
                removed += 1;
            }
            continue;
        }
        let size = match pick {
            85..=96 => {
                batches += 1;
                2 + rng.below(7) as usize
            }
            97..=99 => {
                reloads += 1;
                70
            }
            _ => 1,
        };
        let messages = (0..size)
            .map(|_| churn_message(&mut rng, &mut answered, folder, now))
            .collect();
        inserted += ingest_batch(harness, messages).await?.len();
    }
    println!(
        "  # {inserted} messages stored ({batches} batches of 2-8, {reloads} of 70), {flagged} \
         flag changes, {removed} removals; the list has reopened its view {} times",
        list.model().stats().reopens
    );
    settled(harness).await
}

pub(super) fn folder_key_for(harness: &Harness, spec: &str) -> Result<String, String> {
    if spec == ALL_INBOXES {
        Ok(ALL_INBOXES.to_owned())
    } else {
        find_folder(harness, spec).map(|folder| folder_key(folder.id))
    }
}

/// `folder all-inboxes` or `folder [ACCOUNT:]ROLE`, as picking it in the sidebar.
async fn select_folder(harness: &Harness, spec: &str, account: Option<&str>) -> StepResult {
    let spec = match account {
        Some(account) => format!("{account}:{spec}"),
        None => spec.to_owned(),
    };
    let key = folder_key_for(harness, &spec)?;
    harness.window()?.select_folder(&key);
    settled(harness).await
}

/// `list-clock 2026-10-07T12:00:00` (local time) or `list-clock real`.
async fn set_clock(harness: &Harness, when: &str) -> StepResult {
    let list = list(harness)?;
    let unix = match when {
        "real" => None,
        text => Some(
            glib::DateTime::from_iso8601(text, Some(&glib::TimeZone::local()))
                .map_err(|_| format!("not an ISO 8601 time: {text}"))?
                .to_unix(),
        ),
    };
    list.set_clock(unix);
    settled(harness).await
}

/// `list-key KEY`: runs the List scope's shortcut for KEY on the list view, as the key would.
async fn press(harness: &Harness, key: &str) -> StepResult {
    let list = list(harness)?;
    let wanted = gtk::ShortcutTrigger::parse_string(key)
        .ok_or_else(|| format!("GTK can't parse the key {key}"))?
        .to_str();
    let shortcut = {
        let keys = list.keys().ok_or("the list has no keys")?;
        let model = keys.model();
        (0..model.n_items())
            .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
            .find(|shortcut| {
                shortcut
                    .trigger()
                    .is_some_and(|trigger| trigger.to_str() == wanted)
            })
            .ok_or_else(|| format!("{key} is none of the list's keys"))?
    };
    let action = shortcut.action().ok_or("the shortcut has no action")?;
    action.activate(
        gtk::ShortcutActionFlags::empty(),
        list.list_view(),
        None::<&glib::Variant>,
    );
    settled(harness).await
}

/// The folder counts the list should match.
fn folder_counts(harness: &Harness, scope: ViewScope) -> (u32, u32) {
    let folders = harness.state.mailbox().folders();
    folders
        .iter()
        .filter(|folder| match scope {
            ViewScope::Folder(id) => folder.id == id,
            ViewScope::UnifiedInbox => folder.role == FolderRole::Inbox,
        })
        .fold((0, 0), |(total, unread), folder| {
            (total + folder.total, unread + folder.unread)
        })
}

async fn check(harness: &Harness, assertion: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    let list = list(harness)?;
    let model = list.model();
    match assertion {
        "assert-list-count" => equal("rows", model.total(), number(arg(0))? as u32),
        "assert-list-row" => {
            let position = number(arg(0))? as u32;
            let row = model
                .row_at(position)
                .ok_or_else(|| format!("row {position} isn't loaded"))?;
            let shown = || {
                list.row_widget(position)
                    .map(|row| row.texts())
                    .ok_or_else(|| format!("row {position} isn't bound"))
            };
            let value = match arg(1) {
                "sender" => shown()?[0].clone(),
                "subject" => shown()?[1].clone(),
                "snippet" => shown()?[2].clone(),
                "time" => shown()?[3].clone(),
                "section" => model.section_label(position).to_owned(),
                "unread" => (!row.flags.is_seen()).to_string(),
                "starred" => row.flags.is_flagged().to_string(),
                "attachment" => row.has_attachments.to_string(),
                "threads" => row.thread_count.to_string(),
                other => return Err(format!("no field {other}")),
            };
            equal(
                &format!("row {position} {}", arg(1)),
                value.as_str(),
                arg(2),
            )
        }
        "assert-list-all" => {
            let wanted = super::script::boolean(arg(1))?;
            let rows: Vec<_> = (0..model.total())
                .filter_map(|position| model.row_at(position))
                .collect();
            if rows.is_empty() {
                return Err("no row is loaded".to_owned());
            }
            let field = |row: &sealwax_domain::model::MessageSummary| match arg(0) {
                "unread" => Ok(!row.flags.is_seen()),
                "starred" => Ok(row.flags.is_flagged()),
                "attachment" => Ok(row.has_attachments),
                other => Err(format!("no field {other}")),
            };
            for row in &rows {
                if field(row)? != wanted {
                    return Err(format!("a loaded row's {} isn't {wanted}", arg(0)));
                }
            }
            println!("  # {} loaded rows checked", rows.len());
            Ok(())
        }
        "assert-list-sections" => {
            let labels = crate::list::sections::LABELS;
            let actual: Vec<String> = model
                .section_counts()
                .iter()
                .zip(labels)
                .filter(|(count, _)| **count > 0)
                .map(|(count, label)| format!("{label}:{count}"))
                .collect();
            equal("sections", actual.join(",").as_str(), arg(0))
        }
        "assert-list-headers" => equal("headers", list.headers().join(",").as_str(), arg(0)),
        "assert-list-selected" => {
            let selected = list
                .selected_position()
                .map_or("none".to_owned(), |position| position.to_string());
            equal("selected row", selected.as_str(), arg(0))
        }
        "assert-list-remembered" => {
            let remembered = REMEMBERED.with(Cell::get).ok_or("nothing was remembered")?;
            equal("selected message", list.selected(), Some(remembered))
        }
        "assert-list-scrolled" => {
            let value = list.scroller().vadjustment().value();
            let least = arg(0)
                .parse::<f64>()
                .map_err(|_| format!("expected pixels, got {}", arg(0)))?;
            println!("  # the list is at {value} px");
            if value >= least {
                Ok(())
            } else {
                Err(format!("the list is at {value} px, above {least}"))
            }
        }
        "assert-list-position" => {
            let id = list.selected().ok_or("nothing is selected")?;
            let (n, total) = list
                .position(id)
                .ok_or("the selected message has no position")?;
            equal("position", format!("{n} of {total}").as_str(), arg(0))
        }
        "assert-list-growth" => {
            let frames = list.growth_frames();
            let first_most = number(arg(0))? as u32;
            println!("  # rows bound after each frame: {frames:?}");
            println!("  # row widgets created so far: {}", list.rows_created());
            let first = *frames
                .first()
                .ok_or("no frame was painted after the reset")?;
            if first == 0 || first > first_most {
                return Err(format!(
                    "the first frame bound {first} rows, wanted 1 to {first_most}"
                ));
            }
            let steps_ok = frames
                .windows(2)
                .all(|pair| pair[1] >= pair[0] && pair[1] - pair[0] <= crate::list::GROW_STEP + 8);
            if !steps_ok {
                return Err("the list grew by more than one step in a frame".to_owned());
            }
            Ok(())
        }
        "assert-list-top" => {
            let value = list.scroller().vadjustment().value();
            let in_view = list.row_widget(0).is_some_and(|top| {
                top.is_mapped()
                    && top.compute_bounds(list.scroller()).is_some_and(|bounds| {
                        bounds.y() >= 0.0
                            && bounds.y() + bounds.height() <= list.scroller().height() as f32
                    })
            });
            println!("  # the list is at {value} px; row 0 in view: {in_view}");
            equal(
                "at the top",
                value == 0.0 && in_view,
                super::script::boolean(arg(0))?,
            )
        }
        "assert-list-focus" => {
            let window = harness.window()?;
            let focus = GtkWindowExt::focus(window.window()).ok_or("nothing has the focus")?;
            let within = |widget: &gtk::Widget| &focus == widget || focus.is_ancestor(widget);
            let actual = if within(list.search_entry().upcast_ref()) {
                "search"
            } else if within(list.list_view().upcast_ref()) {
                "list"
            } else {
                "elsewhere"
            };
            equal("focus", actual, arg(0))
        }
        "assert-list-placeholders" => equal(
            "placeholder rows",
            list.bound_rows()
                .iter()
                .filter(|row| row.placeholder)
                .count(),
            number(arg(0))?,
        ),
        "assert-list-empty" => {
            let title = list.empty_state().map(|(title, _)| title);
            equal("empty state", title.as_deref().unwrap_or("none"), arg(0))
        }
        "assert-list-search" => {
            equal("results", model.total(), number(arg(0))? as u32)?;
            equal(
                "more results",
                model.has_more_results(),
                super::script::boolean(arg(1))?,
            )
        }
        "assert-list-matches-folder" => {
            let scope = list.scope().ok_or("the list shows no folder")?;
            let (total, unread) = folder_counts(harness, scope);
            let wanted = match arg(0) {
                "total" => total,
                "unread" => unread,
                other => return Err(format!("use total or unread, not {other}")),
            };
            println!("  # folder: {total} messages, {unread} unread");
            equal("rows", model.total(), wanted)
        }
        "assert-sidebar-count" => {
            let folder = find_folder(harness, arg(0))?;
            let model = harness.state.mailbox().sidebar_model();
            let key = folder_key(folder.id);
            let count = model
                .find(&key)
                .map(|(_, folder)| folder.count)
                .ok_or("the sidebar lacks the folder")?;
            equal("sidebar count", count, number(arg(1))? as u32)
        }
        "assert-list-exact" => exact(harness, &list).await,
        "assert-action-enabled" => {
            let name = arg(0);
            let window = harness.window()?;
            let enabled = match name.split_once('.') {
                Some(("win", action)) => window.window().lookup_action(action),
                Some(("app", action)) => harness.app.lookup_action(action),
                _ => return Err(format!("use win.NAME or app.NAME, not {name}")),
            }
            .ok_or_else(|| format!("no action {name}"))?
            .is_enabled();
            equal(name, enabled, super::script::boolean(arg(1))?)
        }
        "assert-selection-reported" => {
            let reported = REPORTED.with(|reported| reported.borrow().clone());
            let reported = reported.map_or(0, |list| list.borrow().len());
            equal("selections reported", reported, number(arg(0))?)
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// `assert-list-exact`: the whole view read fresh from the store at the version the list has
/// applied equals every row the list has loaded, and the list's section counts equal the fresh
/// rows' (the check the store's `change_events_keep_list_models_exact` makes for a plain model).
async fn exact(harness: &Harness, list: &MessageList) -> StepResult {
    let model = list.model();
    let crate::list::Content::View(spec) = model.content() else {
        return Err("the list shows no store view".to_owned());
    };
    let started = Instant::now();
    loop {
        let version = model.version().ok_or("the view is still opening")?;
        let total = model.total();
        let reply = harness
            .state
            .mailbox()
            .store()
            .ok_or("the store is not open")?
            .page(spec, 0, total.max(1));
        let page = reply.recv().await.map_err(|error| error.to_string())?;
        if page.version != version || model.version() != Some(version) {
            if started.elapsed() > WAIT_TIMEOUT {
                return Err(format!(
                    "the list stayed at version {} while the store is at {}",
                    version.0, page.version.0
                ));
            }
            glib::timeout_future(Duration::from_millis(20)).await;
            continue;
        }
        equal("rows", page.rows.len(), total as usize)?;
        let mut loaded = 0;
        for (position, fresh) in page.rows.iter().enumerate() {
            if let Some(row) = model.row_at(position as u32) {
                loaded += 1;
                if row != *fresh {
                    return Err(format!(
                        "row {position} differs from the store's: {:?} vs {:?}",
                        (row.id, row.flags, row.thread_count),
                        (fresh.id, fresh.flags, fresh.thread_count)
                    ));
                }
            }
        }
        let sections = crate::list::sections::Bounds::from_dates(
            page.rows.iter().map(|row| row.date),
            &model.thresholds(),
        );
        equal("section counts", model.section_counts(), sections.counts())?;
        println!(
            "  # {loaded} loaded rows of {total} equal the store's at version {}",
            version.0
        );
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_dates_are_rfc_5322_in_english() {
        assert_eq!(mail_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(mail_date(1_791_072_000), "Sun, 04 Oct 2026 00:00:00 +0000");
    }

    #[test]
    fn fictional_messages_parse() {
        let fiction = Fiction {
            subject: "Hytteturen".to_owned(),
            from: "Ingrid Solberg".to_owned(),
            text: "Blåbærsyltetøy til frokost.".to_owned(),
            attachment: true,
            reply_to: Some(3),
        };
        let raw = raw_message(&fiction, 1_791_072_000, 7);
        let parsed = sealwax_domain::message::parse(&raw);
        assert_eq!(parsed.headers.subject, "Hytteturen");
        assert_eq!(parsed.headers.from[0].email, "ingridsolberg@example.test");
        assert_eq!(parsed.headers.date, Some(1_791_072_000));
        assert!(parsed.has_attachments());
        assert_eq!(parsed.headers.in_reply_to, ["selftest-3@example.test"]);
        assert!(parsed.body_text.contains("Blåbærsyltetøy"));
    }

    #[test]
    fn every_command_is_named_once() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|(name, _, _)| *name).collect();
        names.extend(
            super::super::script::COMMANDS
                .iter()
                .chain(super::super::perf::COMMANDS)
                .map(|(name, _, _)| *name),
        );
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
