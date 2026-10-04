//! The single-instance application: an `adw::Application` subclass with
//! `HANDLES_COMMAND_LINE | HANDLES_OPEN | SEND_ENVIRONMENT`. A second process forwards its
//! command line (`mailto:` URIs) to the first and exits; D-Bus `Open` calls arrive at `open`.
//! The primary stamps every startup step with `CLOCK_MONOTONIC` and reports them as events.

use crate::config::{Config, Mode};
use crate::desktop::{self, DesktopEvent};
use crate::engine::{self, Command, Engine, EngineEvent};
use crate::notify::{self, BusEvent};
use crate::probe::{self, Painted};
use crate::store::{self, Opened, ReadReply, ReadRequest};
use crate::theme;
use crate::window::MainWindow;
use email_spike_s3::store::ListRow;
use email_spike_s6::clock::{mono_ns, real_ns};
use email_spike_s6::events::emit;
use email_spike_s6::{APP_ID, env, mailto};
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;
use serde_json::{Map, Value, json};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct S6App {
        pub state: OnceCell<Rc<State>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for S6App {
        const NAME: &'static str = "S6App";
        type Type = super::S6App;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for S6App {}

    impl ApplicationImpl for S6App {
        fn dbus_register(
            &self,
            connection: &gio::DBusConnection,
            object_path: &str,
        ) -> Result<(), glib::Error> {
            if let Some(state) = self.state.get() {
                state.stamp("dbus-register");
            }
            self.parent_dbus_register(connection, object_path)
        }

        fn startup(&self) {
            let state = self
                .state
                .get()
                .expect("state is set at construction")
                .clone();
            state.startup(&self.obj(), || self.parent_startup());
        }

        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            let state = self.state.get().expect("state").clone();
            state.command_line(&self.obj(), command_line)
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            let state = self.state.get().expect("state").clone();
            state.open(&self.obj(), files);
        }

        fn activate(&self) {
            let state = self.state.get().expect("state").clone();
            state.show(&self.obj());
        }

        fn before_emit(&self, platform_data: &glib::Variant) {
            if let Some(state) = self.state.get() {
                state.platform_data(platform_data);
            }
            self.parent_before_emit(platform_data);
        }

        fn after_emit(&self, platform_data: &glib::Variant) {
            if let Some(state) = self.state.get() {
                state.platform.replace(None);
            }
            self.parent_after_emit(platform_data);
        }
    }

    impl GtkApplicationImpl for S6App {}
    impl AdwApplicationImpl for S6App {}
}

