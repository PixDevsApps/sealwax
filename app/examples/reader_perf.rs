//! Dry run of the reader's open time (ML-027: a cached message opens in ≤ 150 ms p95),
//! headless. It opens the newest messages of a store that `sealwax-fixtures import` filled, one
//! after another in the real window, twice, and times `Reader::show` → the conversation on screen
//! (the HTML bodies its first screen shows laid out at their width, their images in) → the next
//! frame GTK painted, also by the conversation's size. Then (ML-057) it opens the sample
//! conversations (`reader::samples`: six messages, ten with three newsletters, eight newsletters,
//! 150 messages) from a scratch store, each ten times. Headless numbers come from software
//! rendering under Broadway; the live measurement is separate.
//!
//! ```sh
//! cargo build --release -p sealwax-fixtures -p sealwax --example reader_perf
//! target/release/sealwax-fixtures import --count 2000 --seed 1 \
//!     --store .local/reader-examples/perf/store.sqlite
//! SEALWAX_HEADLESS_TIMEOUT=600 tools/headless.sh 37 target/release/examples/reader_perf \
//!     --store .local/reader-examples/perf/store.sqlite --count 200   # or --ids 12,34
//! ```

#[path = "reader_support/mod.rs"]
mod support;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::reader::{Reader, Shown, ShownBody, samples};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_domain::theme::ThemeOptions;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{FolderSpec, NewAccount, Store, StoreOptions, ViewSpec};
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
    /// These messages instead of the Inbox's newest.
    ids: Vec<MessageId>,
}

