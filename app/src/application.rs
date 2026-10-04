//! The GApplication (ML-020): one instance per app-id. A second `sealwax mailto:…` hands its
//! command line to the running instance and exits; launchers activate it over D-Bus
//! (`DBusActivatable=true`: `Activate` and `Open`). The `command-line`, `open` and `activate`
//! handlers build the window synchronously: GApplication shuts down when one of them returns
//! with neither a window nor a hold (ADR-003 amendment, S6). Until the window is presented the
//! GTK thread does nothing but build it. SIGTERM, SIGHUP and SIGINT quit cleanly.

use crate::cli;
use crate::config;
use crate::window::{self, Window};
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;
use sealwax_domain::mailto::{self, Mailto};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::{Cell, OnceCell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The installed icon (hicolor), the same for debug and release builds.
pub const ICON_NAME: &str = "io.github.pixdevsapps.Sealwax";

/// Test hook: overrides the application id, so a self-test and the instances it starts share
/// an id that no real Sealwax uses.
pub const APP_ID_ENV: &str = "SEALWAX_APP_ID";

/// The signals that end a session: uwsm stops app units with SIGTERM at logout and reboot, a
/// closing terminal sends SIGHUP, Ctrl+C SIGINT. Linux's numbers.
const QUIT_SIGNALS: [(i32, &str); 3] = [(15, "SIGTERM"), (1, "SIGHUP"), (2, "SIGINT")];

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub app_id: String,
    pub dirs: SealwaxDirs,
    pub exit_after: Option<Duration>,
    /// No title bar and no window controls ([`window::tiling_desktop`]).
    pub tiling: bool,
}

impl AppConfig {
    pub fn from_env(exit_after: Option<Duration>) -> Self {
        Self {
            app_id: app_id_from_env(),
            dirs: SealwaxDirs::from_env(),
            exit_after,
            tiling: window::tiling_desktop(),
        }
    }
}

