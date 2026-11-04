use crate::canary::{self, Canary};
use crate::composer::{self, Action, Composer, Event, Fields, Layers, WORLD};
use crate::mime::{self, Draft};
use crate::probe::Paint;
use crate::roundtrip::{self, Copy, Expected};
use crate::{broadway, fixtures, js, png};
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use std::cell::Cell;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::EditorTypingAttributes;
use webkit6::prelude::*;

const LIVE_DEADLINE: Duration = Duration::from_secs(55);

#[derive(Clone, Copy)]
struct Options {
    live: bool,
    samples: usize,
    latency: bool,
    functional: bool,
    width: i32,
    height: i32,
    capture: bool,
    accelerated: bool,
}

#[derive(Default)]
struct Report {
    passed: u32,
    failed: u32,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
        let detail = detail.as_ref();
        let mark = if ok { "PASS" } else { "FAIL" };
        if detail.is_empty() {
            println!("{mark} {name}");
        } else {
            println!("{mark} {name} — {detail}");
        }
    }

    fn note(&self, text: impl AsRef<str>) {
        println!("NOTE {}", text.as_ref());
    }
}

pub fn gate() -> Result<bool, String> {
    if broadway::is_broadway() {
        return Ok(false);
    }
    if std::env::var("MAIL_S7_ALLOW_LIVE").as_deref() == Ok("1") {
        return Ok(true);
    }
    Err(
        "refusing to open a window outside GTK Broadway; run through tools/headless.sh, \
         or set MAIL_S7_ALLOW_LIVE=1 for an authorized live run"
            .into(),
    )
}

pub fn run(args: &[String]) -> ExitCode {
    let live = match gate() {
        Ok(live) => live,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let mut options = Options {
        live,
        samples: if live { 80 } else { 200 },
        latency: true,
        functional: true,
        width: 1100,
        height: 820,
        capture: true,
        accelerated: true,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--samples" => match args.next().and_then(|value| value.parse().ok()) {
                Some(samples) => options.samples = samples,
                None => {
                    eprintln!("--samples needs a number");
                    return ExitCode::from(2);
                }
            },
            "--no-latency" => options.latency = false,
            "--no-capture" => options.capture = false,
            "--no-acceleration" => options.accelerated = false,
            "--size" => match args
                .next()
                .and_then(|value| value.split_once('x'))
                .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
            {
                Some((width, height)) => {
                    options.width = width;
                    options.height = height;
                }
                None => {
                    eprintln!("--size needs WIDTHxHEIGHT");
                    return ExitCode::from(2);
                }
            },
            "--latency-only" => options.functional = false,
            other => {
                eprintln!("unknown option {other}");
                return ExitCode::from(2);
            }
        }
    }
    if !live && let Err(error) = broadway::connect() {
        eprintln!("could not connect the Broadway web client: {error}");
        return ExitCode::FAILURE;
    }
    println!(
        "S7 self-test: {} ({} build), {} samples per latency series, window {}x{}",
        if live {
            "LIVE Wayland session"
        } else {
            "headless GTK Broadway"
        },
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        options.samples,
        options.width,
        options.height
    );
    let app = adw::Application::builder()
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let failures = Rc::new(Cell::new(None::<u32>));
    let result = failures.clone();
    app.connect_activate(move |app| {
        if let Some(settings) = gtk4::Settings::default() {
            settings.set_gtk_cursor_blink(false);
        }
        let app = app.clone();
        let failures = failures.clone();
        let hold = app.hold();
        glib::MainContext::default().spawn_local(async move {
            let failed = selftest(&app, options).await;
            failures.set(Some(failed));
            drop(hold);
            app.quit();
        });
    });
    if live {
        let app = app.clone();
        glib::timeout_add_local_once(LIVE_DEADLINE, move || {
            eprintln!("live deadline reached; quitting");
            app.quit();
        });
    }
    app.run_with_args::<&str>(&[]);
    match result.get() {
        Some(0) => ExitCode::SUCCESS,
        Some(_) => ExitCode::FAILURE,
        None => {
            eprintln!("the self-test did not finish");
            ExitCode::FAILURE
        }
    }
}

async fn selftest(app: &adw::Application, options: Options) -> u32 {
    let mut report = Report::default();
    let filter = match composer::compile_filter().await {
        Ok(filter) => {
            report.check("content filter compiles (4 block rules)", true, "");
            filter
        }
        Err(error) => {
            report.check("content filter compiles", false, error);
            return report.failed;
        }
    };
    let session = composer::ephemeral_session();
    let composer = Composer::new(&filter, &session, options.accelerated);
    let test_area = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.append(&composer.root);
    content.append(&test_area);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("S7 composer self-test")
        .default_width(options.width)
        .default_height(options.height)
        .content(&content)
        .build();
    let key_presses = Rc::new(Cell::new(0u32));
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let counter = key_presses.clone();
    keys.connect_key_pressed(move |_, _, _, _| {
        counter.set(counter.get() + 1);
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    window.present();

    let ready = wait_event(&composer, Duration::from_secs(20), |event| {
        matches!(event, Event::Ready { .. })
    })
    .await;
    report.check(
        "editor page loads; helper runs in the isolated world and posts to the UI",
        matches!(ready, Some(Event::Ready { editable: true })),
        format!("{ready:?}"),
    );
    if ready.is_none() {
        window.close();
        return report.failed;
    }
    composer.view.grab_focus();
    let world = composer
        .eval("typeof window.mailComposer")
        .await
        .map(|value| value.to_str().to_string());
    let main = composer
        .view
        .evaluate_javascript_future("typeof window.mailComposer", None, None)
        .await
        .map(|value| value.to_str().to_string());
    report.check(
        "helper API exists only in the isolated world",
        world.as_deref() == Ok("object") && main.as_deref() == Ok("undefined"),
        format!("isolated world: {world:?}; page world: {main:?}"),
    );

    let canary = match Canary::start() {
        Ok(canary) => canary,
        Err(error) => {
            report.check("local HTTP canary starts", false, error.to_string());
            return report.failed;
        }
    };
    if options.functional {
        hardening(
            &composer,
            &filter,
            &session,
            &test_area,
            &canary,
            &mut report,
        )
        .await;
        formatting(&composer, &mut report).await;
        if options.live {
            report.note(
                "clipboard paste tests skipped live (they would replace the user's clipboard)",
            );
            cleaner_without_clipboard(&composer, &canary, &mut report).await;
        } else {
            paste(&composer, &canary, &mut report).await;
        }
        drop_files(&composer, &mut report).await;
        outgoing(&composer, &canary, options, &mut report).await;
        spell_checking(&mut report);
    }
    if options.latency {
        latency(&composer, options, &mut report).await;
    }
    report.note(format!(
        "real key events received during the run: {} (any would mean the user typed into the window)",
        key_presses.get()
    ));
    report.note(format!(
        "canary requests in total: {} ({} from the positive control); all: {:?}",
        canary.total(),
        canary.hits("control").len(),
        canary.hits("")
    ));
    println!("SUMMARY {} passed, {} failed", report.passed, report.failed);
    window.close();
    report.failed
}

async fn sleep(ms: u64) {
    glib::timeout_future(Duration::from_millis(ms)).await;
}

async fn until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(2).await;
    }
}

