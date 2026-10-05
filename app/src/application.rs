//! The GApplication (ML-020): one instance per app-id. A second `sealwax mailto:…` hands its
//! command line to the running instance and exits; launchers activate it over D-Bus
//! (`DBusActivatable=true`: `Activate` and `Open`). The `command-line`, `open` and `activate`
//! handlers build the window synchronously: GApplication shuts down when one of them returns
//! with neither a window nor a hold (ADR-003 amendment, S6). Until the window is presented the
//! GTK thread does nothing but build it. SIGTERM, SIGHUP and SIGINT quit cleanly.

use crate::actions::{self, ActionSet};
use crate::cli;
use crate::config;
use crate::engine::{self, EngineOptions};
use crate::engine_link::{EngineLink, Subscription};
use crate::list::MessageList;
use crate::mailbox::{self, Latch, Mailbox};
use crate::menu;
use crate::oauth;
use crate::palette;
use crate::preferences::{self, Preferences, SettingsFile, Update};
use crate::reader::{MessageSource, Position, Reader};
use crate::setup::{self, Again, remove::Removable};
use crate::shortcuts;
use crate::theme::{self, ThemeController};
use crate::window::{self, AccountStatus, Pane, SignInPrompt, Window};
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;
use sealwax_domain::account::{AuthMethod, OAuthProvider};
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::mailto::{self, Mailto};
use sealwax_domain::model::MessageId;
use sealwax_engine::api::Command;
use sealwax_engine::service::Engine;
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

/// How long the first window waits for `config.toml` and `keys.toml`, so its first frame already
/// has the user's corners, fonts and keys. The first read takes a few milliseconds.
const SETTINGS_WAIT: Duration = Duration::from_millis(150);

/// How long the first window waits for the store's first rows (S6: at most 300 ms), so its
/// first frame never shows an empty list. The store opens in about 2 ms.
const FIRST_ROWS_WAIT: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub app_id: String,
    pub dirs: SealwaxDirs,
    pub exit_after: Option<Duration>,
    /// No title bar and no window controls ([`window::tiling_desktop`]).
    pub tiling: bool,
    /// Starts the engine after the first frame; self-tests start it from their script.
    pub engine: Option<EngineOptions>,
    /// The build's Microsoft client ID ([`oauth::BUILD_MICROSOFT_CLIENT_ID`]), which
    /// `config.toml` overrides. Self-tests run without one, whatever the build had.
    pub microsoft_client_id: Option<String>,
}

