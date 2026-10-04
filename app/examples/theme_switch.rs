//! Headless check of the live theme switch (ML-018). Run it through the wrapper with a scratch
//! Omarchy state directory:
//!
//! ```sh
//! SEALWAX_OMARCHY_STATE=$PWD/.local/theme-switch/omarchy SEALWAX_HEADLESS_TIMEOUT=120 \
//!     tools/headless.sh 30 cargo run -p sealwax --example theme_switch
//! ```
//!
//! It fills the scratch state with matte-black (dark), opens a window of themed sample
//! widgets, switches the scratch theme to catppuccin-latte (light) and back with the file
//! operations `omarchy-theme-set` uses, then removes `current/` (the built-in palette). Each
//! step logs `theme-applied <name> <mode>` and checks the libadwaita scheme, rendered
//! colours, text colours, sizes and fonts against the theme. Exits 0 when every check passes.
//! It refuses to touch any directory but an empty one or its own earlier scratch directory.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::theme::{Mode, Rgb, Theme, ThemeOptions, avatar_class};
use sealwax_infrastructure::omarchy::{OmarchyPaths, STATE_DIR_ENV};
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

const MARKER: &str = ".sealwax-theme-switch";
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
const SENDER: &str = "ingrid@fjordpost.example";

type Applied = Rc<RefCell<Vec<(String, Mode, Instant)>>>;

fn main() -> glib::ExitCode {
    if let Err(error) = sealwax_infrastructure::logging::init() {
        eprintln!("Could not initialize logging: {error}");
        return glib::ExitCode::FAILURE;
    }
    let state = match scratch_state() {
        Ok(state) => state,
        Err(message) => {
            tracing::error!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };
    if let Err(error) = install_theme(&state, "matte-black") {
        tracing::error!("could not prepare the scratch theme: {error}");
        return glib::ExitCode::FAILURE;
    }

    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ThemeSwitch")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let controller: Rc<RefCell<Option<Rc<ThemeController>>>> = Rc::default();
    let passed = Rc::new(Cell::new(false));
    app.connect_startup(glib::clone!(
        #[strong]
        controller,
        move |_| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            if OmarchyPaths::from_env().state != state_path() {
                tracing::error!("{STATE_DIR_ENV} was not honoured");
            }
            controller.replace(Some(theme));
        }
    ));
    app.connect_activate(glib::clone!(
        #[strong]
        controller,
        #[strong]
        passed,
        move |app| {
            let Some(theme) = controller.borrow().clone() else {
                return;
            };
            let window = build_window(app, &theme.theme());
            window.present();
            let app = app.clone();
            let passed = passed.clone();
            glib::spawn_future_local(async move {
                match scenario(&theme, &window).await {
                    Ok(()) => {
                        tracing::info!("theme-switch: every check passed");
                        passed.set(true);
                    }
                    Err(error) => tracing::error!("theme-switch: {error}"),
                }
                app.quit();
            });
        }
    ));
    let status = app.run_with_args::<&str>(&[]);
    if status != glib::ExitCode::SUCCESS || !passed.get() {
        return glib::ExitCode::FAILURE;
    }
    glib::ExitCode::SUCCESS
}