fn drain_events(composer: &Composer) {
    while composer.events.try_recv().is_ok() {}
}

async fn wait_event(
    composer: &Composer,
    timeout: Duration,
    mut wanted: impl FnMut(&Event) -> bool,
) -> Option<Event> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match glib::future_with_timeout(remaining, composer.events.recv()).await {
            Ok(Ok(event)) if wanted(&event) => return Some(event),
            Ok(Ok(_)) => {}
            _ => return None,
        }
    }
}

async fn set_content(composer: &Composer, html: &str) {
    let _ = composer
        .eval(&format!("mailComposer.setContent({})", js::literal(html)))
        .await;
    sleep(30).await;
}

async fn call(composer: &Composer, function: &str, args: &[&str]) -> bool {
    let args: Vec<String> = args.iter().map(|arg| js::literal(arg)).collect();
    let result = composer
        .eval(&format!("mailComposer.{function}({})", args.join(", ")))
        .await;
    sleep(30).await;
    result.is_ok_and(|value| value.to_boolean())
}

async fn caret(composer: &Composer, needle: &str, offset: u32) -> bool {
    let result = composer
        .eval(&format!(
            "mailComposer.caretAt({}, {offset})",
            js::literal(needle)
        ))
        .await;
    sleep(30).await;
    result.is_ok_and(|value| value.to_boolean())
}

/// Polls the editor HTML until `wanted` holds; returns the last HTML seen.
async fn html_until(composer: &Composer, mut wanted: impl FnMut(&str) -> bool) -> (bool, String) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let html = composer.html().await;
        if wanted(&html) {
            return (true, html);
        }
        if Instant::now() >= deadline {
            return (false, html);
        }
        sleep(10).await;
    }
}

fn short(html: &str) -> String {
    const LIMIT: usize = 220;
    if html.chars().count() <= LIMIT {
        html.to_string()
    } else {
        let cut: String = html.chars().take(LIMIT).collect();
        format!("{cut}…")
    }
}

async fn hardening(
    composer: &Composer,
    filter: &webkit6::UserContentFilter,
    session: &webkit6::NetworkSession,
    test_area: &gtk4::Box,
    canary: &Canary,
    report: &mut Report,
) {
    let variants = [
        ("all", true, true),
        ("filter-only", false, true),
        ("csp-only", true, false),
        ("control", false, false),
    ];
    let mut views = Vec::new();
    for (prefix, csp, use_filter) in variants {
        let layers = Layers {
            csp,
            filter: use_filter.then(|| filter.clone()),
        };
        let (view, _) = composer::hardened_view(&layers, session, Rc::new(Cell::new(0)));
        view.set_size_request(200, 90);
        test_area.append(&view);
        view.load_html(&canary::hostile_page(canary, prefix), None);
        views.push((prefix, view));
    }
    for (_, view) in &views {
        until(Duration::from_secs(10), || !view.is_loading()).await;
    }
    sleep(1500).await;
    for (prefix, view) in &views {
        let hits = canary.hits(prefix);
        let title = view.title().unwrap_or_default();
        if *prefix == "control" {
            report.check(
                "positive control (no CSP, no filter): the canary sees passive loads",
                !hits.is_empty(),
                format!("{} requests: {}", hits.len(), hits.join(" ")),
            );
            let scripted: Vec<&String> = hits
                .iter()
                .filter(|path| path.contains("script") || path.contains("onerror"))
                .collect();
            report.check(
                "enable-javascript-markup=false: no markup script or event handler ran, even without CSP",
                scripted.is_empty() && title == "canary",
                format!("title {title:?}; script-driven requests {scripted:?}"),
            );
        } else {
            report.check(
                &format!("hardened view ({prefix}): zero canary requests"),
                hits.is_empty(),
                format!("{} requests {hits:?}; title {title:?}", hits.len()),
            );
        }
    }
    for (prefix, view) in &views {
        let fetch_url = canary.url(&format!("{prefix}/world-fetch"));
        let image_url = canary.url(&format!("{prefix}/world-image"));
        let body = format!(
            "const image = new Image(); image.src = {}; document.body.appendChild(image); \
             try {{ await fetch({}); return 'fetch completed'; }} \
             catch (error) {{ return 'fetch failed: ' + error; }}",
            js::literal(&image_url),
            js::literal(&fetch_url)
        );
        let outcome = view
            .call_async_javascript_function_future(&body, None, Some(WORLD), None)
            .await
            .map(|value| value.to_str().to_string())
            .unwrap_or_else(|error| error.to_string());
        sleep(500).await;
        let fetched = !canary.hits(&format!("{prefix}/world-fetch")).is_empty();
        let imaged = !canary.hits(&format!("{prefix}/world-image")).is_empty();
        let detail = format!("{outcome}; canary saw fetch {fetched}, image {imaged}");
        match *prefix {
            "all" | "filter-only" => report.check(
                &format!("helper world ({prefix}): fetch() and new Image() reach nothing"),
                !fetched && !imaged,
                detail,
            ),
            _ => report.note(format!(
                "helper world ({prefix}, no content filter): {detail}"
            )),
        }
    }
    for (_, view) in views {
        test_area.remove(&view);
    }

    set_content(composer, "<div>remote test</div>").await;
    let before = composer.blocked_navigations.get();
    caret(composer, "remote", 6).await;
    let _ = composer
        .eval(&format!(
            "document.execCommand('insertHTML', false, '<img src=\"{}\" alt=\"x\">'); \
             location.href = '{}'; true",
            canary.url("composer/inserted.png"),
            canary.url("composer/navigation")
        ))
        .await;
    sleep(1200).await;
    let hits = canary.hits("composer");
    let uri = composer.view.uri().unwrap_or_default();
    report.check(
        "composer: an <img> inserted past the paste cleaner and a scripted navigation load nothing",
        hits.is_empty() && composer.blocked_navigations.get() > before,
        format!(
            "{} requests; navigations blocked {}; uri {uri:?}",
            hits.len(),
            composer.blocked_navigations.get() - before
        ),
    );
}

