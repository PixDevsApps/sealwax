//! Do keys pressed while a message has the focus reach the Reader scope, do wheel steps over
//! a message scroll the reader, and does the focus stay in the reader from one message to the
//! next (ML-027)? Real key, click, motion and wheel events go in through Broadway, as a
//! browser client would send them; nothing is synthesized inside GTK.
//!
//! ```sh
//! SEALWAX_HEADLESS_TIMEOUT=120 tools/headless.sh 37 target/debug/examples/reader_keys
//! ```
//!
//! Prints one `KEYS` line per case and exits 0 when every expectation holds.

#[path = "reader_support/mod.rs"]
mod support;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use sealwax_app::actions;
use sealwax_app::reader::samples::{self, Draft};
use sealwax_app::reader::{Reader, Shown};
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::actions::ActionId;
use sealwax_domain::theme::ThemeOptions;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use support::broadway::{self, Input};

const KEY_J: u32 = 0x6a;
const KEY_E: u32 = 0x65;
const KEY_U: u32 = 0x75;
const KEY_X: u32 = 0x78;
const KEY_PAGE_DOWN: u32 = 0xff56;
const KEY_ESCAPE: u32 = 0xff1b;
const KEY_DELETE: u32 = 0xffff;
const KEY_G: u32 = 0x67;
const KEY_I: u32 = 0x69;

type Counts = Rc<RefCell<HashMap<ActionId, u32>>>;

fn keys_message(now: i64) -> Vec<u8> {
    let mut draft = Draft::new(
        ("Sana Iqbal", "sana@example.test"),
        "Lunch Thursday?",
        now - 600,
        "keys-form",
    );
    let mut html = String::from(
        "<p id=\"text\">Are you free at 12 on Thursday? There's a new ramen place I've been \
         meaning to try.</p><p><label>Your answer <input id=\"answer\" type=\"text\" \
         value=\"\"></label></p>",
    );
    for index in 0..40 {
        html.push_str(&format!(
            "<p>Line {index} of a long message, so the reader has something to scroll.</p>"
        ));
    }
    draft.html = Some(html);
    draft.render()
}

