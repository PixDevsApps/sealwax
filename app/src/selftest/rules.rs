//! Self-test commands for the rules in the app (M9, ADR-020): the Rules dialog and its editor,
//! the message row's menu, the rules file and the engine's rules events. The dialog's widgets
//! are driven with the setup assistant's `setup-type`, `setup-choose` and `setup-click` and the
//! real clicks of `click`; its toasts with `assert-toast`; the file with `write-file`,
//! `assert-file-contains` and `assert-file-lacks` under `$TMP/config/sealwax/rules/`. Every
//! name and address is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `rules-opener` | Open File records the paths it would open instead of opening them |
//! | `rules-wait` | Waits until the Rules dialog has saved every change |
//! | `rules-failed KEY LINE COLUMN TEXT`, `rules-failed KEY file` | `Event::RulesFailed` for the account with KEY: a script error at LINE and COLUMN saying TEXT, or a file that can't be read |
//! | `rules-applied N\|store\|file` | `Event::RulesApplied` for the oldest Apply Rules request waiting: N messages changed, or why the rules didn't run |
//! | `row-menu POS` | Right-clicks the list's row POS through Broadway (opened as the gesture opens it when the click doesn't reach it headless) |
//! | `row-menu-click NAME` | Clicks the open message menu's item NAME (`message-menu-junk-sender`) through Broadway |
//! | `assert-rules-opened NAME\|none` | The file name Open File last opened |
//! | `assert-rules-dialog closed\|rules\|edit\|apply` | The Rules dialog's page on top, or that it is closed |
//! | `assert-rules-set SPEC\|none` | The rules the dialog holds, in order: `Name:on`, `Name:off`, `#hand` for Sieve written by hand, comma-separated |
//! | `assert-rules-senders blocked\|trusted LIST\|none` | The dialog's always-junk or never-junk senders, addresses then domains |
//! | `assert-rule-row N TITLE SUMMARY` | The dialog's rule row N shows TITLE and SUMMARY |
//! | `assert-rules-problem TEXT\|none` | Why the open editor's Save can't run |
//! | `assert-rules-banner ACCOUNT_ID TEXT\|none` | What the app holds as the reason the account's rules can't run |
//! | `assert-row-menu open\|closed` | Whether the message menu is open |
//! | `assert-row-menu-items NAMES` | The open message menu's items in order, by their titles, `!` before an insensitive one (`Reply,!Reply All`) |

use super::{Harness, StepResult, WAIT_TIMEOUT, equal, number};
use crate::rules;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::model::AccountId;
use sealwax_domain::sieve::{self, Item};
use sealwax_engine::api::{Event, RulesError};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("rules-opener", 0, 0),
    ("rules-wait", 0, 0),
    ("rules-failed", 2, 4),
    ("rules-applied", 1, 1),
    ("row-menu", 1, 1),
    ("row-menu-click", 1, 1),
    ("assert-rules-opened", 1, 1),
    ("assert-rules-dialog", 1, 1),
    ("assert-rules-set", 1, 1),
    ("assert-rules-senders", 2, 2),
    ("assert-rule-row", 3, 3),
    ("assert-rules-problem", 1, 1),
    ("assert-rules-banner", 2, 2),
    ("assert-row-menu", 1, 1),
    ("assert-row-menu-items", 1, 1),
];

thread_local! {
    /// The paths Open File would have opened since `rules-opener`.
    static OPENED: RefCell<Option<Rc<RefCell<Vec<PathBuf>>>>> = const { RefCell::new(None) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "rules-opener" => {
            let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
            let record = opened.clone();
            rules::set_opener(move |path| record.borrow_mut().push(path.to_path_buf()));
            OPENED.with(|current| current.replace(Some(opened)));
            Ok(())
        }
        "rules-wait" => {
            let started = Instant::now();
            while rules::dialog::current().is_some_and(|shown| shown.is_saving()) {
                if started.elapsed() > WAIT_TIMEOUT {
                    return Err("the Rules dialog never finished saving".to_owned());
                }
                glib::timeout_future(Duration::from_millis(10)).await;
            }
            harness.wait_until_settled().await;
            Ok(())
        }
        "rules-failed" => {
            let account = account_with_key(harness, arg(0))?;
            let error = match args.len() {
                2 if arg(1) == "file" => RulesError::File,
                4 => RulesError::Script(sieve::Error {
                    line: number(arg(1))? as u32,
                    column: number(arg(2))? as u32,
                    message: arg(3).to_owned(),
                }),
                _ => return Err("use rules-failed KEY LINE COLUMN TEXT or KEY file".to_owned()),
            };
            super::setup::emit(harness, Event::RulesFailed { account, error }).await
        }
        "rules-applied" => {
            let request = *rules::pending_applies()
                .first()
                .ok_or("no Apply Rules request is waiting")?;
            let result = match arg(0) {
                "store" => Err(RulesError::Store),
                "file" => Err(RulesError::File),
                count => Ok(number(count)? as u32),
            };
            let folder = sealwax_domain::model::FolderId(0);
            super::setup::emit(
                harness,
                Event::RulesApplied {
                    request,
                    folder,
                    result,
                },
            )
            .await
        }
        "row-menu" => {
            let row = super::triage::row_widget(harness, arg(0))?;
            harness.wait_until_settled().await;
            super::triage::click_widget(harness, row.upcast_ref(), 0, 3).await?;
            if crate::message_menu::current().is_none() {
                // Headless, Broadway's synthetic right click may not reach the gesture (as
                // `sidebar-menu` says); the menu opens as the gesture opens it.
                println!("  # opened as the gesture does");
                let list = super::mail::list(harness)?;
                let position = number(arg(0))? as u32;
                if !list.open_context_menu(position, None) {
                    return Err(format!("row {position} has no menu"));
                }
            }
            harness.wait_until_settled().await;
            match crate::message_menu::current() {
                Some(_) => Ok(()),
                None => Err("the message menu closed at once".to_owned()),
            }
        }
        "row-menu-click" => {
            let item = crate::message_menu::items()
                .into_iter()
                .find(|item| item.widget_name() == arg(0))
                .ok_or_else(|| format!("the message menu has no {}", arg(0)))?;
            if !item.is_sensitive() {
                return Err(format!("{} is insensitive", arg(0)));
            }
            super::triage::click_button(harness, &item).await?;
            // The command runs once the menu has put the focus back.
            harness.wait_until_settled().await;
            glib::timeout_future(Duration::from_millis(20)).await;
            harness.wait_until_settled().await;
            Ok(())
        }
        _ => check(harness, command, args),
    }
}

