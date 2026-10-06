//! ML-058's acceptance against the local HTTP canary (S2's, from `hostile_support/`): messages
//! whose remote images point at the canary on 127.0.0.1, shown by the real reader over a
//! scratch store, the canary counting connections, requests and TLS ClientHellos after each
//! step. A sender not on the list makes 0 connections; Show images makes TLS hellos only (the
//! CSP upgrades http and the canary speaks no TLS, so no request ever arrives); after "Always
//! for this sender" another message from that sender does the same with no click, also in a
//! second reader that reads the list from the store, while another sender's message still makes
//! 0; after Hide forgets the sender, 0 again. Run it through the wrapper:
//!
//! ```sh
//! SEALWAX_HEADLESS_TIMEOUT=300 tools/headless.sh 35 target/debug/examples/remote_senders
//! ```

#[path = "hostile_support/canary.rs"]
mod canary;
#[path = "reader_support/mod.rs"]
mod support;

use canary::{Canary, Log};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::reader::samples::{self, Draft};
use sealwax_app::reader::{Reader, Shown, ShownBody};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::ThemeOptions;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

const SENDER: (&str, &str) = ("Canary Weekly", "news@canary-weekly.example.test");
const OTHER: (&str, &str) = ("Other Sender", "hello@other-sender.example.test");

/// How long a step waits for what WebKit would fetch after the page is on screen.
const SETTLE_MS: u64 = 1_200;

/// A message from `from` with two remote images at the canary: one over http (upgraded) and
/// one over https.
fn message(
    from: (&str, &str),
    subject: &str,
    id: &str,
    origin: &str,
    port: u16,
    now: i64,
) -> Vec<u8> {
    let mut draft = Draft::new(from, subject, now, id);
    draft.html = Some(format!(
        "<p>{subject}</p><p><img src=\"{origin}/{id}/plain.png\" width=\"120\" height=\"40\" \
         alt=\"Banner\"><img src=\"https://127.0.0.1:{port}/{id}/secure.png\" width=\"1\" \
         height=\"1\" alt=\"\"></p><p>Fictional text.</p>"
    ));
    draft.render()
}

/// What a step saw and wanted.
struct Step {
    name: &'static str,
    log: Log,
    wanted: Want,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    /// No connection at all.
    Nothing,
    /// TLS ClientHellos only: no plain request, and at least one hello.
    TlsOnly,
}

impl Step {
    fn passed(&self) -> bool {
        match self.wanted {
            Want::Nothing => self.log.is_quiet(),
            Want::TlsOnly => self.log.requests.is_empty() && self.log.tls_hellos > 0,
        }
    }
}

