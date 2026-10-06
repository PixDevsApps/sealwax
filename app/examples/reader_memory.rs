//! The reader's memory harness (M5's exit criterion; ADR-011's memory decision: process-tree
//! PSS ≤ 300 MB with a typical 10-message conversation open, live views capped, < 20 MB PSS
//! growth after opening 200 messages). It builds its own scratch store and scratch config, data,
//! state, cache and theme folders under the repository's `.local/reader-examples/memory/`, fills the store
//! with synthetic messages only (`reader::samples`: the ten-message `typical` conversation, three
//! styled newsletters among them, and single messages mixed as a mailbox mixes them), opens the
//! conversation in the real window (its unread messages and the latest expanded), scrolls it top
//! to bottom by setting the column's scroll (no input is injected), and measures:
//!
//! - PSS and RSS of the process tree: this process and every descendant (WebKit's web processes
//!   under bubblewrap, its network process), per process, from `/proc/<pid>/smaps_rollup`, with
//!   each process's Pss split by what it maps (`/proc/<pid>/smaps`: the GPU driver and its device
//!   mappings apart);
//! - the live views at each step of the scroll;
//! - the growth after opening 200 messages one after another, after a warm-up of 20.
//!
//! It prints one `READER_MEMORY …` summary line, writes nothing outside `.local/`, and exits
//! non-zero when a check fails: the conversation as expected, at most `MAX_LIVE_VIEWS` live views
//! at every step, nothing blank on screen once a scroll settles, every process measured, no GLib
//! critical. `--judge` also makes the bars checks (headless runs aren't judged). It runs headless
//! through `tools/headless.sh`, and on a live session only with `--live` (the lead's authorized
//! M5 measurement):
//!
//! ```sh
//! cargo build --release -p sealwax --example reader_memory
//! SEALWAX_HEADLESS_TIMEOUT=600 tools/headless.sh 31 target/release/examples/reader_memory
//! target/release/examples/reader_memory --live --judge   # live session, authorized runs only
//! ```

#[path = "reader_support/mod.rs"]
mod support;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::reader::{MAX_LIVE_VIEWS, Reader, samples};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_domain::theme::ThemeOptions;
use sealwax_infrastructure::omarchy::STATE_DIR_ENV;
use sealwax_infrastructure::store::{FolderSpec, NewAccount};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The bars (TESTING.md "Performance targets"), in bytes.
const TREE_BAR: f64 = 300e6;
const GROWTH_BAR: f64 = 20e6;
const MIB: f64 = 1_048_576.0;
/// Messages opened before the growth's baseline, and after it.
const WARM_UP: usize = 20;
const OPENED: usize = 200;
const WINDOW: (i32, i32) = (1440, 900);

struct Args {
    live: bool,
    judge: bool,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        live: false,
        judge: false,
    };
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--live" => args.live = true,
            "--judge" => args.judge = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

/// The scratch root and the environment that keeps every file this run writes in it: XDG
/// folders, the GPU driver's shader cache, settings in memory, and a stock theme in a scratch
/// Omarchy state. Set before GTK or WebKit start.
fn scratch_environment() -> Result<PathBuf, String> {
    let root = support::scratch("memory");
    let xdg = root.join("xdg");
    let state = root.join("omarchy-state");
    for dir in ["config", "cache", "data", "state"] {
        fs::create_dir_all(xdg.join(dir)).map_err(|error| error.to_string())?;
    }
    let theme = state.join("current/theme");
    fs::create_dir_all(&theme).map_err(|error| error.to_string())?;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/theme");
    fs::copy(
        fixtures.join("matte-black/colors.toml"),
        theme.join("colors.toml"),
    )
    .map_err(|error| error.to_string())?;
    fs::copy(fixtures.join("shell.toml"), theme.join("shell.toml"))
        .map_err(|error| error.to_string())?;
    fs::write(state.join("current/theme.name"), "matte-black\n")
        .map_err(|error| error.to_string())?;
    let vars: [(&str, PathBuf); 7] = [
        ("XDG_CONFIG_HOME", xdg.join("config")),
        ("XDG_CACHE_HOME", xdg.join("cache")),
        ("XDG_DATA_HOME", xdg.join("data")),
        ("XDG_STATE_HOME", xdg.join("state")),
        ("__GL_SHADER_DISK_CACHE_PATH", xdg.join("cache")),
        ("MESA_SHADER_CACHE_DIR", xdg.join("cache")),
        (STATE_DIR_ENV, state),
    ];
    for (name, value) in vars {
        // SAFETY: called first thing in main, before any other thread starts.
        unsafe { std::env::set_var(name, value) };
    }
    // SAFETY: as above.
    unsafe { std::env::set_var("GSETTINGS_BACKEND", "memory") };
    Ok(root)
}

