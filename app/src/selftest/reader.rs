//! The reader's self-test commands (ML-027). `reader-open` fills a scratch store under `$TMP`
//! with the synthetic samples (`reader::samples`: plain, simple, newsletter, cid, attachments,
//! remote, latin1, dark-promo) through `ingest_raw`, and puts a reader over it into the window.
//! Links the reader would open are recorded, never opened.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `reader-open` | The scratch store and the reader in the window's reader slots |
//! | `reader-show NAME` | Shows a sample and waits until it is on screen |
//! | `reader-show-images` | "Show images" for the shown message; waits for the reload |
//! | `reader-click SELECTOR` | A DOM `click()` in Sealwax's isolated world of the shown body |
//! | `reader-clear` | Shows nothing |
//! | `assert-reader FIELD VALUE` | `body` (html-themed, html-paper, plain, text-fallback, empty, none), `banner`, `remote-allowed`, `attachments`, `live-views-at-most`, `cid-loaded` (images served through `cid:`), `remote-loaded` (images from the network), `height-exact` (the view is as tall as its document, at its width), `text-contains`, `opened` (the last link handed to the opener; `none`) |

use super::{Harness, StepResult, blocking, equal};
use crate::reader::{Reader, ReaderState, Shown, ShownBody, ViewEvent, samples};
use gtk4::glib;
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::BodyStyle;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// The commands and their argument counts, for the script parser.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("reader-open", 0, 0),
    ("reader-show", 1, 1),
    ("reader-show-images", 0, 0),
    ("reader-click", 1, 1),
    ("reader-clear", 0, 0),
    ("assert-reader", 2, 2),
];

const WAIT: Duration = Duration::from_secs(10);

struct Opened {
    reader: Reader,
    filled: samples::Filled,
    reports: async_channel::Receiver<Shown>,
    events: Rc<RefCell<Vec<(MessageId, ViewEvent)>>>,
    links: Rc<RefCell<Vec<String>>>,
}

thread_local! {
    static OPENED: RefCell<Option<Rc<Opened>>> = const { RefCell::new(None) };
}

pub fn handles(command: &str) -> bool {
    command.starts_with("reader-")
}

