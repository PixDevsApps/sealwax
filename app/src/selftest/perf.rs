//! The message list's measurements (`perf-*`, tests/selftest/perf-list.sealwax-test), ported
//! from S3's `s3-list` probe: every painted frame's work on the GTK thread and its GdkFrameTimings,
//! scripted scrolls and scrollbar sweeps, jumps, folder opens and new mail, and the report.

use super::mail::{
    ADDED, Fiction, find_folder, folder_key_for, list, raw_message, remove_ids, settled,
};
use super::{Harness, StepResult, blocking, number};
use crate::list::MessageList;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::model::MessageId;
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::ViewScope;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("perf-fullscreen", 0, 0),
    ("perf-phase", 1, 1),
    ("perf-scroll", 2, 2),
    ("perf-sweep", 3, 3),
    ("perf-jump", 1, 2),
    ("perf-open", 1, 2),
    ("perf-insert", 1, 1),
    ("perf-memory", 1, 1),
    ("perf-report", 0, 0),
];

thread_local! {
    static PROBE: RefCell<Option<Rc<Probe>>> = const { RefCell::new(None) };
}

/// Runs `command` when it is one of this module's; `None` otherwise.
pub(super) async fn run(harness: &Harness, command: &str, args: &[String]) -> Option<StepResult> {
    let arg = |index: usize| args[index].as_str();
    let result = match command {
        "perf-fullscreen" => match harness.window() {
            Ok(window) => fullscreen(harness, window.window()).await,
            Err(error) => Err(error),
        },
        "perf-phase" => probe(harness).map(|probe| probe.set_phase(arg(0))),
        "perf-scroll" => match (arg(0).parse::<f64>(), number(arg(1))) {
            (Ok(speed), Ok(ms)) => {
                let phase = format!("scroll {speed:.0} px/s");
                scroll(harness, &phase, Motion::Speed(speed), ms).await
            }
            _ => Err("perf-scroll takes px/s and milliseconds".to_owned()),
        },
        "perf-sweep" => match (arg(0).parse::<f64>(), arg(1).parse::<f64>(), number(arg(2))) {
            (Ok(from), Ok(to), Ok(ms)) => {
                let phase = format!("sweep {from}–{to} in {ms} ms");
                scroll(harness, &phase, Motion::Sweep { from, to, ms }, ms).await
            }
            _ => Err("perf-sweep takes two fractions and milliseconds".to_owned()),
        },
        "perf-jump" => jump(harness, arg(0), args.get(1).map(String::as_str)).await,
        "perf-open" => open_measured(harness, arg(0), args.get(1).map(String::as_str)).await,
        "perf-insert" => match number(arg(0)) {
            Ok(count) => insert_measured(harness, count).await,
            Err(error) => Err(error),
        },
        "perf-memory" => probe(harness).map(|probe| {
            probe
                .memory
                .borrow_mut()
                .push((arg(0).to_owned(), memory(), Mappings::read()));
        }),
        "perf-report" => probe(harness).map(|probe| probe.report()),
        _ => return None,
    };
    Some(result)
}

#[derive(Clone, Debug)]
struct Frame {
    phase: String,
    counter: i64,
    work_ms: f64,
    layout_ms: f64,
    bound: u32,
    placeholders: u32,
    binds: u64,
    frame_time: Option<i64>,
    presentation_time: Option<i64>,
    refresh: Option<i64>,
}

type Condition = Box<dyn Fn(&MessageList) -> bool>;

struct Waiter {
    condition: Condition,
    done: async_channel::Sender<Instant>,
}

struct Probe {
    list: MessageList,
    phase: RefCell<String>,
    frames: RefCell<Vec<Frame>>,
    pending: RefCell<VecDeque<usize>>,
    frame_start: Cell<Option<Instant>>,
    layout_end: Cell<Option<Instant>>,
    binds: Cell<u64>,
    waiters: RefCell<Vec<Waiter>>,
    /// Inserted message ids as their change events arrived, with when.
    inserted: RefCell<Vec<(MessageId, Instant)>>,
    results: RefCell<Vec<(String, f64)>>,
    memory: RefCell<Vec<(String, [f64; 7], Mappings)>>,
    window: (i32, i32),
}

