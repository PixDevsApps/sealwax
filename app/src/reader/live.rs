//! Live views (ADR-011's memory decision): at most [`super::MAX_LIVE_VIEWS`] expanded HTML
//! bodies hold a WebView. Those nearest the visible part of the column keep theirs; bodies far off
//! screen give their view back and keep a placeholder of their measured height, so nothing
//! jumps, and get one again before they scroll into sight. Plain-text bodies never hold a view.
//! The choice is pure ([`wanted`], [`next_to_measure`]); the reader applies it whenever the
//! column scrolls, resizes, or a body's height changes ([`Inner::schedule`]).
//!
//! The column's scroll is the reader's too: a new conversation opens scrolled to the selected
//! message, and a change above what the column shows (a body measured, expanded or collapsed, a
//! message arriving) keeps what it shows in place. Both are applied in the frame clock's layout
//! phase, after GTK's own layout, so no frame shows the content moved.

use super::conversation::{Entry, HtmlBody, Page};
use super::show::entry_widget;
use super::view::MessageView;
use super::{Inner, MAX_LIVE_VIEWS, rgba, stylesheet};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib, graphene};
use sealwax_domain::model::MessageId;
use sealwax_domain::theme::BodyStyle;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long after a change above the visible part the column keeps what it shows in place
/// (text views and bodies settle over a few frames).
const HOLD: Duration = Duration::from_millis(600);
/// Room left above the selected message when the column scrolls to it.
const SCROLL_MARGIN: f64 = 12.0;

/// How far beyond the visible part of the column a body keeps or gets its view, in column
/// heights: one screen above and one below, so a body is rendered again before it scrolls into
/// sight at reading speed.
pub const MARGIN_SCREENS: f64 = 1.0;

/// One expanded HTML body of a page, in page coordinates (px from the page's top).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub id: MessageId,
    pub top: f64,
    pub bottom: f64,
    /// It holds a view now.
    pub live: bool,
    /// Its height is known at the column's current width.
    pub measured: bool,
    /// It keeps its view whatever its place: the focus is in it.
    pub pinned: bool,
}

impl Body {
    /// How far the body is from `window` (`top`, `bottom`); 0 when they overlap.
    fn distance(&self, window: (f64, f64)) -> f64 {
        if self.bottom <= window.0 {
            window.0 - self.bottom
        } else if self.top >= window.1 {
            self.top - window.1
        } else {
            0.0
        }
    }
}

/// The bodies that should hold a view: pinned ones, then the nearest to `window` within
/// `margin` of it (visible ones first), at most `cap` in all. At the same distance a body that
/// holds a view keeps it, so scrolling doesn't trade views back and forth.
pub fn wanted(bodies: &[Body], window: (f64, f64), margin: f64, cap: usize) -> Vec<MessageId> {
    let mut near: Vec<&Body> = bodies
        .iter()
        .filter(|body| body.pinned || body.distance(window) <= margin)
        .collect();
    near.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(a.distance(window).total_cmp(&b.distance(window)))
            .then(b.live.cmp(&a.live))
            .then(a.top.total_cmp(&b.top))
    });
    near.into_iter().take(cap).map(|body| body.id).collect()
}

/// The body to measure next with a spare view: the unmeasured one nearest to `window`, so
/// heights above and below become exact (and placeholders right) before the reader scrolls
/// there.
pub fn next_to_measure(bodies: &[Body], window: (f64, f64)) -> Option<MessageId> {
    bodies
        .iter()
        .filter(|body| !body.measured && !body.live)
        .min_by(|a, b| {
            a.distance(window)
                .total_cmp(&b.distance(window))
                .then(a.top.total_cmp(&b.top))
        })
        .map(|body| body.id)
}

/// The reader's live-view and scroll state.
#[derive(Default)]
pub struct Scheduling {
    /// A scheduling pass waits for the main loop.
    queued: Cell<bool>,
    /// What the column keeps in place while things above it change.
    anchor: RefCell<Option<Anchor>>,
    /// Scroll to the selected message at the next layout (a conversation was just shown), or to
    /// `scroll_target` (Next or Previous Message in Conversation).
    scroll_to: Cell<bool>,
    scroll_target: Cell<Option<MessageId>>,
    /// The shown page came on screen before its first screen was laid out: its selected
    /// message keeps its place while the bodies get their heights.
    settling: Cell<bool>,
    clock: RefCell<Option<(gdk::FrameClock, glib::SignalHandlerId)>>,
    column: RefCell<Option<(gtk::Adjustment, Vec<glib::SignalHandlerId>)>>,
}

