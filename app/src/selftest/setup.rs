//! The setup assistant's self-test commands (ML-033): a fake engine on the app's
//! [`EngineLink`](crate::engine_link::EngineLink) that the script answers with scripted events,
//! and the assistant driven as a user would. Every address, name and server is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `engine-attach`, `engine-detach` | Connects the fake engine, or leaves the app without one; links the assistant opens are recorded from then on, never opened |
//! | `engine-start CA` | Starts the real engine instead, trusting only the CA file (the mail testbed's) and keeping secrets in memory |
//! | `engine-expect detect\|verify\|read-certificate\|add-account\|edit-account\|sign-in\|cancel-sign-in\|update-password\|reconnect\|check-for-updates [KEY=VALUE…]` | Waits for the app's next command and checks its fields: `address`, `name`, `label` and `color` (`none` for none), `key`, `imap` (or `incoming`, the same: IMAP's or POP3's server) and `smtp` (`host:port:tls\|starttls`, then `:pinned:` and the pinned certificate's first four bytes in hex when it has one; `read-certificate`'s is `server`, with `protocol` `imap`, `pop3` or `smtp`), `receive` (`imap`, `pop3:leave`, `pop3:leave:N`, `pop3:delete`; M8), `username`, `auth` (`password`, `microsoft`), `second` (`none`), `account` (a key, `none`), `provider`, `sign-in` (`last`: the last `SignIn`'s request; `none`), `password` (compared, never printed) |
//! | `engine-expect none [but KIND,…]` | The app sent nothing more, or only commands of those kinds (`load-older,folder-opened`: what the list sends by itself) |
//! | `engine-reply progress SOURCE` | `DetectProgress` for the last command: `preset`, `autoconfig`, `ispdb`, `mx`, `srv`, `probe` |
//! | `engine-reply detected FIXTURE` | `Detected`: `fjordpost`, `fastmail`, `icloud`, `imap-off`, `gmail`, `confirm`, `other-username`, `microsoft`, `bridge` (Proton Mail Bridge on 127.0.0.1), `no-imap`, `no-mail`, `not-found` |
//! | `engine-reply verified ok [USERNAME]`, `engine-reply verified imap\|pop3\|smtp FAILURE` | `Verified`: `wrong-password`, `app-password`, `imap-disabled`, `oauth`, `sign-in-declined`, `no-tls`, `certificate`, `unreachable`, `timeout`, `other` |
//! | `engine-reply certificate-read ok BYTE`, `engine-reply certificate-read FAILURE` | `CertificateRead`: a fingerprint of 32 times BYTE (two hex digits), or a failure as `verified`'s (issue #9) |
//! | `engine-reply sign-in-open URL` | `SignInOpen` for the last command |
//! | `engine-reply signed-in ok USERNAME`, `engine-reply signed-in [imap\|smtp] FAILURE` | `SignedIn` (IMAP when no protocol is named) |
//! | `engine-reply added ok\|keyring\|settings-file\|store\|exists\|missing` | `AccountAdded`; `ok` first adds the account (its key is the address's local part) and its role folders to the app's store, as the engine would; `added ok file` also appends it to the scratch `accounts.toml` through the engine's writer and waits for the app to read it (M8) |
//! | `engine-reply edited ok [file]\|settings-file\|missing` | `AccountEdited`; `ok file` first writes the label and colour of the last `EditAccount` to the scratch `accounts.toml` through the engine's writer, waits for the app to read them, and names the store's account by its title as the engine's reconcile would |
//! | `engine-reply removed ok\|keyring\|settings-file\|store\|missing` | `AccountRemoved`; `ok` first removes the account from the app's store |
//! | `engine-reply sync-failed ADDRESS FAILURE`, `engine-reply sync-finished ADDRESS` | `AccountStatus` for the store's account with ADDRESS: the failure's state, or `Synced` |
//! | `engine-reply status ADDRESS STATE` | `AccountStatus` for that account: `syncing`, `synced`, `offline`, `needs-sign-in`, `certificate`, `quota` (ML-048) |
//! | `engine-reply keyring-unavailable` | `KeyringUnavailable` |
//! | `engine-reply ops-refused ADDRESS flags\|move\|expunge\|folder-read N` | `OpsRefused` for that account: the server refused N messages' change for good (ML-051) |
//! | `engine-reply password-updated ok`, `engine-reply password-updated [imap\|pop3\|smtp] FAILURE` | `PasswordUpdated` for the last command (IMAP when no protocol is named) |
//! | `desktop-network up\|down`, `desktop-resume` | What the network monitor and logind would report, through the app's own handler |
//! | `engine-reply body-fetched ok\|offline\|needs-sign-in\|gone\|store\|other` | `BodyFetched` for the first message of the last `fetch-bodies` taken (its fields: `messages`, `count`); storing the body is `bodies-store`'s (`bodies.rs`) |
//! | `engine-reply older-loaded FETCHED complete\|more` | `OlderLoaded` for the folder of the last `load-older` taken (its field: `folder`; `folder-opened` has the same) (ML-043) |
//! | `engine-reply part-fetched ok\|offline\|needs-sign-in\|gone\|store\|other` | `PartFetched` for the last `fetch-part` taken (its fields: `message`, `part`); `ok` carries the part of the message `files.rs` stored (ML-061) |
//! | `engine-reply original-fetched ok\|offline\|needs-sign-in\|gone\|store\|other` | `OriginalFetched` for the last `fetch-original` taken (its field: `message`); `ok` carries the message `files.rs` stored, whole (ML-063) |
//! | `engine-reply folder-changed ok\|offline\|taken\|not-allowed\|missing\|lost\|store`, `engine-reply folder-changed refused TEXT` | `FolderChanged` for the last `create-folder`, `rename-folder` or `delete-folder` taken (their fields: `account`, `parent` (`none`), `folder`, `name`); `ok` first makes the change in the app's store as the engine would (`folders.rs`); `refused` carries TEXT as the server's words (M9) |
//! | `engine-reply update-checked VERSION\|running\|offline\|timeout\|certificate\|busy\|no-release\|unreadable` | `UpdateChecked` for the last `check-for-updates` taken: that release (`running`: this build's version) or the failure |
//! | `engine-reply-stale …` | The same events with a request id the app never used |
//! | `setup-type NAME TEXT`, `setup-choose NAME ITEM`, `setup-click NAME` | Types into a field, picks a choice's item by its text (a security's `tls`, `starttls`; Incoming mail's `imap`, `pop3`), clicks a visible, sensitive button |
//! | `setup-key enter\|escape` | Enter in the focused field, Escape in the dialog |
//! | `setup-respond NAME RESPONSE` | Answers the alert dialog named NAME (Remove Account's `remove-account`) as its button would |
//! | `setup-dismiss-toasts` | Closes the toasts shown and queued, as their × would (for previews) |
//! | `setup-still-waiting-after MS` | Shortens the minute a Microsoft sign-in waits before the note on admin approval shows, for the sign-ins started from then on |
//! | `assert-setup open\|step\|sources\|focus VALUE` | The assistant's state |
//! | `assert-setup client-id set\|none` | Whether a Microsoft client ID is in use (`config.toml`; self-tests have no built-in one) |
//! | `assert-setup sign-in-needed KEY,…\|none` | The accounts the window's banner asks to sign in again, in order |
//! | `assert-setup banner-queue ENTRY,…\|none` | Everything the banner has, in order: `keyring`, `certificate:KEY`, `sign-in:KEY`, `password:KEY`, `quota:KEY`, `rules:KEY`, `offline`, `unreachable` |
//! | `assert-opened last URL\|none`, `assert-opened count N` | The last link handed to the browser opener, and how many were |
//! | `assert-toast TEXT` | The toast on screen says TEXT (`setup-dismiss-toasts` clears earlier ones) |
//! | `assert-clipboard TEXT` | The text Sealwax last put on the clipboard is TEXT |
//! | `assert-text-contains NAME TEXT` | A label's, a button's or an alert dialog's text (its heading and body) contains TEXT |
//! | `assert-field NAME TEXT` | A text field's text or a choice's item (never the password's) |
//! | `assert-account-status ADDRESS synced\|syncing\|offline\|needs-sign-in\|certificate\|quota` | The sidebar's state of the account with ADDRESS |
//! | `assert-account-folders ADDRESS TITLE,…` | The folders the sidebar shows for the account with ADDRESS, role folders first, by the titles it shows (M8) |

