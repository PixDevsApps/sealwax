//! `sealwax --self-test <script>`: runs the real app (window and all) in a scratch directory,
//! executes a `.sealwax-test` script against it and prints a TAP report; exits non-zero when a
//! step fails. Ported from Stet. It refuses to run on a live display unless
//! `SEALWAX_SELFTEST_ALLOW_LIVE=1`: run it through `tools/headless.sh`. The format and the
//! commands are in docs/m1/shell.md and docs/m1/polish.md (and TESTING.md once merged). The
//! theme follower reads a scratch Omarchy state, never the desktop's: the built-in palette
//! until a script installs a theme from `tests/theme/` (`theme`).

mod a11y;
mod background;
mod bodies;
mod broadway;
mod composer;
mod conversation;
mod files;
mod folders;
mod mail;
mod notify;
mod outbox;
mod perf;
mod probe;
mod processes;
mod reader;
mod reply;
mod rules;
mod sample;
mod screenshot;
mod script;
mod search;
mod setup;
mod theme;
mod triage;

use crate::application::{APP_ID_ENV, AppConfig, AppState, SealwaxApplication};
use crate::config::APP_ID;
use crate::window::{self, Pane, SidebarModel, Window};
use gtk4 as gtk;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use processes::TestBus;
use script::{Step, boolean};
use sealwax_infrastructure::omarchy::STATE_DIR_ENV;
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const ALLOW_LIVE_ENV: &str = "SEALWAX_SELFTEST_ALLOW_LIVE";
const WAIT_TIMEOUT: Duration = Duration::from_secs(15);
/// A step that runs longer has hung: it fails and the rest of the script is not run.
/// `SEALWAX_SELFTEST_STEP_TIMEOUT` overrides it, in seconds.
const STEP_TIMEOUT: Duration = Duration::from_secs(120);
const STEP_TIMEOUT_ENV: &str = "SEALWAX_SELFTEST_STEP_TIMEOUT";
/// Pixels a measured size may differ from the wanted one.
const TOLERANCE: f32 = 1.0;

