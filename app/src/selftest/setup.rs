//! The setup assistant's self-test commands (ML-033): a fake engine on the app's
//! [`EngineLink`](crate::engine_link::EngineLink) that the script answers with scripted events,
//! and the assistant driven as a user would. Every address, name and server is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `engine-attach`, `engine-detach` | Connects the fake engine, or leaves the app without one; links the assistant opens are recorded from then on, never opened |
//! | `engine-start CA` | Starts the real engine instead, trusting only the CA file (the mail testbed's) and keeping secrets in memory |
//! | `engine-expect detect\|verify\|add-account\|sign-in\|cancel-sign-in\|update-password\|reconnect [KEY=VALUE…]` | Waits for the app's next command and checks its fields: `address`, `name`, `key`, `imap` and `smtp` (`host:port:tls\|starttls`), `username`, `auth` (`password`, `microsoft`), `second` (`none`), `account` (a key, `none`), `provider`, `sign-in` (`last`: the last `SignIn`'s request; `none`), `password` (compared, never printed) |
//! | `engine-expect none` | The app sent nothing more |
//! | `engine-reply progress SOURCE` | `DetectProgress` for the last command: `preset`, `autoconfig`, `ispdb`, `mx`, `srv`, `probe` |
//! | `engine-reply detected FIXTURE` | `Detected`: `fjordpost`, `fastmail`, `gmail`, `imap-off`, `confirm`, `other-username`, `microsoft`, `bridge`, `no-imap`, `no-mail`, `not-found` |
//! | `engine-reply verified ok [USERNAME]`, `engine-reply verified imap\|smtp FAILURE` | `Verified`: `wrong-password`, `app-password`, `imap-disabled`, `oauth`, `sign-in-declined`, `no-tls`, `certificate`, `unreachable`, `timeout`, `other` |
//! | `engine-reply sign-in-open URL` | `SignInOpen` for the last command |
//! | `engine-reply signed-in ok USERNAME`, `engine-reply signed-in [imap\|smtp] FAILURE` | `SignedIn` (IMAP when no protocol is named) |
//! | `engine-reply added ok\|keyring\|settings-file\|store\|exists\|missing` | `AccountAdded`; `ok` first adds the account (its key is the address's local part) and its role folders to the app's store, as the engine would |
//! | `engine-reply removed ok\|keyring\|settings-file\|store\|missing` | `AccountRemoved`; `ok` first removes the account from the app's store |
//! | `engine-reply sync-failed ADDRESS FAILURE`, `engine-reply sync-finished ADDRESS` | `AccountStatus` for the store's account with ADDRESS: the failure's state, or `Synced` |
//! | `engine-reply status ADDRESS STATE` | `AccountStatus` for that account: `syncing`, `synced`, `offline`, `needs-sign-in`, `certificate`, `quota` (ML-048) |
//! | `engine-reply keyring-unavailable` | `KeyringUnavailable` |
//! | `engine-reply password-updated ok`, `engine-reply password-updated [imap\|smtp] FAILURE` | `PasswordUpdated` for the last command (IMAP when no protocol is named) |
//! | `desktop-network up\|down`, `desktop-resume` | What the network monitor and logind would report, through the app's own handler |
//! | `engine-reply body-fetched ok\|offline\|needs-sign-in\|gone\|store\|other` | `BodyFetched` for the first message of the last `fetch-bodies` taken (its fields: `messages`, `count`); storing the body is `bodies-store`'s (`bodies.rs`) |
//! | `engine-reply older-loaded FETCHED complete\|more` | `OlderLoaded` for the folder of the last `load-older` taken (its field: `folder`; `folder-opened` has the same) (ML-043) |
//! | `engine-reply-stale …` | The same events with a request id the app never used |
//! | `setup-type NAME TEXT`, `setup-choose NAME tls\|starttls`, `setup-click NAME` | Types into a field, picks a security, clicks a visible, sensitive button |
//! | `setup-key enter\|escape` | Enter in the focused field, Escape in the dialog |
//! | `setup-respond NAME RESPONSE` | Answers the alert dialog named NAME (Remove Account's `remove-account`) as its button would |
//! | `setup-dismiss-toasts` | Closes the toasts shown and queued, as their × would (for previews) |
//! | `assert-setup open\|step\|sources\|focus VALUE` | The assistant's state |
//! | `assert-setup client-id set\|none` | Whether a Microsoft client ID is in use (`config.toml`; self-tests have no built-in one) |
//! | `assert-setup sign-in-needed KEY,…\|none` | The accounts the window's banner asks to sign in again, in order |
//! | `assert-setup banner-queue ENTRY,…\|none` | Everything the banner has, in order: `keyring`, `certificate:KEY`, `sign-in:KEY`, `password:KEY`, `quota:KEY`, `offline`, `unreachable` |
//! | `assert-opened last URL\|none`, `assert-opened count N` | The last link handed to the browser opener, and how many were |
//! | `assert-toast TEXT` | The toast on screen says TEXT (`setup-dismiss-toasts` clears earlier ones) |
//! | `assert-text-contains NAME TEXT` | A label's text contains TEXT |
//! | `assert-field NAME TEXT` | A text field's text or a choice's item (never the password's) |
//! | `assert-account-status ADDRESS synced\|syncing\|offline\|needs-sign-in\|certificate\|quota` | The sidebar's state of the account with ADDRESS |

