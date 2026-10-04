//! Headless check of ML-022: the action registry in GTK, the command palette, the scoped
//! shortcuts, the primary menu, the keyboard shortcuts dialog and the live settings files.
//! Run it under the wrapper; it exits by itself and prints one line per check:
//!
//!     cargo build -p sealwax --example palette
//!     tools/headless.sh 32 target/debug/examples/palette
//!
//! It keeps its settings in `.local/palette-check/`, never the user's.
//! `SEALWAX_PALETTE_SHOT=<folder>` also saves screenshots of the palette and the dialog there.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib, graphene, gsk};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_app::actions::{self, ActionSet, ScopedShortcuts};
use sealwax_app::preferences::{self, Preferences, SettingsFile, Update};
use sealwax_app::{menu, palette, shortcuts};
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::config::{Config, CornerRadius};
use sealwax_domain::keymap::{Keymap, default_keys_file};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.pixdevsapps.Sealwax.Devel.PaletteCheck";
/// Long enough for a file monitor event, the 150 ms debounce and the read.
const RELOAD_WAIT: Duration = Duration::from_secs(3);
/// Broadway lays out about once a second without a browser attached.
const LAYOUT_WAIT: Duration = Duration::from_millis(1500);

fn main() -> glib::ExitCode {
    if let Err(error) = sealwax_infrastructure::logging::init() {
        eprintln!("Could not initialize logging: {error}");
        return glib::ExitCode::FAILURE;
    }
    if std::env::var("GDK_BACKEND").as_deref() != Ok("broadway") {
        eprintln!("palette check: run it under tools/headless.sh, never on a live display");
        return glib::ExitCode::FAILURE;
    }
    let failures: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(glib::clone!(
        #[strong]
        failures,
        move |app| start(app, failures.clone())
    ));
    app.run_with_args::<&str>(&[]);
    match failures.get() {
        Some(0) => glib::ExitCode::SUCCESS,
        Some(failed) => {
            eprintln!("palette check: {failed} failed");
            glib::ExitCode::FAILURE
        }
        None => {
            eprintln!("palette check: did not finish");
            glib::ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct Report {
    passed: Cell<u32>,
    failed: Cell<u32>,
}

impl Report {
    fn check(&self, name: &str, ok: bool) {
        if ok {
            self.passed.set(self.passed.get() + 1);
            println!("ok   {name}");
        } else {
            self.failed.set(self.failed.get() + 1);
            println!("FAIL {name}");
        }
    }
}

struct Harness {
    app: adw::Application,
    window: adw::ApplicationWindow,
    prefs: Rc<Preferences>,
    app_actions: ActionSet,
    win_actions: ActionSet,
    list: gtk::Box,
    reader: gtk::Box,
    list_keys: ScopedShortcuts,
    reader_keys: ScopedShortcuts,
    menu_button: gtk::MenuButton,
    ran: RefCell<Vec<ActionId>>,
    sidebar: Cell<Option<bool>>,
    palette: RefCell<Option<palette::Palette>>,
    updates: RefCell<Vec<Update>>,
    shots: Option<PathBuf>,
    report: Report,
}

fn stand_in(name: &str) -> gtk::Box {
    let widget = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .focusable(true)
        .build();
    widget.append(&gtk::Label::new(Some(name)));
    widget
}

fn start(app: &adw::Application, failures: Rc<Cell<Option<u32>>>) {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.local/palette-check");
    if let Err(error) = std::fs::remove_dir_all(&root)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("palette check: could not clear {}: {error}", root.display());
    }
    let dirs = SealwaxDirs::under(&root);
    // The config home exists, as `~/.config` does, so its monitor sees `sealwax/` appear at
    // once; a missing path is only checked from time to time.
    if let Some(config_home) = dirs.config.parent()
        && let Err(error) = std::fs::create_dir_all(config_home)
    {
        eprintln!(
            "palette check: could not create {}: {error}",
            config_home.display()
        );
    }
    let prefs = Preferences::start(&dirs);
    let keymap = prefs.keymap();

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Sealwax palette check")
        .default_width(1000)
        .default_height(700)
        .build();
    let app_actions = actions::install_app_actions(app);
    let win_actions = actions::install_window_actions(&window);
    let list = stand_in("Message list");
    let reader = stand_in("Reader");
    let list_keys = actions::install_scoped(&list, Scope::List, &keymap);
    let reader_keys = actions::install_scoped(&reader, Scope::Reader, &keymap);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .primary(true)
        .menu_model(&menu::primary_menu(&keymap))
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&menu_button);
    let panes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    panes.append(&list);
    panes.append(&reader);
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.set_content(Some(&panes));
    window.set_content(Some(&content));
    actions::apply_accels(app, &keymap);

    let harness = Rc::new(Harness {
        app: app.clone(),
        window,
        prefs,
        app_actions,
        win_actions,
        list,
        reader,
        list_keys,
        reader_keys,
        menu_button,
        ran: RefCell::new(Vec::new()),
        sidebar: Cell::new(None),
        palette: RefCell::new(None),
        updates: RefCell::new(Vec::new()),
        shots: std::env::var_os("SEALWAX_PALETTE_SHOT").map(PathBuf::from),
        report: Report::default(),
    });
    connect_actions(&harness);
    let weak = Rc::downgrade(&harness);
    harness.prefs.connect_changed(move |update| {
        let Some(harness) = weak.upgrade() else {
            return;
        };
        if update.keymap_changed {
            let keymap = harness.prefs.keymap();
            actions::apply_accels(&harness.app, &keymap);
            harness.list_keys.update(&keymap);
            harness.reader_keys.update(&keymap);
            harness
                .menu_button
                .set_menu_model(Some(&menu::primary_menu(&keymap)));
        }
        for problem in &update.problems {
            println!("     toast: {problem}");
        }
        harness.updates.borrow_mut().push(update.clone());
    });
    harness.window.present();

    glib::timeout_add_local_once(
        Duration::from_secs(60),
        glib::clone!(
            #[weak]
            app,
            move || {
                eprintln!("palette check: timed out");
                app.quit();
            }
        ),
    );
    glib::spawn_future_local(async move {
        let failed = harness.run().await;
        failures.set(Some(failed));
        harness.app.quit();
    });
}

fn record(weak: &Weak<Harness>, id: ActionId) {
    if let Some(harness) = weak.upgrade() {
        harness.ran.borrow_mut().push(id);
    }
}

fn connect_actions(harness: &Rc<Harness>) {
    let weak = Rc::downgrade(harness);
    let app = &harness.app_actions;
    let win = &harness.win_actions;
    app.connect(
        ActionId::Quit,
        glib::clone!(
            #[weak(rename_to = application)]
            harness.app,
            move || application.quit()
        ),
    );
    app.connect(ActionId::KeyboardShortcuts, {
        let weak = weak.clone();
        move || {
            record(&weak, ActionId::KeyboardShortcuts);
            if let Some(harness) = weak.upgrade() {
                let available = |id| {
                    harness.app_actions.is_available(id) || harness.win_actions.is_available(id)
                };
                shortcuts::present(&harness.window, &harness.prefs.keymap(), &available);
            }
        }
    });
    for (id, file) in [
        (ActionId::Preferences, SettingsFile::Config),
        (ActionId::CustomizeShortcuts, SettingsFile::Keys),
    ] {
        let weak = weak.clone();
        // The real handlers call `preferences::open_in_editor`; a headless run never starts an
        // editor, so this only writes the file with its defaults.
        app.connect(id, move || {
            record(&weak, id);
            if let Some(harness) = weak.upgrade() {
                let path = harness.prefs.path(file);
                glib::spawn_future_local(async move {
                    if let Err(error) = preferences::ensure_file(path, file).await {
                        println!("     could not create {}: {error}", file.name());
                    }
                });
            }
        });
    }
    win.connect(ActionId::CommandPalette, {
        let weak = weak.clone();
        move || {
            record(&weak, ActionId::CommandPalette);
            if let Some(harness) = weak.upgrade() {
                let palette = palette::present(&harness.window, &harness.prefs.keymap());
                harness.palette.replace(Some(palette));
            }
        }
    });
    for id in [ActionId::Search, ActionId::Archive, ActionId::GoToInbox] {
        let weak = weak.clone();
        win.connect(id, move || record(&weak, id));
    }
    win.connect_toggle(ActionId::ToggleSidebar, true, {
        let weak = weak.clone();
        move |on| {
            record(&weak, ActionId::ToggleSidebar);
            if let Some(harness) = weak.upgrade() {
                harness.sidebar.set(Some(on));
            }
        }
    });
}

async fn wait_until(timeout: Duration, done: impl Fn() -> bool) -> bool {
    let started = Instant::now();
    while !done() {
        if started.elapsed() > timeout {
            return false;
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    true
}

fn same_accels(got: &[glib::GString], want: &[&str]) -> bool {
    got.len() == want.len()
        && got.iter().zip(want).all(|(got, want)| {
            let want = gtk::accelerator_parse(*want);
            want.is_some() && gtk::accelerator_parse(got.as_str()) == want
        })
}

/// Runs the scoped shortcut for `key` on `widget`, as a key press would after matching.
fn press(shortcuts: &ScopedShortcuts, widget: &impl IsA<gtk::Widget>, key: &str) -> Option<bool> {
    let wanted = gtk::accelerator_parse(key)?;
    let model = shortcuts.model();
    (0..model.n_items())
        .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
        .find(|shortcut| {
            shortcut
                .trigger()
                .and_then(|trigger| gtk::accelerator_parse(trigger.to_str()))
                == Some(wanted)
        })
        .and_then(|shortcut| shortcut.action())
        .map(|action| action.activate(gtk::ShortcutActionFlags::empty(), widget, None))
}

fn has_trigger(shortcuts: &ScopedShortcuts, key: &str) -> bool {
    let wanted = gtk::accelerator_parse(key);
    let model = shortcuts.model();
    (0..model.n_items())
        .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
        .filter_map(|shortcut| shortcut.trigger())
        .any(|trigger| gtk::accelerator_parse(trigger.to_str()) == wanted)
}

/// Renders the window, dialogs included, to a PNG with GSK's Cairo renderer, as Stet's
/// self-test does: under Broadway it is the only way to see what the app draws.
fn screenshot(window: &adw::ApplicationWindow, path: &Path) -> Result<(), String> {
    let (width, height) = (window.width(), window.height());
    if width <= 0 || height <= 0 {
        return Err("the window has no size yet".to_owned());
    }
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    let node = snapshot
        .to_node()
        .ok_or_else(|| "the window drew nothing".to_owned())?;
    let renderer = gsk::CairoRenderer::new();
    renderer
        .realize_for_display(&WidgetExt::display(window))
        .map_err(|error| error.to_string())?;
    let viewport = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    renderer.unrealize();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    texture.save_to_png(path).map_err(|error| error.to_string())
}

impl Harness {
    fn activate(&self, id: ActionId) {
        if let Err(error) = WidgetExt::activate_action(&self.window, &id.detailed_name(), None) {
            println!(
                "     {} could not be activated: {error}",
                id.detailed_name()
            );
        }
    }

    fn count(&self, id: ActionId) -> usize {
        self.ran.borrow().iter().filter(|ran| **ran == id).count()
    }

    fn accels(&self, name: &str) -> Vec<glib::GString> {
        self.app.accels_for_action(name)
    }

    fn palette(&self) -> Option<palette::Palette> {
        self.palette.borrow().clone()
    }

    fn visible_dialog(&self) -> Option<adw::Dialog> {
        self.window.visible_dialog()
    }

    async fn dialog_closed(&self) -> bool {
        wait_until(Duration::from_secs(2), || self.visible_dialog().is_none()).await
    }

    async fn shoot(&self, name: &str) {
        let Some(folder) = &self.shots else {
            return;
        };
        wait_until(Duration::from_secs(5), || {
            self.visible_dialog()
                .is_some_and(|dialog| dialog.is_mapped() && dialog.width() > 0)
        })
        .await;
        let path = folder.join(name);
        for attempt in 1..=3 {
            glib::timeout_future(LAYOUT_WAIT).await;
            match screenshot(&self.window, &path) {
                Ok(()) => {
                    println!("     screenshot {}", path.display());
                    return;
                }
                Err(error) if attempt == 3 => println!("     no screenshot {name}: {error}"),
                Err(_) => {}
            }
        }
    }

    /// Waits for a settings update after `seen` updates that `done` accepts.
    async fn update_after(&self, seen: usize, done: impl Fn(&Update) -> bool) -> Option<Update> {
        let found = || {
            self.updates
                .borrow()
                .iter()
                .skip(seen)
                .find(|update| done(update))
                .cloned()
        };
        wait_until(RELOAD_WAIT, || found().is_some()).await;
        found()
    }

    fn write(&self, file: SettingsFile, text: &str) {
        let path = self.prefs.path(file);
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            println!("     could not create {}: {error}", parent.display());
        }
        if let Err(error) = std::fs::write(&path, text) {
            println!("     could not write {}: {error}", file.name());
        }
    }

    async fn run(self: &Rc<Self>) -> u32 {
        let r = &self.report;
        self.prefs.ready(Duration::from_secs(2)).await;
        r.check(
            "missing settings files are the defaults",
            self.prefs.is_loaded()
                && self.prefs.keymap() == Keymap::default()
                && self.prefs.config() == Config::default(),
        );
        r.check(
            "the window maps",
            wait_until(Duration::from_secs(5), || self.window.is_mapped()).await,
        );

        r.check(
            "Ctrl+Shift+P and F1 are the palette's accelerators",
            same_accels(
                &self.accels("win.command-palette"),
                &["<Control><Shift>p", "F1"],
            ),
        );
        r.check(
            "Ctrl+? is the keyboard shortcuts' accelerator",
            same_accels(&self.accels("app.shortcuts"), &["<Control>question"]),
        );
        r.check(
            "message commands have no application accelerators",
            self.accels("win.reply").is_empty() && self.accels("win.undo").is_empty(),
        );

        r.check(
            "commands without a handler are disabled, connected ones enabled",
            !self.window.is_action_enabled("reply")
                && !self.window.is_action_enabled("refresh")
                && self.app.is_action_enabled("shortcuts")
                && self.window.is_action_enabled("search"),
        );
        r.check(
            "Archive waits for a selection",
            !self.window.is_action_enabled("archive"),
        );
        self.win_actions.set_has_selection(true);
        r.check(
            "Archive is enabled with a selection",
            self.window.is_action_enabled("archive"),
        );

        self.activate(ActionId::Search);
        r.check(
            "activating win.search runs its handler",
            self.count(ActionId::Search) == 1,
        );
        self.activate(ActionId::Reply);
        r.check(
            "activating a disabled command runs nothing",
            self.count(ActionId::Reply) == 0,
        );
        self.activate(ActionId::ToggleSidebar);
        let state = self
            .win_actions
            .action(ActionId::ToggleSidebar)
            .and_then(|action| action.state())
            .and_then(|state| state.get::<bool>());
        r.check(
            "activating win.toggle-sidebar flips its state",
            self.sidebar.get() == Some(false) && state == Some(false),
        );

        self.activate(ActionId::CommandPalette);
        let opened = self.palette();
        r.check(
            "win.command-palette opens the palette over the window",
            opened.is_some()
                && self
                    .visible_dialog()
                    .is_some_and(|dialog| dialog.has_css_class("sealwax-palette")),
        );
        let Some(palette) = opened else {
            return self.report.failed.get() + 1;
        };
        let shown = palette.shown();
        let titles: Vec<&str> = shown.iter().map(|id| id.title()).collect();
        println!("     the palette lists: {}", titles.join(", "));
        r.check(
            "the palette lists the enabled commands and nothing else",
            shown.contains(&ActionId::Search)
                && shown.contains(&ActionId::Archive)
                && shown.contains(&ActionId::KeyboardShortcuts)
                && !shown.contains(&ActionId::Reply)
                && !shown.contains(&ActionId::CommandPalette),
        );
        self.shoot("palette.png").await;
        let entry = palette.search_entry();
        // GtkSearchEntry hands the focus to the GtkText inside it.
        let focus = GtkWindowExt::focus(&self.window);
        r.check(
            "the keyboard focus is in the search entry",
            entry.as_ref().is_some_and(|entry| {
                focus.as_ref().is_some_and(|focus| {
                    focus == entry.upcast_ref::<gtk::Widget>() || focus.is_ancestor(entry)
                })
            }),
        );
        if let Some(entry) = &entry {
            entry.set_text("arch");
        }
        wait_until(Duration::from_secs(1), || {
            palette.shown().first() == Some(&ActionId::Archive)
        })
        .await;
        let filtered = palette.shown();
        let titles: Vec<&str> = filtered.iter().map(|id| id.title()).collect();
        println!("     \"arch\" lists: {}", titles.join(", "));
        r.check(
            "typing filters the commands, best match first",
            filtered.first() == Some(&ActionId::Archive) && filtered.len() < shown.len(),
        );
        if let Some(entry) = &entry {
            entry.emit_activate();
        }
        r.check(
            "Enter runs the selected command and closes the palette",
            self.dialog_closed().await && self.count(ActionId::Archive) == 1,
        );

        self.activate(ActionId::CommandPalette);
        if let Some(palette) = self.palette() {
            let first = palette.selected();
            palette.move_selection(1);
            let second = palette.selected();
            r.check(
                "Down moves the selection",
                first.is_some() && second.is_some() && first != second,
            );
            if let Some(entry) = palette.search_entry() {
                entry.emit_stop_search();
            }
            r.check("Escape closes the palette", self.dialog_closed().await);
        }

        self.activate(ActionId::CommandPalette);
        if let Some(palette) = self.palette()
            && let Some(entry) = palette.search_entry()
        {
            entry.set_text("keyboard sh");
            wait_until(Duration::from_secs(1), || {
                palette.shown().first() == Some(&ActionId::KeyboardShortcuts)
            })
            .await;
            entry.emit_activate();
        }
        let shortcuts_shown = wait_until(Duration::from_secs(2), || {
            self.visible_dialog()
                .is_some_and(|dialog| dialog.is::<adw::ShortcutsDialog>())
        })
        .await;
        r.check(
            "the palette opens the keyboard shortcuts dialog",
            shortcuts_shown && self.count(ActionId::KeyboardShortcuts) == 1,
        );
        self.shoot("shortcuts.png").await;
        if let Some(dialog) = self.visible_dialog() {
            dialog.close();
        }
        self.dialog_closed().await;

        r.check(
            "the list and the reader have scoped shortcuts",
            self.list_keys.model().n_items() > 0 && self.reader_keys.model().n_items() > 0,
        );
        r.check(
            "e archives from the list",
            press(&self.list_keys, &self.list, "e") == Some(true)
                && self.count(ActionId::Archive) == 2,
        );
        r.check(
            "e archives from the reader",
            press(&self.reader_keys, &self.reader, "e") == Some(true)
                && self.count(ActionId::Archive) == 3,
        );
        r.check(
            "a scoped key of a disabled command passes the key on",
            press(&self.list_keys, &self.list, "r") == Some(false),
        );
        r.check(
            "g waits for the second key",
            press(&self.list_keys, &self.list, "g") == Some(true)
                && self.list_keys.pending().as_deref() == Some("g"),
        );
        r.check(
            "g then i goes to the Inbox",
            press(&self.list_keys, &self.list, "i") == Some(true)
                && self.count(ActionId::GoToInbox) == 1,
        );
        press(&self.list_keys, &self.list, "g");
        r.check(
            "g then e runs nothing",
            press(&self.list_keys, &self.list, "e") == Some(true)
                && self.count(ActionId::Archive) == 3,
        );
        r.check(
            "Space selects in the list only",
            has_trigger(&self.list_keys, "space") && !has_trigger(&self.reader_keys, "space"),
        );
        r.check(
            "accelerators are not scoped keys",
            !has_trigger(&self.list_keys, "<Control>n") && has_trigger(&self.list_keys, "c"),
        );
        press(&self.list_keys, &self.list, "question");
        r.check(
            "? opens the keyboard shortcuts from the list",
            wait_until(Duration::from_secs(2), || {
                self.visible_dialog()
                    .is_some_and(|dialog| dialog.is::<adw::ShortcutsDialog>())
            })
            .await
                && self.count(ActionId::KeyboardShortcuts) == 2,
        );
        if let Some(dialog) = self.visible_dialog() {
            dialog.close();
        }
        self.dialog_closed().await;

        r.check(
            "the primary menu has its three sections",
            self.menu_button
                .menu_model()
                .is_some_and(|menu| menu.n_items() == 3),
        );

        let seen = self.updates.borrow().len();
        self.write(
            SettingsFile::Keys,
            "command-palette = [\"<Control><Alt>k\", \"F1\"]\narchive = [\"e\"]\n",
        );
        let applied = self
            .update_after(seen, |update| update.keymap_changed)
            .await
            .is_some();
        r.check(
            "keys.toml applies live: accelerators and scoped keys",
            applied
                && same_accels(
                    &self.accels("win.command-palette"),
                    &["<Control><Alt>k", "F1"],
                )
                && has_trigger(&self.list_keys, "e")
                && !has_trigger(&self.list_keys, "y"),
        );

        let seen = self.updates.borrow().len();
        self.write(SettingsFile::Keys, "archive = [\"e\"]\nstar = [\"e\"]\n");
        let rejected = self
            .update_after(seen, |update| !update.problems.is_empty())
            .await;
        r.check(
            "a faulty keys.toml is reported with its line and column, and not applied",
            rejected.is_some_and(|update| {
                update.problems[0].starts_with("keys.toml not applied: line 2, column 9:")
                    && !update.keymap_changed
            }) && same_accels(
                &self.accels("win.command-palette"),
                &["<Control><Alt>k", "F1"],
            ),
        );

        let seen = self.updates.borrow().len();
        self.write(SettingsFile::Config, "[appearance]\ncorner_radius = 12\n");
        let applied = self
            .update_after(seen, |update| update.config_before.is_some())
            .await;
        r.check(
            "config.toml applies live",
            applied.is_some()
                && self.prefs.config().appearance.corner_radius == CornerRadius::Px(12),
        );

        let seen = self.updates.borrow().len();
        self.write(
            SettingsFile::Config,
            "[appearance]\nchrome_font = \"serif\"\n",
        );
        let rejected = self
            .update_after(seen, |update| !update.problems.is_empty())
            .await;
        r.check(
            "a faulty config.toml is reported and not applied",
            rejected.is_some_and(|update| {
                update.problems[0].starts_with("config.toml not applied: line 2, column 15:")
            }) && self.prefs.config().appearance.corner_radius == CornerRadius::Px(12),
        );

        let seen = self.updates.borrow().len();
        if let Err(error) = std::fs::remove_file(self.prefs.path(SettingsFile::Keys)) {
            println!("     could not remove keys.toml: {error}");
        }
        let restored = self
            .update_after(seen, |update| update.keymap_changed)
            .await
            .is_some();
        r.check(
            "removing keys.toml brings the default keys back",
            restored
                && same_accels(
                    &self.accels("win.command-palette"),
                    &["<Control><Shift>p", "F1"],
                )
                && has_trigger(&self.list_keys, "y"),
        );

        self.activate(ActionId::CustomizeShortcuts);
        let path = self.prefs.path(SettingsFile::Keys);
        let created = wait_until(Duration::from_secs(2), || path.exists()).await;
        glib::timeout_future(Duration::from_millis(500)).await;
        r.check(
            "Customize Shortcuts writes keys.toml with every default commented out",
            created
                && std::fs::read_to_string(&path).is_ok_and(|text| text == default_keys_file())
                && self.prefs.errors(SettingsFile::Keys).is_empty()
                && self.prefs.keymap() == Keymap::default(),
        );

        println!(
            "palette check: {} passed, {} failed",
            r.passed.get(),
            r.failed.get()
        );
        r.failed.get()
    }
}