pub fn run(script_path: &Path, started: Instant) -> glib::ExitCode {
    if std::env::var_os("GTK_A11Y").is_some_and(|backend| backend == "none") {
        // SAFETY: nothing else runs yet; the first thread (the Broadway client) starts below.
        // GTK's "none" backend drops accessible labels, its "test" backend keeps them for
        // `assert-labelled`, and neither talks to D-Bus.
        unsafe { std::env::set_var("GTK_A11Y", "test") };
    }
    let broadway = std::env::var_os("GDK_BACKEND").is_some_and(|backend| backend == "broadway");
    let allowed = std::env::var_os(ALLOW_LIVE_ENV).is_some_and(|value| value == "1");
    if !broadway && !allowed {
        eprintln!(
            "sealwax: --self-test opens windows; run it headless with \
             tools/headless.sh <display> sealwax --self-test <script> (or set {ALLOW_LIVE_ENV}=1)"
        );
        return glib::ExitCode::from(2);
    }
    let text = match fs::read_to_string(script_path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("sealwax: cannot read {}: {error}", script_path.display());
            return glib::ExitCode::from(2);
        }
    };
    let script_dir = fs::canonicalize(script_path)
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    theme::set_fixtures(&script_dir);
    let root = match TempRoot::create() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("sealwax: cannot create a scratch directory: {error}");
            return glib::ExitCode::from(2);
        }
    };
    // The configuration home exists, as `~/.config` does, so the settings folder's watch sees
    // `sealwax/` appear when a script writes `keys.toml`.
    let omarchy = theme::state_dir(&root.path);
    for dir in [omarchy.clone(), root.path.join("config")] {
        if let Err(error) = fs::create_dir_all(&dir) {
            eprintln!("sealwax: cannot create {}: {error}", dir.display());
            return glib::ExitCode::from(2);
        }
    }
    // SAFETY: still one thread; the Broadway client starts below. Spawned instances inherit
    // the scratch state too.
    unsafe { std::env::set_var(STATE_DIR_ENV, &omarchy) };
    let tmp = root.path.to_string_lossy().into_owned();
    let dir = script_dir.to_string_lossy().into_owned();
    let steps = match script::parse(&text, &[("TMP", &tmp), ("DIR", &dir)]) {
        Ok(steps) => steps,
        Err(error) => {
            eprintln!("sealwax: {}: {error}", script_path.display());
            return glib::ExitCode::from(2);
        }
    };
    if let Err(error) = reply::before_start(&steps, &root.path) {
        eprintln!("sealwax: {}: {error}", script_path.display());
        return glib::ExitCode::from(2);
    }
    let tiling = match steps.first() {
        Some(step) if step.command == "desktop" => match step.args[0].as_str() {
            "tiling" => true,
            "other" => false,
            other => {
                eprintln!("sealwax: desktop takes tiling or other, not {other}");
                return glib::ExitCode::from(2);
            }
        },
        _ => window::tiling_desktop(),
    };
    if broadway && let Err(error) = broadway::connect() {
        eprintln!("sealwax: cannot reach the Broadway daemon: {error}");
        return glib::ExitCode::from(2);
    }

    let app_id = std::env::var(APP_ID_ENV)
        .ok()
        .filter(|id| gio::Application::id_is_valid(id))
        .unwrap_or_else(|| format!("{APP_ID}.SelfTest"));
    let mut dirs = SealwaxDirs::under(&root.path);
    if let Err(error) = mail::before_start(&steps, &mut dirs) {
        eprintln!("sealwax: {}: {error}", script_path.display());
        return glib::ExitCode::from(2);
    }
    let config = AppConfig {
        app_id: app_id.clone(),
        dirs,
        exit_after: None,
        tiling,
        engine: None,
        microsoft_client_id: None,
    };
    crate::accessibility::record_announcements();
    let app = SealwaxApplication::new(config, started);
    let outcome: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
    let harness_root = root.path.clone();
    app.state().connect_first_window(glib::clone!(
        #[strong]
        outcome,
        #[weak]
        app,
        move |_| {
            glib::spawn_future_local(async move {
                // Screenshots and page checks right after a navigation see its end state.
                if let Some(settings) = gtk::Settings::default() {
                    settings.set_gtk_enable_animations(false);
                }
                let harness = Harness {
                    state: app.state(),
                    app,
                    root: harness_root,
                    app_id,
                    processes: RefCell::new(HashMap::new()),
                    bus: RefCell::new(None),
                };
                let passed = harness.run(&steps).await;
                harness.stop_processes().await;
                outcome.set(Some(passed));
                harness.end(steps.last());
            });
        }
    ));
    app.run_with_args(&["sealwax"]);
    drop(root);
    match outcome.get() {
        Some(true) => glib::ExitCode::SUCCESS,
        Some(false) => glib::ExitCode::FAILURE,
        None => {
            eprintln!("sealwax: the self-test never finished");
            glib::ExitCode::FAILURE
        }
    }
}

/// A scratch directory, removed when dropped.
struct TempRoot {
    path: PathBuf,
    /// Given by [`ROOT_ENV`]: kept after the run, for a second run on the same folders.
    kept: bool,
}

/// A scratch directory to use, and keep, instead of a fresh one: the kill -9 harness
/// (`tools/kill9-compose.sh`) runs Sealwax twice on the same store.
pub const ROOT_ENV: &str = "SEALWAX_SELFTEST_ROOT";