async fn formatting(composer: &Composer, report: &mut Report) {
    let inline = [
        (
            Action::Bold,
            EditorTypingAttributes::BOLD,
            &["<b>beta</b>"][..],
        ),
        (
            Action::Italic,
            EditorTypingAttributes::ITALIC,
            &["<i>beta</i>"][..],
        ),
        (
            Action::Underline,
            EditorTypingAttributes::UNDERLINE,
            &["<u>beta</u>"][..],
        ),
        (
            Action::Strikethrough,
            EditorTypingAttributes::STRIKETHROUGH,
            &["<strike>beta</strike>", "<s>beta</s>"][..],
        ),
    ];
    for (action, flag, tags) in inline {
        set_content(composer, "<div>alpha beta gamma</div>").await;
        call(composer, "selectText", &["beta"]).await;
        composer.run(action);
        let (applied, html) =
            html_until(composer, |html| tags.iter().any(|tag| html.contains(tag))).await;
        report.check(
            &format!("{} applies to the selection", action.label()),
            applied,
            short(&html),
        );
        let on = until(Duration::from_secs(1), || {
            composer.typing_has(flag) && composer.toggle_active(action)
        })
        .await;
        report.check(
            &format!(
                "{}: EditorState typing attribute and toolbar toggle on",
                action.label()
            ),
            on,
            format!("typing attributes {:#x}", composer.typing.get()),
        );
        caret(composer, "alpha", 2).await;
        let off = until(Duration::from_secs(1), || {
            !composer.typing_has(flag) && !composer.toggle_active(action)
        })
        .await;
        caret(composer, "beta", 2).await;
        let back_on = until(Duration::from_secs(1), || {
            composer.typing_has(flag) && composer.toggle_active(action)
        })
        .await;
        report.check(
            &format!(
                "{}: toolbar follows the caret (off in plain text, on inside)",
                action.label()
            ),
            off && back_on,
            format!("off {off}, on again {back_on}"),
        );
        let undo_available = composer.button_sensitive(Action::Undo);
        composer.run(Action::Undo);
        let (undone, html) =
            html_until(composer, |html| !tags.iter().any(|tag| html.contains(tag))).await;
        composer.run(Action::Redo);
        let (redone, _) =
            html_until(composer, |html| tags.iter().any(|tag| html.contains(tag))).await;
        report.check(
            &format!("{}: Undo removes it, Redo restores it", action.label()),
            undo_available && undone && redone,
            format!(
                "undo button enabled {undo_available}; after undo {}",
                short(&html)
            ),
        );
    }

    set_content(composer, "<div>start</div>").await;
    composer.eval_detached("mailComposer.caretAtEnd()");
    sleep(40).await;
    composer.run(Action::Bold);
    let typing_bold = until(Duration::from_secs(1), || {
        composer.typing_has(EditorTypingAttributes::BOLD) && composer.toggle_active(Action::Bold)
    })
    .await;
    composer
        .view
        .execute_editing_command_with_argument("InsertText", " typed");
    let (bold_typed, html) = html_until(composer, |html| {
        html.contains("<b> typed</b>") || html.contains("<b>&nbsp;typed</b>")
    })
    .await;
    report.check(
        "Bold with a collapsed caret sets the typing style; typed text comes out bold",
        typing_bold && bold_typed,
        short(&html),
    );

    set_content(composer, "<div>undo</div>").await;
    composer.eval_detached("mailComposer.caretAtEnd()");
    sleep(40).await;
    for ch in [" ", "o", "n", "e"] {
        composer
            .view
            .execute_editing_command_with_argument("InsertText", ch);
        sleep(15).await;
    }
    let (typed, _) = html_until(composer, |html| html.contains("undo one")).await;
    composer.run(Action::Undo);
    let (undone_once, html) = html_until(composer, |html| !html.contains("one")).await;
    report.check(
        "Undo after four InsertText commands removes the typed run in one step",
        typed && undone_once,
        short(&html),
    );

    let lists = [
        (Action::BulletedList, "<ul>", "bulleted"),
        (Action::NumberedList, "<ol>", "numbered"),
    ];
    for (action, tag, name) in lists {
        set_content(composer, "<div>first line</div><div>second line</div>").await;
        caret(composer, "first", 1).await;
        composer.run(action);
        let (listed, html) = html_until(composer, |html| html.contains(tag)).await;
        let toggled = until(Duration::from_secs(1), || composer.toggle_active(action)).await;
        caret(composer, "second", 1).await;
        let off = until(Duration::from_secs(1), || !composer.toggle_active(action)).await;
        report.check(
            &format!(
                "{} list: one paragraph becomes a list; toggle follows the caret",
                name
            ),
            listed && toggled && off,
            format!("toggle on {toggled}, off outside {off}; {}", short(&html)),
        );
        caret(composer, "first", 1).await;
        composer.run(Action::Indent);
        let (nested, html) = html_until(composer, |html| html.matches(tag).count() >= 2).await;
        report.check(
            &format!("{} list: Indent inside a list nests it", name),
            nested,
            short(&html),
        );
        composer.run(Action::Outdent);
        sleep(60).await;
        composer.run(action);
        let (removed, html) = html_until(composer, |html| !html.contains(tag)).await;
        report.check(
            &format!("{} list: toggling again removes the list", name),
            removed,
            short(&html),
        );
    }

    set_content(composer, "<div>visit example now</div>").await;
    call(composer, "selectText", &["example"]).await;
    composer.create_link("https://example.org/a?b=1&c=2");
    let (linked, html) = html_until(composer, |html| {
        html.contains("<a href=\"https://example.org/a?b=1&amp;c=2\">example</a>")
    })
    .await;
    caret(composer, "example", 3).await;
    let link_on = until(Duration::from_secs(1), || {
        composer.toggle_active(Action::Link)
    })
    .await;
    report.check(
        "Link: CreateLink wraps the selection; the toggle shows the caret is in a link",
        linked && link_on,
        short(&html),
    );
    composer.run(Action::Link);
    let (unlinked, html) = html_until(composer, |html| !html.contains("<a ")).await;
    report.check(
        "Link: the toolbar button unlinks when inside a link",
        unlinked,
        short(&html),
    );
    composer.eval_detached("mailComposer.caretAtEnd()");
    sleep(40).await;
    composer.create_link("https://example.net/");
    let (inserted, html) = html_until(composer, |html| {
        html.contains("<a href=\"https://example.net/\">https://example.net/</a>")
    })
    .await;
    report.check(
        "Link: with a collapsed caret CreateLink inserts the URL as the link text",
        inserted,
        short(&html),
    );

    set_content(composer, "<div>quoted paragraph</div><div>after</div>").await;
    caret(composer, "quoted", 1).await;
    composer.run(Action::Quote);
    let (quoted, html) = html_until(composer, |html| html.contains("<blockquote>")).await;
    let quote_on = until(Duration::from_secs(1), || {
        composer.toggle_active(Action::Quote)
    })
    .await;
    report.check(
        "Quote: FormatBlock blockquote makes a plain <blockquote>; the toggle follows",
        quoted && quote_on,
        short(&html),
    );
    composer.run(Action::Quote);
    let (unquoted, html) = html_until(composer, |html| !html.contains("<blockquote")).await;
    report.check(
        "Quote: pressing it again (Outdent) removes the quote",
        unquoted,
        short(&html),
    );

    set_content(composer, "<div>indent me</div>").await;
    caret(composer, "indent", 1).await;
    composer.run(Action::Indent);
    let (indented, html) = html_until(composer, |html| {
        html.contains("<blockquote style=\"margin: 0 0 0 40px")
    })
    .await;
    composer.run(Action::Outdent);
    let (outdented, after) = html_until(composer, |html| !html.contains("<blockquote")).await;
    report.check(
        "Indent makes WebKit's margin blockquote; Outdent removes it",
        indented && outdented,
        format!("{} → {}", short(&html), short(&after)),
    );

    set_content(composer, "<div>plain <b><i><u>styled</u></i></b> end</div>").await;
    call(composer, "selectText", &["styled"]).await;
    composer.run(Action::ClearFormatting);
    let (cleared, html) = html_until(composer, |html| {
        !html.contains("<b>") && !html.contains("<i>") && !html.contains("<u>")
    })
    .await;
    report.check(
        "Clear formatting (RemoveFormat) strips b/i/u",
        cleared,
        short(&html),
    );
}

