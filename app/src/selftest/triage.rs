//! Self-test commands for triage (M4: ML-052 to ML-055). Keys and clicks go through Broadway as
//! real key and button presses (`broadway.rs`), so GTK's own event handling decides which
//! widget gets them; every message is fictional.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `key KEY [COUNT]` | Presses KEY (a GTK accelerator: `j`, `<Shift>i`, `numbersign`, `<Control>z`), COUNT times, to the focused widget |
//! | `type TEXT` | Types TEXT, a character at a time |
//! | `focus list\|search\|reader\|NAME` | Puts the keyboard focus there |
//! | `click-row POS [ctrl\|shift\|ctrl+shift]` | Clicks a list row with the primary button |
//! | `check-row POS` | Ticks or unticks a row's checkbox, as a click on it does |
//! | `hover-row POS` | Moves the pointer over a row |
//! | `hover NAME` | Moves the pointer to the middle of the widget named NAME (a scrollbar shows itself) |
//! | `double-click NAME` | Two primary clicks in the middle of the widget named NAME |
//! | `drag NAME DX` | Presses the primary button in the middle of the widget named NAME, moves the pointer DX px across (left when negative) and releases it |
//! | `click-quick POS archive\|delete\|star` | Hovers a row and clicks its quick action |
//! | `triage-account NAME KEY` | An account with a key, as `accounts.toml` lists one, and its six role folders (OpsQueued goes to it) |
//! | `triage-mark-read-on-open true\|false` | Switches marking a message read when the reader shows it |
//! | `triage-undo-window MS` | Shortens the undo window (the toast shows at least a second) |
//! | `toast-undo` | Clicks the toast's Undo |
//! | `sidebar-menu all-inboxes\|[ACCOUNT:]ROLE` | Right-clicks a sidebar folder |
//! | `account-menu KEY` | Right-clicks the heading of the sidebar's account KEY (`account-1`) |
//! | `sidebar-menu-click [NAME]` | Clicks the open menu's item named NAME (`folder-menu-rename`, `account-menu-new-folder`, …), else its first: a folder's Mark Folder as Read, an account's Edit Account |
//! | `action-log` | Records the window's and the application's commands as they run, from now on |
//! | `measure-triage KEY COUNT` | Presses KEY COUNT times on the selected message and times each from the command's handler to the list's change and to the frame that shows it |
//! | `measure-leave KEY COUNT [MS]` | Presses KEY COUNT times on the message the reader shows and times each from the command's handler (the key press for a key that moves the selection, `j`) to the first frame without it in the reader and to the frame with the next one; with MS, fails when it stayed longer |
//! | `hold-reader-layout BOOL` | Holds the reader's layout of the next conversations unfinished, as a slow machine would, until switched off |
//! | `assert-focus WHERE` | The keyboard focus is in WHERE (as `focus`) |
//! | `assert-list-selection none\|POS,…` | The selected rows |
//! | `assert-select-mode BOOL` | Whether the list is in the select mode |
//! | `assert-reader-page empty\|message\|batch` | What the reader pane shows |
//! | `assert-undoable BOOL` | Whether Undo has something to undo |
//! | `assert-toasts N` | Toasts on screen |
//! | `assert-row-check POS none\|true\|false` | A row's checkbox: hidden, ticked, not ticked |
//! | `assert-row-quick POS BOOL` | Whether a row shows its quick actions |
//! | `assert-move-to open\|shown\|selected VALUE` | The Move to… picker: open (BOOL), the folders shown (`A,B`), the folder Enter picks |
//! | `assert-sidebar-menu closed\|enabled\|disabled [NAME]` | The sidebar's menu, and its item named NAME, else its first (Mark Folder as Read) |
//! | `assert-action-log NAMES\|none` | The commands run since the last check, in order (`win.archive,win.star`) |

use super::{Harness, StepResult, WAIT_TIMEOUT, boolean, broadway, equal, number};
use crate::move_to;
use gtk4 as gtk;
use gtk4::glib::translate::IntoGlib;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use sealwax_domain::account::AccountKey;
use sealwax_domain::actions::{ActionId, ActionKind, Scope};
use sealwax_domain::model::FolderRole;
use sealwax_infrastructure::store::{FolderSpec, NewAccount};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("key", 1, 2),
    ("type", 1, 1),
    ("focus", 1, 1),
    ("click-row", 1, 2),
    ("check-row", 1, 1),
    ("hover-row", 1, 1),
    ("hover", 1, 1),
    ("drag", 2, 2),
    ("double-click", 1, 1),
    ("click-quick", 2, 2),
    ("click", 1, 2),
    ("triage-account", 2, 2),
    ("triage-mark-read-on-open", 1, 1),
    ("triage-undo-window", 1, 1),
    ("toast-undo", 0, 0),
    ("sidebar-menu", 1, 1),
    ("account-menu", 1, 1),
    ("sidebar-menu-click", 0, 1),
    ("action-log", 0, 0),
    ("measure-triage", 2, 2),
    ("measure-leave", 2, 3),
    ("hold-reader-layout", 1, 1),
    ("assert-focus", 1, 1),
    ("assert-list-selection", 1, 1),
    ("assert-select-mode", 1, 1),
    ("assert-reader-page", 1, 1),
    ("assert-undoable", 1, 1),
    ("assert-pending", 1, 1),
    ("assert-toasts", 1, 1),
    ("assert-row-check", 2, 2),
    ("assert-row-quick", 2, 2),
    ("assert-move-to", 2, 2),
    ("assert-sidebar-menu", 1, 2),
    ("assert-action-log", 1, 1),
];

