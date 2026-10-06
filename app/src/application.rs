//! The GApplication (ML-020): one instance per app-id. A second `sealwax mailto:…` hands its
//! command line to the running instance and exits; launchers activate it over D-Bus
//! (`DBusActivatable=true`: `Activate` and `Open`). The `command-line`, `open` and `activate`
//! handlers build the window synchronously: GApplication shuts down when one of them returns
//! with neither a window nor a hold (ADR-003 amendment, S6). Until the window is presented the
//! GTK thread does nothing but build it. SIGTERM, SIGHUP and SIGINT quit cleanly.

use crate::actions::{self, ActionSet};
use crate::cli;
use crate::compose_actions;
use crate::composer::{self, ComposeRequest};
use crate::config;
use crate::default_handler;
use crate::desktop::{self, DesktopEvent, DesktopWatch};
use crate::engine::{self, EngineOptions};
use crate::engine_link::{EngineLink, Subscription};
use crate::list::{MessageList, Quick, Selected};
use crate::mailbox::{self, Latch, Mailbox};
use crate::menu;
use crate::move_to;
use crate::oauth;
use crate::outbox;
use crate::palette;
use crate::preferences::{self, Preferences, SettingsFile, Update};
use crate::reader::{self, MessageSource, Opened, Position, Reader};
use crate::setup::{self, Again, remove::Removable};
use crate::shortcuts;
use crate::status::{self, Waiting};
use crate::theme::{self, ThemeController};
use crate::triage::{
    self, Controller, Directory, MARK_READ_ON_OPEN, Notice, OpsBackend, Origin, Target,
};
use crate::window::{self, Attention, BannerAction, Pane, Window};
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use libadwaita::subclass::prelude::*;
use sealwax_domain::account::AccountKey;
use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::Keymap;
use sealwax_domain::mailto::{self, Mailto};
use sealwax_domain::model::{FolderId, FolderRole, MessageId};
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Event};
use sealwax_engine::service::Engine;
use sealwax_infrastructure::store::{ViewFilter, ViewScope};
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

/// How long the pending-op count waits after an action, an Undo or a change event, so a burst
/// of them is one count.
const PENDING_COUNT_DELAY: Duration = Duration::from_millis(200);