/// One process of the tree, measured.
#[derive(Debug, Clone, Default)]
struct Measured {
    pid: u32,
    name: String,
    rss: f64,
    pss: f64,
    pss_anon: f64,
    pss_file: f64,
    pss_shmem: f64,
    /// Pss of the GPU driver, its GL and Vulkan libraries and device mappings.
    gpu: f64,
}

/// This process and its descendants, from `/proc`.
fn tree() -> Vec<(u32, String)> {
    let mut parents = HashMap::new();
    let mut names = HashMap::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let ppid = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|ppid| ppid.parse::<u32>().ok())
            .unwrap_or(0);
        parents.insert(pid, ppid);
        names.insert(pid, stat[open + 1..close].to_owned());
    }
    let me = std::process::id();
    let mut out: Vec<(u32, String)> = names
        .iter()
        .filter(|(pid, _)| {
            let mut at = **pid;
            loop {
                if at == me {
                    return true;
                }
                match parents.get(&at) {
                    Some(&parent) if parent > 1 => at = parent,
                    _ => return false,
                }
            }
        })
        .map(|(pid, name)| (*pid, name.clone()))
        .collect();
    out.sort();
    out
}

fn field(text: &str, key: &str) -> f64 {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0)
        * 1024.0
}

/// Whether a mapping belongs to the GPU stack: device files, driver, GL and Vulkan libraries
/// (as `perf-memory`'s split, app/src/selftest/perf.rs).
fn gpu_mapping(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    (path.starts_with("/dev/") && !lower.starts_with("/dev/shm"))
        || [
            "nvidia",
            "vulkan",
            "libegl",
            "libglx",
            "libgldispatch",
            "libgl.so",
            "libgles",
            "libopengl",
            "libllvm",
            "spirv",
            "libgbm",
            "libdrm",
            "mesa",
            "/dri/",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn gpu_share(pid: u32) -> f64 {
    let Ok(smaps) = fs::read_to_string(format!("/proc/{pid}/smaps")) else {
        return 0.0;
    };
    let mut path = String::new();
    let mut total = 0.0;
    for line in smaps.lines() {
        let mut fields = line.split_whitespace();
        let Some(first) = fields.next() else {
            continue;
        };
        if !first.ends_with(':') {
            path = fields.skip(4).collect::<Vec<_>>().join(" ");
            continue;
        }
        if first == "Pss:" && gpu_mapping(&path) {
            total += fields
                .next()
                .and_then(|kib| kib.parse::<f64>().ok())
                .unwrap_or(0.0)
                * 1024.0;
        }
    }
    total
}

/// Every process of the tree, measured now; `split` adds the GPU share (it reads every
/// mapping, so it is slower).
fn measure(split: bool) -> Vec<Measured> {
    tree()
        .into_iter()
        .filter_map(|(pid, name)| {
            let rollup = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
            Some(Measured {
                pid,
                name,
                rss: field(&rollup, "Rss"),
                pss: field(&rollup, "Pss"),
                pss_anon: field(&rollup, "Pss_Anon"),
                pss_file: field(&rollup, "Pss_File"),
                pss_shmem: field(&rollup, "Pss_Shmem"),
                gpu: if split { gpu_share(pid) } else { 0.0 },
            })
        })
        .collect()
}

fn total(processes: &[Measured], value: impl Fn(&Measured) -> f64) -> f64 {
    processes.iter().map(value).sum()
}

fn kind(name: &str, pid: u32) -> &'static str {
    if pid == std::process::id() {
        "app"
    } else if name.starts_with("WebKitWebProces") {
        "web"
    } else if name.starts_with("WebKitNetworkPr") {
        "network"
    } else {
        "other"
    }
}

fn report(label: &str, processes: &[Measured]) {
    println!(
        "MEMORY {label}: tree Pss {:.1} MiB ({:.1} MB), Rss {:.1} MiB, {} processes",
        total(processes, |p| p.pss) / MIB,
        total(processes, |p| p.pss) / 1e6,
        total(processes, |p| p.rss) / MIB,
        processes.len()
    );
    for process in processes {
        println!(
            "MEMORY {label}:   {:<7} {:<16} pid {:>7}: Pss {:>6.1} (anon {:.1}, file {:.1}, \
             shmem {:.1}; GPU stack {:.1}) Rss {:>6.1} MiB",
            kind(&process.name, process.pid),
            process.name,
            process.pid,
            process.pss / MIB,
            process.pss_anon / MIB,
            process.pss_file / MIB,
            process.pss_shmem / MIB,
            process.gpu / MIB,
            process.rss / MIB
        );
    }
}

fn of_kind(processes: &[Measured], wanted: &str) -> f64 {
    processes
        .iter()
        .filter(|process| kind(&process.name, process.pid) == wanted)
        .map(|process| process.pss)
        .sum()
}

struct Filled {
    conversation: MessageId,
    singles: Vec<MessageId>,
}

fn fill(scratch: &support::Scratch) -> Result<Filled, String> {
    let store = &scratch.store;
    let account = store
        .add_account(NewAccount {
            name: samples::ACCOUNT_NAME.into(),
            address: samples::ACCOUNT_ADDRESS.into(),
        })
        .recv_blocking()
        .map_err(|error| error.to_string())?
        .value;
    let folder = |name: &str, role| {
        store
            .upsert_folder(FolderSpec::local(account, name, role))
            .recv_blocking()
            .map(|folder| folder.value)
            .map_err(|error| error.to_string())
    };
    let inbox = folder("INBOX", FolderRole::Inbox)?;
    let sent = folder("Sent", FolderRole::Sent)?;
    let now = glib::real_time() / 1_000_000;
    let conversation = samples::fill_conversation(
        store,
        &scratch.blobs,
        inbox,
        sent,
        samples::conversation("typical", now),
    )
    .map_err(|error| error.to_string())?;
    let singles = samples::fill_conversation(
        store,
        &scratch.blobs,
        inbox,
        sent,
        samples::singles(WARM_UP + OPENED, now - 400_000),
    )
    .map_err(|error| error.to_string())?;
    Ok(Filled {
        conversation: conversation
            .id("typical-10")
            .ok_or("the conversation wasn't filed")?,
        singles: singles.ids.into_iter().map(|(_, id)| id).collect(),
    })
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
            "reader_memory opens a window: run it through tools/headless.sh, or pass --live for \
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
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let input = if broadway {
        match support::broadway::connect() {
            Ok(input) => Some(input),
            Err(error) => {
                eprintln!("no Broadway daemon: {error}");
                return glib::ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let scratch = support::store(&root);
    let filled = match fill(&scratch) {
        Ok(filled) => filled,
        Err(error) => {
            eprintln!("could not fill the scratch store: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ReaderMemory")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let failures: Rc<RefCell<Option<Vec<String>>>> = Rc::default();
    let scratch = Rc::new(scratch);
    let filled = Rc::new(filled);
    let judge = args.judge;
    app.connect_activate(glib::clone!(
        #[strong]
        failures,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let reader = support::reader(&scratch, &theme);
            let window = support::window(app, &reader, WINDOW);
            window.present();
            let app = app.clone();
            let failures = failures.clone();
            let filled = filled.clone();
            let _input = input.clone();
            glib::spawn_future_local(async move {
                let _keep = &theme;
                let result = run(&reader, &window, &filled, broadway, judge).await;
                failures.replace(Some(result.unwrap_or_else(|error| vec![error])));
                app.quit();
            });
        }
    ));
    app.run_with_args::<&str>(&[]);
    match failures.take() {
        Some(failures) if failures.is_empty() => glib::ExitCode::SUCCESS,
        Some(failures) => {
            for failure in failures {
                println!("CHECK failed: {failure}");
            }
            glib::ExitCode::FAILURE
        }
        None => {
            println!("CHECK failed: the run never finished");
            glib::ExitCode::FAILURE
        }
    }
}

fn column(reader: &Reader) -> Result<gtk::ScrolledWindow, String> {
    reader
        .widget()
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
        .ok_or_else(|| "the reader isn't in its column".to_owned())
}

/// HTML bodies on screen that show nothing yet.
fn blank(reader: &Reader, height: f64) -> usize {
    reader
        .bodies()
        .iter()
        .filter(|body| body.bottom > 0.0 && body.top < height)
        .filter(|body| !(body.live && body.loaded && body.painted))
        .count()
}

/// Waits two frames, then until the bodies on screen are drawn (5 s at most).
async fn settle(reader: &Reader, window: &sealwax_app::window::Window) -> Result<(), String> {
    support::frames(window.window(), 2).await?;
    let height = f64::from(column(reader)?.height());
    let drawn = support::wait_until(Duration::from_secs(5), || blank(reader, height) == 0).await;
    support::frames(window.window(), 1).await?;
    if drawn {
        Ok(())
    } else {
        Err(format!(
            "{} bodies stayed blank on screen",
            blank(reader, height)
        ))
    }
}

async fn run(
    reader: &Reader,
    window: &sealwax_app::window::Window,
    filled: &Filled,
    broadway: bool,
    judge: bool,
) -> Result<Vec<String>, String> {
    let mut failures = Vec::new();
    let reports = support::shown_reports(reader);
    support::resize(window.window(), WINDOW.0, WINDOW.1).await?;
    support::sleep(1500).await;
    println!(
        "MEMORY ENV build={} backend={} window={}x{} cap={MAX_LIVE_VIEWS}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        if broadway { "broadway" } else { "live" },
        WINDOW.0,
        WINDOW.1
    );
    report("start", &measure(true));

    let shown = support::show(reader, &reports, filled.conversation).await?;
    settle(reader, window).await?;
    let state = reader.state();
    println!(
        "MEMORY conversation: {} messages, {} expanded ({} HTML), shown in {:.1} ms, {} live views",
        state.messages,
        state.expanded.len(),
        state.html_bodies,
        shown.ready.as_secs_f64() * 1000.0,
        state.live_views
    );
    if (state.messages, state.expanded.len(), state.html_bodies) != (10, 4, 3) {
        failures.push(format!(
            "the conversation shows {} messages, {} expanded, {} HTML; wanted 10, 4, 3",
            state.messages,
            state.expanded.len(),
            state.html_bodies
        ));
    }
    let open = measure(true);
    report("open", &open);

    let adjustment = column(reader)?.vadjustment();
    adjustment.set_value(0.0);
    settle(reader, window).await?;
    let (mut steps, mut most_views, mut peak) = (0, 0, 0.0f64);
    loop {
        if let Err(error) = settle(reader, window).await {
            failures.push(format!("at {:.0} px: {error}", adjustment.value()));
        }
        let views = reader.state().live_views;
        most_views = most_views.max(views);
        if views > MAX_LIVE_VIEWS {
            failures.push(format!(
                "{views} live views at {:.0} px",
                adjustment.value()
            ));
        }
        let now = total(&measure(false), |p| p.pss);
        peak = peak.max(now);
        println!(
            "MEMORY scroll: {:>6.0} px, {views} live views, tree Pss {:.1} MiB",
            adjustment.value(),
            now / MIB
        );
        steps += 1;
        let end = (adjustment.upper() - adjustment.page_size()).max(0.0);
        if adjustment.value() >= end - 0.5 {
            break;
        }
        adjustment.set_value((adjustment.value() + adjustment.page_size() / 3.0).min(end));
    }
    let scrolled = measure(true);
    report("scrolled", &scrolled);

    for id in filled.singles.iter().take(WARM_UP) {
        support::show(reader, &reports, *id).await?;
    }
    support::sleep(1000).await;
    let baseline = measure(true);
    report("baseline (after the warm-up)", &baseline);
    let started = Instant::now();
    for id in filled.singles.iter().skip(WARM_UP).take(OPENED) {
        support::show(reader, &reports, *id).await?;
    }
    let opening = started.elapsed();
    support::sleep(1000).await;
    let after = measure(true);
    report("after 200 opened", &after);
    if reader.state().live_views > MAX_LIVE_VIEWS {
        failures.push(format!(
            "{} live views after the opens",
            reader.state().live_views
        ));
    }

    for (label, processes) in [("open", &open), ("scrolled", &scrolled), ("after", &after)] {
        let kinds: Vec<&str> = processes
            .iter()
            .map(|process| kind(&process.name, process.pid))
            .collect();
        if !kinds.contains(&"web") || !kinds.contains(&"network") {
            failures.push(format!(
                "{label}: the web and network processes weren't both measured ({kinds:?})"
            ));
        }
    }
    let tree_open = total(&open, |p| p.pss);
    let tree_scrolled = total(&scrolled, |p| p.pss);
    let growth = total(&after, |p| p.pss) - total(&baseline, |p| p.pss);
    let growth_app = of_kind(&after, "app") - of_kind(&baseline, "app");
    let criticals = support::criticals();
    if criticals > 0 {
        failures.push(format!("{criticals} GLib criticals"));
    }
    if judge {
        for (label, value) in [
            ("open", tree_open),
            ("scrolled", tree_scrolled),
            ("peak", peak),
        ] {
            if value > TREE_BAR {
                failures.push(format!(
                    "tree Pss {label} {:.1} MB over 300 MB",
                    value / 1e6
                ));
            }
        }
        if growth >= GROWTH_BAR {
            failures.push(format!("growth {:.1} MB, not under 20 MB", growth / 1e6));
        }
    }
    println!(
        "READER_MEMORY build={} backend={} pss_tree_open_mib={:.1} pss_tree_scrolled_mib={:.1} \
         pss_tree_peak_mib={:.1} pss_app_mib={:.1} gpu_app_mib={:.1} pss_web_mib={:.1} \
         pss_network_mib={:.1} rss_tree_mib={:.1} live_views_max={most_views} scroll_steps={steps} \
         growth_tree_mib={:.1} growth_app_mib={:.1} opened={OPENED} opening_s={:.1} \
         criticals={criticals} judged={judge} checks={}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        if broadway { "broadway" } else { "live" },
        tree_open / MIB,
        tree_scrolled / MIB,
        peak / MIB,
        of_kind(&scrolled, "app") / MIB,
        scrolled
            .iter()
            .filter(|process| kind(&process.name, process.pid) == "app")
            .map(|process| process.gpu)
            .sum::<f64>()
            / MIB,
        of_kind(&scrolled, "web") / MIB,
        of_kind(&scrolled, "network") / MIB,
        total(&scrolled, |p| p.rss) / MIB,
        growth / MIB,
        growth_app / MIB,
        opening.as_secs_f64(),
        if failures.is_empty() { "pass" } else { "fail" }
    );
    Ok(failures)
}
