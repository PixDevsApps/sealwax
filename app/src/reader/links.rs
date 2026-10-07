//! Link safety (ML-062; SECURITY.md "Message rendering", rule 4): the real target of the link
//! under the pointer or the focus in a status strip at the reader's bottom-left, as browsers
//! show it; a warning beside a link whose text names another domain than its target (or whose
//! address hides its host behind an `@`); and, when such a link is clicked, a dialog naming
//! both, with Cancel as the default. Every link opens through one vetted opener: absolute
//! http(s) with a host, or mailto ([`super::openable`]). The strip and the warning show what
//! the message links to, so they are message content: never logged.

use super::view::{Hover, Link};
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::links::{self, Verdict};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// How a vetted link opens: in the default browser or mail handler, or a recorder in tests.
pub type Opener = Rc<dyn Fn(&gtk::Widget, &str)>;
/// Decides whether a vetted link may open (`Reader::set_link_check`); the warning dialog when
/// none is set.
pub type LinkCheck = Rc<dyn Fn(&Link) -> bool>;

/// What the status strip and the warning show, for checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkState {
    /// The strip's text while it is shown.
    pub status: Option<String>,
    /// The warning's lines while it is shown.
    pub warning: Option<Vec<String>>,
    /// Warning dialogs presented so far.
    pub dialogs: u32,
}

struct Warning {
    popover: gtk::Popover,
    title: gtk::Label,
    said: gtk::Label,
    opens: gtk::Label,
}

/// The reader's link handling, with the overlay that holds the reader's page and the strip.
pub struct Links {
    overlay: gtk::Overlay,
    strip: gtk::Box,
    status: gtk::Label,
    warning: Warning,
    opener: RefCell<Opener>,
    check: RefCell<Option<LinkCheck>>,
    dialogs: Cell<u32>,
    dialog: RefCell<Option<adw::AlertDialog>>,
    scrolling: RefCell<Option<(glib::WeakRef<gtk::Adjustment>, glib::SignalHandlerId)>>,
    anchor: RefCell<Option<(glib::WeakRef<gtk::Widget>, glib::SignalHandlerId)>>,
}

impl Links {
    /// Links around `page`, the reader's root, opening through `opener`.
    pub fn new(page: &impl IsA<gtk::Widget>, opener: Opener) -> Rc<Links> {
        let overlay = gtk::Overlay::builder().child(page).build();
        overlay.set_widget_name("reader-overlay");
        let status = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        status.set_widget_name("reader-link-status");
        let strip = gtk::Box::builder()
            .css_classes(["sw-link-status"])
            .accessible_role(gtk::AccessibleRole::Status)
            .can_target(false)
            .can_focus(false)
            .visible(false)
            .build();
        strip.set_widget_name("reader-link-strip");
        strip.append(&status);
        overlay.add_overlay(&strip);
        overlay.set_clip_overlay(&strip, false);
        let warning = warning(&overlay);
        let links = Rc::new(Links {
            overlay,
            strip,
            status,
            warning,
            opener: RefCell::new(opener),
            check: RefCell::new(None),
            dialogs: Cell::new(0),
            dialog: RefCell::new(None),
            scrolling: RefCell::new(None),
            anchor: RefCell::new(None),
        });
        let weak = Rc::downgrade(&links);
        links
            .overlay
            .connect_get_child_position(move |overlay, child| {
                let links = weak.upgrade()?;
                (child == links.strip.upcast_ref::<gtk::Widget>())
                    .then(|| links.strip_area(overlay))
                    .flatten()
            });
        links
    }

    /// The reader's root: its page with the strip over it.
    pub fn root(&self) -> &gtk::Overlay {
        &self.overlay
    }

    pub fn set_opener(&self, opener: Opener) {
        self.opener.replace(opener);
    }

    pub fn set_check(&self, check: Option<LinkCheck>) {
        self.check.replace(check);
    }

    pub fn state(&self) -> LinkState {
        LinkState {
            status: self
                .strip
                .is_visible()
                .then(|| self.status.text().to_string()),
            warning: self.warning.popover.is_visible().then(|| {
                [&self.warning.title, &self.warning.said, &self.warning.opens]
                    .iter()
                    .map(|label| label.text().to_string())
                    .collect()
            }),
            dialogs: self.dialogs.get(),
        }
    }

    /// The warning dialog on screen, if any (for checks: they answer it as a user would).
    pub fn dialog(&self) -> Option<adw::AlertDialog> {
        self.dialog.borrow().clone()
    }