impl TempRoot {
    fn create() -> std::io::Result<Self> {
        if let Some(path) = std::env::var_os(ROOT_ENV) {
            fs::create_dir_all(&path)?;
            return Ok(Self {
                path: fs::canonicalize(&path)?,
                kept: true,
            });
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let path =
            std::env::temp_dir().join(format!("sealwax-selftest-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self {
            path: fs::canonicalize(&path)?,
            kept: false,
        })
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        if !self.kept {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct Harness {
    app: SealwaxApplication,
    state: Rc<AppState>,
    root: PathBuf,
    app_id: String,
    /// Background `sealwax` processes, by name.
    processes: RefCell<HashMap<String, Child>>,
    bus: RefCell<Option<TestBus>>,
}

type StepResult = Result<(), String>;

impl Harness {
    async fn run(&self, steps: &[Step]) -> bool {
        println!("TAP version 13");
        let limit = step_timeout();
        let mut failed = 0;
        let mut timed_out = None;
        for (index, step) in steps.iter().enumerate() {
            let number = index + 1;
            if let Some(hung) = timed_out {
                failed += 1;
                println!("not ok {number} - {}", step.source);
                println!("  # not run: step {hung} timed out");
                continue;
            }
            let result = with_timeout(self.step(step), limit).await;
            self.settle().await;
            match result {
                Some(Ok(())) => println!("ok {number} - {}", step.source),
                Some(Err(message)) => {
                    failed += 1;
                    println!("not ok {number} - {}", step.source);
                    for line in message.lines() {
                        println!("  # {line}");
                    }
                }
                None => {
                    failed += 1;
                    timed_out = Some(number);
                    println!("not ok {number} - {}", step.source);
                    println!("  # timed out after {} s", limit.as_secs());
                }
            }
        }
        println!("1..{}", steps.len());
        println!("# {} of {} steps passed", steps.len() - failed, steps.len());
        failed == 0
    }

    fn window(&self) -> Result<Window, String> {
        self.state
            .window()
            .ok_or_else(|| "there is no window".to_owned())
    }

    /// A named widget of the window, or of the setup assistant over it.
    fn widget(&self, name: &str) -> Result<gtk::Widget, String> {
        self.window()?
            .widget(name)
            .or_else(|| crate::setup::current().and_then(|open| open.widget(name)))
            .ok_or_else(|| format!("no widget named {name}"))
    }

    async fn step(&self, step: &Step) -> StepResult {
        let args = &step.args;
        let arg = |index: usize| args[index].as_str();
        match step.command.as_str() {
            // Applied before the app started (see `run`).
            "desktop" => Ok(()),
            "resize" => self.resize(number(arg(0))?, number(arg(1))?).await,
            "action" => {
                self.activate(arg(0))?;
                self.wait_until_settled().await;
                Ok(())
            }
            "color-scheme" => {
                let scheme = match arg(0) {
                    "light" => adw::ColorScheme::ForceLight,
                    "dark" => adw::ColorScheme::ForceDark,
                    "default" => adw::ColorScheme::Default,
                    other => return Err(format!("use light, dark or default, not {other}")),
                };
                adw::StyleManager::default().set_color_scheme(scheme);
                Ok(())
            }
            "theme" => self.switch_theme(arg(0)).await,
            "corners" => self.set_corners(arg(0)),
            "write-file" => self.write_file(arg(0), arg(1)),
            "sidebar" => {
                let model = match arg(0) {
                    "sample" => sample::model(),
                    "none" => SidebarModel::default(),
                    other => return Err(format!("use sample or none, not {other}")),
                };
                self.window()?.set_sidebar(model);
                Ok(())
            }
            "list" => {
                let list = match arg(0) {
                    "sample" => sample::list(),
                    "empty" => {
                        window::empty::folder("No messages", "Messages in this folder appear here.")
                            .widget()
                            .clone()
                    }
                    other => return Err(format!("use sample or empty, not {other}")),
                };
                self.window()?.set_list(&list);
                Ok(())
            }
            "reader" => {
                let window = self.window()?;
                match arg(0) {
                    "sample" => {
                        window.set_reader(&sample::reader(&window));
                        window.set_reader_toolbar(&sample::reader_toolbar(&window));
                    }
                    "none" => {
                        window.clear_reader();
                        window.set_reader_toolbar(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
                    }
                    other => return Err(format!("use sample or none, not {other}")),
                }
                Ok(())
            }
            "background" => {
                self.state.set_background(&self.app, boolean(arg(0))?);
                Ok(())
            }
            "close-window" => {
                self.window()?.window().close();
                self.wait_until_settled().await;
                Ok(())
            }
            "wait-frames" => self.wait_frames(number(arg(0))?).await,
            "sleep" => {
                glib::timeout_future(Duration::from_millis(number(arg(0))? as u64)).await;
                Ok(())
            }
            "screenshot" => {
                let path = Path::new(arg(0));
                match args.get(1) {
                    Some(name) => screenshot::capture(&self.widget(name)?, path)?,
                    None => screenshot::capture(self.window()?.window(), path)?,
                }
                println!("  # {}", normalise(path).display());
                Ok(())
            }
            "dump-layout" => {
                let window = self.window()?;
                dump_widget(
                    window.window().upcast_ref(),
                    window.window().upcast_ref(),
                    0,
                );
                Ok(())
            }
            "wait-until" => self.wait_for(WAIT_TIMEOUT, &args[0], &args[1..]).await,
            "wait-within" => {
                let limit = arg(0)
                    .parse::<u64>()
                    .map_err(|_| format!("expected milliseconds, got {}", arg(0)))?;
                self.wait_for(Duration::from_millis(limit), &args[1], &args[2..])
                    .await
            }
            // Ends the app once every step has run (see `end`).
            "quit" => match args.as_slice() {
                [] => Ok(()),
                [how] if how == "window" => Ok(()),
                [how, signal] if how == "signal" => processes::signal_number(signal).map(|_| ()),
                _ => Err("use quit, quit window or quit signal TERM|HUP|INT".to_owned()),
            },
            "spawn" => self.spawn(args, 0).await,
            "spawn-expect" => {
                let status = arg(0)
                    .parse::<i32>()
                    .map_err(|_| format!("expected an exit status, got {}", arg(0)))?;
                self.spawn(&args[1..], status).await
            }
            "spawn-bg" => self.spawn_bg(arg(0), &args[1..]),
            "signal" => self.signal_process(arg(0), arg(1)),
            "wait-process" => self.wait_process(arg(0), arg(1)).await,
            "bus-open" => self.bus_call(Some(arg(0))).await,
            "bus-activate" => self.bus_call(None).await,
            "test-bus-start" => self.start_test_bus().await,
            "test-bus-spawn" => self.test_bus_spawn(arg(0), &args[1..]),
            "test-bus-open" => self.test_bus_call(Some(arg(0))).await,
            "test-bus-activate" => self.test_bus_call(None).await,
            "test-bus-signal" => self.test_bus_signal(arg(0)).await,
            "test-bus-stop" => self.stop_test_bus().await,
            command if a11y::handles(command) => a11y::step(self, command, args).await,
            command if folders::handles(command) => folders::step(self, command, args).await,
            command if rules::handles(command) => rules::step(self, command, args).await,
            command if conversation::handles(command) => {
                conversation::step(self, command, args).await
            }
            command if reader::handles(command) => reader::step(self, command, args).await,
            command if reply::handles(command) => reply::step(self, command, args).await,
            command if outbox::handles(command) => outbox::step(self, command, args).await,
            command if notify::handles(command) => notify::step(self, command, args).await,
            command if search::handles(command) => search::step(self, command, args).await,
            command if background::handles(command) => background::step(self, command, args).await,
            command if bodies::handles(command) => bodies::step(self, command, args).await,
            command if setup::handles(command) => setup::step(self, command, args).await,
            command if triage::handles(command) => triage::step(self, command, args).await,
            command if files::handles(command) => files::step(self, command, args).await,
            command if composer::handles(command) => composer::step(self, command, args).await,
            command => self.check(command, args).await,
        }
    }

    /// Ends the app once the steps ran. A last `quit` step quits as the user would (the app's
    /// quit path), `quit window` closes the window, `quit signal TERM` sends this process that
    /// signal; otherwise the app just stops.
    fn end(&self, last: Option<&Step>) {
        let quit = last.filter(|step| step.command == "quit");
        match quit.map(|step| step.args.as_slice()) {
            Some([how, signal]) if how == "signal" => {
                let _ = Command::new("kill")
                    .arg(format!("-{signal}"))
                    .arg(std::process::id().to_string())
                    .status();
            }
            Some([how]) if how == "window" => match self.state.window() {
                Some(window) => window.window().close(),
                None => self.app.quit(),
            },
            Some(_) => self.state.quit(&self.app),
            None => self.app.quit(),
        }
    }

    /// Activates a gio action by its detailed name (`nav.pane::reader`, `app.quit`), as a
    /// button or the palette would; window and app actions both resolve from the window.
    fn activate(&self, detailed: &str) -> StepResult {
        let (name, target) =
            gio::Action::parse_detailed_name(detailed).map_err(|error| error.to_string())?;
        match self.state.window() {
            Some(window) => WidgetExt::activate_action(window.window(), &name, target.as_ref())
                .map_err(|_| format!("no action {name}")),
            None => {
                let name = name
                    .strip_prefix("app.")
                    .ok_or_else(|| format!("no window for {name}"))?;
                if self.app.lookup_action(name).is_none() {
                    return Err(format!("no action app.{name}"));
                }
                self.app.activate_action(name, target.as_ref());
                Ok(())
            }
        }
    }

    /// Resizes the window as a user would, and waits until it has the new size.
    async fn resize(&self, width: usize, height: usize) -> StepResult {
        let window = self.window()?;
        let (width, height) = (width as i32, height as i32);
        window.window().set_default_size(width, height);
        let started = Instant::now();
        loop {
            let size = (window.window().width(), window.window().height());
            if size == (width, height) {
                return self.wait_frames(1).await;
            }
            if started.elapsed() > WAIT_TIMEOUT {
                return Err(format!(
                    "the window is {}×{}, not {width}×{height}",
                    size.0, size.1
                ));
            }
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    }

    /// Lets GTK lay out and paint what the last step changed, so the next one sees it: steps
    /// that don't wait would otherwise run back to back without the main loop turning.
    async fn settle(&self) {
        // A mail action's store request answers on the main loop.
        let started = Instant::now();
        while self
            .state
            .triage()
            .is_some_and(|triage| triage.in_flight() > 0)
            && started.elapsed() < WAIT_TIMEOUT
        {
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        self.wait_until_settled().await;
        if self
            .state
            .window()
            .is_some_and(|window| window.window().is_mapped())
        {
            let _ = self.wait_frames(1).await;
        }
    }

    /// Lets idle handlers at default priority run once.
    /// Polls the assertion `check` until it passes, failing once `limit` has passed.
    async fn wait_for(&self, limit: Duration, check: &str, args: &[String]) -> Result<(), String> {
        let started = Instant::now();
        loop {
            match self.check(check, args).await {
                Ok(()) => return Ok(()),
                Err(error) if started.elapsed() > limit => {
                    return Err(format!("still failing after {limit:?}: {error}"));
                }
                Err(_) => glib::timeout_future(Duration::from_millis(20)).await,
            }
        }
    }

    async fn wait_until_settled(&self) {
        let (sender, receiver) = async_channel::bounded(1);
        glib::idle_add_local_full(glib::Priority::LOW, move || {
            let _ = sender.try_send(());
            glib::ControlFlow::Break
        });
        let _ = receiver.recv().await;
    }

    async fn wait_frames(&self, frames: usize) -> StepResult {
        let window = self.window()?;
        let window = window.window();
        let clock = window
            .frame_clock()
            .ok_or_else(|| "the window has no frame clock".to_owned())?;
        let (sender, receiver) = async_channel::unbounded();
        let handler = clock.connect_after_paint(move |_| {
            let _ = sender.try_send(());
        });
        clock.begin_updating();
        window.queue_draw();
        let mut result = Ok(());
        for _ in 0..frames {
            if with_timeout(receiver.recv(), WAIT_TIMEOUT).await.is_none() {
                result = Err("no frame was painted".to_owned());
                break;
            }
        }
        clock.end_updating();
        clock.disconnect(handler);
        result
    }

    async fn check(&self, assertion: &str, args: &[String]) -> StepResult {
        if let Some(result) = mail::run(self, assertion, args).await {
            return result;
        }
        if assertion == "assert-bus-owner" {
            return self.check_bus_owner(boolean(&args[0])?).await;
        }
        let arg = |index: usize| args[index].as_str();
        match assertion {
            "assert-layout" => equal("layout", self.window()?.layout().name(), arg(0)),
            "assert-pane" => {
                let wanted = Pane::from_name(arg(0)).ok_or("use folders, list or reader")?;
                equal("pane", self.window()?.pane(), wanted)
            }
            "assert-folder" => {
                let window = self.window()?;
                equal("folder", window.selected_folder().as_str(), arg(0))?;
                equal(
                    "folder marked in the sidebar",
                    window.sidebar().marked_folder().as_deref(),
                    Some(arg(0)),
                )
            }
            "assert-shown" => {
                let shown = match self.widget(arg(0)) {
                    Ok(widget) => self.shown(&widget)?,
                    Err(_) => false,
                };
                equal(&format!("{} shown", arg(0)), shown, boolean(arg(1))?)
            }
            "assert-width" => {
                let width = self.bounds(arg(0))?.width();
                let wanted = number(arg(1))? as f32;
                if (width - wanted).abs() <= TOLERANCE {
                    Ok(())
                } else {
                    Err(format!("{} is {width} px wide, not {wanted}", arg(0)))
                }
            }
            "assert-height" => {
                let height = self.bounds(arg(0))?.height();
                let wanted = number(arg(1))? as f32;
                if (height - wanted).abs() <= TOLERANCE {
                    Ok(())
                } else {
                    Err(format!("{} is {height} px high, not {wanted}", arg(0)))
                }
            }
            "assert-tooltip" => {
                let tooltip = self.widget(arg(0))?.tooltip_text().unwrap_or_default();
                equal(&format!("{}'s tooltip", arg(0)), tooltip.as_str(), arg(1))
            }
            "assert-color" | "assert-contrast" | "assert-radius" | "assert-font" => {
                self.check_look(assertion, args)
            }
            "assert-max-width" => {
                let width = self.bounds(arg(0))?.width();
                let limit = number(arg(1))? as f32;
                println!("  # {} is {width} px wide", arg(0));
                if width <= limit + TOLERANCE {
                    Ok(())
                } else {
                    Err(format!("{} is {width} px wide, over {limit}", arg(0)))
                }
            }
            "assert-centred" => self.check_centred(arg(0)),
            "assert-right-of" => {
                let (right, left) = (self.bounds(arg(0))?, self.bounds(arg(1))?);
                let gap = right.x() - (left.x() + left.width());
                println!("  # {} starts {gap} px right of {}", arg(0), arg(1));
                if gap >= 0.0 {
                    Ok(())
                } else {
                    Err(format!("{} overlaps {} by {} px", arg(0), arg(1), -gap))
                }
            }
            "assert-sensitive" => {
                let widget = self.widget(arg(0))?;
                equal(
                    &format!("{} sensitive", arg(0)),
                    widget.is_sensitive(),
                    boolean(arg(1))?,
                )
            }
            "assert-label" => {
                let widget = self.widget(arg(0))?;
                let text = if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    label.label().to_string()
                } else if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                    button
                        .label()
                        .map(|label| label.to_string())
                        .unwrap_or_default()
                } else {
                    return Err(format!("{} has no text", arg(0)));
                };
                equal(&format!("{} text", arg(0)), text.as_str(), arg(1))
            }
            "assert-window" => equal("a window", self.state.window().is_some(), boolean(arg(0))?),
            "assert-window-controls" => {
                let window = self.window()?;
                let mut controls = Vec::new();
                collect(window.window().upcast_ref(), &mut |widget| {
                    if widget.is::<gtk::WindowControls>() && widget.is_mapped() {
                        controls.push(widget.clone());
                    }
                });
                equal("window controls on screen", controls.len(), number(arg(0))?)
            }
            "assert-labelled" => {
                let problems = unlabelled(self.window()?.window().upcast_ref());
                if problems.is_empty() {
                    Ok(())
                } else {
                    Err(problems.join("\n"))
                }
            }
            "assert-folder-shown" => {
                // A folder's key, or a store folder as `ACCOUNT:ROLE`.
                let wanted = if arg(0).contains(':') {
                    mail::folder_key_for(self, arg(0))?
                } else {
                    arg(0).to_owned()
                };
                let shown = self.window()?.sidebar().visible_keys().contains(&wanted);
                equal(
                    &format!("{} in the sidebar", arg(0)),
                    shown,
                    boolean(arg(1))?,
                )
            }
            "assert-more" => equal(
                &format!("{}'s More open", arg(0)),
                self.window()?.sidebar().is_expanded(arg(0)),
                boolean(arg(1))?,
            ),
            "assert-compose-count" => equal(
                "compose requests",
                self.state.compose_requests().len(),
                number(arg(0))?,
            ),
            "assert-compose" => {
                let requests = self.state.compose_requests();
                let index = number(arg(0))?;
                let draft = requests
                    .get(index.wrapping_sub(1))
                    .ok_or_else(|| format!("there is no compose request {index}"))?;
                let value = match arg(1) {
                    "to" => draft.to.join(","),
                    "cc" => draft.cc.join(","),
                    "bcc" => draft.bcc.join(","),
                    "subject" => draft.subject.clone().unwrap_or_default(),
                    "body" => draft.body.clone().unwrap_or_default(),
                    "in-reply-to" => draft.in_reply_to.clone().unwrap_or_default(),
                    "ignored" => draft.ignored.join(","),
                    other => return Err(format!("no field {other}")),
                };
                equal(arg(1), value.as_str(), arg(2))
            }
            "assert-activations" => equal(
                "activations",
                self.state.activations() as usize,
                number(arg(0))?,
            ),
            "assert-background" => equal(
                "background mode",
                self.state.is_background(),
                boolean(arg(0))?,
            ),
            "assert-running" => equal(
                &format!("{} running", arg(0)),
                self.running(arg(0))?,
                boolean(arg(1))?,
            ),
            "assert-file-contains" | "assert-file-lacks" => {
                let text =
                    fs::read_to_string(arg(0)).map_err(|error| format!("{}: {error}", arg(0)))?;
                let wanted = assertion == "assert-file-contains";
                if text.contains(arg(1)) == wanted {
                    Ok(())
                } else if wanted {
                    Err(format!("{} lacks {:?}", arg(0), arg(1)))
                } else {
                    Err(format!("{} has {:?}", arg(0), arg(1)))
                }
            }
            "assert-reader" => reader::check(self, args).await,
            "assert-web-sandbox" => blocking(processes::web_sandbox).await?,
            assertion if a11y::handles(assertion) => a11y::step(self, assertion, args).await,
            assertion if folders::handles(assertion) => folders::step(self, assertion, args).await,
            assertion if rules::handles(assertion) => rules::step(self, assertion, args).await,
            assertion if conversation::handles(assertion) => {
                conversation::step(self, assertion, args).await
            }
            assertion if bodies::handles(assertion) => bodies::step(self, assertion, args).await,
            assertion if setup::checks(assertion) => setup::check(self, assertion, args),
            assertion if triage::handles(assertion) => triage::step(self, assertion, args).await,
            assertion if files::handles(assertion) => files::step(self, assertion, args).await,
            assertion if reply::handles(assertion) => reply::step(self, assertion, args).await,
            assertion if outbox::handles(assertion) => outbox::step(self, assertion, args).await,
            assertion if notify::handles(assertion) => notify::step(self, assertion, args).await,
            assertion if search::handles(assertion) => search::step(self, assertion, args).await,
            assertion if background::handles(assertion) => {
                background::step(self, assertion, args).await
            }
            assertion if composer::handles(assertion) => {
                composer::step(self, assertion, args).await
            }
            other => Err(format!("unknown command {other}")),
        }
    }

    /// On screen: mapped, with a size, inside the window.
    fn shown(&self, widget: &gtk::Widget) -> Result<bool, String> {
        if !widget.is_mapped() || widget.width() <= 0 || widget.height() <= 0 {
            return Ok(false);
        }
        let window = self.window()?;
        let window = window.window();
        let Some(bounds) = widget.compute_bounds(window) else {
            return Ok(false);
        };
        Ok(bounds.x() >= -TOLERANCE
            && bounds.y() >= -TOLERANCE
            && bounds.x() + bounds.width() <= window.width() as f32 + TOLERANCE
            && bounds.y() + bounds.height() <= window.height() as f32 + TOLERANCE)
    }

    fn bounds(&self, name: &str) -> Result<gtk::graphene::Rect, String> {
        let window = self.window()?;
        let widget = self.widget(name)?;
        if !self.shown(&widget)? {
            return Err(format!("{name} is not on screen"));
        }
        widget
            .compute_bounds(window.window())
            .ok_or_else(|| format!("{name} has no bounds"))
    }

    /// `write-file PATH TEXT`: a file in the run's scratch directory, such as `keys.toml`.
    fn write_file(&self, path: &str, text: &str) -> StepResult {
        let path = normalise(Path::new(path));
        if !path.starts_with(&self.root) {
            return Err(format!(
                "{} is outside the run's scratch directory",
                path.display()
            ));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// The widget sits in the middle of its pane: as much room on its left as on its right.
    fn check_centred(&self, name: &str) -> StepResult {
        let widget = self.widget(name)?;
        let pane = widget
            .ancestor(adw::ToolbarView::static_type())
            .ok_or_else(|| format!("{name} is not in a pane"))?;
        let bounds = widget
            .compute_bounds(&pane)
            .ok_or_else(|| format!("{name} has no bounds"))?;
        let left = bounds.x();
        let right = pane.width() as f32 - bounds.x() - bounds.width();
        println!(
            "  # {name}: {} px wide, {left} px from the pane's left edge, {right} px from its right",
            bounds.width()
        );
        if (left - right).abs() <= 2.0 * TOLERANCE {
            Ok(())
        } else {
            Err(format!(
                "{name} is off centre: {left} px left, {right} px right"
            ))
        }
    }
}

fn step_timeout() -> Duration {
    std::env::var(STEP_TIMEOUT_ENV)
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .map_or(STEP_TIMEOUT, Duration::from_secs)
}

/// Runs `future` until it finishes or `limit` passes; `None` when it timed out.
async fn with_timeout<T>(
    future: impl std::future::Future<Output = T>,
    limit: Duration,
) -> Option<T> {
    let mut future = std::pin::pin!(future);
    let mut timeout = std::pin::pin!(glib::timeout_future(limit));
    std::future::poll_fn(|context| {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(context) {
            return std::task::Poll::Ready(Some(value));
        }
        if timeout.as_mut().poll(context).is_ready() {
            return std::task::Poll::Ready(None);
        }
        std::task::Poll::Pending
    })
    .await
}

/// Runs `job` on a thread of its own and resolves on the GTK thread with its result.
async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("sealwax-selftest".into())
        .spawn(move || {
            let _ = sender.send_blocking(job());
        })
        .map_err(|error| error.to_string())?;
    receiver
        .recv()
        .await
        .map_err(|_| "the worker thread failed".to_owned())
}

/// Calls `visit` for `widget` and everything under it.
fn collect(widget: &gtk::Widget, visit: &mut impl FnMut(&gtk::Widget)) {
    visit(widget);
    let mut child = widget.first_child();
    while let Some(current) = child {
        collect(&current, visit);
        child = current.next_sibling();
    }
}

/// Controls that a screen reader can't name, and icon-only buttons without a tooltip
/// (DESIGN.md "Accessibility"). GTK's own parts of a menu button and of the window controls are
/// left to GTK, and a toast's to libadwaita. Only controls on screen count: libadwaita keeps
/// unused buttons hidden (an alert dialog's other layout, a toast without an action).
fn unlabelled(root: &gtk::Widget) -> Vec<String> {
    let mut problems = Vec::new();
    let toast = glib::Type::from_name("AdwToastWidget");
    collect(root, &mut |widget| {
        if !widget.is_mapped()
            || widget.ancestor(gtk::MenuButton::static_type()).is_some()
            || widget
                .ancestor(gtk::WindowControls::static_type())
                .is_some()
            || toast.is_some_and(|toast| widget.ancestor(toast).is_some())
        {
            return;
        }
        let row = widget
            .downcast_ref::<gtk::ListBoxRow>()
            .is_some_and(|row| row.is_activatable());
        let control = widget.is::<gtk::Button>() || widget.is::<gtk::MenuButton>() || row;
        if !control {
            return;
        }
        let mut text = false;
        collect(widget, &mut |inner| {
            if let Some(label) = inner.downcast_ref::<gtk::Label>() {
                text |= label.get_visible() && !label.label().trim().is_empty();
            }
        });
        let accessible = gtk::test_accessible_has_property(widget, gtk::AccessibleProperty::Label);
        let what = format!(
            "{} {} [{}]",
            widget.type_().name(),
            widget.widget_name(),
            widget.css_classes().join(" ")
        );
        if !text && !accessible {
            problems.push(format!("{what} has no accessible label"));
        }
        if !text && !row && widget.tooltip_text().is_none() {
            problems.push(format!("{what} shows only an icon and has no tooltip"));
        }
    });
    problems
}

/// Prints the visible widget tree with window coordinates, for diagnosing layouts.
fn dump_widget(widget: &gtk::Widget, root: &gtk::Widget, depth: usize) {
    if !widget.is_visible() {
        return;
    }
    let bounds = widget.compute_bounds(root);
    let (x, y, width, height) = bounds.map_or((0.0, 0.0, 0.0, 0.0), |b| {
        (b.x(), b.y(), b.width(), b.height())
    });
    println!(
        "  # {:indent$}{} #{} [{}] {x:.0},{y:.0} {width:.0}×{height:.0} {}{}",
        "",
        widget.type_().name(),
        widget.widget_name(),
        widget.css_classes().join(" "),
        widget.color(),
        if widget.is_mapped() {
            ""
        } else {
            " (unmapped)"
        },
        indent = depth * 2
    );
    let mut child = widget.first_child();
    while let Some(current) = child {
        dump_widget(&current, root, depth + 1);
        child = current.next_sibling();
    }
}

/// `path` without `..` steps, for the report.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn number(word: &str) -> Result<usize, String> {
    word.parse()
        .map_err(|_| format!("expected a number, got {word}"))
}

fn equal<T: PartialEq + std::fmt::Debug>(what: &str, actual: T, wanted: T) -> StepResult {
    if actual == wanted {
        Ok(())
    } else {
        Err(format!("{what}: got {actual:?}, wanted {wanted:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_lose_parent_steps() {
        assert_eq!(
            normalise(Path::new(
                "/repo/tests/selftest/../../.local/selftest/a.png"
            )),
            Path::new("/repo/.local/selftest/a.png")
        );
    }

    #[test]
    fn layout_names() {
        use crate::window::Layout;
        assert_eq!(Layout::Rail.name(), "rail");
        assert_eq!(Pane::from_name("reader"), Some(Pane::Reader));
        assert_eq!(Pane::from_name("other"), None);
    }
}
