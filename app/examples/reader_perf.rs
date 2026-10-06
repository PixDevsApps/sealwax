//! Dry run of the reader's open time (ML-027: a cached message opens in ≤ 150 ms p95),
//! headless. It opens the newest messages of a store that `sealwax-fixtures import` filled, one
//! after another in the real window, twice, and times `Reader::show` → the page on screen (an
//! HTML body laid out at its width, its images in) → the next frame GTK painted. Headless
//! numbers come from software rendering under Broadway; the live measurement is separate.
//!
//! ```sh
//! cargo build --release -p sealwax-fixtures -p sealwax --example reader_perf
//! target/release/sealwax-fixtures import --count 2000 --seed 1 \
//!     --store .local/reader-examples/perf/store.sqlite
//! SEALWAX_HEADLESS_TIMEOUT=600 tools/headless.sh 37 target/release/examples/reader_perf \
//!     --store .local/reader-examples/perf/store.sqlite --count 200
//! ```

#[path = "reader_support/mod.rs"]
mod support;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::reader::{Reader, Shown, ShownBody};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_domain::theme::ThemeOptions;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions, ViewSpec};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use support::broadway;

struct Args {
    store: PathBuf,
    count: u32,
    /// Opens back to back, without letting the window settle in between.
    back_to_back: bool,
}

fn args() -> Result<Args, String> {
    let mut store = None;
    let mut count = 200;
    let mut back_to_back = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--store" => store = args.next().map(PathBuf::from),
            "--back-to-back" => back_to_back = true,
            "--count" => {
                count = args
                    .next()
                    .and_then(|count| count.parse().ok())
                    .ok_or("--count needs a number")?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        store: store.ok_or("--store PATH (a store sealwax-fixtures imported) is needed")?,
        count,
        back_to_back,
    })
}

