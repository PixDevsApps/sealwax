//! The reader on real-world HTML (M5, ML-059; S2's bars on the real corpus): every template of
//! `tests/corpus/real/SOURCES.toml` (fetched by `tools/corpus/fetch.py` into
//! `.local/corpus/real/`), S2's synthetic corpus and a few personal-style replies, shown
//! through the real reader in a dark (matte-black) and a light (catppuccin-latte) theme. Per
//! message it records the heuristic's choice and why, the height against the document (±1 px)
//! and against a re-measure, blank frames (WebKit's snapshot and GTK's render of the view),
//! horizontal overflow, whether a dark colour scheme reaches the page, and the open time; then
//! it scrolls the tallest bodies programmatically and records the frame intervals. Every
//! remote URL in a template is pointed at a local canary, which must see nothing: images stay
//! hidden behind the content filter.
//!
//! Headless (`tools/headless.sh`) or, for the lead's live run, on a session: it needs only a
//! scratch Omarchy state, makes no input, writes only under `.local/`, and ends by itself.
//!
//! ```sh
//! python3 tools/corpus/fetch.py
//! SEALWAX_OMARCHY_STATE=$PWD/.local/reader-examples/corpus-theme SEALWAX_HEADLESS_TIMEOUT=900 \
//!     tools/headless.sh 33 target/release/examples/reader_corpus [--previews] [--only=ID,…]
//! ```
//!
//! `--previews` saves the gate's screenshots of a few templates in both themes into
//! `tools/progress/public/`. Exits 0 when no body is blank, every height is exact and stable,
//! the canary saw nothing and GLib wrote no critical; times and overflow are recorded, not
//! judged.

#[path = "hostile_support/canary.rs"]
#[allow(dead_code)]
mod canary;
#[path = "../../spikes/s2-webkit/src/corpus.rs"]
#[allow(dead_code)]
mod corpus;
#[path = "../../spikes/s2-webkit/src/png.rs"]
#[allow(dead_code)]
mod png;
#[path = "reader_support/mod.rs"]
mod support;

use canary::Canary;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use sealwax_app::reader::samples::{self, Draft, Part};
use sealwax_app::reader::{Reader, Shown, ShownBody, scan_html};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::theme::{BodyStyle, ThemeOptions};
use sealwax_infrastructure::omarchy::STATE_DIR_ENV;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::prelude::*;

const MARKER: &str = ".sealwax-reader-corpus";
const THEMES: [(&str, &str); 2] = [("dark", "matte-black"), ("light", "catppuccin-latte")];
/// Templates whose screenshots go to the gate with `--previews`.
const PREVIEWS: [&str; 6] = [
    "postmark-basic-receipt",
    "cerberus-hybrid",
    "sendwithus-airmail-invoice",
    "mailgun-action-inlined",
    "s2-nl-31",
    "personal-gmail-reply",
];
/// Bodies scrolled per theme, the tallest first, and the scroll speed (S2's).
const SCROLLED: usize = 3;
const SCROLL_SPEED: f64 = 2400.0;

#[derive(Clone)]
struct Message {
    id: String,
    group: &'static str,
    kind: String,
    html: Option<String>,
    raw: Vec<u8>,
}

#[derive(Debug, Default, Clone)]
struct Row {
    id: String,
    group: &'static str,
    kind: String,
    theme: &'static str,
    style: String,
    evidence: String,
    width: i32,
    height: i32,
    document: i32,
    height_ok: bool,
    stable: bool,
    scroll_width: i32,
    overflow: bool,
    blank_webkit: Option<bool>,
    blank_gtk: Option<bool>,
    dark_query: bool,
    declares_dark: bool,
    body_background: String,
    ready_ms: f64,
    painted_ms: f64,
    failures: Vec<String>,
}