glib::wrapper! {
    pub struct S6App(ObjectSubclass<imp::S6App>)
        @extends adw::Application, gtk4::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl S6App {
    pub fn new(config: Config, t_main: i64, exec_ns: Option<i64>) -> Self {
        let flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE
            | gio::ApplicationFlags::HANDLES_OPEN
            | gio::ApplicationFlags::SEND_ENVIRONMENT;
        let app: Self = glib::Object::builder()
            .property("application-id", APP_ID)
            .property("flags", flags)
            .build();
        let state = Rc::new(State::new(config, exec_ns));
        state.stamps.borrow_mut().insert("main", t_main);
        let _ = app.imp().state.set(state);
        app
    }

    pub fn state(&self) -> Rc<State> {
        self.imp().state.get().expect("state").clone()
    }
}

/// A notification this process sent, and what the bus said about it.
#[derive(Clone, Debug, Default)]
struct Sent {
    key: String,
    target: String,
    sent: i64,
    serial: Option<u32>,
    id: Option<u32>,
    accepted: Option<i64>,
    action: Option<(i64, String)>,
    token: Option<i64>,
    closed: Option<(i64, u32)>,
}

#[derive(Clone, Copy, Debug)]
struct Reconnect {
    cause: &'static str,
    since: i64,
    connected: Option<i64>,
    synced: Option<i64>,
}

pub struct State {
    pub config: Config,
    exec_ns: Option<i64>,
    stamps: RefCell<BTreeMap<&'static str, i64>>,
    reads: OnceCell<async_channel::Sender<ReadRequest>>,
    opened: RefCell<Option<Rc<Opened>>>,
    opened_wake: (async_channel::Sender<()>, async_channel::Receiver<()>),
    reader_times: Cell<Option<store::OpenTimes>>,
    window: RefCell<Option<MainWindow>>,
    windows_built: Cell<u32>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    engine: RefCell<Option<Engine>>,
    engine_stopped: (async_channel::Sender<i64>, async_channel::Receiver<i64>),
    reconnect: Cell<Option<Reconnect>>,
    idling_once: Cell<bool>,
    platform: RefCell<Option<glib::Variant>>,
    unmatched: RefCell<VecDeque<String>>,
    sent: RefCell<Vec<Sent>>,
    tokens: RefCell<HashMap<u32, String>>,
    message_waiters: RefCell<HashMap<i64, Vec<async_channel::Sender<Option<ListRow>>>>>,
    shown: (async_channel::Sender<Value>, async_channel::Receiver<Value>),
    active_since: Cell<Option<i64>>,
    desktop: RefCell<Option<desktop::Watch>>,
    last_resume: Cell<Option<i64>>,
    quitting: Cell<bool>,
}

impl State {
    fn new(config: Config, exec_ns: Option<i64>) -> Self {
        Self {
            config,
            exec_ns,
            stamps: RefCell::default(),
            reads: OnceCell::new(),
            opened: RefCell::default(),
            opened_wake: async_channel::unbounded(),
            reader_times: Cell::default(),
            window: RefCell::default(),
            windows_built: Cell::new(0),
            hold: RefCell::default(),
            engine: RefCell::default(),
            engine_stopped: async_channel::bounded(1),
            reconnect: Cell::default(),
            idling_once: Cell::new(false),
            platform: RefCell::default(),
            unmatched: RefCell::default(),
            sent: RefCell::default(),
            tokens: RefCell::default(),
            message_waiters: RefCell::default(),
            shown: async_channel::unbounded(),
            active_since: Cell::default(),
            desktop: RefCell::default(),
            last_resume: Cell::default(),
            quitting: Cell::new(false),
        }
    }

    pub fn stamp(&self, name: &'static str) {
        self.stamps.borrow_mut().entry(name).or_insert_with(mono_ns);
    }

    pub fn stamp_value(&self, name: &'static str) -> Option<i64> {
        self.stamps.borrow().get(name).copied()
    }

    fn stamps_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(exec) = self.exec_ns {
            map.insert("exec".into(), exec.into());
        }
        for (name, value) in self.stamps.borrow().iter() {
            map.insert((*name).into(), (*value).into());
        }
        if let Some(times) = self.reader_times.get() {
            map.insert("reader-thread".into(), times.thread_start.into());
            map.insert("reader-open".into(), times.connection_open.into());
            map.insert("reader-snapshot".into(), times.snapshot_done.into());
        }
        Value::Object(map)
    }

    fn startup(self: &Rc<Self>, app: &S6App, parent: impl FnOnce()) {
        self.stamp("startup");
        if std::env::var_os("MAIL_S6_PREWARM").is_some() {
            crate::prewarm::start();
        }
        let channels = store::start(self.config.db.clone(), self.config.snapshot_rows);
        let _ = self.reads.set(channels.reads);
        if let Err(error) = gtk4::init() {
            eprintln!("s6: gtk init: {error}");
        }
        self.stamp("gtk-init");
        if let Err(error) = adw::init() {
            eprintln!("s6: adw init: {error}");
        }
        self.stamp("adw-lib");
        parent();
        self.stamp("adw-init");
        #[cfg(feature = "webkit")]
        {
            let _ = <webkit6::WebView as glib::prelude::StaticType>::static_type();
            self.stamp("webkit-type");
        }
        let palette = theme::palette();
        self.stamp("theme-read");
        adw::StyleManager::default().set_color_scheme(if palette.dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        });
        let provider = gtk4::CssProvider::new();
        provider.load_from_string(&theme::css(&palette));
        if let Some(display) = gdk::Display::default() {
            gtk4::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        self.stamp("css");
        self.install_actions(app);
        glib::spawn_future_local(self.clone().replies(channels.replies));
        if let Some(connection) = app.dbus_connection() {
            glib::spawn_future_local(self.clone().bus(notify::watch(&connection)));
        }
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let state = self.clone();
            let app = app.clone();
            glib_unix::unix_signal_add_local(signal, move || {
                emit("signal", json!({ "signal": signal }));
                state.quit(&app);
                glib::ControlFlow::Continue
            });
        }
        {
            let state = self.clone();
            let app = app.clone();
            glib::timeout_add_local_once(self.config.exit_after, move || {
                emit("exit-timer", json!({}));
                state.quit(&app);
            });
        }
        if matches!(
            self.config.mode,
            Mode::Background | Mode::ManualNotify | Mode::ManualResume
        ) {
            self.hold.replace(Some(app.hold()));
        }
        if self.config.mode.uses_imap() {
            let (sender, receiver) = async_channel::unbounded();
            let imap = self
                .config
                .imap
                .clone()
                .expect("IMAP modes have IMAP settings");
            match engine::start(&imap, self.config.run_dir.clone(), sender) {
                Ok(engine) => {
                    self.engine.replace(Some(engine));
                    glib::spawn_future_local(self.clone().engine_events(app.clone(), receiver));
                }
                Err(error) => emit("engine-failed", json!({ "error": format!("{error:#}") })),
            }
            let (sender, receiver) = async_channel::unbounded();
            let watch = desktop::watch(sender);
            emit(
                "desktop-watch",
                json!({
                    "network_monitor": watch.monitor_type,
                    "system_bus": watch.system_bus.as_ref().err(),
                }),
            );
            self.desktop.replace(Some(watch));
            glib::spawn_future_local(self.clone().desktop_events(receiver));
        }
    }

    fn install_actions(self: &Rc<Self>, app: &S6App) {
        let string = Some(glib::VariantTy::STRING);
        let open = gio::SimpleAction::new("open-message", string);
        let state = self.clone();
        let target_app = app.clone();
        open.connect_activate(move |_, parameter| {
            let target = parameter.and_then(|p| p.str().map(str::to_owned));
            state
                .clone()
                .open_message(&target_app, target.unwrap_or_default());
        });
        app.add_action(&open);
        for name in ["archive-message", "mark-read"] {
            let action = gio::SimpleAction::new(name, string);
            let state = self.clone();
            action.connect_activate(move |action, parameter| {
                let target = parameter.and_then(|p| p.str().map(str::to_owned));
                let platform = state.platform.take().map(|p| p.print(true).to_string());
                emit(
                    "button-action",
                    json!({ "action": action.name().to_string(), "target": target, "platform": platform }),
                );
            });
            app.add_action(&action);
        }
        let quit = gio::SimpleAction::new("quit", None);
        let state = self.clone();
        let quit_app = app.clone();
        quit.connect_activate(move |_, _| {
            emit("quit-action", json!({}));
            state.quit(&quit_app);
        });
        app.add_action(&quit);
        let resume = gio::SimpleAction::new("simulate-resume", None);
        let state = self.clone();
        resume.connect_activate(move |_, _| {
            emit("simulate-resume", json!({}));
            state.reconnect("simulated-resume");
        });
        app.add_action(&resume);
        let close = gio::SimpleAction::new("close-window", None);
        let state = self.clone();
        close.connect_activate(move |_, _| state.close_window());
        app.add_action(&close);
    }

    fn platform_data(&self, platform_data: &glib::Variant) {
        let dict = glib::VariantDict::new(Some(platform_data));
        let keys: Vec<String> = platform_data
            .iter()
            .filter_map(|entry| entry.child_value(0).str().map(str::to_owned))
            .collect();
        emit(
            "platform-data",
            json!({
                "keys": keys,
                "activation_token": dict.lookup::<String>("activation-token").ok().flatten().map(|token| token.len()),
                "s6_sent": dict.lookup::<i64>("s6-sent-ns").ok().flatten(),
            }),
        );
        self.platform.replace(Some(platform_data.clone()));
    }

    fn command_line(
        self: &Rc<Self>,
        app: &S6App,
        command_line: &gio::ApplicationCommandLine,
    ) -> glib::ExitCode {
        let received = mono_ns();
        let exec_ns = command_line
            .getenv(env::EXEC_NS)
            .and_then(|value| value.as_str().parse::<i64>().ok());
        let remote = command_line.is_remote();
        let arguments: Vec<String> = command_line
            .arguments()
            .iter()
            .skip(1)
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        let mut handled = false;
        for argument in &arguments {
            if mailto::is_mailto(argument) {
                self.mailto(argument, "command-line", exec_ns, received, remote);
                handled = true;
            } else {
                emit("argument-ignored", json!({ "length": argument.len() }));
            }
        }
        if !remote || !handled || self.window.borrow().is_none() {
            self.show(app);
        } else if let Some(window) = self.window.borrow().as_ref() {
            window.window.present();
        }
        glib::ExitCode::SUCCESS
    }

    fn open(self: &Rc<Self>, app: &S6App, files: &[gio::File]) {
        let received = mono_ns();
        let sent = self.platform.borrow().as_ref().and_then(|platform| {
            glib::VariantDict::new(Some(platform))
                .lookup::<i64>("s6-sent-ns")
                .ok()
                .flatten()
        });
        for file in files {
            let uri = file.uri();
            if mailto::is_mailto(&uri) {
                self.mailto(&uri, "dbus-open", sent, received, true);
            }
        }
        if self.window.borrow().is_none() {
            self.show(app);
        }
    }

    fn mailto(&self, uri: &str, source: &str, sent: Option<i64>, received: i64, remote: bool) {
        let parsed = mailto::parse(uri);
        let parsed_at = mono_ns();
        match parsed {
            Ok(draft) => {
                emit(
                    "mailto",
                    json!({
                        "source": source,
                        "remote": remote,
                        "sent": sent,
                        "received": received,
                        "parsed": parsed_at,
                        "to": draft.to.len(),
                        "cc": draft.cc.len(),
                        "bcc": draft.bcc.len(),
                        "subject_chars": draft.subject.as_ref().map(|s| s.chars().count()),
                        "body_chars": draft.body.as_ref().map(|s| s.chars().count()),
                        "ignored": draft.ignored,
                    }),
                );
                if let Some(window) = self.window.borrow().as_ref() {
                    window.show_banner(&format!(
                        "New draft: {} recipient(s), subject “{}”{}",
                        draft.to.len() + draft.cc.len() + draft.bcc.len(),
                        draft.subject.as_deref().unwrap_or(""),
                        if draft.ignored.is_empty() {
                            String::new()
                        } else {
                            format!(" (ignored: {})", draft.ignored.join(", "))
                        }
                    ));
                }
            }
            Err(error) => emit("mailto-error", json!({ "error": error.to_string() })),
        }
    }

    /// The window, created on the first request.
    fn ensure_window(self: &Rc<Self>, app: &S6App) -> MainWindow {
        if let Some(window) = self.window.borrow().as_ref() {
            return window.clone();
        }
        let opened = self.opened.borrow().clone();
        let thresholds = opened
            .as_ref()
            .map(|opened| opened.thresholds)
            .unwrap_or_else(|| {
                email_spike_s3::sections::Thresholds::local_now()
                    .unwrap_or(email_spike_s3::sections::Thresholds::new(0, 0, 0, 0))
            });
        let reads = self.reads.get().expect("reader started").clone();
        let window = MainWindow::new(app.upcast_ref(), reads, thresholds);
        self.windows_built.set(self.windows_built.get() + 1);
        self.stamp("window-built");
        let state = self.clone();
        window.window.connect_close_request(move |_| {
            let background = state.hold.borrow().is_some() && !state.quitting.get();
            emit("window-closed", json!({ "background": background }));
            state.window.replace(None);
            glib::Propagation::Proceed
        });
        let probe = window.probe.clone();
        let state = self.clone();
        window.window.connect_realize(move |window| {
            state.stamp("realize");
            if let Some(clock) = window.frame_clock() {
                probe.attach(&clock);
            }
        });
        let state = self.clone();
        window.window.connect_map(move |_| state.stamp("map"));
        let state = self.clone();
        window.window.connect_is_active_notify(move |window| {
            let active = window.is_active();
            let t = mono_ns();
            if active {
                state.active_since.set(Some(t));
            }
            emit("window-active", json!({ "active": active, "t": t }));
        });
        self.window.replace(Some(window.clone()));
        window
    }

    /// Creates the window right away (GApplication only keeps running while a window or a
    /// hold exists once `command-line` returns) and presents it when its rows are ready.
    fn show(self: &Rc<Self>, app: &S6App) {
        self.ensure_window(app);
        glib::spawn_future_local(self.clone().present(app.clone(), None));
    }

    async fn wait_opened(&self, timeout: Duration) -> Option<Rc<Opened>> {
        if let Some(opened) = self.opened.borrow().clone() {
            return Some(opened);
        }
        let wake = self.opened_wake.1.clone();
        probe::with_timeout(wake.recv(), timeout).await;
        self.opened.borrow().clone()
    }

    /// Shows the window; a new window waits briefly for the first page, so its first frame
    /// already has rows, and reports that frame.
    async fn present(self: Rc<Self>, app: S6App, message: Option<ListRow>) -> Option<Painted> {
        let existing = self.window.borrow().clone();
        if let Some(window) = existing.filter(|window| window.window.is_visible()) {
            if let Some(row) = &message {
                window.show_message(row);
            }
            let next = window.probe.until(Duration::from_secs(5), |_| true);
            self.stamp_present_call();
            window.window.present();
            return next.await;
        }
        let window = self.ensure_window(&app);
        let opened = self.wait_opened(Duration::from_millis(300)).await;
        if let Some(opened) = &opened {
            window
                .model
                .reset(opened.view, opened.snapshot.clone(), self.config.first_rows);
        }
        if let Some(row) = &message {
            window.show_message(row);
        }
        self.stamp("model-reset");
        let first = window.probe.until(Duration::from_secs(10), |probe| {
            probe.bound() > 0 && probe.placeholders() == 0
        });
        if !self.config.windowed {
            window.window.fullscreen();
        }
        self.stamp_present_call();
        window.window.present();
        let painted = first.await;
        if self.config.first_rows.is_some() {
            let model = window.model.clone();
            glib::idle_add_local_once(move || model.expand());
        }
        if self.windows_built.get() == 1 && self.stamp_value("first-frame").is_none() {
            if let Some(painted) = painted {
                self.stamps
                    .borrow_mut()
                    .insert("first-frame", painted.end_ns);
            }
            let state = self.clone();
            let report_window = window.clone();
            glib::spawn_future_local(async move {
                state.report_startup(&app, &report_window, painted).await;
            });
        }
        painted
    }

    fn stamp_present_call(&self) {
        let t = mono_ns();
        self.stamps.borrow_mut().entry("present").or_insert(t);
        self.stamps.borrow_mut().insert("last-present", t);
    }

    async fn report_startup(
        self: Rc<Self>,
        app: &S6App,
        window: &MainWindow,
        painted: Option<Painted>,
    ) {
        let presented = match painted {
            Some(painted) => {
                window.keep_ticking(30);
                window
                    .probe
                    .presented(painted.counter, Duration::from_millis(1000))
                    .await
            }
            None => None,
        };
        let surface = window.window.surface();
        let monitor = surface
            .as_ref()
            .and_then(|surface| WidgetExt::display(&window.window).monitor_at_surface(surface));
        let settings = gtk4::Settings::default();
        let row = window.row_metrics();
        let memory = memory();
        let scale = json!({
            "env_gdk_scale": std::env::var("GDK_SCALE").ok(),
            "env_gdk_dpi_scale": std::env::var("GDK_DPI_SCALE").ok(),
            "surface_scale_factor": surface.as_ref().map(|s| s.scale_factor()),
            "surface_scale": surface.as_ref().map(|s| s.scale()),
            "widget_scale_factor": window.window.scale_factor(),
            "monitor_scale_factor": monitor.as_ref().map(|m| m.scale_factor()),
            "monitor_scale": monitor.as_ref().map(|m| m.scale()),
            "monitor_geometry": monitor.as_ref().map(|m| {
                let g = m.geometry();
                [g.x(), g.y(), g.width(), g.height()]
            }),
            "window": [window.window.width(), window.window.height()],
            "list": [window.list.width(), window.list.height()],
            "row_height": row.map(|r| r.0),
            "sender_text_px": row.map(|r| [r.1, r.2]),
            "xft_dpi": settings.as_ref().map(|s| s.gtk_xft_dpi()),
            "font_name": settings.as_ref().and_then(|s| s.gtk_font_name()).map(|f| f.to_string()),
            "backend": WidgetExt::display(&window.window).type_().name(),
        });
        if let Some(path) = &self.config.maps_out
            && let Ok(maps) = std::fs::read_to_string("/proc/self/maps")
        {
            let _ = std::fs::write(path, maps);
        }
        emit(
            "startup",
            json!({
                "mode": self.config.mode.name(),
                "stamps": self.stamps_json(),
                "frame": painted.map(|p| json!({
                    "counter": p.counter,
                    "index": p.index,
                    "start": p.start_ns,
                    "layout_end": p.layout_end_ns,
                    "end": p.end_ns,
                    "bound": p.bound,
                    "placeholders": p.placeholders,
                    "binds": p.binds,
                })),
                "presented": presented.map(|p| p.at_ns),
                "next_frames": window
                    .probe
                    .history()
                    .iter()
                    .filter(|p| painted.is_some_and(|first| p.index > first.index))
                    .take(8)
                    .map(|p| json!({
                        "index": p.index,
                        "work_ms": p.start_ns.map(|start| (p.end_ns - start) as f64 / 1e6),
                        "since_first_ms": painted.map(|first| (p.end_ns - first.end_ns) as f64 / 1e6),
                        "binds": p.binds,
                    }))
                    .collect::<Vec<_>>(),
                "refresh_ns": presented.map(|p| p.refresh_ns),
                "renderer": window.window.renderer().map(|r| r.type_().name().to_owned()),
                "rows_total": window.model.total(),
                "snapshot_rows": self.config.snapshot_rows,
                "first_rows": self.config.first_rows,
                "scale": scale,
                "memory": memory,
                "a11y": std::env::var("GTK_A11Y").ok(),
                "renderer_env": std::env::var("GSK_RENDERER").ok(),
                "adw_portal_disabled": std::env::var("ADW_DISABLE_PORTAL").ok(),
            }),
        );
        let state = self.clone();
        let app = app.clone();
        match self.config.mode {
            Mode::Startup => {
                glib::timeout_add_local_once(self.config.linger, move || state.quit(&app));
            }
            Mode::Forward => emit("ready", json!({})),
            Mode::Notify => {
                glib::spawn_future_local(async move { state.notify_check(&app).await });
            }
            Mode::ManualNotify => {
                glib::spawn_future_local(async move { state.manual_notify(&app).await });
            }
            Mode::ManualResume => state.manual_resume_intro(),
            Mode::PresentCheck => {
                glib::spawn_future_local(async move { state.present_check(&app).await });
            }
            Mode::Background => {}
        }
    }

    async fn replies(self: Rc<Self>, replies: async_channel::Receiver<ReadReply>) {
        while let Ok(reply) = replies.recv().await {
            match reply {
                ReadReply::Opened(opened) => {
                    self.stamp("snapshot-received");
                    self.reader_times.set(Some(opened.times));
                    self.opened.replace(Some(Rc::from(opened)));
                    let _ = self.opened_wake.0.try_send(());
                }
                ReadReply::Page { view, page } => {
                    if let Some(window) = self.window.borrow().as_ref() {
                        window.model.apply_page(view, page);
                    }
                }
                ReadReply::Message { id, row } => {
                    let waiters = self.message_waiters.borrow_mut().remove(&id);
                    for waiter in waiters.into_iter().flatten() {
                        let _ = waiter.try_send(row.clone());
                    }
                }
                ReadReply::Failed(error) => emit("read-failed", json!({ "error": error })),
            }
        }
    }

    async fn read_message(&self, id: i64) -> Option<ListRow> {
        let (sender, receiver) = async_channel::bounded(1);
        self.message_waiters
            .borrow_mut()
            .entry(id)
            .or_default()
            .push(sender);
        let reads = self.reads.get()?.clone();
        reads.send(ReadRequest::Message { id }).await.ok()?;
        probe::with_timeout(receiver.recv(), Duration::from_secs(2))
            .await
            .and_then(Result::ok)
            .flatten()
    }

    fn sent_for_target(&self, target: &str) -> Option<Sent> {
        self.sent
            .borrow()
            .iter()
            .rev()
            .find(|sent| sent.target == target)
            .cloned()
    }

    /// The notification's default action: present the window (creating it in background mode)
    /// and show the message, timing each step.
    fn open_message(self: Rc<Self>, app: &S6App, target: String) {
        let action_at = mono_ns();
        let platform = self.platform.take();
        let platform_keys: Vec<String> = platform
            .iter()
            .flat_map(|p| p.iter())
            .filter_map(|entry| entry.child_value(0).str().map(str::to_owned))
            .collect();
        let sent = self.sent_for_target(&target);
        let token = sent
            .as_ref()
            .and_then(|sent| sent.id)
            .and_then(|id| self.tokens.borrow_mut().remove(&id));
        let app = app.clone();
        glib::spawn_future_local(async move {
            let had_window = self
                .window
                .borrow()
                .as_ref()
                .is_some_and(|window| window.window.is_visible());
            let row = match target.split_once(':') {
                Some(("cache", id)) => match id.parse::<i64>() {
                    Ok(id) => self.read_message(id).await,
                    Err(_) => None,
                },
                _ => None,
            };
            let read_at = mono_ns();
            if let Some(token) = &token
                && let Some(window) = self.window.borrow().as_ref()
            {
                window.window.set_startup_id(token);
            }
            self.active_since.set(None);
            let painted = self.clone().present(app.clone(), row.clone()).await;
            if let Some(("imap", uid)) = target.split_once(':')
                && let Some(window) = self.window.borrow().as_ref()
            {
                window.show_text(
                    &format!("New message (UID {uid})"),
                    "Synthetic Sender · testbed INBOX",
                    "A fictional message appended by the S6 bench.",
                );
            }
            let presented = match painted {
                Some(painted) => {
                    let window = self.window.borrow().clone();
                    match window {
                        Some(window) => {
                            window.keep_ticking(30);
                            window
                                .probe
                                .presented(painted.counter, Duration::from_millis(800))
                                .await
                        }
                        None => None,
                    }
                }
                None => None,
            };
            glib::timeout_future(Duration::from_millis(250)).await;
            let active = self.window.borrow().as_ref().map(|w| w.window.is_active());
            let event = json!({
                "target": target,
                "key": sent.as_ref().map(|s| s.key.clone()),
                "notification_id": sent.as_ref().and_then(|s| s.id),
                "invoked_signal": sent.as_ref().and_then(|s| s.action.as_ref().map(|a| a.0)),
                "action": action_at,
                "read": read_at,
                "present": self.stamp_value("last-present"),
                "shown_frame_end": painted.map(|p| p.end_ns),
                "shown_presented": presented.map(|p| p.at_ns),
                "active_at": self.active_since.get(),
                "active_now": active,
                "had_window": had_window,
                "server_token_used": token.as_ref().map(|t| t.len()),
                "platform_keys": platform_keys,
                "row_found": row.is_some(),
                "hyprland_active_class": hyprland_active_class(),
            });
            emit("message-shown", event.clone());
            let _ = self.shown.0.try_send(event);
        });
    }

    fn send_notification(&self, app: &S6App, key: &str, body: &str, target: &str) {
        let notification = notify::new_mail("Mail spike S6", body, target);
        self.unmatched.borrow_mut().push_back(key.to_owned());
        let sent = mono_ns();
        app.send_notification(Some(key), &notification);
        self.sent.borrow_mut().push(Sent {
            key: key.to_owned(),
            target: target.to_owned(),
            sent,
            ..Sent::default()
        });
        emit(
            "notification-sent",
            json!({ "key": key, "target": target, "t": sent }),
        );
    }

    fn sent_by_id(&self, id: u32) -> Option<String> {
        self.sent
            .borrow()
            .iter()
            .find(|sent| sent.id == Some(id))
            .map(|sent| sent.key.clone())
    }

    async fn bus(self: Rc<Self>, events: async_channel::Receiver<BusEvent>) {
        while let Ok(event) = events.recv().await {
            match event {
                BusEvent::Notify {
                    serial,
                    t,
                    app_name,
                    replaces_id,
                    app_icon,
                    actions,
                    hints,
                    desktop_entry,
                    urgency,
                    image_path,
                    expire_timeout,
                } => {
                    let key = self.unmatched.borrow_mut().pop_front();
                    if let Some(key) = &key
                        && let Some(sent) =
                            self.sent.borrow_mut().iter_mut().find(|s| &s.key == key)
                    {
                        sent.serial = Some(serial);
                    }
                    emit(
                        "dbus-notify",
                        json!({
                            "t": t, "key": key, "serial": serial, "app_name": app_name,
                            "replaces_id": replaces_id, "app_icon": app_icon, "actions": actions,
                            "hints": hints, "desktop_entry": desktop_entry, "urgency": urgency,
                            "image_path": image_path, "expire_timeout": expire_timeout,
                        }),
                    );
                }
                BusEvent::Close { serial, t, id } => {
                    emit("dbus-close", json!({ "t": t, "serial": serial, "id": id }));
                }
                BusEvent::Reply {
                    serial,
                    t,
                    id,
                    error,
                } => {
                    let mut sent = self.sent.borrow_mut();
                    let matched = sent.iter_mut().find(|s| s.serial == Some(serial));
                    let key = matched.map(|s| {
                        if s.accepted.is_none() {
                            s.id = id;
                            s.accepted = Some(t);
                        }
                        (s.key.clone(), s.sent)
                    });
                    emit(
                        "dbus-reply",
                        json!({
                            "t": t, "serial": serial, "id": id, "error": error,
                            "key": key.as_ref().map(|k| k.0.clone()),
                            "sent": key.as_ref().map(|k| k.1),
                        }),
                    );
                }
                BusEvent::ActionInvoked { t, id, key } => {
                    let ours = self.sent_by_id(id);
                    if let Some(sent) = self.sent.borrow_mut().iter_mut().find(|s| s.id == Some(id))
                    {
                        sent.action = Some((t, key.clone()));
                    }
                    emit(
                        "dbus-action-invoked",
                        json!({ "t": t, "id": id, "action": key, "ours": ours }),
                    );
                }
                BusEvent::ActivationToken {
                    t,
                    id,
                    token_len,
                    token,
                } => {
                    let ours = self.sent_by_id(id);
                    if ours.is_some() {
                        self.tokens.borrow_mut().insert(id, token);
                        if let Some(sent) =
                            self.sent.borrow_mut().iter_mut().find(|s| s.id == Some(id))
                        {
                            sent.token = Some(t);
                        }
                    }
                    emit(
                        "dbus-activation-token",
                        json!({ "t": t, "id": id, "token_len": token_len, "ours": ours }),
                    );
                }
                BusEvent::Closed { t, id, reason } => {
                    let ours = self.sent_by_id(id);
                    if let Some(sent) = self.sent.borrow_mut().iter_mut().find(|s| s.id == Some(id))
                    {
                        sent.closed = Some((t, reason));
                    }
                    emit(
                        "dbus-closed",
                        json!({ "t": t, "id": id, "reason": reason, "ours": ours }),
                    );
                }
            }
        }
    }

    async fn wait_until(&self, timeout: Duration, condition: impl Fn(&Self) -> bool) -> bool {
        let deadline = mono_ns() + timeout.as_nanos() as i64;
        while mono_ns() < deadline {
            if condition(self) {
                return true;
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        condition(self)
    }

    fn sent_state(&self, key: &str) -> Option<Sent> {
        self.sent.borrow().iter().find(|s| s.key == key).cloned()
    }

    /// Mode `notify`: what the server advertises, one notification, whether it was accepted,
    /// and (on the private bus, where the bench's fake server clicks) what a click does.
    async fn notify_check(self: Rc<Self>, app: &S6App) {
        let Some(connection) = app.dbus_connection() else {
            emit("notify-summary", json!({ "error": "no session bus" }));
            self.quit(app);
            return;
        };
        let info = notify::server_info(&connection).await;
        emit(
            "server-info",
            json!({
                "name": info.name, "vendor": info.vendor, "version": info.version,
                "spec": info.spec, "capabilities": info.capabilities, "error": info.error,
            }),
        );
        let click_wait = Duration::from_millis(
            std::env::var("MAIL_S6_CLICK_WAIT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6000),
        );
        let second = std::env::var_os("MAIL_S6_SECOND_NOTIFICATION").is_some();
        let first_id = self
            .opened
            .borrow()
            .as_ref()
            .and_then(|o| o.snapshot.rows.first().map(|r| r.id));
        let target = format!("cache:{}", first_id.unwrap_or(0));
        self.send_notification(
            app,
            "notify-1",
            "Fictional test message — Mail spike S6 (no action needed)",
            &target,
        );
        self.wait_until(Duration::from_secs(3), |s| {
            s.sent_state("notify-1")
                .is_some_and(|s| s.accepted.is_some())
        })
        .await;
        let shown = probe::with_timeout(self.shown.1.recv(), click_wait).await;
        if second {
            let second_id = self
                .opened
                .borrow()
                .as_ref()
                .and_then(|o| o.snapshot.rows.get(1).map(|r| r.id));
            self.send_notification(
                app,
                "notify-2",
                "Second fictional test message — Mail spike S6",
                &format!("cache:{}", second_id.unwrap_or(0)),
            );
            self.wait_until(click_wait, |s| {
                s.sent_state("notify-2").is_some_and(|s| s.action.is_some())
            })
            .await;
            glib::timeout_future(Duration::from_millis(300)).await;
        }
        for key in ["notify-1", "notify-2"] {
            if self.sent_state(key).is_some_and(|s| s.closed.is_none()) {
                app.withdraw_notification(key);
            }
        }
        self.wait_until(Duration::from_secs(2), |s| {
            s.sent.borrow().iter().all(|sent| sent.closed.is_some())
        })
        .await;
        let sent: Vec<Value> = self.sent.borrow().iter().map(sent_json).collect();
        emit(
            "notify-summary",
            json!({ "sent": sent, "clicked": shown.is_some_and(|r| r.is_ok()) }),
        );
        self.quit(app);
    }

    /// Mode `manual-notify`: two notification clicks by the user, with the window unfocused and
    /// with the window closed (background mode).
    async fn manual_notify(self: Rc<Self>, app: &S6App) {
        let scale = std::env::var("MAIL_S6_MANUAL_SCALE")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(1.0);
        let wait = |seconds: f64| Duration::from_secs_f64(seconds * scale);
        let rows: Vec<i64> = self
            .opened
            .borrow()
            .as_ref()
            .map(|o| o.snapshot.rows.iter().take(2).map(|r| r.id).collect())
            .unwrap_or_default();
        let (first, second) = (
            rows.first().copied().unwrap_or(0),
            rows.get(1).copied().unwrap_or(0),
        );
        let info = match app.dbus_connection() {
            Some(connection) => Some(notify::server_info(&connection).await),
            None => None,
        };
        let say = |title: &str, text: &str| {
            eprintln!("\n== {title}\n{text}\n");
            if let Some(window) = self.window.borrow().as_ref() {
                window.set_instructions(title, text);
            }
        };
        say(
            "Mail spike S6 — notification check, step 1 of 2",
            "Click any other window (for example the terminal you started this from), so this \
             window loses focus. Two seconds later a notification “Mail spike S6” appears. Click \
             it (left button, anywhere on the card). This window should come to the front with the \
             newest message selected.",
        );
        let unfocused = self
            .wait_until(wait(45.0), |s| {
                s.window
                    .borrow()
                    .as_ref()
                    .is_some_and(|w| !w.window.is_active())
            })
            .await;
        glib::timeout_future(wait(2.0)).await;
        self.send_notification(
            app,
            "manual-a",
            "Fictional test message — click to open it (Mail spike S6, step 1)",
            &format!("cache:{first}"),
        );
        let step_a = probe::with_timeout(self.shown.1.recv(), wait(45.0)).await;
        let step_a = step_a.and_then(Result::ok);
        eprintln!(
            "step 1: {}",
            if step_a.is_some() {
                "the click arrived"
            } else {
                "no click within 45 s"
            }
        );
        say(
            "Step 2 of 2",
            "In five seconds this window closes by itself; the app keeps running in the \
             background. Three seconds later a second notification appears. Click it: a new \
             window should open with the second message selected.",
        );
        glib::timeout_future(wait(5.0)).await;
        self.close_window();
        glib::timeout_future(wait(3.0)).await;
        self.send_notification(
            app,
            "manual-b",
            "Fictional test message — click to open it (Mail spike S6, step 2)",
            &format!("cache:{second}"),
        );
        let step_b = probe::with_timeout(self.shown.1.recv(), wait(45.0)).await;
        let step_b = step_b.and_then(Result::ok);
        let sent: Vec<Value> = self.sent.borrow().iter().map(sent_json).collect();
        let summary = json!({
            "server": info.map(|i| json!({
                "name": i.name, "vendor": i.vendor, "version": i.version, "spec": i.spec,
                "capabilities": i.capabilities, "error": i.error,
            })),
            "window_lost_focus": unfocused,
            "step_a": step_a,
            "step_b": step_b,
            "notifications": sent,
        });
        emit("manual-summary", summary.clone());
        let dir = email_spike_s6::paths::repo_root().join("target/s6/manual");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("notify-{}.json", real_ns() / 1_000_000_000));
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        );
        eprintln!("{}", human_summary(&summary));
        eprintln!("Saved to {}", path.display());
        glib::timeout_future(wait(3.0)).await;
        self.quit(app);
    }

    /// Mode `present-check`: a second window of this app takes the focus; then `present()` on
    /// the main window, without an activation token from anyone, should bring it back.
    async fn present_check(self: Rc<Self>, app: &S6App) {
        let Some(main) = self.window.borrow().clone() else {
            self.quit(app);
            return;
        };
        let main_active = self
            .wait_until(Duration::from_secs(3), |s| {
                s.window
                    .borrow()
                    .as_ref()
                    .is_some_and(|w| w.window.is_active())
            })
            .await;
        let second = gtk4::Window::builder()
            .application(app)
            .title("Mail spike S6 — second window")
            .default_width(600)
            .default_height(400)
            .child(&gtk4::Label::new(Some("Mail spike S6: present() check")))
            .build();
        second.present();
        let main_window = main.window.clone();
        let second_window = second.clone();
        let deadline = mono_ns() + 3_000_000_000;
        while mono_ns() < deadline && !(second_window.is_active() && !main_window.is_active()) {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        let moved = second_window.is_active() && !main_window.is_active();
        let class_before = hyprland_active_class();
        let presented = mono_ns();
        main_window.present();
        let deadline = presented + 3_000_000_000;
        while mono_ns() < deadline && !main_window.is_active() {
            glib::timeout_future(Duration::from_millis(2)).await;
        }
        let active_after = main_window.is_active();
        let refocused = active_after.then(|| (mono_ns() - presented) as f64 / 1e6);
        glib::timeout_future(Duration::from_millis(100)).await;
        emit(
            "present-check",
            json!({
                "main_was_active": main_active,
                "second_took_focus": moved,
                "hyprland_class_before": class_before,
                "main_active_after_present": active_after,
                "present_to_active_ms": refocused,
                "hyprland_class_after": hyprland_active_class(),
            }),
        );
        second.destroy();
        self.quit(app);
    }

    fn manual_resume_intro(&self) {
        let text = "This window closes in 8 seconds and the app keeps running in the background \
                    with an IDLE connection to the local testbed. Then suspend the machine (for \
                    example `systemctl suspend`), wait at least 10 seconds, and wake it. The app \
                    logs logind's PrepareForSleep signals, network changes and how long it takes \
                    after the resume until INBOX is synced and idling again (bar: 10 s). Press \
                    Ctrl+C in the terminal when done; it also ends by itself after 15 minutes.";
        eprintln!("\n== Mail spike S6 — resume check\n{text}\n");
        if let Some(window) = self.window.borrow().as_ref() {
            window.set_instructions("Mail spike S6 — resume check", text);
        }
    }

    fn close_window(&self) {
        let window = self.window.borrow().clone();
        if let Some(window) = window {
            emit("close-window", json!({}));
            window.window.close();
        }
    }

    fn reconnect(&self, cause: &'static str) {
        let Some(engine) = self.engine.borrow().as_ref().map(|e| e.commands.clone()) else {
            return;
        };
        self.reconnect.set(Some(Reconnect {
            cause,
            since: mono_ns(),
            connected: None,
            synced: None,
        }));
        let _ = engine.try_send(Command::Reconnect);
    }

    async fn engine_events(
        self: Rc<Self>,
        app: S6App,
        events: async_channel::Receiver<EngineEvent>,
    ) {
        while let Ok(event) = events.recv().await {
            match event {
                EngineEvent::Connected { t } => {
                    emit("imap-connected", json!({ "t": t }));
                    if let Some(mut pending) = self.reconnect.get() {
                        pending.connected.get_or_insert(t);
                        self.reconnect.set(Some(pending));
                    }
                }
                EngineEvent::Synced { started, t } => {
                    emit("imap-synced", json!({ "t": t, "started": started }));
                    if let Some(mut pending) = self.reconnect.get()
                        && pending.connected.is_some()
                    {
                        pending.synced.get_or_insert(t);
                        self.reconnect.set(Some(pending));
                    }
                }
                EngineEvent::Idling { t } => {
                    emit("imap-idling", json!({ "t": t }));
                    if let Some(pending) = self.reconnect.take() {
                        emit(
                            "reconnected",
                            json!({
                                "cause": pending.cause,
                                "since": pending.since,
                                "connected": pending.connected,
                                "synced": pending.synced,
                                "idling": t,
                            }),
                        );
                        if self.config.mode == Mode::ManualResume {
                            let after = |at: Option<i64>| {
                                at.map_or("n/a".to_owned(), |at| {
                                    format!("{:.1} ms", (at - pending.since) as f64 / 1e6)
                                })
                            };
                            eprintln!(
                                "s6: {} → connected {}, INBOX synced {}, idling again {} (bar: 10 s)",
                                pending.cause,
                                after(pending.connected),
                                after(pending.synced),
                                after(Some(t)),
                            );
                        }
                    }
                    if !self.idling_once.replace(true) {
                        let close = match self.config.mode {
                            Mode::Background => self.config.close_when_idling,
                            Mode::ManualResume => true,
                            _ => false,
                        };
                        if close {
                            let state = self.clone();
                            let delay = if self.config.mode == Mode::ManualResume {
                                8000
                            } else {
                                300
                            };
                            glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                                state.close_window()
                            });
                        }
                    }
                }
                EngineEvent::Notice { t } => emit("imap-notice", json!({ "t": t })),
                EngineEvent::Disconnected { t, reason } => {
                    emit("imap-disconnected", json!({ "t": t, "reason": reason }));
                    let pending = self.reconnect.get();
                    if pending.is_none_or(|p| p.connected.is_some()) {
                        self.reconnect.set(Some(Reconnect {
                            cause: pending.map_or("connection-lost", |p| p.cause),
                            since: pending.map_or(t, |p| p.since),
                            connected: None,
                            synced: None,
                        }));
                    }
                }
                EngineEvent::Reconnecting { t } => emit("imap-reconnecting", json!({ "t": t })),
                EngineEvent::NewMail { t, uid, message_id } => {
                    emit(
                        "new-mail",
                        json!({ "t": t, "uid": uid, "message_id": message_id }),
                    );
                    self.send_notification(
                        &app,
                        &format!("mail-{uid}"),
                        "New fictional message on the testbed — Mail spike S6",
                        &format!("imap:{uid}"),
                    );
                }
                EngineEvent::Failed { t, reason } => {
                    emit("engine-failed", json!({ "t": t, "error": reason }))
                }
                EngineEvent::Stopped { t } => {
                    emit("engine-stopped", json!({ "t": t }));
                    let _ = self.engine_stopped.0.try_send(t);
                }
            }
        }
    }

    async fn desktop_events(self: Rc<Self>, events: async_channel::Receiver<DesktopEvent>) {
        let mut available = None;
        while let Ok(event) = events.recv().await {
            match event {
                DesktopEvent::PrepareForSleep {
                    sleeping,
                    mono,
                    boot,
                    real,
                } => {
                    emit(
                        "prepare-for-sleep",
                        json!({ "sleeping": sleeping, "t": mono, "boot": boot, "real": real }),
                    );
                    eprintln!(
                        "s6: PrepareForSleep({sleeping}) at boot clock {:.3} s",
                        boot as f64 / 1e9
                    );
                    if !sleeping {
                        self.last_resume.set(Some(mono));
                        self.reconnect("resume");
                    }
                }
                DesktopEvent::Network {
                    mono,
                    available: now,
                    connectivity,
                    metered,
                } => {
                    emit(
                        "network-changed",
                        json!({ "t": mono, "available": now, "connectivity": connectivity, "metered": metered }),
                    );
                    if self.config.mode == Mode::ManualResume {
                        let since_resume = self
                            .last_resume
                            .get()
                            .map(|resume| {
                                format!(", {:.1} ms after the resume", (mono - resume) as f64 / 1e6)
                            })
                            .unwrap_or_default();
                        eprintln!(
                            "s6: network changed: available {now}, connectivity {connectivity}{since_resume}"
                        );
                    }
                    if available == Some(false) && now {
                        self.reconnect("network-back");
                    }
                    available = Some(now);
                }
            }
        }
    }

    fn quit(self: &Rc<Self>, app: &S6App) {
        if self.quitting.replace(true) {
            return;
        }
        let requested = mono_ns();
        let engine = self.engine.take();
        let state = self.clone();
        let app = app.clone();
        glib::spawn_future_local(async move {
            let mut stopped = None;
            if let Some(engine) = engine {
                let _ = engine.commands.send(Command::Stop).await;
                stopped =
                    probe::with_timeout(state.engine_stopped.1.recv(), Duration::from_secs(5))
                        .await
                        .and_then(Result::ok);
            }
            emit(
                "quit",
                json!({ "requested": requested, "engine_stopped": stopped }),
            );
            state.hold.replace(None);
            if let Some(window) = state.window.take() {
                window.window.destroy();
            }
            app.quit();
        });
    }
}