use super::{Harness, StepResult, WAIT_TIMEOUT, equal, number, with_timeout};
use crate::desktop::DesktopEvent;
use crate::engine::EngineOptions;
use crate::preferences::SettingsFile;
use crate::setup::{self, sources};
use crate::status::Waiting;
use crate::window::Attention;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::{
    AccountColor, AccountKey, AccountSettings, AuthMethod, CertificatePin, DetectSource,
    DetectedSettings, Detection, Endpoint, HelpLink, LoginError, LoginFailure, OAuthProvider,
    Pop3Policy, Protocol, Receive, Security, UnsupportedReason, UsernameForm,
};
use sealwax_domain::folders::NameProblem;
use sealwax_domain::model::{AccountId, FolderId, FolderRole, MessageId};
use sealwax_domain::updates::Version;
use sealwax_engine::api::{
    AccountError, BodyError, Command, Event, FetchedPart, FolderChange, FolderError, Original,
    Password, Refused, RequestId, SyncStatus, UpdateError,
};
use sealwax_engine::imap::ServerText;
use sealwax_engine::net::Trust;
use sealwax_engine::service::SecretBackend;
use sealwax_infrastructure::store::{FolderSpec, NewAccount};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("engine-attach", 0, 0),
    ("engine-detach", 0, 0),
    ("engine-start", 1, 1),
    ("engine-expect", 1, 12),
    ("engine-reply", 1, 4),
    ("engine-reply-stale", 2, 3),
    ("setup-type", 2, 2),
    ("setup-choose", 2, 2),
    ("setup-click", 1, 1),
    ("setup-key", 1, 1),
    ("setup-respond", 2, 2),
    ("setup-dismiss-toasts", 0, 0),
    ("setup-still-waiting-after", 1, 1),
    ("assert-setup", 2, 2),
    ("assert-text-contains", 2, 2),
    ("assert-field", 2, 2),
    ("assert-account-status", 2, 2),
    ("assert-account-folders", 2, 2),
    ("assert-opened", 2, 2),
    ("assert-toast", 1, 1),
    ("assert-clipboard", 1, 1),
    ("desktop-network", 1, 1),
    ("desktop-resume", 0, 0),
];

pub fn handles(command: &str) -> bool {
    command.starts_with("engine-")
        || command.starts_with("setup-")
        || command.starts_with("desktop-")
}

pub fn checks(assertion: &str) -> bool {
    matches!(
        assertion,
        "assert-setup"
            | "assert-text-contains"
            | "assert-field"
            | "assert-account-status"
            | "assert-account-folders"
            | "assert-opened"
            | "assert-toast"
            | "assert-clipboard"
    )
}

/// The last command the script took from the app, for the replies.
#[derive(Clone)]
struct Last {
    request: RequestId,
    settings: Option<AccountSettings>,
    key: Option<AccountKey>,
    /// An `EditAccount`'s label and colour, for `key`.
    look: Option<(Option<String>, Option<AccountColor>)>,
    /// A `ChangeFolder`'s account and change.
    folder: Option<(AccountId, FolderChange)>,
}

struct Fake {
    commands: async_channel::Receiver<Command>,
    events: async_channel::Sender<Event>,
    last: RefCell<Option<Last>>,
    /// The request of the last `SignIn` taken, which `CancelSignIn` and `AddAccount` name.
    last_sign_in: Cell<Option<RequestId>>,
}

thread_local! {
    static FAKE: RefCell<Option<Rc<Fake>>> = const { RefCell::new(None) };
    /// The links the assistant handed to the browser opener, oldest first.
    static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// The messages of the last `FetchBodies` taken (ML-044).
    static REQUESTED: RefCell<Vec<MessageId>> = const { RefCell::new(Vec::new()) };
    /// The folder of the last `LoadOlder` taken (ML-043).
    static OLDER: Cell<Option<FolderId>> = const { Cell::new(None) };
    /// The message and part of the last `FetchPart` taken (ML-061).
    static PART: Cell<Option<(MessageId, u32)>> = const { Cell::new(None) };
    /// The message of the last `FetchOriginal` taken (ML-063).
    static ORIGINAL: Cell<Option<MessageId>> = const { Cell::new(None) };
}

/// The messages the last `FetchBodies` the script took asked for.
pub(super) fn last_requested() -> Vec<MessageId> {
    REQUESTED.with(|requested| requested.borrow().clone())
}

/// The assistant's links go to the record instead of a browser.
fn record_links() {
    setup::browser::set_opener(|_, uri| {
        OPENED.with(|opened| opened.borrow_mut().push(uri.to_owned()));
    });
}

fn fake() -> Result<Rc<Fake>, String> {
    FAKE.with(|fake| fake.borrow().clone())
        .ok_or_else(|| "run engine-attach first".to_owned())
}