fn opened() -> Result<Rc<Opened>, String> {
    OPENED
        .with(|opened| opened.borrow().clone())
        .ok_or_else(|| "run reader-open first".to_owned())
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    match command {
        "reader-open" => open(harness).await,
        "reader-show" => show(&args[0]).await,
        "reader-show-images" => show_images().await,
        "reader-click" => {
            let opened = opened()?;
            let script = format!(
                "const el = document.querySelector({}); if (el) {{ el.click(); return true; }} \
                 return false;",
                js_string(&args[0])
            );
            let clicked = opened.reader.eval(&script).await?;
            if clicked.to_boolean() {
                glib::timeout_future(Duration::from_millis(300)).await;
                Ok(())
            } else {
                Err(format!("nothing matches {}", args[0]))
            }
        }
        "reader-clear" => {
            opened()?.reader.clear();
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    }
}

async fn open(harness: &Harness) -> StepResult {
    let window = harness.window()?;
    let theme = harness.state.theme().ok_or("the theme hasn't started")?;
    let keymap = harness
        .state
        .preferences()
        .map(|prefs| prefs.keymap())
        .unwrap_or_default();
    let folder = harness.root.join("reader-selftest");
    let blobs = BlobStore::open(&folder.join("blobs")).map_err(|error| error.to_string())?;
    let store = Store::open_in_background(
        folder.join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .recv()
    .await
    .map_err(|error| error.to_string())?;
    let store = Arc::new(store);
    let filling = store.clone();
    let now = glib::real_time() / 1_000_000;
    let filled = blocking(move || {
        samples::fill_store(&filling, &blobs, samples::messages(now))
            .map_err(|error| error.to_string())
    })
    .await??;
    let store = Arc::try_unwrap(store).map_err(|_| "the store is still shared")?;
    let reader = Reader::new(
        Rc::new(store),
        theme,
        &keymap,
        &harness.state.config().dirs.cache,
    );
    let links: Rc<RefCell<Vec<String>>> = Rc::default();
    reader.set_link_opener(glib::clone!(
        #[strong]
        links,
        move |_, uri| links.borrow_mut().push(uri.to_owned())
    ));
    let events: Rc<RefCell<Vec<(MessageId, ViewEvent)>>> = Rc::default();
    reader.connect_view_event(glib::clone!(
        #[strong]
        events,
        move |id, event| events.borrow_mut().push((id, event.clone()))
    ));
    let (sender, reports) = async_channel::unbounded();
    reader.connect_shown(move |shown| {
        let _ = sender.try_send(shown.clone());
    });
    window.set_reader(&reader.widget());
    window.set_reader_toolbar(&reader.toolbar());
    OPENED.with(|opened| {
        opened.replace(Some(Rc::new(Opened {
            reader,
            filled,
            reports,
            events,
            links,
        })))
    });
    Ok(())
}

async fn show(name: &str) -> StepResult {
    let opened = opened()?;
    let id = opened
        .filled
        .id(name)
        .ok_or_else(|| format!("no sample {name}; there are {:?}", samples::NAMES))?;
    while opened.reports.try_recv().is_ok() {}
    opened.reader.show(id);
    loop {
        let shown = glib::future_with_timeout(WAIT, opened.reports.recv())
            .await
            .map_err(|_| format!("{name} wasn't shown within {WAIT:?}"))?
            .map_err(|error| error.to_string())?;
        if shown.id == id {
            println!(
                "  # {name}: {} in {:.1} ms",
                body_name(Some(&shown.body)),
                shown.ready.as_secs_f64() * 1000.0
            );
            return Ok(());
        }
    }
}

fn loads(opened: &Opened, id: MessageId) -> usize {
    opened
        .events
        .borrow()
        .iter()
        .filter(|(event_id, event)| *event_id == id && *event == ViewEvent::Loaded)
        .count()
}

async fn show_images() -> StepResult {
    let opened = opened()?;
    let id = opened.reader.message().ok_or("no message is shown")?;
    let before = loads(&opened, id);
    opened.reader.show_remote_images();
    let started = std::time::Instant::now();
    while loads(&opened, id) == before {
        if started.elapsed() > WAIT {
            return Err("the message wasn't loaded again".to_owned());
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    Ok(())
}

fn body_name(body: Option<&ShownBody>) -> &'static str {
    match body {
        None => "none",
        Some(ShownBody::Html {
            style: BodyStyle::Paper,
            ..
        }) => "html-paper",
        Some(ShownBody::Html { .. }) => "html-themed",
        Some(ShownBody::Plain) => "plain",
        Some(ShownBody::TextFallback) => "text-fallback",
        Some(ShownBody::Empty) => "empty",
        Some(ShownBody::NotDownloaded) => "not-downloaded",
        Some(ShownBody::Missing) => "missing",
    }
}

fn js_string(text: &str) -> String {
    let escaped: String = text
        .chars()
        .flat_map(|c| match c {
            '\\' | '\'' => vec!['\\', c],
            '\n' => vec!['\\', 'n'],
            c => vec![c],
        })
        .collect();
    format!("'{escaped}'")
}

/// `assert-reader FIELD VALUE`.
pub async fn check(_harness: &Harness, args: &[String]) -> StepResult {
    let opened = opened()?;
    let state: ReaderState = opened.reader.state();
    let (field, value) = (args[0].as_str(), args[1].as_str());
    let number = |text: &str| {
        text.parse::<usize>()
            .map_err(|_| format!("expected a number, got {text}"))
    };
    let boolean = |text: &str| super::script::boolean(text);
    match field {
        "body" => equal("body", body_name(state.body.as_ref()), value),
        "banner" => equal("banner", state.banner, boolean(value)?),
        "remote-allowed" => {
            let allowed = matches!(
                state.body,
                Some(ShownBody::Html {
                    remote_allowed: true,
                    ..
                })
            );
            equal("remote images allowed", allowed, boolean(value)?)
        }
        "attachments" => equal("attachment cards", state.attachments, number(value)?),
        "live-views-at-most" => {
            let limit = number(value)?;
            if state.live_views <= limit {
                Ok(())
            } else {
                Err(format!("{} live views, over {limit}", state.live_views))
            }
        }
        "cid-loaded" | "remote-loaded" => {
            let probe = opened
                .reader
                .eval(
                    "const images = [...document.images]; \
                     const loaded = i => i.complete && i.naturalWidth > 0; \
                     return [images.filter(i => i.src.startsWith('cid:') && loaded(i)).length, \
                             images.filter(i => /^https?:/.test(i.src) && loaded(i)).length];",
                )
                .await?;
            let index = u32::from(field == "remote-loaded");
            let count = probe
                .object_get_property_at_index(index)
                .map_or(0, |value| value.to_int32()) as usize;
            equal(field, count, number(value)?)
        }
        "height-exact" => {
            let (height, width) = state.view_size.ok_or("no HTML body is shown")?;
            let probe = opened
                .reader
                .eval(
                    "return [document.documentElement.scrollHeight, window.innerHeight, \
                     window.innerWidth];",
                )
                .await?;
            let at = |index: u32| {
                probe
                    .object_get_property_at_index(index)
                    .map_or(-1, |value| value.to_int32())
            };
            let (scroll, inner, inner_width) = (at(0), at(1), at(2));
            let exact = (scroll - height).abs() <= 1 && scroll <= inner && inner_width == width;
            println!(
                "  # view {width}×{height}; document {scroll} high in a {inner_width}×{inner} \
                 viewport"
            );
            equal("height exact", exact, boolean(value)?)
        }
        "text-contains" => {
            let text = match state.body {
                Some(ShownBody::Html { .. }) => opened
                    .reader
                    .eval("return document.body ? document.body.innerText : '';")
                    .await?
                    .to_str()
                    .to_string(),
                _ => plain_text(&opened.reader),
            };
            if text.contains(value) {
                Ok(())
            } else {
                Err(format!("the body lacks {value:?}"))
            }
        }
        "opened" => {
            let last = opened.links.borrow().last().cloned();
            match (value, last) {
                ("none", None) => Ok(()),
                (wanted, Some(last)) if last == wanted => Ok(()),
                (wanted, last) => Err(format!("opened {last:?}, wanted {wanted}")),
            }
        }
        other => Err(format!("assert-reader knows no {other}")),
    }
}

/// The shown plain-text body's text.
fn plain_text(reader: &Reader) -> String {
    use gtk4::prelude::*;
    fn find(widget: &gtk4::Widget) -> Option<gtk4::TextView> {
        if let Some(view) = widget.downcast_ref::<gtk4::TextView>() {
            return Some(view.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find(&current) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    find(&reader.widget())
        .map(|view| {
            let buffer = view.buffer();
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string()
        })
        .unwrap_or_default()
}
