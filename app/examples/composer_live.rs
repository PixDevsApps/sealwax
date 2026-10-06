//! The composer's two open checks from S7 (ADR-014), in the real composer's editor: an edit to
//! the painted frame without acceleration, in a small document and above a long quoted thread,
//! and scrolling that thread. S7's method: each edit is WebKit's `InsertText` editing command
//! (no key event, nothing injected into the session), a paint probe around the editor's WebView
//! notes the first frame the WebView drew after it, and the frame clock's `after-paint` ends
//! that frame. Scrolling moves the document a fixed distance every frame from the frame clock
//! and records the intervals between the WebView's frames.
//!
//! Headless it is a dry run (Broadway paints slowly); the numbers that count come from the
//! lead's authorized live run. It writes only under `.local/`, makes no input, uses a scratch
//! theme and scratch XDG folders, and ends by itself.
//!
//! ```sh
//! cargo build --release -p sealwax --example composer_live
//! tools/headless.sh 45 target/release/examples/composer_live            # dry run
//! target/release/examples/composer_live --live                          # authorized live run
//! ```

#[path = "../../spikes/s7-composer/src/probe.rs"]
#[allow(dead_code)]
mod probe;
#[path = "reader_support/mod.rs"]
mod support;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use probe::{Paint, PaintProbe};
use sealwax_app::composer::editor::{Editor, EditorEngine, WORLD};
use sealwax_domain::theme::{Mode, Theme, ThemeInputs, ThemeOptions};
use std::cell::RefCell;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::prelude::*;

/// The run ends by itself after this long, whatever happens.
const DEADLINE: Duration = Duration::from_secs(90);
/// The WebView counts as quiet after this long without a frame (S7).
const QUIET: Duration = Duration::from_millis(50);
/// S7's bar: an edit's first painted frame within one 60 Hz frame.
const FRAME: f64 = 16.7;
/// Scrolling speed, px per second, and how long a scroll runs.
const SCROLL_SPEED: f64 = 1_500.0;
const SCROLL_TIME: Duration = Duration::from_secs(4);

#[derive(Clone, Copy)]
struct Args {
    live: bool,
    small: usize,
    large: usize,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        live: false,
        small: 80,
        large: 40,
    };
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--live" => args.live = true,
            other => match other.split_once('=') {
                Some(("--small", n)) => args.small = n.parse().map_err(|_| "--small=N")?,
                Some(("--large", n)) => args.large = n.parse().map_err(|_| "--large=N")?,
                _ => return Err(format!("unknown argument {other}")),
            },
        }
    }
    Ok(args)
}

fn scratch_environment() -> Result<PathBuf, String> {
    let root = support::scratch("composer-live");
    let xdg = root.join("xdg");
    for dir in ["config", "cache", "data", "state"] {
        fs::create_dir_all(xdg.join(dir)).map_err(|error| error.to_string())?;
    }
    let vars: [(&str, PathBuf); 6] = [
        ("XDG_CONFIG_HOME", xdg.join("config")),
        ("XDG_CACHE_HOME", xdg.join("cache")),
        ("XDG_DATA_HOME", xdg.join("data")),
        ("XDG_STATE_HOME", xdg.join("state")),
        ("__GL_SHADER_DISK_CACHE_PATH", xdg.join("cache")),
        ("MESA_SHADER_CACHE_DIR", xdg.join("cache")),
    ];
    for (name, value) in vars {
        // SAFETY: called first thing in main, before any other thread starts.
        unsafe { std::env::set_var(name, value) };
    }
    Ok(root)
}

/// S7's long quoted thread: 1,500 paragraphs with links and lists below a reply line.
fn large_quote() -> String {
    let mut html =
        String::from("<div><br></div><div>Svar over sitatet.</div><blockquote type=\"cite\">");
    for index in 0..1500 {
        write!(
            html,
            "<div>Sitert avsnitt {index}: blåbær, multebær og tyttebær i en lang tråd, med en \
             <a href=\"https://example.org/{index}\">lenke</a>.</div>"
        )
        .unwrap();
        if index % 100 == 0 {
            html.push_str("<ul><li>punkt</li><li>punkt</li></ul>");
        }
    }
    html.push_str("</blockquote>");
    html
}

fn percentile(values: &[f64], fraction: f64) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.saturating_sub(1)).copied()
}

fn summary(values: &[f64]) -> String {
    let fmt = |value: Option<f64>| value.map_or_else(|| "–".into(), |v| format!("{v:.2}"));
    format!(
        "n={} p50 {} p95 {} max {} ms",
        values.len(),
        fmt(percentile(values, 0.5)),
        fmt(percentile(values, 0.95)),
        fmt(values.iter().copied().reduce(f64::max))
    )
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

async fn next_paint(paints: &async_channel::Receiver<Paint>, after: Instant) -> Option<Paint> {
    let deadline = after + Duration::from_secs(1);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match glib::future_with_timeout(remaining, paints.recv()).await {
            Ok(Ok(paint)) if paint.snapshot_at >= after => return Some(paint),
            Ok(Ok(_)) => {}
            _ => return None,
        }
    }
}