fn set_clipboard(providers: &[gdk::ContentProvider]) -> bool {
    let Some(display) = gdk::Display::default() else {
        return false;
    };
    let union = gdk::ContentProvider::new_union(providers);
    display.clipboard().set_content(Some(&union)).is_ok()
}

fn bytes_provider(mime: &str, data: &[u8]) -> gdk::ContentProvider {
    gdk::ContentProvider::for_bytes(mime, &glib::Bytes::from(data))
}

fn tiny_png_base64() -> String {
    let encoded = mail_builder::encoders::base64::Base64Encoder::new()
        .encode(&png::sample_image(8, 6))
        .unwrap_or_default();
    String::from_utf8_lossy(&encoded).into_owned()
}

fn paste_checks(html: &str, canary: &Canary, report: &mut Report) {
    let lower = html.to_ascii_lowercase();
    let forbidden = [
        "<script",
        "onerror",
        "onclick",
        "onmouseover",
        "onload",
        "<iframe",
        "<form",
        "<input",
        "<button",
        "<svg",
        "<video",
        "<style",
        "<link",
        "<meta",
        "<title",
        "<font",
        "<o:p",
        "class=",
        "javascript:",
        "127.0.0.1",
        "mso-",
        "background",
    ];
    let found: Vec<&str> = forbidden
        .iter()
        .copied()
        .filter(|needle| lower.contains(needle))
        .collect();
    report.check(
        "paste cleaning: no scripts, handlers, frames, forms, SVG, media, styles, classes, remote URLs",
        found.is_empty(),
        format!("found {found:?}"),
    );
    let kept = [
        ("<b>Bold from Docs</b>", "Docs font-weight:700 span → <b>"),
        (
            "<i>italic from Docs</i>",
            "Docs font-style:italic span → <i>",
        ),
        ("href=\"https://example.org/ok\"", "https link kept"),
        ("<li>list item</li>", "list kept"),
        ("colspan=\"2\"", "table colspan kept"),
        ("data:image/png;base64,", "data: image kept"),
        ("data-cid=\"", "Content-ID token assigned"),
    ];
    let missing: Vec<&str> = kept
        .iter()
        .filter(|(needle, _)| !html.contains(needle))
        .map(|(_, what)| *what)
        .collect();
    report.check(
        "paste cleaning keeps basic formatting (bold, italic, links, lists, tables, data: images)",
        missing.is_empty(),
        format!("missing {missing:?}"),
    );
    let docs_wrapper = !html.contains("<b><div>") && !html.contains("<b>Word paragraph");
    report.check(
        "paste cleaning: Google Docs' <b style=\"font-weight:normal\"> wrapper does not bold everything",
        docs_wrapper,
        "",
    );
    let before = html
        .find("under-strike")
        .map(|at| &html[at.saturating_sub(24)..at])
        .unwrap_or_default();
    report.check(
        "paste cleaning: text-decoration underline + line-through → <u> and <s>",
        before.contains("<u>") && (before.contains("<s>") || before.contains("<strike>")),
        format!("…{before}under-strike"),
    );
    let bad_link_text = html.contains("bad link") && !html.contains("javascript");
    report.check(
        "paste cleaning: a javascript: link keeps its text and loses its href",
        bad_link_text,
        "",
    );
    let hits = canary.hits("paste");
    report.check(
        "paste: zero canary requests from the pasted remote references",
        hits.is_empty(),
        format!("{hits:?}"),
    );
}