fn probe(harness: &Harness) -> Result<Rc<Probe>, String> {
    if let Some(probe) = PROBE.with(|probe| probe.borrow().clone()) {
        return Ok(probe);
    }
    let list = list(harness)?;
    let window = harness.window()?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let probe = Rc::new(Probe {
        binds: Cell::new(list.binds()),
        list,
        phase: RefCell::new("idle".to_owned()),
        frames: RefCell::new(Vec::new()),
        pending: RefCell::new(VecDeque::new()),
        frame_start: Cell::new(None),
        layout_end: Cell::new(None),
        waiters: RefCell::new(Vec::new()),
        inserted: RefCell::new(Vec::new()),
        results: RefCell::new(Vec::new()),
        memory: RefCell::new(Vec::new()),
        window: (window.window().width(), window.window().height()),
    });
    let weak = Rc::downgrade(&probe);
    clock.connect_before_paint(move |_| {
        if let Some(probe) = weak.upgrade() {
            probe.frame_start.set(Some(Instant::now()));
        }
    });
    let weak = Rc::downgrade(&probe);
    clock.connect_layout(move |_| {
        if let Some(probe) = weak.upgrade() {
            probe.layout_end.set(Some(Instant::now()));
        }
    });
    let weak = Rc::downgrade(&probe);
    clock.connect_after_paint(move |clock| {
        if let Some(probe) = weak.upgrade() {
            probe.after_paint(clock);
        }
    });
    let weak = Rc::downgrade(&probe);
    harness.state.mailbox().connect_event(move |event| {
        if let Some(probe) = weak.upgrade() {
            let now = Instant::now();
            probe
                .inserted
                .borrow_mut()
                .extend(event.inserted.iter().map(|id| (*id, now)));
        }
    });
    PROBE.with(|slot| slot.replace(Some(probe.clone())));
    Ok(probe)
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

impl Probe {
    fn set_phase(&self, phase: &str) {
        self.phase.replace(phase.to_owned());
    }

    fn after_paint(&self, clock: &gdk::FrameClock) {
        let end = Instant::now();
        let start = self.frame_start.take();
        let layout = self.layout_end.take();
        let work_ms = start.map_or(0.0, |start| ms(end - start));
        let layout_ms = match (start, layout) {
            (Some(start), Some(layout)) if layout >= start => ms(layout - start),
            _ => 0.0,
        };
        let rows = self.list.bound_rows();
        let binds = self.list.binds();
        let new_binds = binds - self.binds.replace(binds);
        {
            let mut frames = self.frames.borrow_mut();
            frames.push(Frame {
                phase: self.phase.borrow().clone(),
                counter: clock.frame_counter(),
                work_ms,
                layout_ms,
                bound: rows.len() as u32,
                placeholders: rows.iter().filter(|row| row.placeholder).count() as u32,
                binds: new_binds,
                frame_time: None,
                presentation_time: None,
                refresh: None,
            });
            let index = frames.len() - 1;
            let mut pending = self.pending.borrow_mut();
            pending.push_back(index);
            let history = clock.history_start();
            pending.retain(|index| {
                let frame = &mut frames[*index];
                if frame.counter < history {
                    return false;
                }
                match clock.timings(frame.counter) {
                    Some(timings) if timings.is_complete() => {
                        frame.frame_time = Some(timings.frame_time());
                        let presented = timings.presentation_time();
                        frame.presentation_time = (presented != 0).then_some(presented);
                        let refresh = timings.refresh_interval();
                        frame.refresh = (refresh != 0).then_some(refresh);
                        false
                    }
                    _ => true,
                }
            });
        }
        let ready: Vec<Waiter> = {
            let mut waiters = self.waiters.borrow_mut();
            let (ready, waiting) = waiters
                .drain(..)
                .partition(|waiter| (waiter.condition)(&self.list));
            *waiters = waiting;
            ready
        };
        for waiter in ready {
            let _ = waiter.done.try_send(end);
        }
    }

    /// Resolves at the end of the first painted frame in which `condition` holds.
    fn until<F: Fn(&MessageList) -> bool + 'static>(
        &self,
        timeout: Duration,
        condition: F,
    ) -> impl std::future::Future<Output = Option<Instant>> + use<F> {
        let (done, wait) = async_channel::bounded(1);
        self.waiters.borrow_mut().push(Waiter {
            condition: Box::new(condition),
            done,
        });
        async move {
            crate::mailbox::with_timeout(wait.recv(), timeout)
                .await
                .and_then(Result::ok)
        }
    }

    fn record(&self, name: &str, value: Option<f64>) {
        let value = value.unwrap_or(f64::NAN);
        if value.is_nan() {
            println!("  # {name}: timed out");
        } else {
            println!("  # {name}: {value:.1} ms");
        }
        self.results.borrow_mut().push((name.to_owned(), value));
    }

    fn report(&self) {
        let frames = self.frames.borrow();
        println!(
            "  # GTK {}.{}.{}, libadwaita {}.{}.{}; backend {}; renderer {}; {} build",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
            libadwaita::major_version(),
            libadwaita::minor_version(),
            libadwaita::micro_version(),
            gdk::Display::default().map_or("none".to_owned(), |display| display
                .type_()
                .name()
                .to_owned()),
            self.list
                .list_view()
                .native()
                .and_then(|native| native.renderer())
                .map_or("unknown".to_owned(), |renderer| renderer
                    .type_()
                    .name()
                    .to_owned()),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        let view = self.list.list_view();
        println!(
            "  # window {}×{} at the first probe; list view {}×{}; {} rows bound; {} rows in the list",
            self.window.0,
            self.window.1,
            view.width(),
            view.height(),
            self.list.bound_rows().len(),
            self.list.model().total()
        );
        println!(
            "  # interval = difference of consecutive presentation times (frame times where none \
             arrived); work = before-paint → after-paint on the GTK thread"
        );
        println!(
            "  # | phase | frames | refresh ms | interval p50 | p95 | p99 | max | > 16.7 ms | work p50 | p95 | p99 | max | layout p95 | frames with placeholders | binds/frame | rows bound (max) |"
        );
        println!(
            "  # | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
        );
        let mut phases: Vec<&str> = Vec::new();
        for frame in frames.iter() {
            if !phases.contains(&frame.phase.as_str()) {
                phases.push(&frame.phase);
            }
        }
        for phase in phases {
            let selected: Vec<&Frame> = frames.iter().filter(|f| f.phase == phase).collect();
            let pairs = |pick: fn(&Frame) -> Option<i64>| -> Vec<f64> {
                selected
                    .windows(2)
                    .filter(|pair| pair[1].counter == pair[0].counter + 1)
                    .filter_map(|pair| Some((pick(pair[1])? - pick(pair[0])?) as f64 / 1000.0))
                    .collect()
            };
            let presented = pairs(|frame| frame.presentation_time);
            let intervals = if presented.is_empty() {
                pairs(|frame| frame.frame_time)
            } else {
                presented
            };
            let refresh = {
                let mut values: Vec<f64> = selected
                    .iter()
                    .filter_map(|frame| frame.refresh)
                    .map(|value| value as f64 / 1000.0)
                    .collect();
                values.sort_by(f64::total_cmp);
                values.get(values.len() / 2).copied().unwrap_or(0.0)
            };
            let interval = Summary::of(&intervals);
            let work = Summary::of(&selected.iter().map(|f| f.work_ms).collect::<Vec<_>>());
            let layout = Summary::of(&selected.iter().map(|f| f.layout_ms).collect::<Vec<_>>());
            println!(
                "  # | {phase} | {} | {refresh:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {:.1} | {} |",
                selected.len(),
                interval.p50,
                interval.p95,
                interval.p99,
                interval.max,
                intervals
                    .iter()
                    .filter(|value| **value > 1000.0 / 60.0)
                    .count(),
                work.p50,
                work.p95,
                work.p99,
                work.max,
                layout.p95,
                selected.iter().filter(|f| f.placeholders > 0).count(),
                selected.iter().map(|f| f.binds as f64).sum::<f64>() / selected.len() as f64,
                selected.iter().map(|f| f.bound).max().unwrap_or(0)
            );
        }
        // S3's row cost: frames that bound at least 50 rows, per bound row.
        let heavy: Vec<&Frame> = frames.iter().filter(|frame| frame.binds >= 50).collect();
        if !heavy.is_empty() {
            let binds = Summary::of(&heavy.iter().map(|f| f.binds as f64).collect::<Vec<_>>());
            let work = Summary::of(&heavy.iter().map(|f| f.work_ms).collect::<Vec<_>>());
            let layout = Summary::of(&heavy.iter().map(|f| f.layout_ms).collect::<Vec<_>>());
            println!(
                "  # frames that bound ≥ 50 rows: n={}, binds p50 {:.0}, work p50 {:.2} ms, \
                 layout p50 {:.2} ms, layout per bound row {:.3} ms",
                heavy.len(),
                binds.p50,
                work.p50,
                layout.p50,
                layout.p50 / binds.p50.max(1.0)
            );
        }
        let results = self.results.borrow();
        let mut names: Vec<&str> = Vec::new();
        for (name, _) in results.iter() {
            if !names.contains(&name.as_str()) {
                names.push(name);
            }
        }
        println!("  # | measurement | n | timeouts | p50 ms | p95 ms | max ms |");
        println!("  # | --- | --- | --- | --- | --- | --- |");
        for name in names {
            let values: Vec<f64> = results
                .iter()
                .filter(|(candidate, _)| candidate == name)
                .map(|(_, value)| *value)
                .collect();
            let timeouts = values.iter().filter(|value| value.is_nan()).count();
            let done: Vec<f64> = values.into_iter().filter(|value| !value.is_nan()).collect();
            let summary = Summary::of(&done);
            println!(
                "  # | {name} | {} | {timeouts} | {:.1} | {:.1} | {:.1} |",
                done.len(),
                summary.p50,
                summary.p95,
                summary.max
            );
        }
        let stats = self.list.model().stats();
        println!("  # model: {stats:?}");
        println!(
            "  # | memory (MiB) | VmRSS | VmHWM | RssAnon | RssFile | Pss | Pss_Anon | Pss_File |"
        );
        println!("  # | --- | --- | --- | --- | --- | --- | --- | --- |");
        for (label, values, _) in self.memory.borrow().iter() {
            println!(
                "  # | {label} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                values[0], values[1], values[2], values[3], values[4], values[5], values[6]
            );
        }
        for (label, _, mappings) in self.memory.borrow().iter() {
            println!("  # Pss by mapping at {label} (MiB): {}", mappings.kinds());
            println!("  # largest files at {label} (MiB): {}", mappings.files(8));
        }
    }
}

#[derive(Default)]
struct Summary {
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

impl Summary {
    /// Nearest-rank percentiles.
    fn of(values: &[f64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let rank = |p: f64| {
            let index = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
            sorted[index.clamp(1, sorted.len()) - 1]
        };
        Self {
            p50: rank(50.0),
            p95: rank(95.0),
            p99: rank(99.0),
            max: sorted[sorted.len() - 1],
        }
    }
}

/// MiB from `/proc/self/status` (VmRSS, VmHWM, RssAnon, RssFile) and `/proc/self/smaps_rollup`
/// (Pss, Pss_Anon, Pss_File).
fn memory() -> [f64; 7] {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    let field = |text: &str, key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    [
        field(&status, "VmRSS"),
        field(&status, "VmHWM"),
        field(&status, "RssAnon"),
        field(&status, "RssFile"),
        field(&rollup, "Pss"),
        field(&rollup, "Pss_Anon"),
        field(&rollup, "Pss_File"),
    ]
}

/// Pss from `/proc/self/smaps`, summed by what is mapped, so the GPU driver's share can be told
/// from Sealwax's own heap. Files are named by their base name only.
struct Mappings {
    kinds: Vec<(&'static str, f64)>,
    files: Vec<(String, f64)>,
}

impl Mappings {
    fn read() -> Self {
        let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
        let mut kinds: Vec<(&'static str, f64)> = Vec::new();
        let mut files: Vec<(String, f64)> = Vec::new();
        let mut path = String::new();
        for line in smaps.lines() {
            let mut fields = line.split_whitespace();
            let Some(first) = fields.next() else {
                continue;
            };
            if !first.ends_with(':') {
                path = fields.skip(4).collect::<Vec<_>>().join(" ");
                continue;
            }
            if first != "Pss:" {
                continue;
            }
            let mib = fields
                .next()
                .and_then(|kib| kib.parse::<f64>().ok())
                .unwrap_or(0.0)
                / 1024.0;
            let kind = mapping_kind(&path);
            match kinds.iter_mut().find(|(name, _)| *name == kind) {
                Some((_, total)) => *total += mib,
                None => kinds.push((kind, mib)),
            }
            if path.starts_with('/') {
                let name = path.rsplit('/').next().unwrap_or_default().to_owned();
                match files.iter_mut().find(|(file, _)| *file == name) {
                    Some((_, total)) => *total += mib,
                    None => files.push((name, mib)),
                }
            }
        }
        kinds.sort_by(|a, b| b.1.total_cmp(&a.1));
        files.sort_by(|a, b| b.1.total_cmp(&a.1));
        Self { kinds, files }
    }

    fn kinds(&self) -> String {
        let parts: Vec<String> = self
            .kinds
            .iter()
            .map(|(kind, mib)| format!("{kind} {mib:.1}"))
            .collect();
        parts.join(" · ")
    }

    fn files(&self, count: usize) -> String {
        let parts: Vec<String> = self
            .files
            .iter()
            .take(count)
            .map(|(file, mib)| format!("{file} {mib:.1}"))
            .collect();
        parts.join(" · ")
    }
}

fn mapping_kind(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if path.is_empty() || path.starts_with('[') {
        "heap and anonymous"
    } else if path.starts_with("/memfd:") || has(&["/dev/shm/", "sysv"]) {
        "shared memory"
    } else if path.starts_with("/dev/") {
        "GPU device mappings"
    } else if has(&[
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
    ]) {
        "GPU driver and GL/Vulkan libraries"
    } else if has(&["webkit", "javascriptcore"]) {
        "WebKit libraries"
    } else if has(&[".ttf", ".otf", ".ttc", "fontconfig", "/fonts/"]) {
        "fonts"
    } else if lower
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("sealwax"))
    {
        "the sealwax binary"
    } else if has(&[".so"]) {
        "other libraries"
    } else {
        "other files"
    }
}

#[derive(Clone, Copy)]
enum Motion {
    /// Pixels a second.
    Speed(f64),
    /// From one fraction of the list to another, as a scrollbar drag.
    Sweep { from: f64, to: f64, ms: usize },
}

async fn scroll(harness: &Harness, phase: &str, motion: Motion, ms_total: usize) -> StepResult {
    let probe = probe(harness)?;
    probe.list.model().complete_growth();
    probe.set_phase(phase);
    let adjustment = probe.list.scroller().vadjustment();
    let start: Cell<Option<i64>> = Cell::new(None);
    let last: Cell<Option<i64>> = Cell::new(None);
    let tick = probe.list.list_view().add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        let dt = last
            .replace(Some(now))
            .map_or(0.0, |before| (now - before) as f64 / 1e6);
        let top = (adjustment.upper() - adjustment.page_size()).max(0.0);
        match motion {
            Motion::Speed(speed) => {
                adjustment.set_value((adjustment.value() + speed * dt).min(top))
            }
            Motion::Sweep { from, to, ms } => {
                let began = *start.get().get_or_insert(now);
                start.set(Some(began));
                let progress = ((now - began) as f64 / 1e3 / ms as f64).min(1.0);
                adjustment.set_value((from + (to - from) * progress) * top);
            }
        }
        glib::ControlFlow::Continue
    });
    glib::timeout_future(Duration::from_millis(ms_total as u64)).await;
    tick.remove();
    probe.set_phase("idle");
    settled(harness).await
}

/// `perf-fullscreen`: the window fills its monitor, as S3's runs did (3440×1440 live). Broadway
/// ignores fullscreen requests, so there the window takes the monitor's size instead.
async fn fullscreen(harness: &Harness, window: &libadwaita::ApplicationWindow) -> StepResult {
    window.fullscreen();
    glib::timeout_future(Duration::from_millis(500)).await;
    if !window.is_fullscreen()
        && let Some(monitor) = gdk::Display::default()
            .and_then(|display| display.monitors().item(0))
            .and_downcast::<gdk::Monitor>()
    {
        let geometry = monitor.geometry();
        window.set_default_size(geometry.width(), geometry.height());
        glib::timeout_future(Duration::from_millis(500)).await;
    }
    println!(
        "  # window {}×{}, fullscreen: {}",
        window.width(),
        window.height(),
        window.is_fullscreen()
    );
    harness.wait_frames(2).await
}

/// `perf-jump end|start|FRACTION [warm]`: forgets the loaded rows (unless `warm`, S3's row
/// bench, where both ends stay loaded), jumps, and times `scroll_to` → the first painted frame
/// with the target row bound and no placeholder.
async fn jump(harness: &Harness, to: &str, mode: Option<&str>) -> StepResult {
    let warm = match mode {
        None => false,
        Some("warm") => true,
        Some(other) => return Err(format!("use warm or nothing, not {other}")),
    };
    let probe = probe(harness)?;
    settled(harness).await?;
    let model = probe.list.model();
    model.complete_growth();
    let total = model.total();
    if total == 0 {
        return Err("the list is empty".to_owned());
    }
    let target = match to {
        "end" => total - 1,
        "start" => 0,
        fraction => match fraction.parse::<f64>() {
            Ok(fraction) if (0.0..=1.0).contains(&fraction) => {
                ((total - 1) as f64 * fraction) as u32
            }
            _ => return Err(format!("use end, start or a fraction, not {fraction}")),
        },
    };
    let name = if warm {
        format!("warm jump to {to}")
    } else {
        model.drop_cache();
        format!("jump to {to}")
    };
    probe.set_phase(&name);
    let shown = probe.until(Duration::from_secs(5), move |list| {
        let rows = list.bound_rows();
        rows.iter().any(|row| row.position == Some(target))
            && rows.iter().all(|row| !row.placeholder)
    });
    let started = Instant::now();
    probe
        .list
        .list_view()
        .scroll_to(target, gtk::ListScrollFlags::NONE, None);
    let frame = shown.await;
    probe.set_phase("idle");
    let what = if warm {
        "rows loaded"
    } else {
        "loaded rows forgotten"
    };
    probe.record(
        &format!("{name} ({what}) → frame with the row, no placeholders"),
        frame.map(|at| ms(at - started)),
    );
    settled(harness).await
}

/// `perf-open FOLDER [ACCOUNT]`: picks the folder and times it → the first painted frame with
/// its rows and no placeholder.
async fn open_measured(harness: &Harness, spec: &str, account: Option<&str>) -> StepResult {
    let probe = probe(harness)?;
    settled(harness).await?;
    let spec = match account {
        Some(account) => format!("{account}:{spec}"),
        None => spec.to_owned(),
    };
    let key = folder_key_for(harness, &spec)?;
    probe.set_phase("folder open");
    let shown = probe.until(Duration::from_secs(5), |list| {
        let rows = list.bound_rows();
        !list.model().is_pending() && !rows.is_empty() && rows.iter().all(|row| !row.placeholder)
    });
    let started = Instant::now();
    harness.window()?.select_folder(&key);
    let frame = shown.await;
    probe.set_phase("idle");
    probe.record(
        "folder open → first frame with rows, no placeholders",
        frame.map(|at| ms(at - started)),
    );
    settled(harness).await
}

/// `perf-insert N`: N new messages at the top of the shown folder, 250 ms apart, each timed from
/// the hand-over to the store's writer to its change event on the GTK thread and to the first
/// painted frame showing its row; then they are removed again.
async fn insert_measured(harness: &Harness, count: usize) -> StepResult {
    let probe = probe(harness)?;
    settled(harness).await?;
    let list = probe.list.clone();
    let folder = match list.scope() {
        Some(ViewScope::Folder(folder)) => folder,
        _ => find_folder(harness, "inbox")?.id,
    };
    let mailbox = harness.state.mailbox();
    let blobs = mailbox.blobs().ok_or("the blob store is not open")?;
    let mut ids = Vec::new();
    probe.set_phase("insert");
    for index in 0..count {
        let date = list.now_unix() + 60 + index as i64;
        let serial = ADDED.with(|added| {
            added.set(added.get() + 1);
            added.get()
        });
        let fiction = Fiction {
            subject: format!("Perf insert {index}"),
            from: "Marius Dahl".to_owned(),
            text: "A new message for the insert measurement.".to_owned(),
            attachment: false,
            reply_to: None,
        };
        let raw = RawMessage {
            folder,
            internal_date: Some(date),
            raw: raw_message(&fiction, date, serial),
            ..RawMessage::default()
        };
        let worker_blobs = blobs.clone();
        let prepare_started = Instant::now();
        let prepared = blocking(move || ingest::prepare(&worker_blobs, vec![raw], 1))
            .await?
            .map_err(|error| error.to_string())?;
        let prepare_ms = ms(prepare_started.elapsed());
        probe.inserted.borrow_mut().clear();
        let inserted = Rc::new(Cell::new(None::<MessageId>));
        let watched = inserted.clone();
        let event_probe = probe.clone();
        let shown = probe.until(Duration::from_secs(5), move |list| {
            if watched.get().is_none() {
                watched.set(event_probe.inserted.borrow().first().map(|(id, _)| *id));
            }
            let Some(id) = watched.get() else {
                return false;
            };
            list.bound_rows()
                .iter()
                .any(|row| row.id == Some(id) && !row.placeholder)
        });
        let started = Instant::now();
        let reply = mailbox
            .store()
            .ok_or("the store is not open")?
            .ingest(prepared.messages);
        let frame = shown.await;
        let ingested = reply.recv().await.map_err(|error| error.to_string())?;
        let event = probe
            .inserted
            .borrow()
            .first()
            .map(|(_, at)| ms(*at - started));
        probe.record(
            "insert: blob store + parse (worker thread)",
            Some(prepare_ms),
        );
        probe.record("insert: hand-over → change event on the GTK thread", event);
        probe.record(
            "insert: hand-over → first frame showing the row",
            frame.map(|at| ms(at - started)),
        );
        ids.extend(ingested.ids.into_iter().flatten());
        glib::timeout_future(Duration::from_millis(250)).await;
    }
    probe.set_phase("idle");
    remove_ids(harness, ids).await?;
    settled(harness).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let summary = Summary::of(&[5.0, 1.0, 3.0, 2.0, 4.0]);
        assert_eq!((summary.p50, summary.p95, summary.max), (3.0, 5.0, 5.0));
        assert_eq!(Summary::of(&[]).max, 0.0);
    }
}