/// Frames that follow each other with gaps under `gap`, for at most `limit`.
async fn burst(paints: &async_channel::Receiver<Paint>, gap: Duration, limit: Duration) -> usize {
    let deadline = Instant::now() + limit;
    let mut count = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now()).min(gap);
        if remaining.is_zero() {
            return count;
        }
        match glib::future_with_timeout(remaining, paints.recv()).await {
            Ok(Ok(_)) => count += 1,
            _ => return count,
        }
    }
}

struct Series {
    first_paint: Vec<f64>,
    first_snapshot: Vec<f64>,
    timeouts: u32,
}

/// One character per sample, each once the WebView has been quiet for `QUIET`.
async fn typing(
    editor: &Editor,
    paints: &async_channel::Receiver<Paint>,
    samples: usize,
) -> Series {
    const TEXT: &str = "Blåbærsyltetøy på brødskiva, takk! ";
    let text: Vec<char> = TEXT.chars().collect();
    let mut series = Series {
        first_paint: Vec::new(),
        first_snapshot: Vec::new(),
        timeouts: 0,
    };
    burst(paints, Duration::from_millis(80), Duration::from_secs(2)).await;
    for index in 0..samples {
        if index > 0 && index % 60 == 0 {
            editor.command("InsertParagraph");
            burst(paints, Duration::from_millis(80), Duration::from_secs(2)).await;
        }
        let started = Instant::now();
        editor.command_with("InsertText", &text[index % text.len()].to_string());
        let Some(first) = next_paint(paints, started).await else {
            series.timeouts += 1;
            continue;
        };
        series.first_paint.push(ms(first.painted_at - started));
        series.first_snapshot.push(ms(first.snapshot_at - started));
        burst(paints, QUIET, Duration::from_secs(2)).await;
    }
    series
}

/// Scrolls the document by `SCROLL_SPEED` px/s from the frame clock for `SCROLL_TIME` and
/// returns the intervals between the WebView's frames and how far it got.
async fn scroll(
    editor: &Editor,
    probe: &PaintProbe,
    paints: &async_channel::Receiver<Paint>,
) -> (Vec<f64>, f64) {
    editor.eval_detached("document.scrollingElement.scrollTop = 0");
    burst(paints, Duration::from_millis(120), Duration::from_secs(2)).await;
    while paints.try_recv().is_ok() {}
    let start = Rc::new(RefCell::new(None::<i64>));
    let view = editor.view().clone();
    let tick_start = start.clone();
    let tick = probe.add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        let begun = *tick_start.borrow_mut().get_or_insert(now);
        let elapsed = (now - begun) as f64 / 1_000_000.0;
        let top = elapsed * SCROLL_SPEED;
        view.evaluate_javascript(
            &format!("document.scrollingElement.scrollTop = {top:.1}"),
            Some(WORLD),
            None,
            None::<&gio::Cancellable>,
            |_| {},
        );
        glib::ControlFlow::Continue
    });
    let deadline = Instant::now() + SCROLL_TIME;
    let mut painted = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Ok(Ok(paint)) = glib::future_with_timeout(remaining, paints.recv()).await {
            painted.push(paint.painted_at);
        }
    }
    tick.remove();
    let reached = editor
        .eval("document.scrollingElement.scrollTop")
        .await
        .ok()
        .map_or(0.0, |value| value.to_double());
    let intervals = painted
        .windows(2)
        .map(|pair| ms(pair[1] - pair[0]))
        .collect();
    (intervals, reached)
}

/// Puts the caret at the end of the body's `line`th block, where the edits go.
async fn caret_at_line(editor: &Editor, line: usize) {
    editor.eval_detached(&format!(
        "(() => {{ const line = document.body.children[{line}]; \
         const range = document.createRange(); range.selectNodeContents(line); \
         range.collapse(false); const selection = getSelection(); \
         selection.removeAllRanges(); selection.addRange(range); }})()"
    ));
    support::sleep(200).await;
}

