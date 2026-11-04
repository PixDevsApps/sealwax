//! Spike S2 harness: hardened per-message WebViews in a GTK conversation pane (ADR-011,
//! approach A), with the hostile suite, the corpus measurements, memory and scrolling.
//! Run it through `tools/headless.sh`, or on the live session only where authorized.

mod reader;
mod run;
mod ui;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

const USAGE: &str = "usage: email-spike-s2 <features|gpu|hostile|corpus|conversation|live> \
[--snapshots] [--save DIR] [--views N] [--no-related] [--no-accel] [--approach-b] [--short] [--heights a,b] [--scroll SECS] [--tall] \
[--deadline SECS] [--limit N] [--configs a,b] [--cases a,b]
(--cases selects hostile cases or corpus message ids)";

#[derive(Debug, Clone)]
pub struct Args {
    pub mode: String,
    pub snapshots: bool,
    pub save_dir: Option<PathBuf>,
    pub views: usize,
    pub share_process: bool,
    pub scroll_secs: f64,
    pub tall: bool,
    pub deadline_secs: u64,
    pub limit: Option<usize>,
    pub configs: Option<Vec<String>>,
    pub cases: Option<Vec<String>>,
    pub accelerated: bool,
    pub approach_b: bool,
    pub short: bool,
    pub heights: Vec<u32>,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mode = args.next().ok_or("missing mode")?;
        let mut parsed = Self {
            mode,
            snapshots: false,
            save_dir: None,
            views: 10,
            share_process: true,
            scroll_secs: 0.0,
            tall: false,
            deadline_secs: 280,
            limit: None,
            configs: None,
            cases: None,
            accelerated: true,
            approach_b: false,
            short: false,
            heights: Vec::new(),
        };
        while let Some(flag) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
            let number = |text: String| text.parse::<f64>().map_err(|e| format!("{flag}: {e}"));
            let list = |text: String| text.split(',').map(str::to_owned).collect::<Vec<_>>();
            match flag.as_str() {
                "--snapshots" => parsed.snapshots = true,
                "--save" => parsed.save_dir = Some(PathBuf::from(value()?)),
                "--views" => parsed.views = number(value()?)? as usize,
                "--no-related" => parsed.share_process = false,
                "--no-accel" => parsed.accelerated = false,
                "--approach-b" => parsed.approach_b = true,
                "--short" => parsed.short = true,
                "--heights" => {
                    parsed.heights = value()?
                        .split(',')
                        .map(|h| h.parse().map_err(|e| format!("--heights: {e}")))
                        .collect::<Result<_, _>>()?;
                }
                "--scroll" => parsed.scroll_secs = number(value()?)?,
                "--tall" => parsed.tall = true,
                "--deadline" => parsed.deadline_secs = number(value()?)? as u64,
                "--limit" => parsed.limit = Some(number(value()?)? as usize),
                "--configs" => parsed.configs = Some(list(value()?)),
                "--cases" => parsed.cases = Some(list(value()?)),
                _ => return Err(format!("unknown flag {flag}")),
            }
        }
        Ok(parsed)
    }
}

fn main() -> glib::ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return glib::ExitCode::from(2);
        }
    };
    if let Some(dir) = &args.save_dir
        && let Err(error) = std::fs::create_dir_all(dir)
    {
        eprintln!("cannot create {}: {error}", dir.display());
        return glib::ExitCode::from(2);
    }
    let deadline = Duration::from_secs(args.deadline_secs);
    std::thread::spawn(move || {
        std::thread::sleep(deadline);
        eprintln!("watchdog: no result after {deadline:?}; exiting");
        std::process::exit(3);
    });
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Mail.SpikeS2")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let code = Rc::new(Cell::new(1));
    app.connect_activate(glib::clone!(
        #[strong]
        code,
        move |app| {
            let hold = app.hold();
            let app = app.clone();
            let args = args.clone();
            let code = Rc::clone(&code);
            glib::MainContext::default().spawn_local(async move {
                code.set(run::main(app.clone(), args).await);
                drop(hold);
                app.quit();
            });
        }
    ));
    app.run_with_args::<&str>(&[]);
    glib::ExitCode::from(u8::try_from(code.get()).unwrap_or(1))
}