fn state_path() -> PathBuf {
    std::env::var_os(STATE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The scratch state directory: absolute, never Omarchy's own, and either missing, empty or
/// marked as this check's.
fn scratch_state() -> Result<PathBuf, String> {
    let state = state_path();
    if !state.is_absolute() {
        return Err(format!(
            "set {STATE_DIR_ENV} to an absolute scratch directory"
        ));
    }
    let home = std::env::home_dir().unwrap_or_default();
    let live = home.join(".local/state/omarchy");
    let resolved = fs::canonicalize(&state).unwrap_or_else(|_| state.clone());
    let live = fs::canonicalize(&live).unwrap_or(live);
    if resolved.starts_with(&live) || live.starts_with(&resolved) {
        return Err(format!("{STATE_DIR_ENV} must not be Omarchy's own state"));
    }
    if state.is_dir() && !state.join(MARKER).is_file() {
        let empty = fs::read_dir(&state)
            .map_err(|error| error.to_string())?
            .next()
            .is_none();
        if !empty {
            return Err(format!(
                "{STATE_DIR_ENV} is neither empty nor this check's scratch directory"
            ));
        }
    }
    fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    fs::write(state.join(MARKER), "").map_err(|error| error.to_string())?;
    let _ = fs::remove_dir_all(state.join("current"));
    Ok(state)
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/theme")
        .join(name)
}

/// Swaps `name` in as `omarchy-theme-set` does: stage `next-theme/`, remove `theme/`, rename
/// `next-theme/` to `theme/`, then write `theme.name`.
fn install_theme(state: &Path, name: &str) -> std::io::Result<()> {
    let current = state.join("current");
    let next = current.join("next-theme");
    let _ = fs::remove_dir_all(&next);
    fs::create_dir_all(&next)?;
    fs::copy(fixture(name).join("colors.toml"), next.join("colors.toml"))?;
    fs::copy(fixture("shell.toml"), next.join("shell.toml"))?;
    let theme = current.join("theme");
    if theme.exists() {
        fs::remove_dir_all(&theme)?;
    }
    fs::rename(&next, &theme)?;
    fs::write(current.join("theme.name"), format!("{name}\n"))
}

struct Probes {
    window_bg: gtk::Widget,
    sidebar_bg: gtk::Widget,
    paper: gtk::Widget,
    unread_dot: gtk::Widget,
    avatar: gtk::Label,
    secondary: gtk::Label,
    chip: gtk::ToggleButton,
    attachment_type: gtk::Label,
    body: gtk::Label,
}

thread_local! {
    static PROBES: RefCell<Option<Probes>> = const { RefCell::new(None) };
}

fn probe(class: &str) -> gtk::Widget {
    let probe = gtk::Box::builder()
        .width_request(24)
        .height_request(24)
        .css_classes([class])
        .build();
    probe.upcast()
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(classes.to_vec())
        .build()
}

/// Sample widgets in the classes `style.css` defines; all text is fictional.
fn build_window(app: &adw::Application, theme: &Theme) -> adw::ApplicationWindow {
    let sidebar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .css_classes(["sw-sidebar"])
        .build();
    sidebar.append(
        &gtk::Button::builder()
            .label("New message")
            .css_classes(["sw-compose"])
            .build(),
    );
    let folder = gtk::Box::builder()
        .css_classes(["sw-sidebar-row", "sw-current"])
        .build();
    folder.append(&label("Inbox", &[]));
    folder.append(&label("12", &["sw-count", "sw-unread"]));
    sidebar.append(&folder);
    sidebar.append(&label("PERSONAL", &["sw-sidebar-heading"]));
    let status = probe("sw-status-dot");
    status.add_css_class("sw-synced");
    sidebar.append(&status);
    let sidebar_bg = probe("sw-sidebar");
    sidebar.append(&sidebar_bg);

    let chips = gtk::Box::builder()
        .spacing(6)
        .valign(gtk::Align::Start)
        .build();
    let chip = gtk::ToggleButton::builder()
        .label("All")
        .active(true)
        .css_classes(["sw-chip"])
        .build();
    chips.append(&chip);
    chips.append(
        &gtk::ToggleButton::builder()
            .label("Unread")
            .css_classes(["sw-chip"])
            .build(),
    );

    let avatar = gtk::Label::builder()
        .label(sealwax_domain::theme::initials("Ingrid Solberg", SENDER))
        .valign(gtk::Align::Start)
        .css_classes(["sw-avatar".to_owned(), avatar_class(SENDER)])
        .build();
    let unread_dot: gtk::Widget = gtk::Box::builder()
        .css_classes(["sw-unread-dot"])
        .valign(gtk::Align::Start)
        .halign(gtk::Align::Center)
        .build()
        .upcast();
    let secondary = label(
        "Hi! I've booked the cabin for the weekend.",
        &["sw-snippet"],
    );
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    text.append(&label("Ingrid Solberg", &["sw-sender"]));
    text.append(&label("Cabin weekend: final plan", &["sw-subject"]));
    text.append(&secondary);
    let row = gtk::Box::builder()
        .css_classes(["sw-message-row", "sw-unread"])
        .build();
    row.append(&unread_dot);
    row.append(&avatar);
    row.append(&text);
    row.append(
        &gtk::Image::builder()
            .icon_name("starred-symbolic")
            .css_classes(["sw-star"])
            .build(),
    );
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sw-message-list"])
        .build();
    list.append(&label("TODAY", &["sw-section-header"]));
    list.append(&chips);
    list.append(&row);

    let body = label("Plain message text in the reading font.", &["sw-reading"]);
    let paper = probe("sw-paper");
    let attachment = gtk::Box::builder()
        .css_classes(["sw-attachment-card"])
        .build();
    let attachment_type = label("PDF", &["sw-attachment-type"]);
    attachment.append(&attachment_type);
    attachment.append(&label("Packing list.pdf", &["sw-attachment-name"]));
    attachment.append(&label("84 KB", &["sw-attachment-meta"]));
    let empty = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sw-empty-state"])
        .build();
    empty.append(
        &gtk::Image::builder()
            .icon_name("mail-unread-symbolic")
            .css_classes(["sw-empty-icon"])
            .build(),
    );
    empty.append(&label("Nothing archived yet", &["sw-empty-title"]));
    empty.append(&label(
        "Press e on a message to archive it.",
        &["sw-empty-body"],
    ));
    let banner = gtk::Box::builder()
        .css_classes(["sw-banner", "sw-warning"])
        .build();
    banner.append(&label("You're offline.", &[]));
    banner.append(
        &gtk::Button::builder()
            .label("Details")
            .css_classes(["sw-control"])
            .build(),
    );
    let window_bg = probe("sw-reader");
    let reader = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .css_classes(["sw-reader"])
        .build();
    reader.append(&label("Cabin weekend: final plan", &["sw-subject-heading"]));
    reader.append(&body);
    reader.append(&paper);
    reader.append(&attachment);
    reader.append(&banner);
    reader.append(&empty);
    reader.append(&window_bg);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&sidebar);
    content.append(&list);
    content.append(&reader);
    PROBES.with(|probes| {
        probes.replace(Some(Probes {
            window_bg,
            sidebar_bg,
            paper,
            unread_dot,
            avatar,
            secondary,
            chip,
            attachment_type,
            body,
        }))
    });
    tracing::info!(theme = theme.name(), "theme-switch window built");
    adw::ApplicationWindow::builder()
        .application(app)
        .title("Sealwax theme switch")
        .default_width(1100)
        .default_height(700)
        .content(&content)
        .build()
}

async fn scenario(
    theme: &Rc<ThemeController>,
    window: &adw::ApplicationWindow,
) -> Result<(), String> {
    let applied: Applied = Rc::default();
    theme.connect_changed(glib::clone!(
        #[strong]
        applied,
        move |theme| {
            tracing::info!("theme-applied {} {}", theme.name(), theme.mode().as_str());
            applied
                .borrow_mut()
                .push((theme.name().to_owned(), theme.mode(), Instant::now()));
        }
    ));
    let first = theme.theme();
    tracing::info!("theme-applied {} {}", first.name(), first.mode().as_str());
    expect_theme(&first, "matte-black", Mode::Dark)?;
    next_frame(window).await?;
    check(theme, window)?;

    let state = state_path();
    for (name, mode) in [
        ("catppuccin-latte", Mode::Light),
        ("matte-black", Mode::Dark),
    ] {
        applied.borrow_mut().clear();
        let switched = Instant::now();
        install_theme(&state, name).map_err(|error| error.to_string())?;
        let (_, at) = wait_for(&applied, name, |seen, seen_mode| {
            seen == name && seen_mode == mode
        })
        .await?;
        let ms = at.duration_since(switched).as_secs_f64() * 1000.0;
        tracing::info!(
            "theme-switch: {name} applied {ms:.1} ms after the swap (debounce 150 ms included)"
        );
        next_frame(window).await?;
        check(theme, window)?;
    }

    applied.borrow_mut().clear();
    let removed = Instant::now();
    fs::remove_dir_all(state.join("current")).map_err(|error| error.to_string())?;
    let (builtin, at) = wait_for(&applied, "a built-in palette", |seen, _| {
        seen.starts_with("builtin-")
    })
    .await?;
    let ms = at.duration_since(removed).as_secs_f64() * 1000.0;
    tracing::info!("theme-switch: {builtin} applied {ms:.1} ms after current/ was removed");
    next_frame(window).await?;
    check(theme, window)?;
    if theme.css_errors() != 0 {
        return Err(format!("{} CSS errors", theme.css_errors()));
    }
    Ok(())
}

fn expect_theme(theme: &Theme, name: &str, mode: Mode) -> Result<(), String> {
    if theme.name() == name && theme.mode() == mode {
        Ok(())
    } else {
        Err(format!(
            "expected {name} {}, got {} {}",
            mode.as_str(),
            theme.name(),
            theme.mode().as_str()
        ))
    }
}

/// The name of the first applied theme `wanted` accepts, and when it was applied.
async fn wait_for(
    applied: &Applied,
    what: &str,
    wanted: impl Fn(&str, Mode) -> bool,
) -> Result<(String, Instant), String> {
    let started = Instant::now();
    loop {
        if let Some((name, _, at)) = applied
            .borrow()
            .iter()
            .find(|(name, mode, _)| wanted(name, *mode))
        {
            return Ok((name.clone(), *at));
        }
        if started.elapsed() > STEP_TIMEOUT {
            return Err(format!("{what} was never applied"));
        }
        glib::timeout_future(Duration::from_millis(5)).await;
    }
}

/// Resolves after the window has painted again with every probe allocated, so the restyle
/// and the layout have run.
async fn next_frame(window: &adw::ApplicationWindow) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let clock = window
            .frame_clock()
            .ok_or("the window has no frame clock")?;
        let (sender, receiver) = async_channel::bounded(1);
        let handler = clock.connect_after_paint(move |_| {
            let _ = sender.try_send(());
        });
        window.queue_draw();
        let painted = glib::future_with_timeout(STEP_TIMEOUT, receiver.recv()).await;
        clock.disconnect(handler);
        painted
            .map_err(|_| "the window never painted".to_owned())?
            .map_err(|error| error.to_string())?;
        let allocated = PROBES.with(|probes| {
            probes.borrow().as_ref().is_some_and(|probes| {
                [
                    &probes.window_bg,
                    &probes.sidebar_bg,
                    &probes.paper,
                    &probes.unread_dot,
                    probes.avatar.upcast_ref(),
                ]
                .iter()
                .all(|probe| {
                    border_box(probe).is_some_and(|(width, height)| width > 0 && height > 0)
                })
            })
        });
        if allocated {
            return Ok(());
        }
        if started.elapsed() > STEP_TIMEOUT {
            return Err("the probes were never allocated".to_owned());
        }
    }
}

