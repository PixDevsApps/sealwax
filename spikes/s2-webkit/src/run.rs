//! The measurement modes. Every mode prints tagged lines on stdout (`ENV`, `ROW`, `SUMMARY`,
//! …) and returns the process exit code: 0 when its pass bars hold, 1 otherwise.

use crate::Args;
use crate::reader::{
    Defences, Engine, Event, MessageView, Navigation, Stamped, content_security_policy,
};
use crate::ui::{self, Pane};
use email_spike_s2::canary::Canary;
use email_spike_s2::corpus::{self, Assets, Kind, Message, Part};
use email_spike_s2::theme::{self, BodyStyle, Palette};
use email_spike_s2::{heuristic, hostile, procmem, stats};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use webkit6::prelude::*;

pub struct Ctx {
    pub args: Args,
    pub pane: Pane,
    pub engine: Rc<Engine>,
    pub canary: Canary,
    pub palette: Palette,
    pub sans: String,
    pub assets: Assets,
}

fn palette() -> Palette {
    let state = std::env::var_os("MAIL_S2_OMARCHY_STATE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state/omarchy"))
        });
    state
        .and_then(|state| Palette::load_omarchy(&state))
        .unwrap_or_else(Palette::builtin_dark)
}

fn cache_dir() -> PathBuf {
    std::env::var_os("MAIL_S2_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME").map(|dir| PathBuf::from(dir).join("email-spike-s2"))
        })
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/email-spike-s2"))
        })
        .unwrap_or_else(|| PathBuf::from("email-spike-s2-cache"))
}

pub async fn main(app: adw::Application, args: Args) -> i32 {
    let palette = palette();
    let sans = ui::fc_match("sans-serif");
    let radius = ui::hyprland_rounding().unwrap_or(0);
    ui::install_css(&palette, radius);
    let pane = ui::pane(&app);
    pane.window.present();
    ui::wait_for_size(&pane.column).await;
    let cache = cache_dir();
    let engine = match Engine::new(&cache, args.share_process, args.accelerated, &sans).await {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!(
                "could not compile the content filter in {}: {error}",
                cache.display()
            );
            return 1;
        }
    };
    let canary = match Canary::start() {
        Ok(canary) => canary,
        Err(error) => {
            eprintln!("could not start the canary: {error}");
            return 1;
        }
    };
    let font = std::fs::read("/usr/share/fonts/liberation/LiberationSerif-Regular.ttf").ok();
    println!(
        "ENV mode={} display={} renderer={} gtk={}.{}.{} webkit={}.{}.{} palette={} dark={} sans={:?} radius={} share_process={} accelerated={} window={}x{} column={} canary_port={} font_face={}",
        args.mode,
        ui::display_name(),
        ui::renderer_name(&pane.window),
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
        webkit6::functions::major_version(),
        webkit6::functions::minor_version(),
        webkit6::functions::micro_version(),
        palette.source,
        palette.dark,
        sans,
        radius,
        args.share_process,
        args.accelerated,
        pane.window.width(),
        pane.window.height(),
        pane.column.width(),
        canary.port(),
        font.is_some(),
    );
    let ctx = Ctx {
        args,
        pane,
        engine,
        canary,
        palette,
        sans,
        assets: Assets::generate(font),
    };
    let code = match ctx.args.mode.as_str() {
        "features" => features(),
        "gpu" => gpu(&ctx).await,
        "hostile" => hostile(&ctx).await,
        "corpus" => corpus(&ctx).await,
        "conversation" => conversation(&ctx).await,
        "live" => live(&ctx).await,
        other => {
            eprintln!("unknown mode {other}");
            2
        }
    };
    let leaks = ctx.canary.snapshot();
    println!(
        "CANARY-TOTAL connections={} requests={} silent={} tls={}",
        leaks.connections,
        leaks.hits.len(),
        leaks.silent_connections,
        leaks.tls_hellos
    );
    code
}

fn ms(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64() * 1000.0
}

async fn sleep(millis: u64) {
    glib::timeout_future(Duration::from_millis(millis)).await;
}

async fn recv_until(view: &MessageView, until: Instant) -> Option<Stamped> {
    let wait = until.saturating_duration_since(Instant::now());
    glib::future_with_timeout(wait, view.events.recv())
        .await
        .ok()
        .and_then(Result::ok)
}

fn style_for(message: &Message) -> BodyStyle {
    if heuristic::analyse(&message.body).own_colours() {
        BodyStyle::Paper
    } else {
        BodyStyle::Themed
    }
}

fn features() -> i32 {
    let settings = webkit6::Settings::new();
    let Some(list) = webkit6::Settings::all_features() else {
        eprintln!("no feature list");
        return 1;
    };
    for index in 0..list.length() {
        let Some(feature) = list.get(index) else {
            continue;
        };
        println!(
            "FEATURE {} enabled={} status={:?} category={}",
            feature.identifier().unwrap_or_default(),
            settings.is_feature_enabled(&feature),
            feature.status(),
            feature.category().unwrap_or_default(),
        );
    }
    0
}

/// WebKit's own report of its rendering path (`webkit://gpu`), from a plain view in the
/// engine's context and ephemeral session.
async fn gpu(ctx: &Ctx) -> i32 {
    let view = ctx.engine.plain_view();
    view.set_size_request(600, 400);
    ctx.pane.column.append(&view);
    let (sender, finished) = async_channel::bounded::<()>(1);
    view.connect_load_changed(move |_, event| {
        if event == webkit6::LoadEvent::Finished {
            let _ = sender.try_send(());
        }
    });
    view.load_uri("webkit://gpu");
    let _ = glib::future_with_timeout(Duration::from_secs(5), finished.recv()).await;
    sleep(300).await;
    let code = match view
        .evaluate_javascript_future("document.body ? document.body.innerText : ''", None, None)
        .await
    {
        Ok(value) => {
            for line in value
                .to_str()
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                println!("GPU {}", line.trim());
            }
            0
        }
        Err(error) => {
            println!("GPU unavailable: {error}");
            1
        }
    };
    ctx.pane.column.remove(&view);
    code
}

