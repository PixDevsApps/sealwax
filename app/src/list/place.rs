//! Keeping a list's place when its rows change under the selection (issue #29): the selected
//! row, or the one that took the place of a selected row that left, stays where the eye is.
//! GTK keeps its scroll anchor on a row that stays, but asked to scroll to a row it hasn't laid
//! out yet it puts that row at the top, and when the anchor's own row leaves it anchors the row
//! before, a row higher than the one that takes the place.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The row at `position` of `view`, in `scroller`, once its rows changed.
pub(super) struct Hold {
    pub view: gtk::ListView,
    pub scroller: gtk::ScrolledWindow,
    pub position: u32,
    /// How far down the view the selected row was before the change, when it was on screen:
    /// the row at `position` goes there.
    pub top: Option<f32>,
    /// Scrolls the row into view when it isn't on screen after the change (a row new to the
    /// selection); a selection the user scrolled away from stays out of view.
    pub reveal: bool,
}

/// Holds the row in place through the next layout, measuring it with `row_top` (how far down
/// the view it is, `None` off screen), then gives it the keyboard focus if the list has the
/// focus; `still` says whether the row is still the one to hold.
pub(super) fn hold(
    hold: Hold,
    row_top: impl Fn() -> Option<f32> + 'static,
    still: impl Fn() -> bool + 'static,
) {
    let Hold {
        view,
        scroller,
        position,
        top,
        reveal,
    } = hold;
    let Some(clock) = view.frame_clock().filter(|_| view.is_mapped()) else {
        if reveal {
            view.scroll_to(position, gtk::ListScrollFlags::NONE, None);
        }
        return;
    };
    let still = Rc::new(still);
    let shown = Rc::new(Cell::new(false));
    let laid_out: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    let once = laid_out.clone();
    let (layout_still, layout_shown) = (still.clone(), shown.clone());
    let id = clock.connect_layout(move |clock| {
        if let Some(id) = once.take() {
            clock.disconnect(id);
        }
        if !layout_still() {
            return;
        }
        let now = row_top();
        layout_shown.set(now.is_some());
        if let (Some(want), Some(now)) = (top, now)
            && (now - want).abs() > 0.5
        {
            let adjustment = scroller.vadjustment();
            adjustment.set_value(adjustment.value() + f64::from(now - want));
        }
    });
    laid_out.replace(Some(id));
    let painted: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    let once = painted.clone();
    let weak = view.downgrade();
    let id = clock.connect_after_paint(move |clock| {
        if let Some(id) = once.take() {
            clock.disconnect(id);
        }
        let Some(view) = weak.upgrade() else {
            return;
        };
        if !still() || !(shown.get() || reveal) {
            return;
        }
        let focused = view
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| {
                &focus == view.upcast_ref::<gtk::Widget>() || focus.is_ancestor(&view)
            });
        if focused {
            view.scroll_to(position, gtk::ListScrollFlags::FOCUS, None);
        } else if !shown.get() {
            view.scroll_to(position, gtk::ListScrollFlags::NONE, None);
        }
    });
    painted.replace(Some(id));
    view.queue_resize();
}