impl AppConfig {
    pub fn from_env(exit_after: Option<Duration>) -> Self {
        Self {
            app_id: app_id_from_env(),
            dirs: SealwaxDirs::from_env(),
            exit_after,
            tiling: window::tiling_desktop(),
            engine: Some(EngineOptions::from_env()),
            microsoft_client_id: oauth::BUILD_MICROSOFT_CLIENT_ID.map(str::to_owned),
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

        fn shutdown(&self) {
            self.state().stop_engine();
            self.state().mailbox.close();
            self.parent_shutdown();
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
            after_first_frame: Rc::default(),
            early_startup: RefCell::new(Vec::new()),
            on_first_window: RefCell::new(None),
            present_gate: RefCell::new(None),
            compose_requests: RefCell::new(Vec::new()),
            activations: Cell::new(0),
            background: RefCell::new(None),
            quitting: Cell::new(false),
            theme: OnceCell::new(),
            prefs: OnceCell::new(),
            app_actions: OnceCell::new(),
            win_actions: RefCell::new(None),
            mailbox: Mailbox::new(),
            first_rows: Latch::new(),
            list: RefCell::new(None),
            store_warned: Cell::new(false),
            reader: RefCell::new(None),
            reader_shows: Cell::new(None),
            engine: EngineLink::new(),
            engine_service: RefCell::new(None),
            engine_events: RefCell::new(None),
            accounts_read: Cell::new(false),
        });
        let mailbox = state.mailbox.clone();
        let dirs = state.config.dirs.clone();
        state.connect_early_startup(move || mailbox.open(&dirs));
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
    /// Work that waits until the first frame is painted, so it never delays it.
    after_first_frame: Rc<RefCell<Vec<Hook>>>,
    early_startup: RefCell<Vec<Hook>>,
    on_first_window: RefCell<Option<WindowHook>>,
    present_gate: RefCell<Option<PresentGate>>,
    compose_requests: RefCell<Vec<Mailto>>,
    activations: Cell<u32>,
    background: RefCell<Option<gio::ApplicationHoldGuard>>,
    quitting: Cell<bool>,
    theme: OnceCell<Rc<ThemeController>>,
    prefs: OnceCell<Rc<Preferences>>,
    app_actions: OnceCell<ActionSet>,
    win_actions: RefCell<Option<ActionSet>>,
    mailbox: Rc<Mailbox>,
    /// Opens when the list has the first rows of its first folder, or there is nothing to show.
    first_rows: Rc<Latch>,
    list: RefCell<Option<MessageList>>,
    store_warned: Cell<bool>,
    reader: RefCell<Option<Reader>>,
    /// The message the reader shows; `None` while the window shows "No message selected".
    reader_shows: Cell<Option<MessageId>>,
    /// The engine's commands and events (ADR-003); not connected until an engine is attached.
    engine: EngineLink,
    /// The engine this application started; the setup self-tests attach a fake one instead.
    engine_service: RefCell<Option<Engine>>,
    engine_events: RefCell<Option<Subscription>>,
    /// `accounts.toml` has been read without mistakes at least once.
    accounts_read: Cell<bool>,
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
    /// (ADR-004: at most 500 ms) or the store's first page (S6: at most 300 ms); with several
    /// gates, until all of them have. The window is still built synchronously.
    pub fn hold_first_window(&self, gate: impl Future<Output = ()> + 'static) {
        let gate: PresentGate = match self.present_gate.take() {
            Some(earlier) => Box::pin(both(earlier, gate)),
            None => Box::pin(gate),
        };
        self.present_gate.replace(Some(gate));
    }

    /// The message store (ADR-005), opening from `startup` on.
    pub fn mailbox(&self) -> Rc<Mailbox> {
        self.mailbox.clone()
    }

    /// The window's message list, once there is a window.
    pub fn list(&self) -> Option<MessageList> {
        self.list.borrow().clone()
    }

    /// The reader (ML-027), once the first window exists.
    pub fn reader(&self) -> Option<Reader> {
        self.reader.borrow().clone()
    }

    /// The link to the engine; `EngineLink::attach` connects it.
    pub fn engine(&self) -> EngineLink {
        self.engine.clone()
    }

    /// The Microsoft client ID in use (ADR-008): `config.toml`'s, else the build's. Microsoft
    /// sign-in is offered only with one.
    pub fn microsoft_client_id(&self) -> Option<String> {
        let config = self
            .preferences()
            .map(|prefs| prefs.config())
            .unwrap_or_default();
        oauth::microsoft_client_id(&config, self.config.microsoft_client_id.as_deref())
    }

    /// For the setup assistant: whether a Microsoft client ID is in use, asked when it draws.
    fn microsoft_available(self: &Rc<Self>) -> setup::MicrosoftAvailable {
        let weak = Rc::downgrade(self);
        Rc::new(move || {
            weak.upgrade()
                .is_some_and(|state| state.microsoft_client_id().is_some())
        })
    }

    /// The accounts that must sign in again with Microsoft (ML-032), in the sidebar's order:
    /// those the engine reported as needing it that `accounts.toml` signs in with Microsoft.
    /// Password accounts keep the sidebar's state.
    pub fn sign_in_needed(&self) -> Vec<Again> {
        let Some(prefs) = self.preferences() else {
            return Vec::new();
        };
        let listed = prefs.accounts();
        self.mailbox
            .accounts()
            .into_iter()
            .filter(|account| self.mailbox.status(account.id) == AccountStatus::NeedsSignIn)
            .filter_map(|account| {
                let key = account.key?;
                let settings = listed.iter().find(|settings| settings.key == key)?;
                (settings.auth == AuthMethod::OAuth(OAuthProvider::Microsoft)).then(|| Again {
                    key,
                    name: account.name,
                    address: settings.address.clone(),
                })
            })
            .collect()
    }

    /// One banner names the first account that must sign in again and counts the others; its
    /// Sign in takes them one at a time.
    fn update_sign_in_banner(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let waiting = self.sign_in_needed();
        window.show_sign_in_banner(waiting.first().map(|first| SignInPrompt {
            name: &first.name,
            more: waiting.len() - 1,
        }));
    }

    /// The banner's Sign in: the assistant at Microsoft sign-in for the first account it names.
    fn sign_in_again(self: &Rc<Self>) {
        let (Some(window), Some(again)) = (self.window(), self.sign_in_needed().into_iter().next())
        else {
            return;
        };
        setup::sign_in_again(&window, &self.engine, self.microsoft_available(), again);
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

    /// The theme follower (ADR-004), from `startup` on.
    pub fn theme(&self) -> Option<Rc<ThemeController>> {
        self.theme.get().cloned()
    }

    /// `config.toml` and `keys.toml` (ADR-009), from `startup` on.
    pub fn preferences(&self) -> Option<Rc<Preferences>> {
        self.prefs.get().cloned()
    }

    /// Whether a command is built and usable now, in either action group.
    pub fn is_available(&self, id: ActionId) -> bool {
        self.app_actions
            .get()
            .is_some_and(|set| set.is_available(id))
            || self
                .win_actions
                .borrow()
                .as_ref()
                .is_some_and(|set| set.is_available(id))
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
        self.start_settings(app);
        let weak = Rc::downgrade(self);
        self.mailbox.connect_folders_changed(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Some(actions) = state.win_actions.borrow().as_ref() {
                actions.set_available(
                    ActionId::RemoveAccount,
                    !state.removable_accounts().is_empty(),
                );
            }
            if let (Some(window), Some(list)) = (state.window(), state.list()) {
                state.fill_sidebar(&window, &list);
            }
            state.update_sign_in_banner();
        });
        // The sidebar's account states follow whatever engine the link has: the one started
        // below, or a self-test's scripted one.
        let mailbox = Rc::downgrade(&self.mailbox);
        let subscription = self.engine.subscribe(move |event| {
            if let (Some(mailbox), Some((account, status))) =
                (mailbox.upgrade(), engine::account_status(event))
            {
                mailbox.set_status(account, status);
            }
        });
        self.engine_events.replace(Some(subscription));
        if let Some(options) = self.config.engine.clone() {
            let ready = self.mailbox.wait_ready();
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                ready.await;
                if let Some(state) = weak.upgrade() {
                    let started = Rc::downgrade(&state);
                    state.run_after_first_frame(move || {
                        if let Some(state) = started.upgrade() {
                            state.start_engine(&options);
                        }
                    });
                }
            });
        }
        let ready = self.mailbox.wait_ready();
        let first_rows = self.first_rows.wait();
        self.hold_first_window(async move {
            let rows = async move {
                ready.await;
                first_rows.await;
            };
            if mailbox::with_timeout(rows, FIRST_ROWS_WAIT).await.is_none() {
                tracing::debug!("the first window stopped waiting for the list's first rows");
            }
        });
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
                    state.list.replace(None);
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

