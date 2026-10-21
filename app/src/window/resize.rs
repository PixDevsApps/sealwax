//! The panes' drag handles (the user's UI tweak, 2026-10-08): a thin strip on the left edge of
//! the pane right of a border, so it covers neither the folder list's nor the message list's
//! scrollbar. Dragging it moves that border; a double click puts the default width back. As in
//! GtkPaned, the gesture sits on the panes' common parent in the capture phase and starts only
//! on a handle: the pointer is followed in that parent, which doesn't move with the border, and
//! wherever the pointer goes while the button is held.

use super::panes::{classed, named};
use gtk4 as gtk;
use gtk4::graphene;
use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// A handle named `name`, its pointer the column-resize one.
pub fn handle(name: &str) -> gtk::Box {
    let handle = named(
        classed(
            gtk::Box::builder()
                .halign(gtk::Align::Start)
                .vexpand(true)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
            &["sw-pane-handle"],
        ),
        name,
    );
    handle.set_cursor_from_name(Some("col-resize"));
    handle
}

/// What a handle does: `start` gives the width the drag starts from, `drag` takes the width the
/// pointer asks for, `done` follows the release, and `reset` a double click.
pub struct Moves {
    pub start: Box<dyn Fn() -> f64>,
    pub drag: Box<dyn Fn(f64)>,
    pub done: Box<dyn Fn()>,
    pub reset: Box<dyn Fn()>,
}

/// A press on a handle: which one, the width it started from, and when (ms).
#[derive(Clone, Copy)]
struct Press {
    handle: usize,
    width: f64,
    time: u32,
}

/// Makes `handles`, all inside `parent`, drag. A press on the same handle within the desktop's
/// double-click time of the last one is a double click: the drag gesture claims every press on
/// a handle, so a click gesture beside it would never count two.
pub fn connect(parent: &impl IsA<gtk::Widget>, handles: Vec<(gtk::Box, Moves)>) {
    let parent = parent.as_ref();
    let handles = Rc::new(handles);
    // The drag under way, and the press before it.
    let dragging: Rc<Cell<Option<Press>>> = Rc::default();
    let last: Rc<Cell<Option<Press>>> = Rc::default();
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (on, follow, end) = (handles.clone(), handles.clone(), handles);
    let (began, moving, ended) = (dragging.clone(), dragging.clone(), dragging);
    drag.connect_drag_begin(move |gesture, x, y| {
        let Some(index) = handle_at(gesture, &on, x, y) else {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let (handle, moves) = &on[index];
        let press = Press {
            handle: index,
            width: (moves.start)(),
            time: gesture.current_event_time(),
        };
        let double = last.replace(Some(press)).is_some_and(|before| {
            before.handle == index
                && press.time.wrapping_sub(before.time) <= double_click_time(handle)
        });
        if double {
            last.set(None);
            (moves.reset)();
            return;
        }
        handle.add_css_class("sw-dragging");
        began.set(Some(press));
    });
    drag.connect_drag_update(move |_, dx, _| {
        if let Some(press) = moving.get() {
            (follow[press.handle].1.drag)(press.width + dx);
        }
    });
    drag.connect_drag_end(move |_, _, _| {
        if let Some(press) = ended.take() {
            let (handle, moves) = &end[press.handle];
            handle.remove_css_class("sw-dragging");
            (moves.done)();
        }
    });
    parent.add_controller(drag);
}

/// The desktop's double-click time, in ms.
fn double_click_time(widget: &impl IsA<gtk::Widget>) -> u32 {
    let time = widget.settings().gtk_double_click_time();
    u32::try_from(time).unwrap_or(400)
}

/// The handle shown under `(x, y)` in the gesture's widget, unless it is switched off.
fn handle_at(
    gesture: &impl IsA<gtk::EventController>,
    handles: &[(gtk::Box, Moves)],
    x: f64,
    y: f64,
) -> Option<usize> {
    let parent = gesture.widget()?;
    let point = graphene::Point::new(x as f32, y as f32);
    handles.iter().position(|(handle, _)| {
        handle.is_mapped()
            && handle.can_target()
            && parent
                .compute_point(handle, &point)
                .is_some_and(|inside| handle.contains(inside.x().into(), inside.y().into()))
    })
}
