//! A plain-text body (M1; M5: links, quotes): the reading font in a GTK text view, no
//! WebView. Links are found in the text (`sealwax_domain::links::find`: http(s), `www.`,
//! mailto and bare addresses, never `javascript:`, `file:`, `data:` or relative targets), show
//! their target in the reader's status strip on hover, and open through the reader's vetted
//! opener. Quoted lines are muted, and when the page asks, the trailing quoted history
//! collapses behind a "…" toggle (`sealwax_domain::quotes`).

use super::links::Links;
use super::view::{Hover, Link};
use crate::theme::{ListenerId, ThemeController};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use sealwax_domain::theme::{Rgb, Theme};
use sealwax_domain::{links, quotes};
use std::cell::Cell;
use std::rc::Rc;

/// Names the tag of each link: the prefix, then the link's target.
const LINK_TAG: &str = "sw-link ";
const QUOTED_TAG: &str = "sw-quoted";
const HIDDEN_TAG: &str = "sw-quote-hidden";

/// A plain-text body in the reading font, selectable, wrapped, with no WebView.
pub fn plain_text(text: &str) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .accepts_tab(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .css_classes(["sw-reading", "sw-reader-plain"])
        .build();
    view.set_widget_name("reader-plain");
    view.update_property(&[gtk::accessible::Property::Label("Message text")]);
    view.buffer().set_text(text.trim_end());
    // GTK 4.22 leaves a text view that holds a single line at no height until something
    // resizes it, so a one-paragraph message showed no text (M1 gate previews). A resize once
    // it is on screen, after its first validation, gives it its height.
    view.connect_map(|view| {
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            view,
            move || view.queue_resize()
        ));
    });
    view
}

/// A message's plain-text body: [`plain_text`] with its links live, its quoted lines muted
/// in `theme`'s colours, and, with `collapse_quotes`, its trailing quoted history behind "…".
pub fn message_text(
    text: &str,
    links: &Rc<Links>,
    theme: &Rc<ThemeController>,
    collapse_quotes: bool,
) -> gtk::TextView {
    let text = text.trim_end();
    let view = plain_text(text);
    let buffer = view.buffer();
    let chars = |byte: usize| text[..byte.min(text.len())].chars().count() as i32;
    if let Some(quoted) = buffer.create_tag(Some(QUOTED_TAG), &[]) {
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            if line.trim_start().starts_with('>') {
                buffer.apply_tag(
                    &quoted,
                    &buffer.iter_at_offset(chars(offset)),
                    &buffer.iter_at_offset(chars(offset + line.len())),
                );
            }
            offset += line.len();
        }
    }
    for found in links::find(text) {
        let name = format!("{LINK_TAG}{}", found.target);
        let tag = buffer.tag_table().lookup(&name).or_else(|| {
            buffer.create_tag(
                Some(&name),
                &[("underline", &gtk::pango::Underline::Single)],
            )
        });
        if let Some(tag) = tag {
            buffer.apply_tag(
                &tag,
                &buffer.iter_at_offset(chars(found.start)),
                &buffer.iter_at_offset(chars(found.end)),
            );
        }
    }
    if collapse_quotes && let Some(quote) = quotes::trailing_quote(text) {
        collapse(&view, chars(quote.start));
    }
    colour(&view, &theme.theme());
    let weak = view.downgrade();
    let listener = theme.connect_changed(move |theme| {
        if let Some(view) = weak.upgrade() {
            colour(&view, theme);
        }
    });
    let id: Rc<Cell<Option<ListenerId>>> = Rc::new(Cell::new(Some(listener)));
    let theme = Rc::downgrade(theme);
    view.connect_destroy(move |_| {
        if let (Some(theme), Some(id)) = (theme.upgrade(), id.take()) {
            theme.disconnect(id);
        }
    });
    follow_links(&view, links);
    view
}

/// The text a reader shows of the first plain-text body under `widget`, without what is
/// collapsed or the toggle, for checks.
pub fn shown_text(widget: &gtk::Widget) -> String {
    let mut stack = vec![widget.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
            let buffer = view.buffer();
            return buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
        }
        let mut child = widget.last_child();
        while let Some(widget) = child {
            child = widget.prev_sibling();
            stack.push(widget);
        }
    }
    String::new()
}

/// The targets of the links in the plain-text bodies under `widget`, in order, for checks.
pub fn links_in(widget: &gtk::Widget) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![widget.clone()];
    while let Some(widget) = stack.pop() {
        if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
            let buffer = view.buffer();
            let mut iter = buffer.start_iter();
            loop {
                for tag in iter.toggled_tags(true) {
                    if let Some(target) = tag
                        .name()
                        .and_then(|name| name.strip_prefix(LINK_TAG).map(str::to_owned))
                    {
                        found.push(target);
                    }
                }
                if !iter.forward_to_tag_toggle(None::<&gtk::TextTag>) {
                    break;
                }
            }
        }
        let mut child = widget.last_child();
        while let Some(widget) = child {
            child = widget.prev_sibling();
            stack.push(widget);
        }
    }
    found
}

