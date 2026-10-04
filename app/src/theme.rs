//! The live Omarchy theme (ADR-004, ADR-019; ML-018, ML-019). Reads the palette, the font
//! families, `shell.toml` and Hyprland's rounding off the GTK thread, installs `style.css` and
//! the generated variables at application priority, forces libadwaita's dark or light scheme
//! from the palette's mode, and follows every change: a theme switch (`current/`), `omarchy
//! font set` (`~/.config/fontconfig/`) and `omarchy display text size` (`shell.toml` in
//! `~/.config/omarchy/`). Outside Omarchy two built-in palettes follow the system's dark
//! preference. Subscribers such as WebViews hear about every change through
//! [`ThemeController::connect_changed`].

use crate::monitor::{DirWatch, TriggerRule};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use sealwax_domain::theme::{
    FontFamilies, Mode, Palette, ShellValues, Theme, ThemeInputs, ThemeOptions, ThemeSource,
};
use sealwax_infrastructure::omarchy::{self, OmarchyPaths, OmarchyTheme, Readout, Request};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long startup waits for the first read before the first window (ML-018: ≤ 500 ms).
pub const STARTUP_WAIT: Duration = Duration::from_millis(500);

const STYLE_CSS: &str = include_str!("style.css");

type Listener = Rc<dyn Fn(&Theme)>;

/// A [`ThemeController::connect_changed`] subscription, for [`ThemeController::disconnect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

/// Which request last asked for each part; older answers are dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Generations {
    palette: u64,
    shell: u64,
    rounding: u64,
    fonts: u64,
}

impl Generations {
    fn bump(&mut self, request: Request) {
        let parts = [
            (request.palette, &mut self.palette),
            (request.shell, &mut self.shell),
            (request.rounding, &mut self.rounding),
            (request.fonts, &mut self.fonts),
        ];
        for (asked, generation) in parts {
            if asked {
                *generation += 1;
            }
        }
    }
}

struct Answer {
    generations: Generations,
    readout: Readout,
}

pub struct ThemeController {
    paths: OmarchyPaths,
    options: Cell<ThemeOptions>,
    omarchy: RefCell<Option<OmarchyTheme>>,
    shell: RefCell<ShellValues>,
    fonts: RefCell<FontFamilies>,
    rounding: Cell<Option<u32>>,
    theme: RefCell<Rc<Theme>>,
    installed: Cell<bool>,
    provider: gtk::CssProvider,
    style: gtk::CssProvider,
    css_errors: Rc<Cell<u32>>,
    forced: Cell<Option<Mode>>,
    dark_handler: RefCell<Option<glib::SignalHandlerId>>,
    requested: Cell<Generations>,
    answers: mpsc::Receiver<Answer>,
    sender: mpsc::Sender<Answer>,
    wake: async_channel::Sender<()>,
    listeners: RefCell<Vec<(ListenerId, Listener)>>,
    next_listener: Cell<u64>,
    watches: RefCell<Vec<Rc<DirWatch>>>,
    reloads: Cell<u32>,
}

impl ThemeController {
    /// Reads the live theme, waiting at most `wait` on this thread for it, installs the
    /// stylesheets on the default display and follows every change. Call once from the
    /// application's `startup` handler (GTK must be initialised) and keep the result alive.
    /// A read that takes longer than `wait` is applied when it arrives; until then the
    /// built-in palette shows.
    pub fn start(options: ThemeOptions, wait: Duration) -> Rc<Self> {
        Self::start_with(OmarchyPaths::from_env(), options, wait)
    }

