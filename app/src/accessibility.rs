//! What screen readers and the keyboard need beyond each widget's own label (M9, ML-092):
//! spoken announcements for lists that move while the focus stays in their search field, and
//! the keyboard focus kept out of WebKit's hidden key-binding widget.

use gtk4 as gtk;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};

thread_local! {
    /// Whether announcements are kept for the self-tests (`assert-announced`).
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    /// The announcements kept since the last [`take_announcements`].
    static SPOKEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Keeps every announcement from now on, for a self-test to read back.
pub fn record_announcements() {
    RECORDING.with(|recording| recording.set(true));
}

/// Has a screen reader say `text` about `widget`, as when a list's chosen row moves while the
/// keyboard focus stays in the field above it.
pub fn announce(widget: &impl IsA<gtk::Accessible>, text: &str) {
    if text.is_empty() {
        return;
    }
    widget.announce(text, gtk::AccessibleAnnouncementPriority::Medium);
    if RECORDING.with(Cell::get) {
        SPOKEN.with(|spoken| spoken.borrow_mut().push(text.to_owned()));
    }
}

/// What a list row shows, as a screen reader would read it: its visible texts, in order.
pub fn shown_text(widget: &impl IsA<gtk::Widget>) -> String {
    fn collect(widget: &gtk::Widget, parts: &mut Vec<String>) {
        if !widget.is_visible() {
            return;
        }
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            let text = label.text();
            if !text.trim().is_empty() {
                parts.push(text.to_string());
            }
            return;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            collect(&current, parts);
            child = current.next_sibling();
        }
    }
    let mut parts = Vec::new();
    collect(widget.as_ref(), &mut parts);
    parts.join(", ")
}

/// The announcements made since the last call, oldest first.
pub fn take_announcements() -> Vec<String> {
    SPOKEN.with(|spoken| spoken.take())
}

/// WebKitGTK parents a zero-sized GtkTextView to each web view only to translate key bindings
/// (its `KeyBindingTranslator`); it is focusable, so Shift+Tab from the widget after a web view
/// lands in it, where Tab and Shift+Tab insert tabs that go nowhere: a trap, and an unnamed
/// text field for a screen reader. This takes it out of the focus chain and hides it from
/// screen readers, now and whenever WebKit adds one.
pub fn keep_focus_off_key_bindings(view: &impl IsA<gtk::Widget>) {
    let view = view.as_ref();
    untrap(view);
    view.connect_realize(untrap);
    view.connect_map(untrap);
}

fn untrap(view: &gtk::Widget) {
    let mut child = view.first_child();
    while let Some(current) = child {
        if current.is::<gtk::TextView>() {
            current.set_focusable(false);
            current.update_state(&[gtk::accessible::State::Hidden(true)]);
        }
        child = current.next_sibling();
    }
}

/// Tab and Shift+Tab leave `widget` for the next or the previous control in its window, as in
/// a text field, instead of typing a tab: the composer's body, an editable web view, takes Tab
/// as a character and Ctrl+Tab twice (WebKit's key bindings and the window both move).
pub fn tab_leaves(widget: &impl IsA<gtk::Widget>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|keys, key, _, state| {
        use gtk::gdk::{Key, ModifierType};
        if state.intersects(ModifierType::CONTROL_MASK | ModifierType::ALT_MASK) {
            return gtk::glib::Propagation::Proceed;
        }
        let backward = match key {
            Key::ISO_Left_Tab => true,
            Key::Tab | Key::KP_Tab => state.contains(ModifierType::SHIFT_MASK),
            _ => return gtk::glib::Propagation::Proceed,
        };
        match keys.widget() {
            Some(widget) if move_focus_past(&widget, backward) => gtk::glib::Propagation::Stop,
            _ => gtk::glib::Propagation::Proceed,
        }
    });
    widget.add_controller(keys);
}

/// Moves the keyboard focus to the first control after `widget` (before it, `backward`) in
/// its window's Tab order, wrapping round; false when there is none.
pub fn move_focus_past(widget: &gtk::Widget, backward: bool) -> bool {
    let Some(root) = widget.root() else {
        return false;
    };
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut passed = false;
    fn visit(
        current: &gtk::Widget,
        widget: &gtk::Widget,
        passed: &mut bool,
        before: &mut Vec<gtk::Widget>,
        after: &mut Vec<gtk::Widget>,
    ) {
        if !current.is_mapped() || !current.is_sensitive() || !current.can_focus() {
            return;
        }
        if current == widget {
            *passed = true;
            return;
        }
        if current.is_focusable() && !current.is::<gtk::Window>() {
            if *passed {
                after.push(current.clone());
            } else {
                before.push(current.clone());
            }
        }
        let mut child = current.first_child();
        while let Some(next) = child {
            if !next.is::<gtk::Native>() {
                visit(&next, widget, passed, before, after);
            }
            child = next.next_sibling();
        }
    }
    visit(
        root.upcast_ref(),
        widget,
        &mut passed,
        &mut before,
        &mut after,
    );
    let order: Vec<gtk::Widget> = if backward {
        before
            .into_iter()
            .rev()
            .chain(after.into_iter().rev())
            .collect()
    } else {
        after.into_iter().chain(before).collect()
    };
    order.iter().any(|candidate| candidate.grab_focus())
}
