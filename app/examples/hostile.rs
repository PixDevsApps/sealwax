//! The hostile-HTML suite against the real reader (ML-027, ML-065): S2's 47 cases plus M5's
//! surfaces (link hover and the mismatch warning, plain-text links, collapsed quotes,
//! format=flowed, encrypted and signed mail, a `cid:` answer for a view that moved on,
//! page-initiated printing, names for the DNS canary). Each case is a stored message, shown
//! by `Reader::show` through the store, the blob store and the pooled, hardened view, while a
//! canary on 127.0.0.1 counts connections, requests and TLS ClientHellos and the probes look
//! for script traces (title, DOM marker, dialogs, beacon). `tools/ci.sh --ui` runs it:
//!
//! ```sh
//! SEALWAX_HEADLESS_TIMEOUT=900 tools/headless.sh 33 env \
//!     LD_PRELOAD=$PWD/target/debug/libsealwax_dns_canary.so \
//!     SEALWAX_DNS_CANARY_LOG=$PWD/.local/hostile-dns.log target/debug/examples/hostile
//! ```
//!
//! With the DNS canary preloaded (`devtools/dns-canary`), every name the app or WebKit looks
//! up is recorded and refused; without it the DNS column says nothing.
//!
//! Five passes, one row per case: `prod` (the reader as shipped; clicks and hovers are DOM
//! events from the isolated world, as in S2), `real` (cases with a click or a hover: real
//! pointer input through Broadway), `shown` (HTML cases after "Show images": only https images
//! may load, upgraded from http, so the canary sees TLS hellos and never a request), `again`
//! (`prod` in a second reader that reuses the pooled views `shown` lifted the filter from) and
//! `ctrl` (a bare WebView with no defences, scripts on, proving the vectors are live). Bodies
//! collapse their quotes. Exits 0 when `prod`, `real` and `again` are clean in every case,
//! M5's expectations hold, both controls saw traffic, the DNS canary (when preloaded) saw
//! names only from the control, and GLib wrote no critical. Links are recorded, never opened;
//! every clicked link points at the canary or at a name that never resolves.

#[path = "hostile_support/canary.rs"]
mod canary;
#[path = "hostile_support/cases.rs"]
mod cases;
#[path = "reader_support/mod.rs"]
mod support;

use canary::{Canary, Log};
use cases::{Case, Expect, FOREIGN_CID, OK_CID, REUSED_ID, SLOW_CID};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_app::reader::samples::{self, Draft, Part};
use sealwax_app::reader::{
    CidOutcome, InlinePart, Reader, Refused, Shown, ShownBody, ViewEvent, links_in,
};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::ThemeOptions;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use support::broadway::{self, Input};
use webkit6::prelude::*;

const PROBE_JS: &str = r#"
const root = document.documentElement;
const images = [...document.images];
return {
  pwned: root ? (root.getAttribute('data-pwned') || '') : '',
  okImages: images.filter(i => i.id === 's2-ok').length,
  okLoaded: images.filter(i => i.id === 's2-ok' && i.complete && i.naturalWidth > 0).length,
  foreignLoaded: images.filter(i => i.id === 's2-foreign' && i.complete && i.naturalWidth > 0).length,
  url: document.URL,
};
"#;

/// A 1×1 PNG: the second message's part under the slow part's Content-ID.
const DOT_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
/// How long the suite's part loader takes to answer for the slow part.
const SLOW_PART: Duration = Duration::from_millis(900);

#[derive(Debug, Default, Clone)]
struct Outcome {
    log: Log,
    /// Names the DNS canary saw looked up during the case.
    dns: Vec<String>,
    title: String,
    pwned: String,
    dialogs: u32,
    refused: BTreeMap<String, u32>,
    /// Every link the message handed to Sealwax.
    links: Vec<String>,
    /// The links Sealwax would have opened (absolute http(s) or mailto).
    opened: Vec<String>,
    resources: Vec<String>,
    cid_ok: Option<bool>,
    foreign_loaded: bool,
    heights: u32,
    downloads: u32,
    url: String,
    /// The status strip's text after the hover, and whether the warning showed.
    status: Option<String>,
    warning: bool,
    /// Link warning dialogs presented.
    link_dialogs: u32,
    /// The quote events: blocks hidden and whether expanded.
    quotes: Vec<(u32, bool)>,
    failures: Vec<String>,
}

impl Outcome {
    fn script(&self) -> bool {
        self.title.contains("pwned") || !self.pwned.is_empty() || self.dialogs > 0
    }

    fn network(&self) -> bool {
        !self.log.is_quiet()
    }

    /// S2's production criteria, no DNS lookup, and the case's own expectations.
    fn clean(&self) -> bool {
        !self.network()
            && self.dns.is_empty()
            && !self.script()
            && !self.foreign_loaded
            && self.cid_ok != Some(false)
            && self.heights <= 12
            && self.downloads == 0
            && !self
                .resources
                .iter()
                .any(|uri| uri.starts_with("http") || uri.starts_with("{canary}"))
            && !self.refused.contains_key("NewWindow")
            && (self.url.is_empty() || self.url == "about:blank")
            && self.failures.is_empty()
    }

    fn mark(&self) -> String {
        let mut mark = String::new();
        if self.script() {
            mark.push('S');
        }
        mark.push_str(&self.log.mark());
        if !self.dns.is_empty() {
            mark.push_str(&format!("D{}", self.dns.len()));
        }
        if !self.failures.is_empty() {
            mark.push('!');
        }
        if mark.is_empty() { ".".into() } else { mark }
    }
}

