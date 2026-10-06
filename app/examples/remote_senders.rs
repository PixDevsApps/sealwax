//! ML-058's acceptance against the local HTTP canary (S2's, from `hostile_support/`): messages
//! whose remote images point at the canary on 127.0.0.1, shown by the real reader over a
//! scratch store, the canary counting connections, requests and TLS ClientHellos after each
//! step. A sender not on the list makes 0 connections; Show images makes TLS hellos only (the
//! CSP upgrades http and the canary speaks no TLS, so no request ever arrives); after "Always
//! for this sender" another message from that sender does the same with no click, also in a
//! second reader that reads the list from the store, while another sender's message still makes
//! 0; after Hide forgets the sender, 0 again. Then a conversation, each message's images at a
//! canary of its own: two expanded messages from one sender, one from another sender and a
//! collapsed row from the first; "Always for this sender" in the selected message's card loads
//! both of that sender's cards (TLS hellos only at each of their canaries) while the other
//! sender's card stays at 0 connections; the row, expanded, loads its images with no click;
//! Hide in another of the sender's cards hides them all again. Run it through the wrapper:
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
/// How long a step waits for the cards a click builds again.
const REBUILD: Duration = Duration::from_secs(10);

/// The conversation's messages, oldest first: from the sender (read, so a row), from the sender,
/// from the other sender, from the sender (the latest, selected).
const THREAD: [&str; 4] = ["thread-1", "thread-2", "thread-3", "thread-4"];

/// A message from `from` with two remote images at `canary`: one over http (upgraded) and one
/// over https.
fn draft(from: (&str, &str), subject: &str, id: &str, canary: &Canary, now: i64) -> Draft {
    let origin = canary.origin();
    let port = canary.port();
    let mut draft = Draft::new(from, subject, now, id);
    draft.html = Some(format!(
        "<p>{subject}</p><p><img src=\"{origin}/{id}/plain.png\" width=\"120\" height=\"40\" \
         alt=\"Banner\"><img src=\"https://127.0.0.1:{port}/{id}/secure.png\" width=\"1\" \
         height=\"1\" alt=\"\"></p><p>Fictional text.</p>"
    ));
    draft
}

fn message(from: (&str, &str), subject: &str, id: &str, canary: &Canary, now: i64) -> Vec<u8> {
    draft(from, subject, id, canary, now).render()
}