use super::{Harness, StepResult, WAIT_TIMEOUT, equal, with_timeout};
use crate::desktop::DesktopEvent;
use crate::engine::EngineOptions;
use crate::setup::{self, sources};
use crate::status::Waiting;
use crate::window::Attention;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::account::{
    AccountKey, AccountSettings, AuthMethod, DetectSource, DetectedSettings, Detection, Endpoint,
    HelpLink, LoginError, LoginFailure, OAuthProvider, Protocol, Security, UnsupportedReason,
    UsernameForm,
};
use sealwax_domain::model::{FolderId, FolderRole, MessageId};
use sealwax_engine::api::{
    AccountError, BodyError, Command, Event, Password, RequestId, SyncStatus,
};
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
    ("engine-reply", 1, 3),
    ("engine-reply-stale", 2, 3),
    ("setup-type", 2, 2),
    ("setup-choose", 2, 2),
    ("setup-click", 1, 1),
    ("setup-key", 1, 1),
    ("setup-respond", 2, 2),
    ("setup-dismiss-toasts", 0, 0),
    ("assert-setup", 2, 2),
    ("assert-text-contains", 2, 2),
    ("assert-field", 2, 2),
    ("assert-account-status", 2, 2),
    ("assert-opened", 2, 2),
    ("assert-toast", 1, 1),
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
            | "assert-opened"
            | "assert-toast"
    )
}