    /// The pointer or the focus entered a link of `anchor` (`hover.x`, `hover.y` in its
    /// coordinates), or left the links (`None`).
    pub fn hover(&self, anchor: Option<&gtk::Widget>, hover: Option<&Hover>) {
        let Some(hover) = hover.filter(|hover| super::openable(&hover.link.uri).is_some()) else {
            self.strip.set_visible(false);
            self.warning.popover.popdown();
            return;
        };
        self.follow_scrolling();
        if let Some(anchor) = anchor {
            self.follow_anchor(anchor);
        }
        self.status.set_text(&links::status_text(&hover.link.uri));
        self.strip.set_visible(true);
        self.overlay.queue_allocate();
        let verdict = links::check(&hover.link.text, &hover.link.uri);
        let Some(anchor) = anchor.filter(|_| verdict != Verdict::Fine) else {
            self.warning.popover.popdown();
            return;
        };
        let target = links::target(&hover.link.uri);
        let path = target.map(|target| target.rest).unwrap_or_default();
        let (title, said, opens) = match &verdict {
            Verdict::Disguised { shown, target } => (
                "The link's address is disguised",
                format!("It shows <b>{}</b>", glib::markup_escape_text(shown)),
                target.clone(),
            ),
            Verdict::Mismatch { text, target } => (
                "The link text and its target differ",
                format!("Text says <b>{}</b>", glib::markup_escape_text(text)),
                target.clone(),
            ),
            Verdict::Fine => return,
        };
        // A screen reader user reaching the link with the keyboard hears the warning too.
        if hover.focused {
            let said = match &verdict {
                Verdict::Disguised { shown, .. } => format!("It shows {shown}"),
                Verdict::Mismatch { text, .. } => format!("Text says {text}"),
                Verdict::Fine => String::new(),
            };
            crate::accessibility::announce(
                &self.overlay,
                &format!("{title}. {said}. Opens {opens}{path}"),
            );
        }
        self.warning.title.set_text(title);
        self.warning.said.set_markup(&said);
        self.warning.opens.set_markup(&format!(
            "Opens <b>{}</b>{}",
            glib::markup_escape_text(&opens),
            glib::markup_escape_text(&path)
        ));
        let point = anchor.compute_point(
            &self.overlay,
            &gtk::graphene::Point::new(hover.x as f32, hover.y as f32),
        );
        if let Some(point) = point {
            self.warning
                .popover
                .set_pointing_to(Some(&gdk::Rectangle::new(
                    point.x().round() as i32,
                    point.y().round() as i32,
                    hover.width.max(1),
                    hover.height.max(1),
                )));
            self.warning.popover.popup();
        }
    }

    /// Opens `link` if it may open: absolute http(s) with a host, or mailto, and not refused
    /// by the check. Without a check of the caller's, a link whose text and target differ asks
    /// first.
    pub fn open(self: &Rc<Self>, link: &Link) {
        let Some(uri) = super::openable(&link.uri) else {
            tracing::debug!("reader: a link that isn't http(s) or mailto stays closed");
            return;
        };
        let check = self.check.borrow().clone();
        if let Some(check) = check {
            if check(link) {
                self.launch(&uri);
            }
            return;
        }
        match links::check(&link.text, &link.uri) {
            Verdict::Fine => self.launch(&uri),
            verdict => self.ask(&verdict, uri),
        }
    }

    fn launch(&self, uri: &str) {
        let opener = self.opener.borrow().clone();
        opener(self.overlay.upcast_ref(), uri);
    }