const PROBE_JS: &str = r#"
const root = document.documentElement;
const cid = [...document.images].filter(i => (i.getAttribute('src') || '').startsWith('cid:'));
const fonts = [];
if (document.fonts) document.fonts.forEach(f => fonts.push(f.family.replace(/\s+/g, '_') + ':' + f.status));
return {
  scrollHeight: root.scrollHeight,
  clientHeight: root.clientHeight,
  innerHeight: window.innerHeight,
  scrollWidth: root.scrollWidth,
  innerWidth: window.innerWidth,
  box: root.getBoundingClientRect().height,
  pwned: root.getAttribute('data-pwned') || '',
  cidImages: cid.length,
  cidLoaded: cid.filter(i => i.complete && i.naturalWidth > 0).length,
  foreignLoaded: [...document.images].filter(i => i.id === 's2-foreign' && i.complete && i.naturalWidth > 0).length,
  contentBottom: (() => {
    const body = document.body;
    if (!body) return 0;
    let bottom = 0;
    for (const element of body.querySelectorAll('*')) {
      const rect = element.getBoundingClientRect();
      if (rect.height > 0) bottom = Math.max(bottom, rect.bottom);
    }
    const style = getComputedStyle(body);
    const extra = ['paddingBottom', 'borderBottomWidth', 'marginBottom'].map(k => parseFloat(style[k]) || 0);
    return bottom + extra.reduce((a, b) => a + b, 0) + window.scrollY;
  })(),
  okImages: [...document.images].filter(i => i.id === 's2-ok').length,
  okLoaded: [...document.images].filter(i => i.id === 's2-ok' && i.complete && i.naturalWidth > 0).length,
  fonts: fonts.join(' '),
};
"#;

#[derive(Debug, Clone, Default)]
struct Probe {
    scroll_height: f64,
    client_height: f64,
    inner_height: f64,
    scroll_width: f64,
    inner_width: f64,
    root_box: f64,
    pwned: String,
    cid_images: f64,
    cid_loaded: f64,
    foreign_loaded: f64,
    ok_images: f64,
    ok_loaded: f64,
    content_bottom: f64,
    fonts: String,
}

async fn probe(view: &MessageView) -> Result<Probe, String> {
    let value = view
        .eval(PROBE_JS)
        .await
        .map_err(|error| error.to_string())?;
    let number = |key: &str| {
        value
            .object_get_property(key)
            .map_or(f64::NAN, |value| value.to_double())
    };
    let text = |key: &str| {
        value
            .object_get_property(key)
            .map(|value| value.to_str().to_string())
            .unwrap_or_default()
    };
    Ok(Probe {
        scroll_height: number("scrollHeight"),
        client_height: number("clientHeight"),
        inner_height: number("innerHeight"),
        scroll_width: number("scrollWidth"),
        inner_width: number("innerWidth"),
        root_box: number("box"),
        pwned: text("pwned"),
        cid_images: number("cidImages"),
        cid_loaded: number("cidLoaded"),
        foreign_loaded: number("foreignLoaded"),
        ok_images: number("okImages"),
        content_bottom: number("contentBottom"),
        ok_loaded: number("okLoaded"),
        fonts: text("fonts"),
    })
}

/// Paint times of a widget's frame clock while it exists, with the widget's height at each.
struct PaintLog {
    clock: gdk::FrameClock,
    handler: glib::SignalHandlerId,
    times: Rc<RefCell<Vec<(Instant, i32)>>>,
}

impl PaintLog {
    fn start(widget: &impl IsA<gtk::Widget>) -> Option<Self> {
        let widget = widget.as_ref().clone();
        let clock = widget.frame_clock()?;
        let times: Rc<RefCell<Vec<(Instant, i32)>>> = Rc::default();
        let handler = clock.connect_after_paint(glib::clone!(
            #[strong]
            times,
            move |_| times.borrow_mut().push((Instant::now(), widget.height()))
        ));
        Some(Self {
            clock,
            handler,
            times,
        })
    }

    fn stop(self) -> Vec<(Instant, i32)> {
        self.clock.disconnect(self.handler);
        self.times.take()
    }
}

#[derive(Debug, Default)]
struct Settled {
    committed: Option<Instant>,
    finished: Option<Instant>,
    frame: Option<Instant>,
    height: Option<(i32, Instant)>,
    reports: usize,
    other: Vec<Stamped>,
    timed_out: bool,
}

impl Settled {
    fn record(&mut self, stamped: Stamped) {
        let at = stamped.at;
        match stamped.event {
            Event::Committed => self.committed = Some(at),
            Event::Height(height) if self.committed.is_some() => {
                self.height = Some((height, at));
                self.reports += 1;
            }
            Event::Frame if self.committed.is_some() && self.frame.is_none() => {
                self.frame = Some(at)
            }
            Event::Finished if self.committed.is_some() => self.finished = Some(at),
            Event::Height(_) | Event::Frame | Event::Finished => {}
            _ => self.other.push(stamped),
        }
    }
}

/// Waits until the load finished, the first frame rendered and the height stopped changing
/// for `quiet`.
async fn settle(view: &MessageView, start: Instant, limit: Duration, quiet: Duration) -> Settled {
    let deadline = start + limit;
    let mut settled = Settled::default();
    loop {
        let ready = match (settled.finished, settled.frame, settled.height) {
            (Some(finished), Some(frame), Some((_, height))) => {
                Some(finished.max(frame).max(height) + quiet)
            }
            _ => None,
        };
        let wake = ready.unwrap_or(deadline).min(deadline);
        if Instant::now() >= wake {
            settled.timed_out = ready.is_none();
            break;
        }
        if let Some(stamped) = recv_until(view, wake).await {
            settled.record(stamped);
        }
    }
    settled
}

struct Opened {
    style: BodyStyle,
    settled: Settled,
    finished_ms: Option<f64>,
    frame_ms: Option<f64>,
    /// First GTK paint after the web process rendered its first frame.
    paint_ms: Option<f64>,
    /// First GTK paint with the view at its final height (after the first frame).
    sized_ms: Option<f64>,
}

async fn open(ctx: &Ctx, view: &MessageView, frame: &gtk::Box, message: &Message) -> Opened {
    let style = style_for(message);
    ui::set_style(frame, style);
    ctx.engine.set_parts(&view.view, &message.parts);
    view.reset_height();
    view.drain();
    let paints = PaintLog::start(&view.view);
    let start = Instant::now();
    view.load(
        &message.body,
        &theme::reader_css(&ctx.palette, style, &ctx.sans),
        &ui::background(&ctx.palette, style),
    );
    let mut settled = settle(
        view,
        start,
        Duration::from_secs(5),
        Duration::from_millis(120),
    )
    .await;
    follow_heights(view, &mut settled).await;
    let paints = paints.map(PaintLog::stop).unwrap_or_default();
    let paint_ms = settled.frame.and_then(|frame| {
        paints
            .iter()
            .find(|&&(paint, _)| paint >= frame && paint <= frame + Duration::from_millis(100))
            .map(|&(paint, _)| ms(start, paint))
    });
    let sized_ms = settled
        .frame
        .zip(settled.height)
        .and_then(|(frame, (height, _))| {
            paints
                .iter()
                .find(|&&(paint, painted)| paint >= frame && painted == height)
                .map(|&(paint, _)| ms(start, paint))
        });
    Opened {
        style,
        finished_ms: settled.finished.map(|at| ms(start, at)),
        frame_ms: settled.frame.map(|at| ms(start, at)),
        paint_ms,
        sized_ms,
        settled,
    }
}