async fn paste(composer: &Composer, canary: &Canary, report: &mut Report) {
    let hostile = canary::hostile_paste(canary, &tiny_png_base64());
    set_content(composer, "<div>before paste</div>").await;
    composer.eval_detached("mailComposer.caretAtEnd()");
    sleep(40).await;
    let set = set_clipboard(&[
        bytes_provider("text/html", hostile.as_bytes()),
        bytes_provider("text/plain;charset=utf-8", b"plain fallback"),
    ]);
    drain_events(composer);
    composer
        .view
        .execute_editing_command(webkit6::EDITING_COMMAND_PASTE);
    let event = wait_event(
        composer,
        Duration::from_secs(5),
        |event| matches!(event, Event::Transfer(transfer) if transfer.kind == "paste"),
    )
    .await;
    let Some(Event::Transfer(transfer)) = event else {
        report.check(
            "paste: Ctrl+V path (Paste command) fires the paste event in the helper",
            false,
            format!("clipboard set {set}; no paste event"),
        );
        return;
    };
    report.check(
        "paste: the Paste command fires the paste event; the helper reads text/html and cleans it",
        transfer.mode == "html",
        format!(
            "mode {}, types {:?}; {}",
            transfer.mode,
            transfer.types,
            transfer
                .stats
                .as_ref()
                .map(|stats| stats.summary())
                .unwrap_or_default()
        ),
    );
    sleep(800).await;
    let html = composer.html().await;
    report.note(format!("pasted HTML as stored by WebKit: {}", short(&html)));
    paste_checks(&html, canary, report);
    let style_attributes = html.matches("style=").count();
    report.note(format!(
        "style attributes WebKit added while inserting the cleaned paste: {style_attributes}"
    ));
    composer.run(Action::Undo);
    let (undone, html) = html_until(composer, |html| !html.contains("Bold from Docs")).await;
    report.check("paste is one undo step", undone, short(&html));

    set_content(composer, "<div>text:</div>").await;
    composer.eval_detached("mailComposer.caretAtEnd()");
    sleep(40).await;
    set_clipboard(&[bytes_provider(
        "text/plain;charset=utf-8",
        "line one <b>not markup</b>\nline two æøå".as_bytes(),
    )]);
    drain_events(composer);
    composer
        .view
        .execute_editing_command(webkit6::EDITING_COMMAND_PASTE);
    let event = wait_event(composer, Duration::from_secs(5), |event| {
        matches!(event, Event::Transfer(_))
    })
    .await;
    let (plain, html) = html_until(composer, |html| html.contains("&lt;b&gt;not markup")).await;
    report.check(
        "plain-text paste inserts text, never markup",
        plain,
        format!(
            "{:?}; {}",
            event.map(|event| format!("{event:?}")),
            short(&html)
        ),
    );

    set_content(composer, "<div>image:</div>").await;
    composer.eval_detached("mailComposer.caretAtEnd()");
    sleep(40).await;
    set_clipboard(&[bytes_provider("image/png", &png::sample_image(40, 30))]);
    drain_events(composer);
    composer
        .view
        .execute_editing_command(webkit6::EDITING_COMMAND_PASTE);
    let event = wait_event(composer, Duration::from_secs(5), |event| {
        matches!(event, Event::Transfer(_))
    })
    .await;
    let adopted = wait_event(composer, Duration::from_secs(5), |event| {
        matches!(event, Event::Adopted { .. } | Event::Error(_))
    })
    .await;
    let adopted_png = match &adopted {
        Some(Event::Adopted { mime, size }) => {
            report.note(format!("image paste: WebKit's native paste gave a blob: image ({mime}, {size} bytes); the helper turned it into a data: image"));
            mime == "image/png"
        }
        Some(Event::Error(message)) => {
            report.note(format!("image paste: helper error: {message}"));
            false
        }
        _ => false,
    };
    sleep(300).await;
    let html = composer.html().await;
    let data_image = adopted_png
        && html.contains("<img src=\"data:image/png;base64,")
        && html.contains("data-cid=");
    report.check(
        "image paste (clipboard image/png only) becomes a data: image with a Content-ID token",
        data_image,
        format!(
            "{}; {}",
            event
                .map(|event| format!("{event:?}"))
                .unwrap_or_else(|| "no paste event".into()),
            short(&html)
        ),
    );
}

async fn cleaner_without_clipboard(composer: &Composer, canary: &Canary, report: &mut Report) {
    let hostile = canary::hostile_paste(canary, &tiny_png_base64());
    set_content(composer, "<div>before paste</div>").await;
    composer.eval_detached("mailComposer.caretAtEnd()");
    sleep(40).await;
    let stats = composer
        .eval(&format!(
            "mailComposer.insertCleaned({})",
            js::literal(&hostile)
        ))
        .await
        .map(|value| composer::CleanStats::from_value(&value).summary())
        .unwrap_or_else(|error| error);
    report.note(format!("cleaner (no clipboard): {stats}"));
    sleep(800).await;
    let html = composer.html().await;
    paste_checks(&html, canary, report);
}

async fn drop_files(composer: &Composer, report: &mut Report) {
    set_content(composer, "<div>drop here</div>").await;
    drain_events(composer);
    let dropped = composer
        .eval(&format!(
            "mailComposer.testDrop({}, 'image/png', {})",
            js::literal("blåbær.png"),
            js::literal(&tiny_png_base64())
        ))
        .await
        .is_ok_and(|value| value.to_boolean());
    let event = wait_event(
        composer,
        Duration::from_secs(5),
        |event| matches!(event, Event::Transfer(transfer) if transfer.kind == "drop"),
    )
    .await;
    let (inserted, html) = html_until(composer, |html| {
        html.contains("data:image/png;base64,") && html.contains("data-name=\"blåbær.png\"")
    })
    .await;
    let counted = matches!(&event, Some(Event::Transfer(transfer)) if transfer.files == 1 && transfer.images == 1 && transfer.attachments == 0);
    report.check(
        "dropped image file (synthetic DragEvent) becomes a data: image with a Content-ID token",
        dropped && inserted && counted,
        format!(
            "{}; {}",
            event
                .map(|event| format!("{event:?}"))
                .unwrap_or_else(|| "no drop event".into()),
            short(&html)
        ),
    );
    drain_events(composer);
    let _ = composer
        .eval(&format!(
            "mailComposer.testDrop('notater.pdf', 'application/pdf', {})",
            js::literal("JVBERi0xLjQK")
        ))
        .await;
    let attach = wait_event(composer, Duration::from_secs(5), |event| {
        matches!(event, Event::Attach { .. })
    })
    .await;
    report.check(
        "dropped non-image file is handed to the UI as an attachment, not inserted",
        matches!(&attach, Some(Event::Attach { name, mime }) if name == "notater.pdf" && mime == "application/pdf"),
        format!("{attach:?}"),
    );
}

