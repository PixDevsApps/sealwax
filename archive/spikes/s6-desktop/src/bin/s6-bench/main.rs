//! The S6 bench: builds the cached INBOX and drives the app through each measurement, reading
//! its event lines and computing latencies on the shared `CLOCK_MONOTONIC`. Live runs give the
//! app scratch XDG folders under `target/s6/xdg-live`; headless runs use the wrapper's.
//!
//! ```text
//! s6-bench build-store [--inbox 10000] [--seed 6]
//! s6-bench startup --runs N [--label L] [--cold] [--discard N] [--set K=V]… [--unset K]… [--binary P]
//! s6-bench forward [--runs 20] [--open-runs 10] [--stub-runs 10]
//! s6-bench open-uri --uri U          (a GTK-free second process: D-Bus Open, then exit)
//! s6-bench manual-smoke               (headless: the manual notification check, clicked by the fake server)
//! s6-bench notify [--fake] [--second] [--click-wait-ms N]
//! s6-bench background [--server stalwart|dovecot] [--appends 5] [--cuts 3] [--freezes 3]
//!                     [--silent-wait-s 10] [--hold-s 0] [--fake] [--user spike]
//! ```

mod fake;
mod imap;
mod runner;

use anyhow::{Context, Result, bail};
use email_spike_s1::testbed;
use email_spike_s6::cache;
use email_spike_s6::clock::{mono_ns, real_ns};
use email_spike_s6::evict::{self, Residency};
use email_spike_s6::relay::Relay;
use email_spike_s6::stats::summarize;
use email_spike_s6::{env, paths};
use runner::{Launch, Running};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

struct Args {
    command: String,
    flags: BTreeMap<String, Vec<String>>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut raw = std::env::args().skip(1);
        let command = raw.next().context("missing subcommand")?;
        let mut flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut pending: Option<String> = None;
        for item in raw {
            if let Some(name) = item.strip_prefix("--") {
                if let Some(previous) = pending.take() {
                    flags.entry(previous).or_default().push(String::new());
                }
                pending = Some(name.to_owned());
            } else if let Some(name) = pending.take() {
                flags.entry(name).or_default().push(item);
            } else {
                bail!("unexpected argument {item}");
            }
        }
        if let Some(name) = pending {
            flags.entry(name).or_default().push(String::new());
        }
        Ok(Self { command, flags })
    }

    fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .get(name)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T> {
        match self.value(name) {
            Some(text) => text
                .parse()
                .map_err(|_| anyhow::anyhow!("--{name}: not a number: {text}")),
            None => Ok(default),
        }
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }
}

fn main() -> Result<()> {
    let args = Args::parse()?;
    std::fs::create_dir_all(paths::results_dir())?;
    match args.command.as_str() {
        "build-store" => build_store(&args),
        "startup" => startup(&args),
        "forward" => forward(&args),
        "notify" => notify(&args),
        "background" => background(&args),
        "open-uri" => open_uri(&args),
        "manual-smoke" => manual_smoke(),
        other => bail!("unknown subcommand {other}"),
    }
}

fn session_note() -> String {
    format!(
        "{} (GDK_SCALE={:?}, GSK_RENDERER={:?}, GTK_A11Y={:?})",
        if runner::headless() {
            "headless (Broadway, private bus)"
        } else {
            "live session"
        },
        std::env::var("GDK_SCALE").ok(),
        std::env::var("GSK_RENDERER").ok(),
        std::env::var("GTK_A11Y").ok()
    )
}

fn results_file(name: &str) -> Result<std::fs::File> {
    let path = paths::results_dir().join(format!("{}-{name}.jsonl", real_ns() / 1_000_000_000));
    println!("raw events: {}", path.display());
    Ok(std::fs::File::create(path)?)
}

fn save(file: &mut std::fs::File, run: &str, running: &Running) -> Result<()> {
    for (event, value) in &running.events {
        writeln!(
            file,
            "{}",
            json!({ "run": run, "event": event, "value": value })
        )?;
    }
    Ok(())
}

fn build_store(args: &Args) -> Result<()> {
    let db = args
        .value("db")
        .map(PathBuf::from)
        .unwrap_or_else(paths::default_db);
    let built = cache::build(
        &db,
        args.number("inbox", 10_000usize)?,
        args.number("seed", 6u64)?,
    )?;
    println!(
        "built {}: {} messages, INBOX {}, {:.1} MiB, {:.0} ms",
        db.display(),
        built.messages,
        built.inbox,
        built.bytes as f64 / (1024.0 * 1024.0),
        built.build_ms
    );
    let mailboxes: Vec<String> = built
        .per_mailbox
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect();
    println!("mailboxes: {}", mailboxes.join(", "));
    Ok(())
}