fn args() -> Result<Args, String> {
    let mut store = None;
    let mut count = 200;
    let mut back_to_back = false;
    let mut ids = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--store" => store = args.next().map(PathBuf::from),
            "--back-to-back" => back_to_back = true,
            "--ids" => {
                ids = args
                    .next()
                    .ok_or("--ids needs message ids")?
                    .split(',')
                    .map(|id| id.trim().parse().map(MessageId))
                    .collect::<Result<_, _>>()
                    .map_err(|_| "--ids takes message ids separated by commas")?;
            }
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
        ids,
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
            let chosen = args.ids.clone();
            let cache = cache.clone();
            glib::spawn_future_local(async move {
                let fixtures =
                    measure(&store, &reader, &window, count, &chosen, back_to_back).await;
                reader.clear();
                let samples = conversations(&theme, &cache, &window).await;
                match (fixtures, samples) {
                    (Ok(fixtures), Ok(())) => passed.set(fixtures && support::criticals() == 0),
                    (Err(error), _) | (_, Err(error)) => println!("PERF error {error}"),
                }
                println!("PERF GLib criticals: {}", support::criticals());
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
    chosen: &[MessageId],
    back_to_back: bool,
) -> Result<bool, String> {
    let reports = support::shown_reports(reader);
    support::resize(window.window(), 1440, 900).await?;
    support::sleep(1500).await;
    let ids = if chosen.is_empty() {
        ids(store, count).await?
    } else {
        chosen.to_vec()
    };
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
    let mut ok = true;
    for pass in 1..=2 {
        let mut ready: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        let mut sizes: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        let mut painted: Vec<f64> = Vec::new();
        let mut all_ready: Vec<f64> = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let (shown, first_paint) =
                open_timed(reader, &reports, window, *id, back_to_back).await?;
            let ready_ms = shown.ready.as_secs_f64() * 1000.0;
            if std::env::var_os("READER_PERF_ROWS").is_some() {
                println!(
                    "ROW pass={pass} message={} kind={} messages={} expanded={} fetched={:.1} \
                     prepared={:.1} ready={ready_ms:.1} paint={}",
                    id.0,
                    kind(&shown.body),
                    shown.messages,
                    shown.expanded,
                    shown.fetched.as_secs_f64() * 1000.0,
                    shown.prepared.as_secs_f64() * 1000.0,
                    first_paint.map_or("-".to_owned(), |paint| format!("{paint:.1}"))
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
            sizes
                .entry(size(shown.messages))
                .or_default()
                .push(ready_ms);
            all_ready.push(ready_ms);
            painted.extend(first_paint);
        }
        summary(&format!("pass={pass} show→on-screen all"), &all_ready);
        for (kind, values) in &ready {
            summary(&format!("pass={pass} show→on-screen {kind}"), values);
        }
        for (size, values) in &sizes {
            summary(&format!("pass={pass} show→on-screen {size}"), values);
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
    Ok(ok)
}

/// The conversation's size, for the summary lines.
fn size(messages: usize) -> &'static str {
    match messages {
        0 | 1 => "1-message conversations",
        2..=5 => "2-5-message conversations",
        _ => "conversations of 6 messages or more",
    }
}

/// Opens `id` (after letting the window settle, unless `back_to_back`) and times it: the
/// report, and `show` → the first frame GTK painted with it on screen (ms).
async fn open_timed(
    reader: &Reader,
    reports: &async_channel::Receiver<Shown>,
    window: &sealwax_app::window::Window,
    id: MessageId,
    back_to_back: bool,
) -> Result<(Shown, Option<f64>), String> {
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let (sender, paints) = async_channel::unbounded::<Instant>();
    let handler = clock.connect_after_paint(move |_| {
        let _ = sender.try_send(Instant::now());
    });
    if !back_to_back {
        support::frames(window.window(), 2).await?;
        support::sleep(250).await;
    }
    let started = Instant::now();
    let shown = support::show(reader, reports, id).await?;
    let on_screen = started + shown.ready;
    window.window().queue_draw();
    let mut first_paint = None;
    while let Ok(Ok(at)) = glib::future_with_timeout(Duration::from_secs(2), paints.recv()).await {
        if at >= on_screen {
            first_paint = Some(at.duration_since(started).as_secs_f64() * 1000.0);
            break;
        }
    }
    clock.disconnect(handler);
    Ok((shown, first_paint))
}

/// The sample conversations (ML-057) from a scratch store, in a second reader in the window,
/// each opened ten times at its latest message (and the six-message one at its second, whose
/// conversation opens scrolled to it).
async fn conversations(
    theme: &Rc<ThemeController>,
    cache: &std::path::Path,
    window: &sealwax_app::window::Window,
) -> Result<(), String> {
    let root = support::scratch("perf-conversations");
    let scratch = support::store(&root);
    let account = scratch
        .store
        .add_account(NewAccount {
            name: samples::ACCOUNT_NAME.into(),
            address: samples::ACCOUNT_ADDRESS.into(),
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?
        .value;
    let mut folders = Vec::new();
    for (name, role) in [("INBOX", FolderRole::Inbox), ("Sent", FolderRole::Sent)] {
        let reply = scratch
            .store
            .upsert_folder(FolderSpec::local(account, name, role));
        folders.push(reply.recv().await.map_err(|error| error.to_string())?.value);
    }
    let now = glib::real_time() / 1_000_000;
    let mut opened = Vec::new();
    for (name, latest) in [
        ("trip", "trip-6"),
        ("trip", "trip-2"),
        ("typical", "typical-10"),
        ("letters", "letters-8"),
        ("long", "long-150"),
    ] {
        if opened.iter().all(|(filed, _): &(&str, _)| *filed != name) {
            let filed = samples::fill_conversation(
                &scratch.store,
                &scratch.blobs,
                folders[0],
                folders[1],
                samples::conversation(name, now),
            )
            .map_err(|error| error.to_string())?;
            opened.push((name, filed));
        }
        let (_, filed) = opened
            .iter()
            .find(|(filed, _)| *filed == name)
            .ok_or("filed above")?;
        let id = filed.id(latest).ok_or("the sample's message is missing")?;
        let reader = Reader::new(
            scratch.store.clone(),
            theme.clone(),
            &Keymap::default(),
            cache,
        );
        window.set_reader(&reader.widget());
        let reports = support::shown_reports(&reader);
        let mut ready = Vec::new();
        let mut painted = Vec::new();
        let mut shape = (0, 0);
        for _ in 0..10 {
            reader.clear();
            let (shown, paint) = open_timed(&reader, &reports, window, id, false).await?;
            ready.push(shown.ready.as_secs_f64() * 1000.0);
            painted.extend(paint);
            shape = (shown.messages, shown.expanded);
        }
        summary(
            &format!(
                "conversation {latest} ({} messages, {} expanded) show→on-screen",
                shape.0, shape.1
            ),
            &ready,
        );
        summary(
            &format!("conversation {latest} show→first paint after"),
            &painted,
        );
        reader.clear();
    }
    Ok(())
}