async fn outgoing(composer: &Composer, canary: &Canary, options: Options, report: &mut Report) {
    set_content(
        composer,
        "<div>Hei Ola,</div><div><br></div>\
         <div>Dette er fet, kursiv, understreket og gjennomstreket tekst med æøå og 🫐.</div>\
         <div>Punkt én</div><div>Punkt to</div><div>Første steg</div><div>Andre steg</div>\
         <div>Les mer her.</div><div>Sitat fra forrige melding.</div><div>Innrykket linje.</div>\
         <div>Hilsen Kari</div>",
    )
    .await;
    for (needle, action) in [
        ("fet", Action::Bold),
        ("kursiv", Action::Italic),
        ("understreket", Action::Underline),
        ("gjennomstreket", Action::Strikethrough),
    ] {
        call(composer, "selectText", &[needle]).await;
        composer.run(action);
        sleep(40).await;
    }
    call(composer, "selectBetween", &["Punkt én", "Punkt to"]).await;
    composer.run(Action::BulletedList);
    sleep(60).await;
    call(composer, "selectBetween", &["Første steg", "Andre steg"]).await;
    composer.run(Action::NumberedList);
    sleep(60).await;
    call(composer, "selectText", &["her"]).await;
    composer.create_link("https://example.org/blåbær");
    sleep(40).await;
    caret(composer, "Sitat", 1).await;
    composer.run(Action::Quote);
    sleep(40).await;
    caret(composer, "Innrykket", 1).await;
    composer.run(Action::Indent);
    sleep(40).await;
    caret(composer, "Hilsen", 0).await;
    let _ = composer
        .eval(&format!(
            "mailComposer.testDrop({}, 'image/png', {})",
            js::literal("blåbær.png"),
            js::literal(&{
                let encoded = mail_builder::encoders::base64::Base64Encoder::new()
                    .encode(&png::sample_image(120, 75))
                    .unwrap_or_default();
                String::from_utf8_lossy(&encoded).into_owned()
            })
        ))
        .await;
    let (_, html) = html_until(composer, |html| html.contains("data:image/png")).await;
    report.note(format!("composed editor HTML: {}", short(&html)));

    let domain = match crate::ids::id_domain(&composer.fields.from.address) {
        Ok(domain) => domain,
        Err(error) => {
            report.check("sender domain for Content-IDs", false, error);
            return;
        }
    };
    let (fragment, images) = match composer.serialize(&domain).await {
        Ok(serialized) => serialized,
        Err(error) => {
            report.check("serialise the editor", false, error);
            return;
        }
    };
    report.check(
        "serialise: data: images become cid: references with Content-IDs @sender-domain",
        !fragment.contains("data:")
            && !fragment.contains("data-cid")
            && images.len() == 1
            && images.iter().all(|image| {
                image.content_id.ends_with(&format!("@{domain}"))
                    && fragment.contains(&format!("cid:{}", image.content_id))
            }),
        format!("{} images; {}", images.len(), short(&fragment)),
    );
    report.check(
        "serialise: quotes become <blockquote type=\"cite\"> with inline style; indents keep their margin",
        fragment.contains("<blockquote type=\"cite\" style=") && fragment.contains("margin: 0 0 0 40px"),
        "",
    );
    let expected_tags = [
        "<b>fet</b>",
        "<i>kursiv</i>",
        "<u>understreket</u>",
        "<ul>",
        "<ol>",
        "<a href=\"https://example.org/bl",
    ];
    let missing: Vec<&str> = expected_tags
        .iter()
        .copied()
        .filter(|tag| !fragment.contains(tag))
        .collect();
    report.check(
        "serialise: formatting from the toolbar commands survives",
        missing.is_empty(),
        format!("missing {missing:?}"),
    );

    let draft = Draft {
        from: composer.fields.from.clone(),
        to: Fields::mailboxes(&composer.fields.to),
        cc: Fields::mailboxes(&composer.fields.cc),
        bcc: Fields::mailboxes(&composer.fields.bcc),
        subject: composer.fields.subject.text().to_string(),
        html_fragment: fragment,
        inline_images: images,
        attachments: fixtures::sample_attachments(),
    };
    let date = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(fixtures::FIXTURE_DATE);
    let outgoing = match mime::build(&draft, date) {
        Ok(outgoing) => outgoing,
        Err(error) => {
            report.check("build MIME from the composer", false, error);
            return;
        }
    };
    let name = if options.live {
        "composer-live"
    } else {
        "composer-headless"
    };
    match fixtures::write_pair(name, &outgoing) {
        Ok((transmitted, sent)) => report.note(format!(
            "wrote {} and {}",
            transmitted.display(),
            sent.display()
        )),
        Err(error) => report.check("write the composer .eml files", false, error.to_string()),
    }
    let expected = Expected {
        draft: &draft,
        outgoing: &outgoing,
    };
    for (copy, raw) in [
        (Copy::Transmitted, &outgoing.transmitted),
        (Copy::SentCopy, &outgoing.sent_copy),
    ] {
        for check in roundtrip::check(raw, copy, Some(&expected)) {
            report.check(
                &format!("composer {copy:?}: {}", check.name),
                check.ok,
                check.detail,
            );
        }
    }
    let hits = canary.hits("composer");
    report.check(
        "no canary requests while composing and serialising",
        hits.is_empty(),
        format!("{hits:?}"),
    );
}

fn spell_checking(report: &mut Report) {
    let Some(context) = webkit6::WebContext::default() else {
        report.note("no default WebContext");
        return;
    };
    context.set_spell_checking_enabled(true);
    context.set_spell_checking_languages(&["en_US", "nb_NO", "nn_NO"]);
    let languages: Vec<String> = context
        .spell_checking_languages()
        .iter()
        .map(|language| language.to_string())
        .collect();
    report.note(format!(
        "spell checking: enabled {}; languages WebKit accepted from [en_US, nb_NO, nn_NO]: {languages:?}",
        context.is_spell_checking_enabled()
    ));
}

#[derive(Default)]
struct Series {
    name: String,
    first_paint: Vec<f64>,
    first_snapshot: Vec<f64>,
    frame_work: Vec<f64>,
    last_paint: Vec<f64>,
    bursts: u32,
    settled_snapshot: Vec<f64>,
    first_is_settled: u32,
    first_changed: u32,
    captured: u32,
    timeouts: u32,
}

