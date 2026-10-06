//! M7's exit check "a notification click opens the right message" (ML-083), for the lead's
//! authorized live run: the real Sealwax over a scratch, fictional mailbox sends new-mail
//! notifications, and a click on each opens its message. Two steps: with the main window open but
//! not focused (the user clicks another window first), and from background mode with no window.
//! Each step records which message opened, whether the window had to be built, and the time from
//! the server's `ActionInvoked` (seen by a filter on the app's own D-Bus connection) to the first
//! frame painted with the message in the reader.
//!
//! By default it is a dry run under `tools/headless.sh`: the stand-in notification server on the
//! wrapper's private bus receives the notifications and clicks them. With `--live` it sends them
//! to the desktop's notification server (omarchy-shell) and waits for the user's clicks; it
//! injects no input, uses scratch config, data, state, cache and theme folders under `.local/`, a
//! test app-id, and ends by itself.
//!
//! ```sh
//! cargo build -p sealwax --example notify_live
//! tools/headless.sh 56 target/debug/examples/notify_live          # dry run
//! target/debug/examples/notify_live --live                        # authorized live run only
//! ```

#[path = "../src/selftest/notify/server.rs"]
#[allow(dead_code)]
mod server;
#[path = "reader_support/mod.rs"]
mod support;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use sealwax_app::application::{AppConfig, SealwaxApplication};
use sealwax_app::mailbox::folder_key;
use sealwax_app::notify;
use sealwax_app::window;
use sealwax_domain::model::{AccountId, Flags, FolderId, FolderRole, MessageId};
use sealwax_engine::api::{Command, Event};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{FolderSpec, NewAccount, Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.pixdevsapps.Sealwax.NotifyLive";
/// The whole run ends by itself after this long.
const DRY_DEADLINE: Duration = Duration::from_secs(60);
const LIVE_DEADLINE: Duration = Duration::from_secs(300);
/// How long the live run waits for the user (a click, or another window focused).
const LIVE_WAIT: Duration = Duration::from_secs(90);
/// How long the stand-in waits before it clicks.
const DRY_CLICK_AFTER: Duration = Duration::from_millis(300);
/// The dry run's keep-alive tick for Broadway's main loop.
const DRY_TICK: Duration = Duration::from_millis(4);

/// The scratch mailbox: one account, its Inbox with a few fictional messages, an Archive.
struct Mailbox {
    account: AccountId,
    inbox: FolderId,
    archive: FolderId,
    /// A message already read, shown before the steps.
    warm: MessageId,
    /// The two messages the steps announce, in order.
    steps: [MessageId; 2],
}

fn scratch_environment() -> Result<PathBuf, String> {
    let root = support::scratch("notify-live");
    let xdg = root.join("xdg");
    let theme = root.join("omarchy");
    for dir in [
        xdg.join("config"),
        xdg.join("cache"),
        xdg.join("data"),
        xdg.join("state"),
        theme.clone(),
    ] {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let vars: [(&str, PathBuf); 5] = [
        ("XDG_CONFIG_HOME", xdg.join("config")),
        ("XDG_CACHE_HOME", xdg.join("cache")),
        ("XDG_DATA_HOME", xdg.join("data")),
        ("XDG_STATE_HOME", xdg.join("state")),
        (sealwax_infrastructure::omarchy::STATE_DIR_ENV, theme),
    ];
    for (name, value) in vars {
        // SAFETY: called first thing in main, before any other thread starts.
        unsafe { std::env::set_var(name, value) };
    }
    Ok(root)
}

fn raw(subject: &str, from: &str, serial: u32) -> Vec<u8> {
    format!(
        "From: {from} <test@example.test>\r\nTo: Sealwax <sealwax@example.test>\r\n\
         Subject: {subject}\r\nMessage-ID: <notify-live-{serial}@example.test>\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         A fictional message for the notification check. Nothing to do here.\r\n"
    )
    .into_bytes()
}

/// Fills the scratch store before Sealwax opens it.
fn fill(dirs: &SealwaxDirs) -> Result<Mailbox, String> {
    let blobs = BlobStore::open(&dirs.blobs_dir()).map_err(|error| error.to_string())?;
    let store = Store::open(
        &dirs.store_file(),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let account = store
        .add_account(NewAccount {
            name: "Notification check".into(),
            address: "sealwax@example.test".into(),
        })
        .recv_blocking()
        .map_err(|error| error.to_string())?
        .value;
    let mut folders = Vec::new();
    for (name, role) in [
        ("INBOX", FolderRole::Inbox),
        ("Archive", FolderRole::Archive),
    ] {
        let folder = store
            .upsert_folder(FolderSpec::local(account, name, role))
            .recv_blocking()
            .map_err(|error| error.to_string())?
            .value;
        folders.push(folder);
    }
    let (inbox, archive) = (folders[0], folders[1]);
    let now = glib::real_time() / 1_000_000;
    let messages = [
        ("Older, already here", "Sealwax test", 3_600, Flags::SEEN),
        ("Also already here", "Sealwax test", 7_200, Flags::SEEN),
        (
            "Step 1 of 2: click this notification",
            "Sealwax test",
            60,
            Flags::NONE,
        ),
        (
            "Step 2 of 2: click this notification",
            "Sealwax test",
            30,
            Flags::NONE,
        ),
    ];
    let raws = messages
        .iter()
        .enumerate()
        .map(|(index, (subject, from, ago, flags))| RawMessage {
            folder: inbox,
            flags: *flags,
            internal_date: Some(now - ago),
            raw: raw(subject, from, index as u32),
            ..RawMessage::default()
        })
        .collect();
    let prepared = ingest::prepare(&blobs, raws, 1).map_err(|error| error.to_string())?;
    let ids: Vec<MessageId> = store
        .ingest(prepared.messages)
        .recv_blocking()
        .map_err(|error| error.to_string())?
        .ids
        .into_iter()
        .flatten()
        .collect();
    store.close().map_err(|error| error.to_string())?;
    if ids.len() != 4 {
        return Err(format!("{} of 4 messages stored", ids.len()));
    }
    Ok(Mailbox {
        account,
        inbox,
        archive,
        warm: ids[0],
        steps: [ids[2], ids[3]],
    })
}

/// `ActionInvoked` signals as they reach the app's own connection (GDBus's worker thread).
type Invoked = Arc<Mutex<Vec<Instant>>>;

fn watch_invoked(app: &SealwaxApplication) -> Invoked {
    let invoked: Invoked = Arc::default();
    if let Some(connection) = app.dbus_connection() {
        let record = invoked.clone();
        connection.add_filter(move |_, message, incoming| {
            if incoming
                && message.message_type() == gio::DBusMessageType::Signal
                && message.interface().as_deref() == Some("org.freedesktop.Notifications")
                && message.member().as_deref() == Some("ActionInvoked")
            {
                record
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(Instant::now());
            }
            Some(message.clone())
        });
    }
    invoked
}

async fn sleep(duration: Duration) {
    glib::timeout_future(duration).await;
}

async fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while !check() {
        if started.elapsed() > limit {
            return false;
        }
        sleep(Duration::from_millis(20)).await;
    }
    true
}

/// Hyprland's active window class, read once (live only).
fn active_class() -> String {
    std::process::Command::new("hyprctl")
        .args(["-j", "activewindow"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| {
            let start = text.find("\"class\": \"")? + 10;
            let end = text[start..].find('"')? + start;
            Some(text[start..end].to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

struct Run {
    app: SealwaxApplication,
    mailbox: Mailbox,
    events: async_channel::Sender<Event>,
    invoked: Invoked,
    live: bool,
    stand_in: Option<server::StandIn>,
    report: RefCell<String>,
}

impl Run {
    fn line(&self, text: &str) {
        println!("{text}");
        let _ = writeln!(self.report.borrow_mut(), "{text}");
    }

    /// The stand-in clicks the newest notification it hasn't clicked (dry run only).
    async fn stand_in_clicks(&self, count: usize) -> Result<(), String> {
        let Some(stand_in) = &self.stand_in else {
            return Ok(());
        };
        if !wait_until(Duration::from_secs(5), || {
            stand_in.received().len() >= count
        })
        .await
        {
            return Err("the stand-in received no notification".into());
        }
        sleep(DRY_CLICK_AFTER).await;
        let received = stand_in.received();
        let newest = received.last().ok_or("nothing received")?;
        self.line(&format!(
            "  stand-in received: app_name {:?}, {} actions, urgency {}",
            newest.app_name,
            newest.actions.len(),
            newest.hint("urgency").unwrap_or("none")
        ));
        stand_in.click(newest.id, "default").map(|_| ())
    }

    async fn step(&self, number: usize, message: MessageId) -> Result<(), String> {
        let state = self.app.state();
        let clicks_before = notify::clicks().len();
        let invoked_before = self
            .invoked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        self.events
            .send(Event::NewMail {
                account: self.mailbox.account,
                folder: self.mailbox.inbox,
                messages: vec![message],
            })
            .await
            .map_err(|_| "the app's end of the engine is gone")?;
        if self.live {
            println!("  A notification \"Sealwax test\" is on its way: click it.");
        } else {
            self.stand_in_clicks(number).await?;
        }
        let limit = if self.live {
            LIVE_WAIT
        } else {
            Duration::from_secs(10)
        };
        if !wait_until(limit, || notify::clicks().len() > clicks_before).await {
            let signals = self
                .invoked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
                - invoked_before;
            return Err(format!(
                "step {number}: no click within {} s ({signals} ActionInvoked signals reached \
                 the app)",
                limit.as_secs()
            ));
        }
        let click = notify::clicks()[clicks_before];
        wait_until(Duration::from_secs(10), || {
            notify::clicks()[clicks_before].painted.is_some()
        })
        .await;
        sleep(Duration::from_millis(250)).await;
        let click_now = notify::clicks()[clicks_before];
        let invoked = self
            .invoked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(invoked_before)
            .copied();
        let selected = state.list().and_then(|list| list.selected());
        let shown = state.reader().and_then(|reader| reader.message());
        let active = state
            .window()
            .is_some_and(|window| window.window().is_active());
        let ms = |from: Option<Instant>, to: Option<Instant>| match (from, to) {
            (Some(from), Some(to)) => {
                format!("{:.1} ms", to.duration_since(from).as_secs_f64() * 1000.0)
            }
            _ => "not measured".to_owned(),
        };
        self.line(&format!(
            "step {number}: had a window {}; opened the right message: list {}, reader {}",
            click.had_window,
            selected == Some(message),
            shown == Some(message)
        ));
        self.line(&format!(
            "  ActionInvoked → action {}; → selected {}; → painted {}; action → painted {}",
            ms(invoked, Some(click.action)),
            ms(invoked, click_now.selected),
            ms(invoked, click_now.painted),
            ms(Some(click.action), click_now.painted)
        ));
        self.line(&format!(
            "  window active afterwards {active}{}",
            if self.live {
                format!("; Hyprland's active class {}", active_class())
            } else {
                String::new()
            }
        ));
        if selected == Some(message) && shown == Some(message) && click_now.painted.is_some() {
            Ok(())
        } else {
            Err(format!("step {number}: the message wasn't shown"))
        }
    }

    /// Shows a message already read, as a session that has read mail has: the reader's web
    /// process runs before the first click, as it would.
    async fn warm_up(&self) -> Result<(), String> {
        let state = self.app.state();
        let (window, list) = (
            state.window().ok_or("no window")?,
            state.list().ok_or("no list")?,
        );
        window.select_folder(&folder_key(self.mailbox.inbox));
        let warm = self.mailbox.warm;
        let found = wait_until(Duration::from_secs(5), || {
            !list.model().is_pending() && list.model().position_of(warm).is_some()
        })
        .await;
        if let Some(position) = list.model().position_of(warm).filter(|_| found) {
            list.select_position(position);
        }
        wait_until(Duration::from_secs(10), || {
            state.reader().and_then(|reader| reader.message()) == Some(warm)
        })
        .await;
        window.select_folder(&folder_key(self.mailbox.archive));
        sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    async fn run(&self) -> Result<(), String> {
        let state = self.app.state();
        let window = state.window().ok_or("no window")?;
        self.warm_up().await?;
        self.line(&format!(
            "notify_live {}: app-id {APP_ID}",
            if self.live { "live" } else { "dry run" }
        ));

        if self.live {
            println!(
                "Step 1 of 2: click any other window, so the Sealwax window loses the focus \
                 (waiting up to {} s).",
                LIVE_WAIT.as_secs()
            );
            if !wait_until(LIVE_WAIT, || !window.window().is_active()).await {
                return Err("the window kept the focus".into());
            }
            sleep(Duration::from_secs(2)).await;
        }
        self.step(1, self.mailbox.steps[0]).await?;

        if self.live {
            println!("Step 2 of 2: the window closes by itself (background mode).");
        }
        if !state.is_background() {
            return Err("background mode is off in the scratch config.toml".into());
        }
        if let Some(window) = state.window() {
            window.window().close();
        }
        if !wait_until(Duration::from_secs(5), || state.window().is_none()).await {
            return Err("the window didn't close".into());
        }
        sleep(Duration::from_secs(if self.live { 3 } else { 1 })).await;
        self.step(2, self.mailbox.steps[1]).await
    }
}

fn main() -> glib::ExitCode {
    let live = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--live") => true,
        Some(other) => {
            eprintln!("unknown argument {other}; use --live or nothing");
            return glib::ExitCode::from(2);
        }
    };
    let broadway = std::env::var_os("GDK_BACKEND").is_some_and(|backend| backend == "broadway");
    if live == broadway {
        eprintln!(
            "notify_live: run the dry run through tools/headless.sh; --live is only for the \
             lead's authorized run on the live session"
        );
        return glib::ExitCode::from(2);
    }
    glib::set_prgname(Some("sealwax"));
    glib::set_application_name("Sealwax");
    let root = match scratch_environment() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("notify_live: the scratch folders: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let _ = sealwax_infrastructure::logging::init();
    support::count_criticals();
    let dirs = SealwaxDirs::under(&root.join("sealwax"));
    // Background mode's one-time notice counts as shown in the scratch state, so the steps'
    // notifications are the only ones (the notice has its own checks in background-*).
    if let Err(error) = sealwax_app::background::remember_notice(&dirs.state) {
        eprintln!("notify_live: the scratch state: {error}");
        return glib::ExitCode::FAILURE;
    }
    let mailbox = match fill(&dirs) {
        Ok(mailbox) => mailbox,
        Err(error) => {
            eprintln!("notify_live: the scratch mailbox: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let stand_in = if live {
        None
    } else {
        match server::StandIn::start(&server::CAPABILITIES) {
            Ok(stand_in) => Some(stand_in),
            Err(error) => {
                eprintln!("notify_live: {error}");
                return glib::ExitCode::FAILURE;
            }
        }
    };
    // Headless, the reader examples' stand-in Broadway client (a large screen for the window).
    let _client = if live {
        None
    } else {
        match support::broadway::connect() {
            Ok(client) => Some(client),
            Err(error) => {
                eprintln!("notify_live: the Broadway daemon: {error}");
                return glib::ExitCode::FAILURE;
            }
        }
    };
    let config = AppConfig {
        app_id: APP_ID.into(),
        dirs,
        exit_after: None,
        tiling: window::tiling_desktop(),
        engine: None,
        microsoft_client_id: None,
    };
    let app = SealwaxApplication::new(config, Instant::now());
    let deadline = if live { LIVE_DEADLINE } else { DRY_DEADLINE };
    let outcome: Rc<RefCell<Option<Result<(), String>>>> = Rc::default();
    let stand_in = Rc::new(RefCell::new(stand_in));
    {
        let outcome = outcome.clone();
        let weak = app.downgrade();
        let stand_in = stand_in.clone();
        app.state().connect_first_window(move |_| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            glib::timeout_add_local_once(deadline, move || {
                eprintln!("notify_live: the {} s deadline passed", deadline.as_secs());
                std::process::exit(1);
            });
            let (commands, kept) = async_channel::unbounded::<Command>();
            let (events, received) = async_channel::unbounded::<Event>();
            app.state().engine().attach(commands, received);
            if !live {
                // Without a fast timer this dry run's frames came about 1.4 s after the reader
                // had the message (not explained further); with it, 42 ms. Headless only.
                glib::timeout_add_local(DRY_TICK, || glib::ControlFlow::Continue);
            }
            let invoked = watch_invoked(&app);
            let run = Run {
                app: app.clone(),
                mailbox,
                events,
                invoked,
                live,
                stand_in: stand_in.take(),
                report: RefCell::default(),
            };
            glib::spawn_future_local(async move {
                let _commands = kept;
                let result = run.run().await;
                if let Err(error) = &result {
                    run.line(&format!("FAILED: {error}"));
                }
                run.line(&format!("GLib criticals: {}", support::criticals()));
                let stamp = glib::DateTime::now_local()
                    .ok()
                    .and_then(|now| now.format("%Y%m%d-%H%M%S").ok())
                    .map_or_else(|| "run".to_owned(), |text| text.to_string());
                let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("../.local/notify-live/notify-live-{stamp}.txt"));
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&path, run.report.borrow().as_bytes()).is_ok() {
                    println!("REPORT {}", path.display());
                }
                outcome.replace(Some(result));
                app.state().quit(&app);
            });
        });
    }
    app.run_with_args(&["sealwax"]);
    match outcome.take() {
        Some(Ok(())) => glib::ExitCode::SUCCESS,
        _ => glib::ExitCode::FAILURE,
    }
}
