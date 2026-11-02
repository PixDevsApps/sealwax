//! Self-test commands for the accessibility pass (M9, ML-092): an audit of what a screen
//! reader is told about every control, and the keyboard's Tab order through Broadway. GTK's
//! test backend (`GTK_A11Y=test`, which the harness switches on) keeps every accessible
//! property, relation and state; `gtk_test_accessible_check_*` hands back the value it holds
//! when it isn't the one asked for, which is how the audit reads them.
//!
//! | Command | What it does |
//! | --- | --- |
//! | `assert-accessible [window\|dialog\|composer\|all]` | Every control on screen in the main window (with its dialogs and popovers), its top dialog, the newest composer, or every window has an accessible name (its label property, a `labelled-by` relation, or its own text where the role takes its name from its content; a tooltip doesn't count), a role that says what it is, and the states its role needs; icon-only buttons also have a tooltip. Lists each offender |
//! | `accessible-tree [window\|composer\|all]` | Prints every control on screen: role, name, states, the Tab stops' names |
//! | `assert-accessible-name NAME TEXT` | The widget named NAME has the accessible name TEXT (as the audit computes it) |
//! | `assert-accessible-role NAME ROLE` | Its role (GTK's name for it, in lower case: `button`, `toggle-button`, `list-item`…) |
//! | `assert-accessible-state NAME STATE VALUE` | Its state: `pressed`, `checked`, `expanded`, `selected`, `disabled`, `invalid`, as `true`, `false`, `mixed` or `undefined` |
//! | `tab-chain window\|composer forward\|backward [MAX]` | Presses Tab (Shift+Tab backward) as real key presses until the focus is back where it started, at most MAX times (60); prints each stop. Fails when a press leaves the focus nowhere or the chain never comes back |
//! | `assert-tab-chain STOP,STOP,…` | The last chain, from where it started, passed these stops in this order (others may lie between). A stop is the nearest named widget around the focus |
//! | `assert-tab-stops N` | The last chain had N distinct stops |
//! | `assert-focus-stop STOP [window\|composer]` | The keyboard focus is at STOP (as `tab-chain` names stops) in the main window or the newest composer |
//! | `assert-dialog TITLE\|none` | The main window's top dialog has that title, or none is open |
//! | `menu-open NAME`, `menu-close NAME` | Opens or closes the menu of the menu button NAME, as a click does |
//! | `link-hooks` | The app's reader records the links it would open instead of opening them |
//! | `link-dialog-answer cancel\|open` | Answers the reader's "Open this link?" dialog |
//! | `assert-link-strip TEXT\|hidden` | The reader's link strip shows TEXT, or is hidden |
//! | `assert-link-opened URI\|none` | The last link the reader opened (after `link-hooks`) |
//! | `assert-link-dialog open\|closed` | The reader's "Open this link?" dialog |
//! | `assert-web-focus MESSAGE SELECTOR\|none` | In MESSAGE's HTML body (a sample name), the element with the keyboard focus matches SELECTOR, or nothing in the page has it |
//! | `assert-shortcut TITLE built\|"not built"` | The open Keyboard Shortcuts dialog lists command TITLE as built, or as "Not available yet" |
//! | `assert-announced TEXT\|none` | What was announced to screen readers since the last check, joined by ` \| ` |

use super::{Harness, StepResult, broadway, equal, number};
use gtk4 as gtk;
use gtk4::glib::translate::{IntoGlib, ToGlibPtr};
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char, c_int};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The commands this module adds to the script format, with their argument counts.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    ("assert-accessible", 0, 1),
    ("accessible-tree", 0, 1),
    ("assert-accessible-name", 2, 2),
    ("assert-accessible-role", 2, 2),
    ("assert-accessible-state", 3, 3),
    ("tab-chain", 2, 3),
    ("assert-tab-chain", 1, 1),
    ("assert-tab-stops", 1, 1),
    ("assert-focus-stop", 1, 2),
    ("assert-dialog", 1, 1),
    ("assert-announced", 1, 1),
    ("assert-shortcut", 2, 2),
    ("menu-open", 1, 1),
    ("menu-close", 1, 1),
    ("link-hooks", 0, 0),
    ("link-dialog-answer", 1, 1),
    ("assert-link-strip", 1, 1),
    ("assert-link-opened", 1, 1),
    ("assert-link-dialog", 1, 1),
    ("assert-web-focus", 2, 2),
];

/// Tab presses `tab-chain` makes at most when no MAX is given.
const MAX_STOPS: usize = 60;

