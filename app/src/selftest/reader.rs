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
//! | `assert-reader FIELD VALUE` | `body` (html-themed, html-paper, plain, text-fallback, empty, none), `banner`, `released` (the view went back while hidden), `remote-allowed`, `remote-requests` (remote loads that started since `reader-show`), `attachments`, `live-views-at-most`, `cid-loaded` (images served through `cid:`), `remote-loaded` (images from the network), `height-exact` (the view is as tall as its document, at its width), `margins` (a styled body's column sits on the theme's background, the card without its edge), `dark` (the dark script turned a styled body dark; when it did, the page's luminance is at most 0.1 and every text's contrast against what is behind it at least 4.5, or as low as the design's own lowest), `text-contains`, `opened` (the last link handed to the opener; `none`) |

use super::{Harness, StepResult, blocking, equal};
use crate::reader::{Reader, ReaderState, Shown, ShownBody, ViewEvent, samples};
use gtk4::glib;
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::{BodyStyle, Rgb};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// The commands and their argument counts, for the script parser.
pub(super) const COMMANDS: &[(&str, usize, usize)] = &[
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

pub(super) fn handles(command: &str) -> bool {
    command.starts_with("reader-")
}

fn opened() -> Result<Rc<Opened>, String> {
    OPENED
        .with(|opened| opened.borrow().clone())
        .ok_or_else(|| "run reader-open first".to_owned())
}

pub(super) async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
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
    let changes = store.subscribe();
    let reader = Reader::new(
        Rc::new(store),
        theme,
        &keymap,
        &harness.state.config().dirs.cache,
    );
    let forward = reader.clone();
    glib::spawn_future_local(async move {
        while let Ok(event) = changes.recv().await {
            forward.store_changed(&event);
        }
    });
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
    opened.events.borrow_mut().clear();
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

pub(super) fn body_name(body: Option<&ShownBody>) -> &'static str {
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
        Some(ShownBody::Downloading) => "downloading",
        Some(ShownBody::DownloadFailed(_)) => "download-failed",
        Some(ShownBody::Missing) => "missing",
        Some(ShownBody::Unreadable(_)) => "unreadable",
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
pub(super) async fn check(harness: &Harness, args: &[String]) -> StepResult {
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
        "released" => equal("view released", state.released, boolean(value)?),
        "remote-requests" => {
            let id = opened.reader.message().ok_or("no message is shown")?;
            let started = opened
                .events
                .borrow()
                .iter()
                .filter(|(event_id, event)| {
                    *event_id == id
                        && matches!(event, ViewEvent::Resource(uri)
                            if uri.starts_with("http:") || uri.starts_with("https:"))
                })
                .count();
            equal("remote loads started", started, number(value)?)
        }
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
        "text-shown" => {
            use gtk4::prelude::*;
            let shown = wait_until(Duration::from_secs(2), || {
                plain_view(&opened.reader).is_some_and(|view| view.height() > 0)
            })
            .await;
            equal("plain text shown", shown, boolean(value)?)
        }
        "follows-theme" => {
            let style = match state.body {
                Some(ShownBody::Html { style, .. }) => style,
                _ => return Err("no HTML body is shown".to_owned()),
            };
            let theme = harness
                .state
                .theme()
                .ok_or("the theme hasn't started")?
                .theme();
            let probe = opened
                .reader
                .eval(
                    "const root = getComputedStyle(document.documentElement); \
                     const body = getComputedStyle(document.body || document.documentElement); \
                     return [root.backgroundColor, body.color];",
                )
                .await?;
            let at = |index: u32| {
                probe
                    .object_get_property_at_index(index)
                    .map(|value| value.to_str().to_string())
                    .unwrap_or_default()
            };
            let (background, text) = (at(0), at(1));
            let canvas = theme.webview_background(style);
            let paper_canvas = style == BodyStyle::Paper
                && background == "rgba(0, 0, 0, 0)"
                && opened.reader.web_view().is_some_and(|view| {
                    let color = webkit6::prelude::WebViewExt::background_color(&view);
                    let channel = |value: f32| (value * 255.0).round() as u8;
                    (
                        channel(color.red()),
                        channel(color.green()),
                        channel(color.blue()),
                    ) == (canvas.r, canvas.g, canvas.b)
                });
            let paper_margins =
                style == BodyStyle::Paper && css_rgb_near(&background, theme.palette.background);
            let mut follows = paper_canvas
                || paper_margins
                || css_rgb_near(&background, theme.webview_background(style));
            if style == BodyStyle::Themed {
                follows &= css_rgb_near(&text, theme.palette.foreground);
            }
            println!(
                "  # {} body: background {background}, text {text}; theme {} ({})",
                body_name(state.body.as_ref()),
                theme.name(),
                theme.mode().as_str()
            );
            equal("follows the theme", follows, boolean(value)?)
        }
        "margins" => {
            use gtk4::prelude::*;
            let view = opened.reader.web_view().ok_or("no HTML body is shown")?;
            let frame = std::iter::successors(view.parent(), |widget| widget.parent())
                .find(|widget| widget.widget_name() == "reader-frame")
                .ok_or("the body has no frame")?;
            let theme = harness
                .state
                .theme()
                .ok_or("the theme hasn't started")?
                .theme();
            let background = opened
                .reader
                .eval("return getComputedStyle(document.documentElement).backgroundColor;")
                .await?
                .to_str()
                .to_string();
            let edgeless = frame.has_css_class("sw-paper-fitted");
            let painted = css_rgb_near(&background, theme.palette.background);
            println!(
                "  # margins: frame {}, root background {background}",
                if edgeless {
                    "edgeless"
                } else {
                    "with its edge"
                }
            );
            if edgeless != painted {
                return Err(format!(
                    "the frame (edgeless {edgeless}) and the page ({background}) disagree"
                ));
            }
            equal("margins fitted", edgeless, boolean(value)?)
        }
        "dark" => {
            let view = opened.reader.web_view().ok_or("no HTML body is shown")?;
            let canvas = webkit6::prelude::WebViewExt::background_color(&view);
            let probe = opened
                .reader
                .eval(&DARK_PROBE_JS.replace(
                    "CANVAS",
                    &format!(
                        "[{}, {}, {}]",
                        canvas.red() * 255.0,
                        canvas.green() * 255.0,
                        canvas.blue() * 255.0
                    ),
                ))
                .await?;
            let at = |index: u32| {
                probe
                    .object_get_property_at_index(index)
                    .map(|value| value.to_double())
                    .unwrap_or(f64::NAN)
            };
            let (darkened, worst, page, designed) = (at(0) == 1.0, at(1), at(2), at(3));
            let note = probe
                .object_get_property_at_index(4)
                .map(|value| value.to_str().to_string())
                .unwrap_or_default();
            println!(
                "  # dark: {darkened}, page luminance {page:.3}, lowest text contrast {worst:.2} \
                 ({note}; as designed {designed:.2})"
            );
            let wanted = designed.min(4.5) - 0.01;
            if darkened && (page > 0.1 || worst < wanted) {
                return Err(format!(
                    "dark, but the page's luminance is {page:.3} and the lowest text contrast \
                     {worst:.2} (wanted at most 0.1 and at least {wanted:.2})"
                ));
            }
            equal("dark", darkened, boolean(value)?)
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
pub(super) fn plain_text(reader: &Reader) -> String {
    use gtk4::prelude::*;
    plain_view(reader)
        .map(|view| {
            let buffer = view.buffer();
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string()
        })
        .unwrap_or_default()
}

/// The text view of the plain body shown, if any.
fn plain_view(reader: &Reader) -> Option<gtk4::TextView> {
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
}

/// Polls `condition` every 20 ms until it holds or `timeout` passes.
async fn wait_until(timeout: Duration, condition: impl Fn() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    condition()
}

/// Whether a computed CSS colour (`rgb(r, g, b)` or `rgba(…)`) is `expected`, within 2 per
/// channel.
/// Whether the dark script turned the body dark, the lowest WCAG contrast of any text against
/// the background painted behind it, the page's luminance (`CANVAS` is the view's colour, for a
/// page that paints none), and the lowest contrast of the design as it came, on white.
const DARK_PROBE_JS: &str = r#"
  const channel = (v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
  const luminance = (rgb) => 0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2]);
  const painted = (text) => {
    const match = /^rgba?\(([^)]*)\)$/.exec(text.trim());
    if (!match) return null;
    const parts = match[1].split(/[\s,/]+/).filter(Boolean).map(Number);
    if (parts.length > 3 && parts[3] === 0) return null;
    return luminance(parts);
  };
  const root = document.documentElement;
  const body = document.body;
  const look = (canvas) => {
    const page = painted(getComputedStyle(root).backgroundColor)
      ?? painted(getComputedStyle(body).backgroundColor) ?? luminance(canvas);
    let worst = 21;
    let note = '';
    for (const element of [body, ...body.querySelectorAll('*')]) {
      if (![...element.childNodes].some((node) => node.nodeType === 3 && node.data.trim())) continue;
      const style = getComputedStyle(element);
      if (style.display === 'none' || style.visibility === 'hidden') continue;
      const text = painted(style.color);
      if (text === null) continue;
      let behind = null;
      let under = 'the page';
      for (let at = element; at && at !== root && behind === null; at = at.parentElement) {
        behind = painted(getComputedStyle(at).backgroundColor);
        if (behind !== null) under = getComputedStyle(at).backgroundColor;
      }
      if (behind === null) behind = page;
      const contrast = (Math.max(text, behind) + 0.05) / (Math.min(text, behind) + 0.05);
      if (contrast < worst) {
        worst = contrast;
        note = element.localName + ' ' + style.color + ' on ' + under;
      }
    }
    return [worst, page, note];
  };
  const darkened = window.sealwaxDarkened === true;
  const [worst, page, note] = look(CANVAS);
  let designed = worst;
  if (darkened) {
    window.sealwaxDark.undo();
    designed = look([255, 255, 255])[0];
    window.sealwaxDark();
  }
  return [darkened ? 1 : 0, worst, page, designed, note];
"#;

fn css_rgb_near(css: &str, expected: Rgb) -> bool {
    let inner = css
        .trim()
        .trim_start_matches("rgba(")
        .trim_start_matches("rgb(")
        .trim_end_matches(')');
    let channels: Vec<u8> = inner
        .split(',')
        .take(3)
        .filter_map(|part| part.trim().parse::<f64>().ok())
        .map(|value| value.round().clamp(0.0, 255.0) as u8)
        .collect();
    matches!(channels.as_slice(), [r, g, b]
        if r.abs_diff(expected.r) <= 2 && g.abs_diff(expected.g) <= 2 && b.abs_diff(expected.b) <= 2)
}
