//! Self-test commands for background mode (M7: ML-084): the notices `background.rs` sent (its
//! record, so no notification server is needed), its marker, Start Sealwax at Login's entry in
//! a sandboxed configuration home (`reply-sandbox`), a Sealwax of its own that closes its
//! window as a user would, and activation → first frame from background mode. Every name and
//! address is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `background-spawn NAME [ARGS…]` | As `spawn-bg` (its own id and folders under `$TMP/NAME/`, output in `$TMP/NAME.err`), and it closes its main window 300 ms after its first frame (`SEALWAX_CLOSE_WINDOW_AFTER_MS`) |
//! | `background-menu [close]` | Opens (or closes) the main window's menu, for screenshots |
//! | `background-measure N` | N times: closes the main window, waits until it is gone, calls D-Bus `Activate` on this instance and times it to the new window's first frame; prints every run, p50, p95 and the rows bound at that frame |
//! | `background-watch [composer]` | Watches the main window, its list and its reader (or the newest composer window) for `assert-freed` |
//! | `assert-freed` | What `background-watch` watched is gone: nothing holds it after its window closed |
//! | `background-memory LABEL` | Prints this process's proportional set size (PSS, `smaps_rollup`) and its descendant processes' (WebKit's), by name |
//! | `assert-notices N` | `background.rs` sent N notices in this run |
//! | `assert-notice title\|body TEXT` | The latest one's title is TEXT, or its body holds TEXT |
//! | `assert-notice-marker BOOL` | The one-time notice's marker is in this run's state folder |
//! | `assert-autostart on\|off` | The run's autostart entry exists and starts Sealwax (on), or doesn't (off); prints it. Fails outside the run's scratch directory |
//! | `assert-autostart-toggle BOOL` | Start Sealwax at Login's check mark |
//! | `assert-hidden-start BOOL` | A `--background` start holds Sealwax |
//! | `assert-reader-built BOOL` | The main window's reader (its WebKit view) exists: released when the window closes, built after a new window's first frame |
//! | `assert-reader-selected` | The reader shows the list's selected message, in the window on screen |
//! | `assert-file-count PATH TEXT N` | PATH (a log in `$TMP`) has N lines holding TEXT; prints them |
//! | `assert-login-dialog heading\|body TEXT` | Start Sealwax at Login's question on screen: its heading, or text its body holds |

use super::{Harness, StepResult, WAIT_TIMEOUT, equal, number};
use crate::application::APP_ID_ENV;
use crate::autostart;
use crate::background::{self, CLOSE_AFTER_ENV, NOTICE_MARKER};
use crate::config::EXIT_AFTER_ENV;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::ActionId;
use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("background-spawn", 1, usize::MAX),
    ("background-menu", 0, 1),
    ("background-measure", 1, 1),
    ("background-memory", 1, 1),
    ("background-watch", 0, 1),
    ("assert-freed", 0, 0),
    ("assert-notices", 1, 1),
    ("assert-notice", 2, 2),
    ("assert-notice-marker", 1, 1),
    ("assert-autostart", 1, 1),
    ("assert-autostart-toggle", 1, 1),
    ("assert-hidden-start", 1, 1),
    ("assert-reader-built", 1, 1),
    ("assert-reader-selected", 0, 0),
    ("assert-file-count", 3, 3),
    ("assert-login-dialog", 2, 2),
];