    /// The theme, the settings files and the application's commands. The theme read blocks
    /// for at most `theme::STARTUP_WAIT` (about 9 ms here); the settings are read off the GTK
    /// thread, and the first window waits up to `SETTINGS_WAIT` for them.
    fn start_settings(self: &Rc<Self>, app: &SealwaxApplication) {
        let prefs = Preferences::start(&self.config.dirs);
        let theme = ThemeController::start(prefs.config().appearance, theme::STARTUP_WAIT);
        let app_actions = actions::install_app_actions(app);
        let weak_app = app.downgrade();
        let weak = Rc::downgrade(self);
        app_actions.connect(ActionId::Quit, move || {
            if let (Some(app), Some(state)) = (weak_app.upgrade(), weak.upgrade()) {
                state.quit(&app);
            }
        });
        for (id, file) in [
            (ActionId::Preferences, SettingsFile::Config),
            (ActionId::CustomizeShortcuts, SettingsFile::Keys),
        ] {
            let weak = Rc::downgrade(self);
            app_actions.connect(id, move || {
                if let Some(state) = weak.upgrade() {
                    state.open_settings(file);
                }
            });
        }
        let weak = Rc::downgrade(self);
        app_actions.connect(ActionId::KeyboardShortcuts, move || {
            if let Some(state) = weak.upgrade() {
                state.show_shortcuts();
            }
        });
        let weak_app = app.downgrade();
        let weak = Rc::downgrade(self);
        prefs.connect_changed(move |update| {
            if let (Some(app), Some(state)) = (weak_app.upgrade(), weak.upgrade()) {
                state.settings_changed(&app, update);
            }
        });
        let ready = prefs.clone();
        self.hold_first_window(async move { ready.ready(SETTINGS_WAIT).await });
        let _ = self.theme.set(theme);
        let _ = self.prefs.set(prefs);
        let _ = self.app_actions.set(app_actions);
    }