thread_local! {
    /// The stops of the last `tab-chain`, one per press.
    static CHAIN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// The links the reader opened since `link-hooks`.
    static LINKS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub fn handles(command: &str) -> bool {
    COMMANDS.iter().any(|(name, _, _)| *name == command)
}

pub async fn step(harness: &Harness, command: &str, args: &[String]) -> StepResult {
    let arg = |index: usize| args[index].as_str();
    match command {
        "assert-accessible" => {
            let mut problems = Vec::new();
            for root in roots(harness, args.first().map_or("window", String::as_str))? {
                problems.extend(audit(&root));
            }
            if problems.is_empty() {
                Ok(())
            } else {
                Err(problems.join("\n"))
            }
        }
        "accessible-tree" => {
            for root in roots(harness, args.first().map_or("window", String::as_str))? {
                print_tree(&root);
            }
            Ok(())
        }
        "assert-accessible-name" => {
            let widget = named(harness, arg(0))?;
            equal(
                &format!("{}'s accessible name", arg(0)),
                name(&widget).unwrap_or_default().as_str(),
                arg(1),
            )
        }
        "assert-accessible-role" => {
            let widget = named(harness, arg(0))?;
            equal(
                &format!("{}'s role", arg(0)),
                role_name(widget.accessible_role()).as_str(),
                arg(1),
            )
        }
        "assert-accessible-state" => {
            let widget = named(harness, arg(0))?;
            let state = state_by_name(arg(1))?;
            equal(
                &format!("{}'s {} state", arg(0), arg(1)),
                state_value(&widget, state).as_str(),
                arg(2),
            )
        }
        "tab-chain" => {
            let backward = match arg(1) {
                "forward" => false,
                "backward" => true,
                other => return Err(format!("use forward or backward, not {other}")),
            };
            let max = args.get(2).map_or(Ok(MAX_STOPS), |max| number(max))?;
            tab_chain(harness, arg(0), backward, max).await
        }
        "assert-tab-chain" => {
            let chain = CHAIN.with(|chain| chain.borrow().clone());
            let wanted: Vec<&str> = arg(0).split(',').collect();
            let mut next = 0;
            for stop in &chain {
                if next < wanted.len() && stop == wanted[next] {
                    next += 1;
                }
            }
            if next == wanted.len() {
                Ok(())
            } else {
                Err(format!(
                    "the chain never reached {} after {}: {}",
                    wanted[next],
                    if next == 0 {
                        "its start"
                    } else {
                        wanted[next - 1]
                    },
                    collapsed(&chain).join(" → ")
                ))
            }
        }
        "assert-focus-stop" => {
            let window: gtk::Window = match args.get(1).map_or("window", String::as_str) {
                "composer" => composer_window()?,
                _ => harness.window()?.window().clone().upcast(),
            };
            let now = gtk::prelude::GtkWindowExt::focus(&window)
                .map_or_else(|| "nothing".to_owned(), |focus| stop(&focus));
            equal("the focus", now.as_str(), arg(0))
        }
        "assert-tab-stops" => {
            let chain = CHAIN.with(|chain| chain.borrow().clone());
            let mut distinct = chain.clone();
            distinct.sort();
            distinct.dedup();
            equal("distinct Tab stops", distinct.len(), number(arg(0))?)
        }
        "assert-dialog" => {
            let window = harness.window()?;
            let title = window
                .window()
                .visible_dialog()
                .map(|dialog| dialog_title(&dialog));
            equal(
                "the window's dialog",
                title.as_deref().unwrap_or("none"),
                arg(0),
            )
        }
        "menu-open" | "menu-close" => {
            let button = named(harness, arg(0))?
                .downcast::<gtk::MenuButton>()
                .map_err(|_| format!("{} isn't a menu button", arg(0)))?;
            if command == "menu-open" {
                button.popup();
            } else {
                button.popdown();
            }
            harness.wait_until_settled().await;
            harness.wait_frames(1).await
        }
        "link-hooks" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            LINKS.with(|links| links.borrow_mut().clear());
            reader.set_link_opener(|_, uri| {
                LINKS.with(|links| links.borrow_mut().push(uri.to_owned()));
            });
            Ok(())
        }
        "link-dialog-answer" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            let dialog = reader.link_dialog().ok_or("no link dialog is open")?;
            dialog.emit_by_name::<()>("response", &[&arg(0)]);
            dialog.force_close();
            harness.wait_until_settled().await;
            Ok(())
        }
        "assert-link-strip" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            let status = reader.link_state().status;
            equal(
                "the link strip",
                status.as_deref().unwrap_or("hidden"),
                arg(0),
            )
        }
        "assert-link-opened" => {
            let last = LINKS.with(|links| links.borrow().last().cloned());
            equal(
                "the last link opened",
                last.as_deref().unwrap_or("none"),
                arg(0),
            )
        }
        "assert-link-dialog" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            let open = reader
                .link_dialog()
                .is_some_and(|dialog| dialog.is_mapped());
            equal(
                "the link dialog",
                if open { "open" } else { "closed" },
                arg(0),
            )
        }
        "assert-web-focus" => {
            let reader = harness.state.reader().ok_or("there is no reader")?;
            let id = super::conversation::id(arg(0))?;
            let script = format!(
                "const active = document.activeElement; \
                 if (!active || active === document.body || !document.hasFocus()) return 'none'; \
                 return active.matches({selector:?}) ? 'match' : active.outerHTML.slice(0, 80);",
                selector = arg(1)
            );
            let found = reader.eval_in(id, &script).await?.to_str().to_string();
            match (arg(1), found.as_str()) {
                ("none", "none") | (_, "match") => Ok(()),
                (wanted, found) => Err(format!("the page's focus is {found}, not {wanted}")),
            }
        }
        "assert-shortcut" => {
            let dialog = harness
                .window()?
                .window()
                .visible_dialog()
                .ok_or("no dialog is open")?;
            let mut found = None;
            walk(dialog.upcast_ref(), &mut |widget| {
                if found.is_none()
                    && widget.type_().name() == "AdwShortcutRow"
                    && let Some(spoken) = name(widget)
                    && spoken.starts_with(&format!("{} ", arg(0)))
                {
                    found = Some(spoken);
                }
            });
            let spoken = found.ok_or_else(|| format!("no row for {}", arg(0)))?;
            let built = if spoken.contains("Not available yet") {
                "not built"
            } else {
                "built"
            };
            equal(&format!("{} ({spoken})", arg(0)), built, arg(1))
        }
        "assert-announced" => {
            let spoken = crate::accessibility::take_announcements();
            let text = if spoken.is_empty() {
                "none".to_owned()
            } else {
                spoken.join(" | ")
            };
            equal("announced", text.as_str(), arg(0))
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// The windows a command looks at.
fn roots(harness: &Harness, which: &str) -> Result<Vec<gtk::Widget>, String> {
    match which {
        "window" => Ok(vec![harness.window()?.window().clone().upcast()]),
        "composer" => Ok(vec![composer_root()?]),
        "dialog" => harness
            .window()?
            .window()
            .visible_dialog()
            .map(|dialog| vec![dialog.upcast()])
            .ok_or_else(|| "no dialog is open".to_owned()),
        "all" => Ok(gtk::Window::list_toplevels()
            .into_iter()
            .filter(|window| window.is_mapped())
            .collect()),
        other => Err(format!("use window, dialog, composer or all, not {other}")),
    }
}

/// The newest composer card.
fn composer_root() -> Result<gtk::Widget, String> {
    crate::composer::composers()
        .last()
        .map(|composer| composer.root().clone().upcast())
        .ok_or_else(|| "there is no composer".to_owned())
}

/// The window the newest composer card is docked in.
fn composer_window() -> Result<gtk::Window, String> {
    crate::composer::composers()
        .last()
        .and_then(|composer| composer.window())
        .map(|window| window.upcast())
        .ok_or_else(|| "there is no composer".to_owned())
}

/// The widget named `name` in any window, the setup assistant's included.
fn named(harness: &Harness, name: &str) -> Result<gtk::Widget, String> {
    if let Ok(widget) = harness.widget(name) {
        return Ok(widget);
    }
    for window in gtk::Window::list_toplevels() {
        let mut found = None;
        super::collect(&window, &mut |widget| {
            if found.is_none() && widget.widget_name() == name {
                found = Some(widget.clone());
            }
        });
        if let Some(widget) = found {
            return Ok(widget);
        }
    }
    Err(format!("no widget named {name}"))
}

/// A dialog's title, or its heading for an alert.
fn dialog_title(dialog: &adw::Dialog) -> String {
    match dialog.downcast_ref::<adw::AlertDialog>() {
        Some(alert) if dialog.title().is_empty() => alert.heading().unwrap_or_default().into(),
        _ => dialog.title().into(),
    }
}

// --- Reading what GTK's test backend holds ---------------------------------------------------

/// What `check` hands back: `None` when the value equals the one asked for, else the value
/// held, printed by GTK.
fn checked(result: *mut c_char) -> Option<String> {
    if result.is_null() {
        return None;
    }
    // SAFETY: GTK returns a newly allocated, NUL-terminated string, freed here once copied.
    unsafe {
        let text = CStr::from_ptr(result).to_string_lossy().into_owned();
        glib::ffi::g_free(result.cast());
        Some(text)
    }
}

/// The label property, when one is set.
fn label_property(widget: &gtk::Widget) -> Option<String> {
    if !gtk::test_accessible_has_property(widget, gtk::AccessibleProperty::Label) {
        return None;
    }
    // A value no label has, so GTK always hands back the one it holds.
    let probe = c"\u{1}";
    let accessible: &gtk::Accessible = widget.upcast_ref();
    // SAFETY: the label property takes one string; the widget outlives the call.
    let held = checked(unsafe {
        gtk::ffi::gtk_test_accessible_check_property(
            accessible.to_glib_none().0,
            gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
            probe.as_ptr(),
        )
    });
    held.filter(|label| !label.trim().is_empty())
}

/// The level property (a tree row's depth from 1), as GTK prints it; `None` when it isn't set.
pub(super) fn level(widget: &gtk::Widget) -> Option<String> {
    if !gtk::test_accessible_has_property(widget, gtk::AccessibleProperty::Level) {
        return None;
    }
    let accessible: &gtk::Accessible = widget.upcast_ref();
    // SAFETY: the level property takes one integer; no level is 0, so GTK prints the one held.
    let held = checked(unsafe {
        gtk::ffi::gtk_test_accessible_check_property(
            accessible.to_glib_none().0,
            gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LEVEL,
            0 as c_int,
        )
    });
    Some(held.unwrap_or_else(|| "0".to_owned()))
}

/// What the labelled-by relation holds, as GTK prints it; `None` when it isn't set.
fn labelled_by(widget: &gtk::Widget) -> Option<String> {
    if !gtk::test_accessible_has_relation(widget, gtk::AccessibleRelation::LabelledBy) {
        return None;
    }
    let accessible: &gtk::Accessible = widget.upcast_ref();
    // SAFETY: a reference-list relation takes the references and a closing NULL; none is
    // given, so GTK compares an empty list with the one it holds and prints that.
    let held = checked(unsafe {
        gtk::ffi::gtk_test_accessible_check_relation(
            accessible.to_glib_none().0,
            gtk::ffi::GTK_ACCESSIBLE_RELATION_LABELLED_BY,
            std::ptr::null::<c_char>(),
        )
    });
    Some(held.unwrap_or_default())
}

/// A state's value as GTK prints it: `true`, `false`, `mixed`, `undefined`.
pub(super) fn state_value(widget: &gtk::Widget, state: gtk::AccessibleState) -> String {
    if !gtk::test_accessible_has_state(widget, state) {
        return "undefined".to_owned();
    }
    let accessible: &gtk::Accessible = widget.upcast_ref();
    // SAFETY: every state this module reads takes one integer (a boolean, a tristate or an
    // enum, of which 0 is a valid value).
    let held = checked(unsafe {
        gtk::ffi::gtk_test_accessible_check_state(
            accessible.to_glib_none().0,
            state.into_glib(),
            0 as c_int,
        )
    });
    held.unwrap_or_else(|| match state {
        gtk::AccessibleState::Invalid => "false".to_owned(),
        _ => "false".to_owned(),
    })
}

fn state_by_name(name: &str) -> Result<gtk::AccessibleState, String> {
    Ok(match name {
        "pressed" => gtk::AccessibleState::Pressed,
        "checked" => gtk::AccessibleState::Checked,
        "expanded" => gtk::AccessibleState::Expanded,
        "selected" => gtk::AccessibleState::Selected,
        "disabled" => gtk::AccessibleState::Disabled,
        "invalid" => gtk::AccessibleState::Invalid,
        "busy" => gtk::AccessibleState::Busy,
        other => return Err(format!("no state {other}")),
    })
}

/// GTK's name for a role, as its enum's nick: `button`, `toggle-button`, `list-item`.
fn role_name(role: gtk::AccessibleRole) -> String {
    glib::EnumValue::from_value(&role.to_value())
        .map_or_else(|| format!("{role:?}"), |(_, value)| value.nick().to_owned())
}

// --- The name a screen reader is given ---------------------------------------------------------

/// Roles whose name comes from their content when nothing names them (WAI-ARIA's "name from
/// content", which GTK follows).
fn names_from_content(role: gtk::AccessibleRole) -> bool {
    use gtk::AccessibleRole as R;
    matches!(
        role,
        R::Button
            | R::Cell
            | R::Checkbox
            | R::ColumnHeader
            | R::GridCell
            | R::Heading
            | R::Label
            | R::Link
            | R::ListItem
            | R::MenuItem
            | R::MenuItemCheckbox
            | R::MenuItemRadio
            | R::Option
            | R::Radio
            | R::Row
            | R::RowHeader
            | R::Switch
            | R::Tab
            | R::ToggleButton
            | R::Tooltip
            | R::TreeItem
    )
}

/// The accessible name: a labelled-by relation, else the label property, else, for a role
/// that takes it, the names of what it holds. A tooltip doesn't name anything here.
pub(super) fn name(widget: &gtk::Widget) -> Option<String> {
    if let Some(by) = labelled_by(widget)
        && let Some(text) = referenced_text(widget, &by)
    {
        return Some(text);
    }
    if let Some(label) = label_property(widget) {
        return Some(label);
    }
    if names_from_content(widget.accessible_role()) {
        let text = content_name(widget);
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    None
}

/// The text of the widgets a labelled-by relation points at, found by their addresses in
/// GTK's print of the relation (`GtkLabel<0x…>`), among the widgets of `widget`'s window.
fn referenced_text(widget: &gtk::Widget, printed: &str) -> Option<String> {
    let root = widget.root()?;
    let mut parts = Vec::new();
    for reference in printed
        .split(['[', ']', ',', ' '])
        .filter(|part| !part.is_empty())
    {
        let Some(address) = reference
            .split_once('<')
            .and_then(|(_, rest)| rest.strip_suffix('>'))
        else {
            continue;
        };
        let mut found = None;
        super::collect(root.upcast_ref(), &mut |candidate| {
            if found.is_none() && format!("{:p}", candidate.as_ptr()) == address {
                found = Some(candidate.clone());
            }
        });
        if let Some(target) = found {
            let text = label_property(&target).unwrap_or_else(|| content_name(&target));
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
    }
    if parts.is_empty() {
        // Referenced but not found (outside the window, or printed otherwise): still a name.
        Some(printed.to_owned())
    } else {
        Some(parts.join(" "))
    }
}

/// The names of what `widget` holds, in order: each child's label property, else its own
/// content; hidden children and their content don't count.
fn content_name(widget: &gtk::Widget) -> String {
    let mut parts = Vec::new();
    let mut child = widget.first_child();
    while let Some(current) = child {
        if current.is_visible() && !is_popup(&current) {
            if let Some(label) = label_property(&current) {
                parts.push(label);
            } else if let Some(label) = current.downcast_ref::<gtk::Label>() {
                if !label.label().trim().is_empty() {
                    parts.push(label.text().to_string());
                }
            } else {
                let inner = content_name(&current);
                if !inner.trim().is_empty() {
                    parts.push(inner);
                }
            }
        }
        child = current.next_sibling();
    }
    parts.join(" ")
}

/// A popover or other surface of its own inside a widget: not part of its name.
fn is_popup(widget: &gtk::Widget) -> bool {
    widget.is::<gtk::Popover>() || widget.is::<gtk::Native>()
}

// --- The audit -----------------------------------------------------------------------------------

/// Roles of controls a user operates: each needs a name.
fn is_control_role(role: gtk::AccessibleRole) -> bool {
    use gtk::AccessibleRole as R;
    matches!(
        role,
        R::Button
            | R::Checkbox
            | R::ComboBox
            | R::Link
            | R::MenuItem
            | R::MenuItemCheckbox
            | R::MenuItemRadio
            | R::Option
            | R::Radio
            | R::SearchBox
            | R::Slider
            | R::SpinButton
            | R::Switch
            | R::Tab
            | R::TextBox
            | R::ToggleButton
            | R::TreeItem
    )
}

/// Roles that tell a screen reader nothing about what the widget does.
fn is_silent_role(role: gtk::AccessibleRole) -> bool {
    use gtk::AccessibleRole as R;
    matches!(
        role,
        R::Generic | R::Group | R::None | R::Presentation | R::Widget
    )
}

/// Roles a focusable container needs a name for: what Orca says as the focus enters it.
fn is_named_container(role: gtk::AccessibleRole) -> bool {
    use gtk::AccessibleRole as R;
    matches!(
        role,
        R::List
            | R::ListBox
            | R::ListItem
            | R::Row
            | R::Grid
            | R::Tree
            | R::TreeGrid
            | R::Table
            | R::Document
            | R::Dialog
            | R::AlertDialog
            | R::Menu
            | R::MenuBar
            | R::TabList
            | R::TabPanel
    )
}

/// GTK's and libadwaita's own parts, which name themselves or are left to them: a menu
/// button's inner toggle, a text entry's inner text, a drop-down's insides (the drop-down is
/// checked), the window controls, a toast's parts, an alert dialog's scrolled body.
fn gtk_internal(widget: &gtk::Widget) -> bool {
    let toast = glib::Type::from_name("AdwToastWidget");
    let parent_is = |ty: glib::Type| {
        widget
            .parent()
            .is_some_and(|parent| parent.type_().is_a(ty))
    };
    (widget.is::<gtk::ToggleButton>() && parent_is(gtk::MenuButton::static_type()))
        || (widget.is::<gtk::Text>()
            && widget.parent().is_some_and(|parent| {
                parent.is::<gtk::Entry>()
                    || parent.is::<gtk::SearchEntry>()
                    || parent.is::<gtk::PasswordEntry>()
                    || parent.is::<gtk::SpinButton>()
                    || parent.type_().name() == "AdwEntryRow"
                    || parent
                        .parent()
                        .is_some_and(|grand| grand.type_().name() == "AdwEntryRow")
            }))
        || widget
            .ancestor(gtk::WindowControls::static_type())
            .is_some()
        || widget.type_().name() == "AdwGizmo"
        || widget.ancestor(gtk::DropDown::static_type()).is_some()
        || widget.has_css_class("body-scrolled-window")
        || toast.is_some_and(|toast| widget.ancestor(toast).is_some())
}

/// libadwaita's own close button in a dialog's header bar: named "Close" by libadwaita, with
/// no tooltip, as in every libadwaita app.
fn adw_close_button(widget: &gtk::Widget) -> bool {
    widget.has_css_class("close")
        && glib::Type::from_name("AdwHeaderBar").is_some_and(|bar| widget.ancestor(bar).is_some())
}

/// Whether `widget` is a control the audit holds to its rules: a control's role, a web view,
/// or something else that takes the keyboard focus itself. A scrolled window or viewport takes
/// it only when nothing inside it can.
fn is_control(widget: &gtk::Widget) -> bool {
    let role = widget.accessible_role();
    if is_control_role(role) || widget.is::<webkit6::WebView>() {
        return true;
    }
    if !(widget.is_focusable() && widget.can_focus()) {
        return false;
    }
    if (widget.is::<gtk::ScrolledWindow>() || widget.is::<gtk::Viewport>())
        && holds_focusable(widget)
    {
        return false;
    }
    is_named_container(role) || is_silent_role(role)
}

/// Whether something on screen inside `widget` takes the keyboard focus.
fn holds_focusable(widget: &gtk::Widget) -> bool {
    let mut found = false;
    let mut child = widget.first_child();
    while let Some(current) = child {
        walk(&current, &mut |inner| {
            found |= inner.is_focusable() && inner.can_focus() && inner.is_sensitive();
        });
        child = current.next_sibling();
    }
    found
}

/// Everything wrong with the controls on screen under `root`.
fn audit(root: &gtk::Widget) -> Vec<String> {
    let mut problems = Vec::new();
    walk(root, &mut |widget| {
        if gtk_internal(widget) || !is_control(widget) {
            return;
        }
        let role = widget.accessible_role();
        let what = describe(widget);
        let name = name(widget);
        if name.is_none() {
            problems.push(format!("{what} has no accessible name"));
        }
        // GTK names a button by its visible label (a labelled-by relation), which wins over
        // the label property: a label set beside it is never heard.
        if let (Some(spoken), Some(label)) = (&name, label_property(widget))
            && spoken != &label
        {
            problems.push(format!(
                "{what} is named {spoken:?} by its visible label; its label property {label:?} \
                 is never heard"
            ));
        }
        // WebKit gives its web view GTK's generic role and exposes the page through its own
        // accessibility tree (not checked here; see docs/m9/accessibility.md).
        if is_silent_role(role) && !widget.is::<webkit6::WebView>() {
            problems.push(format!(
                "{what} takes the keyboard focus, but its role ({}) doesn't say what it is",
                role_name(role)
            ));
        }
        let needs = match role {
            gtk::AccessibleRole::ToggleButton => Some(gtk::AccessibleState::Pressed),
            gtk::AccessibleRole::Checkbox
            | gtk::AccessibleRole::Radio
            | gtk::AccessibleRole::Switch
            | gtk::AccessibleRole::MenuItemCheckbox
            | gtk::AccessibleRole::MenuItemRadio => Some(gtk::AccessibleState::Checked),
            _ => None,
        };
        if let Some(state) = needs
            && !gtk::test_accessible_has_state(widget, state)
        {
            problems.push(format!("{what} doesn't say whether it is {state:?}"));
        }
        if !widget.get_sensitive() && state_value(widget, gtk::AccessibleState::Disabled) != "true"
        {
            problems.push(format!("{what} is insensitive but not marked disabled"));
        }
        let icon_only = (widget.is::<gtk::Button>() || widget.is::<gtk::MenuButton>())
            && !adw_close_button(widget);
        if icon_only && !has_text(widget) && widget.tooltip_text().is_none() {
            problems.push(format!("{what} shows only an icon and has no tooltip"));
        }
    });
    problems
}

/// Calls `visit` for every widget on screen under `root`, popovers included, the hidden ones'
/// insides not.
fn walk(widget: &gtk::Widget, visit: &mut impl FnMut(&gtk::Widget)) {
    if !widget.is_mapped() || state_value(widget, gtk::AccessibleState::Hidden) == "true" {
        return;
    }
    visit(widget);
    let mut child = widget.first_child();
    while let Some(current) = child {
        walk(&current, visit);
        child = current.next_sibling();
    }
}

/// Whether a visible label with text sits inside `widget`.
fn has_text(widget: &gtk::Widget) -> bool {
    let mut text = false;
    super::collect(widget, &mut |inner| {
        if let Some(label) = inner.downcast_ref::<gtk::Label>() {
            text |= label.is_visible() && !label.label().trim().is_empty();
        }
    });
    text
}

/// A widget for the report: its type, its name, its classes.
fn describe(widget: &gtk::Widget) -> String {
    let mut text = widget.type_().name().to_owned();
    if widget.widget_name() != widget.type_().name() {
        text.push_str(&format!(" #{}", widget.widget_name()));
    }
    let classes = widget.css_classes();
    if !classes.is_empty() {
        text.push_str(&format!(" [{}]", classes.join(" ")));
    }
    match stop_name(widget) {
        Some(named) if !text.contains(&format!("#{named}")) => {
            text.push_str(&format!(" in #{named}"));
        }
        Some(_) => {}
        None => {
            let mut parents = Vec::new();
            let mut parent = widget.parent();
            while let Some(current) = parent
                && parents.len() < 3
            {
                parents.push(current.type_().name().to_owned());
                parent = current.parent();
            }
            text.push_str(&format!(" in {}", parents.join(" < ")));
        }
    }
    text
}

fn print_tree(root: &gtk::Widget) {
    walk(root, &mut |widget| {
        if gtk_internal(widget) || !is_control(widget) {
            return;
        }
        let mut states = Vec::new();
        for state in [
            gtk::AccessibleState::Pressed,
            gtk::AccessibleState::Checked,
            gtk::AccessibleState::Expanded,
            gtk::AccessibleState::Selected,
            gtk::AccessibleState::Disabled,
            gtk::AccessibleState::Invalid,
        ] {
            if gtk::test_accessible_has_state(widget, state) {
                states.push(format!(
                    "{}={}",
                    format!("{state:?}").to_lowercase(),
                    state_value(widget, state)
                ));
            }
        }
        println!(
            "  # {} — {} {:?}{}{}",
            describe(widget),
            role_name(widget.accessible_role()),
            name(widget).unwrap_or_default(),
            if states.is_empty() {
                String::new()
            } else {
                format!(" ({})", states.join(", "))
            },
            if widget.is_focusable() && widget.can_focus() {
                ""
            } else {
                " [not focusable]"
            }
        );
    });
}

// --- The Tab order -------------------------------------------------------------------------------

/// The nearest widget around `widget` with a name of its own (not its type's), and that name.
fn stop_name(widget: &gtk::Widget) -> Option<String> {
    let mut current = Some(widget.clone());
    while let Some(here) = current {
        if here.is::<gtk::Window>() {
            return None;
        }
        let name = here.widget_name();
        if name != here.type_().name() {
            return Some(name.to_string());
        }
        current = here.parent();
    }
    None
}

/// A Tab stop for the report: the nearest named widget, else the type and its name.
fn stop(widget: &gtk::Widget) -> String {
    stop_name(widget).unwrap_or_else(|| {
        format!(
            "{}:{}",
            widget.type_().name(),
            name(widget).unwrap_or_default()
        )
    })
}

/// The chain with repeats of one stop shown once, with their count.
fn collapsed(chain: &[String]) -> Vec<String> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for stop in chain {
        match out.last_mut() {
            Some((last, count)) if last == stop => *count += 1,
            _ => out.push((stop.clone(), 1)),
        }
    }
    out.into_iter()
        .map(|(stop, count)| {
            if count == 1 {
                stop
            } else {
                format!("{stop} ×{count}")
            }
        })
        .collect()
}

async fn tab_chain(harness: &Harness, which: &str, backward: bool, max: usize) -> StepResult {
    let window: gtk::Window = match which {
        "window" => harness.window()?.window().clone().upcast(),
        "composer" => composer_window()?,
        other => return Err(format!("use window or composer, not {other}")),
    };
    let focus_widget = || gtk::prelude::GtkWindowExt::focus(&window);
    let start = focus_widget().ok_or("nothing has the focus to start from")?;
    let mut chain = vec![stop(&start)];
    let mut left = false;
    let mut result = Err(String::new());
    for _ in 0..max {
        let watch = releases(&window);
        press(if backward { "<Shift>Tab" } else { "Tab" })?;
        delivered(watch).await;
        harness.wait_until_settled().await;
        let Some(focus) = focus_widget() else {
            result = Err("a Tab press left the focus nowhere".to_owned());
            break;
        };
        chain.push(stop(&focus));
        // Inside a web view the focus moves through the page while GTK's stays on the view:
        // from there the chain is round once it left the view and came back. Anywhere else a
        // press that keeps the focus where it started means that is the only stop.
        left |= focus != start;
        if focus == start && (left || !start.is::<webkit6::WebView>()) {
            result = Ok(());
            break;
        }
    }
    println!("  # {}", collapsed(&chain).join(" → "));
    CHAIN.with(|stored| stored.replace(chain));
    result.map_err(|error| {
        if error.is_empty() {
            format!(
                "the focus never came back to {} in {max} presses",
                stop(&start)
            )
        } else {
            error
        }
    })
}

/// Presses `spec`, a GTK accelerator, with the keysym already chosen.
fn press(spec: &str) -> StepResult {
    let (keyval, mods) =
        gtk::accelerator_parse(spec).ok_or_else(|| format!("GTK can't parse the key {spec}"))?;
    let mut state = 0;
    let mut keyval = keyval;
    if mods.contains(gdk::ModifierType::SHIFT_MASK) {
        state |= broadway::SHIFT;
        // GDK delivers Shift+Tab as ISO_Left_Tab.
        keyval = if keyval == gdk::Key::Tab {
            gdk::Key::ISO_Left_Tab
        } else {
            keyval.to_upper()
        };
    }
    if mods.contains(gdk::ModifierType::CONTROL_MASK) {
        state |= broadway::CONTROL;
    }
    broadway::key(keyval.into_glib(), state).map_err(|error| error.to_string())
}

/// A window whose key releases are counted, and the counter.
type Counted = (glib::WeakRef<gtk::Window>, Rc<Cell<u64>>);

thread_local! {
    /// Key releases counted per window, by an event controller of its own.
    static RELEASES: RefCell<Vec<Counted>> = const { RefCell::new(Vec::new()) };
}

/// `window`'s release counter and its count now.
fn releases(window: &gtk::Window) -> (Rc<Cell<u64>>, u64) {
    let counter = RELEASES.with(|all| {
        let mut all = all.borrow_mut();
        all.retain(|(watched, _)| watched.upgrade().is_some());
        if let Some((_, counter)) = all
            .iter()
            .find(|(watched, _)| watched.upgrade().as_ref() == Some(window))
        {
            return counter.clone();
        }
        let counter: Rc<Cell<u64>> = Rc::default();
        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        let count = counter.clone();
        legacy.connect_event(move |_, event| {
            if event.event_type() == gdk::EventType::KeyRelease {
                count.set(count.get() + 1);
            }
            glib::Propagation::Proceed
        });
        window.add_controller(legacy);
        all.push((window.downgrade(), counter.clone()));
        counter
    });
    let before = counter.get();
    (counter, before)
}

async fn delivered((counter, before): (Rc<Cell<u64>>, u64)) {
    let started = Instant::now();
    while counter.get() <= before && started.elapsed() < Duration::from_millis(500) {
        glib::timeout_future(Duration::from_millis(2)).await;
    }
}