fn ms_between(stamps: &Value, from: &str, to: &str) -> Option<f64> {
    Some((stamps.get(to)?.as_i64()? - stamps.get(from)?.as_i64()?) as f64 / 1e6)
}

const STAGES: &[(&str, &str, &str)] = &[
    ("exec → main (loader, Rust init)", "exec", "main"),
    (
        "main → dbus_register (GApplication, bus connection)",
        "main",
        "dbus-register",
    ),
    (
        "dbus_register → startup (name request)",
        "dbus-register",
        "startup",
    ),
    ("startup → gtk_init done", "startup", "gtk-init"),
    (
        "gtk_init → GtkApplication + Adw startup",
        "gtk-init",
        "adw-init",
    ),
    ("  of which adw_init()", "gtk-init", "adw-lib"),
    ("  of which the startup chain", "adw-lib", "adw-init"),
    ("Adw → theme + CSS", "adw-init", "css"),
    ("CSS → window built", "css", "window-built"),
    (
        "window built → present (rows in model)",
        "window-built",
        "present",
    ),
    ("present → realize", "present", "realize"),
    ("realize → map", "realize", "map"),
    ("map → first frame painted", "map", "first-frame"),
    (
        "first frame painted → presented",
        "first-frame",
        "presented",
    ),
    ("startup → reader connection open", "startup", "reader-open"),
    (
        "reader: first page (open_view)",
        "reader-open",
        "reader-snapshot",
    ),
    (
        "reader → GTK thread has rows",
        "reader-snapshot",
        "snapshot-received",
    ),
    ("exec → first frame painted", "exec", "first-frame"),
    ("exec → presented on screen", "exec", "presented"),
];

fn eviction_list() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = vec![paths::app_binary()];
    files.extend(cache::db_files(&paths::default_db()));
    if let Ok(maps) = std::fs::read_to_string(paths::results_dir().join("maps.txt")) {
        files.extend(evict::mapped_files(&maps));
    }
    let cache_root = paths::live_xdg().join("cache");
    let mut stack = vec![cache_root];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files.retain(|file| file.is_file());
    files
}