impl Series {
    fn print(&self, report: &Report) {
        let p = |values: &[f64]| {
            format!(
                "p50={} p95={} max={}",
                fmt(percentile(values, 0.50)),
                fmt(percentile(values, 0.95)),
                fmt(percentile(values, 1.0))
            )
        };
        if self.captured > 0 {
            report.note(format!(
                "FRAMES {} (pixel-hash pass, n={}): first frame after the edit differs from the \
                 pre-edit frame in {}, already equals the settled frame in {}; edit→snapshot of the \
                 first frame showing the settled content {} ms; timeouts {}",
                self.name,
                self.captured,
                self.first_changed,
                self.first_is_settled,
                p(&self.settled_snapshot),
                self.timeouts
            ));
        } else {
            report.note(format!(
                "LATENCY {} (n={}): edit→after-paint of the first WebView frame {} ms; edit→its \
                 snapshot {} ms; frame work (snapshot→after-paint) {} ms; edit→after-paint of the \
                 last frame before {} ms of quiet {} ms ({} samples had more than one frame); timeouts {}",
                self.name,
                self.first_paint.len(),
                p(&self.first_paint),
                p(&self.first_snapshot),
                p(&self.frame_work),
                QUIET.as_millis(),
                p(&self.last_paint),
                self.bursts,
                self.timeouts
            ));
        }
    }
}

pub fn percentile(values: &[f64], fraction: f64) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted.get(rank.saturating_sub(1)).copied()
}

pub fn fmt(value: Option<f64>) -> String {
    value.map_or_else(|| "–".into(), |value| format!("{value:.2}"))
}

pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub async fn wait_paint(
    paints: &async_channel::Receiver<Paint>,
    after: Instant,
    timeout: Duration,
) -> Option<Paint> {
    let deadline = after + timeout;
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
pub async fn burst(
    paints: &async_channel::Receiver<Paint>,
    gap: Duration,
    limit: Duration,
) -> Vec<Paint> {
    let deadline = Instant::now() + limit;
    let mut frames = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now()).min(gap);
        if remaining.is_zero() {
            return frames;
        }
        match glib::future_with_timeout(remaining, paints.recv()).await {
            Ok(Ok(paint)) => frames.push(paint),
            _ => return frames,
        }
    }
}

async fn collect_paints(paints: &async_channel::Receiver<Paint>, window: Duration) -> Vec<Paint> {
    let deadline = Instant::now() + window;
    let mut collected = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return collected;
        }
        match glib::future_with_timeout(remaining, paints.recv()).await {
            Ok(Ok(paint)) => collected.push(paint),
            _ => return collected,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    EditingCommand,
    ExecCommand,
}

fn edit(composer: &Composer, method: Method, text: &str) {
    match method {
        Method::EditingCommand => composer
            .view
            .execute_editing_command_with_argument("InsertText", text),
        Method::ExecCommand => composer.eval_detached(&format!(
            "document.execCommand('insertText', false, {})",
            js::literal(text)
        )),
    }
}

/// The WebView counts as quiet after this long without a frame.
const QUIET: Duration = Duration::from_millis(50);

/// Types one character per sample, each only once the WebView has been quiet for `QUIET` (so
/// no frame of an earlier change, such as the overlay scrollbar fading, is still in flight).
/// With `capture`, every frame is pixel-hashed (scrollbar strip excluded) to check that the
/// first frame after the edit already shows the settled content; that pass is slower and is
/// not used for timing.
async fn typing_series(
    composer: &Composer,
    name: &str,
    method: Method,
    samples: usize,
    capture: bool,
) -> Series {
    const TEXT: &str = "Blåbærsyltetøy på brødskiva, takk! ";
    let text: Vec<char> = TEXT.chars().collect();
    let mut series = Series {
        name: name.to_string(),
        ..Series::default()
    };
    composer.probe.set_capture(capture);
    if capture {
        composer.probe.queue_draw();
    }
    burst(
        &composer.paints,
        Duration::from_millis(80),
        Duration::from_secs(2),
    )
    .await;
    for index in 0..samples {
        if index > 0 && index % 60 == 0 {
            match method {
                Method::EditingCommand => composer.view.execute_editing_command("InsertParagraph"),
                Method::ExecCommand => {
                    composer.eval_detached("document.execCommand('insertParagraph')")
                }
            }
            burst(
                &composer.paints,
                Duration::from_millis(80),
                Duration::from_secs(2),
            )
            .await;
        }
        let before = composer.probe.last_hash();
        let started = Instant::now();
        edit(composer, method, &text[index % text.len()].to_string());
        let Some(first) = wait_paint(&composer.paints, started, Duration::from_secs(1)).await
        else {
            series.timeouts += 1;
            continue;
        };
        let mut frames = vec![first];
        frames.extend(burst(&composer.paints, QUIET, Duration::from_secs(2)).await);
        if capture {
            if std::env::var_os("MAIL_S7_TRACE").is_some() {
                let trace: Vec<String> = frames
                    .iter()
                    .map(|frame| {
                        format!(
                            "{:.1}ms/{:04x}",
                            ms(frame.snapshot_at - started),
                            frame.hash.unwrap_or_default() & 0xffff
                        )
                    })
                    .collect();
                println!(
                    "TRACE {name} #{index}: before {:04x}; frames {}",
                    before.unwrap_or_default() & 0xffff,
                    trace.join(" ")
                );
            }
            series.captured += 1;
            let settled = frames.last().and_then(|frame| frame.hash);
            series.first_changed += u32::from(first.hash != before);
            series.first_is_settled += u32::from(first.hash == settled);
            if let Some(frame) = frames.iter().find(|frame| frame.hash == settled) {
                series
                    .settled_snapshot
                    .push(ms(frame.snapshot_at - started));
            }
        } else {
            series.first_paint.push(ms(first.painted_at - started));
            series.first_snapshot.push(ms(first.snapshot_at - started));
            series
                .frame_work
                .push(ms(first.painted_at - first.snapshot_at));
            if let Some(last) = frames.last() {
                series.last_paint.push(ms(last.painted_at - started));
            }
            series.bursts += u32::from(frames.len() > 1);
        }
    }
    composer.probe.set_capture(false);
    series
}