fn main() -> glib::ExitCode {
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let args = match args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return glib::ExitCode::from(2);
        }
    };
    let input = match broadway::connect() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("run this through tools/headless.sh: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let Some(directory) = args.store.parent() else {
        eprintln!("--store needs a folder");
        return glib::ExitCode::from(2);
    };
    let blobs = match BlobStore::open(&directory.join("blobs")) {
        Ok(blobs) => blobs,
        Err(error) => {
            eprintln!("no blob store beside the store: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let store = match Store::open(
        &args.store,
        StoreOptions {
            blobs: Some(blobs),
            ..StoreOptions::default()
        },
    ) {
        Ok(store) => Rc::new(store),
        Err(error) => {
            eprintln!("could not open the store: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let cache = support::scratch("perf-cache");
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ReaderPerf")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let reader = Reader::new(store.clone(), theme.clone(), &Keymap::default(), &cache);
            let window = support::window(app, &reader, (1440, 900));
            window.present();
            let app = app.clone();
            let passed = passed.clone();
            let store = store.clone();
            let _input = input.clone();
            let count = args.count;
            let back_to_back = args.back_to_back;
            glib::spawn_future_local(async move {
                let _keep = &theme;
                match measure(&store, &reader, &window, count, back_to_back).await {
                    Ok(ok) => passed.set(ok),
                    Err(error) => println!("PERF error {error}"),
                }
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

async fn ids(store: &Store, count: u32) -> Result<Vec<MessageId>, String> {
    let folders = store
        .folders()
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    let inbox = folders
        .value
        .iter()
        .find(|folder| folder.role == FolderRole::Inbox)
        .ok_or("the store has no inbox")?;
    let page = store
        .page(ViewSpec::folder(inbox.id), 0, count)
        .recv()
        .await
        .map_err(|error| error.to_string())?;
    Ok(page.rows.into_iter().map(|row| row.id).collect())
}

fn kind(body: &ShownBody) -> &'static str {
    match body {
        ShownBody::Html {
            style: sealwax_domain::theme::BodyStyle::Paper,
            ..
        } => "html-paper",
        ShownBody::Html { .. } => "html-themed",
        ShownBody::Plain => "plain",
        ShownBody::TextFallback => "text-fallback",
        ShownBody::Empty => "empty",
        ShownBody::NotDownloaded => "not-downloaded",
        ShownBody::Downloading => "downloading",
        ShownBody::DownloadFailed(_) => "download-failed",
        ShownBody::Missing => "missing",
        ShownBody::Unreadable(_) => "unreadable",
    }
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    values[rank.clamp(1, values.len()) - 1]
}

fn summary(label: &str, values: &[f64]) {
    if values.is_empty() {
        println!("PERF {label} n=0");
        return;
    }
    let mut values = values.to_vec();
    println!(
        "PERF {label} n={} p50={:.1} p95={:.1} max={:.1} ms",
        values.len(),
        percentile(&mut values, 50.0),
        percentile(&mut values, 95.0),
        percentile(&mut values, 100.0)
    );
}

async fn measure(
    store: &Store,
    reader: &Reader,
    window: &sealwax_app::window::Window,
    count: u32,
    back_to_back: bool,
) -> Result<bool, String> {
    let reports = support::shown_reports(reader);
    support::resize(window.window(), 1440, 900).await?;
    support::sleep(1500).await;
    let ids = ids(store, count).await?;
    println!(
        "PERF ENV messages={} live_views_before={} build={} mode={}",
        ids.len(),
        reader.state().live_views,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        if back_to_back {
            "back-to-back"
        } else {
            "settled (two frames and 250 ms before each open)"
        }
    );
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let mut ok = true;
    for pass in 1..=2 {
        let mut ready: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        let mut painted: Vec<f64> = Vec::new();
        let mut all_ready: Vec<f64> = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let (sender, paints) = async_channel::unbounded::<Instant>();
            let handler = clock.connect_after_paint(move |_| {
                let _ = sender.try_send(Instant::now());
            });
            if !back_to_back {
                support::frames(window.window(), 2).await?;
                support::sleep(250).await;
            }
            let started = Instant::now();
            let shown: Shown = support::show(reader, &reports, *id).await?;
            let on_screen = started + shown.ready;
            window.window().queue_draw();
            let mut first_paint = None;
            while let Ok(Ok(at)) =
                glib::future_with_timeout(Duration::from_secs(2), paints.recv()).await
            {
                if at >= on_screen {
                    first_paint = Some(at);
                    break;
                }
            }
            clock.disconnect(handler);
            let ready_ms = shown.ready.as_secs_f64() * 1000.0;
            if std::env::var_os("READER_PERF_ROWS").is_some() {
                println!(
                    "ROW pass={pass} message={} kind={} fetched={:.1} prepared={:.1} ready={:.1} \
                     paint={}",
                    id.0,
                    kind(&shown.body),
                    shown.fetched.as_secs_f64() * 1000.0,
                    shown.prepared.as_secs_f64() * 1000.0,
                    ready_ms,
                    first_paint.map_or("-".to_owned(), |at| format!(
                        "{:.1}",
                        at.duration_since(started).as_secs_f64() * 1000.0
                    ))
                );
            }
            if pass == 1 && index == 0 {
                println!(
                    "PERF first open (web process {}): ready {:.1} ms",
                    if reader.state().live_views > 0 {
                        "started at map"
                    } else {
                        "not started"
                    },
                    ready_ms
                );
            }
            ready.entry(kind(&shown.body)).or_default().push(ready_ms);
            all_ready.push(ready_ms);
            if let Some(at) = first_paint {
                painted.push(at.duration_since(started).as_secs_f64() * 1000.0);
            }
        }
        summary(&format!("pass={pass} show→on-screen all"), &all_ready);
        for (kind, values) in &ready {
            summary(&format!("pass={pass} show→on-screen {kind}"), values);
        }
        summary(&format!("pass={pass} show→first paint after"), &painted);
        let mut values = all_ready.clone();
        if pass == 2 && percentile(&mut values, 95.0) > 150.0 {
            ok = false;
        }
    }
    println!(
        "PERF live views at the end: {} (cap {})",
        reader.state().live_views,
        sealwax_app::reader::MAX_LIVE_VIEWS
    );
    println!("PERF GLib criticals: {}", support::criticals());
    Ok(ok && support::criticals() == 0)
}