/// An entry of the shown page and its top (page coordinates) when the change began.
struct Anchor {
    widget: gtk::Widget,
    top: f64,
    until: Instant,
}

/// What the first screen of a page being laid out needs: the bodies that must be measured and
/// loaded before it shows (selected first), and those already done that it shows.
struct FirstScreen {
    needed: Vec<MessageId>,
    done: Vec<MessageId>,
}

/// `widget`'s top in `root`'s coordinates.
fn top_in(widget: &impl IsA<gtk::Widget>, root: &impl IsA<gtk::Widget>) -> Option<f64> {
    widget
        .as_ref()
        .compute_point(root.as_ref(), &graphene::Point::zero())
        .map(|point| f64::from(point.y()))
}

/// Whether the focus is in `html`'s view.
fn focused(html: &HtmlBody, focus: Option<&gtk::Widget>) -> bool {
    let (Some(view), Some(focus)) = (&html.view, focus) else {
        return false;
    };
    let web = view.web_view().upcast_ref::<gtk::Widget>();
    focus == web || focus.is_ancestor(web)
}

/// The expanded HTML bodies of `page`, with their place in it.
fn bodies(page: &Page, focus: Option<&gtk::Widget>) -> Vec<Body> {
    page.items()
        .filter_map(|item| {
            let html = item.card.as_ref()?.html.as_ref()?;
            let top = top_in(&html.frame, &page.root)?;
            Some(Body {
                id: item.id(),
                top,
                bottom: top + f64::from(html.frame.height()),
                live: html.view.is_some(),
                measured: html.measured_at(html.holder.width()),
                pinned: focused(html, focus),
            })
        })
        .collect()
}

/// The part of `page` the column shows once it is on screen, scrolled to the selected message:
/// from the top while the selected message starts in the first third of a screen, else from
/// just above it.
fn first_window(page: &Page, height: f64) -> Option<(f64, f64)> {
    let selected = top_in(&page.item(page.selected)?.slot, &page.root)?;
    let top = if selected <= height / 3.0 {
        0.0
    } else {
        selected - SCROLL_MARGIN
    };
    Some((top, top + height))
}

/// Every expanded HTML body up to the selected message, and those below it that start within
/// the first screen, is needed; done once measured at its width and loaded, or only measured
/// when its images must come from the server (they fill in as they arrive).
fn first_screen(page: &Page, height: f64) -> Option<FirstScreen> {
    let window = first_window(page, height)?;
    let selected = page
        .entries
        .iter()
        .position(|entry| matches!(entry, Entry::Message(item) if item.id() == page.selected))?;
    let mut needed: Vec<(f64, MessageId)> = Vec::new();
    let mut done = Vec::new();
    for (index, entry) in page.entries.iter().enumerate() {
        let Entry::Message(item) = entry else {
            continue;
        };
        let Some(html) = item.card.as_ref().and_then(|card| card.html.as_ref()) else {
            continue;
        };
        let Some(top) = top_in(&html.frame, &page.root) else {
            continue;
        };
        let bottom = top + f64::from(html.frame.height());
        if index > selected && top >= window.1 {
            continue;
        }
        if html.measured_at(html.holder.width()) && (html.loaded || html.parts.waits_for_server()) {
            if bottom > window.0 && top < window.1 {
                done.push(item.id());
            }
        } else {
            let distance = if item.id() == page.selected {
                -1.0
            } else if bottom <= window.0 {
                window.0 - bottom
            } else {
                (top - window.1).max(0.0)
            };
            needed.push((distance, item.id()));
        }
    }
    needed.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(FirstScreen {
        needed: needed.into_iter().map(|(_, id)| id).collect(),
        done,
    })
}

impl Inner {
    fn scroller(&self) -> Option<gtk::ScrolledWindow> {
        self.stage
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
    }

    /// The column's height, while the reader is on screen.
    fn column_height(&self) -> Option<f64> {
        let height = f64::from(self.scroller()?.height());
        (self.stage.is_mapped() && height > 1.0).then_some(height)
    }

