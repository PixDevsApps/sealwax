//! Headless check that open HTML messages follow a live theme switch (ML-027; M1's exit
//! criterion "a live dark↔light theme switch recolours everything, WebViews included"). Two
//! readers stay open: one with simple HTML in the theme's colours, one with a styled
//! newsletter on the paper card. The scratch Omarchy state switches matte-black (dark) →
//! catppuccin-latte (light) → matte-black with the file operations `omarchy-theme-set` uses;
//! after each switch the check compares what the WebViews render and compute with the theme,
//! checks that the documents were restyled in place (not reloaded), and saves screenshots in
//! `.local/reader/`.
//!
//! ```sh
//! SEALWAX_OMARCHY_STATE=$PWD/.local/reader-examples/theme-state SEALWAX_HEADLESS_TIMEOUT=180 \
//!     tools/headless.sh 37 target/debug/examples/reader_theme
//! ```
//!
//! It refuses a state folder that is relative, Omarchy's own, or not empty and not its own.

#[path = "reader_support/mod.rs"]
mod support;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::reader::samples;
use sealwax_app::reader::{Reader, ViewEvent};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::theme::{Mode, PAPER, PAPER_TEXT, Rgb, Theme, ThemeOptions};
use sealwax_infrastructure::omarchy::STATE_DIR_ENV;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use support::broadway;
use webkit6::prelude::*;

const MARKER: &str = ".sealwax-reader-theme";