    /// [`start`](Self::start) with explicit paths, for headless checks.
    pub fn start_with(paths: OmarchyPaths, options: ThemeOptions, wait: Duration) -> Rc<Self> {
        let display = gdk::Display::default().expect("GTK is initialised before the theme");
        let css_errors = Rc::new(Cell::new(0));
        let style = provider(&css_errors, "style.css");
        style.load_from_string(STYLE_CSS);
        let generated = provider(&css_errors, "theme");
        for provider in [&style, &generated] {
            gtk::style_context_add_provider_for_display(
                &display,
                provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        let (sender, answers) = mpsc::channel();
        let (wake, woken) = async_channel::unbounded();
        let defaults =
            ThemeInputs::builtin(Mode::from_dark(adw::StyleManager::default().is_dark()));
        let this = Rc::new(Self {
            paths,
            options: Cell::new(options),
            omarchy: RefCell::new(None),
            shell: RefCell::new(ShellValues::new()),
            fonts: RefCell::new(FontFamilies::default()),
            rounding: Cell::new(None),
            theme: RefCell::new(Rc::new(Theme::new(&defaults, &options))),
            installed: Cell::new(false),
            provider: generated,
            style,
            css_errors,
            forced: Cell::new(None),
            dark_handler: RefCell::new(None),
            requested: Cell::new(Generations::default()),
            answers,
            sender,
            wake,
            listeners: RefCell::new(Vec::new()),
            next_listener: Cell::new(0),
            watches: RefCell::new(Vec::new()),
            reloads: Cell::new(0),
        });

        let started = Instant::now();
        this.request(Request::ALL);
        match this.answers.recv_timeout(wait) {
            Ok(answer) => {
                this.merge(answer);
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                tracing::info!("theme read in {ms:.1} ms");
            }
            Err(_) => tracing::warn!(
                "theme not read within {} ms; showing the built-in palette until it is",
                wait.as_millis()
            ),
        }
        this.apply();

        let weak = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            while woken.recv().await.is_ok() {
                let Some(this) = weak.upgrade() else {
                    break;
                };
                this.drain();
            }
        });

        let weak = Rc::downgrade(&this);
        let handler = adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(this) = weak.upgrade()
                && this.theme.borrow().source == ThemeSource::Builtin
            {
                this.apply();
            }
        });
        this.dark_handler.replace(Some(handler));

        this.watch();
        this
    }

    /// The theme as last applied.
    pub fn theme(&self) -> Rc<Theme> {
        self.theme.borrow().clone()
    }

    pub fn paths(&self) -> &OmarchyPaths {
        &self.paths
    }

    pub fn options(&self) -> ThemeOptions {
        self.options.get()
    }

    /// Applies `config.toml`'s choices; nothing is read again.
    pub fn set_options(&self, options: ThemeOptions) {
        if options != self.options.get() {
            self.options.set(options);
            self.apply();
        }
    }

    /// Calls `listener` after every change, with the new theme: restyle WebViews here (their
    /// stylesheets come from [`Theme::webview_css`]). GTK widgets restyle on their own.
    pub fn connect_changed(&self, listener: impl Fn(&Theme) + 'static) -> ListenerId {
        let id = ListenerId(self.next_listener.get());
        self.next_listener.set(id.0 + 1);
        self.listeners.borrow_mut().push((id, Rc::new(listener)));
        id
    }

    pub fn disconnect(&self, id: ListenerId) {
        self.listeners
            .borrow_mut()
            .retain(|(known, _)| *known != id);
    }

    /// Reads everything again, as at startup, without waiting.
    pub fn reload(&self) {
        self.request(Request::ALL);
    }

    /// Completed applications, the first and unchanged reloads included (for self-tests).
    pub fn reloads(&self) -> u32 {
        self.reloads.get()
    }

    /// CSS parsing errors reported for `style.css` and the generated stylesheet.
    pub fn css_errors(&self) -> u32 {
        self.css_errors.get()
    }

    fn request(&self, request: Request) {
        let mut requested = self.requested.get();
        requested.bump(request);
        self.requested.set(requested);
        let paths = self.paths.clone();
        let sender = self.sender.clone();
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new()
            .name("sealwax-theme".into())
            .spawn(move || {
                let readout = omarchy::read(&paths, request);
                let answer = Answer {
                    generations: requested,
                    readout,
                };
                if sender.send(answer).is_ok() {
                    let _ = wake.try_send(());
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "could not start the theme reader");
        }
    }

    fn drain(&self) {
        let mut merged = false;
        while let Ok(answer) = self.answers.try_recv() {
            merged |= self.merge(answer);
        }
        if merged {
            self.apply();
        }
    }

    /// Takes the parts of `answer` that no newer request has asked for since.
    fn merge(&self, answer: Answer) -> bool {
        let current = self.requested.get();
        let asked = answer.generations;
        let readout = answer.readout;
        let mut merged = false;
        if let Some(palette) = readout.palette
            && asked.palette == current.palette
        {
            self.omarchy.replace(palette);
            merged = true;
        }
        if let Some(shell) = readout.shell
            && asked.shell == current.shell
        {
            self.shell.replace(shell);
            merged = true;
        }
        if let Some(rounding) = readout.rounding
            && asked.rounding == current.rounding
        {
            self.rounding.set(rounding);
            merged = true;
        }
        if let Some(fonts) = readout.fonts
            && asked.fonts == current.fonts
        {
            self.fonts.replace(fonts);
            merged = true;
        }
        merged
    }

    fn apply(&self) {
        let started = Instant::now();
        let omarchy = self.omarchy.borrow().clone();
        let (palette, source) = match omarchy {
            Some(theme) => (
                Palette::from_resolved(&theme.resolved),
                ThemeSource::Omarchy(theme.name),
            ),
            None => {
                let style = adw::StyleManager::default();
                if self.forced.take().is_some() {
                    style.set_color_scheme(adw::ColorScheme::Default);
                }
                (
                    Palette::builtin(Mode::from_dark(style.is_dark())),
                    ThemeSource::Builtin,
                )
            }
        };
        let inputs = ThemeInputs {
            source,
            palette,
            shell: self.shell.borrow().clone(),
            fonts: self.fonts.borrow().clone(),
            hyprland_rounding: self.rounding.get(),
        };
        let theme = Theme::new(&inputs, &self.options.get());
        self.reloads.set(self.reloads.get() + 1);
        if self.installed.get() && *self.theme.borrow().as_ref() == theme {
            tracing::debug!("theme reloaded; unchanged");
            return;
        }
        self.provider.load_from_string(&theme.css());
        self.installed.set(true);
        let theme = Rc::new(theme);
        // Replaced before the colour scheme changes: that emits `notify::dark`, which applies
        // again only while a built-in palette is in use.
        self.theme.replace(theme.clone());
        if let ThemeSource::Omarchy(_) = theme.source
            && self.forced.get() != Some(theme.mode())
        {
            self.forced.set(Some(theme.mode()));
            adw::StyleManager::default().set_color_scheme(match theme.mode() {
                Mode::Dark => adw::ColorScheme::ForceDark,
                Mode::Light => adw::ColorScheme::ForceLight,
            });
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        tracing::info!(
            theme = theme.name(),
            mode = theme.mode().as_str(),
            radius = theme.radius,
            "theme applied in {ms:.1} ms"
        );
        let listeners: Vec<Listener> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            listener(&theme);
        }
    }

    fn watch(self: &Rc<Self>) {
        let watches = [
            (self.paths.current(), theme_rule(), Request::THEME),
            (
                self.paths.fontconfig.clone(),
                any_change_rule(),
                Request::FONTS,
            ),
            (self.paths.config.clone(), shell_rule(), Request::SHELL),
        ];
        for (dir, rule, request) in watches {
            let weak = Rc::downgrade(self);
            let watch = DirWatch::start(dir, rule, move || {
                if let Some(this) = weak.upgrade() {
                    this.request(request);
                }
            });
            self.watches.borrow_mut().push(watch);
        }
    }
}

