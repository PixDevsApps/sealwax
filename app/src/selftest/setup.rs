//! The setup assistant's self-test commands (ML-033): a fake engine on the app's
//! [`EngineLink`](crate::engine_link::EngineLink) that the script answers with scripted events,
//! and the assistant driven as a user would. Every address, name and server is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `engine-attach`, `engine-detach` | Connects the fake engine, or leaves the app without one |
//! | `engine-expect detect\|verify\|add-account [KEY=VALUE…]` | Waits for the app's next command and checks its fields: `address`, `name`, `key`, `imap` and `smtp` (`host:port:tls\|starttls`), `username`, `second` (`none`), `password` (compared, never printed) |
//! | `engine-expect none` | The app sent nothing more |
//! | `engine-reply progress SOURCE` | `DetectProgress` for the last command: `preset`, `autoconfig`, `ispdb`, `mx`, `srv`, `probe` |
//! | `engine-reply detected FIXTURE` | `Detected`: `fjordpost`, `fastmail`, `gmail`, `imap-off`, `confirm`, `other-username`, `microsoft`, `bridge`, `no-imap`, `no-mail`, `not-found` |
//! | `engine-reply verified ok [USERNAME]`, `engine-reply verified imap\|smtp FAILURE` | `Verified`: `wrong-password`, `app-password`, `imap-disabled`, `oauth`, `no-tls`, `certificate`, `unreachable`, `timeout`, `other` |
//! | `engine-reply added ok\|keyring\|settings-file\|store\|exists\|missing` | `AccountAdded`; `ok` first adds the account and its role folders to the app's store, as the engine would |
//! | `engine-reply-stale …` | The same events with a request id the app never used |
//! | `setup-type NAME TEXT`, `setup-choose NAME tls\|starttls`, `setup-click NAME` | Types into a field, picks a security, clicks a visible, sensitive button |
//! | `setup-key enter\|escape` | Enter in the focused field, Escape in the dialog |
//! | `assert-setup open\|step\|sources\|focus VALUE` | The assistant's state |
//! | `assert-text-contains NAME TEXT` | A label's text contains TEXT |
//! | `assert-field NAME TEXT` | A text field's text or a choice's item (never the password's) |

use super::{Harness, StepResult, WAIT_TIMEOUT, equal, with_timeout};
use crate::setup::{self, sources};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::account::{
    AccountSettings, AuthMethod, DetectSource, DetectedSettings, Detection, Endpoint, HelpLink,
    LoginError, LoginFailure, OAuthProvider, Protocol, Security, UnsupportedReason, UsernameForm,
};
use sealwax_domain::model::FolderRole;
use sealwax_engine::api::{AccountError, Command, Event, Password, RequestId};
use sealwax_infrastructure::store::{FolderSpec, NewAccount};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("engine-attach", 0, 0),
    ("engine-detach", 0, 0),
    ("engine-expect", 1, 12),
    ("engine-reply", 2, 3),
    ("engine-reply-stale", 2, 3),
    ("setup-type", 2, 2),
    ("setup-choose", 2, 2),
    ("setup-click", 1, 1),
    ("setup-key", 1, 1),
    ("assert-setup", 2, 2),
    ("assert-text-contains", 2, 2),
    ("assert-field", 2, 2),
];

pub fn handles(command: &str) -> bool {
    command.starts_with("engine-") || command.starts_with("setup-")
}

pub fn checks(assertion: &str) -> bool {
    matches!(
        assertion,
        "assert-setup" | "assert-text-contains" | "assert-field"
    )
}

/// The last command the script took from the app, for the replies.
#[derive(Clone)]
struct Last {
    request: RequestId,
    settings: Option<AccountSettings>,
}

struct Fake {
    commands: async_channel::Receiver<Command>,
    events: async_channel::Sender<Event>,
    last: RefCell<Option<Last>>,
}

thread_local! {
    static FAKE: RefCell<Option<Rc<Fake>>> = const { RefCell::new(None) };
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
                })))
            });
            Ok(())
        }
        "engine-detach" => {
            harness.state.engine().detach();
            FAKE.with(|fake| fake.take());
            Ok(())
        }
        "engine-expect" => expect(harness, args).await,
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
                other => Err(format!(
                    "assert-setup checks open, step, sources or focus, not {other}"
                )),
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

fn endpoint_text(endpoint: &Endpoint) -> String {
    let security = match endpoint.security {
        Security::Tls => "tls",
        Security::StartTls => "starttls",
    };
    format!("{}:{}:{security}", endpoint.host, endpoint.port)
}

/// A command's kind, its request and its fields as `KEY=VALUE` pairs; never the password.
fn fields(command: &Command) -> (&'static str, Option<RequestId>, Vec<(&'static str, String)>) {
    let settings_fields = |settings: &AccountSettings| {
        vec![
            ("address", settings.address.clone()),
            ("name", settings.name.clone()),
            ("key", settings.key.to_string()),
            ("imap", endpoint_text(&settings.imap)),
            ("smtp", endpoint_text(&settings.smtp)),
            ("username", settings.username.clone()),
        ]
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
            request, settings, ..
        } => ("add-account", Some(*request), settings_fields(settings)),
        Command::RemoveAccount { request, .. } => ("remove-account", Some(*request), Vec::new()),
        Command::Shutdown => ("shutdown", None, Vec::new()),
    }
}

fn password(command: &Command) -> Option<&Password> {
    match command {
        Command::Verify { password, .. } | Command::AddAccount { password, .. } => Some(password),
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
            Ok(command) => Err(format!("the app sent {}", fields(&command).0)),
            Err(_) => Ok(()),
        };
    }
    let command = with_timeout(fake.commands.recv(), WAIT_TIMEOUT)
        .await
        .ok_or_else(|| format!("no {} came from the app", args[0]))?
        .map_err(|_| "the app closed the engine's channel".to_owned())?;
    let (kind, request, fields) = fields(&command);
    let shown: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    println!("  # {kind} {}", shown.join(" "));
    fake.last.replace(Some(Last {
        request: request.unwrap_or_default(),
        settings: settings(&command).cloned(),
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
                protocol: match protocol.as_str() {
                    "imap" => Protocol::Imap,
                    "smtp" => Protocol::Smtp,
                    other => return Err(format!("use imap or smtp, not {other}")),
                },
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
        _ => return Err(format!("no reply {}", args.join(" "))),
    };
    fake.events
        .send(event)
        .await
        .map_err(|_| "the app's end of the engine is gone".to_owned())?;
    // The link hands events over on the main loop; let it run once more.
    glib::timeout_future(Duration::from_millis(10)).await;
    harness.wait_until_settled().await;
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
        .add_account(NewAccount {
            name: settings.name.clone(),
            address: settings.address.clone(),
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