#[derive(Default)]
struct Recorder {
    events: Vec<(MessageId, ViewEvent)>,
    opened: Vec<String>,
}

/// The DNS canary's log: one line per name looked up, `process<TAB>name`.
struct Dns {
    path: PathBuf,
}

impl Dns {
    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn snapshot(&self) -> usize {
        self.lines().len()
    }

    fn since(&self, earlier: usize) -> Vec<String> {
        self.lines().into_iter().skip(earlier).collect()
    }
}

fn messages(cases: &[Case], now: i64) -> Vec<(&'static str, Vec<u8>)> {
    let ok = glib::base64_decode(samples::LOGO_PNG_BASE64);
    let mut messages: Vec<(&'static str, Vec<u8>)> = cases
        .iter()
        .map(|case| {
            if let Some(raw) = &case.raw {
                return (case.id, raw.clone());
            }
            let mut draft = Draft::new(
                ("Hostile Suite", "hostile@example.test"),
                case.id,
                now - 60,
                &format!("hostile-{}", case.id),
            );
            draft.html = Some(case.html.clone());
            draft.inline = vec![Part::inline(OK_CID, "image/png", ok.clone())];
            if case.expect == Expect::CidCancelled {
                draft
                    .inline
                    .push(Part::inline(SLOW_CID, "image/png", Vec::new()));
            }
            (case.id, draft.render())
        })
        .collect();
    let mut other = Draft::new(
        ("Another Sender", "other@example.test"),
        "The message the foreign part belongs to",
        now - 120,
        "hostile-other",
    );
    other.html = Some(format!("<p><img src=\"cid:{FOREIGN_CID}\"></p>"));
    other.inline = vec![Part::inline(FOREIGN_CID, "image/png", ok)];
    messages.push(("other-message", other.render()));
    let mut next = Draft::new(
        ("Another Sender", "other@example.test"),
        "The message that reuses the slow part's Content-ID",
        now - 180,
        "hostile-next",
    );
    next.html = Some(format!(
        "<p><img id=\"s2-next\" src=\"cid:{SLOW_CID}\" alt=\"next\"></p>"
    ));
    next.inline = vec![Part::inline(
        SLOW_CID,
        "image/png",
        glib::base64_decode(DOT_PNG_BASE64),
    )];
    messages.push((REUSED_ID, next.render()));
    messages
}

fn main() -> glib::ExitCode {
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let wanted: Option<Vec<String>> = std::env::args()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--cases=").map(str::to_owned))
        .map(|list| list.split(',').map(str::to_owned).collect());
    let input = match broadway::connect() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("run this through tools/headless.sh: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let canary = match Canary::start() {
        Ok(canary) => Rc::new(canary),
        Err(error) => {
            eprintln!("could not start the canary: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let preloaded = std::env::var("LD_PRELOAD").is_ok_and(|value| value.contains("dns_canary"));
    let dns = std::env::var_os("SEALWAX_DNS_CANARY_LOG")
        .filter(|_| preloaded)
        .map(|path| Dns {
            path: PathBuf::from(path),
        });
    if let Some(dns) = &dns {
        let _ = std::fs::write(&dns.path, "");
    }
    let cases: Vec<Case> = cases::cases(&canary.origin())
        .into_iter()
        .filter(|case| {
            wanted
                .as_ref()
                .is_none_or(|wanted| wanted.iter().any(|id| id == case.id))
        })
        .collect();
    println!(
        "ENV cases={} canary=127.0.0.1:{} dns_canary={} webkit={}.{}.{} gtk={}.{}.{}",
        cases.len(),
        canary.port(),
        if dns.is_some() { "on" } else { "off" },
        webkit6::functions::major_version(),
        webkit6::functions::minor_version(),
        webkit6::functions::micro_version(),
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
    );
    for case in &cases {
        println!("CASE {:<20} {}", case.id, case.vector);
    }
    let root = support::scratch("hostile");
    let scratch = support::store(&root);
    let now = glib::real_time() / 1_000_000;
    let filled = samples::fill_store(&scratch.store, &scratch.blobs, messages(&cases, now))
        .expect("the hostile messages go into the scratch store");
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.Hostile")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let scratch = Rc::new(scratch);
    let cases = Rc::new(cases);
    let dns = Rc::new(dns);
    let started = Instant::now();
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let recorder: Rc<RefCell<Recorder>> = Rc::default();
            let readers = [
                support::reader(&scratch, &theme),
                support::reader(&scratch, &theme),
            ];
            for reader in &readers {
                reader.set_collapse_quotes(true);
                reader.set_link_opener(glib::clone!(
                    #[strong]
                    recorder,
                    move |_, uri| recorder.borrow_mut().opened.push(uri.to_owned())
                ));
                reader.connect_view_event(glib::clone!(
                    #[strong]
                    recorder,
                    move |id, event| recorder.borrow_mut().events.push((id, event.clone()))
                ));
            }
            let window = support::window(app, &readers[0], (1280, 900));
            window.present();
            let app = app.clone();
            let passed = passed.clone();
            let canary = canary.clone();
            let cases = cases.clone();
            let filled = filled.clone();
            let input = input.clone();
            let dns = dns.clone();
            glib::spawn_future_local(async move {
                let _keep = &theme;
                let context = Context {
                    readers,
                    current: Cell::new(0),
                    window,
                    canary,
                    dns,
                    recorder,
                    input,
                };
                let ok = run(&context, &cases, &filled).await;
                passed.set(ok);
                app.quit();
            });
        }
    ));
    app.run_with_args::<&str>(&[]);
    println!(
        "TIME {:.1} s for the whole suite",
        started.elapsed().as_secs_f64()
    );
    if passed.get() {
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}

struct Context {
    /// The second takes over for the `again` pass: its own allowed set, the same pooled views.
    readers: [Reader; 2],
    current: Cell<usize>,
    window: sealwax_app::window::Window,
    canary: Rc<Canary>,
    dns: Rc<Option<Dns>>,
    recorder: Rc<RefCell<Recorder>>,
    input: Input,
}

impl Context {
    fn reader(&self) -> &Reader {
        &self.readers[self.current.get()]
    }

    fn dns_snapshot(&self) -> usize {
        self.dns.as_ref().as_ref().map_or(0, Dns::snapshot)
    }

    fn dns_since(&self, earlier: usize) -> Vec<String> {
        self.dns
            .as_ref()
            .as_ref()
            .map(|dns| dns.since(earlier))
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Production,
    RealClicks,
    ImagesShown,
    /// Production again in a new reader, after `shown` lifted the filter of the pooled views.
    Again,
}

impl Pass {
    fn name(self) -> &'static str {
        match self {
            Pass::Production => "prod",
            Pass::RealClicks => "real",
            Pass::ImagesShown => "shown",
            Pass::Again => "again",
        }
    }

    /// Whether `case` runs in this pass.
    fn runs(self, case: &Case) -> bool {
        match self {
            Pass::Production => true,
            _ if case.expect == Expect::CidCancelled => false,
            Pass::RealClicks => case.click.is_some() || case.hover.is_some(),
            Pass::ImagesShown => case.is_html(),
            Pass::Again => true,
        }
    }
}

async fn run(context: &Context, cases: &[Case], filled: &samples::Filled) -> bool {
    let reports = [
        support::shown_reports(&context.readers[0]),
        support::shown_reports(&context.readers[1]),
    ];
    if let Err(error) = support::resize(context.window.window(), 1280, 900).await {
        println!("ERROR {error}");
        return false;
    }
    let mut rows: BTreeMap<&str, BTreeMap<&str, String>> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut traffic = BTreeMap::new();
    let mut production_dns = 0;
    for pass in [
        Pass::Production,
        Pass::RealClicks,
        Pass::ImagesShown,
        Pass::Again,
    ] {
        let pass_started = Instant::now();
        if pass == Pass::Again {
            context.current.set(1);
            context.window.set_reader(&context.reader().widget());
            context
                .window
                .set_reader_toolbar(&context.reader().toolbar());
        }
        let mut network = 0;
        let mut scripts = 0;
        for case in cases.iter().filter(|case| pass.runs(case)) {
            let Some(id) = filled.id(case.id) else {
                println!("ERROR {} was not stored", case.id);
                return false;
            };
            let outcome = if case.expect == Expect::CidCancelled {
                cid_cancelled(context, &reports[context.current.get()], id, filled).await
            } else {
                run_case(context, &reports[context.current.get()], case, id, pass).await
            };
            network += usize::from(outcome.network());
            scripts += usize::from(outcome.script());
            if pass != Pass::ImagesShown {
                production_dns += outcome.dns.len();
            }
            print_row(pass.name(), case, &outcome);
            rows.entry(case.id)
                .or_default()
                .insert(pass.name(), outcome.mark());
            if pass != Pass::ImagesShown && !outcome.clean() {
                failures.push(format!("{}/{}", pass.name(), case.id));
            }
            if pass == Pass::ImagesShown && !outcome.failures.is_empty() {
                failures.push(format!("{}/{}", pass.name(), case.id));
            }
        }
        println!(
            "SUMMARY pass={} with_network={network} with_script={scripts} seconds={:.1}",
            pass.name(),
            pass_started.elapsed().as_secs_f64()
        );
        traffic.insert(pass.name(), network);
    }
    let control_cases: Vec<&Case> = cases
        .iter()
        .filter(|case| case.is_html() && case.expect != Expect::CidCancelled)
        .collect();
    let control = control_pass(context, &control_cases).await;
    let mut control_network = 0;
    let mut control_scripts = 0;
    let mut control_dns = 0;
    for (case, outcome) in control_cases.iter().zip(&control) {
        control_network += usize::from(outcome.network());
        control_scripts += usize::from(outcome.script());
        control_dns += outcome.dns.len();
        print_row("ctrl", case, outcome);
        rows.entry(case.id)
            .or_default()
            .insert("ctrl", outcome.mark());
    }
    println!(
        "SUMMARY pass=ctrl with_network={control_network} with_script={control_scripts} \
         dns_lookups={control_dns}"
    );
    println!("MATRIX case                  prod   real  shown  again   ctrl");
    for case in cases {
        let row = &rows[case.id];
        let cell = |pass: &str| row.get(pass).cloned().unwrap_or_else(|| "-".into());
        println!(
            "MATRIX {:<20} {:>5} {:>6} {:>6} {:>6} {:>6}",
            case.id,
            cell("prod"),
            cell("real"),
            cell("shown"),
            cell("again"),
            cell("ctrl")
        );
    }
    let total = context.canary.snapshot();
    println!(
        "CANARY-TOTAL connections={} requests={} silent={} tls={}",
        total.connections,
        total.requests.len(),
        total.silent,
        total.tls_hellos
    );
    let blind = traffic.get("shown").copied().unwrap_or(0) == 0 || control_network == 0;
    if blind {
        println!("SUMMARY the controls saw no traffic: the canary proves nothing");
    }
    let dns = match context.dns.as_ref() {
        None => "not-observed (the DNS canary isn't preloaded)".to_owned(),
        Some(_) if control_dns == 0 => "blind (the control looked up no name either)".to_owned(),
        Some(_) => format!("production={production_dns} control={control_dns}"),
    };
    let criticals = support::criticals();
    println!(
        "SUMMARY production_failures={failures:?} cases={} dns={dns} glib_criticals={criticals}",
        cases.len()
    );
    let dns_blind = context.dns.is_some() && control_dns == 0;
    failures.is_empty() && !blind && !dns_blind && criticals == 0
}

fn print_row(pass: &str, case: &Case, outcome: &Outcome) {
    println!(
        "ROW pass={pass} case={} network={} connections={} silent={} tls={} requests={:?} \
         dns={:?} script={} title={:?} dom={:?} dialogs={} refused={:?} links={:?} opened={:?} \
         cid_ok={:?} foreign_loaded={} heights={} downloads={} url={:?} status={:?} \
         warning={} link_dialogs={} quotes={:?} resources={:?} failures={:?}",
        case.id,
        outcome.network(),
        outcome.log.connections,
        outcome.log.silent,
        outcome.log.tls_hellos,
        outcome.log.requests,
        outcome.dns,
        outcome.script(),
        outcome.title,
        outcome.pwned,
        outcome.dialogs,
        outcome.refused,
        outcome.links,
        outcome.opened,
        outcome.cid_ok,
        outcome.foreign_loaded,
        outcome.heights,
        outcome.downloads,
        outcome.url,
        outcome.status,
        outcome.warning,
        outcome.link_dialogs,
        outcome.quotes,
        outcome.resources,
        outcome.failures,
    );
}

async fn run_case(
    context: &Context,
    reports: &async_channel::Receiver<Shown>,
    case: &Case,
    id: MessageId,
    pass: Pass,
) -> Outcome {
    let reader = context.reader();
    let mut outcome = Outcome::default();
    let before = context.canary.snapshot();
    let dns_before = context.dns_snapshot();
    let downloads = reader.refused_downloads();
    let link_dialogs = reader.link_state().dialogs;
    {
        let mut recorder = context.recorder.borrow_mut();
        recorder.events.clear();
        recorder.opened.clear();
    }
    let short = |text: &str| text.replace(&context.canary.origin(), "{canary}");
    match support::show(reader, reports, id).await {
        Ok(shown) => {
            if pass == Pass::ImagesShown {
                let loads = loaded_count(context, id);
                reader.show_remote_images();
                let reloaded = support::wait_until(Duration::from_secs(3), || {
                    loaded_count(context, id) > loads
                })
                .await;
                if !reloaded {
                    outcome.failures.push("no reload after Show images".into());
                }
            }
            let expected = match (&case.expect, case.is_html()) {
                (Expect::Unreadable(_), _) => "unreadable",
                (_, true) => "html",
                (_, false) => "plain",
            };
            let body = match shown.body {
                ShownBody::Html { .. } => "html",
                ShownBody::Plain => "plain",
                ShownBody::Unreadable(_) => "unreadable",
                _ => "other",
            };
            if body != expected {
                outcome
                    .failures
                    .push(format!("shown as {:?}, not {expected}", shown.body));
            }
        }
        Err(error) => outcome.failures.push(error),
    }
    support::sleep(300).await;
    if let Some(target) = case.hover {
        let hovered = match (case.is_html(), pass) {
            (true, Pass::RealClicks) => real_hover(context, target).await,
            (true, _) => dom_event(reader, target, "mouseover").await,
            (false, Pass::RealClicks) => text_point(context, target).and_then(|(x, y)| {
                context
                    .input
                    .motion(x, y)
                    .map_err(|error| error.to_string())
            }),
            (false, _) => Ok(()),
        };
        if let Err(error) = hovered {
            outcome.failures.push(format!("hover: {error}"));
        }
        support::sleep(300).await;
        let state = reader.link_state();
        outcome.status = state.status.map(|status| short(&status));
        outcome.warning = state.warning.is_some();
        if pass == Pass::RealClicks {
            shots(context, case, "hover").await;
        }
    }
    if let Some(selector) = case.click {
        let quotes = case.expect == Expect::Quotes && pass == Pass::RealClicks;
        if quotes {
            shots(context, case, "collapsed").await;
        }
        let clicked = match (case.is_html(), pass) {
            (true, Pass::RealClicks) => real_click(context, selector).await,
            (true, _) => dom_click(reader, selector).await,
            (false, Pass::RealClicks) if selector == "toggle" => {
                widget_click(context, "reader-quote-toggle").await
            }
            (false, Pass::RealClicks) => text_point(context, selector)
                .and_then(|(x, y)| context.input.click(x, y).map_err(|error| error.to_string())),
            (false, _) if selector == "toggle" => named(&reader.widget(), "reader-quote-toggle")
                .and_downcast::<gtk::Button>()
                .map(|button| button.emit_clicked())
                .ok_or_else(|| "no quote toggle".to_owned()),
            (false, _) => Ok(()),
        };
        if let Err(error) = clicked {
            outcome.failures.push(format!("click: {error}"));
        }
        support::sleep(700).await;
        if quotes {
            shots(context, case, "expanded").await;
        }
    }
    expectations(context, case, pass, &mut outcome).await;
    if case.hover.is_some() {
        let left = match (case.is_html(), pass) {
            (_, Pass::RealClicks) => context.input.motion(2.0, 2.0).map_err(|e| e.to_string()),
            (true, _) => dom_event(reader, case.hover.unwrap_or_default(), "mouseout").await,
            (false, _) => Ok(()),
        };
        if let Err(error) = left {
            outcome.failures.push(format!("leaving the link: {error}"));
        }
        support::sleep(150).await;
        if reader.link_state().status.is_some() {
            outcome
                .failures
                .push("the status strip stayed after the pointer left".into());
        }
    }
    if case.is_html() {
        if let Some(view) = reader.web_view() {
            outcome.title = view.title().map(String::from).unwrap_or_default();
        }
        match reader.eval(PROBE_JS).await {
            Ok(value) => {
                let number = |key: &str| {
                    value
                        .object_get_property(key)
                        .map_or(0.0, |value| value.to_double())
                };
                let text = |key: &str| {
                    value
                        .object_get_property(key)
                        .map(|value| value.to_str().to_string())
                        .unwrap_or_default()
                };
                outcome.pwned = text("pwned");
                outcome.url = text("url");
                let (images, loaded) = (number("okImages"), number("okLoaded"));
                outcome.cid_ok = (images > 0.0).then_some(images == loaded);
                outcome.foreign_loaded = number("foreignLoaded") > 0.0;
            }
            Err(error) => outcome.failures.push(format!("probe: {error}")),
        }
    }
    support::sleep(150).await;
    {
        let recorder = context.recorder.borrow();
        for (event_id, event) in &recorder.events {
            if *event_id != id {
                continue;
            }
            match event {
                ViewEvent::Height { .. } => outcome.heights += 1,
                ViewEvent::Refused(Refused::Dialog) => outcome.dialogs += 1,
                ViewEvent::Refused(refused) => {
                    *outcome.refused.entry(format!("{refused:?}")).or_default() += 1;
                }
                ViewEvent::Link(link) => outcome.links.push(short(&link.uri)),
                ViewEvent::Quotes { blocks, expanded } => outcome.quotes.push((*blocks, *expanded)),
                ViewEvent::Resource(uri) if !uri.starts_with("about:") => {
                    outcome.resources.push(short(uri));
                }
                _ => {}
            }
        }
        outcome.opened = recorder.opened.iter().map(|uri| short(uri)).collect();
    }
    if case.expect == Expect::Quotes && case.is_html() {
        let collapsed = outcome
            .quotes
            .first()
            .is_some_and(|(blocks, open)| *blocks > 0 && !open);
        let expanded = case.click.is_none() || outcome.quotes.last().is_some_and(|(_, open)| *open);
        if !(collapsed && expanded) {
            outcome.failures.push(format!(
                "the quote didn't collapse and expand: {:?}",
                outcome.quotes
            ));
        }
    }
    outcome.link_dialogs = reader.link_state().dialogs - link_dialogs;
    if let Expect::Mismatch(_) = case.expect
        && case.click.is_some()
    {
        let wanted: &[&str] = if pass == Pass::RealClicks {
            &["{canary}/h/link-mismatch/c/8f3a91"]
        } else {
            &[]
        };
        if outcome.link_dialogs != 1 || outcome.opened != wanted {
            outcome.failures.push(format!(
                "a mismatched link: {} dialogs, opened {:?}, wanted 1 and {wanted:?}",
                outcome.link_dialogs, outcome.opened
            ));
        }
    }
    outcome.downloads = reader.refused_downloads() - downloads;
    outcome.log = context.canary.snapshot().since(&before);
    outcome.log.requests = outcome.log.requests.iter().map(|r| short(r)).collect();
    outcome.dns = context.dns_since(dns_before);
    outcome
}

/// With `SEALWAX_HOSTILE_SHOTS` set, the window (and the link warning, a surface of its own)
/// as PNGs in `.local/reader/`, to look at the strip, the warning and the dialog.
async fn shots(context: &Context, case: &Case, moment: &str) {
    if std::env::var_os("SEALWAX_HOSTILE_SHOTS").is_none() {
        return;
    }
    let _ = support::frames(context.window.window(), 2).await;
    let window = support::output(&format!("hostile-{}-{moment}.png", case.id));
    let _ = support::screenshot(context.window.window(), &window);
    if let Some(warning) = named(&context.reader().widget(), "reader-link-warning")
        .filter(|warning| warning.is_visible())
    {
        let path = support::output(&format!("hostile-{}-{moment}-warning.png", case.id));
        let _ = support::screenshot(&warning, &path);
    }
}

/// M5's expectations, checked after the hover and the click.
async fn expectations(context: &Context, case: &Case, pass: Pass, outcome: &mut Outcome) {
    let reader = context.reader();
    let origin = context.canary.origin();
    let short = |text: &str| text.replace(&origin, "{canary}");
    let hovered = case.is_html() || pass == Pass::RealClicks;
    match &case.expect {
        Expect::Status(status) if hovered => {
            if outcome.status.as_deref() != Some(short(status).as_str()) || outcome.warning {
                outcome.failures.push(format!(
                    "the strip showed {:?} (warning {}), wanted {:?}",
                    outcome.status,
                    outcome.warning,
                    short(status)
                ));
            }
        }
        Expect::Mismatch(status) => {
            if outcome.status.as_deref() != Some(short(status).as_str()) || !outcome.warning {
                outcome.failures.push(format!(
                    "the strip showed {:?} and warning {}, wanted {:?} with the warning",
                    outcome.status,
                    outcome.warning,
                    short(status)
                ));
            }
            let dialog =
                support::wait_until(Duration::from_secs(2), || reader.link_dialog().is_some())
                    .await;
            if pass == Pass::RealClicks {
                shots(context, case, "clicked").await;
            }
            match reader.link_dialog().filter(|_| dialog) {
                Some(dialog) if pass == Pass::RealClicks => {
                    dialog.emit_by_name::<()>("response", &[&"open"]);
                    dialog.force_close();
                }
                Some(dialog) => {
                    dialog.close();
                }
                None => outcome.failures.push("no warning dialog".into()),
            }
            support::sleep(200).await;
        }
        Expect::PlainLinks(wanted) => {
            let found: Vec<String> = links_in(&reader.widget());
            if &found != wanted {
                outcome
                    .failures
                    .push(format!("plain-text links {found:?}, wanted {wanted:?}"));
            }
            if pass == Pass::RealClicks {
                let opened = context.recorder.borrow().opened.clone();
                if !opened.is_empty() {
                    outcome
                        .failures
                        .push(format!("a click on javascript: text opened {opened:?}"));
                }
                let clicked = text_point(context, "plain-links/ok").and_then(|(x, y)| {
                    context.input.click(x, y).map_err(|error| error.to_string())
                });
                support::sleep(500).await;
                let opened = context.recorder.borrow().opened.clone();
                if clicked.is_err() || opened != [wanted[0].clone()] {
                    outcome.failures.push(format!(
                        "the http link opened {opened:?} ({clicked:?}), wanted {:?}",
                        wanted[0]
                    ));
                }
            }
        }
        Expect::Quotes if !case.is_html() => {
            let text = sealwax_app::reader::shown_text(&reader.widget());
            let shows_quote = text.contains("plain-quote/inside");
            let wanted = case.click.is_some();
            if shows_quote != wanted || !text.contains("Sounds good.") {
                outcome.failures.push(format!(
                    "the quote shows: {shows_quote}, wanted {wanted} after the toggle"
                ));
            }
        }
        Expect::Flowed(wanted) => {
            let text = sealwax_app::reader::shown_text(&reader.widget());
            if &text != wanted {
                outcome
                    .failures
                    .push(format!("flowed text {text:?}, wanted {wanted:?}"));
            }
        }
        Expect::Unreadable(secret) => {
            let shown = all_text(&reader.widget());
            if shown.contains(secret) || !shown.contains(sealwax_app::reader::ENCRYPTED) {
                outcome
                    .failures
                    .push("the ciphertext showed, or the notice didn't".into());
            }
        }
        Expect::Signed => {
            let shown = all_text(&reader.widget());
            if reader.state().attachments != 0
                || !shown.contains("Signed minutes.")
                || !shown.contains(sealwax_app::reader::SIGNED)
            {
                outcome.failures.push(format!(
                    "signed mail: {} cards, text and the signed line shown: {}",
                    reader.state().attachments,
                    shown.contains("Signed minutes.")
                ));
            }
        }
        _ => {}
    }
}

/// A slow `cid:` part, its view reused for another message with the same Content-ID: the
/// slow answer must be dropped, and the next message must show its own part.
async fn cid_cancelled(
    context: &Context,
    reports: &async_channel::Receiver<Shown>,
    id: MessageId,
    filled: &samples::Filled,
) -> Outcome {
    let reader = context.reader();
    let mut outcome = Outcome::default();
    let before = context.canary.snapshot();
    let dns_before = context.dns_snapshot();
    let logo = glib::Bytes::from_owned(glib::base64_decode(samples::LOGO_PNG_BASE64));
    reader.set_part_loader(Some(Rc::new(move |_message, _part| {
        let logo = logo.clone();
        Box::pin(async move {
            glib::timeout_future(SLOW_PART).await;
            Some(InlinePart {
                mime_type: "image/png".into(),
                data: logo,
            })
        })
    })));
    let asked = reader.cid_requests().len();
    reader.show(id);
    let requested = support::wait_until(Duration::from_secs(3), || {
        reader.cid_requests()[asked..]
            .iter()
            .any(|request| request.found && request.outcome == CidOutcome::Pending)
    })
    .await;
    let slow = reader.cid_requests()[asked..]
        .iter()
        .position(|request| request.outcome == CidOutcome::Pending)
        .map(|position| asked + position);
    if !requested || slow.is_none() {
        outcome
            .failures
            .push("the slow part was never asked for".into());
    }
    reader.clear();
    support::sleep(300).await;
    let next_asked = reader.cid_requests().len();
    match filled.id(REUSED_ID) {
        Some(next) => {
            if let Err(error) = support::show(reader, reports, next).await {
                outcome.failures.push(error);
            }
        }
        None => outcome
            .failures
            .push("the next message wasn't stored".into()),
    }
    support::sleep(SLOW_PART.as_millis() as u64 + 400).await;
    let requests = reader.cid_requests();
    let old = slow.and_then(|slow| requests.get(slow).cloned());
    let reused = requests[next_asked..]
        .iter()
        .any(|request| old.as_ref().is_some_and(|old| old.page == request.page));
    let served_next = requests[next_asked..]
        .iter()
        .filter(|request| request.outcome == CidOutcome::Served)
        .count();
    let width = reader
        .eval("const i = document.getElementById('s2-next'); return i ? i.naturalWidth : -1;")
        .await
        .map(|value| value.to_int32())
        .unwrap_or(-1);
    println!(
        "CID-CANCELLED slow={:?} view_reused={reused} next_served={served_next} \
         next_image_width={width}",
        old.as_ref().map(|old| old.outcome)
    );
    if old.as_ref().map(|old| old.outcome) != Some(CidOutcome::Dropped) {
        outcome
            .failures
            .push("the slow answer wasn't dropped".into());
    }
    if width != 1 {
        outcome.failures.push(format!(
            "the next message's image is {width} px wide, not its own 1 px part"
        ));
    }
    reader.set_part_loader(None);
    outcome.log = context.canary.snapshot().since(&before);
    outcome.dns = context.dns_since(dns_before);
    outcome
}

fn loaded_count(context: &Context, id: MessageId) -> usize {
    context
        .recorder
        .borrow()
        .events
        .iter()
        .filter(|(event_id, event)| *event_id == id && *event == ViewEvent::Loaded)
        .count()
}

async fn dom_click(reader: &Reader, selector: &str) -> Result<(), String> {
    let clicked = reader
        .eval(&format!(
            "const el = document.querySelector('{selector}'); \
             if (el) {{ el.click(); return true; }} return false;"
        ))
        .await?;
    if clicked.to_boolean() {
        Ok(())
    } else {
        Err(format!("{selector} not found"))
    }
}

/// A `mouseover` or `mouseout` on the element, from the isolated world.
async fn dom_event(reader: &Reader, selector: &str, kind: &str) -> Result<(), String> {
    let sent = reader
        .eval(&format!(
            "const el = document.querySelector('{selector}'); if (!el) return false; \
             el.dispatchEvent(new MouseEvent('{kind}', {{bubbles: true, cancelable: true, \
             relatedTarget: '{kind}' === 'mouseout' ? document.body : null}})); return true;"
        ))
        .await?;
    if sent.to_boolean() {
        Ok(())
    } else {
        Err(format!("{selector} not found"))
    }
}

/// The element's centre in the window's surface, scrolled into the window first.
async fn element_point(context: &Context, selector: &str) -> Result<(f64, f64), String> {
    let reader = context.reader();
    let view = reader.web_view().ok_or("no web view")?;
    let rect = reader
        .eval(&format!(
            "const el = document.querySelector('{selector}'); if (!el) return null; \
             const r = el.getBoundingClientRect(); \
             return [r.left + r.width / 2, r.top + r.height / 2];"
        ))
        .await?;
    if rect.is_null() {
        return Err(format!("{selector} not found"));
    }
    let x = rect
        .object_get_property_at_index(0)
        .map_or(0.0, |value| value.to_double());
    let y = rect
        .object_get_property_at_index(1)
        .map_or(0.0, |value| value.to_double());
    if let Some(column) = reader
        .widget()
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
        && let Some(point) =
            view.compute_point(&column, &gtk::graphene::Point::new(x as f32, y as f32))
    {
        let adjustment = column.vadjustment();
        let page = adjustment.page_size();
        let target = adjustment.value() + f64::from(point.y()) - page / 2.0;
        adjustment.set_value(target.max(0.0));
        support::frames(context.window.window(), 2).await?;
    }
    let (sx, sy) = support::surface_point(&view, x, y).ok_or("no surface point")?;
    let window = context.window.window();
    let inside =
        sx >= 0.0 && sy >= 0.0 && sx < f64::from(window.width()) && sy < f64::from(window.height());
    if inside {
        Ok((sx, sy))
    } else {
        Err(format!(
            "{selector} is outside the window at {sx:.0},{sy:.0}"
        ))
    }
}

/// Scrolls the element into the window and clicks its centre through Broadway.
async fn real_click(context: &Context, selector: &str) -> Result<(), String> {
    let (x, y) = element_point(context, selector).await?;
    context.input.click(x, y).map_err(|error| error.to_string())
}

/// Moves the pointer onto the element's centre through Broadway.
async fn real_hover(context: &Context, selector: &str) -> Result<(), String> {
    let (x, y) = element_point(context, selector).await?;
    context
        .input
        .motion(x - 1.0, y)
        .map_err(|error| error.to_string())?;
    support::sleep(50).await;
    context
        .input
        .motion(x, y)
        .map_err(|error| error.to_string())
}

/// Clicks the named widget's centre through Broadway.
async fn widget_click(context: &Context, name: &str) -> Result<(), String> {
    let widget = named(&context.reader().widget(), name).ok_or(format!("no {name}"))?;
    let (x, y) = support::surface_point(
        &widget,
        f64::from(widget.width()) / 2.0,
        f64::from(widget.height()) / 2.0,
    )
    .ok_or("no surface point")?;
    context.input.click(x, y).map_err(|error| error.to_string())
}

/// Where `needle` starts in the shown plain-text body, a little inside its first character,
/// in the window's surface.
fn text_point(context: &Context, needle: &str) -> Result<(f64, f64), String> {
    let view = named(&context.reader().widget(), "reader-plain")
        .and_downcast::<gtk::TextView>()
        .ok_or("no plain-text body")?;
    let buffer = view.buffer();
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    let at = text
        .find(needle)
        .ok_or(format!("{needle:?} isn't in the text"))?;
    let iter = buffer.iter_at_offset(text[..at].chars().count() as i32 + 2);
    let rect = view.iter_location(&iter);
    let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
    support::surface_point(
        &view,
        f64::from(x) + 2.0,
        f64::from(y) + f64::from(rect.height()) / 2.0,
    )
    .ok_or_else(|| "no surface point".to_owned())
}

/// The first widget named `name` under `root`.
fn named(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if widget.widget_name() == name {
            return Some(widget);
        }
        let mut child = widget.last_child();
        while let Some(next) = child {
            child = next.prev_sibling();
            stack.push(next);
        }
    }
    None
}

/// Every label's and text view's text under `root`.
fn all_text(root: &gtk::Widget) -> String {
    let mut text = String::new();
    let mut stack = vec![root.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            text.push_str(&label.text());
            text.push('\n');
        } else if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
            let buffer = view.buffer();
            text.push_str(&buffer.text(&buffer.start_iter(), &buffer.end_iter(), true));
            text.push('\n');
        }
        let mut child = widget.last_child();
        while let Some(next) = child {
            child = next.prev_sibling();
            stack.push(next);
        }
    }
    text
}