impl Drop for ThemeController {
    fn drop(&mut self) {
        if let Some(handler) = self.dark_handler.take() {
            adw::StyleManager::default().disconnect(handler);
        }
        if let Some(display) = gdk::Display::default() {
            for provider in [&self.style, &self.provider] {
                gtk::style_context_remove_provider_for_display(&display, provider);
            }
        }
    }
}

fn provider(errors: &Rc<Cell<u32>>, name: &'static str) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    let errors = errors.clone();
    provider.connect_parsing_error(move |_, section, error| {
        errors.set(errors.get() + 1);
        tracing::warn!(stylesheet = name, section = %section.to_str(), %error, "CSS error");
    });
    provider
}

/// `theme.name` CHANGES_DONE_HINT, or a rename to `theme` (`omarchy-theme-set`'s swap).
/// `current/` appearing or disappearing is handled by [`DirWatch`] itself.
fn theme_rule() -> TriggerRule {
    Box::new(|event, file, other| {
        use gio::FileMonitorEvent as E;
        match event {
            E::ChangesDoneHint => file == "theme.name",
            E::Renamed => other == Some("theme"),
            _ => false,
        }
    })
}

/// Any finished change in the directory (`omarchy font set` rewrites `fonts.conf`).
fn any_change_rule() -> TriggerRule {
    Box::new(|event, _, _| {
        use gio::FileMonitorEvent as E;
        matches!(
            event,
            E::ChangesDoneHint | E::Created | E::Deleted | E::Renamed | E::MovedIn | E::MovedOut
        )
    })
}

/// A finished change to `shell.toml` (`omarchy display text size` moves a new one into place).
fn shell_rule() -> TriggerRule {
    Box::new(|event, file, other| {
        use gio::FileMonitorEvent as E;
        let about_shell = file == "shell.toml" || other == Some("shell.toml");
        about_shell
            && matches!(
                event,
                E::ChangesDoneHint
                    | E::Created
                    | E::Deleted
                    | E::Renamed
                    | E::MovedIn
                    | E::MovedOut
            )
    })
}