    /// `config.toml` or `keys.toml` changed, or one of them has a mistake.
    fn settings_changed(&self, app: &SealwaxApplication, update: &Update) {
        let Some(prefs) = self.preferences() else {
            return;
        };
        if update.config_before.is_some() {
            if let Some(theme) = self.theme() {
                theme.set_options(prefs.config().appearance);
            }
            // A client ID added or removed makes Microsoft sign-in available or not.
            setup::refresh();
        }
        if update.keymap_changed {
            let keymap = prefs.keymap();
            actions::apply_accels(app, &keymap);
            if let Some(window) = self.window() {
                window.set_menu_model(Some(menu::primary_menu(&keymap).upcast_ref()));
                window.set_keymap(&keymap);
            }
            if let Some(list) = self.list() {
                list.update_keys(&keymap);
            }
            if let Some(reader) = self.reader() {
                reader.update_keys(&keymap);
            }
        }
        if update.accounts_changed {
            self.accounts_read.set(true);
            self.send_accounts();
            self.update_sign_in_banner();
        }
        for problem in &update.problems {
            tracing::warn!("{problem}");
            if let Some(window) = self.window() {
                window.add_toast(adw::Toast::new(problem));
            }
        }
    }

    /// The window's commands: the palette, and the main menu built from the registry.
    fn install_window(self: &Rc<Self>, app: &SealwaxApplication, window: &Window) {
        let Some(prefs) = self.preferences() else {
            return;
        };
        let keymap = prefs.keymap();
        let win_actions = actions::install_window_actions(window.window());
        let target = window.window().downgrade();
        win_actions.connect(ActionId::CommandPalette, move || {
            if let Some(window) = target.upgrade() {
                palette::present(&window, &prefs.keymap());
            }
        });
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::AddAccount, move || {
            if let Some(state) = weak.upgrade()
                && let Some(window) = state.window()
            {
                setup::open(&window, &state.engine, state.microsoft_available());
            }
        });
        let weak = Rc::downgrade(self);
        window.connect_sign_in(move || {
            if let Some(state) = weak.upgrade() {
                state.sign_in_again();
            }
        });
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::RemoveAccount, move || {
            if let Some(state) = weak.upgrade()
                && let Some(window) = state.window()
            {
                setup::remove::confirm(&window, &state.engine, state.removable_accounts());
            }
        });
        win_actions.set_available(
            ActionId::RemoveAccount,
            !self.removable_accounts().is_empty(),
        );
        window.set_menu_model(Some(menu::primary_menu(&keymap).upcast_ref()));
        window.set_keymap(&keymap);
        actions::apply_accels(app, &keymap);
        self.install_list(window, &win_actions, &keymap);
        self.win_actions.replace(Some(win_actions));
    }

    /// The message list (ML-026) in the window's list slot. It shows the folder picked in the
    /// sidebar, which the store fills and keeps current; its keys move and open the selection.
    fn install_list(self: &Rc<Self>, window: &Window, win_actions: &ActionSet, keymap: &Keymap) {
        let list = MessageList::new(self.mailbox.clone());
        list.set_first_rows(self.first_rows.clone());
        list.install_keys(keymap);
        window.set_list(list.widget());
        GtkWindowExt::set_focus(window.window(), Some(list.list_view()));

        let target = list.clone();
        win_actions.connect(ActionId::NextMessage, move || target.select_next());
        let target = list.clone();
        win_actions.connect(ActionId::PreviousMessage, move || target.select_previous());
        let target = list.clone();
        win_actions.connect(ActionId::OpenMessage, move || target.open_selected());
        let target = list.clone();
        win_actions.connect(ActionId::Search, move || target.focus_search());
        let pane = window.clone();
        list.connect_message_opened(move |_| pane.show_pane(Pane::Reader));
        let weak = Rc::downgrade(self);
        list.connect_message_selected(move |_| {
            if let Some(state) = weak.upgrade() {
                state.set_has_selection(true);
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_selection_cleared(move || {
            if let Some(state) = weak.upgrade() {
                state.set_has_selection(false);
            }
        });
        let heading = window.clone();
        list.connect_search_changed(move |search| match search {
            Some(search) => heading.set_list_heading("Search", &search.subtitle()),
            None => {
                let (title, subtitle) = heading
                    .sidebar()
                    .heading(&heading.selected_folder())
                    .unwrap_or_default();
                heading.set_list_heading(&title, &subtitle);
            }
        });

        let mailbox = self.mailbox.clone();
        let shown = list.clone();
        window.connect_folder_selected(move |_, key| shown.show(mailbox.scope_for_key(key)));
        if self.mailbox.is_ready() {
            self.fill_sidebar(window, &list);
        }
        // The reader's WebKit engine is created after the first frame: it isn't needed to show
        // the list, and creating it inside the window build cost startup time (M1, live). It
        // takes the keymap current by then, since `settings_changed` only updates a reader
        // that exists.
        let weak = Rc::downgrade(self);
        let (target, shown) = (window.clone(), list.clone());
        self.run_after_first_frame(move || {
            if let Some(state) = weak.upgrade()
                && let Some(prefs) = state.preferences()
            {
                state.install_reader(&target, &shown, &prefs.keymap());
            }
        });
        self.list.replace(Some(list));
    }

    /// Starts the engine on the open store and attaches it to the app's link. Once
    /// `accounts.toml` has been read, the engine gets its accounts and syncs them.
    pub fn start_engine(self: &Rc<Self>, options: &EngineOptions) {
        if self.engine_service.borrow().is_some() || self.quitting.get() {
            return;
        }
        let (Some(store), Some(blobs)) = (self.mailbox.shared_store(), self.mailbox.blobs()) else {
            tracing::warn!("the engine needs the store, which isn't open");
            return;
        };
        let engine = match engine::start(
            &self.config.app_id,
            &self.config.dirs,
            store,
            blobs,
            options,
        ) {
            Ok(engine) => engine,
            Err(error) => {
                tracing::error!("the engine could not start: {error}");
                return;
            }
        };
        self.engine.attach(engine.commands(), engine.events());
        self.engine_service.replace(Some(engine));
        tracing::info!("engine started");
        self.send_accounts();
    }

    /// The accounts Remove Account offers: those `accounts.toml` lists, so not the local
    /// accounts of fixtures and self-tests.
    fn removable_accounts(&self) -> Vec<Removable> {
        self.mailbox
            .accounts()
            .into_iter()
            .filter_map(|account| {
                Some(Removable {
                    key: account.key?,
                    name: account.name,
                    address: account.address,
                })
            })
            .collect()
    }

    /// Hands the accounts from `accounts.toml` to the engine this application started.
    fn send_accounts(&self) {
        if !self.accounts_read.get() || self.engine_service.borrow().is_none() {
            return;
        }
        let Some(prefs) = self.preferences() else {
            return;
        };
        let accounts = prefs.accounts();
        for account in self.mailbox.accounts() {
            if account
                .key
                .as_ref()
                .is_some_and(|key| accounts.iter().any(|listed| &listed.key == key))
            {
                self.mailbox.set_status(account.id, AccountStatus::Syncing);
            }
        }
        if self.engine.send(Command::Accounts { accounts }).is_err() {
            tracing::warn!("the engine stopped listening");
        }
    }

    /// Stops the engine before the store closes: its connections end and its writes finish.
    fn stop_engine(&self) {
        self.engine_events.take();
        let Some(engine) = self.engine_service.take() else {
            return;
        };
        self.engine.detach();
        let started = Instant::now();
        engine.stop();
        tracing::info!(
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "engine stopped"
        );
    }

    /// Runs `hook` once the first window's first frame is painted, or at once if it already is.
    fn run_after_first_frame(&self, hook: impl FnOnce() + 'static) {
        if self.first_frame.get().is_some() {
            hook();
        } else {
            self.after_first_frame.borrow_mut().push(Box::new(hook));
        }
    }

    /// The reader (ML-027) shows the list's selection, with its place in the list ("n of N")
    /// and previous and next. Clearing goes reader first, then the window: the other order
    /// moves GTK's focus through a view already back in the pool.
    fn install_reader(self: &Rc<Self>, window: &Window, list: &MessageList, keymap: &Keymap) {
        let Some(theme) = self.theme() else {
            return;
        };
        let source: Rc<dyn MessageSource> = self.mailbox.clone();
        let reader = Reader::new(source, theme, keymap, &self.config.dirs.cache);
        window.set_reader_toolbar(&reader.toolbar());

        let weak = Rc::downgrade(self);
        let target = window.clone();
        list.connect_message_selected(move |id| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let Some(reader) = state.reader() else {
                return;
            };
            if state.reader_shows.replace(Some(id)).is_none() {
                target.set_reader(&reader.widget());
            }
            reader.show(id);
            state.update_position();
        });
        let weak = Rc::downgrade(self);
        let target = window.clone();
        list.connect_selection_cleared(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if state.reader_shows.take().is_some() {
                if let Some(reader) = state.reader() {
                    reader.clear();
                    reader.set_position(None);
                }
                target.clear_reader();
            }
        });
        if let Some(id) = list.selected() {
            self.reader_shows.set(Some(id));
            window.set_reader(&reader.widget());
            reader.show(id);
        }
        let previous = list.clone();
        reader.connect_previous(move || previous.select_previous());
        let next = list.clone();
        reader.connect_next(move || next.select_next());
        let weak = Rc::downgrade(self);
        self.mailbox.connect_event(move |_| {
            if let Some(state) = weak.upgrade() {
                state.update_position();
            }
        });
        self.reader.replace(Some(reader));
    }

    /// "n of N" for the message the reader shows; new mail above it moves it.
    fn update_position(&self) {
        let (Some(reader), Some(list), Some(id)) =
            (self.reader(), self.list(), self.reader_shows.get())
        else {
            return;
        };
        let position = list.position(id).map(|(n, total)| Position {
            index: n.saturating_sub(1),
            total,
        });
        reader.set_position(position);
    }

    /// The store's accounts and folders in the window's sidebar; with no account, the first
    /// window waits no longer for rows.
    fn fill_sidebar(&self, window: &Window, list: &MessageList) {
        if let Some(error) = self.mailbox.error()
            && !self.store_warned.replace(true)
        {
            window.add_toast(adw::Toast::new(&format!(
                "The mail store could not be opened: {error}"
            )));
        }
        let model = self.mailbox.sidebar_model();
        if model.accounts.is_empty() {
            self.first_rows.open();
        }
        window.set_sidebar(model);
        list.refresh_heading();
    }

    /// Whether a message is selected, for the commands that act on one.
    fn set_has_selection(&self, has_selection: bool) {
        if let Some(actions) = self.win_actions.borrow().as_ref() {
            actions.set_has_selection(has_selection);
        }
    }

    /// Preferences and Customize Shortcuts: the file, created with its defaults when missing,
    /// in the default text editor; the outcome as a toast.
    fn open_settings(&self, file: SettingsFile) {
        let (Some(prefs), Some(window)) = (self.preferences(), self.window()) else {
            return;
        };
        let path = prefs.path(file);
        glib::spawn_future_local(async move {
            let message = preferences::open_in_editor(window.window(), path, file).await;
            window.add_toast(adw::Toast::new(&message));
        });
    }

    fn show_shortcuts(self: &Rc<Self>) {
        let (Some(prefs), Some(window)) = (self.preferences(), self.window()) else {
            return;
        };
        let state = self.clone();
        shortcuts::present(window.window(), &prefs.keymap(), &move |id| {
            state.is_available(id)
        });
    }

    /// The window, built on the first request.
    fn window_or_new(self: &Rc<Self>, app: &SealwaxApplication) -> Window {
        if let Some(window) = self.window() {
            return window;
        }
        let window = Window::new(app.upcast_ref(), self.config.tiling);
        self.install_window(app, &window);
        self.window.replace(Some(window.clone()));
        self.update_sign_in_banner();
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
            let exec_ns = std::env::var(config::EXEC_NS_ENV)
                .ok()
                .and_then(|value| value.parse::<i64>().ok());
            if std::env::var_os(config::START_FULLSCREEN_ENV).is_some_and(|value| value == "1") {
                window.window().fullscreen();
            }
            log_first_frame(
                window.window(),
                started,
                exec_ns,
                self.first_frame.clone(),
                self.after_first_frame.clone(),
            );
        }
        window.present();
        if let Some(hook) = self.on_first_window.take() {
            hook(window.clone());
        }
    }
}