pub fn app_id_from_env() -> String {
    std::env::var(APP_ID_ENV)
        .ok()
        .filter(|id| gio::Application::id_is_valid(id))
        .unwrap_or_else(|| config::APP_ID.to_owned())
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SealwaxApplication {
        pub state: OnceCell<Rc<AppState>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SealwaxApplication {
        const NAME: &'static str = "SealwaxApplication";
        type Type = super::SealwaxApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for SealwaxApplication {}

    impl ApplicationImpl for SealwaxApplication {
        fn startup(&self) {
            let state = self.state();
            for hook in state.early_startup.take() {
                hook();
            }
            self.parent_startup();
            state.startup(&self.obj());
        }

        fn command_line(&self, command_line: &gio::ApplicationCommandLine) -> glib::ExitCode {
            self.state().command_line(&self.obj(), command_line)
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            self.state().open(&self.obj(), files);
        }

        fn activate(&self) {
            let state = self.state();
            let window = state.window_or_new(&self.obj());
            state.present(&window);
        }
    }

    impl GtkApplicationImpl for SealwaxApplication {}
    impl AdwApplicationImpl for SealwaxApplication {}

    impl SealwaxApplication {
        fn state(&self) -> Rc<AppState> {
            self.state
                .get()
                .expect("the state is set when the application is built")
                .clone()
        }
    }
}

glib::wrapper! {
    pub struct SealwaxApplication(ObjectSubclass<imp::SealwaxApplication>)
        @extends adw::Application, gtk4::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl SealwaxApplication {
    pub fn new(config: AppConfig, started: Instant) -> Self {
        let flags =
            gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::HANDLES_OPEN;
        let app: Self = glib::Object::builder()
            .property("application-id", config.app_id.as_str())
            .property("flags", flags)
            .build();
        let state = Rc::new(AppState {
            config,
            started: Cell::new(Some(started)),
            window: RefCell::new(None),
            first_frame: Rc::new(Cell::new(None)),
            early_startup: RefCell::new(Vec::new()),
            on_first_window: RefCell::new(None),
            present_gate: RefCell::new(None),
            compose_requests: RefCell::new(Vec::new()),
            activations: Cell::new(0),
            background: RefCell::new(None),
            quitting: Cell::new(false),
        });
        if app.imp().state.set(state).is_err() {
            unreachable!("a new application has no state yet");
        }
        app
    }

    pub fn state(&self) -> Rc<AppState> {
        self.imp()
            .state
            .get()
            .expect("the state is set when the application is built")
            .clone()
    }
}

pub fn run(config: AppConfig, started: Instant) -> glib::ExitCode {
    SealwaxApplication::new(config, started).run()
}

type Hook = Box<dyn FnOnce()>;
type WindowHook = Box<dyn FnOnce(Window)>;
type PresentGate = Pin<Box<dyn Future<Output = ()>>>;

pub struct AppState {
    config: AppConfig,
    started: Cell<Option<Instant>>,
    window: RefCell<Option<Window>>,
    first_frame: Rc<Cell<Option<Instant>>>,
    early_startup: RefCell<Vec<Hook>>,
    on_first_window: RefCell<Option<WindowHook>>,
    present_gate: RefCell<Option<PresentGate>>,
    compose_requests: RefCell<Vec<Mailto>>,
    activations: Cell<u32>,
    background: RefCell<Option<gio::ApplicationHoldGuard>>,
    quitting: Cell<bool>,
}

impl AppState {
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn window(&self) -> Option<Window> {
        self.window.borrow().clone()
    }

    /// Runs `hook` first in `startup`, before GTK and libadwaita initialise (≈ 45 ms, S6): the
    /// place to start the store's reader thread (ADR-003), so the first page is ready before
    /// the window needs it.
    pub fn connect_early_startup(&self, hook: impl FnOnce() + 'static) {
        self.early_startup.borrow_mut().push(Box::new(hook));
    }

    /// Runs `hook` once the first window has been presented (the self-test starts there).
    pub fn connect_first_window(&self, hook: impl FnOnce(Window) + 'static) {
        self.on_first_window.replace(Some(Box::new(hook)));
    }

    /// Keeps the first window hidden until `gate` resolves, such as the theme's first read
    /// (ADR-004: at most 500 ms) or the store's first page (S6: at most 300 ms). The window is
    /// still built synchronously.
    pub fn hold_first_window(&self, gate: impl Future<Output = ()> + 'static) {
        self.present_gate.replace(Some(Box::pin(gate)));
    }

    /// When the first window's first frame was painted.
    pub fn first_frame(&self) -> Option<Instant> {
        self.first_frame.get()
    }

    /// The `mailto:` drafts asked for so far, oldest first; the composer (M6) takes them.
    pub fn compose_requests(&self) -> Vec<Mailto> {
        self.compose_requests.borrow().clone()
    }

    /// How many launches and activations showed the window.
    pub fn activations(&self) -> u32 {
        self.activations.get()
    }

    /// Background mode (D6) is an application hold: with it, closing the window leaves the
    /// process running, and the next activation builds a new window (ADR-003 amendment).
    pub fn set_background(&self, app: &SealwaxApplication, on: bool) {
        if on {
            if self.background.borrow().is_none() {
                self.background.replace(Some(app.hold()));
            }
        } else {
            self.background.take();
        }
    }

    pub fn is_background(&self) -> bool {
        self.background.borrow().is_some()
    }

    /// Quits the application: the one path for signals, the exit timer and the Quit action.
    pub fn quit(&self, app: &SealwaxApplication) {
        if self.quitting.replace(true) {
            return;
        }
        self.background.take();
        for window in app.windows() {
            window.destroy();
        }
        app.quit();
    }

    fn startup(self: &Rc<Self>, app: &SealwaxApplication) {
        gtk4::Window::set_default_icon_name(ICON_NAME);
        // A closed window leaves the application here; GTK emits `destroy` only once nothing
        // holds the window, and this state does.
        app.connect_window_removed(glib::clone!(
            #[weak(rename_to = state)]
            self,
            move |_, removed| {
                let ours =
                    state.window.borrow().as_ref().is_some_and(|window| {
                        window.window().upcast_ref::<gtk4::Window>() == removed
                    });
                if ours {
                    state.window.replace(None);
                }
            }
        ));
        for (signal, name) in QUIT_SIGNALS {
            glib_unix::unix_signal_add_local(
                signal,
                glib::clone!(
                    #[weak]
                    app,
                    #[weak(rename_to = state)]
                    self,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        tracing::info!("{name}: quitting");
                        state.quit(&app);
                        glib::ControlFlow::Continue
                    }
                ),
            );
        }
        if let Some(delay) = self.config.exit_after {
            glib::timeout_add_local_once(
                delay,
                glib::clone!(
                    #[weak]
                    app,
                    #[weak(rename_to = state)]
                    self,
                    move || {
                        tracing::info!("{} elapsed; quitting", config::EXIT_AFTER_ENV);
                        state.quit(&app);
                    }
                ),
            );
        }
    }

    fn command_line(
        self: &Rc<Self>,
        app: &SealwaxApplication,
        command_line: &gio::ApplicationCommandLine,
    ) -> glib::ExitCode {
        let parsed = match cli::parse(command_line.arguments()) {
            Ok(parsed) => parsed,
            Err(error) => {
                let text = error.render().to_string();
                if error.use_stderr() {
                    command_line.printerr_literal(&text);
                } else {
                    command_line.print_literal(&text);
                }
                return glib::ExitCode::from(error.exit_code() as u8);
            }
        };
        if parsed.self_test.is_some() {
            command_line.printerr_literal(
                "sealwax: --self-test starts its own instance; it cannot be sent to a running \
                 one\n",
            );
            return glib::ExitCode::from(2);
        }
        let window = self.window_or_new(app);
        for uri in &parsed.uris {
            self.compose(uri);
        }
        self.present(&window);
        glib::ExitCode::SUCCESS
    }

    fn open(self: &Rc<Self>, app: &SealwaxApplication, files: &[gio::File]) {
        let window = self.window_or_new(app);
        for file in files {
            let uri = file.uri();
            if mailto::is_mailto(&uri) {
                self.compose(&uri);
            } else {
                tracing::warn!("ignored an open request that is not a mailto: link");
            }
        }
        self.present(&window);
    }

    /// A `mailto:` link to write a message from. The composer comes in M6; until then the
    /// draft is kept, and only the request is logged, never its contents.
    fn compose(&self, uri: &str) {
        match mailto::parse(uri) {
            Ok(draft) => {
                tracing::info!("compose requested");
                self.compose_requests.borrow_mut().push(draft);
            }
            Err(error) => tracing::warn!("{error}"),
        }
    }

    /// The window, built on the first request.
    fn window_or_new(self: &Rc<Self>, app: &SealwaxApplication) -> Window {
        if let Some(window) = self.window() {
            return window;
        }
        let window = Window::new(app.upcast_ref(), self.config.tiling);
        self.window.replace(Some(window.clone()));
        window
    }

    /// Shows the window. The first time, after the gate (if any), with the first frame logged.
    fn present(self: &Rc<Self>, window: &Window) {
        self.activations.set(self.activations.get() + 1);
        if window.window().is_visible() {
            window.present();
            return;
        }
        match self.present_gate.take() {
            Some(gate) => {
                let state = self.clone();
                let window = window.clone();
                glib::spawn_future_local(async move {
                    gate.await;
                    state.show(&window);
                });
            }
            None => self.show(window),
        }
    }

    fn show(&self, window: &Window) {
        if let Some(started) = self.started.take() {
            log_first_frame(window.window(), started, self.first_frame.clone());
        }
        window.present();
        if let Some(hook) = self.on_first_window.take() {
            hook(window.clone());
        }
    }
}

/// Logs `first-frame <ms since process start>` after the first paint that follows `window`
/// mapping, and records when it was. The CI smoke run greps for it.
fn log_first_frame(
    window: &adw::ApplicationWindow,
    started: Instant,
    painted: Rc<Cell<Option<Instant>>>,
) {
    let on_map = Rc::new(RefCell::new(None));
    let handler = window.connect_map(glib::clone!(
        #[strong]
        on_map,
        move |window| {
            if let Some(handler) = on_map.take() {
                window.disconnect(handler);
            }
            let Some(clock) = window.frame_clock() else {
                tracing::warn!("mapped window has no frame clock; first-frame not measured");
                return;
            };
            let on_paint = Rc::new(RefCell::new(None));
            let painted = painted.clone();
            let handler = clock.connect_after_paint(glib::clone!(
                #[strong]
                on_paint,
                move |clock| {
                    if let Some(handler) = on_paint.take() {
                        clock.disconnect(handler);
                    }
                    painted.set(Some(Instant::now()));
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    tracing::info!("first-frame {ms:.1}");
                }
            ));
            on_paint.replace(Some(handler));
        }
    ));
    on_map.replace(Some(handler));
}