/// A bare WebView with WebKit's defaults, scripts on and no CSP, filter or interception, in
/// a window of its own: S2's no-defence control. Navigations go only to the canary,
/// `about:srcdoc`, `data:` and the case's own `about:blank` load.
async fn control_pass(context: &Context, cases: &[&Case]) -> Vec<Outcome> {
    let session = webkit6::NetworkSession::new_ephemeral();
    let downloads = Rc::new(Cell::new(0u32));
    session.connect_download_started(glib::clone!(
        #[strong]
        downloads,
        move |_, download| {
            download.cancel();
            downloads.set(downloads.get() + 1);
        }
    ));
    let web_context = webkit6::WebContext::new();
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_enable_javascript_markup(true);
    settings.set_javascript_can_open_windows_automatically(true);
    let view = webkit6::WebView::builder()
        .web_context(&web_context)
        .network_session(&session)
        .settings(&settings)
        .build();
    view.set_size_request(900, 700);
    let window = gtk::Window::builder()
        .title("hostile control")
        .default_width(960)
        .default_height(760)
        .child(&view)
        .build();
    window.present();
    let dialogs = Rc::new(Cell::new(0u32));
    let new_windows = Rc::new(Cell::new(0u32));
    let expecting = Rc::new(Cell::new(false));
    let origin = context.canary.origin();
    view.connect_script_dialog(glib::clone!(
        #[strong]
        dialogs,
        move |_, _| {
            dialogs.set(dialogs.get() + 1);
            true
        }
    ));
    view.connect_create(glib::clone!(
        #[strong]
        new_windows,
        move |_, _| {
            new_windows.set(new_windows.get() + 1);
            None
        }
    ));
    view.connect_permission_request(|_, request| {
        request.deny();
        true
    });
    view.connect_print(|_, _| true);
    view.connect_decide_policy(glib::clone!(
        #[strong]
        expecting,
        move |_, decision, kind| {
            let uri = decision
                .downcast_ref::<webkit6::NavigationPolicyDecision>()
                .and_then(|decision| decision.navigation_action())
                .and_then(|action| action.request())
                .and_then(|request| request.uri())
                .map(String::from)
                .unwrap_or_default();
            let allowed = match kind {
                webkit6::PolicyDecisionType::Response => true,
                _ if expecting.get() && uri == "about:blank" => {
                    expecting.set(false);
                    true
                }
                _ => [origin.as_str(), "about:srcdoc", "data:"]
                    .iter()
                    .any(|prefix| uri.starts_with(prefix)),
            };
            if allowed {
                decision.use_();
            } else {
                decision.ignore();
            }
            true
        }
    ));
    let mut outcomes = Vec::new();
    for case in cases {
        let before = context.canary.snapshot();
        let dns_before = context.dns_snapshot();
        let (dialogs_before, windows_before, downloads_before) =
            (dialogs.get(), new_windows.get(), downloads.get());
        let mut outcome = Outcome::default();
        let (sender, finished) = async_channel::unbounded::<()>();
        let handler = view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                let _ = sender.try_send(());
            }
        });
        expecting.set(true);
        view.load_html(&case.html, None);
        let _ = glib::future_with_timeout(Duration::from_secs(3), finished.recv()).await;
        view.disconnect(handler);
        support::sleep(300).await;
        if let Some(selector) = case.click {
            let script = format!(
                "const el = document.querySelector('{selector}'); \
                 if (el) {{ el.click(); return true; }} return false;"
            );
            let _ = view
                .call_async_javascript_function_future(&script, None, Some("probe"), None)
                .await;
            support::sleep(700).await;
        }
        outcome.title = view.title().map(String::from).unwrap_or_default();
        if let Ok(value) = view
            .call_async_javascript_function_future(PROBE_JS, None, Some("probe"), None)
            .await
        {
            outcome.pwned = value
                .object_get_property("pwned")
                .map(|value| value.to_str().to_string())
                .unwrap_or_default();
        }
        support::sleep(150).await;
        outcome.dialogs = dialogs.get() - dialogs_before;
        if new_windows.get() > windows_before {
            outcome
                .refused
                .insert("NewWindow".into(), new_windows.get() - windows_before);
        }
        outcome.downloads = downloads.get() - downloads_before;
        outcome.log = context.canary.snapshot().since(&before);
        let origin = context.canary.origin();
        outcome.log.requests = outcome
            .log
            .requests
            .iter()
            .map(|request| request.replace(&origin, "{canary}"))
            .collect();
        outcome.dns = context.dns_since(dns_before);
        outcomes.push(outcome);
    }
    window.destroy();
    outcomes
}