/// Resolves once both futures have, polling them together.
async fn both(first: impl Future<Output = ()>, second: impl Future<Output = ()>) {
    let mut first = Box::pin(first);
    let mut second = Box::pin(second);
    let (mut first_done, mut second_done) = (false, false);
    std::future::poll_fn(|context| {
        first_done = first_done || first.as_mut().poll(context).is_ready();
        second_done = second_done || second.as_mut().poll(context).is_ready();
        if first_done && second_done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
}

/// Logs `first-frame <ms since process start>` after the first paint that follows `window`
/// mapping, and records when it was. The CI smoke run greps for it.
fn log_first_frame(
    window: &adw::ApplicationWindow,
    started: Instant,
    exec_ns: Option<i64>,
    painted: Rc<Cell<Option<Instant>>>,
    after: Rc<RefCell<Vec<Hook>>>,
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
            let after = after.clone();
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
                    if let Some(exec_ns) = exec_ns {
                        let since = (glib::monotonic_time() * 1000 - exec_ns) as f64 / 1e6;
                        tracing::info!("first-frame-from-exec {since:.1}");
                    }
                    let hooks = std::mem::take(&mut *after.borrow_mut());
                    if !hooks.is_empty() {
                        glib::idle_add_local_once(move || {
                            for hook in hooks {
                                hook();
                            }
                        });
                    }
                }
            ));
            on_paint.replace(Some(handler));
        }
    ));
    on_map.replace(Some(handler));
}