async fn run(app: &adw::Application, args: Args, root: PathBuf) -> Result<String, String> {
    let theme = Theme::new(&ThemeInputs::builtin(Mode::Dark), &ThemeOptions::default());
    // Headless, a Broadway client keeps the frames coming (without one GTK paints once a second).
    let _client = (!args.live)
        .then(|| {
            std::env::var_os("BROADWAY_DISPLAY").and_then(|_| support::broadway::connect().ok())
        })
        .flatten();
    // S7's condition: no caret blink, so an idle WebView draws nothing (this process only).
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_cursor_blink(false);
    }
    let engine = EditorEngine::shared(&root.join("xdg/cache"));
    let editor = Editor::new(&engine, &theme);
    let (probe, paints) = PaintProbe::new(editor.view());
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Sealwax composer check")
        .default_width(1000)
        .default_height(820)
        .content(&probe)
        .build();
    window.present();
    let filter = engine
        .filter()
        .await
        .ok_or("the content filter didn't compile")?;
    editor.load(&filter);
    if !support::wait_until(Duration::from_secs(10), || editor.is_ready()).await {
        return Err("the editor page never became ready".into());
    }
    support::sleep(500).await;
    let mut report = String::new();
    let surface = window.surface();
    let monitor = surface
        .as_ref()
        .and_then(|surface| gtk::prelude::WidgetExt::display(&window).monitor_at_surface(surface));
    writeln!(
        report,
        "ENV backend {} renderer {} window {}×{} webview {}×{} scale {} monitor {}",
        if args.live { "live" } else { "broadway" },
        window
            .renderer()
            .map_or("–".into(), |renderer| renderer.type_().name().to_string()),
        window.width(),
        window.height(),
        editor.view().width(),
        editor.view().height(),
        window.scale_factor(),
        monitor.map_or("–".into(), |monitor| format!(
            "{}×{} at {:.0} Hz",
            monitor.geometry().width(),
            monitor.geometry().height(),
            monitor.refresh_rate() as f64 / 1000.0
        )),
    )
    .unwrap();
    editor.view().grab_focus();

    editor.set_content("<div><br></div>").await;
    caret_at_line(&editor, 0).await;
    let small = typing(&editor, &paints, args.small).await;
    writeln!(
        report,
        "LATENCY small document, edit → painted WebView frame: {} (edit → snapshot {}); \
         over {FRAME} ms: {}; timeouts {}",
        summary(&small.first_paint),
        summary(&small.first_snapshot),
        small
            .first_paint
            .iter()
            .filter(|value| **value > FRAME)
            .count(),
        small.timeouts
    )
    .unwrap();

    let quote = large_quote();
    editor.set_content(&quote).await;
    support::sleep(800).await;
    caret_at_line(&editor, 1).await;
    let large = typing(&editor, &paints, args.large).await;
    writeln!(
        report,
        "LATENCY above a {} KB quoted thread, edit → painted WebView frame: {} (edit → snapshot \
         {}); over {FRAME} ms: {}; timeouts {}",
        quote.len() / 1000,
        summary(&large.first_paint),
        summary(&large.first_snapshot),
        large
            .first_paint
            .iter()
            .filter(|value| **value > FRAME)
            .count(),
        large.timeouts
    )
    .unwrap();

    let (intervals, reached) = scroll(&editor, &probe, &paints).await;
    writeln!(
        report,
        "SCROLL the quoted thread at {SCROLL_SPEED:.0} px/s for {} s: frame interval {}; over \
         {FRAME} ms: {}; reached {reached:.0} px",
        SCROLL_TIME.as_secs(),
        summary(&intervals),
        intervals.iter().filter(|value| **value > FRAME).count(),
    )
    .unwrap();
    window.close();
    Ok(report)
}

fn main() -> glib::ExitCode {
    let args = match args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return glib::ExitCode::from(2);
        }
    };
    let broadway = std::env::var_os("GDK_BACKEND").is_some_and(|backend| backend == "broadway");
    if !broadway && !args.live {
        eprintln!(
            "composer_live opens a window: run it through tools/headless.sh, or pass --live for \
             an authorized measurement on the live session"
        );
        return glib::ExitCode::from(2);
    }
    let root = match scratch_environment() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("could not prepare the scratch folders: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ComposerCheck")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let failed = Rc::new(std::cell::Cell::new(false));
    let done = failed.clone();
    app.connect_activate(move |app| {
        let app = app.clone();
        let failed = done.clone();
        let root = root.clone();
        let hold = app.hold();
        glib::timeout_add_local_once(DEADLINE, || {
            eprintln!(
                "composer_live: the {} s deadline passed",
                DEADLINE.as_secs()
            );
            std::process::exit(1);
        });
        glib::spawn_future_local(async move {
            let _hold = hold;
            match run(&app, args, root).await {
                Ok(report) => {
                    print!("{report}");
                    let stamp = glib::DateTime::now_local()
                        .ok()
                        .and_then(|now| now.format("%Y%m%d-%H%M%S").ok())
                        .map_or_else(|| "run".to_owned(), |text| text.to_string());
                    let path = support::output(&format!("composer-live-{stamp}.txt"));
                    if let Some(parent) = path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    if fs::write(&path, &report).is_ok() {
                        println!("REPORT {}", path.display());
                    }
                }
                Err(error) => {
                    eprintln!("composer_live: {error}");
                    failed.set(true);
                }
            }
            app.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
    if failed.get() {
        glib::ExitCode::FAILURE
    } else {
        glib::ExitCode::SUCCESS
    }
}