thread_local! {
    /// The commands run since `action-log` or the last `assert-action-log`.
    static LOG: RefCell<Option<Rc<RefCell<Vec<String>>>>> = const { RefCell::new(None) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "key" => {
            let times = args.get(1).map_or(Ok(1), |count| number(count))?;
            for _ in 0..times {
                let watch = releases(harness);
                press(arg(0))?;
                delivered(harness, watch, 1).await;
            }
            Ok(())
        }
        "type" => {
            let watch = releases(harness);
            for character in arg(0).chars() {
                let keysym = gtk::gdk::unicode_to_keyval(u32::from(character));
                broadway::key(keysym, 0).map_err(|error| error.to_string())?;
            }
            delivered(harness, watch, arg(0).chars().count() as u64).await;
            Ok(())
        }
        "focus" => {
            focus_target(harness, arg(0))?.grab_focus();
            harness.wait_until_settled().await;
            Ok(())
        }
        "click-row" => {
            let row = row_widget(harness, arg(0))?;
            let state = modifiers(args.get(1).map_or("none", String::as_str))?;
            click_widget(harness, row.upcast_ref(), state, 1).await
        }
        "check-row" => {
            let row = row_widget(harness, arg(0))?;
            let check = row
                .check_button()
                .ok_or_else(|| format!("row {} shows no checkbox", arg(0)))?;
            let before = check.is_active();
            click_widget(harness, check.upcast_ref(), 0, 1).await?;
            harness.wait_until_settled().await;
            clicked(row.check_state() != Some(before), "the checkbox")
        }
        "hover-row" => {
            let row = row_widget(harness, arg(0))?;
            let (x, y) = centre(harness, row.upcast_ref())?;
            broadway::hover(x, y).map_err(|error| error.to_string())?;
            hovered(harness, &row).await
        }
        "hover" => {
            let widget = harness.widget(arg(0))?;
            let (x, y) = centre(harness, &widget)?;
            broadway::hover(x, y).map_err(|error| error.to_string())?;
            glib::timeout_future(Duration::from_millis(300)).await;
            harness.wait_until_settled().await;
            Ok(())
        }
        "double-click" => {
            let widget = harness.widget(arg(0))?;
            let (x, y) = centre(harness, &widget)?;
            broadway::double_click(x, y).map_err(|error| error.to_string())?;
            glib::timeout_future(Duration::from_millis(100)).await;
            harness.wait_until_settled().await;
            Ok(())
        }
        "drag" => {
            let widget = harness.widget(arg(0))?;
            let distance: f64 = arg(1)
                .parse()
                .map_err(|_| format!("expected a distance in px, got {}", arg(1)))?;
            let (x, y) = centre(harness, &widget)?;
            let watch = releases(harness);
            broadway::drag((x, y), (x + distance, y), 8).map_err(|error| error.to_string())?;
            delivered(harness, watch, 1).await;
            Ok(())
        }
        "click-quick" => {
            let row = row_widget(harness, arg(0))?;
            let (x, y) = centre(harness, row.upcast_ref())?;
            broadway::hover(x, y).map_err(|error| error.to_string())?;
            hovered(harness, &row).await?;
            let quick = crate::list::Quick::ALL
                .into_iter()
                .find(|quick| quick.name() == arg(1))
                .ok_or_else(|| format!("use archive, delete or star, not {}", arg(1)))?;
            let button = row
                .quick_button(quick)
                .filter(|button| button.is_mapped())
                .ok_or("the row shows no quick actions")?;
            click_button(harness, &button).await
        }
        "click" => {
            let widget = harness.widget(arg(0))?;
            let state = modifiers(args.get(1).map_or("none", String::as_str))?;
            match widget.downcast_ref::<gtk::Button>() {
                Some(button) if state == 0 => click_button(harness, button).await,
                _ => {
                    click_widget(harness, &widget, state, 1).await?;
                    harness.wait_until_settled().await;
                    Ok(())
                }
            }
        }
        "triage-account" => add_account(harness, arg(0), arg(1)).await,
        "triage-mark-read-on-open" => {
            harness
                .state
                .set_mark_read_on_open(super::script::boolean(arg(0))?);
            Ok(())
        }
        "triage-undo-window" => {
            let triage = harness
                .state
                .triage()
                .ok_or("there is no triage controller")?;
            triage.set_window(Duration::from_millis(number(arg(0))? as u64));
            Ok(())
        }
        "toast-undo" => {
            let button = toast_buttons(harness)?
                .into_iter()
                .find(|button| button.label().as_deref() == Some("Undo"))
                .ok_or("no toast offers Undo")?;
            click_button(harness, &button).await
        }
        "sidebar-menu" => {
            let key = super::mail::folder_key_for(harness, arg(0))?;
            let name = format!("folder:{key}");
            harness.wait_until_settled().await;
            let row = harness.widget(&name)?;
            click_widget(harness, &row, 0, 3).await?;
            let window = harness.window()?;
            let sidebar = window.sidebar();
            match sidebar.menu() {
                Some(menu) if menu.parent().is_some_and(|row| row.widget_name() == name) => {
                    return Ok(());
                }
                Some(menu) => {
                    println!("  # the right click opened another folder's menu");
                    menu.popdown();
                }
                // Buttons that act on release don't take Broadway's synthetic clicks headless
                // (docs/m4/triage-app.md); the menu opens as the row's gesture opens it, and
                // the step says so.
                None => println!("  # the right click opened no menu"),
            }
            println!("  # opened as the gesture does");
            if !harness.window()?.sidebar().open_menu(&key) {
                return Err(format!("no folder row {key}"));
            }
            harness.wait_until_settled().await;
            if harness.window()?.sidebar().menu().is_none() {
                return Err("the menu closed at once".to_owned());
            }
            Ok(())
        }
        "account-menu" => {
            let name = format!("heading:{}", arg(0));
            harness.wait_until_settled().await;
            let row = harness.widget(&name)?;
            click_widget(harness, &row, 0, 3).await?;
            let opened = harness
                .window()?
                .sidebar()
                .menu()
                .is_some_and(|menu| menu.parent().is_some_and(|row| row.widget_name() == name));
            if !opened {
                // As `sidebar-menu`: headless, Broadway's synthetic click may not reach the
                // gesture.
                println!("  # opened as the gesture does");
                if !harness.window()?.sidebar().open_account_menu(arg(0)) {
                    return Err(format!("no account heading {}", arg(0)));
                }
            }
            harness.wait_until_settled().await;
            if harness.window()?.sidebar().menu().is_none() {
                return Err("the menu closed at once".to_owned());
            }
            Ok(())
        }
        "sidebar-menu-click" => {
            let button = menu_item(harness, args.first().map(String::as_str))?;
            if !button.is_sensitive() {
                return Err(format!(
                    "{} is insensitive",
                    button.label().unwrap_or_default()
                ));
            }
            click_button(harness, &button).await
        }
        "action-log" => start_log(harness),
        "measure-triage" => measure(harness, arg(0), number(arg(1))?).await,
        "hold-reader-layout" => {
            let reader = harness.state.reader().ok_or("the reader isn't installed")?;
            reader.hold_layout(boolean(arg(0))?);
            Ok(())
        }
        "measure-leave" => {
            let bound = args.get(2).map(|ms| number(ms)).transpose()?;
            measure_leave(harness, arg(0), number(arg(1))?, bound).await
        }
        _ => check(harness, command, args).await,
    }
}