/// Follows a body's height reports until one has been allocated and no newer report arrived
/// within a quiet window (each resize can change the layout and bring another report).
async fn follow_heights(view: &MessageView, settled: &mut Settled) {
    for _ in 0..10 {
        let Some((height, _)) = settled.height else {
            break;
        };
        wait_for_allocation(view, height, Duration::from_secs(4)).await;
        let reports = settled.reports;
        let quiet = Instant::now() + Duration::from_millis(120);
        while let Some(stamped) = recv_until(view, quiet).await {
            settled.record(stamped);
        }
        if settled.reports == reports {
            break;
        }
    }
}

/// Waits until GTK has allocated `height` to the view; returns when that was first seen.
async fn wait_for_allocation(view: &MessageView, height: i32, limit: Duration) -> Option<Instant> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if view.view.height() == height {
            return Some(Instant::now());
        }
        sleep(2).await;
    }
    None
}

/// Probes once WebKit's viewport matches the height GTK allocated (the resize reaches the
/// web process asynchronously).
async fn settled_probe(view: &MessageView, limit: Duration) -> Probe {
    let deadline = Instant::now() + limit;
    loop {
        let probe = probe(view).await.unwrap_or_default();
        if f64::from(view.view.height()) == probe.inner_height || Instant::now() >= deadline {
            return probe;
        }
        sleep(10).await;
    }
}