fn state_path() -> PathBuf {
    std::env::var_os(STATE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The scratch state folder: absolute, never Omarchy's own, and missing, empty or this
/// check's (as `reader_theme.rs`).
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

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// As `omarchy-theme-set`: stage `next-theme/`, remove `theme/`, rename, write `theme.name`.
fn install_theme(state: &Path, name: &str) -> std::io::Result<()> {
    let fixture = |file: &str| repository().join("tests/theme").join(file);
    let current = state.join("current");
    let next = current.join("next-theme");
    let _ = fs::remove_dir_all(&next);
    fs::create_dir_all(&next)?;
    fs::copy(
        fixture(&format!("{name}/colors.toml")),
        next.join("colors.toml"),
    )?;
    fs::copy(fixture("shell.toml"), next.join("shell.toml"))?;
    let theme = current.join("theme");
    if theme.exists() {
        fs::remove_dir_all(&theme)?;
    }
    fs::rename(&next, &theme)?;
    fs::write(current.join("theme.name"), format!("{name}\n"))
}

/// The templates the manifest lists that `fetch.py` fetched: id, kind and file.
fn real_templates() -> Vec<(String, String, PathBuf)> {
    let manifest =
        fs::read_to_string(repository().join("tests/corpus/real/SOURCES.toml")).unwrap_or_default();
    let mut found = Vec::new();
    let mut id = None;
    for line in manifest.lines() {
        let value = |line: &str| {
            line.split_once('=')
                .map(|(_, value)| value.trim().trim_matches('"').to_owned())
        };
        if line.starts_with("id = ") {
            id = value(line);
        } else if line.starts_with("kind = ")
            && let Some(id) = id.take()
        {
            let kind = value(line)
                .unwrap_or_default()
                .trim_matches(['[', ']'])
                .replace(['"', ' '], "");
            let path = repository()
                .join(".local/corpus/real")
                .join(format!("{id}.html"));
            found.push((id, kind, path));
        }
    }
    found
}

/// Points every http(s) URL at the canary (`/r/<host>/<path>`), except a doctype's and
/// namespaces, so a remote load that got through would reach it instead of the internet.
fn to_canary(html: &str, origin: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let doctype_end = lower
        .find("<!doctype")
        .and_then(|start| lower[start..].find('>').map(|end| start + end))
        .unwrap_or(0);
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    while let Some(offset) = lower[at..].find("http") {
        let start = at + offset;
        let rest = &lower[start..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            out.push_str(&html[at..start + 4]);
            at = start + 4;
            continue;
        };
        let namespace = lower[start.saturating_sub(16)..start].contains("xmlns");
        out.push_str(&html[at..start]);
        if start < doctype_end || namespace {
            out.push_str(&html[start..start + scheme]);
        } else {
            out.push_str(origin);
            out.push_str("/r/");
        }
        at = start + scheme;
    }
    out.push_str(&html[at..]);
    out
}

/// Replies as Outlook, Gmail, Apple Mail and Thunderbird write them (the markup each puts
/// around a short reply; every name and address fictional), for the themed side of the
/// heuristic, which the templates don't exercise.
fn personal() -> Vec<(&'static str, String)> {
    vec![
        (
            "personal-gmail-reply",
            "<div dir=\"ltr\">Thursday works. I'll bring the printouts.<div><br></div>\
             <div>Marius</div></div><br><div class=\"gmail_quote gmail_quote_container\">\
             <div dir=\"ltr\" class=\"gmail_attr\">On Mon, Oct 5, 2026 at 10:02 AM Leah Kim \
             &lt;<a href=\"mailto:leah@example.test\">leah@example.test</a>&gt; wrote:<br></div>\
             <blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px \
             solid rgb(204,204,204);padding-left:1ex\"><div dir=\"ltr\">Can we review the \
             draft on Thursday?</div></blockquote></div>"
                .into(),
        ),
        (
            "personal-apple-mail",
            "<html><head><meta http-equiv=\"content-type\" content=\"text/html; charset=utf-8\">\
             </head><body style=\"overflow-wrap: break-word; -webkit-nbsp-mode: space; \
             line-break: after-white-space;\">Sounds good, see you at noon.<div><br></div>\
             <div>Tomas<br id=\"lineBreakAtBeginningOfSignature\"><div dir=\"ltr\">Sent from \
             my phone</div><div dir=\"ltr\"><br><blockquote type=\"cite\">On 5 Oct 2026, at \
             10:00, Ingrid &lt;ingrid@example.test&gt; wrote:<br><br></blockquote></div>\
             <blockquote type=\"cite\"><div dir=\"ltr\">Lunch Thursday?</div></blockquote>\
             </div></body></html>"
                .into(),
        ),
        (
            "personal-thunderbird",
            "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">\
             </head><body><p>Yes, the cabin has bedding.</p><div class=\"moz-cite-prefix\">On \
             05.10.2026 10:00, Sana wrote:<br></div><blockquote type=\"cite\" \
             cite=\"mid:sample@example.test\">Do we need to bring bedding?</blockquote><br>\
             </body></html>"
                .into(),
        ),
        (
            "personal-outlook-desktop",
            "<html xmlns:v=\"urn:schemas-microsoft-com:vml\" xmlns:o=\"urn:schemas-microsoft-com:office:office\" \
             xmlns:w=\"urn:schemas-microsoft-com:office:word\" xmlns=\"http://www.w3.org/TR/REC-html40\">\
             <head><meta http-equiv=Content-Type content=\"text/html; charset=utf-8\"><meta \
             name=Generator content=\"Microsoft Word 15 (filtered medium)\"><style><!--\n\
             @font-face {font-family:Calibri; panose-1:2 15 5 2 2 2 4 3 2 4;}\n\
             p.MsoNormal, li.MsoNormal, div.MsoNormal {margin:0cm; font-size:11.0pt; \
             font-family:\"Calibri\",sans-serif;}\n\
             a:link, span.MsoHyperlink {mso-style-priority:99; color:#0563C1; \
             text-decoration:underline;}\n\
             span.EmailStyle17 {mso-style-type:personal-compose; font-family:\"Calibri\",sans-serif; \
             color:windowtext;}\n\
             .MsoChpDefault {mso-style-type:export-only; font-size:10.0pt;}\n\
             div.WordSection1 {page:WordSection1;}\n--></style></head>\
             <body lang=EN-US link=\"#0563C1\" vlink=\"#954F72\" style='word-wrap:break-word'>\
             <div class=WordSection1><p class=MsoNormal>Hi Leah,<o:p></o:p></p><p \
             class=MsoNormal><o:p>&nbsp;</o:p></p><p class=MsoNormal>The figures are on \
             <a href=\"https://example.test/q3\">the shared drive</a>.<o:p></o:p></p><p \
             class=MsoNormal><o:p>&nbsp;</o:p></p><p class=MsoNormal>Marius<o:p></o:p></p><p \
             class=MsoNormal><o:p>&nbsp;</o:p></p><div style='border:none;border-top:solid \
             #E1E1E1 1.0pt;padding:3.0pt 0cm 0cm 0cm'><p class=MsoNormal><b>From:</b> Leah Kim \
             &lt;leah@example.test&gt; <br><b>Sent:</b> Monday, October 5, 2026 10:00<br>\
             <b>To:</b> Marius Dahl &lt;marius@example.test&gt;<br><b>Subject:</b> Q3 \
             figures<o:p></o:p></p></div><p class=MsoNormal><o:p>&nbsp;</o:p></p><p \
             class=MsoNormal>Where are the Q3 figures?<o:p></o:p></p></div></body></html>"
                .into(),
        ),
        (
            "personal-outlook-web",
            "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">\
             <style type=\"text/css\" style=\"display:none;\"> P {margin-top:0;margin-bottom:0;} \
             </style></head><body dir=\"ltr\"><div class=\"elementToProof\" style=\"font-family: \
             Aptos, Aptos_EmbeddedFont, Aptos_MSFontService, Calibri, Helvetica, sans-serif; \
             font-size: 12pt; color: rgb(0, 0, 0);\">Thanks, booked.</div><div \
             id=\"appendonsend\"></div><hr style=\"display:inline-block;width:98%\" \
             tabindex=\"-1\"><div id=\"divRplyFwdMsg\" dir=\"ltr\"><font face=\"Calibri, \
             sans-serif\" style=\"font-size:11pt\" color=\"#000000\"><b>From:</b> Kari \
             &lt;kari@example.test&gt;<br><b>Sent:</b> Monday, October 5, 2026 10:00<br>\
             <b>Subject:</b> Room</font><div>&nbsp;</div></div><div>Can you book the \
             room?</div></body></html>"
                .into(),
        ),
        (
            "personal-plain-html",
            "<p>Hi,</p><p>The agenda for Thursday:</p><ol><li>Budget</li><li>Hiring</li></ol>\
             <p>Ada</p>"
                .into(),
        ),
    ]
}

fn messages(origin: &str, now: i64) -> Vec<Message> {
    let mut messages = Vec::new();
    let draft = |id: &str, subject: &str| {
        Draft::new(
            ("Corpus Sender", "corpus@example.test"),
            subject,
            now - 60,
            &format!("corpus-{id}"),
        )
    };
    for (id, kind, path) in real_templates() {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let html = to_canary(&String::from_utf8_lossy(&bytes), origin);
        let mut draft = draft(&id, &id);
        draft.html = Some(html.clone());
        messages.push(Message {
            id,
            group: "real",
            kind,
            html: Some(html),
            raw: draft.render(),
        });
    }
    let font = fs::read("/usr/share/fonts/liberation/LiberationSerif-Regular.ttf").ok();
    let assets = corpus::Assets::generate(font);
    let s2 = corpus::newsletters(origin, &assets)
        .into_iter()
        .chain(corpus::simple_html())
        .chain(corpus::plain_text());
    for message in s2 {
        let id = format!("s2-{}", message.id);
        let mut draft = draft(&id, &message.subject);
        let html = (message.kind != corpus::Kind::PlainText).then(|| message.body.clone());
        match &html {
            Some(html) => draft.html = Some(html.clone()),
            None => draft.text = Some(message.body.clone()),
        }
        draft.inline = message
            .parts
            .iter()
            .map(|part| Part::inline(&part.cid, part.content_type, part.data.to_vec()))
            .collect();
        messages.push(Message {
            id,
            group: "s2",
            kind: format!("{:?}", message.kind).to_lowercase(),
            html,
            raw: draft.render(),
        });
    }
    for (id, html) in personal() {
        let mut draft = draft(id, id);
        draft.html = Some(html.clone());
        messages.push(Message {
            id: id.into(),
            group: "personal",
            kind: "reply".into(),
            html: Some(html),
            raw: draft.render(),
        });
    }
    messages
}

fn main() -> glib::ExitCode {
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let previews = args.iter().any(|arg| arg == "--previews");
    let only: Option<Vec<String>> = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--only="))
        .map(|list| list.split(',').map(str::to_owned).collect());
    let state = match scratch_state() {
        Ok(state) => state,
        Err(message) => {
            eprintln!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };
    if let Err(error) = install_theme(&state, THEMES[0].1) {
        eprintln!("could not prepare the scratch theme: {error}");
        return glib::ExitCode::FAILURE;
    }
    let _input =
        std::env::var_os("BROADWAY_DISPLAY").and_then(|_| support::broadway::connect().ok());
    let canary = match Canary::start() {
        Ok(canary) => Rc::new(canary),
        Err(error) => {
            eprintln!("could not start the canary: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let now = glib::real_time() / 1_000_000;
    let messages: Vec<Message> = messages(&canary.origin(), now)
        .into_iter()
        .filter(|message| only.as_ref().is_none_or(|only| only.contains(&message.id)))
        .collect();
    let groups = |group: &str| messages.iter().filter(|m| m.group == group).count();
    println!(
        "ENV messages={} real={} s2={} personal={} canary=127.0.0.1:{} backend={} webkit={}.{}.{}",
        messages.len(),
        groups("real"),
        groups("s2"),
        groups("personal"),
        canary.port(),
        if std::env::var_os("BROADWAY_DISPLAY").is_some() {
            "broadway"
        } else {
            "session"
        },
        webkit6::functions::major_version(),
        webkit6::functions::minor_version(),
        webkit6::functions::micro_version(),
    );
    let root = support::scratch("corpus");
    let scratch = support::store(&root);
    let raws: Vec<(&'static str, Vec<u8>)> = messages
        .iter()
        .map(|message| {
            let id: &'static str = Box::leak(message.id.clone().into_boxed_str());
            (id, message.raw.clone())
        })
        .collect();
    let filled = samples::fill_store(&scratch.store, &scratch.blobs, raws)
        .expect("the corpus goes into the scratch store");
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ReaderCorpus")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let scratch = Rc::new(scratch);
    let messages = Rc::new(messages);
    let started = Instant::now();
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let reader = support::reader(&scratch, &theme);
            let window = support::window(app, &reader, (1600, 1000));
            window.present();
            let app = app.clone();
            let passed = passed.clone();
            let canary = canary.clone();
            let messages = messages.clone();
            let filled = filled.clone();
            let state = state.clone();
            glib::spawn_future_local(async move {
                let ok = run(
                    &theme, &reader, &window, &state, &messages, &filled, &canary, previews,
                )
                .await
                .unwrap_or_else(|error| {
                    println!("ERROR {error}");
                    false
                });
                passed.set(ok);
                app.quit();
            });
        }
    ));
    app.run_with_args::<&str>(&[]);
    println!("TIME {:.1} s", started.elapsed().as_secs_f64());
    if passed.get() {
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    theme: &Rc<ThemeController>,
    reader: &Reader,
    window: &sealwax_app::window::Window,
    state: &Path,
    messages: &[Message],
    filled: &samples::Filled,
    canary: &Canary,
    previews: bool,
) -> Result<bool, String> {
    let reports = support::shown_reports(reader);
    support::resize(window.window(), 1600, 1000).await?;
    let mut rows: Vec<Row> = Vec::new();
    let mut scrolls: Vec<(String, &'static str, Vec<f64>)> = Vec::new();
    for (mode, name) in THEMES {
        install_theme(state, name).map_err(|error| error.to_string())?;
        let applied =
            support::wait_until(Duration::from_secs(10), || theme.theme().name() == name).await;
        if !applied {
            return Err(format!("the theme never became {name}"));
        }
        support::frames(window.window(), 2).await?;
        println!("THEME {mode} {name}");
        let theme_rows_start = rows.len();
        for message in messages {
            let id = filled
                .id(&message.id)
                .ok_or_else(|| format!("{} wasn't stored", message.id))?;
            let row = measure(reader, &reports, window, message, id, mode).await;
            print_row(&row);
            if previews && PREVIEWS.contains(&message.id.as_str()) {
                let date = glib::DateTime::now_local()
                    .and_then(|now| now.format("%Y-%m-%d"))
                    .map_err(|error| error.to_string())?;
                let path = repository().join(format!(
                    "tools/progress/public/m5-corpus-{}-{mode}-{date}.png",
                    message.id
                ));
                support::screenshot(window.window(), &path)?;
                println!("PREVIEW {}", path.display());
            }
            rows.push(row);
        }
        let mut tallest: Vec<&Row> = rows[theme_rows_start..]
            .iter()
            .filter(|row| row.height > 0)
            .collect();
        tallest.sort_by_key(|row| std::cmp::Reverse(row.height));
        for row in tallest.into_iter().take(SCROLLED) {
            let id = filled.id(&row.id).ok_or("a scrolled message vanished")?;
            support::show(reader, &reports, id).await?;
            support::frames(window.window(), 2).await?;
            let intervals = scroll(reader, window).await?;
            println!(
                "SCROLL theme={mode} id={} height={} frames={} p50={:.2} p95={:.2} max={:.2} \
                 over_16_7={}",
                row.id,
                row.height,
                intervals.len(),
                percentile(&intervals, 50.0),
                percentile(&intervals, 95.0),
                intervals.iter().copied().fold(0.0, f64::max),
                intervals.iter().filter(|ms| **ms > 16.7).count()
            );
            scrolls.push((row.id.clone(), mode, intervals));
        }
    }
    write_results(&rows)?;
    let html_rows: Vec<&Row> = rows.iter().filter(|row| row.width > 0).collect();
    let count = |check: &dyn Fn(&Row) -> bool| html_rows.iter().filter(|row| check(row)).count();
    let mut ready: Vec<f64> = html_rows.iter().map(|row| row.ready_ms).collect();
    let mut painted: Vec<f64> = html_rows.iter().map(|row| row.painted_ms).collect();
    ready.sort_by(f64::total_cmp);
    painted.sort_by(f64::total_cmp);
    let mut frames: Vec<f64> = scrolls
        .iter()
        .flat_map(|(_, _, f)| f.iter().copied())
        .collect();
    frames.sort_by(f64::total_cmp);
    let total = canary.snapshot();
    let criticals = support::criticals();
    let failures: Vec<String> = rows
        .iter()
        .filter(|row| !row.failures.is_empty())
        .map(|row| format!("{}/{}: {}", row.theme, row.id, row.failures.join("; ")))
        .collect();
    for failure in &failures {
        println!("FAIL {failure}");
    }
    let ok = failures.is_empty() && total.is_quiet() && criticals == 0;
    println!(
        "SUMMARY messages={} rows={} html={} paper={} themed={} heights_ok={}/{} stable={}/{} \
         blank_webkit={} blank_gtk={} overflow={} dark_query={} open_ready_p50={:.1} \
         open_ready_p95={:.1} open_painted_p50={:.1} open_painted_p95={:.1} scroll_frames={} \
         scroll_p95={:.2} canary_connections={} canary_tls={} criticals={criticals} {}",
        messages.len(),
        rows.len(),
        html_rows.len(),
        count(&|row| row.style == "paper"),
        count(&|row| row.style == "themed"),
        count(&|row| row.height_ok),
        html_rows.len(),
        count(&|row| row.stable),
        html_rows.len(),
        count(&|row| row.blank_webkit == Some(true)),
        count(&|row| row.blank_gtk == Some(true)),
        count(&|row| row.overflow),
        count(&|row| row.dark_query),
        percentile(&ready, 50.0),
        percentile(&ready, 95.0),
        percentile(&painted, 50.0),
        percentile(&painted, 95.0),
        frames.len(),
        percentile(&frames, 95.0),
        total.connections,
        total.tls_hellos,
        if ok { "PASS" } else { "FAIL" }
    );
    Ok(ok)
}

const PROBE_JS: &str = r#"
const root = document.documentElement;
const body = document.body;
const declared = (document.querySelector('meta[name="color-scheme" i], meta[name="supported-color-schemes" i]') || {}).content || '';
const styles = [...document.querySelectorAll('style')].map(s => s.textContent).join(' ');
return [
  root.scrollHeight, window.innerHeight, window.innerWidth, root.scrollWidth,
  matchMedia('(prefers-color-scheme: dark)').matches,
  /dark/i.test(declared) || /prefers-color-scheme\s*:\s*dark/i.test(styles),
  body ? getComputedStyle(body).backgroundColor : '',
];
"#;

async fn measure(
    reader: &Reader,
    reports: &async_channel::Receiver<Shown>,
    window: &sealwax_app::window::Window,
    message: &Message,
    id: sealwax_domain::model::MessageId,
    theme: &'static str,
) -> Row {
    let mut row = Row {
        id: message.id.clone(),
        group: message.group,
        kind: message.kind.clone(),
        theme,
        ..Row::default()
    };
    if let Some(html) = &message.html {
        let scan = scan_html(html);
        row.evidence = scan
            .evidence
            .map_or("none", |evidence| evidence.as_str())
            .to_owned();
    }
    let started = Instant::now();
    let shown = match support::show(reader, reports, id).await {
        Ok(shown) => shown,
        Err(error) => {
            row.failures.push(error);
            return row;
        }
    };
    row.ready_ms = shown.ready.as_secs_f64() * 1000.0;
    if support::frames(window.window(), 1).await.is_ok() {
        row.painted_ms = started.elapsed().as_secs_f64() * 1000.0;
    }
    row.style = match &shown.body {
        ShownBody::Html {
            style: BodyStyle::Paper,
            ..
        } => "paper".into(),
        ShownBody::Html { .. } => "themed".into(),
        ShownBody::Plain => "plain".into(),
        other => format!("{other:?}"),
    };
    if message.html.is_some() != matches!(shown.body, ShownBody::Html { .. }) {
        row.failures.push(format!("shown as {:?}", shown.body));
        return row;
    }
    let Some(view) = reader.web_view() else {
        return row;
    };
    settle(reader, window).await;
    let first = match probe(reader).await {
        Ok(first) => first,
        Err(error) => {
            row.failures.push(error);
            return row;
        }
    };
    let (height, width) = reader.state().view_size.unwrap_or((0, 0));
    row.width = width;
    row.height = height;
    row.document = first.document;
    row.scroll_width = first.scroll_width;
    row.overflow = first.scroll_width > first.inner_width + 1;
    row.dark_query = first.dark;
    row.declares_dark = first.declares_dark;
    row.body_background = first.body_background.clone();
    let capped = height >= sealwax_app::reader::MAX_VIEW_HEIGHT;
    row.height_ok = (capped
        || ((first.document - height).abs() <= 1 && first.document <= first.inner_height + 1))
        && view.height() == height
        && first.inner_width == width;
    support::sleep(400).await;
    let _ = support::frames(window.window(), 2).await;
    match probe(reader).await {
        Ok(again) => {
            let (later, _) = reader.state().view_size.unwrap_or((0, 0));
            row.stable =
                (again.document - first.document).abs() <= 1 && (later - height).abs() <= 1;
        }
        Err(error) => row.failures.push(error),
    }
    if !row.height_ok {
        row.failures.push(format!(
            "height {height} for a {}-px document in a {}×{} viewport (allocated {})",
            first.document,
            first.inner_width,
            first.inner_height,
            view.height()
        ));
    }
    if !row.stable {
        row.failures.push("the height moved on a re-measure".into());
    }
    row.blank_webkit = match view
        .snapshot_future(
            webkit6::SnapshotRegion::Visible,
            webkit6::SnapshotOptions::NONE,
        )
        .await
    {
        Ok(texture) => Some(uniform(&texture)),
        Err(error) => {
            row.failures.push(format!("WebKit snapshot: {error}"));
            None
        }
    };
    row.blank_gtk = rendered(&view).map(|texture| uniform(&texture));
    if row.blank_webkit == Some(true) || row.blank_gtk == Some(true) {
        row.failures.push("a blank frame".into());
    }
    row
}

/// Waits until the view's height and WebKit's viewport have stayed put for 300 ms (content
/// that converges, such as a `min-height: 100vh` hero, takes a few reports), at most 3 s, as
/// S2 measured after the chain of height reports.
async fn settle(reader: &Reader, window: &sealwax_app::window::Window) {
    let started = Instant::now();
    let mut last = (reader.state().view_size, -1);
    let mut since = Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        let _ = support::frames(window.window(), 1).await;
        support::sleep(50).await;
        let inner = reader
            .eval("return window.innerHeight;")
            .await
            .map_or(-1, |value| value.to_int32());
        let now = (reader.state().view_size, inner);
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= Duration::from_millis(300) {
            return;
        }
    }
}

struct Probe {
    document: i32,
    inner_height: i32,
    inner_width: i32,
    scroll_width: i32,
    dark: bool,
    declares_dark: bool,
    body_background: String,
}

async fn probe(reader: &Reader) -> Result<Probe, String> {
    let value = reader.eval(PROBE_JS).await?;
    let at = |index: u32| value.object_get_property_at_index(index);
    let number = |index: u32| at(index).map_or(-1, |value| value.to_int32());
    let boolean = |index: u32| at(index).is_some_and(|value| value.to_boolean());
    Ok(Probe {
        document: number(0),
        inner_height: number(1),
        inner_width: number(2),
        scroll_width: number(3),
        dark: boolean(4),
        declares_dark: boolean(5),
        body_background: at(6)
            .map(|value| value.to_str().to_string())
            .unwrap_or_default(),
    })
}

/// GTK's render of the view, the buffer it composites.
fn rendered(view: &webkit6::WebView) -> Option<gdk::Texture> {
    let (width, height) = (view.width(), view.height());
    if width <= 0 || height <= 0 {
        return None;
    }
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(view)).snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node()?;
    let renderer = view.native()?.renderer()?;
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    Some(renderer.render_texture(&node, Some(&viewport)))
}

/// Every second pixel of every second row has one colour.
fn uniform(texture: &gdk::Texture) -> bool {
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    if width == 0 || height == 0 {
        return true;
    }
    let stride = width * 4;
    let mut data = vec![0; stride * height];
    texture.download(&mut data, stride);
    let first = [data[0], data[1], data[2]];
    (0..height).step_by(2).all(|y| {
        (0..width)
            .step_by(2)
            .all(|x| data[y * stride + x * 4..y * stride + x * 4 + 3] == first)
    })
}

/// Scrolls the reader's column from the top to the end at [`SCROLL_SPEED`] with a tick
/// callback (no input), and returns the intervals between painted frames in ms: presentation
/// times where the backend reports them, else the paint clock.
async fn scroll(reader: &Reader, window: &sealwax_app::window::Window) -> Result<Vec<f64>, String> {
    let column = reader
        .widget()
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
        .ok_or("the reader isn't in a scrolled column")?;
    let adjustment = column.vadjustment();
    adjustment.set_value(0.0);
    support::frames(window.window(), 2).await?;
    let end = (adjustment.upper() - adjustment.page_size()).max(0.0);
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let painted: Rc<RefCell<Vec<(i64, Instant)>>> = Rc::default();
    let handler = clock.connect_after_paint(glib::clone!(
        #[strong]
        painted,
        move |clock| {
            let presented = clock
                .current_timings()
                .map_or(0, |timings| timings.presentation_time());
            painted.borrow_mut().push((presented, Instant::now()));
        }
    ));
    let (done, finished) = async_channel::bounded::<()>(1);
    let start = Instant::now();
    let limit = Duration::from_secs(8);
    column.add_tick_callback(move |_, _| {
        let elapsed = start.elapsed();
        let value = (elapsed.as_secs_f64() * SCROLL_SPEED).min(end);
        adjustment.set_value(value);
        if value >= end || elapsed > limit {
            let _ = done.try_send(());
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
    let _ = glib::future_with_timeout(limit + Duration::from_secs(2), finished.recv()).await;
    clock.disconnect(handler);
    let painted = painted.borrow();
    let presented = painted.iter().all(|(presented, _)| *presented > 0);
    Ok(painted
        .windows(2)
        .map(|pair| {
            if presented {
                (pair[1].0 - pair[0].0) as f64 / 1000.0
            } else {
                pair[1].1.duration_since(pair[0].1).as_secs_f64() * 1000.0
            }
        })
        .collect())
}

fn percentile(sorted_or_not: &[f64], p: f64) -> f64 {
    if sorted_or_not.is_empty() {
        return 0.0;
    }
    let mut values = sorted_or_not.to_vec();
    values.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    values[rank.clamp(1, values.len()) - 1]
}

fn print_row(row: &Row) {
    println!(
        "ROW theme={} id={} group={} kind={} style={} evidence={} size={}x{} document={} \
         height_ok={} stable={} scroll_width={} overflow={} blank_webkit={:?} blank_gtk={:?} \
         dark_query={} declares_dark={} body_bg={:?} ready_ms={:.1} painted_ms={:.1} \
         failures={:?}",
        row.theme,
        row.id,
        row.group,
        row.kind,
        row.style,
        row.evidence,
        row.width,
        row.height,
        row.document,
        row.height_ok,
        row.stable,
        row.scroll_width,
        row.overflow,
        row.blank_webkit,
        row.blank_gtk,
        row.dark_query,
        row.declares_dark,
        row.body_background,
        row.ready_ms,
        row.painted_ms,
        row.failures,
    );
}

/// The rows as a tab-separated table, for the report.
fn write_results(rows: &[Row]) -> Result<(), String> {
    let mut table = String::from(
        "theme\tid\tgroup\tkind\tstyle\tevidence\twidth\theight\tdocument\theight_ok\tstable\t\
         scroll_width\toverflow\tblank_webkit\tblank_gtk\tdark_query\tdeclares_dark\tbody_bg\t\
         ready_ms\tpainted_ms\n",
    );
    let mut by_id: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows {
        *by_id.entry(&row.id).or_default() += 1;
        let _ = writeln!(
            table,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{:.1}\t{:.1}",
            row.theme,
            row.id,
            row.group,
            row.kind,
            row.style,
            row.evidence,
            row.width,
            row.height,
            row.document,
            row.height_ok,
            row.stable,
            row.scroll_width,
            row.overflow,
            row.blank_webkit,
            row.blank_gtk,
            row.dark_query,
            row.declares_dark,
            row.body_background,
            row.ready_ms,
            row.painted_ms,
        );
    }
    let path = repository().join(".local/corpus/results.tsv");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&path, table).map_err(|error| error.to_string())?;
    println!("RESULTS {} ({} messages)", path.display(), by_id.len());
    Ok(())
}
