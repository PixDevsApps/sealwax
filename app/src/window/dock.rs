//! The composer's dock (the user's UI tweak, 2026-10-08): drafts open as cards over the panes,
//! at the window's bottom right, as Gmail's composer does, instead of windows of their own. The
//! first card sits at the right edge and each newer one to its left; a card can be open,
//! minimized to its header, or expanded to the middle of the panes over a dimmed backdrop (the
//! other cards hide meanwhile; a click on the backdrop shrinks it back). Cards that no longer fit
//! minimize, the oldest first. While one pane fills the window an open card fills the panes
//! above the minimized ones. The dock only places cards: what a state shows is the card's own
//! business, told through the handler [`Dock::add`] takes.

use super::RAIL_MIN_WIDTH;
use super::panes::{classed, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// An open card, in px, while the window has room for it.
pub const CARD_SIZE: (i32, i32) = (640, 600);
/// A minimized card's width.
pub const MINIMIZED_WIDTH: i32 = 300;
/// An expanded card at most, and the room it keeps from the panes' edges.
pub const EXPANDED_SIZE: (i32, i32) = (1000, 820);
pub const EXPANDED_MARGIN: i32 = 32;
/// The room right of the first card, and between cards.
pub const EDGE: i32 = 16;
pub const GAP: i32 = 12;
/// The room an open card leaves above it, so the panes' bars still show.
pub const TOP_ROOM: i32 = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardState {
    Open,
    Minimized,
    Expanded,
}

impl CardState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Minimized => "minimized",
            Self::Expanded => "expanded",
        }
    }
}

type StateHandler = Rc<dyn Fn(CardState)>;

struct Card {
    widget: gtk::Widget,
    state: Cell<CardState>,
    on_state: StateHandler,
}

/// The cards over the panes of one window.
#[derive(Clone)]
pub struct Dock {
    inner: Rc<Inner>,
}

/// A dock that doesn't keep its window's widgets alive.
#[derive(Clone, Default)]
pub struct WeakDock {
    inner: Weak<Inner>,
}

impl WeakDock {
    pub fn upgrade(&self) -> Option<Dock> {
        self.inner.upgrade().map(|inner| Dock { inner })
    }
}

struct Inner {
    overlay: gtk::Overlay,
    /// Under the cards, over the panes, while a card is expanded.
    backdrop: gtk::Box,
    /// Oldest first: the rightmost card first.
    cards: RefCell<Vec<Rc<Card>>>,
    /// Minimizing the cards a smaller window crowds is due (`place` can't change states).
    tidy_due: Cell<bool>,
}

impl Dock {
    /// A dock over `panes`; [`Dock::widget`] takes their place in the window.
    pub fn new(panes: &impl IsA<gtk::Widget>) -> Self {
        let overlay = named(
            gtk::Overlay::builder()
                .child(panes)
                .vexpand(true)
                .hexpand(true)
                .build(),
            "composer-dock",
        );
        let backdrop = named(
            classed(
                gtk::Box::builder()
                    .visible(false)
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build(),
                &["sw-composer-backdrop"],
            ),
            "composer-backdrop",
        );
        overlay.add_overlay(&backdrop);
        let inner = Rc::new(Inner {
            overlay,
            backdrop,
            cards: RefCell::new(Vec::new()),
            tidy_due: Cell::new(false),
        });
        let weak = Rc::downgrade(&inner);
        inner
            .overlay
            .connect_get_child_position(move |overlay, widget| {
                let inner = weak.upgrade()?;
                inner.place(overlay, widget)
            });
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&inner);
        click.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(inner) = weak.upgrade()
                && let Some(card) = inner.expanded()
            {
                inner.set_state(&card.widget, CardState::Open);
            }
        });
        inner.backdrop.add_controller(click);
        Self { inner }
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.inner.overlay
    }

    pub fn downgrade(&self) -> WeakDock {
        WeakDock {
            inner: Rc::downgrade(&self.inner),
        }
    }

    /// Docks `widget` open, left of the cards there are; `on_state` hears every state it takes,
    /// this first one included.
    pub fn add(&self, widget: &impl IsA<gtk::Widget>, on_state: impl Fn(CardState) + 'static) {
        let inner = &self.inner;
        let widget = widget.as_ref().clone();
        // The dock gives each card its rectangle (`place`); Fill keeps GTK from aligning it
        // inside.
        widget.set_halign(gtk::Align::Fill);
        widget.set_valign(gtk::Align::Fill);
        let on_state: StateHandler = Rc::new(on_state);
        if let Some(expanded) = inner.expanded() {
            inner.set_state(&expanded.widget, CardState::Open);
        }
        inner.cards.borrow_mut().push(Rc::new(Card {
            widget: widget.clone(),
            state: Cell::new(CardState::Open),
            on_state: on_state.clone(),
        }));
        inner.overlay.add_overlay(&widget);
        on_state(CardState::Open);
        inner.make_room(&widget);
        inner.relayout();
    }

    /// Takes `widget` out of the dock.
    pub fn remove(&self, widget: &impl IsA<gtk::Widget>) {
        let inner = &self.inner;
        let widget = widget.as_ref();
        let removed = {
            let mut cards = inner.cards.borrow_mut();
            let before = cards.len();
            cards.retain(|card| &card.widget != widget);
            cards.len() != before
        };
        if removed {
            inner.overlay.remove_overlay(widget);
            inner.relayout();
        }
    }

    pub fn state(&self, widget: &impl IsA<gtk::Widget>) -> Option<CardState> {
        self.inner
            .card(widget.as_ref())
            .map(|card| card.state.get())
    }

    /// Puts `widget` in `state`: expanding one card puts an expanded other back open, and
    /// opening one minimizes the oldest others that no longer fit.
    pub fn set_state(&self, widget: &impl IsA<gtk::Widget>, state: CardState) {
        self.inner.set_state(widget.as_ref(), state);
    }

    /// The docked widgets, oldest first.
    pub fn cards(&self) -> Vec<gtk::Widget> {
        self.inner
            .cards
            .borrow()
            .iter()
            .map(|card| card.widget.clone())
            .collect()
    }

    /// Whether the backdrop of an expanded card shows.
    pub fn backdrop_shown(&self) -> bool {
        self.inner.backdrop.is_visible()
    }
}