/// The centre pixel of what GTK draws for `widget`.
fn rendered(widget: &impl IsA<gtk::Widget>) -> Result<Rgb, String> {
    rendered_at(widget, 0.5, 0.5)
}

/// The pixel at `from_left` of the width and `from_top` of the height (0–1) of what GTK
/// draws for `widget`, border included.
fn rendered_at(
    widget: &impl IsA<gtk::Widget>,
    from_left: f64,
    from_top: f64,
) -> Result<Rgb, String> {
    let widget = widget.as_ref();
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let (width, height) = (paintable.intrinsic_width(), paintable.intrinsic_height());
    if width <= 0 || height <= 0 {
        return Err(format!(
            "a {} probe is {width}x{height}",
            widget.css_classes().join(".")
        ));
    }
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().ok_or("a probe drew nothing")?;
    let renderer = widget
        .native()
        .and_then(|native| native.renderer())
        .ok_or("no renderer")?;
    let texture: gdk::Texture = renderer.render_texture(&node, None);
    let (texture_width, texture_height) = (texture.width() as usize, texture.height() as usize);
    let stride = texture_width * 4;
    let mut data = vec![0; stride * texture_height];
    texture.download(&mut data, stride);
    let row = ((texture_height as f64 * from_top) as usize).min(texture_height - 1);
    let column = ((texture_width as f64 * from_left) as usize).min(texture_width - 1);
    let at = row * stride + column * 4;
    Ok(Rgb {
        r: data[at + 2],
        g: data[at + 1],
        b: data[at],
    })
}

