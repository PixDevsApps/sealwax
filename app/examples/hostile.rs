//! S2's hostile-HTML suite (47 cases) against the real reader (ML-027; ML-065 puts it in CI in
//! M5). Each case is a stored message, shown by `Reader::show` through the store, the blob
//! store and the pooled, hardened view, while a canary on 127.0.0.1 counts connections,
//! requests and TLS ClientHellos and the probes look for script traces (title, DOM marker,
//! dialogs, beacon). Run it through the wrapper:
//!
//! ```sh
//! SEALWAX_HEADLESS_TIMEOUT=900 tools/headless.sh 37 target/debug/examples/hostile
//! ```
//!
//! Five passes, one row per case: `prod` (the reader as shipped; clicks are DOM `click()`
//! from the isolated world, as in S2), `real` (click cases only: the click arrives as real
//! pointer input through Broadway), `shown` (after "Show images": only https images may load,
//! upgraded from http, so the canary sees TLS hellos and never a request), `again` (`prod` in a
//! second reader that reuses the pooled views `shown` lifted the filter from) and `ctrl` (a
//! bare WebView with no defences, scripts on, proving the vectors are live). Exits 0 when
//! `prod`, `real` and `again` are clean in every case, both controls saw traffic and GLib wrote
//! no critical. Links are recorded, never opened; every clicked link points at the canary.

#[path = "hostile_support/canary.rs"]
mod canary;
#[path = "hostile_support/cases.rs"]
mod cases;
#[path = "reader_support/mod.rs"]
mod support;

use canary::{Canary, Log};
use cases::{Case, FOREIGN_CID, OK_CID};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::reader::samples::{self, Draft, Part};
use sealwax_app::reader::{Reader, Refused, Shown, ViewEvent};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::ThemeOptions;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;
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

#[derive(Debug, Default, Clone)]
struct Outcome {
    log: Log,
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
    failures: Vec<String>,
}

impl Outcome {
    fn script(&self) -> bool {
        self.title.contains("pwned") || !self.pwned.is_empty() || self.dialogs > 0
    }

    fn network(&self) -> bool {
        !self.log.is_quiet()
    }

    /// S2's production criteria.
    fn clean(&self) -> bool {
        !self.network()
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
    }

    fn mark(&self) -> String {
        let mut mark = String::new();
        if self.script() {
            mark.push('S');
        }
        mark.push_str(&self.log.mark());
        if mark.is_empty() { ".".into() } else { mark }
    }
}

#[derive(Default)]
struct Recorder {
    events: Vec<(MessageId, ViewEvent)>,
    opened: Vec<String>,
}