impl Inner {
    fn card(&self, widget: &gtk::Widget) -> Option<Rc<Card>> {
        self.cards
            .borrow()
            .iter()
            .find(|card| &card.widget == widget)
            .cloned()
    }

    fn expanded(&self) -> Option<Rc<Card>> {
        self.cards
            .borrow()
            .iter()
            .find(|card| card.state.get() == CardState::Expanded)
            .cloned()
    }

    fn set_state(&self, widget: &gtk::Widget, state: CardState) {
        let Some(card) = self.card(widget) else {
            return;
        };
        if state == CardState::Expanded
            && let Some(other) = self.expanded().filter(|other| &other.widget != widget)
        {
            self.set_state(&other.widget, CardState::Open);
        }
        if card.state.replace(state) == state {
            return;
        }
        (card.on_state)(state);
        if state != CardState::Minimized {
            self.make_room(widget);
        }
        self.relayout();
    }

    /// Minimizes the oldest open cards until `keep` fits beside the rest; while one pane fills
    /// the window, every open card but `keep`.
    fn make_room(&self, keep: &gtk::Widget) {
        let width = self.overlay.width();
        if width <= 0 {
            return;
        }
        loop {
            let cards = self.cards.borrow().clone();
            if !crowded(&cards, width) {
                return;
            }
            let Some(oldest) = cards
                .iter()
                .find(|card| card.state.get() != CardState::Minimized && &card.widget != keep)
            else {
                return;
            };
            oldest.state.set(CardState::Minimized);
            (oldest.on_state)(CardState::Minimized);
        }
    }

    /// While a card is expanded the backdrop shows and the other cards hide under it.
    fn relayout(&self) {
        let expanded = self.expanded().map(|card| card.widget.clone());
        self.backdrop.set_visible(expanded.is_some());
        for card in self.cards.borrow().iter() {
            card.widget
                .set_child_visible(expanded.as_ref().is_none_or(|shown| shown == &card.widget));
        }
        self.overlay.queue_allocate();
    }