/// Link and quote colours from the theme: the accent and the secondary text.
fn colour(view: &gtk::TextView, theme: &Theme) {
    let rgba = |color: Rgb| {
        gdk::RGBA::new(
            f32::from(color.r) / 255.0,
            f32::from(color.g) / 255.0,
            f32::from(color.b) / 255.0,
            1.0,
        )
    };
    let table = view.buffer().tag_table();
    table.foreach(|tag| {
        let name = tag.name().unwrap_or_default();
        if name.starts_with(LINK_TAG) {
            tag.set_foreground_rgba(Some(&rgba(theme.palette.accent)));
        } else if name == QUOTED_TAG {
            tag.set_foreground_rgba(Some(&rgba(theme.mail.secondary_text)));
        }
    });
}

/// Hides the text from char `start` to the end behind a "…" toggle placed where it starts.
fn collapse(view: &gtk::TextView, start: i32) {
    let buffer = view.buffer();
    let Some(hidden) = buffer.create_tag(Some(HIDDEN_TAG), &[("invisible", &true)]) else {
        return;
    };
    let mut at = buffer.iter_at_offset(start);
    let anchor = buffer.create_child_anchor(&mut at);
    buffer.apply_tag(
        &hidden,
        &buffer.iter_at_offset(start + 1),
        &buffer.end_iter(),
    );
    let toggle = gtk::Button::builder()
        .label("…")
        .tooltip_text("Show quoted text")
        .css_classes(["sw-quote-toggle"])
        .build();
    toggle.set_widget_name("reader-quote-toggle");
    toggle.update_property(&[gtk::accessible::Property::Label("Show quoted text")]);
    toggle.connect_clicked(move |toggle| {
        let expand = hidden.is_invisible();
        hidden.set_invisible(!expand);
        let label = if expand {
            "Hide quoted text"
        } else {
            "Show quoted text"
        };
        toggle.set_tooltip_text(Some(label));
        toggle.update_property(&[gtk::accessible::Property::Label(label)]);
    });
    view.add_child_at_anchor(&toggle, &anchor);
}

/// The link at a point of `view`'s widget coordinates: its target, text and box.
fn link_at(view: &gtk::TextView, x: f64, y: f64) -> Option<Hover> {
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    let iter = view.iter_at_location(bx, by)?;
    let tag = iter
        .tags()
        .into_iter()
        .find(|tag| tag.name().is_some_and(|name| name.starts_with(LINK_TAG)))?;
    let target = tag.name()?.strip_prefix(LINK_TAG)?.to_owned();
    let (mut start, mut end) = (iter, iter);
    if !start.starts_tag(Some(&tag)) {
        start.backward_to_tag_toggle(Some(&tag));
    }
    if !end.ends_tag(Some(&tag)) {
        end.forward_to_tag_toggle(Some(&tag));
    }
    let text = view.buffer().text(&start, &end, false).to_string();
    let first = view.iter_location(&start);
    let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, first.x(), first.y());
    let last = view.iter_location(&end);
    let width = if last.y() == first.y() {
        (last.x() - first.x()).max(1)
    } else {
        view.width() - x
    };
    Some(Hover {
        link: Link { uri: target, text },
        x,
        y,
        width,
        height: first.height(),
    })
}

/// The strip on hover, a pointer cursor over links, and a click (without a selection) opens.
fn follow_links(view: &gtk::TextView, links: &Rc<Links>) {
    let over = Rc::new(Cell::new(false));
    let motion = gtk::EventControllerMotion::new();
    let weak = Rc::downgrade(links);
    motion.connect_motion(glib::clone!(
        #[strong]
        over,
        move |motion, x, y| {
            let (Some(links), Some(view)) = (
                weak.upgrade(),
                motion.widget().and_downcast::<gtk::TextView>(),
            ) else {
                return;
            };
            let hover = link_at(&view, x, y);
            view.set_cursor_from_name(Some(if hover.is_some() { "pointer" } else { "text" }));
            if hover.is_some() || over.get() {
                links.hover(Some(view.upcast_ref()), hover.as_ref());
            }
            over.set(hover.is_some());
        }
    ));
    let weak = Rc::downgrade(links);
    motion.connect_leave(glib::clone!(
        #[strong]
        over,
        move |_| {
            if over.replace(false)
                && let Some(links) = weak.upgrade()
            {
                links.hover(None, None);
            }
        }
    ));
    view.add_controller(motion);
    let click = gtk::GestureClick::builder().button(0).build();
    let weak = Rc::downgrade(links);
    click.connect_released(move |click, _, x, y| {
        let button = click.current_button();
        let (Some(links), Some(view)) = (
            weak.upgrade(),
            click.widget().and_downcast::<gtk::TextView>(),
        ) else {
            return;
        };
        if !(button == gdk::BUTTON_PRIMARY || button == gdk::BUTTON_MIDDLE)
            || view.buffer().has_selection()
        {
            return;
        }
        if let Some(hover) = link_at(&view, x, y) {
            links.open(&hover.link);
        }
    });
    view.add_controller(click);
}