fn assistant() -> Result<setup::Assistant, String> {
    setup::current().ok_or_else(|| "the setup assistant isn't open".to_owned())
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    match command {
        "engine-attach" => {
            let (commands, from_app) = async_channel::unbounded();
            let (to_app, events) = async_channel::unbounded();
            harness.state.engine().attach(commands, events);
            FAKE.with(|fake| {
                fake.replace(Some(Rc::new(Fake {
                    commands: from_app,
                    events: to_app,
                    last: RefCell::new(None),
                    last_sign_in: Cell::new(None),
                })))
            });
            record_links();
            Ok(())
        }
        "engine-detach" => {
            harness.state.engine().detach();
            FAKE.with(|fake| fake.take());
            Ok(())
        }
        "engine-start" => {
            FAKE.with(|fake| fake.take());
            record_links();
            harness.state.start_engine(&EngineOptions {
                trust: Trust::Roots(args[0].clone().into()),
                secrets: SecretBackend::Memory,
            });
            if harness.state.engine().is_connected() {
                Ok(())
            } else {
                Err("the engine didn't start".to_owned())
            }
        }
        "engine-expect" => expect(harness, args).await,
        "setup-respond" => {
            let found = harness.widget(&args[0])?;
            let dialog = found
                .downcast_ref::<adw::AlertDialog>()
                .ok_or_else(|| format!("{} is not an alert dialog", args[0]))?;
            if !dialog.has_response(&args[1]) {
                return Err(format!("{} has no response {}", args[0], args[1]));
            }
            dialog.emit_by_name::<()>("response", &[&args[1]]);
            dialog.force_close();
            harness.wait_until_settled().await;
            Ok(())
        }
        "setup-dismiss-toasts" => {
            // libadwaita registers the toast's type with the first toast: none yet, none shown.
            let Some(toast) = glib::Type::from_name("AdwToastWidget") else {
                return Ok(());
            };
            for _ in 0..10 {
                let window = harness.window()?;
                let mut close = None;
                super::collect(window.window().upcast_ref(), &mut |widget| {
                    if close.is_none()
                        && widget.is_mapped()
                        && widget.has_css_class("circular")
                        && widget.ancestor(toast).is_some()
                    {
                        close = widget.downcast_ref::<gtk::Button>().cloned();
                    }
                });
                let Some(close) = close else {
                    return Ok(());
                };
                close.emit_clicked();
                harness.wait_until_settled().await;
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            Err("toasts kept coming".to_owned())
        }
        "engine-reply" => reply(harness, args, false).await,
        "engine-reply-stale" => reply(harness, args, true).await,
        "setup-still-waiting-after" => {
            setup::set_still_waiting_after(Duration::from_millis(number(&args[0])? as u64));
            Ok(())
        }
        "setup-type" => {
            let field = harness.widget(&args[0])?;
            let editable = field
                .dynamic_cast_ref::<gtk::Editable>()
                .ok_or_else(|| format!("{} is not a text field", args[0]))?;
            // As a user would: into the field first, then the text.
            field.grab_focus();
            editable.set_text(&args[1]);
            Ok(())
        }
        "setup-choose" => {
            let field = harness.widget(&args[0])?;
            let dropdown = field
                .downcast_ref::<gtk::DropDown>()
                .ok_or_else(|| format!("{} is not a choice", args[0]))?;
            // The security's TLS or STARTTLS, or "Incoming mail"'s IMAP or POP3 (M8), by name.
            let items = dropdown.model().ok_or("the choice has no items")?;
            let position = (0..items.n_items())
                .find(|&position| {
                    items
                        .item(position)
                        .and_downcast::<gtk::StringObject>()
                        .is_some_and(|item| item.string().eq_ignore_ascii_case(&args[1]))
                })
                .ok_or_else(|| format!("{} offers no {}", args[0], args[1]))?;
            dropdown.set_selected(position);
            Ok(())
        }
        "setup-click" => {
            let found = harness.widget(&args[0])?;
            if found.is::<gtk::LinkButton>() {
                return Err(format!(
                    "{} opens a web page; the self-test never does",
                    args[0]
                ));
            }
            let button = found
                .downcast_ref::<gtk::Button>()
                .ok_or_else(|| format!("{} is not a button", args[0]))?;
            if !button.is_mapped() || !button.is_sensitive() {
                return Err(format!(
                    "{} can't be clicked: shown {}, sensitive {}",
                    args[0],
                    button.is_mapped(),
                    button.is_sensitive()
                ));
            }
            button.emit_clicked();
            Ok(())
        }
        "desktop-network" => {
            let available = match args[0].as_str() {
                "up" => true,
                "down" => false,
                other => return Err(format!("use up or down, not {other}")),
            };
            harness
                .state
                .desktop_event(DesktopEvent::Network(available));
            harness.wait_until_settled().await;
            Ok(())
        }
        "desktop-resume" => {
            harness.state.desktop_event(DesktopEvent::Resumed);
            harness.wait_until_settled().await;
            Ok(())
        }
        "setup-key" => {
            let assistant = assistant()?;
            match args[0].as_str() {
                "enter" => assistant.press_enter(),
                "escape" => {
                    assistant.press_escape();
                    Ok(())
                }
                other => Err(format!("use enter or escape, not {other}")),
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

pub fn check(harness: &Harness, assertion: &str, args: &[String]) -> StepResult {
    match assertion {
        "assert-account-folders" => {
            let mailbox = harness.state.mailbox();
            let account = mailbox
                .accounts()
                .into_iter()
                .find(|account| account.address == args[0])
                .ok_or_else(|| format!("no account has the address {}", args[0]))?;
            let model = mailbox.sidebar_model();
            let shown = model
                .accounts
                .iter()
                .find(|shown| shown.name == account.name)
                .ok_or_else(|| format!("the sidebar doesn't show {}", args[0]))?;
            let titles: Vec<&str> = shown
                .role_folders()
                .into_iter()
                .chain(&shown.more)
                .map(|folder| folder.title())
                .collect();
            equal("folders", titles.join(",").as_str(), args[1].as_str())
        }
        "assert-account-status" => {
            let mailbox = harness.state.mailbox();
            let account = mailbox
                .accounts()
                .into_iter()
                .find(|account| account.address == args[0])
                .ok_or_else(|| format!("no account has the address {}", args[0]))?;
            let model = mailbox.sidebar_model();
            let status = model
                .accounts
                .iter()
                .find(|shown| shown.name == account.name)
                .map(|shown| shown.status.label())
                .ok_or_else(|| format!("the sidebar doesn't show {}", args[0]))?;
            let wanted = match args[1].as_str() {
                "synced" => "synced",
                "syncing" => "syncing…",
                "offline" => "offline",
                "needs-sign-in" => "needs sign-in",
                "certificate" => "bad certificate",
                "quota" => "mailbox full",
                other => {
                    return Err(format!(
                        "use synced, syncing, offline, needs-sign-in, certificate or quota, \
                         not {other}"
                    ));
                }
            };
            equal("account status", status, wanted)
        }
        "assert-setup" => {
            let wanted = args[1].as_str();
            match args[0].as_str() {
                "open" => {
                    let open = setup::current().is_some_and(|open| open.dialog().is_mapped());
                    equal("assistant open", open.to_string().as_str(), wanted)
                }
                "step" => equal("step", assistant()?.step().name(), wanted),
                "sources" => equal("sources", assistant()?.sources().as_str(), wanted),
                "focus" => equal(
                    "focus",
                    assistant()?.focused().as_deref().unwrap_or("nothing"),
                    wanted,
                ),
                "client-id" => equal(
                    "Microsoft client ID",
                    if harness.state.microsoft_client_id().is_some() {
                        "set"
                    } else {
                        "none"
                    },
                    wanted,
                ),
                "sign-in-needed" => {
                    let keys: Vec<String> = harness
                        .state
                        .sign_in_needed()
                        .into_iter()
                        .map(|again| again.key.to_string())
                        .collect();
                    let keys = if keys.is_empty() {
                        "none".to_owned()
                    } else {
                        keys.join(",")
                    };
                    equal("accounts to sign in again", keys.as_str(), wanted)
                }
                "banner-queue" => {
                    let queue: Vec<String> =
                        harness.state.waiting().iter().map(waiting_name).collect();
                    let queue = if queue.is_empty() {
                        "none".to_owned()
                    } else {
                        queue.join(",")
                    };
                    equal("the banner's queue", queue.as_str(), wanted)
                }
                other => Err(format!(
                    "assert-setup checks open, step, sources, focus, client-id, sign-in-needed \
                     or banner-queue, not {other}"
                )),
            }
        }
        "assert-toast" => {
            let toast =
                glib::Type::from_name("AdwToastWidget").ok_or("libadwaita has no toast widget")?;
            let mut shown = Vec::new();
            super::collect(harness.window()?.window().upcast_ref(), &mut |widget| {
                if let Some(label) = widget.downcast_ref::<gtk::Label>()
                    && label.is_mapped()
                    && label.ancestor(toast).is_some()
                {
                    shown.push(label.label().to_string());
                }
            });
            if shown.iter().any(|text| *text == args[0]) {
                Ok(())
            } else {
                Err(format!("the toast on screen says {shown:?}"))
            }
        }
        "assert-opened" => {
            let opened = OPENED.with(|opened| opened.borrow().clone());
            match args[0].as_str() {
                "last" => equal(
                    "last link opened",
                    opened.last().map_or("none", String::as_str),
                    args[1].as_str(),
                ),
                "count" => equal(
                    "links opened",
                    opened.len().to_string().as_str(),
                    args[1].as_str(),
                ),
                other => Err(format!("assert-opened checks last or count, not {other}")),
            }
        }
        "assert-clipboard" => {
            let display = gtk::gdk::Display::default().ok_or("no display")?;
            let text = display
                .clipboard()
                .content()
                .and_then(|content| content.value(glib::Type::STRING).ok())
                .and_then(|value| value.get::<String>().ok())
                .ok_or("Sealwax put no text on the clipboard")?;
            equal("clipboard", text.as_str(), args[0].as_str())
        }
        "assert-text-contains" => {
            let found = harness.widget(&args[0])?;
            let text = if let Some(label) = found.downcast_ref::<gtk::Label>() {
                label.label().to_string()
            } else if let Some(button) = found.downcast_ref::<gtk::Button>() {
                button
                    .label()
                    .map(|label| label.to_string())
                    .unwrap_or_default()
            } else if let Some(dialog) = found.downcast_ref::<adw::AlertDialog>() {
                format!(
                    "{}\n{}",
                    dialog.heading().unwrap_or_default(),
                    dialog.body()
                )
            } else {
                return Err(format!("{} has no text", args[0]));
            };
            if text.contains(args[1].as_str()) {
                Ok(())
            } else {
                Err(format!(
                    "{}'s text is {text:?}, without {:?}",
                    args[0], args[1]
                ))
            }
        }
        "assert-field" => {
            let found = harness.widget(&args[0])?;
            if found.is::<gtk::PasswordEntry>() {
                return Err("the password is never read back".to_owned());
            }
            let value = if let Some(editable) = found.dynamic_cast_ref::<gtk::Editable>() {
                editable.text().to_string()
            } else if let Some(dropdown) = found.downcast_ref::<gtk::DropDown>() {
                dropdown
                    .selected_item()
                    .and_downcast::<gtk::StringObject>()
                    .map(|item| item.string().to_string())
                    .unwrap_or_default()
            } else {
                return Err(format!("{} is not a field", args[0]));
            };
            equal(&args[0], value.as_str(), args[1].as_str())
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// A banner entry as `assert-setup banner-queue` names it: `keyring`, `certificate:KEY`,
/// `sign-in:KEY` (Microsoft), `password:KEY`, `quota:KEY`, `uncertain` (a send that may have
/// gone out, M6), `offline` (no network) or `unreachable` (no server answers).
fn waiting_name(waiting: &Waiting) -> String {
    let kind = match &waiting.attention {
        Attention::NoKeyring => "keyring",
        Attention::CertificateChanged { .. } => "certificate",
        Attention::SignIn { browser: true, .. } => "sign-in",
        Attention::SignIn { browser: false, .. } => "password",
        Attention::QuotaFull { .. } => "quota",
        Attention::UncertainSend { .. } => "uncertain",
        Attention::RulesFailed { .. } => "rules",
        Attention::Offline { network: true, .. } => "offline",
        Attention::Offline { .. } => "unreachable",
    };
    match &waiting.account {
        Some(key) => format!("{kind}:{key}"),
        None => kind.to_owned(),
    }
}

fn endpoint_text(endpoint: &Endpoint) -> String {
    let security = match endpoint.security {
        Security::Tls => "tls",
        Security::StartTls => "starttls",
    };
    let pinned = endpoint
        .certificate
        .map(|pin| format!(":pinned:{}", pin.hex("")[..8].to_lowercase()))
        .unwrap_or_default();
    format!("{}:{}:{security}{pinned}", endpoint.host, endpoint.port)
}

/// How the account receives, as `engine-expect` names it (M8): `imap`, `pop3:leave` (mail kept
/// on the server), `pop3:leave:N` (deleted there after N days) or `pop3:delete` (deleted once
/// downloaded).
fn receive_text(receive: Receive) -> String {
    match receive {
        Receive::Imap => "imap".to_owned(),
        Receive::Pop3(Pop3Policy {
            leave_on_server: false,
            ..
        }) => "pop3:delete".to_owned(),
        Receive::Pop3(Pop3Policy {
            delete_after_days: Some(days),
            ..
        }) => format!("pop3:leave:{days}"),
        Receive::Pop3(_) => "pop3:leave".to_owned(),
    }
}

fn auth_text(auth: AuthMethod) -> &'static str {
    match auth {
        AuthMethod::Password => "password",
        AuthMethod::OAuth(OAuthProvider::Microsoft) => "microsoft",
    }
}

/// A command's kind, its request and its fields as `KEY=VALUE` pairs; never the password. A
/// request that is `last_sign_in` reads `last`.
fn fields(
    command: &Command,
    last_sign_in: Option<RequestId>,
) -> (&'static str, Option<RequestId>, Vec<(&'static str, String)>) {
    let settings_fields = |settings: &AccountSettings| {
        vec![
            ("address", settings.address.clone()),
            ("name", settings.name.clone()),
            (
                "label",
                settings.label.clone().unwrap_or_else(|| "none".to_owned()),
            ),
            (
                "color",
                settings
                    .color
                    .map_or("none", |color| color.name())
                    .to_owned(),
            ),
            ("key", settings.key.to_string()),
            ("imap", endpoint_text(&settings.incoming)),
            ("incoming", endpoint_text(&settings.incoming)),
            ("receive", receive_text(settings.receive)),
            ("smtp", endpoint_text(&settings.smtp)),
            ("username", settings.username.clone()),
            ("auth", auth_text(settings.auth).to_owned()),
        ]
    };
    let sign_in = |request: Option<RequestId>| match request {
        None => "none".to_owned(),
        Some(request) if Some(request) == last_sign_in => "last".to_owned(),
        Some(request) => request.to_string(),
    };
    match command {
        Command::Detect { request, address } => {
            ("detect", Some(*request), vec![("address", address.clone())])
        }
        Command::Verify {
            request,
            settings,
            second_username,
            ..
        } => {
            let mut fields = settings_fields(settings);
            fields.push((
                "second",
                second_username.clone().unwrap_or_else(|| "none".to_owned()),
            ));
            ("verify", Some(*request), fields)
        }
        Command::AddAccount {
            request,
            settings,
            sign_in: signed_in,
            ..
        } => {
            let mut fields = settings_fields(settings);
            fields.push(("sign-in", sign_in(*signed_in)));
            ("add-account", Some(*request), fields)
        }
        Command::ReadCertificate {
            request,
            endpoint,
            protocol,
        } => (
            "read-certificate",
            Some(*request),
            vec![
                ("server", endpoint_text(endpoint)),
                ("protocol", protocol_text(*protocol).to_owned()),
            ],
        ),
        Command::RemoveAccount { request, key } => (
            "remove-account",
            Some(*request),
            vec![("key", key.to_string())],
        ),
        Command::EditAccount {
            request,
            key,
            label,
            color,
        } => (
            "edit-account",
            Some(*request),
            vec![
                ("key", key.to_string()),
                ("label", label.clone().unwrap_or_else(|| "none".to_owned())),
                (
                    "color",
                    color.map_or("none", |color| color.name()).to_owned(),
                ),
            ],
        ),
        Command::SignIn {
            request,
            provider,
            address,
            account,
        } => (
            "sign-in",
            Some(*request),
            vec![
                ("address", address.clone()),
                (
                    "provider",
                    auth_text(AuthMethod::OAuth(*provider)).to_owned(),
                ),
                (
                    "account",
                    account
                        .as_ref()
                        .map_or_else(|| "none".to_owned(), ToString::to_string),
                ),
            ],
        ),
        Command::CancelSignIn { request } => (
            "cancel-sign-in",
            Some(*request),
            vec![("sign-in", sign_in(Some(*request)))],
        ),
        Command::MicrosoftClient { client_id } => (
            "microsoft-client",
            None,
            vec![("set", client_id.is_some().to_string())],
        ),
        Command::Accounts { accounts } => (
            "accounts",
            None,
            vec![("count", accounts.len().to_string())],
        ),
        Command::FolderOpened { folder } => (
            "folder-opened",
            None,
            vec![("folder", folder.0.to_string())],
        ),
        Command::LoadOlder { folder } => {
            ("load-older", None, vec![("folder", folder.0.to_string())])
        }
        Command::ApplyRules {
            request,
            folder,
            rule,
            ..
        } => (
            "apply-rules",
            Some(*request),
            vec![
                ("folder", folder.0.to_string()),
                ("rule", rule.as_deref().map_or("none".to_owned(), rule_name)),
            ],
        ),
        Command::FetchBodies { messages } => (
            "fetch-bodies",
            None,
            vec![
                (
                    "messages",
                    messages
                        .iter()
                        .map(|message| message.0.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                ("count", messages.len().to_string()),
            ],
        ),
        Command::FetchPart { message, part } => (
            "fetch-part",
            None,
            vec![
                ("message", message.0.to_string()),
                ("part", part.to_string()),
            ],
        ),
        Command::FetchOriginal { message } => (
            "fetch-original",
            None,
            vec![("message", message.0.to_string())],
        ),
        Command::Storage { storage } => (
            "storage",
            None,
            vec![
                (
                    "cache-size",
                    storage
                        .cache_size
                        .map_or_else(|| "unlimited".to_owned(), |bytes| bytes.to_string()),
                ),
                ("prefetch-days", storage.prefetch_days.to_string()),
            ],
        ),
        Command::Reconnect => ("reconnect", None, Vec::new()),
        Command::CheckMail => ("check-mail", None, Vec::new()),
        Command::OpsQueued { account } => {
            ("ops-queued", None, vec![("account", account.0.to_string())])
        }
        Command::OutboxQueued { account } => (
            "outbox-queued",
            None,
            vec![("account", account.0.to_string())],
        ),
        Command::DraftsChanged { account } => (
            "drafts-changed",
            None,
            vec![("account", account.0.to_string())],
        ),
        Command::SubmissionLimit { request, account } => (
            "submission-limit",
            Some(*request),
            vec![("account", account.0.to_string())],
        ),
        Command::UpdatePassword { request, key, .. } => (
            "update-password",
            Some(*request),
            vec![("key", key.to_string())],
        ),
        Command::ChangeFolder {
            request,
            account,
            change,
        } => {
            let mut fields = vec![("account", account.0.to_string())];
            let kind = match change {
                FolderChange::Create { parent, name } => {
                    fields.push((
                        "parent",
                        parent.map_or_else(|| "none".to_owned(), |id| id.0.to_string()),
                    ));
                    fields.push(("name", name.clone()));
                    "create-folder"
                }
                FolderChange::Rename { folder, name } => {
                    fields.push(("folder", folder.0.to_string()));
                    fields.push(("name", name.clone()));
                    "rename-folder"
                }
                FolderChange::Delete { folder } => {
                    fields.push(("folder", folder.0.to_string()));
                    "delete-folder"
                }
            };
            (kind, Some(*request), fields)
        }
        Command::CheckForUpdates { request } => ("check-for-updates", Some(*request), Vec::new()),
        Command::Shutdown => ("shutdown", None, Vec::new()),
    }
}

/// The name of the one rule an `ApplyRules` runs (a rule just saved), `?` when it holds
/// something else.
fn rule_name(script: &str) -> String {
    let set = sealwax_domain::sieve::RuleSet::read(script, &Default::default());
    match set.as_ref().map(|set| set.rules().collect::<Vec<_>>()) {
        Ok(rules) if rules.len() == 1 => crate::rules::text::rule_title(rules[0]).to_owned(),
        _ => "?".to_owned(),
    }
}

fn password(command: &Command) -> Option<&Password> {
    match command {
        Command::Verify { password, .. }
        | Command::AddAccount { password, .. }
        | Command::UpdatePassword { password, .. } => Some(password),
        _ => None,
    }
}

fn settings(command: &Command) -> Option<&AccountSettings> {
    match command {
        Command::Verify { settings, .. } | Command::AddAccount { settings, .. } => Some(settings),
        _ => None,
    }
}

/// `engine-expect KIND [KEY=VALUE…]`.
async fn expect(harness: &Harness, args: &[String]) -> StepResult {
    let fake = fake()?;
    if args[0] == "none" {
        harness.wait_until_settled().await;
        // `none but KIND,…`: commands of those kinds the list sends by itself are let through.
        let allowed: Vec<&str> = match args.get(1).map(String::as_str) {
            Some("but") => args.get(2).map_or("", String::as_str).split(',').collect(),
            Some(other) => return Err(format!("use engine-expect none [but KIND,…], not {other}")),
            None => Vec::new(),
        };
        while let Ok(command) = fake.commands.try_recv() {
            let kind = fields(&command, None).0;
            if !allowed.contains(&kind) {
                return Err(format!("the app sent {kind}"));
            }
        }
        return Ok(());
    }
    let command = with_timeout(fake.commands.recv(), WAIT_TIMEOUT)
        .await
        .ok_or_else(|| format!("no {} came from the app", args[0]))?
        .map_err(|_| "the app closed the engine's channel".to_owned())?;
    if let Command::SignIn { request, .. } = &command {
        fake.last_sign_in.set(Some(*request));
    }
    if let Command::FetchBodies { messages } = &command {
        REQUESTED.with(|requested| requested.replace(messages.clone()));
    }
    if let Command::LoadOlder { folder } = &command {
        OLDER.with(|older| older.set(Some(*folder)));
    }
    if let Command::FetchPart { message, part } = &command {
        PART.with(|asked| asked.set(Some((*message, *part))));
    }
    if let Command::FetchOriginal { message } = &command {
        ORIGINAL.with(|asked| asked.set(Some(*message)));
    }
    let (kind, request, fields) = fields(&command, fake.last_sign_in.get());
    let shown: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    println!("  # {kind} {}", shown.join(" "));
    fake.last.replace(Some(Last {
        request: request.unwrap_or_default(),
        settings: settings(&command).cloned(),
        key: match &command {
            Command::RemoveAccount { key, .. } | Command::EditAccount { key, .. } => {
                Some(key.clone())
            }
            _ => None,
        },
        look: match &command {
            Command::EditAccount { label, color, .. } => Some((label.clone(), *color)),
            _ => None,
        },
        folder: match &command {
            Command::ChangeFolder {
                account, change, ..
            } => Some((*account, change.clone())),
            _ => None,
        },
    }));
    equal("command", kind, args[0].as_str())?;
    for pair in &args[1..] {
        let (key, wanted) = pair
            .split_once('=')
            .ok_or_else(|| format!("expected KEY=VALUE, got {pair}"))?;
        if key == "password" {
            let typed = password(&command).ok_or("this command carries no password")?;
            if typed.expose() != wanted {
                return Err("the password differs from the one typed".to_owned());
            }
            continue;
        }
        let value = fields
            .iter()
            .find(|(field, _)| *field == key)
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| format!("{kind} has no field {key}"))?;
        equal(key, value, wanted)?;
    }
    Ok(())
}

/// `engine-reply KIND ARGS…`, to the last command taken; `stale` uses a request the app never
/// sent.
async fn reply(harness: &Harness, args: &[String], stale: bool) -> StepResult {
    let fake = fake()?;
    if let Some(event) = sync_event(harness, args)? {
        return send_event(harness, &fake, event).await;
    }
    if let Some(event) = body_event(args)? {
        return send_event(harness, &fake, event).await;
    }
    if let Some(event) = older_event(args)? {
        return send_event(harness, &fake, event).await;
    }
    if let Some(event) = download_event(args)? {
        return send_event(harness, &fake, event).await;
    }
    let last = fake
        .last
        .borrow()
        .clone()
        .ok_or("no command to reply to; use engine-expect first")?;
    let request = if stale {
        last.request + 1000
    } else {
        last.request
    };
    let event = match (args[0].as_str(), &args[1..]) {
        ("progress", [source]) => Event::DetectProgress {
            request,
            source: sources::from_name(source).ok_or_else(|| format!("no source {source}"))?,
        },
        ("detected", [name]) => Event::Detected {
            request,
            detection: detection(name)?,
        },
        ("verified", [ok]) if ok == "ok" => Event::Verified {
            request,
            result: Ok(last
                .settings
                .as_ref()
                .map(|settings| settings.username.clone())
                .ok_or("the last command had no settings")?),
        },
        ("verified", [ok, username]) if ok == "ok" => Event::Verified {
            request,
            result: Ok(username.clone()),
        },
        ("verified", [protocol, failure]) => Event::Verified {
            request,
            result: Err(LoginError {
                protocol: protocol_named(protocol)?,
                failure: login_failure(failure)?,
            }),
        },
        ("certificate-read", [ok, byte]) if ok == "ok" => Event::CertificateRead {
            request,
            result: Ok(CertificatePin(
                [u8::from_str_radix(byte, 16).map_err(|_| format!("{byte} is no hex byte"))?; 32],
            )),
        },
        ("certificate-read", [failure]) => Event::CertificateRead {
            request,
            result: Err(login_failure(failure)?),
        },
        ("added", [outcome]) => Event::AccountAdded {
            request,
            result: match outcome.as_str() {
                "ok" => Ok(add_to_store(harness, &last).await?),
                other => Err(account_error(other)?),
            },
        },
        ("added", [ok, file]) if ok == "ok" && file == "file" => {
            write_accounts_file(harness, &last).await?;
            Event::AccountAdded {
                request,
                result: Ok(add_to_store(harness, &last).await?),
            }
        }
        ("edited", [ok, file]) if ok == "ok" && file == "file" => {
            relabel_accounts_file(harness, &last).await?;
            Event::AccountEdited {
                request,
                result: Ok(()),
            }
        }
        ("edited", [outcome]) => Event::AccountEdited {
            request,
            result: match outcome.as_str() {
                "ok" => Ok(()),
                other => Err(account_error(other)?),
            },
        },
        ("removed", [outcome]) => Event::AccountRemoved {
            request,
            result: match outcome.as_str() {
                "ok" => Ok(remove_from_store(harness, &last).await?),
                other => Err(account_error(other)?),
            },
        },
        ("sign-in-open", [url]) => Event::SignInOpen {
            request,
            url: url.clone(),
        },
        ("signed-in", [ok, username]) if ok == "ok" => Event::SignedIn {
            request,
            result: Ok(username.clone()),
        },
        ("signed-in", [failure]) => Event::SignedIn {
            request,
            result: Err(LoginError {
                protocol: Protocol::Imap,
                failure: login_failure(failure)?,
            }),
        },
        ("signed-in", [protocol, failure]) => Event::SignedIn {
            request,
            result: Err(LoginError {
                protocol: protocol_named(protocol)?,
                failure: login_failure(failure)?,
            }),
        },
        ("folder-changed", outcome) => {
            let (account, change) = last
                .folder
                .clone()
                .ok_or("the last command was no ChangeFolder")?;
            let result = match outcome {
                [ok] if ok == "ok" => {
                    super::folders::apply(harness, account, &change).await?;
                    Ok(())
                }
                [refused, text] if refused == "refused" => {
                    Err(FolderError::Refused(ServerText::new(text.clone())))
                }
                [other] => Err(match other.as_str() {
                    "offline" => FolderError::Offline,
                    "taken" => FolderError::Name(NameProblem::Taken),
                    "not-allowed" => FolderError::NotAllowed,
                    "missing" => FolderError::Missing,
                    "lost" => FolderError::Lost,
                    "store" => FolderError::Store,
                    other => return Err(format!("no folder-changed outcome {other}")),
                }),
                _ => return Err("folder-changed takes an outcome".to_owned()),
            };
            Event::FolderChanged {
                request,
                account,
                result,
            }
        }
        ("password-updated", [ok]) if ok == "ok" => Event::PasswordUpdated {
            request,
            result: Ok(()),
        },
        ("password-updated", [failure]) => Event::PasswordUpdated {
            request,
            result: Err(LoginError {
                protocol: Protocol::Imap,
                failure: login_failure(failure)?,
            }),
        },
        ("password-updated", [protocol, failure]) => Event::PasswordUpdated {
            request,
            result: Err(LoginError {
                protocol: protocol_named(protocol)?,
                failure: login_failure(failure)?,
            }),
        },
        ("update-checked", [answer]) => Event::UpdateChecked {
            request,
            result: update_result(answer)?,
        },
        _ => return Err(format!("no reply {}", args.join(" "))),
    };
    send_event(harness, &fake, event).await
}

/// `update-checked`'s answer: a version, `running` (this build's), or a failure.
fn update_result(answer: &str) -> Result<Result<Version, UpdateError>, String> {
    Ok(match answer {
        "running" => Ok(crate::updates::running()),
        "offline" => Err(UpdateError::Offline),
        "timeout" => Err(UpdateError::Timeout),
        "certificate" => Err(UpdateError::Certificate),
        "busy" => Err(UpdateError::Busy),
        "no-release" => Err(UpdateError::NoRelease),
        "unreadable" => Err(UpdateError::Unreadable),
        version => Ok(Version::parse(version).ok_or_else(|| {
            format!(
                "use a version, running, offline, timeout, certificate, busy, no-release or \
                 unreadable, not {version}"
            )
        })?),
    })
}

/// Hands `event` to the app as the fake engine (`engine-attach` first), for other modules'
/// commands (M6: the sends' outcomes).
pub(super) async fn emit(harness: &Harness, event: Event) -> StepResult {
    let fake = fake()?;
    send_event(harness, &fake, event).await
}

/// The app's next command to the fake engine, as `engine-expect` names it, or `None` when it
/// sent nothing more.
pub(super) fn next_command() -> Result<Option<String>, String> {
    let fake = fake()?;
    Ok(fake.commands.try_recv().ok().map(|command| {
        let (kind, _, fields) = fields(&command, None);
        let shown: Vec<String> = fields
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        format!("{kind} {}", shown.join(" ")).trim().to_owned()
    }))
}

async fn send_event(harness: &Harness, fake: &Fake, event: Event) -> StepResult {
    fake.events
        .send(event)
        .await
        .map_err(|_| "the app's end of the engine is gone".to_owned())?;
    // The link hands events over on the main loop; let it run once more.
    glib::timeout_future(Duration::from_millis(10)).await;
    harness.wait_until_settled().await;
    Ok(())
}

/// Events no command asked for: `sync-failed ADDRESS FAILURE`, `sync-finished ADDRESS` and
/// `status ADDRESS STATE`, what the engine reports about the sync of the store's account with
/// that address; and `keyring-unavailable`.
fn sync_event(harness: &Harness, args: &[String]) -> Result<Option<Event>, String> {
    let account = |address: &str| {
        harness
            .state
            .mailbox()
            .accounts()
            .into_iter()
            .find(|account| account.address == address)
            .map(|account| account.id)
            .ok_or_else(|| format!("no account has the address {address}"))
    };
    Ok(match (args[0].as_str(), &args[1..]) {
        ("sync-failed", [address, failure]) => Some(Event::AccountStatus {
            account: account(address)?,
            status: SyncStatus::from_failure(login_failure(failure)?),
        }),
        ("sync-finished", [address]) => Some(Event::AccountStatus {
            account: account(address)?,
            status: SyncStatus::Synced,
        }),
        ("status", [address, state]) => Some(Event::AccountStatus {
            account: account(address)?,
            status: sync_status(state)?,
        }),
        ("keyring-unavailable", []) => Some(Event::KeyringUnavailable),
        ("ops-refused", [address, change, messages]) => Some(Event::OpsRefused {
            account: account(address)?,
            change: match change.as_str() {
                "flags" => Refused::Flags,
                "move" => Refused::Move,
                "expunge" => Refused::Expunge,
                "folder-read" => Refused::FolderRead,
                other => {
                    return Err(format!(
                        "use flags, move, expunge or folder-read, not {other}"
                    ));
                }
            },
            messages: messages
                .parse()
                .map_err(|_| format!("expected a count, got {messages}"))?,
        }),
        _ => None,
    })
}

/// A `SyncStatus` by name: `syncing`, `synced`, `offline`, `needs-sign-in`, `certificate`,
/// `quota`.
fn sync_status(name: &str) -> Result<SyncStatus, String> {
    Ok(match name {
        "syncing" => SyncStatus::Syncing,
        "synced" => SyncStatus::Synced,
        "offline" => SyncStatus::Offline(LoginFailure::Unreachable),
        "needs-sign-in" => SyncStatus::NeedsSignIn(LoginFailure::WrongPassword),
        "certificate" => SyncStatus::CertificateChanged,
        "quota" => SyncStatus::QuotaFull,
        other => {
            return Err(format!(
                "use syncing, synced, offline, needs-sign-in, certificate or quota, not {other}"
            ));
        }
    })
}

/// `body-fetched ok|offline|needs-sign-in|gone|store|other`: `BodyFetched` for the first
/// message of the last `FetchBodies` taken.
fn body_event(args: &[String]) -> Result<Option<Event>, String> {
    let ("body-fetched", [outcome]) = (args[0].as_str(), &args[1..]) else {
        return Ok(None);
    };
    let message = last_requested()
        .first()
        .copied()
        .ok_or("no fetch-bodies to answer; use engine-expect first")?;
    let result = match outcome.as_str() {
        "ok" => Ok(()),
        "offline" => Err(BodyError::Offline),
        "needs-sign-in" => Err(BodyError::NeedsSignIn),
        "gone" => Err(BodyError::Gone),
        "store" => Err(BodyError::Store),
        "other" => Err(BodyError::Other),
        other => return Err(format!("no body outcome {other}")),
    };
    Ok(Some(Event::BodyFetched { message, result }))
}

/// `part-fetched OUTCOME` and `original-fetched OUTCOME`: the answer to the last `FetchPart` or
/// `FetchOriginal` taken; `ok` carries the bytes of the message `files.rs` stored.
fn download_event(args: &[String]) -> Result<Option<Event>, String> {
    let (kind @ ("part-fetched" | "original-fetched"), [outcome]) = (args[0].as_str(), &args[1..])
    else {
        return Ok(None);
    };
    let failure = match outcome.as_str() {
        "ok" => None,
        "offline" => Some(BodyError::Offline),
        "needs-sign-in" => Some(BodyError::NeedsSignIn),
        "gone" => Some(BodyError::Gone),
        "store" => Some(BodyError::Store),
        "other" => Some(BodyError::Other),
        other => return Err(format!("no download outcome {other}")),
    };
    Ok(Some(match kind {
        "part-fetched" => {
            let (message, part) = PART
                .with(Cell::get)
                .ok_or("no fetch-part to answer; use engine-expect first")?;
            let result = match failure {
                Some(error) => Err(error),
                None => Ok(FetchedPart {
                    mime_type: "application/octet-stream".to_owned(),
                    data: super::files::part_bytes(message, part)
                        .ok_or("files.rs stored no such part")?,
                }),
            };
            Event::PartFetched {
                message,
                part,
                result,
            }
        }
        "original-fetched" => {
            let message = ORIGINAL
                .with(Cell::get)
                .ok_or("no fetch-original to answer; use engine-expect first")?;
            let result = match failure {
                Some(error) => Err(error),
                None => Ok(Original(
                    super::files::original_bytes(message)
                        .ok_or("files.rs stored no such message")?,
                )),
            };
            Event::OriginalFetched { message, result }
        }
        _ => return Ok(None),
    }))
}

/// `older-loaded FETCHED complete|more`: `OlderLoaded` for the folder of the last `LoadOlder`
/// taken.
fn older_event(args: &[String]) -> Result<Option<Event>, String> {
    let ("older-loaded", [fetched, complete]) = (args[0].as_str(), &args[1..]) else {
        return Ok(None);
    };
    let folder = OLDER
        .with(Cell::get)
        .ok_or("no load-older to answer; use engine-expect first")?;
    let fetched = fetched
        .parse()
        .map_err(|_| format!("FETCHED is a count, not {fetched}"))?;
    let complete = match complete.as_str() {
        "complete" => true,
        "more" => false,
        other => return Err(format!("use complete or more, not {other}")),
    };
    Ok(Some(Event::OlderLoaded {
        folder,
        fetched,
        complete,
    }))
}

fn protocol_text(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Imap => "imap",
        Protocol::Smtp => "smtp",
        Protocol::Pop3 => "pop3",
    }
}

fn protocol_named(name: &str) -> Result<Protocol, String> {
    match name {
        "imap" => Ok(Protocol::Imap),
        "smtp" => Ok(Protocol::Smtp),
        "pop3" => Ok(Protocol::Pop3),
        other => Err(format!("use imap, pop3 or smtp, not {other}")),
    }
}

/// The key the fake engine gives an added account: its address's local part.
fn fake_key(settings: &AccountSettings) -> AccountKey {
    AccountKey(
        settings
            .address
            .split('@')
            .next()
            .unwrap_or("account")
            .to_owned(),
    )
}

/// What the engine does on `RemoveAccount`, for the store: the account and its rows go.
async fn remove_from_store(harness: &Harness, last: &Last) -> Result<(), String> {
    let key = last
        .key
        .clone()
        .ok_or("the last command named no account")?;
    let mailbox = harness.state.mailbox();
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .account_by_key(key);
    let account = reply
        .recv()
        .await
        .map_err(|error| error.to_string())?
        .value
        .ok_or("the store has no account with that key")?;
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .remove_account(account.id);
    reply.recv().await.map_err(|error| error.to_string())?;
    let started = Instant::now();
    while mailbox
        .accounts()
        .iter()
        .any(|known| known.id == account.id)
    {
        if started.elapsed() > WAIT_TIMEOUT {
            return Err("the sidebar still lists the removed account".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// What the engine does on `AddAccount`, for `accounts.toml` (`added ok file`): the entry
/// appended through the same writer, with the fake engine's key; then the app's read of it.
async fn write_accounts_file(harness: &Harness, last: &Last) -> StepResult {
    let mut settings = last
        .settings
        .clone()
        .ok_or("the last command had no settings")?;
    settings.key = fake_key(&settings);
    let prefs = harness
        .state
        .preferences()
        .ok_or("the settings aren't read yet")?;
    sealwax_infrastructure::accounts::add(&prefs.path(SettingsFile::Accounts), &settings)
        .map_err(|error| format!("accounts.toml: {error}"))?;
    let started = Instant::now();
    while !prefs.accounts().contains(&settings) {
        if started.elapsed() > WAIT_TIMEOUT {
            return Err("the app never read the new accounts.toml entry".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// What the engine does on `EditAccount` (`edited ok file`): the account's label and colour
/// written through the same writer, the app's read of them, and the store's name as the
/// engine's `reconcile` sets it once the app sends the file.
async fn relabel_accounts_file(harness: &Harness, last: &Last) -> StepResult {
    let key = last
        .key
        .clone()
        .ok_or("the last command named no account")?;
    let (label, color) = last
        .look
        .clone()
        .ok_or("the last command was no EditAccount")?;
    let prefs = harness
        .state
        .preferences()
        .ok_or("the settings aren't read yet")?;
    let path = prefs.path(SettingsFile::Accounts);
    if !sealwax_infrastructure::accounts::relabel(&path, &key, label.as_deref(), color)
        .map_err(|error| format!("accounts.toml: {error}"))?
    {
        return Err(format!("accounts.toml has no account {key}"));
    }
    let started = Instant::now();
    let settings = loop {
        let read = prefs
            .accounts()
            .into_iter()
            .find(|settings| settings.key == key && settings.label == label);
        if let Some(settings) = read.filter(|settings| settings.color == color) {
            break settings;
        }
        if started.elapsed() > WAIT_TIMEOUT {
            return Err("the app never read the edited accounts.toml entry".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    };
    let reply = harness
        .state
        .mailbox()
        .store()
        .ok_or("the store is not open")?
        .ensure_account(
            key,
            NewAccount {
                name: settings.title().to_owned(),
                address: settings.address.clone(),
            },
        );
    reply.recv().await.map_err(|error| error.to_string())?;
    Ok(())
}

/// What the engine does on `AddAccount`, for the store: the account and its role folders, so
/// the sidebar shows it as it would after a real setup; a POP3 account's are local (M8).
async fn add_to_store(
    harness: &Harness,
    last: &Last,
) -> Result<sealwax_domain::model::AccountId, String> {
    let settings = last
        .settings
        .as_ref()
        .ok_or("the last command had no settings")?;
    let mailbox = harness.state.mailbox();
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .ensure_account(
            fake_key(settings),
            NewAccount {
                name: settings.title().to_owned(),
                address: settings.address.clone(),
            },
        );
    let account = reply.recv().await.map_err(|error| error.to_string())?.value;
    let inbox = match settings.receive {
        Receive::Imap => "INBOX",
        Receive::Pop3(_) => "Inbox",
    };
    for (role, folder) in [
        (FolderRole::Inbox, inbox),
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
            return Err("the store never listed the new account's folders".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    Ok(account)
}

fn login_failure(name: &str) -> Result<LoginFailure, String> {
    Ok(match name {
        "wrong-password" => LoginFailure::WrongPassword,
        "app-password" => LoginFailure::AppPasswordRequired,
        "imap-disabled" => LoginFailure::ImapDisabled,
        "oauth" => LoginFailure::OAuthRequired,
        "sign-in-declined" => LoginFailure::SignInDeclined,
        "no-tls" => LoginFailure::NoTls,
        "certificate" => LoginFailure::Certificate,
        "unreachable" => LoginFailure::Unreachable,
        "timeout" => LoginFailure::Timeout,
        "other" => LoginFailure::Other,
        other => return Err(format!("no login failure {other}")),
    })
}

fn account_error(name: &str) -> Result<AccountError, String> {
    Ok(match name {
        "keyring" => AccountError::Keyring,
        "settings-file" => AccountError::SettingsFile,
        "store" => AccountError::Store,
        "exists" => AccountError::Exists,
        "missing" => AccountError::Missing,
        other => return Err(format!("no account error {other}")),
    })
}

fn link(label: &str, url: &str) -> Option<HelpLink> {
    Some(HelpLink {
        label: label.to_owned(),
        url: url.to_owned(),
    })
}

fn tls(host: &str, port: u16) -> Endpoint {
    Endpoint {
        host: host.to_owned(),
        port,
        security: Security::Tls,
        certificate: None,
    }
}

/// Scripted detection results. Provider names are real ones' or the mock's fictional ones;
/// every host is under `example.test`, except `gmail`'s servers: Gmail's, as Mozilla's ISPDB
/// lists them, without its out-of-date "enable IMAP" link, as the engine leaves it out (for
/// docs/GMAIL.md's screenshots; the fake engine connects nowhere).
fn detection(name: &str) -> Result<Detection, String> {
    let found = |provider: &str, source| DetectedSettings {
        imap: tls("imap.example.test", 993),
        smtp: tls("smtp.example.test", 465),
        username: UsernameForm::Address,
        second_username: Some(UsernameForm::LocalPart),
        auth: AuthMethod::Password,
        provider: Some(provider.to_owned()),
        confirm: false,
        app_password: None,
        enable_imap: None,
        source,
    };
    Ok(match name {
        "fjordpost" => Detection::Found(Box::new(DetectedSettings {
            imap: tls("mail.example.test", 993),
            smtp: tls("mail.example.test", 465),
            ..found("Fjordpost", DetectSource::Autoconfig)
        })),
        "fastmail" => Detection::Found(Box::new(DetectedSettings {
            second_username: None,
            app_password: link(
                "Create an app password",
                "https://example.test/settings/app-passwords",
            ),
            ..found("Fastmail", DetectSource::Preset)
        })),
        "icloud" => Detection::Found(Box::new(DetectedSettings {
            second_username: None,
            app_password: link(
                "Open the Apple account page",
                "https://example.test/apppasswords",
            ),
            ..found("iCloud", DetectSource::Preset)
        })),
        "imap-off" => Detection::Found(Box::new(DetectedSettings {
            enable_imap: link(
                "Open the IMAP settings",
                "https://example.test/settings/imap",
            ),
            ..found("Fjordpost", DetectSource::Ispdb)
        })),
        "gmail" => Detection::Found(Box::new(DetectedSettings {
            imap: tls("imap.gmail.com", 993),
            smtp: tls("smtp.gmail.com", 465),
            ..found("GMail", DetectSource::Ispdb)
        })),
        "confirm" => Detection::Found(Box::new(DetectedSettings {
            imap: tls("imap.fjordhost.example.test", 993),
            smtp: Endpoint {
                host: "smtp.fjordhost.example.test".to_owned(),
                port: 587,
                security: Security::StartTls,
                certificate: None,
            },
            username: UsernameForm::LocalPart,
            second_username: Some(UsernameForm::Address),
            confirm: true,
            ..found("Fjordhost", DetectSource::Mx)
        })),
        "other-username" => Detection::Found(Box::new(DetectedSettings {
            username: UsernameForm::Other,
            second_username: None,
            ..found("Fjordpost", DetectSource::Srv)
        })),
        "microsoft" => Detection::Found(Box::new(DetectedSettings {
            auth: AuthMethod::OAuth(OAuthProvider::Microsoft),
            second_username: None,
            ..found("Outlook.com", DetectSource::Preset)
        })),
        "bridge" => Detection::Found(Box::new(DetectedSettings {
            imap: Endpoint {
                host: "127.0.0.1".to_owned(),
                port: 1143,
                security: Security::StartTls,
                certificate: None,
            },
            smtp: Endpoint {
                host: "127.0.0.1".to_owned(),
                port: 1025,
                security: Security::StartTls,
                certificate: None,
            },
            second_username: None,
            app_password: link("Get Proton Mail Bridge", "https://example.test/bridge"),
            ..found("Proton Mail", DetectSource::Preset)
        })),
        "no-imap" => Detection::Unsupported {
            provider: "Tuta".to_owned(),
            reason: UnsupportedReason::NoImap,
        },
        "no-mail" => Detection::NoMail,
        "not-found" => Detection::NotFound,
        other => return Err(format!("no detection fixture {other}")),
    })
}