/// The last command the script took from the app, for the replies.
#[derive(Clone)]
struct Last {
    request: RequestId,
    settings: Option<AccountSettings>,
    key: Option<AccountKey>,
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
            let toast =
                glib::Type::from_name("AdwToastWidget").ok_or("libadwaita has no toast widget")?;
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
            dropdown.set_selected(match args[1].as_str() {
                "tls" => 0,
                "starttls" => 1,
                other => return Err(format!("use tls or starttls, not {other}")),
            });
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
        "assert-text-contains" => {
            let found = harness.widget(&args[0])?;
            let text = if let Some(label) = found.downcast_ref::<gtk::Label>() {
                label.label().to_string()
            } else if let Some(button) = found.downcast_ref::<gtk::Button>() {
                button
                    .label()
                    .map(|label| label.to_string())
                    .unwrap_or_default()
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
/// `sign-in:KEY` (Microsoft), `password:KEY`, `quota:KEY`, `offline` (no network) or
/// `unreachable` (no server answers).
fn waiting_name(waiting: &Waiting) -> String {
    let kind = match &waiting.attention {
        Attention::NoKeyring => "keyring",
        Attention::CertificateChanged { .. } => "certificate",
        Attention::SignIn { browser: true, .. } => "sign-in",
        Attention::SignIn { browser: false, .. } => "password",
        Attention::QuotaFull { .. } => "quota",
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
    format!("{}:{}:{security}", endpoint.host, endpoint.port)
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
            ("key", settings.key.to_string()),
            ("imap", endpoint_text(&settings.imap)),
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
        Command::RemoveAccount { request, key } => (
            "remove-account",
            Some(*request),
            vec![("key", key.to_string())],
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
        Command::UpdatePassword { request, key, .. } => (
            "update-password",
            Some(*request),
            vec![("key", key.to_string())],
        ),
        Command::Shutdown => ("shutdown", None, Vec::new()),
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
        return match fake.commands.try_recv() {
            Ok(command) => Err(format!("the app sent {}", fields(&command, None).0)),
            Err(_) => Ok(()),
        };
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
            Command::RemoveAccount { key, .. } => Some(key.clone()),
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
        ("added", [outcome]) => Event::AccountAdded {
            request,
            result: match outcome.as_str() {
                "ok" => Ok(add_to_store(harness, &last).await?),
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
        _ => return Err(format!("no reply {}", args.join(" "))),
    };
    send_event(harness, &fake, event).await
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

fn protocol_named(name: &str) -> Result<Protocol, String> {
    match name {
        "imap" => Ok(Protocol::Imap),
        "smtp" => Ok(Protocol::Smtp),
        other => Err(format!("use imap or smtp, not {other}")),
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

/// What the engine does on `AddAccount`, for the store: the account and its role folders, so
/// the sidebar shows it as it would after a real setup.
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
                name: settings.name.clone(),
                address: settings.address.clone(),
            },
        );
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
    }
}

/// Scripted detection results. Provider names are real ones' or the mock's fictional ones;
/// every host is under `example.test`.
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
        "fjordpost" => Detection::Found(DetectedSettings {
            imap: tls("mail.example.test", 993),
            smtp: tls("mail.example.test", 465),
            ..found("Fjordpost", DetectSource::Autoconfig)
        }),
        "fastmail" => Detection::Found(DetectedSettings {
            second_username: None,
            app_password: link(
                "Create an app password",
                "https://example.test/settings/app-passwords",
            ),
            ..found("Fastmail", DetectSource::Preset)
        }),
        "gmail" => Detection::Found(DetectedSettings {
            second_username: None,
            app_password: link(
                "Open the Google account page",
                "https://example.test/apppasswords",
            ),
            ..found("Gmail", DetectSource::Preset)
        }),
        "imap-off" => Detection::Found(DetectedSettings {
            enable_imap: link(
                "Open the IMAP settings",
                "https://example.test/settings/imap",
            ),
            ..found("Fjordpost", DetectSource::Ispdb)
        }),
        "confirm" => Detection::Found(DetectedSettings {
            imap: tls("imap.fjordhost.example.test", 993),
            smtp: Endpoint {
                host: "smtp.fjordhost.example.test".to_owned(),
                port: 587,
                security: Security::StartTls,
            },
            username: UsernameForm::LocalPart,
            second_username: Some(UsernameForm::Address),
            confirm: true,
            ..found("Fjordhost", DetectSource::Mx)
        }),
        "other-username" => Detection::Found(DetectedSettings {
            username: UsernameForm::Other,
            second_username: None,
            ..found("Fjordpost", DetectSource::Srv)
        }),
        "microsoft" => Detection::Found(DetectedSettings {
            auth: AuthMethod::OAuth(OAuthProvider::Microsoft),
            second_username: None,
            ..found("Outlook.com", DetectSource::Preset)
        }),
        "bridge" => Detection::Unsupported {
            provider: "Proton Mail".to_owned(),
            reason: UnsupportedReason::BridgeOnly,
        },
        "no-imap" => Detection::Unsupported {
            provider: "Tuta".to_owned(),
            reason: UnsupportedReason::NoImap,
        },
        "no-mail" => Detection::NoMail,
        "not-found" => Detection::NotFound,
        other => return Err(format!("no detection fixture {other}")),
    })
}