    /// The part of the shown `page` the column shows, in page coordinates: from the column's
    /// scroll value and the page's place in the scrolled content, which allocations only catch
    /// up with at the next layout.
    fn window(&self, page: &Page) -> Option<(f64, f64)> {
        let scroller = self.scroller()?;
        let content = scroller.child().and_downcast::<gtk::Viewport>()?.child()?;
        let adjustment = scroller.vadjustment();
        let top = adjustment.value() - top_in(&page.root, &content)?;
        let height = match adjustment.page_size() {
            size if size > 1.0 => size,
            _ => f64::from(scroller.height()),
        };
        Some((top, top + height))
    }

    /// Follows the column's scroll and size, and the window's layout, while the reader is on
    /// screen.
    pub(super) fn watch_column(self: &Rc<Self>) {
        self.unwatch_column();
        let Some(scroller) = self.scroller() else {
            return;
        };
        let adjustment = scroller.vadjustment();
        let weak = Rc::downgrade(self);
        let moved = adjustment.connect_value_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.schedule();
            }
        });
        let weak = Rc::downgrade(self);
        let resized = adjustment.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.schedule();
            }
        });
        self.scheduling
            .column
            .replace(Some((adjustment, vec![moved, resized])));
        if let Some(clock) = self.stage.frame_clock() {
            let weak = Rc::downgrade(self);
            let handler = clock.connect_layout(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.on_layout();
                }
            });
            self.scheduling.clock.replace(Some((clock, handler)));
        }
    }

    pub(super) fn unwatch_column(&self) {
        if let Some((adjustment, handlers)) = self.scheduling.column.take() {
            for handler in handlers {
                adjustment.disconnect(handler);
            }
        }
        if let Some((clock, handler)) = self.scheduling.clock.take() {
            clock.disconnect(handler);
        }
    }

    /// Chooses which bodies hold views once the main loop is free.
    pub(super) fn schedule(self: &Rc<Self>) {
        if self.scheduling.queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade() {
                inner.scheduling.queued.set(false);
                inner.schedule_now();
            }
        });
    }

    /// Gives views to the bodies that need them and takes them from the others: first the
    /// bodies a conversation being laid out needs for its first screen (while the shown one
    /// keeps the views it shows), then the shown conversation's nearest bodies, then one more
    /// to measure a body not measured yet.
    pub(super) fn schedule_now(self: &Rc<Self>) {
        let Some(height) = self.column_height() else {
            return;
        };
        let margin = height * MARGIN_SCREENS;
        let focus = self.stage.root().and_then(|root| root.focus());
        let mut keep: Vec<(u64, MessageId)> = Vec::new();
        let shown = self.shown.borrow().as_ref().and_then(|page| {
            let window = self.window(page)?;
            Some((page.key, bodies(page, focus.as_ref()), window))
        });
        let pending = self.pending.borrow().as_ref().and_then(|page| {
            let staged = self
                .stage
                .staged()
                .is_some_and(|staged| staged == *page.root.upcast_ref::<gtk::Widget>());
            if !staged || page.root.width() <= 1 {
                return None;
            }
            Some((page.key, first_screen(page, height)?))
        });
        if let Some((key, screen)) = &pending {
            let showing = shown.as_ref().map_or(0, |(_, bodies, window)| {
                bodies
                    .iter()
                    .filter(|body| body.pinned || (body.live && body.distance(*window) == 0.0))
                    .count()
            });
            let budget = MAX_LIVE_VIEWS.saturating_sub(showing).max(1);
            keep.extend(
                screen
                    .needed
                    .iter()
                    .chain(&screen.done)
                    .take(budget)
                    .map(|id| (*key, *id)),
            );
        }
        if let Some((key, bodies, window)) = &shown {
            let budget = MAX_LIVE_VIEWS.saturating_sub(keep.len());
            let mut chosen = wanted(bodies, *window, margin, budget);
            for body in bodies {
                if chosen.len() < budget
                    && body.live
                    && !body.measured
                    && !chosen.contains(&body.id)
                {
                    chosen.push(body.id);
                }
            }
            if chosen.len() < budget
                && pending.is_none()
                && let Some(next) = next_to_measure(bodies, *window)
            {
                chosen.push(next);
            }
            keep.extend(chosen.into_iter().map(|id| (*key, id)));
        }
        tracing::trace!(
            window = ?shown.as_ref().map(|(_, _, window)| *window),
            bodies = shown.as_ref().map_or(0, |(_, bodies, _)| bodies.len()),
            keep = ?keep.iter().map(|(_, id)| id.0).collect::<Vec<_>>(),
            "reader: live views chosen"
        );
        self.apply_views(&keep, focus.as_ref());
    }

    /// Takes views from the bodies not in `keep` (not from one with the focus) and gives views
    /// to those in it, in its order, as long as the pool has one.
    fn apply_views(self: &Rc<Self>, keep: &[(u64, MessageId)], focus: Option<&gtk::Widget>) {
        let mut released = Vec::new();
        for pages in [&self.shown, &self.pending] {
            let mut pages = pages.borrow_mut();
            let Some(page) = pages.as_mut() else {
                continue;
            };
            let key = page.key;
            for item in page.items_mut() {
                let id = item.id();
                let Some(html) = item.card.as_mut().and_then(|card| card.html.as_mut()) else {
                    continue;
                };
                if html.view.is_none() || keep.contains(&(key, id)) || focused(html, focus) {
                    continue;
                }
                if let Some(view) = html.view.take() {
                    html.holder
                        .set_placeholder(html.measured.map_or(view.height(), |(height, _)| height));
                    released.push(view);
                }
            }
        }
        for view in released {
            self.engine.release(view);
        }
        let fonts = self.theme.theme().webview_fonts();
        for (key, id) in keep {
            let lacks = self
                .with_page(*key, |page| {
                    page.item(*id)
                        .and_then(|item| item.card.as_ref())
                        .and_then(|card| card.html.as_ref())
                        .is_some_and(|html| html.view.is_none())
                })
                .unwrap_or(false);
            if !lacks {
                continue;
            }
            let Some(view) = self.engine.acquire(&fonts) else {
                break;
            };
            self.attach(*key, *id, view);
        }
    }

    /// Loads message `id`'s body into `view`, at the height it was measured at, with the content
    /// filter unless its remote images are allowed.
    fn attach(self: &Rc<Self>, key: u64, id: MessageId, view: MessageView) {
        let theme = self.theme.theme();
        let filter = self.filter.borrow().clone();
        let token = self.next_token();
        let attached = self
            .with_page(key, |page| {
                let card = page.item_mut(id)?.card.as_mut()?;
                let allowed = self.remote.allows(id, &card.sender);
                let html = card.html.as_mut()?;
                let filter = match (allowed || html.remote_allowed, &filter) {
                    (true, _) => None,
                    (false, Some(filter)) => Some(filter),
                    (false, None) => return None,
                };
                html.token = token;
                html.loaded = false;
                html.painted = false;
                let height = html.measured.map_or(1, |(height, _)| height);
                view.web_view().set_size_request(-1, height);
                html.holder.set_child(Some(view.web_view()));
                let weak = Rc::downgrade(self);
                view.set_handler(move |event| {
                    if let Some(inner) = weak.upgrade() {
                        inner.body_event(key, id, token, event);
                    }
                });
                self.engine.set_parts(view.page_id(), html.parts.clone());
                view.set_collapse_quotes(html.collapse_quotes);
                view.set_fit_margins(html.style == BodyStyle::Paper);
                view.load(
                    &html.html,
                    &stylesheet(&theme, html.style),
                    &rgba(theme.webview_background(html.style)),
                    filter,
                );
                html.view = Some(view.clone());
                Some(())
            })
            .flatten();
        if attached.is_none() {
            self.engine.release(view);
        }
    }

    /// Whether the page `key` being laid out still waits for a body its first screen needs;
    /// `None` once it isn't being laid out.
    pub(super) fn first_screen_waiting(&self, key: u64) -> Option<bool> {
        let pending = self.pending.borrow();
        let page = pending.as_ref().filter(|page| page.key == key)?;
        if page.root.width() <= 1 || self.layout_held.get() {
            return Some(true);
        }
        let Some(height) = self.column_height() else {
            return Some(false);
        };
        Some(first_screen(page, height).is_some_and(|screen| !screen.needed.is_empty()))
    }

    /// The page just shown came on screen before its first screen was laid out (`early`): its
    /// selected message keeps its place until it is.
    pub(super) fn settle(&self, early: bool) {
        self.scheduling.settling.set(early);
    }

    /// Keeps what the column shows in place while what is above it changes: the topmost entry
    /// it shows keeps its place on screen for [`HOLD`]; the selected message instead while the
    /// page is [settling](Self::settle).
    pub(super) fn hold_scroll(&self) {
        let now = Instant::now();
        let mut anchor = self.scheduling.anchor.borrow_mut();
        if let Some(held) = anchor.as_mut().filter(|held| held.until > now) {
            held.until = now + HOLD;
            return;
        }
        let shown = self.shown.borrow();
        let Some(page) = shown.as_ref() else {
            return;
        };
        if self.scheduling.settling.get() {
            let laid_out = self
                .column_height()
                .and_then(|height| first_screen(page, height))
                .is_some_and(|screen| screen.needed.is_empty());
            if laid_out {
                self.scheduling.settling.set(false);
            } else if let Some(item) = page.item(page.selected) {
                *anchor = top_in(&item.slot, &page.root).map(|top| Anchor {
                    widget: item.slot.clone().upcast(),
                    top,
                    until: now + HOLD,
                });
                return;
            }
        }
        let Some((top, _)) = self.window(page) else {
            return;
        };
        *anchor = page.entries.iter().find_map(|entry| {
            let widget = entry_widget(entry);
            let start = top_in(&widget, &page.root)?;
            (start + f64::from(widget.height()) > top).then_some(Anchor {
                widget,
                top: start,
                until: now + HOLD,
            })
        });
    }

    /// Scrolls the column half its height down or up (Scroll Message Down and Up, issue #10),
    /// within the page.
    pub(super) fn scroll_half_page(&self, down: bool) {
        let Some(scroller) = self.scroller() else {
            return;
        };
        let adjustment = scroller.vadjustment();
        let step = adjustment.page_size() / 2.0;
        let target = adjustment.value() + if down { step } else { -step };
        let end = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        self.scheduling.anchor.replace(None);
        adjustment.set_value(target.clamp(adjustment.lower(), end));
    }

    /// The conversation just shown opens at its selected message.
    pub(super) fn scroll_to_selected(&self) {
        self.scheduling.anchor.replace(None);
        self.scheduling.scroll_target.set(None);
        self.scheduling.scroll_to.set(true);
        self.stage.queue_resize();
    }

    /// Scrolls the column to message `id`'s row or card at the next layout, its top a margin
    /// below the column's.
    pub(super) fn scroll_to_message(&self, id: MessageId) {
        self.scheduling.anchor.replace(None);
        self.scheduling.scroll_target.set(Some(id));
        self.scheduling.scroll_to.set(true);
        self.stage.queue_resize();
    }

    /// After GTK's layout in a frame: scrolls to the selected message, or moves the scroll by as
    /// much as the held entry moved; the frame clock lays out again before painting.
    fn on_layout(&self) {
        let Some(scroller) = self.scroller() else {
            return;
        };
        let adjustment = scroller.vadjustment();
        if self.scheduling.scroll_to.get() {
            if let Some(value) = self.selected_scroll(&scroller) {
                self.scheduling.scroll_to.set(false);
                adjustment.set_value(value);
            }
            return;
        }
        let mut anchor = self.scheduling.anchor.borrow_mut();
        let Some(held) = anchor.as_mut() else {
            return;
        };
        let shown = self.shown.borrow();
        let top = shown
            .as_ref()
            .filter(|page| held.until > Instant::now() && held.widget.is_ancestor(&page.root))
            .and_then(|page| top_in(&held.widget, &page.root));
        let Some(top) = top else {
            *anchor = None;
            return;
        };
        let delta = top - held.top;
        held.top = top;
        if delta.abs() >= 0.5 {
            adjustment.set_value(adjustment.value() + delta);
        }
    }

    /// Where message `id`'s row or card starts in the column, px from the top of what it shows.
    pub(super) fn message_top(&self, id: MessageId) -> Option<f64> {
        let shown = self.shown.borrow();
        top_in(&shown.as_ref()?.item(id)?.slot, &self.scroller()?)
    }

    /// The shown conversation's expanded HTML bodies, where the column shows them.
    pub(super) fn body_reports(&self) -> Vec<super::BodyReport> {
        let shown = self.shown.borrow();
        let (Some(page), Some(scroller)) = (shown.as_ref(), self.scroller()) else {
            return Vec::new();
        };
        page.items()
            .filter_map(|item| {
                let html = item.card.as_ref()?.html.as_ref()?;
                let top = top_in(&html.frame, &scroller)?;
                Some(super::BodyReport {
                    id: item.id(),
                    top,
                    bottom: top + f64::from(html.frame.height()),
                    live: html.view.is_some(),
                    loaded: html.loaded,
                    painted: html.painted,
                    measured: html.measured.map(|(height, _)| height),
                    height: html.holder.height(),
                })
            })
            .collect()
    }

    /// Where the column scrolls to show the selected message, once the page is laid out.
    fn selected_scroll(&self, scroller: &gtk::ScrolledWindow) -> Option<f64> {
        let shown = self.shown.borrow();
        let page = shown.as_ref()?;
        if page.root.width() <= 1 {
            return None;
        }
        let root = top_in(&page.root, scroller)? + scroller.vadjustment().value();
        if let Some(target) = self.scheduling.scroll_target.get() {
            let top = top_in(&page.item(target)?.slot, &page.root)?;
            self.scheduling.scroll_target.set(None);
            return Some(root + top - SCROLL_MARGIN);
        }
        let item = page.item(page.selected)?;
        let selected = top_in(&item.slot, &page.root)?;
        let height = f64::from(scroller.height());
        Some(if selected <= height / 3.0 {
            0.0
        } else {
            root + selected - SCROLL_MARGIN
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(id: i64, top: f64, height: f64) -> Body {
        Body {
            id: MessageId(id),
            top,
            bottom: top + height,
            live: false,
            measured: true,
            pinned: false,
        }
    }

    fn ids(list: &[MessageId]) -> Vec<i64> {
        list.iter().map(|id| id.0).collect()
    }

    /// Eight 1,500 px newsletters one under the other, read through a 900 px window.
    fn newsletters() -> Vec<Body> {
        (0..8)
            .map(|n| body(n, n as f64 * 1_520.0, 1_500.0))
            .collect()
    }

    #[test]
    fn the_visible_and_nearest_bodies_hold_views_up_to_the_cap() {
        let bodies = newsletters();
        assert_eq!(ids(&wanted(&bodies, (0.0, 900.0), 900.0, 4)), [0, 1]);
        assert_eq!(
            ids(&wanted(&bodies, (3_000.0, 3_900.0), 900.0, 4)),
            [1, 2, 3]
        );
        assert_eq!(ids(&wanted(&bodies, (3_000.0, 3_900.0), 900.0, 2)), [1, 2]);
        // Short bodies: more fit near the window than the cap allows.
        let short: Vec<Body> = (0..10).map(|n| body(n, n as f64 * 200.0, 180.0)).collect();
        let chosen = wanted(&short, (800.0, 1_700.0), 900.0, 4);
        assert_eq!(chosen.len(), 4);
        assert!(
            ids(&chosen).iter().all(|id| (4..=8).contains(id)),
            "{chosen:?}"
        );
    }

    #[test]
    fn a_focused_body_keeps_its_view_and_live_ones_win_ties() {
        let mut bodies = newsletters();
        bodies[7].pinned = true;
        bodies[7].live = true;
        assert_eq!(ids(&wanted(&bodies, (0.0, 900.0), 900.0, 2)), [7, 0]);
        let mut pair = vec![body(1, 0.0, 100.0), body(2, 900.0, 100.0)];
        pair[1].live = true;
        assert_eq!(ids(&wanted(&pair, (450.0, 550.0), 900.0, 1)), [2]);
        pair[1].live = false;
        assert_eq!(ids(&wanted(&pair, (450.0, 550.0), 900.0, 1)), [1]);
    }

    #[test]
    fn the_nearest_unmeasured_body_is_measured_next() {
        let mut bodies = newsletters();
        for body in &mut bodies {
            body.measured = false;
        }
        bodies[1].live = true;
        assert_eq!(
            next_to_measure(&bodies, (3_000.0, 3_900.0)),
            Some(MessageId(2))
        );
        bodies[2].measured = true;
        assert_eq!(
            next_to_measure(&bodies, (3_000.0, 3_900.0)),
            Some(MessageId(3))
        );
        for body in &mut bodies {
            body.measured = true;
        }
        assert_eq!(next_to_measure(&bodies, (0.0, 900.0)), None);
    }
}
