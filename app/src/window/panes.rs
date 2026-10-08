//! What the panes share: the bar on top of each pane, icon buttons, and the reader's column.
//! Sizes, spacing and colours come from `style.css`; the classes are in docs/m1/polish.md.

use gtk4 as gtk;
use gtk4::gio;
use gtk4::prelude::*;
use libadwaita as adw;

/// The reader's column at its widest: it fills the reader pane up to this (the user's UI
/// tweak, 2026-10-08; 760 px before).
pub const READER_COLUMN: i32 = 1200;

/// An icon-only button. `label` is its tooltip and its accessible label; `icons` are tried in
/// order, so a name the icon theme lacks falls back to the next.
pub fn icon_button(icons: &[&str], label: &str) -> gtk::Button {
    let button = classed(
        gtk::Button::builder()
            .child(&icon(icons))
            .tooltip_text(label)
            .build(),
        &["sw-icon-button"],
    );
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// A themed icon with fallbacks: Yaru has `mail-inbox-symbolic`, Adwaita doesn't.
pub fn icon(icons: &[&str]) -> gtk::Image {
    gtk::Image::from_gicon(&gio::ThemedIcon::from_names(icons))
}

/// The main menu (ML-022), filled by `Window::set_menu_model`.
pub fn menu_button() -> gtk::MenuButton {
    let button = classed(
        gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main menu")
            .build(),
        &["sw-icon-button"],
    );
    button.update_property(&[gtk::accessible::Property::Label("Main menu")]);
    button
}

/// The bar on top of a pane: a back button for one-pane navigation, the pane's own widgets, and
/// outside Hyprland the window controls, shown where the pane meets the window's edge. Outside
/// Hyprland the bar also drags the window, as a title bar would; on Hyprland there is no title
/// bar at all (DESIGN.md "Layout").
pub struct PaneBar {
    pub root: gtk::Widget,
    /// The pane's own widgets, between the back button and the end controls.
    pub content: gtk::Box,
    /// Hidden until the window shows one pane at a time.
    pub back: Option<gtk::Button>,
    pub start_controls: Option<gtk::WindowControls>,
    pub end_controls: Option<gtk::WindowControls>,
}

impl PaneBar {
    /// `back` goes to the `nav.pane` page it names (`folders` or `list`); `classes` style the
    /// bar beside `sw-pane-bar`.
    pub fn new(tiling: bool, back: Option<(gtk::Button, &str)>, classes: &[&str]) -> Self {
        let row = classed(
            classed(
                gtk::Box::new(gtk::Orientation::Horizontal, 0),
                &["sw-pane-bar"],
            ),
            classes,
        );
        let controls = |side| {
            let controls = gtk::WindowControls::new(side);
            controls.set_visible(false);
            controls
        };
        let start_controls = (!tiling).then(|| controls(gtk::PackType::Start));
        let end_controls = (!tiling).then(|| controls(gtk::PackType::End));
        let back = back.map(|(button, page)| {
            button.set_action_name(Some("nav.pane"));
            button.set_action_target_value(Some(&page.to_variant()));
            button.set_visible(false);
            button
        });
        let content = gtk::Box::builder().hexpand(true).build();
        if let Some(controls) = &start_controls {
            row.append(controls);
        }
        if let Some(back) = &back {
            row.append(back);
        }
        row.append(&content);
        if let Some(controls) = &end_controls {
            row.append(controls);
        }
        let root = if tiling {
            row.upcast()
        } else {
            gtk::WindowHandle::builder().child(&row).build().upcast()
        };
        Self {
            root,
            content,
            back,
            start_controls,
            end_controls,
        }
    }

    /// Shows the window controls this bar holds on the given sides; a no-op on Hyprland.
    pub fn show_controls(&self, start: bool, end: bool) {
        if let Some(controls) = &self.start_controls {
            controls.set_visible(start);
        }
        if let Some(controls) = &self.end_controls {
            controls.set_visible(end);
        }
    }

    /// What the bar shows while one pane fills the window: its back button, and the window
    /// controls on both sides.
    pub fn narrow_setters(&self, breakpoint: &adw::Breakpoint) {
        let shown = true.to_value();
        for controls in self.start_controls.iter().chain(&self.end_controls) {
            breakpoint.add_setter(controls, "visible", Some(&shown));
        }
        if let Some(back) = &self.back {
            breakpoint.add_setter(back, "visible", Some(&shown));
        }
    }
}

/// The reader's column: one scrolled column, centred and at most [`READER_COLUMN`] px wide (a
/// plain maximum, as the mock's `max-width`: the clamp doesn't ease toward it), while the
/// reader's toolbar above it stretches. Its margins are `style.css`'s.
pub struct ReaderColumn {
    pub scroller: gtk::ScrolledWindow,
    /// Holds the reader's content (`Window::set_reader`).
    pub slot: adw::Bin,
}

impl ReaderColumn {
    pub fn new() -> Self {
        let slot = classed(adw::Bin::new(), &["sw-reader-column"]);
        let clamp = classed(
            adw::Clamp::builder()
                .maximum_size(READER_COLUMN)
                .tightening_threshold(READER_COLUMN)
                .unit(adw::LengthUnit::Px)
                .child(&slot)
                .build(),
            &["sw-reader-clamp"],
        );
        // No Tab stop of its own while a conversation is still being built: the focus would
        // land on an unnamed pane.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .focusable(false)
            .child(&clamp)
            .build();
        Self { scroller, slot }
    }
}

impl Default for ReaderColumn {
    fn default() -> Self {
        Self::new()
    }
}

/// A pane bar's title with a quieter subtitle after it on the same baseline, as the mock's
/// list header ("Inbox  12 unread · Personal").
pub fn title_block(title: &gtk::Label, subtitle: &gtk::Label) -> gtk::Box {
    title.add_css_class("sw-pane-title");
    subtitle.add_css_class("sw-pane-subtitle");
    subtitle.set_hexpand(true);
    for label in [title, subtitle] {
        label.set_xalign(0.0);
        label.set_valign(gtk::Align::BaselineCenter);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    }
    let block = classed(
        gtk::Box::builder()
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build(),
        &["sw-pane-heading"],
    );
    block.append(title);
    block.append(subtitle);
    block
}

/// Adds style classes and keeps the ones GTK gave the widget, which a builder's `css_classes`
/// would replace (a window's `background`, a toggle button's `toggle`).
pub fn classed<W: IsA<gtk::Widget>>(widget: W, classes: &[&str]) -> W {
    for class in classes {
        widget.add_css_class(class);
    }
    widget
}

/// Keeps `widget` named for the self-test (`Window::widget`).
pub fn named<W: IsA<gtk::Widget>>(widget: W, name: &str) -> W {
    widget.set_widget_name(name);
    widget
}