fn state_path() -> PathBuf {
    std::env::var_os(STATE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The scratch state folder: absolute, never Omarchy's own, and missing, empty or this
/// check's. As `theme_switch.rs`.
fn scratch_state() -> Result<PathBuf, String> {
    let state = state_path();
    if !state.is_absolute() {
        return Err(format!("set {STATE_DIR_ENV} to an absolute scratch folder"));
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
                "{STATE_DIR_ENV} is neither empty nor this check's scratch folder"
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

/// As `omarchy-theme-set`: stage `next-theme/`, remove `theme/`, rename, write `theme.name`.
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

fn main() -> glib::ExitCode {
    let _ = sealwax_infrastructure::logging::init();
    let state = match scratch_state() {
        Ok(state) => state,
        Err(message) => {
            eprintln!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };
    if let Err(error) = install_theme(&state, "matte-black") {
        eprintln!("could not prepare the scratch theme: {error}");
        return glib::ExitCode::FAILURE;
    }
    let input = match broadway::connect() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("run this through tools/headless.sh: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let root = support::scratch("theme");
    let scratch = support::store(&root);
    let now = glib::real_time() / 1_000_000;
    let filled = samples::fill_store(&scratch.store, &scratch.blobs, samples::messages(now))
        .expect("the samples go into the scratch store");
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ReaderTheme")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let scratch = Rc::new(scratch);
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let themed = support::reader(&scratch, &theme);
            let window = support::window(app, &themed, (1440, 900));
            window.present();
            let paper = support::reader(&scratch, &theme);
            let column = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&paper.widget())
                .build();
            let second = gtk::Window::builder()
                .title("paper")
                .default_width(820)
                .default_height(900)
                .child(&column)
                .build();
            second.present();
            let app = app.clone();
            let passed = passed.clone();
            let filled = filled.clone();
            let _input = input.clone();
            glib::spawn_future_local(async move {
                let readers = Readers {
                    themed,
                    paper,
                    window,
                    second,
                };
                let ok = match scenario(&theme, &readers, &filled).await {
                    Ok(()) => true,
                    Err(error) => {
                        println!("THEME error {error}");
                        false
                    }
                };
                println!("THEME {}", if ok { "every check passed" } else { "FAILED" });
                passed.set(ok);
                app.quit();
            });
        }
    ));
    app.run_with_args::<&str>(&[]);
    if passed.get() {
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}

struct Readers {
    themed: Reader,
    paper: Reader,
    window: sealwax_app::window::Window,
    second: gtk::Window,
}

/// What a document computes and the view draws, read from Sealwax's isolated world.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    text: Rgb,
    link: Option<Rgb>,
    body_background: Option<Rgb>,
    font_size: String,
    scheme: String,
    marker: bool,
    view_background: Rgb,
    corner: Rgb,
}

const LOOK_JS: &str = r#"
const style = getComputedStyle(document.documentElement);
const body = getComputedStyle(document.body);
const link = document.querySelector('a');
return {
  text: style.color,
  link: link ? getComputedStyle(link).color : '',
  body: body.backgroundColor,
  size: body.fontSize,
  scheme: style.colorScheme,
  marker: window.sealwaxThemeMarker === 1,
};
"#;

fn parse_rgb(css: &str) -> Option<Rgb> {
    let inner = css
        .trim()
        .strip_prefix("rgba(")
        .or_else(|| css.trim().strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() == 4 && parts[3].parse::<f64>().ok()? == 0.0 {
        return None;
    }
    let channel = |index: usize| parts.get(index)?.parse::<f64>().ok().map(|v| v as u8);
    Some(Rgb {
        r: channel(0)?,
        g: channel(1)?,
        b: channel(2)?,
    })
}

async fn look(reader: &Reader) -> Result<Look, String> {
    let value = reader.eval(LOOK_JS).await?;
    let text = |key: &str| {
        value
            .object_get_property(key)
            .map(|value| value.to_str().to_string())
            .unwrap_or_default()
    };
    let view = reader.web_view().ok_or("no web view")?;
    let background = view.background_color();
    let channel = |value: f32| (value * 255.0).round() as u8;
    Ok(Look {
        text: parse_rgb(&text("text")).ok_or("no text colour")?,
        link: parse_rgb(&text("link")),
        body_background: parse_rgb(&text("body")),
        font_size: text("size"),
        scheme: text("scheme"),
        marker: value
            .object_get_property("marker")
            .is_some_and(|value| value.to_boolean()),
        view_background: Rgb {
            r: channel(background.red()),
            g: channel(background.green()),
            b: channel(background.blue()),
        },
        corner: support::painted_pixel(&view, view.width() - 4, view.height() - 4).await?,
    })
}

async fn mark(reader: &Reader) -> Result<(), String> {
    reader
        .eval("window.sealwaxThemeMarker = 1; return true;")
        .await
        .map(|_| ())
}

fn loads(events: &Rc<RefCell<Vec<ViewEvent>>>) -> usize {
    events
        .borrow()
        .iter()
        .filter(|event| **event == ViewEvent::Loaded)
        .count()
}

fn expect(what: &str, held: bool, detail: String) -> Result<(), String> {
    println!(
        "THEME {} {what}: {detail}",
        if held { "ok" } else { "UNEXPECTED" }
    );
    if held {
        Ok(())
    } else {
        Err(format!("{what}: {detail}"))
    }
}

/// The themed body takes the theme: its text, links, background, corner pixel and scheme.
fn check_themed(theme: &Theme, look: &Look) -> Result<(), String> {
    let p = &theme.palette;
    let scheme = match theme.mode() {
        Mode::Dark => "dark",
        Mode::Light => "light",
    };
    expect(
        &format!("{} themed body follows the theme", theme.name()),
        support::close(look.text, p.foreground)
            && look.link.is_some_and(|link| support::close(link, p.accent))
            && support::close(look.view_background, p.background)
            && support::close(look.corner, p.background)
            && look.scheme == scheme
            && look.font_size == format!("{}px", theme.webview_fonts().default_size),
        format!(
            "text {} (theme {}), link {:?} (accent {}), view {} and corner pixel {} \
             (background {}), scheme {}, font {}",
            look.text,
            p.foreground,
            look.link.map(|link| link.to_string()),
            p.accent,
            look.view_background,
            look.corner,
            p.background,
            look.scheme,
            look.font_size
        ),
    )
}

/// The paper body keeps its own design on white whatever the theme.
fn check_paper(theme: &Theme, look: &Look, first: &Look) -> Result<(), String> {
    expect(
        &format!("{} paper body keeps its design", theme.name()),
        support::close(look.view_background, PAPER)
            && support::close(look.text, PAPER_TEXT)
            && look.body_background == first.body_background
            && look.link == first.link
            && look.corner == first.corner
            && look.scheme == "light",
        format!(
            "view {}, text {}, body background {:?}, link {:?}, corner pixel {}, scheme {}",
            look.view_background,
            look.text,
            look.body_background.map(|color| color.to_string()),
            look.link.map(|color| color.to_string()),
            look.corner,
            look.scheme
        ),
    )
}

/// The paper card's frame border follows the theme's border colour.
async fn check_frame(theme: &Theme, reader: &Reader) -> Result<(), String> {
    let frame = reader
        .web_view()
        .and_then(|view| view.ancestor(gtk::Box::static_type()))
        .ok_or("no paper frame")?;
    let border = support::painted_pixel(&frame, 0, frame.height() / 2).await?;
    expect(
        &format!("{} paper card border follows the theme", theme.name()),
        support::close(border, theme.ui.border) && frame.has_css_class("sw-paper"),
        format!("border pixel {border}, theme border {}", theme.ui.border),
    )
}

async fn scenario(
    controller: &Rc<ThemeController>,
    readers: &Readers,
    filled: &samples::Filled,
) -> Result<(), String> {
    let applied: Rc<RefCell<Vec<(String, Mode, Instant)>>> = Rc::default();
    controller.connect_changed(glib::clone!(
        #[strong]
        applied,
        move |theme| {
            applied
                .borrow_mut()
                .push((theme.name().to_owned(), theme.mode(), Instant::now()));
        }
    ));
    let themed_events: Rc<RefCell<Vec<ViewEvent>>> = Rc::default();
    let paper_events: Rc<RefCell<Vec<ViewEvent>>> = Rc::default();
    for (reader, events) in [
        (&readers.themed, &themed_events),
        (&readers.paper, &paper_events),
    ] {
        let events = events.clone();
        reader.connect_view_event(move |_, event| events.borrow_mut().push(event.clone()));
    }
    let themed_reports = support::shown_reports(&readers.themed);
    let paper_reports = support::shown_reports(&readers.paper);
    support::resize(readers.window.window(), 1440, 900).await?;
    support::resize(&readers.second, 820, 900).await?;
    let id = |name: &str| filled.id(name).ok_or(format!("no sample {name}"));
    let shown = support::show(&readers.themed, &themed_reports, id("simple")?).await?;
    println!(
        "THEME themed message shown: {:?} in {:?}",
        shown.body, shown.ready
    );
    let shown = support::show(&readers.paper, &paper_reports, id("newsletter")?).await?;
    println!(
        "THEME paper message shown: {:?} in {:?}",
        shown.body, shown.ready
    );
    support::frames(readers.window.window(), 2).await?;
    support::frames(&readers.second, 2).await?;
    mark(&readers.themed).await?;
    mark(&readers.paper).await?;
    let (web, network) = support::webkit_processes();
    let live = readers.themed.state().live_views;
    expect(
        "every view shares one web process (related-view)",
        web == 1 && network == 1 && live >= 2,
        format!("{live} live views, {web} web process(es), {network} network process(es)"),
    )?;

    let theme = controller.theme();
    expect(
        "the first theme is matte-black, dark",
        theme.name() == "matte-black" && theme.mode() == Mode::Dark,
        format!("{} {}", theme.name(), theme.mode().as_str()),
    )?;
    check_themed(&theme, &look(&readers.themed).await?)?;
    let first_paper = look(&readers.paper).await?;
    check_paper(&theme, &first_paper, &first_paper)?;
    check_frame(&theme, &readers.paper).await?;
    shots(readers, "dark")?;

    let state = state_path();
    for (name, mode) in [
        ("catppuccin-latte", Mode::Light),
        ("matte-black", Mode::Dark),
    ] {
        applied.borrow_mut().clear();
        let (themed_loads, paper_loads) = (loads(&themed_events), loads(&paper_events));
        let switched = Instant::now();
        install_theme(&state, name).map_err(|error| error.to_string())?;
        let seen = support::wait_until(Duration::from_secs(10), || {
            applied
                .borrow()
                .iter()
                .any(|(seen, seen_mode, _)| seen == name && *seen_mode == mode)
        })
        .await;
        if !seen {
            return Err(format!("{name} was never applied"));
        }
        let at = applied
            .borrow()
            .iter()
            .find(|(seen, _, _)| seen == name)
            .map(|(_, _, at)| *at)
            .unwrap_or(switched);
        let theme = controller.theme();
        let foreground = theme.palette.foreground;
        let mut restyled_ms = None;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            let look = look(&readers.themed).await?;
            if support::close(look.text, foreground)
                && support::close(look.corner, theme.palette.background)
            {
                restyled_ms = Some(at.elapsed().as_secs_f64() * 1000.0);
                break;
            }
            support::frames(readers.window.window(), 1).await?;
        }
        println!(
            "THEME {name} applied {:.1} ms after the swap (debounce 150 ms included); the \
             themed body rendered it {:.1} ms after that",
            at.duration_since(switched).as_secs_f64() * 1000.0,
            restyled_ms.unwrap_or(f64::NAN)
        );
        support::frames(readers.window.window(), 2).await?;
        support::frames(&readers.second, 2).await?;
        let themed = look(&readers.themed).await?;
        check_themed(&theme, &themed)?;
        let paper = look(&readers.paper).await?;
        check_paper(&theme, &paper, &first_paper)?;
        check_frame(&theme, &readers.paper).await?;
        expect(
            &format!("{name}: both documents were restyled in place, not reloaded"),
            themed.marker
                && paper.marker
                && loads(&themed_events) == themed_loads
                && loads(&paper_events) == paper_loads,
            format!(
                "markers {} and {}, loads {}→{} and {}→{}",
                themed.marker,
                paper.marker,
                themed_loads,
                loads(&themed_events),
                paper_loads,
                loads(&paper_events)
            ),
        )?;
        expect(
            &format!("{name}: libadwaita follows"),
            adw::StyleManager::default().is_dark() == (mode == Mode::Dark),
            format!("dark {}", adw::StyleManager::default().is_dark()),
        )?;
        shots(readers, theme.mode().as_str())?;
        if mode == Mode::Light {
            more_shots(readers, filled, &themed_reports, "light").await?;
            support::show(&readers.themed, &themed_reports, id("simple")?).await?;
            mark(&readers.themed).await?;
        }
    }
    more_shots(readers, filled, &themed_reports, "dark").await?;
    if controller.css_errors() != 0 {
        return Err(format!("{} CSS errors", controller.css_errors()));
    }
    Ok(())
}

fn shots(readers: &Readers, mode: &str) -> Result<(), String> {
    let themed = support::output(&format!("reader-themed-{mode}.png"));
    support::screenshot(readers.window.window(), &themed)?;
    let paper = support::output(&format!("reader-paper-{mode}.png"));
    support::screenshot(&readers.second, &paper)?;
    println!("THEME screenshots {} {}", themed.display(), paper.display());
    Ok(())
}

/// The other samples in the window, for the preview.
async fn more_shots(
    readers: &Readers,
    filled: &samples::Filled,
    reports: &async_channel::Receiver<sealwax_app::reader::Shown>,
    mode: &str,
) -> Result<(), String> {
    for name in [
        "newsletter",
        "plain",
        "attachments",
        "cid",
        "remote",
        "dark-promo",
    ] {
        let id = filled.id(name).ok_or(format!("no sample {name}"))?;
        support::show(&readers.themed, reports, id).await?;
        support::frames(readers.window.window(), 3).await?;
        let path = support::output(&format!("reader-{name}-{mode}.png"));
        support::screenshot(readers.window.window(), &path)?;
        println!("THEME screenshot {}", path.display());
    }
    Ok(())
}