/// What a step saw at one canary and wanted.
struct Step {
    name: String,
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

/// The canaries: one for the single messages, one for each message of the conversation.
struct Canaries {
    single: Canary,
    thread: [Canary; 4],
}

impl Canaries {
    fn snapshot(&self) -> Vec<Log> {
        std::iter::once(&self.single)
            .chain(&self.thread)
            .map(Canary::snapshot)
            .collect()
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
    let start = || Canary::start().map_err(|error| format!("could not start a canary: {error}"));
    let canaries = match (|| -> Result<Canaries, String> {
        Ok(Canaries {
            single: start()?,
            thread: [start()?, start()?, start()?, start()?],
        })
    })() {
        Ok(canaries) => Rc::new(canaries),
        Err(error) => {
            eprintln!("{error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let single = &canaries.single;
    println!(
        "ENV canaries=127.0.0.1:{},{}",
        single.port(),
        canaries
            .thread
            .iter()
            .map(|canary| canary.port().to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let root = support::scratch("remote-senders");
    let scratch = Rc::new(support::store(&root));
    let now = glib::real_time() / 1_000_000;
    let messages = vec![
        (
            "first",
            message(SENDER, "First issue", "first", single, now - 60),
        ),
        (
            "second",
            message(SENDER, "Second issue", "second", single, now - 120),
        ),
        (
            "other",
            message(OTHER, "From someone else", "other", single, now - 180),
        ),
        (
            "third",
            message(SENDER, "Third issue", "third", single, now - 240),
        ),
        (
            "fourth",
            message(SENDER, "Fourth issue", "fourth", single, now - 300),
        ),
    ];
    let filled = samples::fill_store(&scratch.store, &scratch.blobs, messages)
        .expect("the canary messages go into the scratch store");
    let from = [SENDER, SENDER, OTHER, SENDER];
    let drafts = THREAD
        .iter()
        .zip(from)
        .zip(&canaries.thread)
        .enumerate()
        .map(|(index, ((id, from), canary))| {
            let subject = if index == 0 {
                "Canary digest".to_owned()
            } else {
                "Re: Canary digest".to_owned()
            };
            let date = now - 3_600 * (4 - index as i64);
            (draft(from, &subject, id, canary, date), index == 0)
        })
        .collect();
    let thread = samples::fill_conversation(
        &scratch.store,
        &scratch.blobs,
        filled.inbox,
        filled.inbox,
        samples::files_conversation("thread", drafts),
    )
    .expect("the conversation goes into the scratch store");
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
            let canaries = canaries.clone();
            let filled = filled.clone();
            let thread = thread.clone();
            let scratch = scratch.clone();
            glib::spawn_future_local(async move {
                let ids = Ids {
                    single: filled,
                    thread,
                };
                let ok = run(&window, &reader, &theme, &scratch, &canaries, &ids).await;
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

/// Where the messages went.
struct Ids {
    single: samples::Filled,
    thread: samples::Filed,
}

impl Ids {
    fn id(&self, name: &str) -> MessageId {
        self.single
            .id(name)
            .or_else(|| self.thread.id(name))
            .expect("stored")
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

/// Whether message `id`'s card shows the widget named `name`.
fn shows(reader: &Reader, id: MessageId, name: &str) -> bool {
    reader
        .message_card(id)
        .and_then(|card| find(&card, name))
        .is_some_and(|widget| widget.is_visible())
}

/// Clicks a button of message `id`'s card, found by its widget name.
fn click(reader: &Reader, id: MessageId, name: &str) -> Result<(), String> {
    let card = reader
        .message_card(id)
        .ok_or_else(|| format!("message {} has no card", id.0))?;
    let button = find(&card, name)
        .and_downcast::<gtk::Button>()
        .ok_or_else(|| format!("no button {name}"))?;
    button.emit_clicked();
    Ok(())
}

/// Waits until `check` holds (the cards a click builds again), then for WebKit to fetch.
async fn rebuilt(what: &str, check: impl FnMut() -> bool) -> Result<(), String> {
    let done = support::wait_until(REBUILD, check).await;
    support::sleep(SETTLE_MS).await;
    if done {
        Ok(())
    } else {
        Err(format!("{what} never happened"))
    }
}

async fn run(
    window: &sealwax_app::window::Window,
    reader: &Reader,
    theme: &Rc<ThemeController>,
    scratch: &Rc<support::Scratch>,
    canaries: &Canaries,
    ids: &Ids,
) -> bool {
    let id = |name: &str| ids.id(name);
    let reports = support::shown_reports(reader);
    if let Err(error) = support::resize(window.window(), 1280, 900).await {
        println!("ERROR {error}");
        return false;
    }
    support::wait_until(Duration::from_secs(10), || reader.remote_senders_loaded()).await;
    let mut steps = Vec::new();
    let mut failures = Vec::new();
    // Records one step: what each canary saw since `before` (the single messages' canary,
    // then the conversation's) against what it should have.
    let mut record = |name: &str, before: &[Log], wanted: &[Want], check: Result<(), String>| {
        if let Err(error) = check {
            failures.push(format!("{name}: {error}"));
        }
        let now = canaries.snapshot();
        let labels = [
            "",
            ": issue 1 (row)",
            ": issue 2",
            ": the other sender",
            ": issue 4",
        ];
        for (index, label) in labels.iter().enumerate() {
            let step = Step {
                name: format!("{name}{label}"),
                log: now[index].since(&before[index]),
                wanted: wanted.get(index).copied().unwrap_or(Want::Nothing),
            };
            if !step.passed() {
                failures.push(format!("{}: the canary saw {}", step.name, step.log.mark()));
            }
            // The conversation's canaries are listed where a step names what they want; they
            // must stay quiet in the others, and are listed when they don't.
            if index < wanted.len() || !step.passed() {
                steps.push(step);
            }
        }
    };
    use Want::{Nothing, TlsOnly};

    // 1. A sender not on the list: nothing.
    let before = canaries.snapshot();
    let check = match show(reader, &reports, id("first")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) if reader.state().banner => Ok(()),
            other => Err(format!(
                "wanted hidden images and the banner, got {other:?}"
            )),
        },
        Err(error) => Err(error),
    };
    record("not on the list", &before, &[Nothing], check);

    // 2. Show images: this message only, https only.
    let before = canaries.snapshot();
    reader.show_remote_images();
    support::sleep(SETTLE_MS).await;
    let check = if reader.state().banner {
        Err("the banner stayed".to_owned())
    } else {
        Ok(())
    };
    record("show images", &before, &[TlsOnly], check);

    // 3. The same sender's next message still asks.
    let before = canaries.snapshot();
    let check = match show(reader, &reports, id("second")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("same sender, before always", &before, &[Nothing], check);

    // 4. Always for this sender: its card is built again with its images, no other click.
    let before = canaries.snapshot();
    let second = id("second");
    let check = match click(reader, second, "reader-always-show-images") {
        Ok(()) => {
            rebuilt("the card with its images", || {
                reader.remote_allowed(second) && shows(reader, second, "reader-sender-note")
            })
            .await
        }
        Err(error) => Err(error),
    }
    .and_then(|()| match reader.state().banner {
        false => Ok(()),
        true => Err("the banner stayed".to_owned()),
    });
    record("always for this sender", &before, &[TlsOnly], check);

    // 5. Another sender's message: nothing.
    let before = canaries.snapshot();
    let check = match show(reader, &reports, id("other")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("another sender", &before, &[Nothing], check);

    // 6. Another message from the listed sender, never shown before: no click needed.
    let before = canaries.snapshot();
    let check = match show(reader, &reports, id("third")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, true)) => Ok(()),
            other => Err(format!("wanted the images allowed, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("listed sender, no click", &before, &[TlsOnly], check);

    // 7. A second reader reads the list from the store.
    let second_reader = support::reader(scratch, theme);
    second_reader.set_link_opener(|_, _| {});
    window.set_reader(&second_reader.widget());
    window.set_reader_toolbar(&second_reader.toolbar());
    let second_reports = support::shown_reports(&second_reader);
    support::wait_until(Duration::from_secs(10), || {
        second_reader.remote_senders_loaded()
    })
    .await;
    let before = canaries.snapshot();
    let check = match show(&second_reader, &second_reports, id("fourth")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, true)) => Ok(()),
            other => Err(format!("wanted the images allowed, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("listed sender, a new reader", &before, &[TlsOnly], check);

    // 8. Hide forgets the sender: the card is built again with its images hidden.
    let before = canaries.snapshot();
    let fourth = id("fourth");
    let check = match click(&second_reader, fourth, "reader-hide-images") {
        Ok(()) => {
            rebuilt("the card with its banner", || {
                !second_reader.remote_allowed(fourth) && second_reader.state().banner
            })
            .await
        }
        Err(error) => Err(error),
    };
    record("hide (forgotten)", &before, &[Nothing], check);

    // 9. The sender's other messages ask again; so does a third reader, from the store.
    let before = canaries.snapshot();
    let check = match show(&second_reader, &second_reports, id("third")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("forgotten, another message", &before, &[Nothing], check);
    let third_reader = support::reader(scratch, theme);
    window.set_reader(&third_reader.widget());
    let third_reports = support::shown_reports(&third_reader);
    support::wait_until(Duration::from_secs(10), || {
        third_reader.remote_senders_loaded()
    })
    .await;
    let before = canaries.snapshot();
    let check = match show(&third_reader, &third_reports, id("second")).await {
        Ok(shown) => match remote(&shown) {
            Some((true, false)) => Ok(()),
            other => Err(format!("wanted hidden images, got {other:?}")),
        },
        Err(error) => Err(error),
    };
    record("forgotten, a new reader", &before, &[Nothing], check);

    // 10. A conversation from the forgotten sender and another: every card asks.
    let [one, two, cafe, four] = THREAD.map(id);
    let quiet = [Nothing; 5];
    let before = canaries.snapshot();
    let check = match show(&third_reader, &third_reports, four).await {
        Ok(_) => {
            let state = third_reader.state();
            let banners = [two, cafe, four]
                .iter()
                .all(|id| shows(&third_reader, *id, "reader-banner"));
            if state.expanded.len() == 3 && state.collapsed == [one] && banners {
                Ok(())
            } else {
                Err(format!(
                    "wanted three cards with banners and one row, got {} expanded, {} rows, \
                     banners {banners}",
                    state.expanded.len(),
                    state.collapsed.len()
                ))
            }
        }
        Err(error) => Err(error),
    };
    record("conversation, not on the list", &before, &quiet, check);

    // 11. Always for this sender in the selected card: both of the sender's cards load their
    //     images; the other sender's card keeps its banner.
    let before = canaries.snapshot();
    let check = match click(&third_reader, four, "reader-always-show-images") {
        Ok(()) => {
            rebuilt("both of the sender's cards with their images", || {
                [two, four].iter().all(|id| {
                    third_reader.remote_allowed(*id)
                        && shows(&third_reader, *id, "reader-sender-note")
                })
            })
            .await
        }
        Err(error) => Err(error),
    }
    .and_then(|()| {
        match shows(&third_reader, cafe, "reader-banner") && !third_reader.remote_allowed(cafe) {
            true => Ok(()),
            false => Err("the other sender's card lost its banner".to_owned()),
        }
    });
    record(
        "conversation, always",
        &before,
        &[Nothing, Nothing, TlsOnly, Nothing, TlsOnly],
        check,
    );

    // 12. The sender's row, expanded: its images load with no click.
    let before = canaries.snapshot();
    third_reader.expand(one);
    let check = rebuilt("the row's card with its images", || {
        third_reader.remote_allowed(one) && shows(&third_reader, one, "reader-sender-note")
    })
    .await;
    record(
        "conversation, row expanded",
        &before,
        &[Nothing, TlsOnly, Nothing, Nothing, Nothing],
        check,
    );

    // 13. Hide in another of the sender's cards: all three ask again.
    let before = canaries.snapshot();
    let check = match click(&third_reader, two, "reader-hide-images") {
        Ok(()) => {
            rebuilt("the sender's cards with their banners", || {
                [one, two, four].iter().all(|id| {
                    !third_reader.remote_allowed(*id) && shows(&third_reader, *id, "reader-banner")
                })
            })
            .await
        }
        Err(error) => Err(error),
    };
    record("conversation, hide", &before, &quiet, check);

    println!(
        "STEP {:<50} {:>6} {:>8} {:>5} {:>6}  verdict",
        "step", "conns", "requests", "tls", "silent"
    );
    for step in &steps {
        println!(
            "STEP {:<50} {:>6} {:>8} {:>5} {:>6}  {}",
            step.name,
            step.log.connections,
            step.log.requests.len(),
            step.log.tls_hellos,
            step.log.silent,
            if step.passed() { "pass" } else { "FAIL" }
        );
    }
    let total = canaries.snapshot();
    println!(
        "CANARY-TOTAL connections={} requests={} silent={} tls={}",
        total.iter().map(|log| log.connections).sum::<u64>(),
        total.iter().map(|log| log.requests.len()).sum::<usize>(),
        total.iter().map(|log| log.silent).sum::<u64>(),
        total.iter().map(|log| log.tls_hellos).sum::<u64>()
    );
    let criticals = support::criticals();
    println!("SUMMARY failures={failures:?} glib_criticals={criticals}");
    failures.is_empty() && criticals == 0
}