fn check(harness: &Harness, assertion: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match assertion {
        "assert-rules-opened" => {
            let opened = OPENED
                .with(|opened| opened.borrow().clone())
                .ok_or("run rules-opener first")?;
            let last = opened
                .borrow()
                .last()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "none".to_owned());
            equal("the file Open File opened", last.as_str(), arg(0))
        }
        "assert-rules-dialog" => {
            let page = rules::dialog::current().map_or_else(|| "closed".to_owned(), |shown| shown.page());
            equal("the Rules dialog", page.as_str(), arg(0))
        }
        "assert-rules-set" => {
            let set = shown_set()?;
            let shown: Vec<String> = set
                .items
                .iter()
                .map(|item| match item {
                    Item::Rule(rule) => format!(
                        "{}:{}",
                        rules::text::rule_title(rule),
                        if rule.enabled { "on" } else { "off" }
                    ),
                    Item::HandWritten(_) => "#hand".to_owned(),
                })
                .collect();
            let shown = if shown.is_empty() {
                "none".to_owned()
            } else {
                shown.join(",")
            };
            equal("the rules", shown.as_str(), arg(0))
        }
        "assert-rules-senders" => {
            let set = shown_set()?;
            let senders = match arg(0) {
                "blocked" => &set.blocked,
                "trusted" => &set.trusted,
                other => return Err(format!("use blocked or trusted, not {other}")),
            };
            let listed: Vec<&str> = senders
                .addresses
                .iter()
                .chain(&senders.domains)
                .map(String::as_str)
                .collect();
            let listed = if listed.is_empty() {
                "none".to_owned()
            } else {
                listed.join(",")
            };
            equal(&format!("the {} senders", arg(0)), listed.as_str(), arg(1))
        }
        "assert-rule-row" => {
            let name = format!("rule-row-{}", arg(0));
            let row = harness
                .widget(&name)?
                .downcast::<adw::ActionRow>()
                .map_err(|_| format!("{name} is no rule row"))?;
            equal(&format!("{name}'s title"), row.title().as_str(), arg(1))?;
            equal(
                &format!("{name}'s summary"),
                row.subtitle().unwrap_or_default().as_str(),
                arg(2),
            )
        }
        "assert-rules-problem" => {
            let shown = rules::dialog::current().ok_or("the Rules dialog isn't open")?;
            equal(
                "why Save can't run",
                shown.editor_problem().unwrap_or("none"),
                arg(0),
            )
        }
        "assert-rules-banner" => {
            let account = AccountId(number(arg(0))? as i64);
            let problem = rules::problems()
                .into_iter()
                .find(|(id, _)| *id == account)
                .map_or_else(|| "none".to_owned(), |(_, problem)| problem);
            equal("why the rules can't run", problem.as_str(), arg(1))
        }
        "assert-row-menu" => {
            let open = if crate::message_menu::current().is_some() {
                "open"
            } else {
                "closed"
            };
            equal("the message menu", open, arg(0))
        }
        "assert-row-menu-items" => {
            let items: Vec<String> = crate::message_menu::items()
                .iter()
                .map(|item| {
                    let mut title = String::new();
                    super::collect(item.upcast_ref(), &mut |widget| {
                        if title.is_empty()
                            && let Some(label) = widget.downcast_ref::<gtk::Label>()
                        {
                            title = label.label().to_string();
                        }
                    });
                    if item.is_sensitive() {
                        title
                    } else {
                        format!("!{title}")
                    }
                })
                .collect();
            equal("the message menu's items", items.join(",").as_str(), arg(0))
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// The rules the open dialog holds.
fn shown_set() -> Result<sieve::RuleSet, String> {
    rules::dialog::current()
        .ok_or("the Rules dialog isn't open")?
        .rule_set()
        .ok_or_else(|| "the Rules dialog shows no rules".to_owned())
}

fn account_with_key(harness: &Harness, key: &str) -> Result<AccountId, String> {
    harness
        .state
        .mailbox()
        .accounts()
        .into_iter()
        .find(|account| account.key.as_ref().is_some_and(|known| known.0 == key))
        .map(|account| account.id)
        .ok_or_else(|| format!("no account with the key {key}"))
}