fn messages(cases: &[Case], now: i64) -> Vec<(&'static str, Vec<u8>)> {
    let ok = glib::base64_decode(samples::LOGO_PNG_BASE64);
    let mut messages: Vec<(&'static str, Vec<u8>)> = cases
        .iter()
        .map(|case| {
            let mut draft = Draft::new(
                ("Hostile Suite", "hostile@example.test"),
                case.id,
                now - 60,
                &format!("hostile-{}", case.id),
            );
            draft.html = Some(case.html.clone());
            draft.inline = vec![Part::inline(OK_CID, "image/png", ok.clone())];
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
    let cases: Vec<Case> = cases::cases(&canary.origin())
        .into_iter()
        .filter(|case| {
            wanted
                .as_ref()
                .is_none_or(|wanted| wanted.iter().any(|id| id == case.id))
        })
        .collect();
    println!(
        "ENV cases={} canary=127.0.0.1:{} webkit={}.{}.{} gtk={}.{}.{}",
        cases.len(),
        canary.port(),
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
            glib::spawn_future_local(async move {
                let _keep = &theme;
                let context = Context {
                    readers,
                    current: Cell::new(0),
                    window,
                    canary,
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
    recorder: Rc<RefCell<Recorder>>,
    input: Input,
}

impl Context {
    fn reader(&self) -> &Reader {
        &self.readers[self.current.get()]
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
    for pass in [
        Pass::Production,
        Pass::RealClicks,
        Pass::ImagesShown,
        Pass::Again,
    ] {
        if pass == Pass::Again {
            context.current.set(1);
            context.window.set_reader(&context.reader().widget());
            context
                .window
                .set_reader_toolbar(&context.reader().toolbar());
        }
        let mut network = 0;
        let mut scripts = 0;
        for case in cases {
            if pass == Pass::RealClicks && case.click.is_none() {
                continue;
            }
            let Some(id) = filled.id(case.id) else {
                println!("ERROR {} was not stored", case.id);
                return false;
            };
            let outcome = run_case(context, &reports[context.current.get()], case, id, pass).await;
            network += usize::from(outcome.network());
            scripts += usize::from(outcome.script());
            print_row(pass.name(), case, &outcome);
            rows.entry(case.id)
                .or_default()
                .insert(pass.name(), outcome.mark());
            if pass != Pass::ImagesShown && !outcome.clean() {
                failures.push(format!("{}/{}", pass.name(), case.id));
            }
        }
        println!(
            "SUMMARY pass={} with_network={network} with_script={scripts}",
            pass.name()
        );
        traffic.insert(pass.name(), network);
    }
    let control = control_pass(context, cases).await;
    let mut control_network = 0;
    let mut control_scripts = 0;
    for (case, outcome) in cases.iter().zip(&control) {
        control_network += usize::from(outcome.network());
        control_scripts += usize::from(outcome.script());
        print_row("ctrl", case, outcome);
        rows.entry(case.id)
            .or_default()
            .insert("ctrl", outcome.mark());
    }
    println!("SUMMARY pass=ctrl with_network={control_network} with_script={control_scripts}");
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
    let criticals = support::criticals();
    println!(
        "SUMMARY production_failures={failures:?} cases={} glib_criticals={criticals}",
        cases.len()
    );
    failures.is_empty() && !blind && criticals == 0
}

fn print_row(pass: &str, case: &Case, outcome: &Outcome) {
    println!(
        "ROW pass={pass} case={} network={} connections={} silent={} tls={} requests={:?} \
         script={} title={:?} dom={:?} dialogs={} refused={:?} links={:?} opened={:?} \
         cid_ok={:?} foreign_loaded={} heights={} downloads={} url={:?} resources={:?} \
         failures={:?}",
        case.id,
        outcome.network(),
        outcome.log.connections,
        outcome.log.silent,
        outcome.log.tls_hellos,
        outcome.log.requests,
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
    let downloads = reader.refused_downloads();
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
            if !matches!(shown.body, sealwax_app::reader::ShownBody::Html { .. }) {
                outcome
                    .failures
                    .push(format!("shown as {:?}, not HTML", shown.body));
            }
        }
        Err(error) => outcome.failures.push(error),
    }
    support::sleep(300).await;
    if let Some(selector) = case.click {
        let clicked = match pass {
            Pass::RealClicks => real_click(context, selector).await,
            _ => dom_click(reader, selector).await,
        };
        if let Err(error) = clicked {
            outcome.failures.push(format!("click: {error}"));
        }
        support::sleep(700).await;
    }
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
    support::sleep(150).await;
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
            ViewEvent::Resource(uri) if !uri.starts_with("about:") => {
                outcome.resources.push(short(uri));
            }
            _ => {}
        }
    }
    outcome.opened = recorder.opened.iter().map(|uri| short(uri)).collect();
    outcome.downloads = reader.refused_downloads() - downloads;
    outcome.log = context.canary.snapshot().since(&before);
    outcome.log.requests = outcome.log.requests.iter().map(|r| short(r)).collect();
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

/// Scrolls the element into the window and clicks its centre through Broadway.
async fn real_click(context: &Context, selector: &str) -> Result<(), String> {
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
    if !inside {
        return Err(format!(
            "{selector} is outside the window at {sx:.0},{sy:.0}"
        ));
    }
    context
        .input
        .click(sx, sy)
        .map_err(|error| error.to_string())
}

/// A bare WebView with WebKit's defaults, scripts on and no CSP, filter or interception, in
/// a window of its own: S2's no-defence control. Navigations go only to the canary,
/// `about:srcdoc`, `data:` and the case's own `about:blank` load.
async fn control_pass(context: &Context, cases: &[Case]) -> Vec<Outcome> {
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
        outcomes.push(outcome);
    }
    window.destroy();
    outcomes
}