    /// Where `widget` goes in an overlay `overlay`'s size.
    fn place(
        self: &Rc<Self>,
        overlay: &gtk::Overlay,
        widget: &gtk::Widget,
    ) -> Option<gdk::Rectangle> {
        let (width, height) = (overlay.width(), overlay.height());
        if widget == self.backdrop.upcast_ref::<gtk::Widget>() {
            return Some(gdk::Rectangle::new(0, 0, width, height));
        }
        let cards = self.cards.borrow();
        let index = cards.iter().position(|card| &card.widget == widget)?;
        if crowded(&cards, width) && !self.tidy_due.replace(true) {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.tidy();
                }
            });
        }
        if one_pane(width) {
            return Some(stacked(&cards, index, width, height));
        }
        let state = cards[index].state.get();
        let (min_width, _) = measure(widget, gtk::Orientation::Horizontal, -1);
        if state == CardState::Expanded {
            let w = EXPANDED_SIZE
                .0
                .min(width - 2 * EXPANDED_MARGIN)
                .max(min_width);
            let (min_height, _) = measure(widget, gtk::Orientation::Vertical, w);
            let h = EXPANDED_SIZE
                .1
                .min(height - 2 * EXPANDED_MARGIN)
                .max(min_height);
            return Some(gdk::Rectangle::new(
                ((width - w) / 2).max(0),
                ((height - h) / 2).max(0),
                w,
                h,
            ));
        }
        let mut right = width - EDGE;
        for card in &cards[..index] {
            right -= slot_width(card.state.get()) + GAP;
        }
        let w = slot_width(state).max(min_width);
        let (min_height, natural_height) = measure(widget, gtk::Orientation::Vertical, w);
        let h = match state {
            CardState::Minimized => natural_height.max(min_height),
            _ => CARD_SIZE.1.min(height - TOP_ROOM).max(min_height),
        };
        Some(gdk::Rectangle::new((right - w).max(0), height - h, w, h))
    }

    /// A smaller window crowds the cards: the newest one shown stays, the oldest others
    /// minimize.
    fn tidy(&self) {
        self.tidy_due.set(false);
        let newest = self
            .cards
            .borrow()
            .iter()
            .rev()
            .find(|card| card.state.get() != CardState::Minimized)
            .map(|card| card.widget.clone());
        if let Some(newest) = newest {
            self.make_room(&newest);
            self.relayout();
        }
    }
}

/// While one pane fills the window, cards take the panes' whole width: minimized ones stacked
/// at the bottom, the oldest lowest, and an open one filling the room above them.
fn stacked(cards: &[Rc<Card>], index: usize, width: i32, height: i32) -> gdk::Rectangle {
    let header = |card: &Card| {
        let (min, natural) = measure(&card.widget, gtk::Orientation::Vertical, width);
        natural.max(min)
    };
    let minimized = |cards: &[Rc<Card>]| -> i32 {
        cards
            .iter()
            .filter(|card| card.state.get() == CardState::Minimized)
            .map(|card| header(card))
            .sum()
    };
    let card = &cards[index];
    if card.state.get() == CardState::Minimized {
        let below = minimized(&cards[..index]);
        let h = header(card);
        return gdk::Rectangle::new(0, height - below - h, width, h);
    }
    let below = minimized(cards);
    let (min_height, _) = measure(&card.widget, gtk::Orientation::Vertical, width);
    let h = (height - below).max(min_height);
    gdk::Rectangle::new(0, height - below - h, width, h)
}

/// One pane fills the window (`RAIL_MIN_WIDTH`): an open card fills the panes.
fn one_pane(width: i32) -> bool {
    f64::from(width) < RAIL_MIN_WIDTH
}

/// Whether the cards that aren't minimized don't fit side by side in a dock `width` wide; while
/// one pane fills the window, whether more than one is.
fn crowded(cards: &[Rc<Card>], width: i32) -> bool {
    let shown = cards
        .iter()
        .filter(|card| card.state.get() != CardState::Minimized)
        .count();
    if shown < 2 {
        return false;
    }
    if one_pane(width) {
        return true;
    }
    let needed: i32 = EDGE
        + cards
            .iter()
            .map(|card| slot_width(card.state.get()) + GAP)
            .sum::<i32>();
    needed > width
}

/// The width a card in `state` takes beside the others.
fn slot_width(state: CardState) -> i32 {
    match state {
        CardState::Minimized => MINIMIZED_WIDTH,
        CardState::Open | CardState::Expanded => CARD_SIZE.0,
    }
}

fn measure(widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> (i32, i32) {
    let (min, natural, _, _) = widget.measure(orientation, for_size);
    (min, natural)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_card_fits_beside_the_rail() {
        assert!(one_pane(RAIL_MIN_WIDTH as i32 - 1));
        assert!(!one_pane(RAIL_MIN_WIDTH as i32));
        assert!(CARD_SIZE.0 + 2 * EDGE <= RAIL_MIN_WIDTH as i32);
        assert!(MINIMIZED_WIDTH < CARD_SIZE.0);
        assert!(EXPANDED_SIZE.0 > CARD_SIZE.0 && EXPANDED_SIZE.1 > CARD_SIZE.1);
    }
}