/// How often the pending-op count is read again while ops wait.
const PENDING_RECHECK: Duration = Duration::from_secs(3);

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
            keyring_missing: Cell::new(false),
            keyring_dismissed: Cell::new(false),
            network: Cell::new(true),
            desktop: RefCell::new(None),
            triage: RefCell::new(None),
            toast: RefCell::new(None),
            mark_read_on_open: Cell::new(MARK_READ_ON_OPEN),
            read_on_leave: RefCell::new((None, Vec::new())),
            read_when_shown: RefCell::new(Vec::new()),
            triage_started: Cell::new(None),
            pending_ops: Cell::new(0),
            pending_queued: Cell::new(false),
            pending_recheck: Cell::new(false),
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
    /// The engine keeps secrets in memory: no Secret Service answered (ML-048).
    keyring_missing: Cell<bool>,
    /// The no-keyring banner was dismissed for this session.
    keyring_dismissed: Cell<bool>,
    /// The desktop reports a network; true until it says otherwise.
    network: Cell<bool>,
    /// logind and the network monitor, watched once this application's engine starts.
    desktop: RefCell<Option<Rc<DesktopWatch>>>,
    /// The mail actions (ML-052 to ML-055), once there is a window.
    triage: RefCell<Option<Controller>>,
    /// The toast of the last action, which a new one replaces.
    toast: RefCell<Option<adw::Toast>>,
    /// Whether a message the reader shows is marked read ([`MARK_READ_ON_OPEN`]; the self-test
    /// turns it off and on).
    mark_read_on_open: Cell<bool>,
    /// Unread messages a conversation shown in the Unread view expanded, marked read once the
    /// selection leaves the message it was shown for.
    read_on_leave: RefCell<(Option<MessageId>, Vec<Target>)>,
    /// Unread messages the reader showed while it was hidden (one pane at a time), marked read
    /// when the reader is shown.
    read_when_shown: RefCell<Vec<Target>>,
    /// When the last mail command's handler ran, and the window's frame then (measurements).
    triage_started: Cell<Option<(Instant, i64)>>,
    /// The ops waiting for a server, for the offline banner.
    pending_ops: Cell<u32>,
    pending_queued: Cell<bool>,
    pending_recheck: Cell<bool>,
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

    /// What the window's banner shows (ML-048), most important first: the keyring, then the
    /// accounts `accounts.toml` lists by the state the engine reported, then the network.
    pub fn waiting(&self) -> Vec<Waiting> {
        let listed = self
            .preferences()
            .map(|prefs| prefs.accounts())
            .unwrap_or_default();
        let known = self.mailbox.accounts();
        let accounts: Vec<status::Account<'_>> = known
            .iter()
            .filter_map(|account| {
                let key = account.key.as_ref()?;
                Some(status::Account {
                    name: &account.name,
                    status: self.mailbox.status(account.id),
                    settings: listed.iter().find(|settings| &settings.key == key)?,
                })
            })
            .collect();
        let uncertain: Vec<String> = outbox::uncertain()
            .into_iter()
            .map(|(_, account)| {
                known
                    .iter()
                    .find(|known| known.id == account)
                    .map(|known| known.name.clone())
                    .unwrap_or_default()
            })
            .collect();
        status::queue(
            &accounts,
            self.keyring_missing.get() && !self.keyring_dismissed.get(),
            self.network.get(),
            self.pending_ops.get() + outbox::waiting_sends(),
            &uncertain,
        )
    }

    /// The accounts that must sign in again, in the sidebar's order: with Microsoft in the
    /// browser (ML-032) or with their password typed again (ML-048).
    pub fn sign_in_needed(&self) -> Vec<Again> {
        self.waiting()
            .into_iter()
            .filter(|waiting| matches!(waiting.attention, Attention::SignIn { .. }))
            .filter_map(|waiting| self.again(waiting.account.as_ref()?))
            .collect()
    }

    /// The account `key` as the setup assistant and the certificate's details need it.
    fn again(&self, key: &AccountKey) -> Option<Again> {
        let settings = self
            .preferences()?
            .accounts()
            .into_iter()
            .find(|settings| &settings.key == key)?;
        let account = self
            .mailbox
            .accounts()
            .into_iter()
            .find(|account| account.key.as_ref() == Some(key))?;
        Some(Again {
            key: key.clone(),
            name: account.name,
            address: settings.address,
            imap: settings.imap,
            smtp: settings.smtp,
        })
    }

    /// One banner shows the most important state and counts the others.
    pub(crate) fn update_banner(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let queue: Vec<Attention> = self
            .waiting()
            .into_iter()
            .map(|waiting| waiting.attention)
            .collect();
        window.show_banner(&queue);
    }

    /// A banner button, for the state the banner shows.
    fn banner_action(self: &Rc<Self>, action: BannerAction) {
        let Some(window) = self.window() else {
            return;
        };
        let again = self
            .waiting()
            .into_iter()
            .next()
            .and_then(|first| self.again(first.account.as_ref()?));
        match (action, again) {
            (BannerAction::SignIn, Some(again)) => {
                setup::sign_in_again(&window, &self.engine, self.microsoft_available(), again);
            }
            (BannerAction::TypePassword, Some(again)) => {
                setup::type_password_again(
                    &window,
                    &self.engine,
                    self.microsoft_available(),
                    again,
                );
            }
            (BannerAction::Details, Some(again)) => {
                let weak = Rc::downgrade(self);
                status::certificate_details(&window, &again, move || {
                    if let Some(state) = weak.upgrade() {
                        state.retry();
                    }
                });
            }
            (BannerAction::Retry, _) => self.retry(),
            (BannerAction::Decide, _) => outbox::decide(self),
            (BannerAction::Dismiss, _) => {
                self.keyring_dismissed.set(true);
                self.update_banner();
            }
            (BannerAction::SignIn | BannerAction::TypePassword | BannerAction::Details, None) => {
                tracing::warn!("a banner button for an account accounts.toml doesn't list");
            }
        }
    }

    /// Retry: reconnect now, and say so.
    fn retry(&self) {
        self.reconnect("retry");
        if let Some(window) = self.window() {
            window.add_toast(adw::Toast::new("Connecting again…"));
        }
    }

    /// Drops every connection and makes it again (ADR-003 amendment, S6), without waiting for
    /// a socket to fail.
    fn reconnect(&self, why: &'static str) {
        match self.engine.send(Command::Reconnect) {
            Ok(()) => tracing::info!(why, "reconnecting"),
            Err(_) => tracing::debug!(why, "no engine to reconnect"),
        }
    }

    /// The machine resumed, or the desktop's network came or went (ML-048): reconnect when
    /// it is back, and show whether it is.
    pub fn desktop_event(&self, event: DesktopEvent) {
        match event {
            DesktopEvent::Resumed => self.reconnect("resumed"),
            DesktopEvent::Network(available) => {
                let was = self.network.replace(available);
                if available && !was {
                    self.reconnect("network back");
                }
                if available != was {
                    tracing::info!(available, "the desktop's network changed");
                    self.update_banner();
                }
            }
        }
    }

    /// logind's resume and the network monitor, for the engine this application started.
    fn watch_desktop(self: &Rc<Self>) {
        if self.desktop.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let watch = desktop::watch(move |event| {
            if let Some(state) = weak.upgrade() {
                state.desktop_event(event);
            }
        });
        self.desktop.replace(Some(watch));
    }

    /// When the first window's first frame was painted.
    pub fn first_frame(&self) -> Option<Instant> {
        self.first_frame.get()
    }

    /// The `mailto:` links handed to the composer so far, oldest first (`composer::requests`).
    pub fn compose_requests(&self) -> Vec<Mailto> {
        composer::requests()
            .into_iter()
            .filter_map(|request| match request {
                ComposeRequest::Mailto(mailto) => Some(mailto),
                _ => None,
            })
            .collect()
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
    /// Open composer windows save their drafts first (`composer::save_all`).
    pub fn quit(&self, app: &SealwaxApplication) {
        if self.quitting.replace(true) {
            return;
        }
        self.background.take();
        let app = app.clone();
        let finish = move || {
            for window in app.windows() {
                window.destroy();
            }
            app.quit();
        };
        if crate::composer::is_open() {
            glib::spawn_future_local(async move {
                crate::composer::save_all().await;
                finish();
            });
        } else {
            finish();
        }
    }

    fn startup(self: &Rc<Self>, app: &SealwaxApplication) {
        gtk4::Window::set_default_icon_name(ICON_NAME);
        self.start_settings(app);
        // Composer windows draft into the store; the ones open when Sealwax last stopped
        // reopen once the first frame is painted.
        composer::use_store(app);
        let reopen = app.downgrade();
        self.run_after_first_frame(move || {
            if let Some(app) = reopen.upgrade() {
                composer::reopen_drafts(&app);
            }
        });
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
            state.update_banner();
        });
        // The sidebar's account states and the banner follow whatever engine the link has: the
        // one started below, or a self-test's scripted one.
        let weak = Rc::downgrade(self);
        let subscription = self.engine.subscribe(move |event| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if let Some((account, status)) = engine::account_status(event) {
                state.mailbox.set_status(account, status);
            }
            if let Event::KeyringUnavailable = event {
                tracing::info!("secrets stay in memory: no keyring answered");
                state.keyring_missing.set(true);
                state.update_banner();
            }
            if let Event::OlderLoaded {
                folder,
                fetched,
                complete,
            } = event
                && let Some(list) = state.list()
            {
                list.older_loaded(*folder, *fetched, *complete);
            }
            if let Event::OpsRefused {
                account,
                change,
                messages,
            } = event
                && let Some(triage) = state.triage()
            {
                tracing::info!(account = account.0, messages, "the server refused changes");
                let notice = triage.refused(*account, *change, *messages);
                state.show_notice(&notice, false);
            }
        });
        self.engine_events.replace(Some(subscription));
        outbox::install(self);
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
                            state.watch_desktop();
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
        let uris: Vec<&str> = parsed.uris.iter().map(String::as_str).collect();
        self.compose_links(app, &uris);
        glib::ExitCode::SUCCESS
    }

    /// `mailto:` links from the command line or D-Bus `Open`. With no main window shown yet
    /// (a link from a browser, or background mode after the window closed) they open composer
    /// windows alone (M6); otherwise, or when none of them opened, the main window shows too.
    fn compose_links(self: &Rc<Self>, app: &SealwaxApplication, uris: &[&str]) {
        let alone = self.window().is_none() && !uris.is_empty();
        let before = crate::composer::composers().len();
        let window = (!alone).then(|| self.window_or_new(app));
        for uri in uris {
            compose(app, uri);
        }
        if alone && crate::composer::composers().len() > before {
            return;
        }
        let window = window.unwrap_or_else(|| self.window_or_new(app));
        self.present(&window);
    }

    fn open(self: &Rc<Self>, app: &SealwaxApplication, files: &[gio::File]) {
        let uris: Vec<String> = files
            .iter()
            .map(|file| file.uri().to_string())
            .filter(|uri| {
                let link = mailto::is_mailto(uri);
                if !link {
                    tracing::warn!("ignored an open request that is not a mailto: link");
                }
                link
            })
            .collect();
        let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
        self.compose_links(app, &uris);
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
        let weak = Rc::downgrade(self);
        app_actions.connect(ActionId::DefaultMailApp, move || {
            if let Some(state) = weak.upgrade()
                && let Some(window) = state.window()
            {
                default_handler::run(&window, &state.config.app_id);
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
            self.send_microsoft_client();
            self.send_storage();
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
                list.set_quick_tooltips(quick_tooltips(&keymap));
            }
            if let Some(reader) = self.reader() {
                reader.update_keys(&keymap);
            }
            self.refresh_commands();
        }
        if update.accounts_changed {
            self.accounts_read.set(true);
            self.send_accounts();
            self.update_banner();
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
        let weak = Rc::downgrade(self);
        win_actions.connect(ActionId::CommandPalette, move || {
            if let (Some(window), Some(state)) = (target.upgrade(), weak.upgrade()) {
                let titles = state.command_titles();
                palette::present_titled(&window, &prefs.keymap(), &move |id| {
                    titles
                        .iter()
                        .find(|(candidate, _)| *candidate == id)
                        .map(|(_, title)| title.clone())
                });
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
        window.connect_banner(move |action| {
            if let Some(state) = weak.upgrade() {
                state.banner_action(action);
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
        compose_actions::install(self, window, &win_actions);
        self.win_actions.replace(Some(win_actions));
    }

    /// The message list (ML-026) in the window's list slot. It shows the folder picked in the
    /// sidebar, which the store fills and keeps current; its keys move and open the selection.
    fn install_list(self: &Rc<Self>, window: &Window, win_actions: &ActionSet, keymap: &Keymap) {
        let list = MessageList::new(self.mailbox.clone());
        list.set_first_rows(self.first_rows.clone());
        list.install_keys(keymap);
        list.set_quick_tooltips(quick_tooltips(keymap));
        window.set_list(list.widget());
        GtkWindowExt::set_focus(window.window(), Some(list.list_view()));
        list.keep_focus(window.window());
        self.install_triage(window, &list, win_actions);

        let target = list.clone();
        win_actions.connect(ActionId::NextMessage, move || target.select_next());
        let target = list.clone();
        win_actions.connect(ActionId::PreviousMessage, move || target.select_previous());
        let target = list.clone();
        win_actions.connect(ActionId::OpenMessage, move || target.open_selected());
        let target = list.clone();
        win_actions.connect(ActionId::Search, move || target.focus_search());
        let pane = window.clone();
        let weak = Rc::downgrade(self);
        list.connect_message_opened(move |message| {
            let edited = weak
                .upgrade()
                .is_some_and(|state| compose_actions::open_if_draft(&state, &pane, message));
            if !edited {
                pane.show_pane(Pane::Reader);
            }
        });
        let weak = Rc::downgrade(self);
        list.connect_message_selected(move |id| {
            if let Some(state) = weak.upgrade() {
                state.leave_opened(Some(id));
                state.set_has_selection(true);
                state.refresh_commands();
            }
        });
        let weak = Rc::downgrade(self);
        let shown = window.clone();
        list.connect_selection_cleared(move || {
            if let Some(state) = weak.upgrade() {
                state.leave_opened(None);
                state.set_has_selection(false);
                if shown.reader_page() == "batch" {
                    shown.clear_reader();
                }
                state.refresh_commands();
            }
        });
        let weak = Rc::downgrade(self);
        let shown = window.clone();
        list.connect_batch_selected(move |count| {
            if let Some(state) = weak.upgrade() {
                state.leave_opened(None);
                if state.reader_shows.take().is_some()
                    && let Some(reader) = state.reader()
                {
                    reader.clear();
                    reader.set_position(None);
                }
                shown.show_batch(count);
                state.set_has_selection(true);
                state.refresh_commands();
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

        // A folder (not All Inboxes) is synced now and kept current while it is shown (ML-042);
        // at the end of its first-sync window the list asks for older mail (ML-043).
        let (mailbox, engine) = (self.mailbox.clone(), self.engine.clone());
        let shown = list.clone();
        window.connect_folder_selected(move |_, key| {
            let scope = mailbox.scope_for_key(key);
            if let Some(ViewScope::Folder(folder)) = scope {
                let _ = engine.send(Command::FolderOpened { folder });
            }
            shown.show(scope);
        });
        let weak = Rc::downgrade(self);
        list.connect_older_wanted(move |folder| {
            if let Some(state) = weak.upgrade()
                && state.engine.send(Command::LoadOlder { folder }).is_err()
                && let Some(list) = state.list()
            {
                list.older_loaded(folder, 0, false);
            }
        });
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

    /// The mail actions (ML-052 to ML-055): the registry's commands on the list's selection,
    /// the rows' quick actions and the sidebar's Mark Folder as Read, through a
    /// [`triage::Controller`] over the store's op queue ([`OpsBackend`]); toasts for its
    /// notices.
    fn install_triage(self: &Rc<Self>, window: &Window, list: &MessageList, actions: &ActionSet) {
        let directory: Rc<dyn Directory> = self.mailbox.clone();
        let controller = Controller::new(
            Rc::new(OpsBackend::new(self.mailbox.clone())),
            directory,
            self.engine.clone(),
        );
        let weak = Rc::downgrade(self);
        controller.connect_notice(move |notice| {
            if let Some(state) = weak.upgrade() {
                state.show_notice(notice, true);
            }
        });
        let weak = Rc::downgrade(self);
        controller.connect_changed(move || {
            if let Some(state) = weak.upgrade() {
                if state.triage().is_some_and(|triage| triage.can_undo()) {
                    outbox::forget_last();
                }
                state.refresh_commands();
            }
        });
        let weak = Rc::downgrade(self);
        controller.connect_answered(move || {
            if let Some(state) = weak.upgrade() {
                state.queue_pending_count();
            }
        });
        for id in triage::COMMANDS {
            let weak = Rc::downgrade(self);
            actions.connect(id, move || {
                if let Some(state) = weak.upgrade() {
                    state.triage_command(id);
                }
            });
        }
        let weak = Rc::downgrade(self);
        actions.connect(ActionId::MarkFolderRead, move || {
            if let Some(state) = weak.upgrade()
                && let Some(list) = state.list()
                && list.query().is_empty()
            {
                state.mark_read(list.scope());
            }
        });
        let target = list.clone();
        actions.connect(ActionId::Select, move || target.toggle_selection());
        let weak = Rc::downgrade(self);
        actions.connect(ActionId::Undo, move || {
            if let Some(state) = weak.upgrade()
                && !outbox::undo_last(&state)
                && let Some(triage) = state.triage()
            {
                triage.undo(None);
            }
        });
        // Escape leaves the select mode wherever the focus is, as it does in the list: on the
        // batch page, or nowhere once a button there went insensitive under it. Text fields and
        // dialogs take their Escape first.
        let escape = gtk4::EventControllerKey::new();
        let target = list.clone();
        escape.connect_key_pressed(move |_, key, _, state| {
            if key == gtk4::gdk::Key::Escape && state.is_empty() && target.is_select_mode() {
                target.leave_select_mode();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.window().add_controller(escape);
        let weak = Rc::downgrade(self);
        list.connect_quick_action(move |id, target| {
            if let Some(state) = weak.upgrade() {
                state.mark_started();
                state.triage_targets(id, vec![target]);
            }
        });
        let (applies, activate) = (Rc::downgrade(self), Rc::downgrade(self));
        window.sidebar().connect_mark_read(
            move |key| {
                applies.upgrade().is_some_and(|state| {
                    let scope = state.mailbox.scope_for_key(key);
                    state.unread_in(scope) > 0
                })
            },
            move |key| {
                if let Some(state) = activate.upgrade() {
                    let scope = state.mailbox.scope_for_key(key);
                    state.mark_read(scope);
                }
            },
        );
        let weak = Rc::downgrade(self);
        self.mailbox.connect_event(move |_| {
            if let Some(state) = weak.upgrade() {
                state.refresh_commands();
                state.queue_pending_count();
            }
        });
        // Mark Folder as Read follows the folder shown, also when no selection changes with it.
        let weak = Rc::downgrade(self);
        window.connect_folder_selected(move |_, _| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(state) = weak.upgrade() {
                    state.refresh_commands();
                }
            });
        });
        self.triage.replace(Some(controller));
        self.queue_pending_count();
    }

    /// Counts the ops waiting for a server, for the offline banner's "3 waiting", a moment
    /// after the last action, Undo or change event: one read per account with a server, on the
    /// store's reader, the replies awaited on the main loop.
    fn queue_pending_count(self: &Rc<Self>) {
        if self.pending_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(PENDING_COUNT_DELAY, move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            state.pending_queued.set(false);
            let Some(triage) = state.triage() else {
                return;
            };
            let backend = triage.backend();
            let counts: Vec<_> = state
                .mailbox
                .accounts()
                .into_iter()
                .filter(|account| account.key.is_some())
                .map(|account| backend.pending(account.id))
                .collect();
            let weak = Rc::downgrade(&state);
            glib::spawn_future_local(async move {
                let mut total = 0;
                for count in counts {
                    total += count.await.unwrap_or(0);
                }
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.pending_ops.replace(total) != total {
                    state.update_banner();
                }
                // An op the server carried out without changing what is shown (a flag the
                // overlay showed already) makes no change event: count again while any wait.
                if total > 0 && !state.pending_recheck.replace(true) {
                    let weak = Rc::downgrade(&state);
                    glib::timeout_add_local_once(PENDING_RECHECK, move || {
                        if let Some(state) = weak.upgrade() {
                            state.pending_recheck.set(false);
                            state.queue_pending_count();
                        }
                    });
                }
            });
        });
    }

    /// The ops waiting for a server, as last counted.
    pub fn pending_ops(&self) -> u32 {
        self.pending_ops.get()
    }

    /// The mail actions, once there is a window.
    pub fn triage(&self) -> Option<Controller> {
        self.triage.borrow().clone()
    }

    /// Whether a message shown in the reader is marked read; the self-test switches it.
    pub fn set_mark_read_on_open(&self, on: bool) {
        self.mark_read_on_open.set(on);
    }

    /// When the last mail command's handler ran, and the window's frame counter then.
    pub fn triage_started(&self) -> Option<(Instant, i64)> {
        self.triage_started.get()
    }

    fn mark_started(&self) {
        let frame = self
            .window()
            .and_then(|window| window.window().frame_clock())
            .map_or(0, |clock| clock.frame_counter());
        self.triage_started.set(Some((Instant::now(), frame)));
    }

    /// A triage command on the selected messages; selected rows far down a range load first.
    fn triage_command(self: &Rc<Self>, id: ActionId) {
        self.mark_started();
        let Some(list) = self.list() else {
            return;
        };
        if id == ActionId::MoveTo {
            self.open_move_to(&list);
            return;
        }
        match list.selected_targets() {
            Ok(targets) => self.triage_targets(id, targets),
            Err(_) => {
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let targets = list.wait_targets().await;
                    let Some(state) = weak.upgrade() else {
                        return;
                    };
                    match targets {
                        Some(targets) => state.triage_targets(id, targets),
                        None => state.show_notice(
                            &Notice::plain(
                                "Some selected messages didn't load in time; nothing changed"
                                    .to_owned(),
                            ),
                            true,
                        ),
                    }
                });
            }
        }
    }

    fn triage_targets(&self, id: ActionId, targets: Vec<Target>) {
        let (Some(triage), Some(list)) = (self.triage(), self.list()) else {
            return;
        };
        let action = match id {
            ActionId::Archive => Triage::Archive,
            ActionId::Delete => Triage::Delete,
            ActionId::Junk => Triage::Junk,
            ActionId::Star => triage::star_action(&targets),
            ActionId::MarkRead => Triage::Read(true),
            ActionId::MarkUnread => Triage::Read(false),
            _ => return,
        };
        if matches!(action, Triage::Read(_)) {
            self.read_on_leave
                .borrow_mut()
                .1
                .retain(|pending| targets.iter().all(|target| target.id != pending.id));
        }
        let ids: Vec<MessageId> = targets.iter().map(|target| target.id).collect();
        list.advance_after(&ids);
        triage.queue(action, targets, Origin::User);
    }

    /// Move to…: the picker over the account's folders; insensitive across accounts.
    fn open_move_to(self: &Rc<Self>, list: &MessageList) {
        let (Some(window), Ok(targets)) = (self.window(), list.selected_targets()) else {
            return;
        };
        let folders = self.mailbox.folders();
        let Ok(choices) = triage::move_choices(&targets, &folders) else {
            return;
        };
        if choices.is_empty() {
            return;
        }
        let weak = Rc::downgrade(self);
        let count = targets.len() as u32;
        move_to::present(window.window(), &choices, count, move |folder| {
            if let Some(state) = weak.upgrade() {
                state.move_targets(folder, targets.clone());
            }
        });
    }

    fn move_targets(&self, folder: FolderId, targets: Vec<Target>) {
        let (Some(triage), Some(list)) = (self.triage(), self.list()) else {
            return;
        };
        self.mark_started();
        let ids: Vec<MessageId> = targets.iter().map(|target| target.id).collect();
        list.advance_after(&ids);
        triage.queue(Triage::MoveTo(folder), targets, Origin::User);
    }

    /// The folders Mark Folder as Read marks for what the list shows, and its name: a folder,
    /// or every account's Inbox for All Inboxes.
    fn read_folders(&self, scope: Option<ViewScope>) -> (Vec<FolderId>, String) {
        let folders = self.mailbox.folders();
        match scope {
            Some(ViewScope::Folder(id)) => (
                vec![id],
                folders
                    .iter()
                    .find(|folder| folder.id == id)
                    .map(triage::folder_title)
                    .unwrap_or_default(),
            ),
            Some(ViewScope::UnifiedInbox) => (
                folders
                    .iter()
                    .filter(|folder| folder.role == FolderRole::Inbox)
                    .map(|folder| folder.id)
                    .collect(),
                "All Inboxes".to_owned(),
            ),
            None => (Vec::new(), String::new()),
        }
    }

    /// Unread messages the sidebar counts for what `scope` shows.
    fn unread_in(&self, scope: Option<ViewScope>) -> u32 {
        let (ids, _) = self.read_folders(scope);
        self.mailbox
            .folders()
            .iter()
            .filter(|folder| ids.contains(&folder.id))
            .map(|folder| folder.unread)
            .sum()
    }

    fn mark_read(&self, scope: Option<ViewScope>) {
        let Some(triage) = self.triage() else {
            return;
        };
        let (folders, name) = self.read_folders(scope);
        if !folders.is_empty() {
            self.mark_started();
            triage.mark_folders_read(folders, name);
        }
    }

    /// Commands' titles where they differ from the registry's, for the palette.
    fn command_titles(&self) -> Vec<(ActionId, String)> {
        let Some(list) = self.list() else {
            return Vec::new();
        };
        let targets = loaded_targets(&list);
        let folders = self.mailbox.folders();
        triage::COMMANDS
            .into_iter()
            .map(|id| (id, triage::availability(id, &targets, &folders).title))
            .filter(|(id, title)| *title != id.title())
            .map(|(id, title)| (id, title.to_owned()))
            .collect()
    }

    /// Enables the mail commands where they apply to the selection, with their titles and
    /// reasons in the reader's toolbar and on the batch page (ML-052).
    pub(crate) fn refresh_commands(&self) {
        let (Some(list), Some(window)) = (self.list(), self.window()) else {
            return;
        };
        let actions = self.win_actions.borrow();
        let Some(actions) = actions.as_ref() else {
            return;
        };
        let keymap = self
            .preferences()
            .map(|prefs| prefs.keymap())
            .unwrap_or_default();
        let targets = loaded_targets(&list);
        let folders = self.mailbox.folders();
        let reader = self.reader();
        for id in triage::COMMANDS {
            let availability = triage::availability(id, &targets, &folders);
            actions.set_applies(id, availability.enabled());
            if let Some(reader) = &reader {
                reader.set_command(id, availability.title, availability.reason);
            }
            window.batch_page().set_command(
                id,
                availability.title,
                &reader::tooltip(&keymap, id, availability.title, availability.reason),
            );
        }
        let one = matches!(list.reported(), Selected::One(_));
        for id in [
            ActionId::OpenMessage,
            ActionId::Print,
            ActionId::ShowOriginal,
            ActionId::SaveMessage,
        ] {
            actions.set_applies(id, one);
        }
        actions.set_applies(ActionId::Select, list.model().total() > 0);
        actions.set_applies(
            ActionId::MarkFolderRead,
            list.query().is_empty() && self.unread_in(list.scope()) > 0,
        );
        let undo = self.triage().is_some_and(|triage| triage.can_undo()) || outbox::can_undo();
        actions.set_applies(ActionId::Undo, undo);
        compose_actions::refresh(actions, &list, &targets, &folders);
    }

    /// Shows `notice` as a toast; with `replaces`, it takes the last action's toast's place
    /// (ADR-015: only the last action is undoable from the toast).
    fn show_notice(self: &Rc<Self>, notice: &Notice, replaces: bool) {
        let toast = adw::Toast::builder()
            .title(notice.text.as_str())
            .use_markup(false)
            .timeout(notice.shown.as_millis().div_ceil(1000).max(1) as u32)
            .priority(adw::ToastPriority::High)
            .build();
        if let Some(batch) = notice.undo {
            toast.set_button_label(Some("Undo"));
            let weak = Rc::downgrade(self);
            toast.connect_button_clicked(move |_| {
                if let Some(state) = weak.upgrade()
                    && let Some(triage) = state.triage()
                {
                    triage.undo(Some(batch));
                }
            });
        }
        self.show_toast(&toast, replaces);
    }

    /// Shows `toast` in the main window; with `replaces`, it takes the last action's toast's
    /// place (M6: the outbox's Undo-send toast is the last action's too).
    pub fn show_toast(&self, toast: &adw::Toast, replaces: bool) {
        let Some(window) = self.window() else {
            return;
        };
        if replaces && let Some(previous) = self.toast.take() {
            previous.dismiss();
        }
        window.add_toast(toast.clone());
        if replaces {
            self.toast.replace(Some(toast.clone()));
        }
    }

    /// The reader showed message bodies of the selected message's conversation (ML-057): the
    /// unread ones are read now, or in the Unread view once the selection leaves the message, or
    /// when a hidden reader is shown ([`MARK_READ_ON_OPEN`]).
    fn messages_opened(&self, opened: &Opened) {
        if !self.mark_read_on_open.get() || self.reader_shows.get() != Some(opened.selected) {
            return;
        }
        let targets: Vec<Target> = opened
            .messages
            .iter()
            .filter(|message| !message.flags.is_seen())
            .map(|message| Target {
                id: message.id,
                folder: message.folder,
                flags: message.flags,
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let visible = self
            .reader()
            .is_some_and(|reader| reader.widget().is_mapped());
        if visible {
            self.read_opened(opened.selected, targets);
        } else {
            self.read_when_shown.borrow_mut().extend(targets);
        }
    }

    /// The reader came on screen (one pane at a time): the messages it showed are read now.
    fn reader_mapped(&self) {
        let waiting = self.read_when_shown.take();
        if let Some(selected) = self.reader_shows.get()
            && !waiting.is_empty()
        {
            self.read_opened(selected, waiting);
        }
    }

    fn read_opened(&self, selected: MessageId, targets: Vec<Target>) {
        let (Some(list), Some(triage)) = (self.list(), self.triage()) else {
            return;
        };
        if list.filter() == ViewFilter::Unread {
            let mut leave = self.read_on_leave.borrow_mut();
            leave.0 = Some(selected);
            leave.1.extend(targets);
        } else {
            triage.queue(Triage::Read(true), targets, Origin::Opened);
        }
    }

    /// The selection left the message shown in the Unread view: what it opened is read now.
    fn leave_opened(&self, now: Option<MessageId>) {
        self.read_when_shown.borrow_mut().clear();
        let opened_for = self.read_on_leave.borrow().0;
        if now.is_some() && now == opened_for {
            return;
        }
        let (_, targets) = self.read_on_leave.replace((None, Vec::new()));
        if !targets.is_empty()
            && let Some(triage) = self.triage()
        {
            triage.queue(Triage::Read(true), targets, Origin::Opened);
        }
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
            self.microsoft_client_id(),
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
        self.send_storage();
        self.send_accounts();
    }

    /// Hands `config.toml`'s `[storage]` to the engine this application started, once the file
    /// has been read: the disk cap is enforced as soon as it arrives.
    fn send_storage(&self) {
        let Some(prefs) = self.preferences().filter(|prefs| prefs.is_loaded()) else {
            return;
        };
        if self.engine_service.borrow().is_none() {
            return;
        }
        let storage = prefs.config().storage;
        if self.engine.send(Command::Storage { storage }).is_err() {
            tracing::warn!("the engine stopped listening");
        }
    }

    /// Hands the Microsoft client ID in use to the engine this application started.
    fn send_microsoft_client(&self) {
        if self.engine_service.borrow().is_none() {
            return;
        }
        let client_id = self.microsoft_client_id();
        if self
            .engine
            .send(Command::MicrosoftClient { client_id })
            .is_err()
        {
            tracing::warn!("the engine stopped listening");
        }
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

    /// Hands the accounts from `accounts.toml` to the engine this application started. Their
    /// states come from the engine: an account whose sync starts or restarts reports syncing,
    /// and one already running keeps its state (M3: `Accounts` no longer syncs every account
    /// again).
    fn send_accounts(&self) {
        if !self.accounts_read.get() || self.engine_service.borrow().is_none() {
            return;
        }
        let Some(prefs) = self.preferences() else {
            return;
        };
        let accounts = prefs.accounts();
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
        reader.connect_engine(&self.engine);
        window.set_reader_toolbar(&reader.toolbar());
        // The batch page stands in for the reader: the Reader scope's keys work there too.
        reader
            .keys()
            .attach(&window.batch_page().widget, gtk4::PropagationPhase::Bubble);

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
        reader.connect_opened(move |opened| {
            if let Some(state) = weak.upgrade() {
                state.messages_opened(opened);
            }
        });
        let weak = Rc::downgrade(self);
        reader.widget().connect_map(move |_| {
            if let Some(state) = weak.upgrade() {
                state.reader_mapped();
            }
        });
        let weak = Rc::downgrade(self);
        self.mailbox.connect_event(move |event| {
            if let Some(state) = weak.upgrade() {
                state.update_position();
                if let Some(reader) = state.reader() {
                    reader.store_changed(event);
                }
            }
        });
        if let Some(actions) = self.win_actions.borrow().as_ref() {
            crate::reader::commands::connect(actions, &reader);
        }
        compose_actions::connect_reader(&reader, window);
        self.reader.replace(Some(reader));
        self.refresh_commands();
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
        self.update_banner();
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

/// A `mailto:` link to write a message from (PRD C8): its safe fields go to a composer window
/// (`domain::mailto` drops `attach` and the rest); only the request's kind is logged.
fn compose(app: &SealwaxApplication, uri: &str) {
    match mailto::parse(uri) {
        Ok(mailto) => composer::open(app, ComposeRequest::Mailto(mailto)),
        Err(error) => tracing::warn!("{error}"),
    }
}

/// The rows' quick actions' tooltips, with their keys.
fn quick_tooltips(keymap: &Keymap) -> Vec<(Quick, String)> {
    Quick::ALL
        .into_iter()
        .map(|quick| {
            let id = quick.action();
            (quick, reader::tooltip(keymap, id, id.title(), None))
        })
        .collect()
}

/// The selected messages whose rows are loaded.
fn loaded_targets(list: &MessageList) -> Vec<Target> {
    list.selected_positions()
        .into_iter()
        .filter_map(|position| list.target_at(position))
        .collect()
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