fn startup(args: &Args) -> Result<()> {
    let runs: usize = args.number("runs", 10)?;
    let discard: usize = args.number("discard", 1)?;
    let cold = args.has("cold");
    let label = args.value("label").unwrap_or("startup").to_owned();
    println!(
        "# startup: {label}, {runs} runs ({discard} discarded), {}, {}",
        if cold { "cold-ish" } else { "warm" },
        session_note()
    );
    if !paths::default_db().exists() {
        bail!("no cached store; run `s6-bench build-store` first");
    }
    let mut out = results_file(&format!("startup-{label}"))?;
    let mut launch = Launch::mode("startup");
    launch.set(env::EXIT_AFTER_MS, "20000");
    launch.set(
        env::MAPS_OUT,
        paths::results_dir().join("maps.txt").display().to_string(),
    );
    launch.stderr_log = Some(paths::results_dir().join(format!("{label}-stderr.log")));
    if let Some(binary) = args.value("binary") {
        launch.binary = Some(PathBuf::from(binary));
    }
    for pair in args.all("set") {
        let (key, value) = pair.split_once('=').context("--set needs KEY=VALUE")?;
        launch.set(key, value);
    }
    launch.unset = args.all("unset");
    let mut rows: Vec<Value> = Vec::new();
    let mut residency = Residency::default();
    for run in 0..runs {
        runner::ensure_not_running()?;
        let mut evicted = None;
        if cold {
            let mut total = Residency::default();
            for file in eviction_list() {
                if let Ok(one) = evict::evict(&file) {
                    total.add(one);
                }
            }
            evicted = Some(total);
            if run >= discard {
                residency.add(total);
            }
        }
        let mut running = runner::spawn(&launch)?;
        let report = running.wait("startup", Duration::from_secs(20));
        let status = running.wait_exit(Duration::from_secs(10));
        save(&mut out, &format!("{label}-{run}"), &running)?;
        let Some(report) = report else {
            println!("run {run}: no startup report ({status:?})");
            continue;
        };
        let mut stamps = report["stamps"].clone();
        if let Some(presented) = report["presented"].as_i64() {
            stamps["presented"] = presented.into();
        }
        let frame = &report["frame"];
        let total = ms_between(&stamps, "exec", "first-frame");
        let on_screen = ms_between(&stamps, "exec", "presented");
        println!(
            "run {run}: exec→painted {} ms, exec→presented {} ms, frame #{} cpu {} ms layout {} ms, rows bound {}, renderer {}, scale {}×{}{}{}",
            fmt(total),
            fmt(on_screen),
            frame["index"],
            fmt(span(frame, "start", "end")),
            fmt(span(frame, "start", "layout_end")),
            frame["bound"],
            report["renderer"].as_str().unwrap_or("?"),
            report["scale"]["surface_scale_factor"],
            report["scale"]["surface_scale"],
            evicted
                .map(|r| format!(
                    ", evicted {:.0} of {:.0} MiB resident",
                    mib(r.resident_before - r.resident_after),
                    mib(r.resident_before)
                ))
                .unwrap_or_default(),
            if run < discard { " (discarded)" } else { "" }
        );
        if let Some(next) = report["next_frames"].as_array().filter(|n| !n.is_empty()) {
            let frames: Vec<String> = next
                .iter()
                .map(|frame| {
                    format!(
                        "+{} ms: {} ms ({} binds)",
                        fmt(frame["since_first_ms"].as_f64()),
                        fmt(frame["work_ms"].as_f64()),
                        frame["binds"]
                    )
                })
                .collect();
            println!("  frames after the first: {}", frames.join(", "));
        }
        if let Some(prewarm) = running.find("prewarm").next() {
            let exec = stamps["exec"].as_i64().unwrap_or(0);
            let at = |key: &str| prewarm[key].as_i64().map(|t| (t - exec) as f64 / 1e6);
            println!(
                "  prewarm (ms after exec): started {}, loader {}, instance {}, device done {}; GTK present {}, realize {}",
                fmt(at("started")),
                fmt(at("loaded")),
                fmt(at("instance")),
                fmt(at("device")),
                fmt(ms_between(&stamps, "exec", "present")),
                fmt(ms_between(&stamps, "exec", "realize"))
            );
        }
        if run >= discard {
            rows.push(json!({ "stamps": stamps, "report": report }));
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    println!("\n| stage | p50 ms | p95 ms | min | max |\n| --- | --- | --- | --- | --- |");
    for (name, from, to) in STAGES {
        let samples: Vec<f64> = rows
            .iter()
            .filter_map(|row| ms_between(&row["stamps"], from, to))
            .collect();
        if samples.is_empty() {
            continue;
        }
        let s = summarize(&samples);
        println!(
            "| {name} | {:.1} | {:.1} | {:.1} | {:.1} |",
            s.p50, s.p95, s.min, s.max
        );
    }
    let frame_cpu: Vec<f64> = rows
        .iter()
        .filter_map(|row| span(&row["report"]["frame"], "start", "end"))
        .collect();
    let frame_layout: Vec<f64> = rows
        .iter()
        .filter_map(|row| span(&row["report"]["frame"], "start", "layout_end"))
        .collect();
    println!(
        "| first frame: GTK work (before-paint → after-paint) | {:.1} | {:.1} | {:.1} | {:.1} |",
        summarize(&frame_cpu).p50,
        summarize(&frame_cpu).p95,
        summarize(&frame_cpu).min,
        summarize(&frame_cpu).max
    );
    println!(
        "| first frame: layout part | {:.1} | {:.1} | {:.1} | {:.1} |",
        summarize(&frame_layout).p50,
        summarize(&frame_layout).p95,
        summarize(&frame_layout).min,
        summarize(&frame_layout).max
    );
    if let Some(first) = rows.first() {
        let report = &first["report"];
        println!(
            "\nfirst kept run: renderer {}, window {}, list {}, rows bound {}, frame index {}, scale {}, memory {}",
            report["renderer"],
            report["scale"]["window"],
            report["scale"]["list"],
            report["frame"]["bound"],
            report["frame"]["index"],
            report["scale"],
            report["memory"]
        );
    }
    if cold {
        println!(
            "eviction over kept runs: {:.0} MiB resident before, {:.0} MiB after (files mapped by other processes stay resident)",
            mib(residency.resident_before) / rows.len().max(1) as f64,
            mib(residency.resident_after) / rows.len().max(1) as f64
        );
    }
    Ok(())
}

fn span(value: &Value, from: &str, to: &str) -> Option<f64> {
    Some((value.get(to)?.as_i64()? - value.get(from)?.as_i64()?) as f64 / 1e6)
}

fn fmt(value: Option<f64>) -> String {
    value.map_or("n/a".into(), |v| format!("{v:.1}"))
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn forward(args: &Args) -> Result<()> {
    let runs: usize = args.number("runs", 20)?;
    let open_runs: usize = args.number("open-runs", 10)?;
    println!(
        "# forward: {runs} second instances, {open_runs} D-Bus Open calls, {}",
        session_note()
    );
    runner::ensure_not_running()?;
    let mut out = results_file("forward")?;
    let mut launch = Launch::mode("forward");
    launch.set(env::EXIT_AFTER_MS, "55000");
    launch.stderr_log = Some(paths::results_dir().join("forward-stderr.log"));
    let mut primary = runner::spawn(&launch)?;
    primary
        .wait("ready", Duration::from_secs(15))
        .context("the primary never became ready")?;
    let mut parsed_ms = Vec::new();
    let mut exit_ms = Vec::new();
    let mut remote_main = Vec::new();
    let mut remote_registered = Vec::new();
    let mut handoff = Vec::new();
    let mut ignored_seen = Vec::new();
    for run in 0..runs {
        let uri = if run % 4 == 3 {
            "mailto:test@example.com?subject=Hi&attach=%2Fetc%2Fpasswd&attachment=~%2F.ssh%2Fid_ed25519"
        } else {
            "mailto:test@example.com?subject=Hi"
        };
        let second = Launch {
            args: vec![uri.to_owned()],
            stderr_log: Some(paths::results_dir().join("forward-stderr.log")),
            ..Launch::default()
        };
        let mut remote = runner::spawn(&second)?;
        let started = remote.spawned;
        let status = remote.wait_exit(Duration::from_secs(10))?;
        let exited = mono_ns();
        let parsed = primary.wait_for(Duration::from_secs(3), |event, value| {
            event == "mailto" && value["sent"].as_i64() == Some(started)
        });
        save(&mut out, &format!("remote-{run}"), &remote)?;
        let remote_event = remote.find("remote").next().cloned();
        let Some(parsed) = parsed else {
            println!("run {run}: the primary never reported the mailto (remote {status})");
            continue;
        };
        let to_parsed = (parsed["parsed"].as_i64().unwrap_or(0) - started) as f64 / 1e6;
        parsed_ms.push(to_parsed);
        exit_ms.push((exited - started) as f64 / 1e6);
        if let Some(remote_event) = &remote_event {
            if let Some(main) = remote_event["main"].as_i64() {
                remote_main.push((main - started) as f64 / 1e6);
            }
            if let (Some(main), Some(registered)) = (
                remote_event["main"].as_i64(),
                remote_event["registered"].as_i64(),
            ) {
                remote_registered.push((registered - main) as f64 / 1e6);
            }
            if let (Some(registered), Some(received)) = (
                remote_event["registered"].as_i64(),
                parsed["received"].as_i64(),
            ) {
                handoff.push((received - registered) as f64 / 1e6);
            }
        }
        if run % 4 == 3 {
            ignored_seen.push(parsed["ignored"].clone());
        }
        println!(
            "run {run}: exec → primary parsed {to_parsed:.1} ms, exec → second process exited {:.1} ms, remote {status}, ignored {}",
            (exited - started) as f64 / 1e6,
            parsed["ignored"]
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    let mut open_ms = Vec::new();
    let mut open_reply_ms = Vec::new();
    for run in 0..open_runs {
        let (sent, replied) = runner::dbus_open("mailto:test@example.com?subject=Hi")?;
        let parsed = primary.wait_for(Duration::from_secs(3), |event, value| {
            event == "mailto" && value["sent"].as_i64() == Some(sent)
        });
        if let Some(parsed) = parsed {
            let value = (parsed["parsed"].as_i64().unwrap_or(0) - sent) as f64 / 1e6;
            open_ms.push(value);
            open_reply_ms.push((replied - sent) as f64 / 1e6);
            println!(
                "open {run}: call → parsed {value:.2} ms, call → reply {:.2} ms",
                (replied - sent) as f64 / 1e6
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let stub_runs: usize = args.number("stub-runs", 10)?;
    let mut stub_ms = Vec::new();
    let mut stub_main = Vec::new();
    for run in 0..stub_runs {
        let stub = Launch {
            binary: Some(std::env::current_exe()?),
            args: vec![
                "open-uri".into(),
                "--uri".into(),
                "mailto:test@example.com?subject=Hi".into(),
            ],
            ..Launch::default()
        };
        let mut process = runner::spawn(&stub)?;
        let started = process.spawned;
        let status = process.wait_exit(Duration::from_secs(10))?;
        let parsed = primary.wait_for(Duration::from_secs(3), |event, value| {
            event == "mailto" && value["sent"].as_i64() == Some(started)
        });
        save(&mut out, &format!("stub-{run}"), &process)?;
        if let Some(parsed) = parsed {
            let value = (parsed["parsed"].as_i64().unwrap_or(0) - started) as f64 / 1e6;
            stub_ms.push(value);
            if let Some(main) = process.find("stub").next().and_then(|v| v["main"].as_i64()) {
                stub_main.push((main - started) as f64 / 1e6);
            }
            println!("stub {run}: process start → primary parsed {value:.1} ms ({status})");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let quit_sent = runner::activate_action("quit")?;
    let status = primary.wait_exit(Duration::from_secs(10))?;
    println!(
        "primary quit {:.1} ms after the quit action, {status}",
        (mono_ns() - quit_sent) as f64 / 1e6
    );
    save(&mut out, "primary", &primary)?;
    println!(
        "\n| measure | n | p50 ms | p95 ms | min | max |\n| --- | --- | --- | --- | --- | --- |"
    );
    for (name, samples) in [
        (
            "second process start → primary parsed the mailto",
            &parsed_ms,
        ),
        ("second process start → second process exited", &exit_ms),
        ("second process start → its main()", &remote_main),
        (
            "second process main() → registered (found the primary)",
            &remote_registered,
        ),
        (
            "second process registered → primary received the command line",
            &handoff,
        ),
        ("D-Bus Open call → primary parsed", &open_ms),
        ("D-Bus Open call → reply", &open_reply_ms),
        ("GTK-free stub process start → primary parsed", &stub_ms),
        ("GTK-free stub process start → its main()", &stub_main),
    ] {
        let s = summarize(samples);
        println!(
            "| {name} | {} | {:.1} | {:.1} | {:.1} | {:.1} |",
            s.n, s.p50, s.p95, s.min, s.max
        );
    }
    println!("attach variants: ignored fields reported by the primary: {ignored_seen:?}");
    Ok(())
}

/// What a launcher without GTK would do: one `org.freedesktop.Application.Open` call. The
/// process start time comes from the parent through `MAIL_S6_EXEC_NS`, like the app's.
fn open_uri(args: &Args) -> Result<()> {
    let uri = args.value("uri").context("--uri")?;
    let started = std::env::var(env::EXEC_NS)
        .ok()
        .and_then(|value| value.parse::<i64>().ok());
    let main = mono_ns();
    let (_, replied) = runner::dbus_open_from(uri, started.unwrap_or(main))?;
    println!(
        "{}",
        email_spike_s6::events::line(
            "stub",
            json!({ "exec": started, "main": main, "replied": replied })
        )
    );
    Ok(())
}

/// Runs the user's notification check headless with short waits; the fake server clicks both
/// notifications, so the whole flow and its summary can be checked without a person.
fn manual_smoke() -> Result<()> {
    println!("# manual-notify smoke test, {}", session_note());
    runner::ensure_not_running()?;
    let server = fake::start(fake::Clicks::AllDefault)?;
    let mut launch = Launch::mode("manual-notify");
    launch.set(env::EXIT_AFTER_MS, "60000");
    launch.set("MAIL_S6_MANUAL_SCALE", "0.05");
    let mut out = results_file("manual-smoke")?;
    let mut running = runner::spawn(&launch)?;
    let summary = running.wait("manual-summary", Duration::from_secs(60));
    let status = running.wait_exit(Duration::from_secs(15));
    save(&mut out, "manual-smoke", &running)?;
    println!("summary: {}", summary.unwrap_or(Value::Null));
    println!("fake server clicks: {:?}", server.clicks());
    println!("app exit: {status:?}");
    Ok(())
}

fn notify(args: &Args) -> Result<()> {
    let fake = args.has("fake");
    println!(
        "# notify: {}, {}",
        if fake {
            "fake server"
        } else {
            "the session's server"
        },
        session_note()
    );
    runner::ensure_not_running()?;
    let server = if fake {
        Some(fake::start(fake::Clicks::DefaultThenButton)?)
    } else {
        None
    };
    let mut out = results_file(if fake { "notify-fake" } else { "notify-live" })?;
    let mut launch = Launch::mode("notify");
    launch.set(env::EXIT_AFTER_MS, "40000");
    launch.set(
        "MAIL_S6_CLICK_WAIT_MS",
        args.value("click-wait-ms")
            .unwrap_or(if fake { "4000" } else { "6000" }),
    );
    if args.has("second") {
        launch.set("MAIL_S6_SECOND_NOTIFICATION", "1");
    }
    launch.stderr_log = Some(paths::results_dir().join("notify-stderr.log"));
    let mut running = runner::spawn(&launch)?;
    let summary = running.wait("notify-summary", Duration::from_secs(40));
    let status = running.wait_exit(Duration::from_secs(10));
    save(&mut out, "notify", &running)?;
    for name in [
        "server-info",
        "notification-sent",
        "dbus-notify",
        "dbus-reply",
        "dbus-activation-token",
        "dbus-action-invoked",
        "platform-data",
        "message-shown",
        "button-action",
        "dbus-close",
        "dbus-closed",
        "window-active",
    ] {
        for value in running.find(name) {
            println!("{name}: {value}");
        }
    }
    if let Some(summary) = &summary {
        for sent in summary["sent"].as_array().into_iter().flatten() {
            let accepted = span(sent, "sent", "accepted");
            println!(
                "notification {}: id {}, send → server reply (accepted) {} ms, action {}, closed {}",
                sent["key"],
                sent["id"],
                fmt(accepted),
                sent["action"],
                sent["closed"]
            );
        }
    }
    for shown in running.find("message-shown") {
        println!(
            "click: ActionInvoked → action handler {} ms, action → message frame painted {} ms, server token used: {}",
            fmt(span(shown, "invoked_signal", "action")),
            fmt(span(shown, "action", "shown_frame_end")),
            shown["server_token_used"]
        );
    }
    if let Some(server) = server {
        for received in server.received() {
            println!(
                "fake server received id {} at {}: app_name {:?}, summary {:?}, actions {:?}, hints {:?}",
                received.id,
                received.t,
                received.app_name,
                received.summary,
                received.actions,
                received.hints
            );
        }
        for (id, key, t) in server.clicks() {
            println!("fake server clicked id {id} action {key:?} at {t}");
        }
    }
    println!("app exit: {status:?}");
    Ok(())
}

fn background(args: &Args) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(background_async(args))
}

fn wait_event(
    running: &mut Running,
    timeout: Duration,
    matches: impl Fn(&str, &Value) -> bool,
) -> Option<Value> {
    running.wait_for(timeout, matches)
}

async fn background_async(args: &Args) -> Result<()> {
    let server_name = args.value("server").unwrap_or("stalwart").to_owned();
    let server = testbed::server(&server_name).context("unknown server")?;
    let user = args.value("user").unwrap_or("spike").to_owned();
    let appends: usize = args.number("appends", 5)?;
    let cuts: usize = args.number("cuts", 3)?;
    let freezes: usize = args.number("freezes", 3)?;
    let silent_wait: u64 = args.number("silent-wait-s", 10)?;
    let hold: u64 = args.number("hold-s", 0)?;
    let fake = args.has("fake");
    println!(
        "# background: {server_name}, user {user}, {appends} appends, {cuts} cuts, {freezes} freezes, {}, {}",
        if fake {
            "fake notification server"
        } else {
            "the session's notification server"
        },
        session_note()
    );
    runner::ensure_not_running()?;
    let notifications = if fake {
        Some(fake::start(fake::Clicks::None)?)
    } else {
        None
    };
    let mut appender = imap::Appender::connect(server, &user).await?;
    let removed = appender.empty_inbox().await?;
    println!("emptied INBOX of {user} on {server_name} ({removed} messages)");
    let relay = Relay::start(format!("127.0.0.1:{}", server.tls_port).parse()?).await?;
    let mut out = results_file(&format!("background-{server_name}"))?;
    let mut launch = Launch::mode("background");
    let exit_after = if fake { 170_000 } else { 55_000 } + hold * 1000;
    launch.set(env::EXIT_AFTER_MS, exit_after.to_string());
    launch.set(env::IMAP_PORT, relay.addr.port().to_string());
    launch.set(env::IMAP_SERVER, server_name.clone());
    launch.set(env::IMAP_USER, user.clone());
    launch.stderr_log = Some(paths::results_dir().join("background-stderr.log"));
    let mut running = runner::spawn(&launch)?;
    let run = real_ns() / 1_000_000;
    let started = running.spawned;
    let first = running
        .wait("startup", Duration::from_secs(20))
        .context("no first frame")?;
    let idling = running.seen_or_wait("imap-idling", Duration::from_secs(20));
    let closed = running.seen_or_wait("window-closed", Duration::from_secs(5));
    let Some(idling) = idling else {
        save(&mut out, "background", &running)?;
        bail!("never reached IDLE");
    };
    println!(
        "first frame {:.0} ms after start; IDLE {:.0} ms after start; window closed into background: {}",
        (first["stamps"]["first-frame"].as_i64().unwrap_or(0) - started) as f64 / 1e6,
        (idling["t"].as_i64().unwrap_or(0) - started) as f64 / 1e6,
        closed.map_or("no".into(), |c| c["background"].to_string())
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut deliveries = Vec::new();
    let mut deliver = |label: String, appended: i64, message_id: String, running: &mut Running| {
        let new_mail = wait_event(running, Duration::from_secs(15), |event, value| {
            event == "new-mail" && value["message_id"].as_str() == Some(message_id.as_str())
        });
        let Some(new_mail) = new_mail else {
            println!("{label}: no new-mail event for {message_id}");
            return;
        };
        let key = format!("mail-{}", new_mail["uid"]);
        let sent = wait_event(running, Duration::from_secs(5), |event, value| {
            event == "notification-sent" && value["key"].as_str() == Some(key.as_str())
        });
        let reply = wait_event(running, Duration::from_secs(5), |event, value| {
            event == "dbus-reply" && value["key"].as_str() == Some(key.as_str())
        });
        let at = |value: &Option<Value>| value.as_ref().and_then(|v| v["t"].as_i64());
        let ms = |t: Option<i64>| t.map(|t| (t - appended) as f64 / 1e6);
        let row = json!({
            "label": label,
            "new_mail": ms(new_mail["t"].as_i64()),
            "sent": ms(at(&sent)),
            "accepted": ms(at(&reply)),
            "reply_error": reply.as_ref().map(|r| r["error"].clone()),
        });
        println!(
            "{label}: APPEND OK → new mail {} ms → notification sent {} ms → server accepted {} ms",
            fmt(row["new_mail"].as_f64()),
            fmt(row["sent"].as_f64()),
            fmt(row["accepted"].as_f64())
        );
        deliveries.push(row);
    };
    for index in 0..appends {
        let message_id = format!("s6.{run}.{index}@example.test");
        let appended = appender.append(&message_id).await?;
        deliver(
            format!("append {index}"),
            appended,
            message_id,
            &mut running,
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    let mut reconnects = Vec::new();
    for index in 0..cuts {
        let cut_at = mono_ns();
        let cut = relay.cut_all();
        let reconnected = wait_event(&mut running, Duration::from_secs(20), |event, value| {
            event == "reconnected" && value["idling"].as_i64().is_some_and(|t| t > cut_at)
        });
        let detected = running
            .find("imap-disconnected")
            .filter_map(|v| v["t"].as_i64())
            .find(|t| *t >= cut_at);
        let at = |key: &str| {
            reconnected
                .as_ref()
                .and_then(|v| v[key].as_i64())
                .map(|t| (t - cut_at) as f64 / 1e6)
        };
        println!(
            "cut {index} ({cut} connections): detected {} ms, connected {} ms, synced {} ms, idling {} ms after the cut",
            fmt(detected.map(|t| (t - cut_at) as f64 / 1e6)),
            fmt(at("connected")),
            fmt(at("synced")),
            fmt(at("idling"))
        );
        reconnects.push(json!({ "kind": "cut", "detected": detected.map(|t| (t - cut_at) as f64 / 1e6), "connected": at("connected"), "synced": at("synced"), "idling": at("idling") }));
        let message_id = format!("s6.{run}.cut{index}@example.test");
        let appended = appender.append(&message_id).await?;
        deliver(
            format!("after cut {index}"),
            appended,
            message_id,
            &mut running,
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    for index in 0..freezes {
        let frozen_at = mono_ns();
        let frozen = relay.freeze_all();
        if index == 0 && silent_wait > 0 {
            let message_id = format!("s6.{run}.frozen@example.test");
            let appended = appender.append(&message_id).await?;
            let noticed = wait_event(
                &mut running,
                Duration::from_secs(silent_wait),
                |event, value| {
                    event == "imap-disconnected"
                        || (event == "new-mail"
                            && value["message_id"].as_str() == Some(message_id.as_str()))
                },
            );
            println!(
                "freeze {index}: with no resume signal the app noticed nothing for {silent_wait} s: {} (a message appended {:.0} ms after the freeze {})",
                noticed.is_none(),
                (appended - frozen_at) as f64 / 1e6,
                if noticed.is_some() {
                    "arrived"
                } else {
                    "did not arrive"
                }
            );
        } else {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let resumed_at =
            tokio::task::spawn_blocking(|| runner::activate_action("simulate-resume")).await??;
        let reconnected = wait_event(&mut running, Duration::from_secs(20), |event, value| {
            event == "reconnected"
                && value["cause"] == "simulated-resume"
                && value["idling"].as_i64().is_some_and(|t| t > resumed_at)
        });
        let at = |key: &str| {
            reconnected
                .as_ref()
                .and_then(|v| v[key].as_i64())
                .map(|t| (t - resumed_at) as f64 / 1e6)
        };
        println!(
            "freeze {index} ({frozen} connections): resume signal → connected {} ms, synced {} ms, idling {} ms",
            fmt(at("connected")),
            fmt(at("synced")),
            fmt(at("idling"))
        );
        reconnects.push(json!({ "kind": "freeze+resume", "connected": at("connected"), "synced": at("synced"), "idling": at("idling") }));
        if index == 0 && silent_wait > 0 {
            let message_id = format!("s6.{run}.frozen@example.test");
            let arrived = running.seen_or_wait_for(Duration::from_secs(10), |event, value| {
                event == "new-mail" && value["message_id"].as_str() == Some(message_id.as_str())
            });
            println!(
                "freeze {index}: the message appended while frozen reached the app after the resume: {}",
                arrived.is_some()
            );
        }
        let message_id = format!("s6.{run}.resume{index}@example.test");
        let appended = appender.append(&message_id).await?;
        deliver(
            format!("after resume {index}"),
            appended,
            message_id,
            &mut running,
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    if hold > 0 {
        println!("holding {hold} s in background mode before a last append");
        let before = running.events.len();
        tokio::time::sleep(Duration::from_secs(hold)).await;
        running.drain();
        let idled = running.events[before..]
            .iter()
            .filter(|(event, _)| event == "imap-idling")
            .count();
        let disconnects = running.events[before..]
            .iter()
            .filter(|(event, _)| event == "imap-disconnected")
            .count();
        println!("during the hold: {idled} new IDLE starts, {disconnects} disconnects");
        let message_id = format!("s6.{run}.after-hold@example.test");
        let appended = appender.append(&message_id).await?;
        deliver("after the hold".into(), appended, message_id, &mut running);
    }
    let on_server = appender.count().await?;
    appender.logout().await;
    let quit_sent = tokio::task::spawn_blocking(|| runner::activate_action("quit")).await??;
    let status = running.wait_exit(Duration::from_secs(10))?;
    let quit_ms = (mono_ns() - quit_sent) as f64 / 1e6;
    save(&mut out, "background", &running)?;
    let sent_count = running.find("notification-sent").count();
    let quit = running.find("quit").next().cloned();
    println!(
        "quit: process exited {quit_ms:.0} ms after the quit action ({status}); engine stopped: {}; notifications sent: {sent_count}; messages on the server: {on_server}; relay links still open: {}",
        quit.map(|q| q["engine_stopped"].is_i64()).unwrap_or(false),
        relay.open_links()
    );
    if let Some(server) = notifications {
        let received = server.received();
        println!("fake server received {} notifications", received.len());
    }
    let pick = |key: &str| -> Vec<f64> {
        deliveries
            .iter()
            .filter_map(|row| row[key].as_f64())
            .collect()
    };
    println!(
        "\n| measure | n | p50 ms | p95 ms | min | max |\n| --- | --- | --- | --- | --- | --- |"
    );
    for (name, samples) in [
        ("APPEND OK → engine has the new mail", pick("new_mail")),
        ("APPEND OK → notification sent", pick("sent")),
        (
            "APPEND OK → server accepted (Notify reply)",
            pick("accepted"),
        ),
    ] {
        let s = summarize(&samples);
        println!(
            "| {name} | {} | {:.1} | {:.1} | {:.1} | {:.1} |",
            s.n, s.p50, s.p95, s.min, s.max
        );
    }
    for (kind, key) in [
        ("cut", "detected"),
        ("cut", "idling"),
        ("freeze+resume", "synced"),
        ("freeze+resume", "idling"),
    ] {
        let samples: Vec<f64> = reconnects
            .iter()
            .filter(|row| row["kind"] == kind)
            .filter_map(|row| row[key].as_f64())
            .collect();
        let s = summarize(&samples);
        println!(
            "| {kind}: → {key} | {} | {:.1} | {:.1} | {:.1} | {:.1} |",
            s.n, s.p50, s.p95, s.min, s.max
        );
    }
    Ok(())
}