fn text_color(widget: &impl IsA<gtk::Widget>) -> Rgb {
    let color = widget.as_ref().color();
    let channel = |value: f32| (value * 255.0).round() as u8;
    Rgb {
        r: channel(color.red()),
        g: channel(color.green()),
        b: channel(color.blue()),
    }
}

fn close(a: Rgb, b: Rgb) -> bool {
    a.r.abs_diff(b.r) <= 2 && a.g.abs_diff(b.g) <= 2 && a.b.abs_diff(b.b) <= 2
}

/// Compares what GTK shows with the theme the controller applied.
fn check(controller: &ThemeController, window: &adw::ApplicationWindow) -> Result<(), String> {
    let theme = controller.theme();
    let dark = adw::StyleManager::default().is_dark();
    if dark != (theme.mode() == Mode::Dark) {
        return Err(format!("libadwaita is dark={dark} under {}", theme.name()));
    }
    PROBES.with(|probes| {
        let probes = probes.borrow();
        let probes = probes.as_ref().ok_or("no probes")?;
        let tint = theme.avatar(SENDER);
        let colours = [
            (
                "window background",
                rendered(&probes.window_bg)?,
                theme.palette.background,
            ),
            (
                "sidebar background",
                rendered(&probes.sidebar_bg)?,
                theme.palette.dark_background,
            ),
            ("paper card", rendered(&probes.paper)?, Rgb::WHITE),
            (
                "paper card border",
                rendered_at(&probes.paper, 0.0, 0.5)?,
                theme.ui.border,
            ),
            (
                "unread dot",
                rendered(&probes.unread_dot)?,
                theme.mail.unread,
            ),
            (
                "avatar tint",
                rendered_at(&probes.avatar, 0.5, 0.1)?,
                tint.background,
            ),
            (
                "secondary text",
                text_color(&probes.secondary),
                theme.mail.secondary_text,
            ),
            ("initials", text_color(&probes.avatar), tint.foreground),
            (
                "selected chip text",
                text_color(&probes.chip),
                theme.mail.selected_chip,
            ),
            (
                "attachment type text",
                text_color(&probes.attachment_type),
                theme.mail.selected_chip,
            ),
            ("window text", text_color(window), theme.palette.foreground),
        ];
        for (what, shown, expected) in colours {
            if !close(shown, expected) {
                return Err(format!(
                    "{}: {what} is {shown}, expected {expected}",
                    theme.name()
                ));
            }
        }
        let size = |widget: &gtk::Widget| border_box(widget).unwrap_or_default();
        let sizes = [
            ("avatar", size(probes.avatar.upcast_ref()), Some(32)),
            ("chip", size(probes.chip.upcast_ref()), None),
            ("unread dot", size(&probes.unread_dot), Some(7)),
        ];
        for (what, (width, height), square) in sizes {
            let expected_height = square.unwrap_or(26);
            let expected = |px: u32| theme.spacing.space(px) as i32;
            let wrong_width = square.is_some_and(|px| width != expected(px));
            if height != expected(expected_height) || wrong_width {
                return Err(format!(
                    "{what} is {width}x{height}, expected {} high",
                    expected(expected_height)
                ));
            }
        }
        let chrome = family(window);
        let reading = family(&probes.body);
        if chrome != theme.fonts.chrome || reading != theme.fonts.reading {
            return Err(format!(
                "fonts are {chrome:?} / {reading:?}, expected {:?} / {:?}",
                theme.fonts.chrome, theme.fonts.reading
            ));
        }
        tracing::info!(
            theme = theme.name(),
            mode = theme.mode().as_str(),
            chrome = %chrome,
            reading = %reading,
            radius = theme.radius,
            "theme-switch: colours, sizes and fonts match"
        );
        Ok(())
    })
}

/// The first family of the widget's font, as Pango gets it from CSS.
fn family(widget: &impl IsA<gtk::Widget>) -> String {
    widget
        .as_ref()
        .pango_context()
        .font_description()
        .and_then(|description| description.family())
        .and_then(|families| families.split(',').next().map(str::to_owned))
        .unwrap_or_default()
}

/// The widget's border box (padding and border included), in px.
fn border_box(widget: &gtk::Widget) -> Option<(i32, i32)> {
    let bounds = widget.compute_bounds(widget)?;
    Some((
        bounds.width().round() as i32,
        bounds.height().round() as i32,
    ))
}