/// GTK's render of the widget, retried until GTK has painted it once (under Broadway without
/// a browser attached, GTK paints about once a second).
async fn render_when_painted(
    widget: &impl IsA<gtk::Widget>,
    limit: Duration,
) -> Result<gdk::Texture, String> {
    let deadline = Instant::now() + limit;
    loop {
        match ui::render_widget(widget) {
            Ok(texture) => return Ok(texture),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => sleep(20).await,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HeightCheck {
    applied: i32,
    allocated: i32,
    ok: bool,
    inner_vscroll: bool,
    hscroll: bool,
    /// Applied height minus the bottom of the lowest element (blank space at the end).
    slack: f64,
}

/// Blank space at the end of a body beyond this counts as a wrong height. Elements' boxes
/// exclude trailing margins, so a few px of slack are normal.
const SLACK_LIMIT: f64 = 40.0;

fn check_height(view: &MessageView, applied: Option<i32>, probe: &Probe) -> HeightCheck {
    let applied = applied.unwrap_or(-1);
    let allocated = view.view.height();
    let inner_vscroll = probe.scroll_height > probe.inner_height;
    let slack = f64::from(applied) - probe.content_bottom;
    HeightCheck {
        applied,
        allocated,
        ok: (f64::from(applied) - probe.scroll_height).abs() <= 1.0
            && allocated == applied
            && !inner_vscroll
            && slack <= SLACK_LIMIT,
        inner_vscroll,
        hscroll: probe.scroll_width > probe.inner_width,
        slack,
    }
}

fn opt(value: Option<f64>) -> String {
    value.map_or_else(|| "-".into(), |value| format!("{value:.1}"))
}

fn summary_line(name: &str, values: &[f64]) {
    match stats::summarize(values) {
        Some(summary) => println!("SUMMARY {name} {summary}"),
        None => println!("SUMMARY {name} n=0"),
    }
}

/// The corpus in one pre-warmed view: open latency, height accuracy, `cid:` images and,
/// with `--snapshots`, the blank-frame check through both WebKit's snapshot and GTK's
/// render of the widget.
async fn corpus(ctx: &Ctx) -> i32 {
    let mut messages = corpus::newsletters(&ctx.canary.origin(), &ctx.assets);
    messages.extend(corpus::simple_html());
    if let Some(ids) = &ctx.args.cases {
        let documents = hostile::cases(&ctx.canary.origin())
            .into_iter()
            .map(|case| Message {
                id: case.id.into(),
                kind: Kind::SimpleHtml,
                family: "hostile",
                sender: "Hostile Suite".into(),
                subject: case.vector.into(),
                body: case.html,
                parts: Vec::new(),
            });
        messages.extend(documents);
        messages.retain(|message| ids.contains(&message.id));
    }
    if let Some(limit) = ctx.args.limit {
        messages.truncate(limit);
    }
    let view = ctx.engine.new_view(&Defences::production());
    let warm = [corpus::simple_html().remove(0), messages[0].clone()];
    let (root, frame) = ui::message(&warm[0], 0);
    frame.append(&ui::BodyBox::new(&view.view));
    ctx.pane.column.append(&root);
    for message in &warm {
        open(ctx, &view, &frame, message).await;
    }
    let canary_before = ctx.canary.snapshot();
    let cid_before = ctx.engine.cid_requests.borrow().len();
    let mut finished = Vec::new();
    let mut first_frame = Vec::new();
    let mut painted = Vec::new();
    let mut sized = Vec::new();
    let mut opened = Vec::new();
    let mut height_ok = 0;
    let mut height_total = 0;
    let mut height_failures = Vec::new();
    let mut blank = Vec::new();
    let mut snapshots = 0;
    let mut cid = (0.0, 0.0);
    let mut hscroll = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let result = open(ctx, &view, &frame, message).await;
        let probe = settled_probe(&view, Duration::from_secs(2)).await;
        let check = check_height(&view, result.settled.height.map(|(h, _)| h), &probe);
        if let Some(value) = result.finished_ms {
            finished.push(value);
        }
        if let Some(value) = result.frame_ms {
            first_frame.push(value);
        }
        if let Some(value) = result.paint_ms {
            painted.push(value);
        }
        if let Some(value) = result.sized_ms {
            sized.push(value);
        }
        if let (Some(a), Some(b)) = (result.finished_ms, result.frame_ms) {
            opened.push(a.max(b));
        }
        height_total += 1;
        if check.ok {
            height_ok += 1;
        } else {
            height_failures.push(message.id.clone());
        }
        if check.hscroll {
            hscroll.push(message.id.clone());
        }
        cid.0 += probe.cid_loaded;
        cid.1 += probe.cid_images;
        let mut shots = String::new();
        if ctx.args.snapshots {
            snapshots += 1;
            let webkit = view
                .view
                .snapshot_future(
                    webkit6::SnapshotRegion::Visible,
                    webkit6::SnapshotOptions::NONE,
                )
                .await
                .map(|texture| ui::uniformity(&texture));
            let gtk_texture = render_when_painted(&view.view, Duration::from_secs(3)).await;
            let gtk = gtk_texture.as_ref().map(ui::uniformity);
            let blank_webkit = webkit.as_ref().map_or(true, ui::Uniformity::blank);
            let blank_gtk = gtk.as_ref().map_or(true, ui::Uniformity::blank);
            if blank_webkit || blank_gtk {
                blank.push(format!(
                    "{}(webkit={blank_webkit},gtk={blank_gtk})",
                    message.id
                ));
            }
            shots = format!(
                " webkit=[{}] gtk=[{}]",
                webkit.map_or_else(|error| error.to_string(), |u| u.to_string()),
                gtk.as_ref()
                    .map_or_else(|error| error.to_string(), |u| u.to_string())
            );
            if let (Some(dir), Ok(texture)) = (&ctx.args.save_dir, &gtk_texture)
                && (index % 11 == 0 || message.kind == Kind::SimpleHtml && index % 4 == 0)
            {
                let _ = texture.save_to_png(dir.join(format!("{}-gtk.png", message.id)));
                if let Ok(window) = ui::render_widget(&ctx.pane.window) {
                    let _ = window.save_to_png(dir.join(format!("{}-window.png", message.id)));
                }
            }
        }
        println!(
            "ROW corpus id={} family={} style={:?} finished_ms={} frame_ms={} paint_ms={} sized_ms={} applied={} allocated={} scrollHeight={} clientHeight={} innerHeight={} box={:.1} slack={:.1} reports={} height_ok={} inner_vscroll={} hscroll={} scrollWidth={} innerWidth={} cid={}/{} fonts=[{}] timeout={}{shots}",
            message.id,
            message.family,
            result.style,
            opt(result.finished_ms),
            opt(result.frame_ms),
            opt(result.paint_ms),
            opt(result.sized_ms),
            check.applied,
            check.allocated,
            probe.scroll_height,
            probe.client_height,
            probe.inner_height,
            probe.root_box,
            check.slack,
            result.settled.reports,
            check.ok,
            check.inner_vscroll,
            check.hscroll,
            probe.scroll_width,
            probe.inner_width,
            probe.cid_loaded,
            probe.cid_images,
            probe.fonts,
            result.settled.timed_out,
        );
    }
    let plain = plain_text_latency(ctx).await;
    let leaks = ctx.canary.snapshot().since(&canary_before);
    let cid_misses: Vec<String> = ctx.engine.cid_requests.borrow()[cid_before..]
        .iter()
        .filter(|request| !request.found)
        .map(|request| format!("page{}:{}", request.page, request.cid))
        .collect();
    summary_line("open_finished_ms", &finished);
    summary_line("open_first_frame_ms", &first_frame);
    summary_line("open_gtk_paint_ms", &painted);
    summary_line("open_gtk_paint_at_final_height_ms", &sized);
    summary_line("open_ms(max(finished,first_frame))", &opened);
    summary_line("plain_text_paint_ms", &plain);
    println!(
        "SUMMARY height_ok {height_ok}/{height_total} failures={height_failures:?} hscroll={hscroll:?}"
    );
    println!(
        "SUMMARY cid_images_loaded {}/{} cid_misses={cid_misses:?}",
        cid.0, cid.1
    );
    println!(
        "SUMMARY canary_during_corpus connections={} requests={}",
        leaks.connections,
        leaks.hits.len()
    );
    if ctx.args.snapshots {
        println!("SUMMARY blank {}/{snapshots} {blank:?}", blank.len());
    }
    let p95 = stats::summarize(&opened).map_or(f64::INFINITY, |summary| summary.p95);
    let pass = height_ok == height_total
        && leaks.is_empty()
        && cid.0 == cid.1
        && p95 <= 150.0
        && blank.is_empty();
    ctx.pane.column.remove(&root);
    i32::from(!pass)
}

async fn plain_text_latency(ctx: &Ctx) -> Vec<f64> {
    let mut times = Vec::new();
    for (index, message) in corpus::plain_text().iter().enumerate() {
        let (root, frame) = ui::message(message, index);
        ui::set_style(&frame, BodyStyle::Themed);
        ctx.pane.column.prepend(&root);
        sleep(30).await;
        let paints = PaintLog::start(&ctx.pane.column);
        let start = Instant::now();
        frame.append(&ui::plain_text(message));
        let deadline = start + Duration::from_secs(2);
        while Instant::now() < deadline
            && paints
                .as_ref()
                .is_some_and(|log| log.times.borrow().is_empty())
        {
            sleep(1).await;
        }
        if let Some(first) = paints
            .map(PaintLog::stop)
            .and_then(|times| times.first().copied())
        {
            times.push(ms(start, first.0));
        }
        ctx.pane.column.remove(&root);
    }
    times
}

fn conversation_messages(ctx: &Ctx) -> Vec<Message> {
    let newsletters = if ctx.args.short {
        corpus::simple_html()
    } else {
        corpus::newsletters(&ctx.canary.origin(), &ctx.assets)
    };
    let plain = corpus::plain_text();
    let mut messages = Vec::new();
    for index in 0..ctx.args.views {
        let pick = if ctx.args.short {
            index % newsletters.len()
        } else {
            (index * corpus::VARIANTS + index % corpus::VARIANTS) % newsletters.len()
        };
        messages.push(newsletters[pick].clone());
        if index % 3 == 2 {
            messages.push(plain[(index / 3) % plain.len()].clone());
        }
    }
    messages.extend(
        ctx.args
            .heights
            .iter()
            .map(|&height| corpus::sized_message(height)),
    );
    if ctx.args.tall {
        let tall = corpus::tall_message(400);
        for suffix in ["gpu", "cpu"] {
            messages.push(Message {
                id: format!("{}-{suffix}", tall.id),
                ..tall.clone()
            });
        }
    }
    messages
}

struct Body {
    message: Message,
    view: Rc<MessageView>,
    frame: gtk::Box,
}

/// A conversation with `--views` HTML bodies open at once: heights, memory, blank frames and,
/// with `--scroll`, frame times while scrolling it.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The same messages as one document for the approach-B proxy: an empty iframe per HTML
/// body (filled from Mail's world, see [`FILL_FRAMES_JS`]), plain text inline.
fn approach_b_document(messages: &[Message]) -> String {
    let mut html = String::from(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         html,body{margin:0;background:#121212}section{max-width:860px;margin:0 auto;padding:10px 0}\
         header{color:#bebebe;font:14px sans-serif;padding:6px 2px}\
         iframe{display:block;width:100%;border:0;height:400px;background:#fff}\
         pre{white-space:pre-wrap;margin:0;padding:14px 18px;color:#bebebe;font:15px/1.55 sans-serif}\
         </style></head><body>",
    );
    for message in messages {
        let _ = write!(
            html,
            "<section><header><b>{}</b> &middot; {}</header>",
            escape_html(&message.sender),
            escape_html(&message.subject)
        );
        if message.kind == Kind::PlainText {
            let _ = write!(html, "<pre>{}</pre>", escape_html(&message.body));
        } else {
            html.push_str("<iframe></iframe>");
        }
        html.push_str("</section>");
    }
    html.push_str("</body></html>");
    html
}

/// With `enable-javascript-markup` off WebKit strips `srcdoc` attributes while parsing (they
/// can carry script), so the proxy's frames get their documents from Mail's world.
const FILL_FRAMES_JS: &str = r#"
const bodies = [BODIES];
const frames = [...document.querySelectorAll('iframe')];
await Promise.all(frames.map((frame, index) => new Promise(resolve => {
  frame.addEventListener('load', resolve, { once: true });
  frame.srcdoc = bodies[index];
})));
return frames.length;
"#;

const FIT_FRAMES_JS: &str = r#"
const heights = [];
for (const frame of document.querySelectorAll('iframe')) {
  const doc = frame.contentDocument;
  if (!doc || !doc.documentElement) { heights.push('none'); continue; }
  frame.style.height = '1px';
  const height = Math.ceil(doc.documentElement.getBoundingClientRect().height);
  frame.style.height = height + 'px';
  heights.push(height);
}
return heights.join(' ');
"#;

const SCROLL_B_JS: &str = r#"
return await new Promise(resolve => {
  const speed = 2.4;
  const stamps = [];
  let last = null, y = 0, direction = 1;
  const start = performance.now();
  const max = document.documentElement.scrollHeight - window.innerHeight;
  const step = (now) => {
    if (last !== null) {
      y += direction * speed * (now - last);
      if (y >= max) { y = max; direction = -1; }
      if (y <= 0) { y = 0; direction = 1; }
    }
    last = now;
    stamps.push(now);
    window.scrollTo(0, y);
    if (now - start < seconds * 1000) requestAnimationFrame(step); else resolve(stamps.join(','));
  };
  requestAnimationFrame(step);
});
"#;

/// Approach-B proxy: one viewport-sized view holding the conversation, for comparing
/// memory and scrolling with approach A on the same content.
async fn conversation_b(ctx: &Ctx) -> i32 {
    let gpu_before = gpu_memory_used();
    let messages = conversation_messages(ctx);
    let defences = Defences {
        name: "approach-b-proxy",
        csp: None,
        navigation: Navigation::AllowOnly(vec!["about:srcdoc".into(), "about:blank".into()]),
        fit_height: false,
        ..Defences::production()
    };
    let view = ctx.engine.new_view(&defences);
    let page = ctx.pane.scroller.height() - 40;
    view.view.set_size_request(-1, page);
    ctx.pane.column.append(&ui::BodyBox::new(&view.view));
    let parts: Vec<Part> = messages
        .iter()
        .flat_map(|message| message.parts.clone())
        .collect();
    ctx.engine.set_parts(&view.view, &parts);
    let start = Instant::now();
    view.load(
        &approach_b_document(&messages),
        &theme::reader_css(&ctx.palette, BodyStyle::Paper, &ctx.sans),
        &ui::background(&ctx.palette, BodyStyle::Themed),
    );
    let settled = settle(
        &view,
        start,
        Duration::from_secs(20),
        Duration::from_millis(300),
    )
    .await;
    let bodies: Vec<String> = messages
        .iter()
        .filter(|message| message.kind != Kind::PlainText)
        .map(|message| message.body.clone())
        .collect();
    let arguments = glib::VariantDict::new(None);
    let mut names = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        let name = format!("body{index}");
        arguments.insert_value(&name, &body.to_variant());
        names.push(name);
    }
    let fill = FILL_FRAMES_JS.replace("BODIES", &names.join(","));
    let filled = view
        .view
        .call_async_javascript_function_future(
            &fill,
            Some(&arguments.end()),
            Some(crate::reader::WORLD),
            None,
        )
        .await
        .map_or_else(
            |error| error.to_string(),
            |value| value.to_str().to_string(),
        );
    let fitted = view.eval(FIT_FRAMES_JS).await.map_or_else(
        |error| error.to_string(),
        |value| value.to_str().to_string(),
    );
    sleep(500).await;
    let content = probe(&view).await.unwrap_or_default();
    println!(
        "SUMMARY approach_b messages={} frames_filled={filled} frame_heights=[{fitted}] view_height={page} content_height={} ready_ms={} timeout={}",
        messages.len(),
        content.scroll_height,
        opt(settled
            .finished
            .zip(settled.frame)
            .map(|(a, b)| ms(start, a.max(b)))),
        settled.timed_out
    );
    sleep(1500).await;
    let memory_ok = memory_report(&ctx.args, gpu_before);
    let blank = render_when_painted(&view.view, Duration::from_secs(3))
        .await
        .map(|texture| ui::uniformity(&texture));
    println!(
        "SUMMARY approach_b_blank {}",
        blank
            .as_ref()
            .map_or_else(|error| error.clone(), ToString::to_string)
    );
    let mut scroll_ok = true;
    if ctx.args.scroll_secs > 0.0 {
        let paints = PaintLog::start(&view.view);
        let script = SCROLL_B_JS.replace("seconds", &ctx.args.scroll_secs.to_string());
        let stamps = view
            .eval(&script)
            .await
            .map(|value| value.to_str().to_string())
            .unwrap_or_default();
        let paints = paints.map(PaintLog::stop).unwrap_or_default();
        let frames: Vec<f64> = stamps
            .split(',')
            .filter_map(|stamp| stamp.parse::<f64>().ok())
            .collect::<Vec<_>>()
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect();
        let gtk: Vec<f64> = paints
            .windows(2)
            .map(|pair| ms(pair[0].0, pair[1].0))
            .collect();
        summary_line("approach_b_scroll_webkit_frame_interval_ms", &frames);
        summary_line("approach_b_scroll_gtk_paint_interval_ms", &gtk);
        scroll_ok = stats::summarize(&frames).is_some_and(|summary| summary.p95 <= 16.7);
    }
    ui::clear(&ctx.pane.column);
    i32::from(!(memory_ok && scroll_ok && blank.is_ok_and(|u| !u.blank())))
}

async fn conversation(ctx: &Ctx) -> i32 {
    if ctx.args.approach_b {
        return conversation_b(ctx).await;
    }
    let baseline = procmem::tree(std::process::id());
    println!(
        "MEM-BASELINE processes={} rss_mib={:.1} pss_mib={:.1}",
        baseline.len(),
        baseline.iter().map(|p| p.rss_kib).sum::<u64>() as f64 / 1024.0,
        baseline.iter().filter_map(|p| p.pss_kib).sum::<u64>() as f64 / 1024.0
    );
    let gpu_before = gpu_memory_used();
    let messages = conversation_messages(ctx);
    let mut bodies = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let (root, frame) = ui::message(message, index);
        ctx.pane.column.append(&root);
        if message.kind == Kind::PlainText {
            ui::set_style(&frame, BodyStyle::Themed);
            frame.append(&ui::plain_text(message));
            continue;
        }
        let defences = Defences {
            cpu_rendering: message.id.ends_with("-cpu"),
            ..Defences::production()
        };
        let view = Rc::new(ctx.engine.new_view(&defences));
        frame.append(&ui::BodyBox::new(&view.view));
        bodies.push(Body {
            message: message.clone(),
            view,
            frame,
        });
    }
    let start = Instant::now();
    for body in &bodies {
        let style = style_for(&body.message);
        ui::set_style(&body.frame, style);
        ctx.engine.set_parts(&body.view.view, &body.message.parts);
        body.view.load(
            &body.message.body,
            &theme::reader_css(&ctx.palette, style, &ctx.sans),
            &ui::background(&ctx.palette, style),
        );
    }
    let tasks: Vec<_> = bodies
        .iter()
        .map(|body| {
            let view = Rc::clone(&body.view);
            glib::MainContext::default().spawn_local(async move {
                let mut settled = settle(
                    &view,
                    start,
                    Duration::from_secs(20),
                    Duration::from_millis(300),
                )
                .await;
                follow_heights(&view, &mut settled).await;
                settled
            })
        })
        .collect();
    let mut all_ready = Vec::new();
    let mut height_ok = 0;
    for (body, task) in bodies.iter().zip(tasks) {
        let settled = task.await.unwrap_or_default();
        let probe = settled_probe(&body.view, Duration::from_secs(2)).await;
        let check = check_height(&body.view, settled.height.map(|(h, _)| h), &probe);
        height_ok += usize::from(check.ok);
        if let (Some(finished), Some(frame)) = (settled.finished, settled.frame) {
            all_ready.push(ms(start, finished.max(frame)));
        }
        println!(
            "ROW conversation id={} applied={} allocated={} scrollHeight={} height_ok={} cid={}/{} ready_ms={} timeout={}",
            body.message.id,
            check.applied,
            check.allocated,
            probe.scroll_height,
            check.ok,
            probe.cid_loaded,
            probe.cid_images,
            opt(settled
                .finished
                .zip(settled.frame)
                .map(|(a, b)| ms(start, a.max(b)))),
            settled.timed_out,
        );
    }
    println!(
        "SUMMARY conversation views={} height_ok={height_ok}/{} all_ready_ms={:.1} total_height={}",
        bodies.len(),
        bodies.len(),
        all_ready.iter().copied().fold(0.0, f64::max),
        ctx.pane.column.height()
    );
    sleep(2000).await;
    let memory_ok = memory_report(&ctx.args, gpu_before);
    let mut blank = Vec::new();
    for body in &bodies {
        match render_when_painted(&body.view.view, Duration::from_secs(3))
            .await
            .map(|texture| ui::uniformity(&texture))
        {
            Ok(uniformity) if !uniformity.blank() => {}
            Ok(uniformity) => blank.push(format!("{}:{uniformity}", body.message.id)),
            Err(error) => blank.push(format!("{}:{error}", body.message.id)),
        }
    }
    println!(
        "SUMMARY conversation_blank {}/{} {blank:?}",
        blank.len(),
        bodies.len()
    );
    if let Some(dir) = &ctx.args.save_dir
        && let Ok(texture) = ui::render_widget(&ctx.pane.window)
    {
        let _ = texture.save_to_png(dir.join("conversation-window.png"));
    }
    let mut scroll_ok = true;
    if ctx.args.scroll_secs > 0.0 {
        scroll_ok = scroll(ctx, ctx.args.scroll_secs).await;
    }
    ui::clear(&ctx.pane.column);
    i32::from(!(height_ok == bodies.len() && blank.is_empty() && memory_ok && scroll_ok))
}

/// GPU memory in use on the whole device (MiB), from `nvidia-smi` when it exists.
fn gpu_memory_used() -> Option<f64> {
    let output = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn memory_report(args: &Args, gpu_before: Option<f64>) -> bool {
    let processes = procmem::tree(std::process::id());
    let mut by_name: BTreeMap<String, (usize, u64, u64)> = BTreeMap::new();
    let mut pss_complete = true;
    for process in &processes {
        println!(
            "MEM pid={} ppid={} name={} rss_kib={} pss_kib={}",
            process.pid,
            process.ppid,
            process.name,
            process.rss_kib,
            process
                .pss_kib
                .map_or_else(|| "unreadable".into(), |pss| pss.to_string())
        );
        let entry = by_name.entry(process.name.clone()).or_default();
        entry.0 += 1;
        entry.1 += process.rss_kib;
        entry.2 += process.pss_kib.unwrap_or(0);
        pss_complete &= process.pss_kib.is_some();
    }
    for (name, (count, rss, pss)) in &by_name {
        println!(
            "MEM-GROUP name={name} count={count} rss_mib={:.1} pss_mib={:.1}",
            *rss as f64 / 1024.0,
            *pss as f64 / 1024.0
        );
    }
    let rss: u64 = processes.iter().map(|process| process.rss_kib).sum();
    let pss: u64 = processes.iter().filter_map(|process| process.pss_kib).sum();
    let relevant: u64 = processes
        .iter()
        .filter(|process| {
            process.pid == std::process::id()
                || process.name.starts_with("WebKitWebProces")
                || process.name.starts_with("WebKitNetworkPr")
                || process.name.starts_with("WebKitGPUProces")
        })
        .map(|process| process.rss_kib)
        .sum();
    let gpu_after = gpu_memory_used();
    println!(
        "SUMMARY memory views={} short={} share_process={} rss_app_and_webkit_mib={:.1} rss_tree_mib={:.1} pss_tree_mib={:.1} pss_complete={pss_complete} gpu_device_used_mib_before={} after={}",
        args.views,
        args.short,
        args.share_process,
        relevant as f64 / 1024.0,
        rss as f64 / 1024.0,
        pss as f64 / 1024.0,
        gpu_before.map_or_else(|| "-".into(), |v| v.to_string()),
        gpu_after.map_or_else(|| "-".into(), |v| v.to_string()),
    );
    relevant as f64 / 1024.0 <= 300.0
}

#[derive(Default)]
struct ScrollLog {
    ticks: Vec<i64>,
    starts: BTreeMap<i64, Instant>,
    work_ms: Vec<f64>,
    pending: Vec<i64>,
    presented: BTreeMap<i64, (i64, i64)>,
    incomplete: usize,
    direction: f64,
    last_time: Option<i64>,
    bounces: usize,
}

fn poll_timings(clock: &gdk::FrameClock, log: &mut ScrollLog, all: bool) {
    let current = clock.frame_counter();
    let pending = std::mem::take(&mut log.pending);
    for counter in pending {
        if !all && current - counter < 3 {
            log.pending.push(counter);
            continue;
        }
        match clock.timings(counter) {
            Some(timings) if timings.is_complete() => {
                log.presented.insert(
                    counter,
                    (timings.presentation_time(), timings.refresh_interval()),
                );
            }
            Some(_) if !all && current - counter < 12 => log.pending.push(counter),
            _ => log.incomplete += 1,
        }
    }
}

/// Scrolls the conversation programmatically at `SPEED` px/s (bouncing at the ends) and
/// reports GdkFrameClock timings: presentation intervals, tick intervals and the UI thread's
/// work per frame.
async fn scroll(ctx: &Ctx, secs: f64) -> bool {
    const SPEED: f64 = 2400.0;
    let scroller = &ctx.pane.scroller;
    let Some(clock) = scroller.frame_clock() else {
        println!("SUMMARY scroll no frame clock");
        return false;
    };
    let adjustment = scroller.vadjustment();
    adjustment.set_value(0.0);
    sleep(300).await;
    let log = Rc::new(RefCell::new(ScrollLog {
        direction: 1.0,
        ..ScrollLog::default()
    }));
    let before = clock.connect_before_paint(glib::clone!(
        #[strong]
        log,
        move |clock| {
            log.borrow_mut()
                .starts
                .insert(clock.frame_counter(), Instant::now());
        }
    ));
    let after = clock.connect_after_paint(glib::clone!(
        #[strong]
        log,
        move |clock| {
            let mut log = log.borrow_mut();
            let counter = clock.frame_counter();
            if let Some(start) = log.starts.remove(&counter) {
                log.work_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                log.pending.push(counter);
            }
            poll_timings(clock, &mut log, false);
        }
    ));
    let (done, finished) = async_channel::bounded::<()>(1);
    let end = Instant::now() + Duration::from_secs_f64(secs);
    scroller.add_tick_callback(glib::clone!(
        #[strong]
        log,
        #[strong]
        adjustment,
        move |_, clock| {
            let mut log = log.borrow_mut();
            let now = clock.frame_time();
            let dt = log
                .last_time
                .map_or(0.0, |last| (now - last) as f64 / 1_000_000.0);
            log.last_time = Some(now);
            log.ticks.push(now);
            let max = adjustment.upper() - adjustment.page_size();
            let mut value = adjustment.value() + log.direction * SPEED * dt;
            if value >= max {
                value = max;
                log.direction = -1.0;
                log.bounces += 1;
            } else if value <= 0.0 && log.direction < 0.0 {
                value = 0.0;
                log.direction = 1.0;
                log.bounces += 1;
            }
            adjustment.set_value(value);
            if Instant::now() >= end {
                let _ = done.try_send(());
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        }
    ));
    let _ = finished.recv().await;
    sleep(250).await;
    clock.disconnect(before);
    clock.disconnect(after);
    let mut log = log.borrow_mut();
    poll_timings(&clock, &mut log, true);
    let ticks: Vec<f64> = log
        .ticks
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as f64 / 1000.0)
        .collect();
    let presented: Vec<(i64, i64, i64)> = log
        .presented
        .iter()
        .map(|(&counter, &(time, refresh))| (counter, time, refresh))
        .collect();
    let presentation: Vec<f64> = presented
        .windows(2)
        .filter(|pair| pair[1].0 == pair[0].0 + 1 && pair[0].1 > 0 && pair[1].1 > 0)
        .map(|pair| (pair[1].1 - pair[0].1) as f64 / 1000.0)
        .collect();
    let refresh = presented
        .iter()
        .map(|&(_, _, refresh)| refresh)
        .find(|&refresh| refresh > 0)
        .map_or(0.0, |refresh| refresh as f64 / 1000.0);
    println!(
        "SUMMARY scroll secs={secs} speed_px_s={SPEED} content_height={:.0} page={:.0} bounces={} frames={} presented={} incomplete={} refresh_ms={refresh:.3}",
        adjustment.upper(),
        adjustment.page_size(),
        log.bounces,
        log.ticks.len(),
        log.presented.len(),
        log.incomplete,
    );
    summary_line("scroll_presentation_interval_ms", &presentation);
    summary_line("scroll_tick_interval_ms", &ticks);
    summary_line("scroll_ui_frame_work_ms", &log.work_ms);
    let over = |values: &[f64], limit: f64| values.iter().filter(|&&v| v > limit).count();
    println!(
        "SUMMARY scroll_long_frames presentation>16.7ms={} tick>16.7ms={} presentation>1.5x_refresh={}",
        over(&presentation, 16.7),
        over(&ticks, 16.7),
        if refresh > 0.0 {
            over(&presentation, refresh * 1.5)
        } else {
            0
        }
    );
    let measured = if presentation.len() >= 10 {
        &presentation
    } else {
        &ticks
    };
    stats::summarize(measured).is_some_and(|summary| summary.p95 <= 16.7)
}

async fn live(ctx: &Ctx) -> i32 {
    let gpu = gpu(ctx).await;
    let corpus = corpus(ctx).await;
    let conversation = conversation(ctx).await;
    println!("SUMMARY live gpu={gpu} corpus={corpus} conversation={conversation}");
    i32::from(gpu != 0 || corpus != 0 || conversation != 0)
}

#[derive(Debug, Default)]
struct CaseResult {
    hits: Vec<String>,
    connections: u64,
    silent: u64,
    tls: u64,
    title_changed: bool,
    dom_marker: bool,
    dialogs: Vec<String>,
    navigations: Vec<String>,
    links: Vec<String>,
    new_windows: Vec<String>,
    downloads: Vec<String>,
    resources: Vec<String>,
    cid_ok: Option<bool>,
    foreign_loaded: bool,
    height: i32,
    height_reports: usize,
    failures: Vec<String>,
}

impl CaseResult {
    fn network(&self) -> bool {
        self.connections > 0 || !self.hits.is_empty()
    }

    fn script(&self) -> bool {
        self.title_changed || self.dom_marker || !self.dialogs.is_empty()
    }
}

fn defence_matrix(origin: &str) -> Vec<Defences> {
    let production = Defences::production();
    vec![
        production.clone(),
        Defences {
            name: "production-without-link-interception",
            intercept_links: false,
            ..production.clone()
        },
        Defences {
            name: "production-without-feature-switches",
            network_features_off: false,
            ..production.clone()
        },
        Defences {
            name: "adr-011-as-written",
            intercept_links: false,
            network_features_off: false,
            ..production.clone()
        },
        Defences {
            name: "filter-only",
            csp: None,
            ..production.clone()
        },
        Defences {
            name: "csp-only",
            content_filter: false,
            ..production.clone()
        },
        Defences {
            name: "markup-off-only",
            csp: None,
            content_filter: false,
            intercept_links: false,
            network_features_off: false,
            ..production.clone()
        },
        Defences {
            name: "remote-allowed",
            content_filter: false,
            csp: Some(content_security_policy(Some(origin))),
            ..production.clone()
        },
        Defences {
            name: "remote-allowed-upgrade",
            content_filter: false,
            csp: Some(format!(
                "{}; upgrade-insecure-requests",
                content_security_policy(None)
            )),
            ..production.clone()
        },
        Defences {
            name: "control-no-defences",
            page_scripts: true,
            csp: None,
            content_filter: false,
            navigation: Navigation::AllowOnly(vec![
                origin.to_owned(),
                "about:srcdoc".into(),
                "data:".into(),
            ]),
            hardened_settings: false,
            network_features_off: false,
            intercept_links: false,
            fit_height: true,
            cpu_rendering: false,
        },
    ]
}

async fn run_case(ctx: &Ctx, view: &MessageView, case: &hostile::Case) -> CaseResult {
    let ok_part = Part {
        cid: hostile::OK_CID.into(),
        content_type: "image/png",
        data: Arc::from(email_spike_s2::png::picture(
            16,
            16,
            [200, 0, 0],
            [0, 0, 200],
        )),
    };
    ctx.engine
        .set_parts(&view.view, std::slice::from_ref(&ok_part));
    let before = ctx.canary.snapshot();
    let downloads_before = ctx.engine.downloads.borrow().len();
    let canary = ctx.canary.origin();
    let short = |uri: String| uri.replace(&canary, "{canary}");
    view.drain();
    let start = Instant::now();
    view.load(
        &case.html,
        &theme::reader_css(&ctx.palette, BodyStyle::Themed, &ctx.sans),
        &ui::background(&ctx.palette, BodyStyle::Themed),
    );
    let mut events = Vec::new();
    let deadline = start + Duration::from_secs(3);
    let mut finished = false;
    while !finished {
        match recv_until(view, deadline).await {
            Some(stamped) => {
                finished = matches!(stamped.event, Event::Finished);
                events.push(stamped);
            }
            None => break,
        }
    }
    sleep(300).await;
    let mut result = CaseResult::default();
    if !finished {
        result.failures.push("no load-finished within 3 s".into());
    }
    if let Some(selector) = case.click {
        let click = format!(
            "const el = document.querySelector('{selector}'); if (el) {{ el.click(); return true; }} return false;"
        );
        match view.eval(&click).await {
            Ok(value) if value.to_boolean() => {}
            Ok(_) => result.failures.push(format!("{selector} not found")),
            Err(error) => result.failures.push(format!("click: {error}")),
        }
        sleep(700).await;
    }
    let title = view.view.title().map(String::from).unwrap_or_default();
    result.title_changed = title.contains("pwned");
    match probe(view).await {
        Ok(probe) => {
            result.dom_marker = !probe.pwned.is_empty();
            result.cid_ok = (probe.ok_images > 0.0).then_some(probe.ok_loaded == probe.ok_images);
            result.foreign_loaded = probe.foreign_loaded > 0.0;
        }
        Err(error) => result.failures.push(format!("probe: {error}")),
    }
    sleep(150).await;
    events.extend(view.drain());
    for stamped in events {
        match stamped.event {
            Event::Dialog(text) => result.dialogs.push(text),
            Event::Height(height) => {
                result.height = height;
                result.height_reports += 1;
            }
            Event::Navigation { uri, kind, blocked } => result.navigations.push(format!(
                "{}{}:{}",
                if blocked { "blocked:" } else { "allowed:" },
                kind,
                short(uri)
            )),
            Event::NewWindow(uri) => result.new_windows.push(short(uri)),
            Event::Link(uri) => result.links.push(short(uri)),
            Event::Download(uri) => result.downloads.push(short(uri)),
            Event::Resource(uri) if !uri.starts_with("about:") => result.resources.push(short(uri)),
            Event::Failed(error) => result.failures.push(error),
            Event::Crashed(reason) => result.failures.push(format!("web process: {reason}")),
            _ => {}
        }
    }
    let engine_downloads = ctx.engine.downloads.borrow()[downloads_before..].to_vec();
    result
        .downloads
        .extend(engine_downloads.into_iter().map(short));
    let leaks = ctx.canary.snapshot().since(&before);
    result.hits = leaks
        .hits
        .iter()
        .map(|hit| format!("{} {}", hit.method, hit.path))
        .collect();
    result.connections = leaks.connections;
    result.silent = leaks.silent_connections;
    result.tls = leaks.tls_hellos;
    result
}

/// Every hostile case under every combination of defences. Production must show no
/// request, no connection and no script trace; the controls show each vector is live.
async fn hostile(ctx: &Ctx) -> i32 {
    let origin = ctx.canary.origin();
    let cases: Vec<_> = hostile::cases(&origin)
        .into_iter()
        .filter(|case| {
            ctx.args
                .cases
                .as_ref()
                .is_none_or(|wanted| wanted.iter().any(|id| id == case.id))
        })
        .collect();
    let configs: Vec<_> = defence_matrix(&origin)
        .into_iter()
        .filter(|config| {
            ctx.args
                .configs
                .as_ref()
                .is_none_or(|wanted| wanted.iter().any(|name| name == config.name))
        })
        .collect();
    ctx.engine.set_parts_for_page(
        u64::MAX - 1,
        &[Part {
            cid: hostile::FOREIGN_CID.into(),
            content_type: "image/png",
            data: Arc::from(email_spike_s2::png::picture(16, 16, [0, 200, 0], [0, 0, 0])),
        }],
    );
    let mut production_failures = Vec::new();
    for config in &configs {
        let view = ctx.engine.new_view(config);
        let message = Message {
            id: config.name.into(),
            kind: Kind::SimpleHtml,
            family: "hostile",
            sender: "Hostile Suite".into(),
            subject: config.name.into(),
            body: String::new(),
            parts: Vec::new(),
        };
        let (root, frame) = ui::message(&message, 0);
        ui::set_style(&frame, BodyStyle::Themed);
        view.view.set_size_request(-1, 400);
        frame.append(&ui::BodyBox::new(&view.view));
        ctx.pane.column.append(&root);
        let mut network = 0;
        let mut scripts = 0;
        for case in &cases {
            let result = run_case(ctx, &view, case).await;
            network += usize::from(result.network());
            scripts += usize::from(result.script());
            println!(
                "ROW hostile config={} case={} network={} connections={} silent={} tls={} requests={:?} script={} title={} dom={} dialogs={:?} navigations={:?} links={:?} new_windows={:?} downloads={:?} cid_ok={:?} foreign_loaded={} height={} height_reports={} resources={:?} failures={:?}",
                config.name,
                case.id,
                result.network(),
                result.connections,
                result.silent,
                result.tls,
                result.hits,
                result.script(),
                result.title_changed,
                result.dom_marker,
                result.dialogs,
                result.navigations,
                result.links,
                result.new_windows,
                result.downloads,
                result.cid_ok,
                result.foreign_loaded,
                result.height,
                result.height_reports,
                result.resources,
                result.failures,
            );
            if config.name == "production" {
                let navigated = result.navigations.iter().any(|n| n.starts_with("allowed:"));
                if result.network()
                    || result.script()
                    || result.foreign_loaded
                    || result.cid_ok == Some(false)
                    || result.height_reports > 12
                    || navigated
                    || !result.downloads.is_empty()
                    || !result.new_windows.is_empty()
                {
                    production_failures.push(case.id);
                }
            }
        }
        println!(
            "SUMMARY hostile config={} cases={} with_network={network} with_script={scripts}",
            config.name,
            cases.len()
        );
        ctx.pane.column.remove(&root);
    }
    println!(
        "SUMMARY hostile production_failures={production_failures:?} cases={}",
        cases.len()
    );
    i32::from(!production_failures.is_empty())
}
