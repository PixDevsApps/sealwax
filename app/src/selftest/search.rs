//! Self-test commands for search in the message list (M7, ML-082): the search field's
//! suggestions, its scope, its delay and timings, and Rebuild Search Index. Keys and clicks go
//! through Broadway with triage's `key`, `type`, `focus search` and `click NAME` (the
//! suggestions' rows are `list-search-suggestion-N`); every name and address is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `search-wait` | Waits until the list searches for what the field holds (after its delay) and has settled |
//! | `search-delay MS` | Sets the field's search delay |
//! | `measure-search TEXT` | Types TEXT into the focused search field a key at a time; times each key from its press to the first painted frame with that text's results, and from the field's search-changed to that frame |
//! | `measure-search-burst TEXT MS` | Types TEXT a key every MS ms; times the last key to the final results painted and counts the searches run |
//! | `search-report LABEL` | Prints the timings measured since the last report (p50, p95, max) and forgets them |
//! | `search-rebuild-hold` | Rebuilds of the search index wait, with their toast up, until `search-rebuild-release` |
//! | `search-rebuild-release` | Lets held and later rebuilds run |
//! | `assert-suggestions closed\|N` | The suggestions are closed, or open with N rows |
//! | `assert-suggestion POS TITLE [DETAIL]` | Row POS shows TITLE (and DETAIL) |
//! | `assert-suggestion-selected POS\|none` | The row chosen with Down and Up |
//! | `assert-suggestion-marked POS KEY\|none` | The row whose key hint shows, and its key (`Tab`, `Enter`); `none` when none does |
//! | `assert-search-cursor N` | The search field's cursor, in characters |
//! | `assert-search-scope hidden\|all\|FOLDER` | The scope row is hidden, or shows with All folders or the folder named FOLDER chosen |
//! | `assert-search-key TEXT\|hidden` | The key at the end of the empty search field |
//! | `assert-search-rebuilding BOOL` | Whether a rebuild of the search index runs |
//! | `assert-search-toast TEXT` | A toast on screen starts with TEXT (the rebuild's counts and time vary) |
//! | `search-mark` | Notes how many searches the list has run, for `assert-searched` |
//! | `assert-searched BOOL` | Whether the list ran a search since `search-mark` |

use super::{Harness, StepResult, WAIT_TIMEOUT, broadway, equal, mail, number};
use crate::list::{Content, MessageList};
use crate::search;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("search-wait", 0, 0),
    ("search-delay", 1, 1),
    ("measure-search", 1, 1),
    ("measure-search-burst", 2, 2),
    ("search-report", 1, 1),
    ("search-rebuild-hold", 0, 0),
    ("search-rebuild-release", 0, 0),
    ("assert-suggestions", 1, 1),
    ("assert-suggestion", 2, 3),
    ("assert-suggestion-selected", 1, 1),
    ("assert-suggestion-marked", 1, 2),
    ("assert-search-cursor", 1, 1),
    ("assert-search-scope", 1, 1),
    ("assert-search-key", 1, 1),
    ("assert-search-rebuilding", 1, 1),
    ("assert-search-toast", 1, 1),
    ("search-mark", 0, 0),
    ("assert-searched", 1, 1),
];

/// How long one key may take to show its results before the measurement gives up.
const MEASURE_WAIT: Duration = Duration::from_secs(10);
/// The pause after each measured key, so its search is over before the next key.
const BETWEEN_KEYS: Duration = Duration::from_millis(60);

#[derive(Default)]
struct Timings {
    /// Key press → first frame painted with that text's results, in ms.
    key_to_paint: Vec<f64>,
    /// The field's search-changed → that frame, in ms.
    search_to_paint: Vec<f64>,
}

