//! The user's manual check: real typing through fcitx5 with the Norwegian layout, dead keys and
//! Compose. Key presses, input-method commits and WebView frames are all timestamped on the GTK
//! thread; nothing is injected.

use crate::composer::{self, Composer, Event};
use crate::probe::Paint;
use crate::selftest::{fmt, gate, ms, percentile};
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::prelude::*;

const EXPECTED: [&str; 5] = [
    "blåbærsyltetøy på brødskiva ÆØÅ",
    "café crème naïve",
    "señor Hôtel Zürich",
    "ø æ å €",
    "The quick brown fox æøå",
];

const INSTRUCTIONS: &str = "<b>S7 manual check: fcitx5, Norwegian layout, dead keys</b>

Click in the editor on the right and type each line below, pressing <b>Enter</b> after each. \
Type at your normal speed. Every key is timed from the key press to the frame that shows it. \
The caret does not blink during the check (that keeps blink repaints out of the timings).

<b>1. æ ø å keys</b>
<tt>blåbærsyltetøy på brødskiva ÆØÅ</tt>

<b>2. Dead keys</b>
<tt>café crème naïve</tt>
é = AltGr + the key left of Backspace (´), then e
è = Shift + that key (`), then e
ï = the key right of Å (¨), then i

<b>3. More dead keys</b>
<tt>señor Hôtel Zürich</tt>
ñ = AltGr + ¨, then n   ô = Shift + ¨ (^), then o   ü = ¨, then u

<b>4. Compose</b> (Caps Lock is Compose on Omarchy)
<tt>ø æ å €</tt>
ø = Caps, o, /   æ = Caps, a, e   å = Caps, a, a   € = Caps, =, e

<b>5. US layout</b> (if you use one: switch fcitx5 to it first)
<tt>The quick brown fox æøå</tt>
(æøå through Compose)

<b>6. Optional</b>: select a word, press Ctrl+B, Ctrl+I, Ctrl+U, then Ctrl+Z.

Press <b>Finish</b> when done, or wait for the timer. The results print in the terminal.";

#[derive(Clone)]
struct KeyPress {
    at: Instant,
    name: String,
    text: Option<char>,
    control: bool,
    shift: bool,
    /// WebKit did not handle the key and dispatched it again to the toplevel.
    unhandled: bool,
}

#[derive(Default)]
struct Log {
    keys: Vec<KeyPress>,
    commits: Vec<(Instant, String)>,
    preedits: Vec<(Instant, &'static str)>,
    paints: Vec<Paint>,
    inputs: Vec<(Instant, String, String, bool)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Ascii,
    NonAscii,
    Dead,
    DeadCompletion,
    ComposeKey,
    ComposeStep,
    Shortcut,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Ascii => "ASCII keys",
            Kind::NonAscii => "æ/ø/å and other non-ASCII keys",
            Kind::Dead => "dead keys themselves",
            Kind::DeadCompletion => "keys completing a dead key",
            Kind::ComposeKey => "Compose (Caps Lock)",
            Kind::ComposeStep => "keys inside a Compose sequence",
            Kind::Shortcut => "Ctrl shortcuts",
        }
    }
}

fn is_modifier(name: &str) -> bool {
    matches!(
        name,
        "Shift_L"
            | "Shift_R"
            | "Control_L"
            | "Control_R"
            | "Alt_L"
            | "Alt_R"
            | "Meta_L"
            | "Meta_R"
            | "Super_L"
            | "Super_R"
            | "Hyper_L"
            | "Hyper_R"
            | "ISO_Level3_Shift"
            | "ISO_Level3_Latch"
            | "ISO_Level5_Shift"
            | "Caps_Lock"
            | "Num_Lock"
    )
}

struct Measured {
    kind: Kind,
    latency: Option<f64>,
}

fn classify(keys: &[KeyPress]) -> Vec<(usize, Kind)> {
    let mut out = Vec::new();
    let mut previous_dead = false;
    let mut compose_left = 0;
    for (index, key) in keys.iter().enumerate() {
        if is_modifier(&key.name) {
            continue;
        }
        let kind = if key.control {
            Kind::Shortcut
        } else if key.name == "Multi_key" {
            compose_left = 2;
            Kind::ComposeKey
        } else if key.name.starts_with("dead_") {
            Kind::Dead
        } else if compose_left > 0 {
            compose_left -= 1;
            Kind::ComposeStep
        } else if previous_dead {
            Kind::DeadCompletion
        } else if key.text.is_some_and(|ch| !ch.is_ascii()) {
            Kind::NonAscii
        } else {
            Kind::Ascii
        };
        previous_dead = kind == Kind::Dead;
        out.push((index, kind));
    }
    out
}

fn first_paint_between(paints: &[Paint], from: Instant, until: Instant) -> Option<&Paint> {
    paints
        .iter()
        .find(|paint| paint.snapshot_at > from && paint.snapshot_at < until)
}

fn measure(log: &Log) -> Vec<Measured> {
    let classified = classify(&log.keys);
    let mut measured = Vec::new();
    for (position, (index, kind)) in classified.iter().enumerate() {
        let key = &log.keys[*index];
        let next = classified
            .get(position + 1)
            .map(|(next, _)| log.keys[*next].at)
            .unwrap_or(key.at + Duration::from_millis(250));
        let limit = next.min(key.at + Duration::from_millis(250));
        let latency = first_paint_between(&log.paints, key.at, limit)
            .map(|paint| ms(paint.painted_at - key.at));
        measured.push(Measured {
            kind: *kind,
            latency,
        });
    }
    measured
}

fn stats(values: &[f64]) -> String {
    format!(
        "n={} p50={} p95={} max={} ms",
        values.len(),
        fmt(percentile(values, 0.50)),
        fmt(percentile(values, 0.95)),
        fmt(percentile(values, 1.0))
    )
}

/// The editing `input` event a shortcut caused, if any (formatBold, historyUndo, …).
fn shortcut_effect(log: &Log, key: &KeyPress) -> String {
    let next = log
        .keys
        .iter()
        .find(|other| other.at > key.at && !is_modifier(&other.name))
        .map_or(key.at + Duration::from_millis(150), |other| other.at);
    let window = next.min(key.at + Duration::from_millis(150));
    log.inputs
        .iter()
        .find(|(at, kind, _, _)| *at > key.at && *at < window && kind != "insertText")
        .map_or_else(
            || "no editing input event".to_string(),
            |(_, kind, _, _)| format!("input event {kind}"),
        )
}

fn summary(log: &Log, text: &str, details: &str, seconds: u64) -> String {
    let mut out = String::new();
    let measured = measure(log);
    let _ = writeln!(
        out,
        "S7 manual check — {} build, {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        if crate::broadway::is_broadway() {
            "headless Broadway (smoke run)"
        } else {
            "live Wayland session"
        }
    );
    let _ = writeln!(
        out,
        "Environment: GTK_IM_MODULE={:?} INPUT_METHOD={:?} XMODIFIERS={:?}; fcitx5 current input method: {}",
        std::env::var("GTK_IM_MODULE").ok(),
        std::env::var("INPUT_METHOD").ok(),
        std::env::var("XMODIFIERS").ok(),
        fcitx5_input_method()
    );
    let _ = writeln!(out, "Display: {details}; time limit {seconds} s");
    let painted: Vec<f64> = measured
        .iter()
        .filter(|item| !matches!(item.kind, Kind::Dead | Kind::ComposeKey | Kind::ComposeStep))
        .filter_map(|item| item.latency)
        .collect();
    let _ = writeln!(
        out,
        "Key press → painted WebView frame, every key that produces text: {}",
        stats(&painted)
    );
    for kind in [
        Kind::Ascii,
        Kind::NonAscii,
        Kind::DeadCompletion,
        Kind::Dead,
        Kind::ComposeKey,
        Kind::ComposeStep,
        Kind::Shortcut,
    ] {
        let of_kind: Vec<&Measured> = measured.iter().filter(|item| item.kind == kind).collect();
        if of_kind.is_empty() {
            continue;
        }
        let values: Vec<f64> = of_kind.iter().filter_map(|item| item.latency).collect();
        let _ = writeln!(
            out,
            "  {}: {} keys, {} with a frame before the next key: {}",
            kind.label(),
            of_kind.len(),
            values.len(),
            stats(&values)
        );
    }
    let commit_latencies: Vec<f64> = log
        .commits
        .iter()
        .filter_map(|(at, _)| {
            first_paint_between(&log.paints, *at, *at + Duration::from_millis(250))
                .map(|paint| ms(paint.painted_at - *at))
        })
        .collect();
    let committed: Vec<&str> = log.commits.iter().map(|(_, text)| text.as_str()).collect();
    let _ = writeln!(
        out,
        "Input-method commits (WebKit's input method context): {}; commit → painted frame: {}",
        log.commits.len(),
        stats(&commit_latencies)
    );
    let _ = writeln!(out, "  committed text: {committed:?}");
    let _ = writeln!(
        out,
        "Preedit events: {} (started/changed/finished)",
        log.preedits.len()
    );
    let composing = log.inputs.iter().filter(|input| input.3).count();
    let mut input_types: Vec<String> = log.inputs.iter().map(|input| input.1.clone()).collect();
    input_types.sort();
    input_types.dedup();
    let _ = writeln!(
        out,
        "DOM input events: {} ({} while composing); types {input_types:?}",
        log.inputs.len(),
        composing
    );
    for key in log
        .keys
        .iter()
        .filter(|key| key.control && !is_modifier(&key.name))
    {
        let _ = writeln!(
            out,
            "  Ctrl+{}{}: {}; {}",
            if key.shift { "Shift+" } else { "" },
            key.name,
            if key.unhandled {
                "WebKit did not handle it (re-dispatched to the window)"
            } else {
                "handled by WebKit"
            },
            shortcut_effect(log, key)
        );
    }
    let lines: Vec<String> = text.lines().map(|line| line.trim().to_string()).collect();
    for (index, expected) in EXPECTED.iter().enumerate() {
        let found = lines.iter().any(|line| line == expected);
        let _ = writeln!(
            out,
            "Line {}: {} ({expected:?})",
            index + 1,
            if found { "typed exactly" } else { "NOT found" }
        );
    }
    let verdict = percentile(&painted, 0.95).is_some_and(|p95| p95 < 16.0);
    let _ = writeln!(
        out,
        "Verdict: key press → painted frame p95 < 16 ms: {}",
        if painted.is_empty() {
            "NO DATA"
        } else if verdict {
            "PASS"
        } else {
            "FAIL"
        }
    );
    out
}

fn detail(log: &Log, text: &str) -> String {
    let mut out = String::new();
    let Some(origin) = log.keys.first().map(|key| key.at) else {
        return out;
    };
    let _ = writeln!(
        out,
        "\nPer key (ms from the first key; latency to the painted frame):"
    );
    let measured = measure(log);
    let classified = classify(&log.keys);
    for (index, key) in log.keys.iter().enumerate() {
        let item = classified
            .iter()
            .position(|(classified, _)| *classified == index)
            .and_then(|position| measured.get(position));
        let (kind, latency) = match item {
            Some(item) => (
                item.kind.label(),
                item.latency.map_or_else(
                    || "no frame before the next key".to_string(),
                    |latency| format!("{latency:.2}"),
                ),
            ),
            None => ("modifier", String::new()),
        };
        let _ = writeln!(
            out,
            "  {:>9.1} {:<20} {:<32} {latency}{}",
            ms(key.at - origin),
            key.name,
            kind,
            if key.unhandled {
                " (unhandled by WebKit)"
            } else {
                ""
            }
        );
    }
    let _ = writeln!(out, "\nInput-method commits:");
    for (at, committed) in &log.commits {
        let _ = writeln!(
            out,
            "  {:>9.1} {committed:?}",
            ms(at.saturating_duration_since(origin))
        );
    }
    let _ = writeln!(out, "\nEditor text at the end:\n{text}");
    out
}

fn fcitx5_input_method() -> String {
    std::process::Command::new("fcitx5-remote")
        .arg("-n")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown (fcitx5-remote failed or is missing)".into())
}

pub fn run(args: &[String]) -> ExitCode {
    if let Err(message) = gate() {
        eprintln!("{message}");
        return ExitCode::from(2);
    }
    let mut seconds = 300;
    let mut auto = false;
    let mut accelerated = true;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--auto" {
            auto = true;
            continue;
        }
        if arg == "--no-acceleration" {
            accelerated = false;
            continue;
        }
        let value = args.next().and_then(|value| value.parse::<u64>().ok());
        match (arg.as_str(), value) {
            ("--minutes", Some(minutes)) => seconds = minutes * 60,
            ("--seconds", Some(value)) => seconds = value,
            _ => {
                eprintln!("usage: email-spike-s7 manual [--minutes N] [--no-acceleration]");
                return ExitCode::from(2);
            }
        }
    }
    let keyboard = if crate::broadway::is_broadway() {
        match crate::broadway::connect() {
            Ok(keyboard) => Some(keyboard),
            Err(error) => {
                eprintln!("could not connect the Broadway web client: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    if auto && keyboard.is_none() {
        eprintln!("--auto types through the private Broadway display only; it never runs live");
        return ExitCode::from(2);
    }
    let keyboard = keyboard.filter(|_| auto);
    let app = adw::Application::builder()
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        if let Some(settings) = gtk4::Settings::default() {
            settings.set_gtk_cursor_blink(false);
        }
        let app = app.clone();
        let hold = app.hold();
        let keyboard = keyboard.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Err(error) = manual(&app, seconds, keyboard, accelerated).await {
                eprintln!("manual check failed: {error}");
            }
            drop(hold);
            app.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
    ExitCode::SUCCESS
}

const SHIFT: u32 = 1;
const CONTROL: u32 = 4;

/// The instruction lines as X keysyms, the way the Norwegian layout produces them: dead keys
/// and Compose (`Multi_key`) are separate key presses, so GTK's input method composes them.
const SCRIPT: [&str; 5] = [
    "blåbærsyltetøy på brødskiva ÆØÅ",
    "caf{dead_acute}e cr{dead_grave}eme na{dead_diaeresis}ive",
    "se{dead_tilde}nor H{dead_circumflex}otel Z{dead_diaeresis}urich",
    "{Multi_key}o/ {Multi_key}ae {Multi_key}aa {Multi_key}=e",
    "The quick brown fox {Multi_key}ae{Multi_key}o/{Multi_key}aa",
];

fn named_keysym(name: &str) -> Option<u32> {
    Some(match name {
        "dead_grave" => 0xfe50,
        "dead_acute" => 0xfe51,
        "dead_circumflex" => 0xfe52,
        "dead_tilde" => 0xfe53,
        "dead_diaeresis" => 0xfe57,
        "Multi_key" => 0xff20,
        "Return" => 0xff0d,
        "Left" => 0xff51,
        "Shift_L" => 0xffe1,
        "Control_L" => 0xffe3,
        _ => return None,
    })
}

/// Presses and releases: (keysym, modifier state, pressed).
fn script_events() -> Vec<(u32, u32, bool)> {
    let mut events = Vec::new();
    let tap = |events: &mut Vec<(u32, u32, bool)>, keysym: u32, state: u32| {
        events.push((keysym, state, true));
        events.push((keysym, state, false));
    };
    for line in SCRIPT {
        let mut rest = line;
        while let Some(ch) = rest.chars().next() {
            if ch == '{'
                && let Some(end) = rest.find('}')
                && let Some(keysym) = named_keysym(&rest[1..end])
            {
                tap(&mut events, keysym, 0);
                rest = &rest[end + 1..];
                continue;
            }
            rest = &rest[ch.len_utf8()..];
            let keysym = ch as u32;
            if ch.is_uppercase() {
                events.push((0xffe1, 0, true));
                tap(&mut events, keysym, SHIFT);
                events.push((0xffe1, SHIFT, false));
            } else {
                tap(&mut events, keysym, 0);
            }
        }
        tap(&mut events, 0xff0d, 0);
    }
    for ch in "fet".chars() {
        tap(&mut events, ch as u32, 0);
    }
    events.push((0xffe3, 0, true));
    events.push((0xffe1, CONTROL, true));
    tap(&mut events, 0xff51, CONTROL | SHIFT);
    events.push((0xffe1, CONTROL | SHIFT, false));
    for key in *b"biuz" {
        tap(&mut events, u32::from(key), CONTROL);
    }
    events.push((0xffe1, CONTROL, true));
    tap(&mut events, u32::from(b'Z'), CONTROL | SHIFT);
    events.push((0xffe1, CONTROL | SHIFT, false));
    tap(&mut events, u32::from(b'y'), CONTROL);
    tap(&mut events, u32::from(b'a'), CONTROL);
    events.push((0xffe3, CONTROL, false));
    events
}

async fn type_script(keyboard: &crate::broadway::Keyboard) -> Result<(), String> {
    for (keysym, state, pressed) in script_events() {
        keyboard
            .key(keysym, state, pressed)
            .map_err(|error| error.to_string())?;
        glib::timeout_future(Duration::from_millis(if pressed { 15 } else { 60 })).await;
    }
    Ok(())
}

async fn manual(
    app: &adw::Application,
    seconds: u64,
    keyboard: Option<crate::broadway::Keyboard>,
    accelerated: bool,
) -> Result<(), String> {
    let filter = composer::compile_filter().await?;
    let session = composer::ephemeral_session();
    let composer = Composer::new(&filter, &session, accelerated);
    let log = Rc::new(RefCell::new(Log::default()));

    let instructions = gtk4::Label::builder()
        .label(INSTRUCTIONS)
        .use_markup(true)
        .wrap(true)
        .xalign(0.0)
        .yalign(0.0)
        .selectable(false)
        .build();
    let status = gtk4::Label::builder().xalign(0.0).wrap(true).build();
    let finish = gtk4::Button::with_label("Finish");
    finish.add_css_class("suggested-action");
    let panel = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_start(16)
        .margin_end(16)
        .margin_top(16)
        .margin_bottom(16)
        .width_request(420)
        .build();
    panel.append(&instructions);
    panel.append(&status);
    panel.append(&finish);
    let scrolled = gtk4::ScrolledWindow::builder()
        .child(&panel)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .build();
    let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    content.append(&scrolled);
    content.append(&gtk4::Separator::new(gtk4::Orientation::Vertical));
    content.append(&composer.root);
    composer.root.set_hexpand(true);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("S7 manual typing check")
        .default_width(1400)
        .default_height(900)
        .content(&content)
        .build();

    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let key_log = log.clone();
    keys.connect_key_pressed(move |_, keyval, _, state| {
        let now = Instant::now();
        let name = keyval
            .name()
            .map(|name| name.to_string())
            .unwrap_or_default();
        let control = state.contains(gdk::ModifierType::CONTROL_MASK);
        let mut log = key_log.borrow_mut();
        let start = log.keys.len().saturating_sub(4);
        if let Some(earlier) = log.keys[start..].iter_mut().rev().find(|earlier| {
            earlier.name == name
                && earlier.control == control
                && now - earlier.at < Duration::from_millis(5)
        }) {
            earlier.unhandled = true;
            return glib::Propagation::Proceed;
        }
        log.keys.push(KeyPress {
            at: now,
            name,
            text: keyval.to_unicode(),
            control,
            shift: state.contains(gdk::ModifierType::SHIFT_MASK),
            unhandled: false,
        });
        glib::Propagation::Proceed
    });
    window.add_controller(keys);
    if let Some(context) = composer.view.input_method_context() {
        let commits = log.clone();
        context.connect_committed(move |_, text| {
            commits
                .borrow_mut()
                .commits
                .push((Instant::now(), text.to_string()));
        });
        for (signal, label) in [
            ("preedit-started", "started"),
            ("preedit-changed", "changed"),
            ("preedit-finished", "finished"),
        ] {
            let preedits = log.clone();
            context.connect_local(signal, false, move |_| {
                preedits.borrow_mut().preedits.push((Instant::now(), label));
                None
            });
        }
    }
    window.present();

    let ready = glib::future_with_timeout(Duration::from_secs(20), async {
        loop {
            match composer.events.recv().await {
                Ok(Event::Ready { .. }) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    if !ready {
        return Err("the editor did not load".into());
    }
    let _ = composer.eval("mailComposer.reportInput(true)").await;
    composer.view.grab_focus();

    let paints = composer.paints.clone();
    let paint_log = log.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(paint) = paints.recv().await {
            paint_log.borrow_mut().paints.push(paint);
        }
    });
    let events = composer.events.clone();
    let input_log = log.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = events.recv().await {
            if let Event::Input {
                input_type,
                data,
                composing,
                ..
            } = event
            {
                input_log
                    .borrow_mut()
                    .inputs
                    .push((Instant::now(), input_type, data, composing));
            }
        }
    });

    let (done_sender, done) = async_channel::bounded::<()>(1);
    let sender = done_sender.clone();
    finish.connect_clicked(move |_| {
        let _ = sender.try_send(());
    });
    let sender = done_sender.clone();
    window.connect_close_request(move |_| {
        let _ = sender.try_send(());
        glib::Propagation::Stop
    });
    if let Some(keyboard) = keyboard {
        let sender = done_sender.clone();
        glib::MainContext::default().spawn_local(async move {
            glib::timeout_future(Duration::from_millis(500)).await;
            if let Err(error) = type_script(&keyboard).await {
                eprintln!("scripted typing failed: {error}");
            }
            glib::timeout_future(Duration::from_millis(800)).await;
            let _ = sender.try_send(());
        });
    }
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let status_log = log.clone();
    let status_label = status.clone();
    let ticker = glib::timeout_add_local(Duration::from_millis(500), move || {
        let log = status_log.borrow();
        let painted: Vec<f64> = measure(&log)
            .iter()
            .filter(|item| !matches!(item.kind, Kind::Dead | Kind::ComposeKey | Kind::ComposeStep))
            .filter_map(|item| item.latency)
            .collect();
        status_label.set_text(&format!(
            "{} s left · keys timed: {} · key→frame p50 {} ms, p95 {} ms · input-method commits: {}",
            deadline.saturating_duration_since(Instant::now()).as_secs(),
            painted.len(),
            fmt(percentile(&painted, 0.50)),
            fmt(percentile(&painted, 0.95)),
            log.commits.len()
        ));
        glib::ControlFlow::Continue
    });
    let _ = glib::future_with_timeout(Duration::from_secs(seconds), done.recv()).await;
    ticker.remove();

    let details = crate::selftest::display_details(&composer);
    let text = composer
        .eval("document.body.innerText")
        .await
        .map(|value| value.to_str().to_string())
        .unwrap_or_default();
    let log = log.borrow();
    let summary = summary(&log, &text, &details, seconds);
    println!("{summary}");
    let dir = composer::state_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let path = dir.join(format!("manual-check-{stamp}.txt"));
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&path, format!("{summary}{}", detail(&log, &text))));
    match written {
        Ok(()) => println!("Detailed log: {}", path.display()),
        Err(error) => eprintln!("could not write {}: {error}", path.display()),
    }
    window.destroy();
    Ok(())
}
