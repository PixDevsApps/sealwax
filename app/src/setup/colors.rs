//! The first step's "Colour": a round swatch for each account colour and one for none, as a
//! radio group. The chosen swatch is the group's one Tab stop; Left and Right (Up and Down,
//! Home and End) move the choice and the focus along, a click or Space chooses, and Enter
//! advances as in the step's text fields. Swatches are named `setup-color-<name>`
//! (`setup-color-none`) for the self-test.

use crate::window::panes::{classed, named};
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::account::AccountColor;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What a screen reader and the tooltip call the swatch without a colour.
const NONE_TITLE: &str = "No colour";

pub(super) struct ColorChoice {
    pub(super) root: gtk::Box,
    swatches: Vec<(Option<AccountColor>, gtk::Button)>,
    /// The chosen colour's name beside the swatches.
    shown: gtk::Label,
    chosen: Cell<Option<AccountColor>>,
    /// What Enter does: the assistant's next step.
    on_enter: RefCell<Option<Rc<dyn Fn()>>>,
}

fn title(color: Option<AccountColor>) -> &'static str {
    color.map_or(NONE_TITLE, AccountColor::title)
}

fn swatch(color: Option<AccountColor>) -> gtk::Button {
    let dot = classed(
        gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
        &["sw-setup-swatch-dot"],
    );
    let name = color.map_or("none", AccountColor::name);
    let button = named(
        classed(
            gtk::Button::builder()
                .child(&dot)
                .accessible_role(gtk::AccessibleRole::Radio)
                .tooltip_text(title(color))
                .valign(gtk::Align::Center)
                .build(),
            &["sw-setup-swatch"],
        ),
        &format!("setup-color-{name}"),
    );
    match color {
        Some(color) => button.add_css_class(&format!("sw-account-{}", color.name())),
        None => button.add_css_class("sw-setup-swatch-none"),
    }
    button.update_property(&[gtk::accessible::Property::Label(title(color))]);
    button
}

impl ColorChoice {
    pub(super) fn new() -> Rc<Self> {
        let root = named(
            classed(
                gtk::Box::builder()
                    .accessible_role(gtk::AccessibleRole::RadioGroup)
                    .build(),
                &["sw-setup-swatches"],
            ),
            "setup-color",
        );
        let swatches: Vec<_> = AccountColor::ALL
            .into_iter()
            .map(Some)
            .chain([None])
            .map(|color| (color, swatch(color)))
            .collect();
        for (_, button) in &swatches {
            root.append(button);
        }
        let shown = classed(
            gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
            &["sw-setup-swatch-name"],
        );
        root.append(&shown);
        let choice = Rc::new(Self {
            root,
            swatches,
            shown,
            chosen: Cell::new(None),
            on_enter: RefCell::new(None),
        });
        for (color, button) in &choice.swatches {
            let weak = Rc::downgrade(&choice);
            let color = *color;
            button.connect_clicked(move |_| {
                if let Some(choice) = weak.upgrade() {
                    choice.choose(color, true);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&choice);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(choice) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if modifiers
                .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK)
            {
                return glib::Propagation::Proceed;
            }
            if matches!(
                key,
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::ISO_Enter
            ) {
                choice.enter();
                return glib::Propagation::Stop;
            }
            let count = choice.swatches.len();
            let at = choice.index();
            let next = match key {
                gtk::gdk::Key::Right | gtk::gdk::Key::Down => (at + 1) % count,
                gtk::gdk::Key::Left | gtk::gdk::Key::Up => (at + count - 1) % count,
                gtk::gdk::Key::Home => 0,
                gtk::gdk::Key::End => count - 1,
                _ => return glib::Propagation::Proceed,
            };
            choice.choose(choice.swatches[next].0, true);
            glib::Propagation::Stop
        });
        choice.root.add_controller(keys);
        choice.choose(None, false);
        choice
    }

    fn index(&self) -> usize {
        let chosen = self.chosen.get();
        self.swatches
            .iter()
            .position(|(color, _)| *color == chosen)
            .unwrap_or_default()
    }

    /// Runs `action` when Enter is pressed on a swatch.
    pub(super) fn connect_enter(&self, action: impl Fn() + 'static) {
        self.on_enter.replace(Some(Rc::new(action)));
    }

    /// Enter on a swatch.
    pub(super) fn enter(&self) {
        let action = self.on_enter.borrow().clone();
        if let Some(action) = action {
            action();
        }
    }

    /// Whether a swatch has the keyboard focus.
    pub(super) fn has_focus(&self) -> bool {
        self.root.focus_child().is_some()
    }

    pub(super) fn chosen(&self) -> Option<AccountColor> {
        self.chosen.get()
    }

    /// Chooses `color`; `focus` moves the focus to its swatch, as a key or a click does.
    pub(super) fn choose(&self, color: Option<AccountColor>, focus: bool) {
        self.chosen.set(color);
        for (swatch_color, button) in &self.swatches {
            let on = *swatch_color == color;
            button.set_focusable(on);
            if on {
                button.add_css_class("sw-chosen");
            } else {
                button.remove_css_class("sw-chosen");
            }
            button.update_state(&[gtk::accessible::State::Checked(if on {
                gtk::AccessibleTristate::True
            } else {
                gtk::AccessibleTristate::False
            })]);
            if on && focus {
                button.grab_focus();
            }
        }
        self.shown.set_label(title(color));
    }
}