thread_local! {
    /// What `background-watch` watches, for `assert-freed`.
    static WATCHED: RefCell<Vec<(&'static str, glib::WeakRef<gtk::Widget>)>> =
        const { RefCell::new(Vec::new()) };
}

/// How long after its first frame a `background-spawn` Sealwax closes its window.
const CLOSE_AFTER_MS: &str = "300";

/// A spawned Sealwax quits by itself after this long, should a test lose track of it.
const SAFETY_EXIT_MS: &str = "120000";

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "background-spawn" => spawn(harness, arg(0), &args[1..]),
        "background-menu" => {
            let menu = harness
                .widget("list-menu")?
                .downcast::<gtk::MenuButton>()
                .map_err(|_| "list-menu isn't a menu button".to_owned())?;
            match args.first().map(String::as_str) {
                None => menu.popup(),
                Some("close") => menu.popdown(),
                Some(other) => return Err(format!("use background-menu [close], not {other}")),
            }
            harness.wait_until_settled().await;
            Ok(())
        }
        "background-measure" => measure(harness, number(arg(0))?).await,
        "background-memory" => {
            memory(arg(0));
            Ok(())
        }
        "background-watch" => {
            if args.first().is_some_and(|what| what == "composer") {
                let composer = crate::composer::composers()
                    .pop()
                    .ok_or("no composer is open")?;
                let watched = vec![(
                    "composer",
                    composer.window().upcast_ref::<gtk::Widget>().downgrade(),
                )];
                WATCHED.with(|current| current.replace(watched));
                return Ok(());
            }
            let window = harness.window()?;
            let mut watched = vec![(
                "window",
                window.window().upcast_ref::<gtk::Widget>().downgrade(),
            )];
            if let Some(list) = harness.state.list() {
                watched.push((
                    "list",
                    list.widget().upcast_ref::<gtk::Widget>().downgrade(),
                ));
            }
            if let Some(reader) = harness.state.reader() {
                watched.push(("reader", reader.widget().downgrade()));
            }
            WATCHED.with(|current| current.replace(watched));
            Ok(())
        }
        "assert-freed" => {
            let alive: Vec<&str> = WATCHED.with(|watched| {
                watched
                    .borrow()
                    .iter()
                    .filter(|(_, weak)| weak.upgrade().is_some())
                    .map(|(name, _)| *name)
                    .collect()
            });
            equal("still alive", alive, Vec::<&str>::new())
        }
        "assert-notices" => equal("notices sent", background::sent().len(), number(arg(0))?),
        "assert-notice" => {
            let last = background::sent().pop().ok_or("no notice was sent")?;
            match arg(0) {
                "title" => equal("title", last.title.as_str(), arg(1)),
                "body" if last.body.contains(arg(1)) => {
                    println!("  # {}", last.body);
                    Ok(())
                }
                "body" => Err(format!("the body lacks {:?}: {}", arg(1), last.body)),
                other => Err(format!("use title or body, not {other}")),
            }
        }
        "assert-notice-marker" => {
            let marker = harness.state.config().dirs.state.join(NOTICE_MARKER);
            if !marker.starts_with(&harness.root) {
                return Err(format!("{} is outside the run", marker.display()));
            }
            equal(
                "the notice's marker",
                marker.exists(),
                super::script::boolean(arg(0))?,
            )
        }
        "assert-autostart" => check_autostart(harness, arg(0)),
        "assert-autostart-toggle" => {
            let state = harness
                .app
                .action_state(ActionId::StartAtLogin.name())
                .and_then(|state| state.get::<bool>())
                .ok_or("Start Sealwax at Login has no state")?;
            equal(
                "Start Sealwax at Login",
                state,
                super::script::boolean(arg(0))?,
            )
        }
        "assert-hidden-start" => equal(
            "held for --background",
            harness.state.is_hidden_start(),
            super::script::boolean(arg(0))?,
        ),
        "assert-reader-built" => equal(
            "the reader built",
            harness.state.reader().is_some(),
            super::script::boolean(arg(0))?,
        ),
        "assert-login-dialog" => {
            let dialog = harness
                .widget("start-at-login")?
                .downcast::<adw::AlertDialog>()
                .map_err(|_| "start-at-login isn't an alert dialog".to_owned())?;
            match arg(0) {
                "heading" => equal(
                    "heading",
                    dialog.heading().unwrap_or_default().as_str(),
                    arg(1),
                ),
                "body" if dialog.body().contains(arg(1)) => {
                    println!("  # {}", dialog.body().replace('\n', " "));
                    Ok(())
                }
                "body" => Err(format!("the body lacks {:?}: {}", arg(1), dialog.body())),
                other => Err(format!("use heading or body, not {other}")),
            }
        }
        "assert-file-count" => {
            let path = std::path::Path::new(arg(0));
            if !path.starts_with(&harness.root) {
                return Err(format!("{} is outside the run", path.display()));
            }
            let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", arg(0)))?;
            let lines: Vec<&str> = text.lines().filter(|line| line.contains(arg(1))).collect();
            for line in &lines {
                println!("  # {line}");
            }
            equal(
                &format!("lines with {:?}", arg(1)),
                lines.len(),
                number(arg(2))?,
            )
        }
        "assert-reader-selected" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            let selected = harness
                .state
                .list()
                .and_then(|list| list.selected())
                .ok_or("nothing is selected")?;
            equal("the reader's message", reader.message(), Some(selected))?;
            let window = harness.window()?;
            let inside = reader
                .widget()
                .ancestor(gtk::Window::static_type())
                .is_some_and(|ancestor| &ancestor == window.window().upcast_ref::<gtk::Widget>());
            equal("the reader in the window on screen", inside, true)?;
            equal("the reader mapped", reader.widget().is_mapped(), true)
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// A Sealwax of its own, as `spawn-bg` starts one, that closes its window as a user would.
fn spawn(harness: &Harness, name: &str, args: &[String]) -> StepResult {
    let app_id = format!("{}.{name}", harness.app_id);
    if !gtk::gio::Application::id_is_valid(&app_id) {
        return Err(format!("{name} can't be part of an application id"));
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let output = |extension: &str| {
        File::create(harness.root.join(format!("{name}.{extension}")))
            .map_err(|error| error.to_string())
    };
    let home = harness.root.join(name);
    let dir = |folder: &str| home.join(folder);
    let child = Command::new(exe)
        .args(args)
        .current_dir(&harness.root)
        .stdin(Stdio::null())
        .stdout(output("out")?)
        .stderr(output("err")?)
        .env(APP_ID_ENV, app_id)
        .env(EXIT_AFTER_ENV, SAFETY_EXIT_MS)
        .env(CLOSE_AFTER_ENV, CLOSE_AFTER_MS)
        .env("XDG_CONFIG_HOME", dir("config"))
        .env("XDG_DATA_HOME", dir("data"))
        .env("XDG_STATE_HOME", dir("state"))
        .env("XDG_CACHE_HOME", dir("cache"))
        .spawn()
        .map_err(|error| error.to_string())?;
    harness
        .processes
        .borrow_mut()
        .insert(name.to_owned(), child);
    Ok(())
}

/// The run's autostart entry, which must be in its sandbox.
fn check_autostart(harness: &Harness, wanted: &str) -> StepResult {
    let config = glib::user_config_dir();
    if !config.starts_with(&harness.root) {
        return Err(format!(
            "the configuration home is {}, outside the run: start with reply-sandbox",
            config.display()
        ));
    }
    let probe = autostart::probe(&config, &harness.app_id);
    match fs::read_to_string(&probe.path) {
        Ok(text) => {
            println!("  # {}:", probe.path.display());
            for line in text.lines() {
                println!("  #   {line}");
            }
        }
        Err(_) => println!("  # {} doesn't exist", probe.path.display()),
    }
    match wanted {
        "on" => equal("Sealwax starts at login", probe.on, true),
        "off" => equal("Sealwax starts at login", probe.on, false),
        other => Err(format!("use on or off, not {other}")),
    }
}

/// Activation → first frame from background mode, `runs` times.
async fn measure(harness: &Harness, runs: usize) -> StepResult {
    if !harness.state.is_background() {
        return Err("background mode is off: closing the window would quit".to_owned());
    }
    let mut times = Vec::new();
    for run in 1..=runs {
        if let Some(window) = harness.state.window() {
            window.window().close();
        }
        let started = Instant::now();
        while harness.state.window().is_some() {
            if started.elapsed() > WAIT_TIMEOUT {
                return Err("the window didn't close".to_owned());
            }
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        // Let what a close leaves behind (the reader's view, idle work) settle, as it would in
        // the minutes before a user comes back.
        glib::timeout_future(Duration::from_millis(300)).await;
        let painted: Rc<Cell<Option<(Instant, usize)>>> = Rc::default();
        let handlers: Rc<RefCell<Vec<glib::SignalHandlerId>>> = Rc::default();
        let added = {
            let (painted, state) = (painted.clone(), harness.state.clone());
            harness.app.connect_window_added(move |_, window| {
                let (painted, state) = (painted.clone(), state.clone());
                window.connect_map(move |window| {
                    let Some(clock) = window.frame_clock() else {
                        return;
                    };
                    let (painted, state) = (painted.clone(), state.clone());
                    let once: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
                    let handler = clock.connect_after_paint(glib::clone!(
                        #[strong]
                        once,
                        move |clock| {
                            if painted.get().is_none() {
                                let rows = state.list().map_or(0, |list| {
                                    list.bound_rows()
                                        .iter()
                                        .filter(|row| !row.placeholder && row.id.is_some())
                                        .count()
                                });
                                painted.set(Some((Instant::now(), rows)));
                            }
                            if let Some(handler) = once.take() {
                                clock.disconnect(handler);
                            }
                        }
                    ));
                    once.replace(Some(handler));
                });
            })
        };
        handlers.borrow_mut().push(added);
        let activated = Instant::now();
        let call = harness.bus_call(None).await;
        let waited = Instant::now();
        while painted.get().is_none() && waited.elapsed() < WAIT_TIMEOUT {
            glib::timeout_future(Duration::from_millis(2)).await;
        }
        for handler in handlers.take() {
            harness.app.disconnect(handler);
        }
        call?;
        let (at, rows) = painted.get().ok_or("the new window never painted")?;
        let ms = at.duration_since(activated).as_secs_f64() * 1000.0;
        println!("  # run {run}: Activate → first frame {ms:.1} ms, {rows} rows bound");
        times.push(ms);
    }
    times.sort_by(f64::total_cmp);
    let at = |share: f64| times[((times.len() as f64 * share).ceil() as usize).saturating_sub(1)];
    println!(
        "  # activation → first frame from background mode, {} runs: p50 {:.1} ms, p95 {:.1} ms, \
         max {:.1} ms",
        times.len(),
        at(0.5),
        at(0.95),
        times.last().copied().unwrap_or_default()
    );
    Ok(())
}

/// A process's proportional set size in KiB, from `/proc/PID/smaps_rollup`.
fn pss_kib(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("Pss:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
}

/// The processes under `pid`, with their names: WebKit's web and network processes, possibly
/// behind a sandbox launcher.
fn descendants(pid: u32) -> Vec<(u32, String)> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let processes: Vec<(u32, u32, String)> = entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|candidate| {
            let stat = fs::read_to_string(format!("/proc/{candidate}/stat")).ok()?;
            let (name, rest) = stat.split_once(" (")?.1.rsplit_once(") ")?;
            let parent = rest.split_whitespace().nth(1)?.parse().ok()?;
            Some((candidate, parent, name.to_owned()))
        })
        .collect();
    let mut found = Vec::new();
    let mut frontier = vec![pid];
    while let Some(parent) = frontier.pop() {
        for (child, of, name) in &processes {
            if *of == parent {
                found.push((*child, name.clone()));
                frontier.push(*child);
            }
        }
    }
    found
}

fn memory(label: &str) {
    let own = std::process::id();
    let mib = |kib: u64| kib as f64 / 1024.0;
    let own_pss = pss_kib(own).unwrap_or_default();
    let children: Vec<(String, u64)> = descendants(own)
        .into_iter()
        .map(|(pid, name)| (name, pss_kib(pid).unwrap_or_default()))
        .collect();
    let total = own_pss + children.iter().map(|(_, kib)| kib).sum::<u64>();
    let listed: Vec<String> = children
        .iter()
        .map(|(name, kib)| format!("{name} {:.1}", mib(*kib)))
        .collect();
    println!(
        "  # {label}: PSS {:.1} MiB in all; sealwax {:.1} MiB; {} processes under it: {}",
        mib(total),
        mib(own_pss),
        children.len(),
        if listed.is_empty() {
            "none".to_owned()
        } else {
            listed.join(", ")
        }
    );
}