fn main() -> glib::ExitCode {
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let _input = match support::broadway::connect() {
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
    let origin = canary.origin();
    let port = canary.port();
    println!("ENV canary=127.0.0.1:{port}");
    let root = support::scratch("remote-senders");
    let scratch = Rc::new(support::store(&root));
    let now = glib::real_time() / 1_000_000;
    let messages = vec![
        (
            "first",
            message(SENDER, "First issue", "first", &origin, port, now - 60),
        ),
        (
            "second",
            message(SENDER, "Second issue", "second", &origin, port, now - 120),
        ),
        (
            "other",
            message(
                OTHER,
                "From someone else",
                "other",
                &origin,
                port,
                now - 180,
            ),
        ),
        (
            "third",
            message(SENDER, "Third issue", "third", &origin, port, now - 240),
        ),
        (
            "fourth",
            message(SENDER, "Fourth issue", "fourth", &origin, port, now - 300),
        ),
    ];
    let filled = samples::fill_store(&scratch.store, &scratch.blobs, messages)
        .expect("the canary messages go into the scratch store");
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.RemoteSenders")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let reader = support::reader(&scratch, &theme);
            reader.set_link_opener(|_, _| {});
            let window = support::window(app, &reader, (1280, 900));
            window.present();
            let app = app.clone();
            let passed = passed.clone();
            let canary = canary.clone();
            let filled = filled.clone();
            let scratch = scratch.clone();
            glib::spawn_future_local(async move {
                let ok = run(&window, &reader, &theme, &scratch, &canary, &filled).await;
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

/// Shows `id` and waits until it is on screen and WebKit had time to fetch.
async fn show(
    reader: &Reader,
    reports: &async_channel::Receiver<Shown>,
    id: MessageId,
) -> Result<Shown, String> {
    let shown = support::show(reader, reports, id).await?;
    support::sleep(SETTLE_MS).await;
    Ok(shown)
}

/// The next `Shown` report for `id` (a reshow after the list changed).
async fn reshown(reports: &async_channel::Receiver<Shown>, id: MessageId) -> Result<Shown, String> {
    loop {
        let report = glib::future_with_timeout(Duration::from_secs(10), reports.recv())
            .await
            .map_err(|_| format!("message {} wasn't shown again", id.0))?
            .map_err(|error| error.to_string())?;
        if report.id == id {
            support::sleep(SETTLE_MS).await;
            return Ok(report);
        }
    }
}

fn remote(shown: &Shown) -> Option<(bool, bool)> {
    match shown.body {
        ShownBody::Html {
            remote_images,
            remote_allowed,
            ..
        } => Some((remote_images, remote_allowed)),
        _ => None,
    }
}

/// Clicks a button of the shown page, found by its widget name.
fn click(reader: &Reader, name: &str) -> Result<(), String> {
    fn find(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.widget_name() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(found) = find(&next, name) {
                return Some(found);
            }
            child = next.next_sibling();
        }
        None
    }
    let page = reader
        .widget()
        .first_child()
        .ok_or("the reader shows nothing")?;
    let button = find(&page, name)
        .and_downcast::<gtk::Button>()
        .ok_or_else(|| format!("no button {name}"))?;
    button.emit_clicked();
    Ok(())
}

async fn run(
    window: &sealwax_app::window::Window,
    reader: &Reader,
    theme: &Rc<ThemeController>,
    scratch: &Rc<support::Scratch>,
    canary: &Canary,
    filled: &samples::Filled,
) -> bool {
    let id = |name: &str| filled.id(name).expect("stored");
    let reports = support::shown_reports(reader);
    if let Err(error) = support::resize(window.window(), 1280, 900).await {
        println!("ERROR {error}");
        return false;
    }
    support::wait_until(Duration::from_secs(10), || reader.remote_senders_loaded()).await;
    let mut steps = Vec::new();
    let mut failures = Vec::new();
    let mut record = |name: &'static str, before: &Log, wanted: Want, check: Result<(), String>| {
        let step = Step {
            name,
            log: canary.snapshot().since(before),
            wanted,
        };
        if let Err(error) = check {
            failures.push(format!("{name}: {error}"));
        }
        if !step.passed() {
            failures.push(format!("{name}: the canary saw {}", step.log.mark()));
        }
        steps.push(step);
    };

    // 1. A sender not on the list: nothing.
    let before = canary.snapshot();
    let check = match show(reader, &reports, id("first")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) if reader.state().banner => Ok(()),
            other => Err(format!(
                "wanted hidden images and the banner, got {other:?}"
            )),
        },
        Err(error) => Err(error),
    };
    record("not on the list", &before, Want::Nothing, check);

    // 2. Show images: this message only, https only.
    let before = canary.snapshot();
    reader.show_remote_images();
    support::sleep(SETTLE_MS).await;
    let check = if reader.state().banner {
        Err("the banner stayed".to_owned())
    } else {
        Ok(())
    };
    record("show images", &before, Want::TlsOnly, check);

    // 3. The same sender's next message still asks.
    let before = canary.snapshot();
    let check = match show(reader, &reports, id("second")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("same sender, before always", &before, Want::Nothing, check);

    // 4. Always for this sender: shown again with its images, no other click.
    let before = canary.snapshot();
    let check = match click(reader, "reader-always-show-images") {
        Ok(()) => match reshown(&reports, id("second")).await {
            Ok(shown) => match remote(&shown) {
                Some((true, true)) if !reader.state().banner => Ok(()),
                other => Err(format!("wanted the images allowed, got {other:?}")),
            },
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    record("always for this sender", &before, Want::TlsOnly, check);

    // 5. Another sender's message: nothing.
    let before = canary.snapshot();
    let check = match show(reader, &reports, id("other")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("another sender", &before, Want::Nothing, check);

    // 6. Another message from the listed sender, never shown before: no click needed.
    let before = canary.snapshot();
    let check = match show(reader, &reports, id("third")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, true)) => Ok(()),
            other => Err(format!("wanted the images allowed, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("listed sender, no click", &before, Want::TlsOnly, check);

    // 7. A second reader reads the list from the store.
    let second = support::reader(scratch, theme);
    second.set_link_opener(|_, _| {});
    window.set_reader(&second.widget());
    window.set_reader_toolbar(&second.toolbar());
    let second_reports = support::shown_reports(&second);
    support::wait_until(Duration::from_secs(10), || second.remote_senders_loaded()).await;
    let before = canary.snapshot();
    let check = match show(&second, &second_reports, id("fourth")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, true)) => Ok(()),
            other => Err(format!("wanted the images allowed, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("listed sender, a new reader", &before, Want::TlsOnly, check);

    // 8. Hide forgets the sender: shown again with its images hidden.
    let before = canary.snapshot();
    let check = match click(&second, "reader-hide-images") {
        Ok(()) => match reshown(&second_reports, id("fourth")).await {
            Ok(shown) => match remote(&shown) {
                Some((true, false)) if second.state().banner => Ok(()),
                other => Err(format!("wanted hidden images, got {other:?}")),
            },
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    record("hide (forgotten)", &before, Want::Nothing, check);

    // 9. The sender's other messages ask again; so does a third reader, from the store.
    let before = canary.snapshot();
    let check = match show(&second, &second_reports, id("third")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("forgotten, another message", &before, Want::Nothing, check);
    let third = support::reader(scratch, theme);
    window.set_reader(&third.widget());
    let third_reports = support::shown_reports(&third);
    support::wait_until(Duration::from_secs(10), || third.remote_senders_loaded()).await;
    let before = canary.snapshot();
    let check = match show(&third, &third_reports, id("second")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("forgotten, a new reader", &before, Want::Nothing, check);

    println!(
        "STEP {:<30} {:>6} {:>8} {:>5} {:>6}  verdict",
        "step", "conns", "requests", "tls", "silent"
    );
    for step in &steps {
        println!(
            "STEP {:<30} {:>6} {:>8} {:>5} {:>6}  {}",
            step.name,
            step.log.connections,
            step.log.requests.len(),
            step.log.tls_hellos,
            step.log.silent,
            if step.passed() { "pass" } else { "FAIL" }
        );
    }
    let total = canary.snapshot();
    println!(
        "CANARY-TOTAL connections={} requests={} silent={} tls={}",
        total.connections,
        total.requests.len(),
        total.silent,
        total.tls_hellos
    );
    let criticals = support::criticals();
    println!("SUMMARY failures={failures:?} glib_criticals={criticals}");
    failures.is_empty() && criticals == 0
}