async fn check(harness: &Harness, assertion: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    let boolean = |text: &str| super::script::boolean(text);
    match assertion {
        "assert-focus" => {
            let window = harness.window()?;
            let focus = gtk::prelude::GtkWindowExt::focus(window.window());
            let wanted = focus_target(harness, arg(0))?;
            let within = focus
                .as_ref()
                .is_some_and(|focus| *focus == wanted || focus.is_ancestor(&wanted));
            let now = focus.map_or_else(
                || "nothing".to_owned(),
                |focus| format!("{} {}", focus.type_().name(), focus.widget_name()),
            );
            equal(&format!("focus in {} (now {now})", arg(0)), within, true)
        }
        "assert-list-selection" => {
            let positions = super::mail::list(harness)?.selected_positions();
            let text = if positions.is_empty() {
                "none".to_owned()
            } else {
                positions
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            equal("selected rows", text.as_str(), arg(0))
        }
        "assert-select-mode" => equal(
            "select mode",
            super::mail::list(harness)?.is_select_mode(),
            boolean(arg(0))?,
        ),
        "assert-reader-page" => equal(
            "reader page",
            harness.window()?.reader_page().as_str(),
            arg(0),
        ),
        "assert-pending" => equal(
            "ops waiting for a server",
            harness.state.pending_ops(),
            number(arg(0))? as u32,
        ),
        "assert-undoable" => equal(
            "undoable",
            harness
                .state
                .triage()
                .is_some_and(|triage| triage.can_undo()),
            boolean(arg(0))?,
        ),
        "assert-toasts" => equal("toasts on screen", toasts(harness)?.len(), number(arg(0))?),
        "assert-row-check" => {
            let row = row_widget(harness, arg(0))?;
            let state = match row.check_state() {
                None => "none",
                Some(true) => "true",
                Some(false) => "false",
            };
            equal(&format!("row {}'s checkbox", arg(0)), state, arg(1))
        }
        "assert-row-quick" => equal(
            &format!("row {}'s quick actions", arg(0)),
            row_widget(harness, arg(0))?.shows_quick(),
            boolean(arg(1))?,
        ),
        "assert-move-to" => {
            let picker = move_to::current();
            match arg(0) {
                "open" => equal("Move to… open", picker.is_some(), boolean(arg(1))?),
                "shown" => {
                    let shown = picker.ok_or("Move to… isn't open")?.shown();
                    let text = if shown.is_empty() {
                        "none".to_owned()
                    } else {
                        shown.join(",")
                    };
                    equal("folders shown", text.as_str(), arg(1))
                }
                "selected" => {
                    let picker = picker.ok_or("Move to… isn't open")?;
                    equal(
                        "folder Enter picks",
                        picker.selected().unwrap_or_default().as_str(),
                        arg(1),
                    )
                }
                other => Err(format!(
                    "assert-move-to checks open, shown or selected, not {other}"
                )),
            }
        }
        "assert-sidebar-menu" => {
            let state = match harness.window()?.sidebar().menu() {
                None => "closed",
                Some(_) => {
                    let item = menu_item(harness, args.get(1).map(String::as_str))?;
                    if item.is_sensitive() {
                        "enabled"
                    } else {
                        "disabled"
                    }
                }
            };
            equal("sidebar menu", state, arg(0))
        }
        "assert-action-log" => {
            let log = LOG
                .with(|log| log.borrow().clone())
                .ok_or("run action-log first")?;
            // The log is emptied when it matches, so `wait-until` can wait for a command that
            // runs late, as keys a WebView hands back do.
            let text = match log.borrow().as_slice() {
                [] => "none".to_owned(),
                ran => ran.join(","),
            };
            equal("commands run", text.as_str(), arg(0))?;
            log.borrow_mut().clear();
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// The open sidebar menu's item named `name`, else its first.
fn menu_item(harness: &Harness, name: Option<&str>) -> Result<gtk::Button, String> {
    let window = harness.window()?;
    let sidebar = window.sidebar();
    sidebar.menu().ok_or("no sidebar menu is open")?;
    let items = sidebar.menu_items();
    match name {
        None => items
            .into_iter()
            .next()
            .ok_or_else(|| "the menu holds no item".to_owned()),
        Some(name) => {
            let names: Vec<String> = items
                .iter()
                .map(|item| item.widget_name().to_string())
                .collect();
            items
                .into_iter()
                .find(|item| item.widget_name() == name)
                .ok_or_else(|| format!("the menu has no {name}, only {}", names.join(", ")))
        }
    }
}

/// The widget `name` names for `focus` and `assert-focus`: `list`, `search`, `reader`, the
/// Move to… picker's entry `move-to`, or a named widget.
fn focus_target(harness: &Harness, name: &str) -> Result<gtk::Widget, String> {
    match name {
        "list" => Ok(super::mail::list(harness)?.list_view().clone().upcast()),
        "search" => Ok(super::mail::list(harness)?.search_entry().clone().upcast()),
        "reader" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            if let Some(view) = reader.web_view() {
                return Ok(view.upcast());
            }
            let mut plain = None;
            super::collect(&reader.widget(), &mut |widget| {
                if plain.is_none() && widget.widget_name() == "reader-plain" {
                    plain = Some(widget.clone());
                }
            });
            Ok(plain.unwrap_or_else(|| reader.widget()))
        }
        "move-to" => move_to::current()
            .and_then(|picker| picker.search_entry())
            .map(|entry| entry.upcast())
            .ok_or_else(|| "Move to… isn't open".to_owned()),
        other => harness.widget(other),
    }
}

pub(super) fn row_widget(
    harness: &Harness,
    position: &str,
) -> Result<crate::list::row::MessageRow, String> {
    let position = number(position)? as u32;
    super::mail::list(harness)?
        .row_widget(position)
        .ok_or_else(|| format!("row {position} isn't bound"))
}

fn modifiers(spec: &str) -> Result<u32, String> {
    let mut state = 0;
    if spec == "none" {
        return Ok(state);
    }
    for part in spec.split('+') {
        state |= match part {
            "ctrl" => broadway::CONTROL,
            "shift" => broadway::SHIFT,
            "alt" => broadway::ALT,
            other => return Err(format!("use ctrl, shift or alt, not {other}")),
        };
    }
    Ok(state)
}

/// The centre of `widget` in the window's coordinates.
/// Where `widget`'s centre is on Broadway's screen: its surface's place there (a popover's
/// relative to the window's), the window's shadow, which the surface includes, and the widget's
/// place in its window or popover.
fn centre(harness: &Harness, widget: &gtk::Widget) -> Result<(f64, f64), String> {
    point(harness, widget, None)
}

/// Where the point `at` px from `widget`'s top left corner (its middle without one) is on the
/// Broadway screen.
fn point(
    harness: &Harness,
    widget: &gtk::Widget,
    at: Option<(f64, f64)>,
) -> Result<(f64, f64), String> {
    harness.window()?;
    if !widget.is_mapped() {
        return Err(format!("{} isn't on screen", widget.widget_name()));
    }
    let native = widget.native().ok_or("the widget is in no window")?;
    let bounds = widget
        .compute_bounds(&native)
        .ok_or("the widget has no bounds")?;
    let (mut left, mut top) = broadway::window_origin().map_err(|error| error.to_string())?;
    if let Some(popup) = native.surface().and_downcast::<gdk::Popup>() {
        left += f64::from(popup.position_x());
        top += f64::from(popup.position_y());
    }
    let (shadow_x, shadow_y) = native.surface_transform();
    let (x, y) = at.unwrap_or((
        f64::from(bounds.width() / 2.0),
        f64::from(bounds.height() / 2.0),
    ));
    Ok((
        left + shadow_x + f64::from(bounds.x()) + x,
        top + shadow_y + f64::from(bounds.y()) + y,
    ))
}

/// A real primary click `at` px from `widget`'s top left corner (in its middle without one).
pub(super) async fn click_at(
    harness: &Harness,
    widget: &gtk::Widget,
    at: Option<(f64, f64)>,
) -> StepResult {
    let (x, y) = point(harness, widget, at)?;
    let watch = releases(harness);
    broadway::click(x, y, 0, 1).map_err(|error| error.to_string())?;
    delivered(harness, watch, 1).await;
    harness.wait_until_settled().await;
    Ok(())
}

pub(super) async fn click_widget(
    harness: &Harness,
    widget: &gtk::Widget,
    state: u32,
    button: u32,
) -> StepResult {
    let (x, y) = centre(harness, widget)?;
    let watch = releases(harness);
    broadway::click(x, y, state, button).map_err(|error| error.to_string())?;
    delivered(harness, watch, 1).await;
    Ok(())
}

/// A real click on `button` through Broadway; the step fails unless the button saw it.
pub(super) async fn click_button(harness: &Harness, button: &gtk::Button) -> StepResult {
    let seen = Rc::new(Cell::new(false));
    let mark = seen.clone();
    let handler = button.connect_clicked(move |_| mark.set(true));
    let result = click_widget(harness, button.upcast_ref(), 0, 1).await;
    let started = Instant::now();
    while result.is_ok() && !seen.get() && started.elapsed() < Duration::from_millis(500) {
        glib::timeout_future(Duration::from_millis(2)).await;
    }
    if button.parent().is_some() {
        button.disconnect(handler);
    }
    result?;
    harness.wait_until_settled().await;
    clicked(seen.get(), &format!("the button {}", button.widget_name()))
}

fn clicked(seen: bool, what: &str) -> StepResult {
    if seen {
        Ok(())
    } else {
        Err(format!("{what} didn't take the click"))
    }
}

/// Waits until the pointer's arrival over `row` shows its quick actions (or half a second).
async fn hovered(harness: &Harness, row: &crate::list::row::MessageRow) -> StepResult {
    let started = Instant::now();
    while !row.shows_quick() && started.elapsed() < Duration::from_millis(500) {
        glib::timeout_future(Duration::from_millis(2)).await;
    }
    harness.wait_until_settled().await;
    Ok(())
}

/// Presses `spec`, a GTK accelerator (`j`, `<Shift>i`, `numbersign`, `<Control>z`), with the
/// keysym already chosen: a letter with Shift arrives as its capital, as GDK delivers it.
fn press(spec: &str) -> StepResult {
    let (keyval, mods) =
        gtk::accelerator_parse(spec).ok_or_else(|| format!("GTK can't parse the key {spec}"))?;
    let mut state = 0;
    let mut keyval = keyval;
    if mods.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
        state |= broadway::SHIFT;
        keyval = keyval.to_upper();
    }
    if mods.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
        state |= broadway::CONTROL;
    }
    if mods.contains(gtk::gdk::ModifierType::ALT_MASK) {
        state |= broadway::ALT;
    }
    broadway::key(keyval.into_glib(), state).map_err(|error| error.to_string())
}

/// Lets the input reach GTK through the daemon and be handled: the window sees each key or
/// button release after the press was handled (a popover's own surface isn't watched, so input
/// there is given half a second).
async fn delivered(harness: &Harness, watch: Option<(Rc<Cell<u64>>, u64)>, count: u64) {
    let started = Instant::now();
    loop {
        glib::timeout_future(Duration::from_millis(2)).await;
        let caught_up = watch
            .as_ref()
            .is_some_and(|(counter, before)| counter.get() >= before + count);
        if caught_up || started.elapsed() > Duration::from_millis(500) {
            break;
        }
    }
    harness.wait_until_settled().await;
}

thread_local! {
    /// Key and button releases the window has seen, counted by an event controller of its own.
    static RELEASES: RefCell<Option<Watched>> = const { RefCell::new(None) };
}

/// The window whose releases are counted, and the counter.
type Watched = (glib::WeakRef<gtk::Window>, Rc<Cell<u64>>);

/// The window's release counter and its count now; installed on the first use.
fn releases(harness: &Harness) -> Option<(Rc<Cell<u64>>, u64)> {
    let window: gtk::Window = harness.window().ok()?.window().clone().upcast();
    let counter = RELEASES.with(|releases| {
        let mut releases = releases.borrow_mut();
        match releases.as_ref() {
            Some((watched, counter)) if watched.upgrade().as_ref() == Some(&window) => {
                counter.clone()
            }
            _ => {
                let counter: Rc<Cell<u64>> = Rc::default();
                let legacy = gtk::EventControllerLegacy::new();
                legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
                let count = counter.clone();
                legacy.connect_event(move |_, event| {
                    if matches!(
                        event.event_type(),
                        gtk::gdk::EventType::KeyRelease | gtk::gdk::EventType::ButtonRelease
                    ) {
                        count.set(count.get() + 1);
                    }
                    glib::Propagation::Proceed
                });
                window.add_controller(legacy);
                releases.replace((window.downgrade(), counter.clone()));
                counter
            }
        }
    });
    let before = counter.get();
    Some((counter, before))
}

/// The toast widgets on screen; libadwaita registers their type with the first toast.
fn toasts(harness: &Harness) -> Result<Vec<gtk::Widget>, String> {
    let mut found = Vec::new();
    let Some(toast) = glib::Type::from_name("AdwToastWidget") else {
        return Ok(found);
    };
    super::collect(harness.window()?.window().upcast_ref(), &mut |widget| {
        if widget.type_() == toast && widget.is_mapped() {
            found.push(widget.clone());
        }
    });
    Ok(found)
}

/// The buttons of the toasts on screen.
fn toast_buttons(harness: &Harness) -> Result<Vec<gtk::Button>, String> {
    let mut buttons = Vec::new();
    for toast in toasts(harness)? {
        super::collect(&toast, &mut |widget| {
            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && button.is_mapped()
            {
                buttons.push(button.clone());
            }
        });
    }
    Ok(buttons)
}

/// `triage-account NAME KEY`: an account `accounts.toml` would list, with its six role folders.
async fn add_account(harness: &Harness, name: &str, key: &str) -> StepResult {
    let mailbox = harness.state.mailbox();
    let local: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    let reply = mailbox
        .store()
        .ok_or("the store is not open")?
        .ensure_account(
            AccountKey(key.to_owned()),
            NewAccount {
                name: name.to_owned(),
                address: format!("{local}@example.test"),
            },
        );
    let account = reply.recv().await.map_err(|error| error.to_string())?.value;
    for (role, folder) in [
        (FolderRole::Inbox, "INBOX"),
        (FolderRole::Drafts, "Drafts"),
        (FolderRole::Sent, "Sent"),
        (FolderRole::Archive, "Archive"),
        (FolderRole::Junk, "Junk"),
        (FolderRole::Trash, "Trash"),
    ] {
        let reply = mailbox
            .store()
            .ok_or("the store is not open")?
            .upsert_folder(FolderSpec::local(account, folder, role));
        reply.recv().await.map_err(|error| error.to_string())?;
    }
    let started = Instant::now();
    while !mailbox.folders().iter().any(|f| f.account == account) {
        if started.elapsed() > WAIT_TIMEOUT {
            return Err("the sidebar never got the new account's folders".to_owned());
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    println!("  # account {}", account.0);
    super::mail::settled(harness).await
}

/// `action-log`: every command of the window and the application, as it runs.
fn start_log(harness: &Harness) -> StepResult {
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let window = harness.window()?;
    for id in ActionId::ALL {
        if id.kind() != ActionKind::Command {
            continue;
        }
        let action = if id.scope() == Scope::App {
            harness.app.lookup_action(id.name())
        } else {
            window.window().lookup_action(id.name())
        };
        let Some(action) = action.and_downcast::<gio::SimpleAction>() else {
            continue;
        };
        let record = log.clone();
        let name = id.detailed_name();
        action.connect_activate(move |_, _| record.borrow_mut().push(name.clone()));
    }
    LOG.with(|current| current.replace(Some(log)));
    Ok(())
}

/// One timed command: frames and milliseconds from its handler.
struct Sample {
    frames: i64,
    to_change: f64,
    to_paint: f64,
    key_to_handler: f64,
}

/// `measure-triage KEY COUNT`: presses KEY on the selected message COUNT times and times each,
/// from the command's handler to the list's change (rows removed, or the row's flags) and to
/// the first painted frame after it. PRD: "Actions: local result in the same frame".
async fn measure(harness: &Harness, key: &str, count: usize) -> StepResult {
    let list = super::mail::list(harness)?;
    let window = harness.window()?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    clock.begin_updating();
    let mut samples = Vec::new();
    let mut failure = None;
    for _ in 0..count {
        let Some(position) = list.selected_positions().first().copied() else {
            failure = Some("nothing is selected".to_owned());
            break;
        };
        let changed: Rc<Cell<Option<Instant>>> = Rc::default();
        let painted: Rc<Cell<Option<(Instant, i64)>>> = Rc::default();
        let mark = changed.clone();
        let items = list.model().connect_items_changed(move |_, _, _, _| {
            if mark.get().is_none() {
                mark.set(Some(Instant::now()));
            }
        });
        let item = list
            .model()
            .item(position)
            .and_downcast::<crate::list::item::MessageItem>()
            .ok_or("the selected row has no item")?;
        let mark = changed.clone();
        let flags = item.connect_changed(move |_| {
            if mark.get().is_none() {
                mark.set(Some(Instant::now()));
            }
        });
        let (seen, record) = (changed.clone(), painted.clone());
        let paint = clock.connect_after_paint(move |clock| {
            if seen.get().is_some() && record.get().is_none() {
                record.set(Some((Instant::now(), clock.frame_counter())));
            }
        });
        let pressed = Instant::now();
        let result = press(key);
        let started = Instant::now();
        while result.is_ok() && painted.get().is_none() && started.elapsed() < WAIT_TIMEOUT {
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        list.model().disconnect(items);
        item.disconnect(flags);
        clock.disconnect(paint);
        result?;
        let (Some(changed), Some((painted, frame)), Some((handled, handler_frame))) =
            (changed.get(), painted.get(), harness.state.triage_started())
        else {
            failure = Some("the list never showed the change".to_owned());
            break;
        };
        if handled < pressed {
            failure = Some("the command's handler didn't run".to_owned());
            break;
        }
        let ms = |from: Instant, to: Instant| to.duration_since(from).as_secs_f64() * 1000.0;
        samples.push(Sample {
            frames: frame - handler_frame,
            to_change: ms(handled, changed),
            to_paint: ms(handled, painted),
            key_to_handler: ms(pressed, handled),
        });
        super::mail::settled(harness).await?;
        reader_caught_up(harness, &list).await?;
    }
    clock.end_updating();
    if let Some(failure) = failure {
        return Err(failure);
    }
    let mut frames: Vec<i64> = samples.iter().map(|sample| sample.frames).collect();
    frames.sort_unstable();
    let summary = |values: Vec<f64>| {
        format!(
            "p50 {:.2}, p95 {:.2}, max {:.2}",
            nearest_rank(&values, 0.5),
            nearest_rank(&values, 0.95),
            nearest_rank(&values, 1.0)
        )
    };
    let same = frames.iter().filter(|frames| **frames <= 1).count();
    println!(
        "  # {key} ×{}: the next frame showed the change {same} times; frames after the \
         handler's {frames:?}",
        samples.len()
    );
    let to_change: Vec<f64> = samples.iter().map(|sample| sample.to_change).collect();
    println!(
        "  # handler → list changed (ms): {}",
        summary(to_change.clone())
    );
    println!(
        "  # handler → frame painted (ms): {}",
        summary(samples.iter().map(|sample| sample.to_paint).collect())
    );
    println!(
        "  # key sent → handler (ms, Broadway's delivery): {}",
        summary(samples.iter().map(|sample| sample.key_to_handler).collect())
    );
    // The list must hold the change before the next frame at 60 Hz (PRD: "local result in the
    // same frame"); Broadway paints unpaced, so its frame counts don't judge it.
    let p95 = nearest_rank(&to_change, 0.95);
    if p95 <= FRAME_MS {
        Ok(())
    } else {
        Err(format!(
            "handler → list changed p95 {p95:.2} ms, over a {FRAME_MS:.1} ms frame"
        ))
    }
}

/// `measure-leave KEY COUNT [MS]`: presses KEY (an action that takes the selected message out of
/// the list) COUNT times on the message the reader shows, and times each from the command's
/// handler to the first painted frame without that message in the reader, and to the frame that
/// shows the next one. Prints the times and the next message's phases; with `bound` (MS), fails
/// when the message stayed longer in any round.
async fn measure_leave(
    harness: &Harness,
    key: &str,
    count: usize,
    bound: Option<usize>,
) -> StepResult {
    let list = super::mail::list(harness)?;
    let reader = harness.state.reader().ok_or("the reader isn't installed")?;
    let window = harness.window()?;
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let shown: Rc<RefCell<Option<(crate::reader::Shown, Instant)>>> = Rc::default();
    let record = shown.clone();
    reader.connect_shown(move |report| {
        record.replace(Some((report.clone(), Instant::now())));
    });
    clock.begin_updating();
    let ms = |from: Instant, to: Instant| to.duration_since(from).as_secs_f64() * 1000.0;
    let mut gone_ms = Vec::new();
    let mut next_ms = Vec::new();
    let mut failure = None;
    for round in 0..count {
        let Some(left) = reader.message().filter(|id| list.selected() == Some(*id)) else {
            failure = Some(format!(
                "round {round}: the reader doesn't show the selected message"
            ));
            break;
        };
        shown.replace(None);
        let gone: Rc<Cell<Option<Instant>>> = Rc::default();
        let next: Rc<Cell<Option<Instant>>> = Rc::default();
        let (mark_gone, mark_next, watched) = (gone.clone(), next.clone(), reader.clone());
        let paint = clock.connect_after_paint(move |_| {
            let now = watched.message();
            if mark_gone.get().is_none() && now != Some(left) {
                mark_gone.set(Some(Instant::now()));
            }
            if mark_next.get().is_none() && now.is_some_and(|id| id != left) {
                mark_next.set(Some(Instant::now()));
            }
        });
        let pressed = Instant::now();
        let result = press(key);
        let started = Instant::now();
        while result.is_ok() && next.get().is_none() && started.elapsed() < WAIT_TIMEOUT {
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        clock.disconnect(paint);
        result?;
        let handled = harness
            .state
            .triage_started()
            .map(|(at, _)| at)
            .filter(|at| *at >= pressed)
            .unwrap_or(pressed);
        let (Some(gone), Some(next)) = (gone.get(), next.get()) else {
            failure = Some(format!(
                "round {round}: the reader never showed the next message"
            ));
            break;
        };
        gone_ms.push(ms(handled, gone));
        next_ms.push(ms(handled, next));
        if let Some((report, at)) = shown.borrow().as_ref() {
            let asked = at.checked_sub(report.ready).unwrap_or(*at);
            println!(
                "  # round {round}: gone {:.1} ms, next {:.1} ms; the reader asked at {:.1}, \
                 read {:.1}, prepared {:.1}, ready {:.1} ms after, painted {:.1} ms after; {:?}",
                ms(handled, gone),
                ms(handled, next),
                ms(handled, asked),
                report.fetched.as_secs_f64() * 1000.0,
                report.prepared.as_secs_f64() * 1000.0,
                report.ready.as_secs_f64() * 1000.0,
                ms(*at, next),
                report.body
            );
        }
        super::mail::settled(harness).await?;
        glib::timeout_future(Duration::from_millis(300)).await;
    }
    clock.end_updating();
    if let Some(failure) = failure {
        return Err(failure);
    }
    let summary = |values: &[f64]| {
        format!(
            "p50 {:.1}, p95 {:.1}, max {:.1}",
            nearest_rank(values, 0.5),
            nearest_rank(values, 0.95),
            nearest_rank(values, 1.0)
        )
    };
    println!(
        "  # {key} ×{}: handler → the message gone from the reader (ms): {}",
        gone_ms.len(),
        summary(&gone_ms)
    );
    println!(
        "  # handler → the next message shown (ms): {}",
        summary(&next_ms)
    );
    let longest = nearest_rank(&gone_ms, 1.0);
    match bound {
        Some(bound) if longest > bound as f64 => Err(format!(
            "a message that left stayed {longest:.1} ms in the reader, over {bound} ms"
        )),
        _ => Ok(()),
    }
}

/// Waits until the reader shows the list's one selected message and has painted it, so a sample doesn't
/// time the frame that draws the previous action's next message: under Broadway, which draws in
/// software, that frame takes 40–60 ms and blocks the main loop, and a key pressed just before
/// it waits behind it (the junk step's ~40 ms "stall", 2026-10-12).
async fn reader_caught_up(harness: &Harness, list: &crate::list::MessageList) -> StepResult {
    let Some(reader) = harness.state.reader() else {
        return Ok(());
    };
    let started = Instant::now();
    while let Some(selected) = list.selected()
        && reader.message() != Some(selected)
        && started.elapsed() < WAIT_TIMEOUT
    {
        glib::timeout_future(Duration::from_millis(2)).await;
    }
    harness.wait_frames(2).await
}

/// One frame at 60 Hz, in milliseconds.
const FRAME_MS: f64 = 1000.0 / 60.0;

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