fn sent_json(sent: &Sent) -> Value {
    json!({
        "key": sent.key,
        "target": sent.target,
        "sent": sent.sent,
        "id": sent.id,
        "accepted": sent.accepted,
        "action": sent.action.as_ref().map(|(t, key)| json!({ "t": t, "key": key })),
        "token": sent.token,
        "closed": sent.closed.map(|(t, reason)| json!({ "t": t, "reason": reason })),
    })
}

fn human_summary(summary: &Value) -> String {
    let ms = |value: &Value, from: &str, to: &str| -> String {
        match (value[from].as_i64(), value[to].as_i64()) {
            (Some(a), Some(b)) => format!("{:.1} ms", (b - a) as f64 / 1e6),
            _ => "n/a".into(),
        }
    };
    let mut lines = vec!["\n== Results".to_owned()];
    if let Some(server) = summary["server"].as_object() {
        lines.push(format!(
            "server: {} {} (spec {}), capabilities {}",
            server["name"], server["version"], server["spec"], server["capabilities"]
        ));
    }
    for (name, step) in [
        ("step 1 (window unfocused)", &summary["step_a"]),
        ("step 2 (window closed)", &summary["step_b"]),
    ] {
        if step.is_null() {
            lines.push(format!("{name}: no click arrived"));
            continue;
        }
        lines.push(format!(
            "{name}: ActionInvoked → action {}; action → message painted {}; action → window active {}; \
             active now {}; Hyprland active class {}; server activation token used: {}",
            ms(step, "invoked_signal", "action"),
            ms(step, "action", "shown_frame_end"),
            ms(step, "action", "active_at"),
            step["active_now"],
            step["hyprland_active_class"],
            step["server_token_used"]
        ));
    }
    lines.join("\n")
}

/// `hyprctl activewindow -j`'s class (read-only), to cross-check focus on the live session.
fn hyprland_active_class() -> Option<String> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let output = std::process::Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    value["class"].as_str().map(str::to_owned)
}

fn memory() -> Value {
    let field = |text: &str, key: &str| -> Option<f64> {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
            .map(|kib| kib / 1024.0)
    };
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    let threads = status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|rest| rest.trim().parse::<u32>().ok());
    json!({
        "rss_mib": field(&status, "VmRSS"),
        "pss_mib": field(&rollup, "Pss"),
        "threads": threads,
    })
}