    /// The dialog for a link that doesn't go where its text says: Cancel (the default) or
    /// Open.
    fn ask(self: &Rc<Self>, verdict: &Verdict, uri: String) {
        let body = match verdict {
            Verdict::Mismatch { text, target } => format!(
                "The link's text says <b>{}</b>, but it opens <b>{}</b>.",
                glib::markup_escape_text(text),
                glib::markup_escape_text(target)
            ),
            Verdict::Disguised { shown, target } => format!(
                "The link's address shows <b>{}</b> first, but it opens <b>{}</b>.",
                glib::markup_escape_text(shown),
                glib::markup_escape_text(target)
            ),
            Verdict::Fine => return,
        };
        let dialog = adw::AlertDialog::builder()
            .heading("Open this link?")
            .body(body)
            .body_use_markup(true)
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("open", "Open")]);
        dialog.set_response_appearance("open", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(links) = weak.upgrade() else {
                return;
            };
            links.dialog.replace(None);
            if response == "open" {
                links.launch(&uri);
            }
        });
        self.dialogs.set(self.dialogs.get() + 1);
        self.dialog.replace(Some(dialog.clone()));
        self.warning.popover.popdown();
        dialog.present(Some(&self.overlay));
    }

    /// The strip's place: the visible bottom-left corner of the scrolled reader pane around
    /// the overlay (or the overlay's own, outside one), at most two thirds of its width.
    fn strip_area(&self, overlay: &gtk::Overlay) -> Option<gdk::Rectangle> {
        let pane = overlay
            .ancestor(gtk::ScrolledWindow::static_type())
            .unwrap_or_else(|| overlay.clone().upcast());
        let bottom_left = pane.compute_point(
            overlay,
            &gtk::graphene::Point::new(0.0, pane.height() as f32),
        )?;
        let (_, natural) = self.strip.preferred_size();
        let width = natural.width().min((pane.width() * 2 / 3).max(160));
        let height = natural.height().max(1);
        Some(gdk::Rectangle::new(
            bottom_left.x().round() as i32,
            bottom_left.y().round() as i32 - height,
            width,
            height,
        ))
    }

    /// Keeps the strip in the corner while the pane scrolls.
    fn follow_scrolling(&self) {
        let Some(scroller) = self
            .overlay
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
        else {
            return;
        };
        let adjustment = scroller.vadjustment();
        let followed = self
            .scrolling
            .borrow()
            .as_ref()
            .and_then(|(adjustment, _)| adjustment.upgrade())
            .is_some_and(|followed| followed == adjustment);
        if followed {
            return;
        }
        let overlay = self.overlay.downgrade();
        let handler = adjustment.connect_value_changed(move |_| {
            if let Some(overlay) = overlay.upgrade() {
                overlay.queue_allocate();
            }
        });
        if let Some((old, handler)) = self
            .scrolling
            .replace(Some((adjustment.downgrade(), handler)))
            && let Some(old) = old.upgrade()
        {
            old.disconnect(handler);
        }
    }
}

impl Links {
    /// The strip and the warning go when the body they describe leaves the screen (a card
    /// collapsed, a view released off screen, another conversation shown): its page no longer
    /// reports the pointer leaving.
    fn follow_anchor(&self, anchor: &gtk::Widget) {
        let followed = self
            .anchor
            .borrow()
            .as_ref()
            .and_then(|(widget, _)| widget.upgrade())
            .is_some_and(|widget| widget == *anchor);
        if followed {
            return;
        }
        let (strip, popover) = (self.strip.clone(), self.warning.popover.clone());
        let handler = anchor.connect_unmap(move |_| {
            strip.set_visible(false);
            popover.popdown();
        });
        if let Some((old, handler)) = self.anchor.replace(Some((anchor.downgrade(), handler)))
            && let Some(old) = old.upgrade()
        {
            old.disconnect(handler);
        }
    }
}

impl Drop for Links {
    fn drop(&mut self) {
        self.warning.popover.unparent();
        if let Some((adjustment, handler)) = self.scrolling.take()
            && let Some(adjustment) = adjustment.upgrade()
        {
            adjustment.disconnect(handler);
        }
        if let Some((anchor, handler)) = self.anchor.take()
            && let Some(anchor) = anchor.upgrade()
        {
            anchor.disconnect(handler);
        }
    }
}

/// The warning popover (the mock's): the danger icon, "The link text and its target differ",
/// what the text says and where the link opens.
fn warning(overlay: &gtk::Overlay) -> Warning {
    let popover = gtk::Popover::builder()
        .autohide(false)
        .can_focus(false)
        .can_target(false)
        .position(gtk::PositionType::Bottom)
        .css_classes(["sw-link-warning"])
        .build();
    popover.set_widget_name("reader-link-warning");
    let row = gtk::Box::builder().spacing(10).build();
    let image = icon(&["dialog-warning-symbolic"]);
    image.add_css_class("sw-link-warning-icon");
    image.set_valign(gtk::Align::Start);
    row.append(&image);
    let lines = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    let line = |classes: &[&str]| {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .max_width_chars(48)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(classes.to_vec())
            .build();
        lines.append(&label);
        label
    };
    let title = line(&["sw-strong"]);
    let said = line(&["sw-small"]);
    let opens = line(&["sw-small", "sw-secondary"]);
    row.append(&lines);
    popover.set_child(Some(&row));
    popover.set_parent(overlay);
    Warning {
        popover,
        title,
        said,
        opens,
    }
}