async fn toolbar_series(composer: &Composer, samples: usize) -> Vec<f64> {
    let mut latencies = Vec::new();
    set_content(composer, "<div>plain <b>bold</b> plain</div>").await;
    for index in 0..samples {
        let (needle, want) = if index % 2 == 0 {
            ("bold", true)
        } else {
            ("plain", false)
        };
        let started = Instant::now();
        composer.eval_detached(&format!("mailComposer.caretAt({}, 2)", js::literal(needle)));
        let seen = until(Duration::from_secs(1), || {
            composer
                .typing_changed_at
                .get()
                .is_some_and(|at| at >= started)
                && composer.typing_has(EditorTypingAttributes::BOLD) == want
        })
        .await;
        if seen && let Some(at) = composer.typing_changed_at.get() {
            latencies.push(ms(at - started));
        }
        sleep(30).await;
    }
    latencies
}

fn large_quote() -> String {
    let mut html = String::from("<div><br></div><div>Svar over sitatet.</div><blockquote>");
    for index in 0..1500 {
        html.push_str(&format!(
            "<div>Sitert avsnitt {index}: blåbær, multebær og tyttebær i en lang tråd, \
             med en <a href=\"https://example.org/{index}\">lenke</a>.</div>"
        ));
        if index % 100 == 0 {
            html.push_str("<ul><li>punkt</li><li>punkt</li></ul>");
        }
    }
    html.push_str("</blockquote>");
    html
}

/// The renderer, sizes and monitor the latency numbers belong to.
pub fn display_details(composer: &Composer) -> String {
    let renderer = composer
        .probe
        .native()
        .and_then(|native| native.renderer())
        .map(|renderer| renderer.type_().name().to_string())
        .unwrap_or_else(|| "unknown".into());
    let window = composer
        .probe
        .root()
        .map(|root| format!("{}x{}", root.width(), root.height()))
        .unwrap_or_default();
    let monitor = composer
        .probe
        .native()
        .and_then(|native| native.surface())
        .and_then(|surface| {
            gdk::Display::default().and_then(|display| display.monitor_at_surface(&surface))
        })
        .map(|monitor| {
            let geometry = monitor.geometry();
            format!(
                "{}x{} at {:.2} Hz, scale {}",
                geometry.width(),
                geometry.height(),
                f64::from(monitor.refresh_rate()) / 1000.0,
                monitor.scale()
            )
        })
        .unwrap_or_else(|| "unknown".into());
    format!(
        "GSK renderer {renderer}; window {window}; WebView {}x{}; monitor {monitor}; WebKit \
         hardware-acceleration-policy {:?}; GSK_RENDERER={:?}, WEBKIT_DISABLE_DMABUF_RENDERER={:?}",
        composer.probe.width(),
        composer.probe.height(),
        WebViewExt::settings(&composer.view)
            .map(|settings| settings.hardware_acceleration_policy()),
        std::env::var("GSK_RENDERER").ok(),
        std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").ok()
    )
}

async fn fresh_small_document(composer: &Composer) {
    set_content(composer, "<div><br></div>").await;
    composer.eval_detached("mailComposer.caretAtEnd()");
    composer.view.grab_focus();
    sleep(200).await;
}

async fn latency(composer: &Composer, options: Options, report: &mut Report) {
    report.note(
        "latency method: an edit is dispatched on the GTK thread; the probe (the WebView's parent) \
         notes the first re-snapshot of the WebView after it, and the frame clock's after-paint ends \
         that frame; no key events, no input method",
    );
    fresh_small_document(composer).await;
    while composer.paints.try_recv().is_ok() {}
    let started = Instant::now();
    let idle: Vec<String> = collect_paints(&composer.paints, Duration::from_millis(2000))
        .await
        .iter()
        .map(|paint| format!("{:.0}", ms(paint.snapshot_at - started)))
        .collect();
    report.note(format!(
        "WebView repaints during 2 s idle with gtk-cursor-blink=false: {} at ms {idle:?}",
        idle.len()
    ));
    report.note(display_details(composer));
    let samples = options.samples;
    let validation = (samples / 4).clamp(10, 40);
    let small = "InsertText editing command, small document";
    if options.capture {
        typing_series(composer, small, Method::EditingCommand, validation, true)
            .await
            .print(report);
    }
    fresh_small_document(composer).await;
    let small_series = typing_series(composer, small, Method::EditingCommand, samples, false).await;
    small_series.print(report);

    fresh_small_document(composer).await;
    let exec = "execCommand('insertText') from the isolated world, small document";
    let exec_series = typing_series(composer, exec, Method::ExecCommand, samples, false).await;
    exec_series.print(report);

    let quote = large_quote();
    report.note(format!(
        "large document: {} bytes of quoted HTML; typing above the quote",
        quote.len()
    ));
    let large = "InsertText editing command, large document";
    set_content(composer, &quote).await;
    caret(composer, "Svar", 0).await;
    sleep(500).await;
    if options.capture {
        typing_series(composer, large, Method::EditingCommand, validation, true)
            .await
            .print(report);
    }
    set_content(composer, &quote).await;
    caret(composer, "Svar", 0).await;
    sleep(500).await;
    let large_series =
        typing_series(composer, large, Method::EditingCommand, samples / 2, false).await;
    large_series.print(report);

    let toolbar = toolbar_series(composer, 40).await;
    report.note(format!(
        "LATENCY caret move (isolated-world selection change) → EditorState typing-attributes notify \
         on the GTK thread (n={}): p50={} p95={} max={} ms",
        toolbar.len(),
        fmt(percentile(&toolbar, 0.50)),
        fmt(percentile(&toolbar, 0.95)),
        fmt(percentile(&toolbar, 1.0))
    ));

    for (name, series) in [
        (small, &small_series),
        (exec, &exec_series),
        (large, &large_series),
    ] {
        let p95 = percentile(&series.first_paint, 0.95);
        let name = format!(
            "edit-to-paint p95 < 16 ms, first WebView frame after the edit ({name}; synthetic \
             edits, no key events, no input method)"
        );
        if options.live {
            report.check(
                &name,
                p95.is_some_and(|p95| p95 < 16.0) && series.timeouts == 0,
                format!("p95 {} ms", fmt(p95)),
            );
        } else {
            report.note(format!(
                "{name}: p95 {} ms — not judged headless, where Broadway's own frame work \
                 (texture upload) dominates",
                fmt(p95)
            ));
        }
    }
}