fn main() -> glib::ExitCode {
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let input = match broadway::connect() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("run this through tools/headless.sh: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let root = support::scratch("keys");
    let scratch = support::store(&root);
    let now = glib::real_time() / 1_000_000;
    let mut messages = samples::messages(now);
    messages.push(("keys", keys_message(now)));
    let filled = samples::fill_store(&scratch.store, &scratch.blobs, messages)
        .expect("the samples go into the scratch store");
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ReaderKeys")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let scratch = Rc::new(scratch);
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let reader = support::reader(&scratch, &theme);
            let window = support::window(app, &reader, (1280, 700));
            let counts: Counts = Rc::default();
            let win_actions = actions::install_window_actions(window.window());
            for id in [
                ActionId::NextMessage,
                ActionId::PreviousMessage,
                ActionId::BackToList,
                ActionId::Archive,
                ActionId::Delete,
                ActionId::GoToInbox,
            ] {
                let counts = counts.clone();
                win_actions.connect(id, move || {
                    *counts.borrow_mut().entry(id).or_default() += 1;
                });
            }
            win_actions.set_has_selection(true);
            window.present();
            let app = app.clone();
            let passed = passed.clone();
            let input = input.clone();
            let filled = filled.clone();
            glib::spawn_future_local(async move {
                let _keep = (&theme, &win_actions);
                let ok = match scenario(&reader, &window, &input, &counts, &filled).await {
                    Ok(ok) => ok,
                    Err(error) => {
                        println!("KEYS error {error}");
                        false
                    }
                };
                println!(
                    "KEYS {}",
                    if ok {
                        "every expectation held"
                    } else {
                        "FAILED"
                    }
                );
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

fn count(counts: &Counts, id: ActionId) -> u32 {
    counts.borrow().get(&id).copied().unwrap_or(0)
}

/// Sends `keyval` and waits for WebKit's round trip.
async fn press(input: &Input, keyval: u32) -> Result<(), String> {
    input.key(keyval, 0).map_err(|error| error.to_string())?;
    support::sleep(300).await;
    Ok(())
}

/// The centre of the element `selector` of the shown message, in surface coordinates.
async fn element_point(reader: &Reader, selector: &str) -> Result<(f64, f64), String> {
    let rect = reader
        .eval(&format!(
            "const r = document.querySelector('{selector}').getBoundingClientRect(); \
             return [r.left + r.width / 2, r.top + r.height / 2];"
        ))
        .await?;
    let x = rect
        .object_get_property_at_index(0)
        .map_or(0.0, |value| value.to_double());
    let y = rect
        .object_get_property_at_index(1)
        .map_or(0.0, |value| value.to_double());
    let view = reader.web_view().ok_or("no web view")?;
    support::surface_point(&view, x, y).ok_or_else(|| "no surface point".to_owned())
}

fn scroller(reader: &Reader) -> Option<gtk::ScrolledWindow> {
    reader
        .widget()
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast()
}

async fn scenario(
    reader: &Reader,
    window: &sealwax_app::window::Window,
    input: &Input,
    counts: &Counts,
    filled: &samples::Filled,
) -> Result<bool, String> {
    let reports = support::shown_reports(reader);
    support::resize(window.window(), 1280, 700).await?;
    let mut ok = true;
    let mut expect = |what: &str, held: bool, detail: String| {
        println!(
            "KEYS {} {what}: {detail}",
            if held { "ok" } else { "UNEXPECTED" }
        );
        ok &= held;
    };
    let id = |name: &str| filled.id(name).ok_or(format!("no sample {name}"));

    let shown: Shown = support::show(reader, &reports, id("keys")?).await?;
    println!("KEYS shown {:?} in {:?}", shown.body, shown.ready);
    support::frames(window.window(), 2).await?;
    let view = reader.web_view().ok_or("no web view")?;

    // 1. The WebView has the focus (as after Tab or a click), the page has nothing editable
    // focused: j, e and u.
    let column = scroller(reader).ok_or("the reader isn't in a scrolled window")?;
    let before_focus = column.vadjustment().value();
    view.grab_focus();
    support::frames(window.window(), 2).await?;
    let after_focus = column.vadjustment().value();
    expect(
        "focusing the WebView doesn't scroll the reader",
        (after_focus - before_focus).abs() < 1.0,
        format!(
            "scrolled from {before_focus} to {after_focus} (view {} px high)",
            view.height()
        ),
    );
    column.vadjustment().set_value(0.0);
    support::frames(window.window(), 1).await?;
    let before = (
        count(counts, ActionId::NextMessage),
        count(counts, ActionId::Archive),
        count(counts, ActionId::BackToList),
    );
    press(input, KEY_J).await?;
    press(input, KEY_E).await?;
    press(input, KEY_U).await?;
    let after = (
        count(counts, ActionId::NextMessage),
        count(counts, ActionId::Archive),
        count(counts, ActionId::BackToList),
    );
    expect(
        "j, e and u with the focus in the WebView reach the Reader scope",
        after == (before.0 + 1, before.1 + 1, before.2 + 1),
        format!(
            "focus in web view: {}; next {}→{}, archive {}→{}, back {}→{}",
            view.has_focus(),
            before.0,
            after.0,
            before.1,
            after.1,
            before.2,
            after.2
        ),
    );

    // Escape and Delete, which WebKit might keep, and the two-key sequence g then i.
    let before = (
        count(counts, ActionId::BackToList),
        count(counts, ActionId::Delete),
        count(counts, ActionId::GoToInbox),
    );
    press(input, KEY_ESCAPE).await?;
    press(input, KEY_DELETE).await?;
    press(input, KEY_G).await?;
    press(input, KEY_I).await?;
    let after = (
        count(counts, ActionId::BackToList),
        count(counts, ActionId::Delete),
        count(counts, ActionId::GoToInbox),
    );
    expect(
        "Escape, Delete and g then i with the focus in the WebView reach the Reader scope",
        after == (before.0 + 1, before.1 + 1, before.2 + 1),
        format!(
            "focus in web view: {}; back {}→{}, delete {}→{}, go to inbox {}→{}",
            view.has_focus(),
            before.0,
            after.0,
            before.1,
            after.1,
            before.2,
            after.2
        ),
    );

    // 2. A real click into the message text while the focus is elsewhere in the reader.
    let toggle = find(&reader.widget(), "reader-details-toggle").ok_or("no details toggle")?;
    toggle.grab_focus();
    column.vadjustment().set_value(0.0);
    support::frames(window.window(), 2).await?;
    let (x, y) = element_point(reader, "#text").await?;
    input.click(x, y).map_err(|error| error.to_string())?;
    support::sleep(300).await;
    support::frames(window.window(), 2).await?;
    expect(
        "a click into the text focuses the WebView without scrolling the reader",
        view.has_focus() && column.vadjustment().value().abs() < 1.0,
        format!(
            "focus in web view: {}; reader scrolled to {}",
            view.has_focus(),
            column.vadjustment().value()
        ),
    );
    let next = count(counts, ActionId::NextMessage);
    press(input, KEY_J).await?;
    expect(
        "j after clicking the message text",
        count(counts, ActionId::NextMessage) == next + 1,
        format!(
            "focus in web view: {}; next {}→{}",
            view.has_focus(),
            next,
            count(counts, ActionId::NextMessage)
        ),
    );

    // 3. A text field in the message keeps its letters.
    let (x, y) = element_point(reader, "#answer").await?;
    input.click(x, y).map_err(|error| error.to_string())?;
    support::sleep(300).await;
    let active = reader
        .eval("const a = document.activeElement; return a ? (a.id || a.tagName) : '';")
        .await?
        .to_str()
        .to_string();
    println!("KEYS after clicking the field at {x:.0},{y:.0}: active element {active:?}");
    if active != "answer" {
        reader
            .eval("document.getElementById('answer').focus(); return true;")
            .await?;
        let active = reader
            .eval("const a = document.activeElement; return a ? (a.id || a.tagName) : '';")
            .await?
            .to_str()
            .to_string();
        println!("KEYS after focus() from the isolated world: active element {active:?}");
    }
    let next = count(counts, ActionId::NextMessage);
    press(input, KEY_J).await?;
    press(input, KEY_X).await?;
    let value = reader
        .eval("return document.getElementById('answer').value;")
        .await?
        .to_str()
        .to_string();
    expect(
        "j and x typed into a message's text field stay there",
        count(counts, ActionId::NextMessage) == next && value == "jx",
        format!(
            "field value {value:?}; next {}→{}",
            next,
            count(counts, ActionId::NextMessage)
        ),
    );

    // 4. Wheel steps over the message scroll the reader.
    let scroller = scroller(reader).ok_or("the reader isn't in a scrolled window")?;
    let adjustment = scroller.vadjustment();
    adjustment.set_value(0.0);
    support::frames(window.window(), 1).await?;
    let (x, y) = element_point(reader, "#text").await?;
    for _ in 0..3 {
        input
            .scroll(x, y, true)
            .map_err(|error| error.to_string())?;
        support::sleep(150).await;
    }
    support::frames(window.window(), 2).await?;
    let scrolled = adjustment.value();
    expect(
        "wheel steps over the WebView scroll the reader",
        scrolled > 0.0,
        format!("scrolled to {scrolled} of {}", adjustment.upper()),
    );

    // 5. Page Down with the focus in the WebView scrolls the reader.
    adjustment.set_value(0.0);
    view.grab_focus();
    support::frames(window.window(), 1).await?;
    press(input, KEY_PAGE_DOWN).await?;
    support::frames(window.window(), 2).await?;
    let paged = adjustment.value();
    expect(
        "Page Down with the focus in the WebView scrolls the reader",
        paged > 0.0,
        format!("scrolled to {paged}"),
    );

    // 6. A plain-text message: the focus in its text view.
    support::show(reader, &reports, id("plain")?).await?;
    support::frames(window.window(), 2).await?;
    let text = reader
        .widget()
        .first_child()
        .and_then(|page| find(&page, "reader-plain"))
        .ok_or("no plain text view")?;
    text.grab_focus();
    support::frames(window.window(), 1).await?;
    let next = count(counts, ActionId::NextMessage);
    press(input, KEY_J).await?;
    expect(
        "j with the focus in a plain-text body",
        count(counts, ActionId::NextMessage) == next + 1,
        format!(
            "focus in text view: {}; next {}→{}",
            text.has_focus(),
            next,
            count(counts, ActionId::NextMessage)
        ),
    );

    // 7. Another message keeps the focus in the reader: the next body takes it. Left to GTK,
    // the focus moved out of the page that left on the next pointer event, through the whole
    // window (a Gtk-CRITICAL), to the next page's subject.
    let focus = || describe(GtkWindowExt::focus(window.window()));
    support::show(reader, &reports, id("keys")?).await?;
    support::frames(window.window(), 2).await?;
    let first = reader.web_view().ok_or("no web view")?;
    expect(
        "from a focused plain-text body, the next HTML body takes the focus",
        first.has_focus(),
        format!("focus on {}", focus()),
    );
    column.vadjustment().set_value(0.0);
    support::frames(window.window(), 1).await?;
    let (x, y) = element_point(reader, "#text").await?;
    input.click(x, y).map_err(|error| error.to_string())?;
    support::sleep(300).await;
    support::show(reader, &reports, id("newsletter")?).await?;
    input
        .motion(x, y + 12.0)
        .map_err(|error| error.to_string())?;
    support::sleep(300).await;
    support::frames(window.window(), 2).await?;
    let second = reader.web_view().ok_or("no web view")?;
    expect(
        "after a click into a message, the next message's body takes the focus",
        second != first && second.has_focus(),
        format!("focus on {}", focus()),
    );
    support::show(reader, &reports, id("plain")?).await?;
    input.motion(x, y).map_err(|error| error.to_string())?;
    support::sleep(300).await;
    let plain = find(&reader.widget(), "reader-plain").ok_or("no plain text view")?;
    expect(
        "from an HTML body, the next plain-text body takes the focus",
        plain.has_focus(),
        format!("focus on {}", focus()),
    );
    reader.clear();
    input
        .motion(x, y + 12.0)
        .map_err(|error| error.to_string())?;
    support::sleep(300).await;
    support::frames(window.window(), 1).await?;
    let left = GtkWindowExt::focus(window.window());
    expect(
        "a cleared reader keeps no focus",
        left.as_ref()
            .is_none_or(|left| !left.is_ancestor(&reader.widget())),
        format!("focus on {}", describe(left)),
    );
    expect(
        "no GLib criticals",
        support::criticals() == 0,
        format!("{} written", support::criticals()),
    );
    Ok(ok)
}

fn describe(widget: Option<gtk::Widget>) -> String {
    widget.map_or_else(
        || "nothing".to_owned(),
        |widget| format!("{} {:?}", widget.type_().name(), widget.widget_name()),
    )
}

fn find(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find(&current, name) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}