thread_local! {
    static TIMINGS: RefCell<Timings> = RefCell::new(Timings::default());
    static HOLD: RefCell<Option<async_channel::Sender<()>>> = const { RefCell::new(None) };
    static MARK: Cell<u64> = const { Cell::new(0) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "search-wait" => wait_for_search(harness).await,
        "search-delay" => {
            let delay = Duration::from_millis(number(arg(0))? as u64);
            mail::list(harness)?.set_search_delay(delay);
            Ok(())
        }
        "measure-search" => measure(harness, arg(0)).await,
        "measure-search-burst" => {
            burst(
                harness,
                arg(0),
                Duration::from_millis(number(arg(1))? as u64),
            )
            .await
        }
        "search-report" => {
            report(arg(0));
            Ok(())
        }
        "search-rebuild-hold" => {
            HOLD.with(|hold| hold.replace(Some(search::hold())));
            Ok(())
        }
        "search-rebuild-release" => {
            HOLD.with(|hold| hold.take());
            harness.wait_until_settled().await;
            Ok(())
        }
        "assert-suggestions" => {
            let shown = mail::list(harness)?.suggestions().shown();
            match arg(0) {
                "closed" => equal("suggestions shown", shown.len(), 0),
                count => equal(
                    &format!("suggestions shown ({shown:?})"),
                    shown.len(),
                    number(count)?,
                ),
            }
        }
        "assert-suggestion" => {
            let shown = mail::list(harness)?.suggestions().shown();
            let position = number(arg(0))?;
            let row = shown
                .get(position)
                .ok_or_else(|| format!("no suggestion {position} in {shown:?}"))?;
            equal(
                &format!("suggestion {position}"),
                row.title.as_str(),
                arg(1),
            )?;
            match args.get(2) {
                Some(detail) => equal(
                    &format!("suggestion {position}'s detail"),
                    row.detail.as_str(),
                    detail.as_str(),
                ),
                None => Ok(()),
            }
        }
        "assert-suggestion-selected" => {
            let selected = mail::list(harness)?.suggestions().selected();
            let wanted = match arg(0) {
                "none" => None,
                position => Some(number(position)?),
            };
            equal("suggestion chosen", selected, wanted)
        }
        "assert-suggestion-marked" => {
            let (marked, key) = marked(harness)?;
            let wanted = match arg(0) {
                "none" => None,
                position => Some(number(position)?),
            };
            equal("suggestion marked", marked, wanted)?;
            match wanted {
                Some(_) => equal(
                    "its key",
                    key.as_str(),
                    args.get(1).map_or("", String::as_str),
                ),
                None => Ok(()),
            }
        }
        "assert-search-cursor" => {
            let list = mail::list(harness)?;
            equal(
                "search field cursor",
                list.search_entry().position(),
                number(arg(0))? as i32,
            )
        }
        "assert-search-scope" => {
            let list = mail::list(harness)?;
            let row = list.scope_row();
            let shown = row.is_visible() && row.is_mapped();
            let actual = if !shown {
                "hidden".to_owned()
            } else if list.is_everywhere() {
                "all".to_owned()
            } else {
                scope_folder(row).unwrap_or_default()
            };
            equal("search scope", actual.as_str(), arg(0))
        }
        "assert-search-key" => {
            let key = harness.widget("list-search-key")?;
            let label = key
                .downcast_ref::<gtk::Label>()
                .ok_or("list-search-key isn't a label")?;
            let actual = if label.is_visible() && label.is_mapped() {
                label.label().to_string()
            } else {
                "hidden".to_owned()
            };
            equal("search key", actual.as_str(), arg(0))
        }
        "assert-search-rebuilding" => equal(
            "rebuilding the search index",
            search::is_running(),
            super::script::boolean(arg(0))?,
        ),
        "assert-search-toast" => {
            let shown = toast_texts(harness)?;
            if shown.iter().any(|text| text.starts_with(arg(0))) {
                Ok(())
            } else {
                Err(format!("the toasts on screen say {shown:?}"))
            }
        }
        "search-mark" => {
            let runs = mail::list(harness)?.model().stats().searches;
            MARK.with(|mark| mark.set(runs));
            Ok(())
        }
        "assert-searched" => {
            let runs = mail::list(harness)?.model().stats().searches;
            equal(
                "searched since search-mark",
                runs > MARK.with(Cell::get),
                super::script::boolean(arg(0))?,
            )
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// The texts of the toasts on screen.
fn toast_texts(harness: &Harness) -> Result<Vec<String>, String> {
    let mut shown = Vec::new();
    let Some(toast) = glib::Type::from_name("AdwToastWidget") else {
        return Ok(shown);
    };
    super::collect(harness.window()?.window().upcast_ref(), &mut |widget| {
        if let Some(label) = widget.downcast_ref::<gtk::Label>()
            && label.is_mapped()
            && label.ancestor(toast).is_some()
        {
            shown.push(label.label().to_string());
        }
    });
    Ok(shown)
}

/// The folder chip's label in the scope row.
fn scope_folder(row: &gtk::Box) -> Option<String> {
    let mut found = None;
    super::collect(row.upcast_ref(), &mut |widget| {
        if found.is_none()
            && widget.widget_name() == "search-scope-folder"
            && let Some(button) = widget.downcast_ref::<gtk::Button>()
        {
            found = button.label().map(|label| label.to_string());
        }
    });
    found
}

/// The row whose key hint shows, and the hint's text.
fn marked(harness: &Harness) -> Result<(Option<usize>, String), String> {
    let list = mail::list(harness)?;
    let popover = list.suggestions().popover().clone();
    let mut found = (None, String::new());
    super::collect(popover.upcast_ref(), &mut |widget| {
        if let Some(label) = widget.downcast_ref::<gtk::Label>()
            && label.has_css_class("sw-search-key-hint")
            && label.is_visible()
            && let Some(row) = label
                .ancestor(gtk::ListBoxRow::static_type())
                .and_downcast::<gtk::ListBoxRow>()
        {
            found = (usize::try_from(row.index()).ok(), label.label().to_string());
        }
    });
    Ok(found)
}

/// Whether the list shows the settled results of a search for `text`.
fn shows_results(list: &MessageList, text: &str) -> bool {
    let model = list.model();
    list.query() == text
        && matches!(model.content(), Content::Search { text: shown, .. } if shown == text)
        && !model.is_pending()
        && !model.is_searching()
}

async fn wait_for_search(harness: &Harness) -> StepResult {
    let list = mail::list(harness)?;
    let started = Instant::now();
    loop {
        let typed = list.search_entry().text().trim().to_owned();
        if list.query() == typed {
            return mail::settled(harness).await;
        }
        if started.elapsed() > WAIT_TIMEOUT {
            return Err("the list never searched for what the field holds".to_owned());
        }
        glib::timeout_future(Duration::from_millis(5)).await;
    }
}

/// Resolves with the first painted frame at which `ready` holds.
async fn painted_when(
    clock: &gtk::gdk::FrameClock,
    ready: impl Fn() -> bool + 'static,
) -> Option<Instant> {
    let (sender, receiver) = async_channel::bounded(1);
    let handler = clock.connect_after_paint(move |_| {
        if ready() {
            let _ = sender.try_send(Instant::now());
        }
    });
    let painted = super::with_timeout(receiver.recv(), MEASURE_WAIT).await;
    clock.disconnect(handler);
    painted.and_then(Result::ok)
}

/// `measure-search TEXT`: types TEXT into the focused search field a key at a time and times
/// each key from its press to the first frame painted with the results for the text so far.
async fn measure(harness: &Harness, text: &str) -> StepResult {
    let list = mail::list(harness)?;
    let window = harness.window()?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let changed: Rc<Cell<Option<Instant>>> = Rc::default();
    let mark = changed.clone();
    let handler = list.search_entry().connect_search_changed(move |_| {
        mark.set(Some(Instant::now()));
    });
    clock.begin_updating();
    let mut typed = list.search_entry().text().to_string();
    let mut lines = Vec::new();
    let mut failure = None;
    for character in text.chars() {
        typed.push(character);
        let wanted = typed.trim().to_owned();
        let keysym = gtk::gdk::unicode_to_keyval(u32::from(character));
        if wanted == list.query() {
            broadway::key(keysym, 0).map_err(|error| error.to_string())?;
            glib::timeout_future(BETWEEN_KEYS).await;
            continue;
        }
        changed.set(None);
        let shown = list.clone();
        let target = wanted.clone();
        let started = Instant::now();
        broadway::key(keysym, 0).map_err(|error| error.to_string())?;
        let Some(painted) = painted_when(&clock, move || shows_results(&shown, &target)).await
        else {
            failure = Some(format!(
                "no results painted for a {}-character search",
                wanted.chars().count()
            ));
            break;
        };
        let key_to_paint = ms(painted - started);
        let search_to_paint = changed
            .get()
            .map(|at| ms(painted.saturating_duration_since(at)));
        lines.push(format!(
            "{} chars: key → painted {key_to_paint:.1} ms, search-changed → painted {} ({} results)",
            wanted.chars().count(),
            search_to_paint.map_or("?".to_owned(), |value| format!("{value:.1} ms")),
            list.model().total()
        ));
        TIMINGS.with(|timings| {
            let mut timings = timings.borrow_mut();
            timings.key_to_paint.push(key_to_paint);
            if let Some(value) = search_to_paint {
                timings.search_to_paint.push(value);
            }
        });
        glib::timeout_future(BETWEEN_KEYS).await;
    }
    clock.end_updating();
    list.search_entry().disconnect(handler);
    println!(
        "  # delay {} ms, {} rows in view",
        list.search_delay().as_millis(),
        list.model().total()
    );
    for line in lines {
        println!("  # {line}");
    }
    failure.map_or(Ok(()), Err)
}

/// `measure-search-burst TEXT MS`: types TEXT a key every MS ms, then times the last key to the
/// first frame painted with the final results, and counts the searches the list ran.
async fn burst(harness: &Harness, text: &str, pace: Duration) -> StepResult {
    let list = mail::list(harness)?;
    let window = harness.window()?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let before = list.model().stats().searches;
    let mut typed = list.search_entry().text().to_string();
    typed.push_str(text);
    let wanted = typed.trim().to_owned();
    clock.begin_updating();
    let mut last = Instant::now();
    let count = text.chars().count();
    for (index, character) in text.chars().enumerate() {
        let keysym = gtk::gdk::unicode_to_keyval(u32::from(character));
        last = Instant::now();
        broadway::key(keysym, 0).map_err(|error| error.to_string())?;
        if index + 1 < count {
            glib::timeout_future(pace).await;
        }
    }
    let shown = list.clone();
    let target = wanted.clone();
    let painted = painted_when(&clock, move || shows_results(&shown, &target)).await;
    clock.end_updating();
    let painted = painted.ok_or("the final results were never painted")?;
    let after_last = ms(painted - last);
    println!(
        "  # delay {} ms, a key every {} ms: last key → painted {after_last:.1} ms; {} searches \
         for {count} keys; {} results",
        list.search_delay().as_millis(),
        pace.as_millis(),
        list.model().stats().searches - before,
        list.model().total()
    );
    TIMINGS.with(|timings| timings.borrow_mut().key_to_paint.push(after_last));
    Ok(())
}

fn report(label: &str) {
    let timings = TIMINGS.with(|timings| std::mem::take(&mut *timings.borrow_mut()));
    for (what, values) in [
        ("key → painted", &timings.key_to_paint),
        ("search-changed → painted", &timings.search_to_paint),
    ] {
        if values.is_empty() {
            continue;
        }
        println!(
            "  # {label}: {what} n={} p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
            values.len(),
            nearest_rank(values, 0.5),
            nearest_rank(values, 0.95),
            nearest_rank(values, 1.0)
        );
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The nearest-rank percentile `p` (0 to 1) of `values`.
fn nearest_rank(values: &[f64], p: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(nearest_rank(&values, 0.5), 10.0);
        assert_eq!(nearest_rank(&values, 0.95), 19.0);
        assert_eq!(nearest_rank(&values, 1.0), 20.0);
        assert!(nearest_rank(&[], 0.5).is_nan());
    }
}
