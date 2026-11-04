//! The message row from S3's `s3-list` in the configuration S3 recommends (text cut before it is
//! set, ligatures off through CSS): unread dot · avatar · sender, count and time · subject ·
//! two-line snippet · star and attachment column. Binding never changes CSS classes.

use crate::model::MessageItem;
use crate::probe::Probe;
use crate::theme;
use email_spike_s3::sections::Thresholds;
use email_spike_s3::store::flags;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{glib, graphene, gsk, pango};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

mod avatar_imp {
    use super::*;

    #[derive(Default)]
    pub struct Avatar {
        pub layout: OnceCell<pango::Layout>,
        pub tint: Cell<usize>,
        pub placeholder: Cell<bool>,
        pub round: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Avatar {
        const NAME: &'static str = "S6Avatar";
        type Type = super::Avatar;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for Avatar {}

    impl WidgetImpl for Avatar {
        fn measure(&self, _orientation: gtk4::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (32, 32, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let widget = self.obj();
            let (width, height) = (widget.width() as f32, widget.height() as f32);
            let bounds = graphene::Rect::new(0.0, 0.0, width, height);
            let radius = if self.round.get() { width / 2.0 } else { 0.0 };
            theme::with_colors(|colors| {
                let tint = self.tint.get() % colors.tints.len();
                let fill = if self.placeholder.get() {
                    &colors.placeholder
                } else {
                    &colors.tints[tint]
                };
                snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, radius));
                snapshot.append_color(fill, &bounds);
                snapshot.pop();
                if self.placeholder.get() {
                    return;
                }
                if let Some(layout) = self.layout.get() {
                    let (text_width, text_height) = layout.pixel_size();
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(
                        (width - text_width as f32) / 2.0,
                        (height - text_height as f32) / 2.0,
                    ));
                    snapshot.append_layout(layout, &colors.tint_fg[tint]);
                    snapshot.restore();
                }
            });
        }
    }
}

glib::wrapper! {
    pub struct Avatar(ObjectSubclass<avatar_imp::Avatar>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Avatar {
    fn new(round: bool) -> Self {
        let avatar: Self = glib::Object::builder()
            .property("valign", gtk4::Align::Start)
            .build();
        let layout = avatar.create_pango_layout(None);
        layout.set_font_description(Some(&pango::FontDescription::from_string(
            "monospace Bold 11px",
        )));
        let _ = avatar.imp().layout.set(layout);
        avatar.imp().round.set(round);
        avatar.imp().placeholder.set(true);
        avatar
    }

    fn show(&self, initials: Option<(&str, usize)>) {
        let imp = self.imp();
        match initials {
            Some((text, tint)) => {
                if let Some(layout) = imp.layout.get() {
                    layout.set_text(text);
                }
                imp.tint.set(tint);
                imp.placeholder.set(false);
            }
            None => imp.placeholder.set(true),
        }
        self.queue_draw();
    }
}

mod dot_imp {
    use super::*;

    #[derive(Default)]
    pub struct Dot {
        pub on: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Dot {
        const NAME: &'static str = "S6UnreadDot";
        type Type = super::Dot;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for Dot {}

    impl WidgetImpl for Dot {
        fn measure(&self, _orientation: gtk4::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (7, 7, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            if !self.on.get() {
                return;
            }
            let bounds = graphene::Rect::new(0.0, 0.0, 7.0, 7.0);
            theme::with_colors(|colors| {
                snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, 3.5));
                snapshot.append_color(&colors.accent, &bounds);
                snapshot.pop();
            });
        }
    }
}

glib::wrapper! {
    pub struct Dot(ObjectSubclass<dot_imp::Dot>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Dot {
    fn set_on(&self, on: bool) {
        if self.imp().on.replace(on) != on {
            self.queue_draw();
        }
    }
}

fn cut(text: &str, chars: Option<usize>) -> &str {
    match chars.and_then(|chars| text.char_indices().nth(chars)) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

struct Parts {
    dot: Dot,
    avatar: Avatar,
    sender: gtk4::Label,
    count: gtk4::Label,
    time: gtk4::Label,
    subject: gtk4::Label,
    snippet: gtk4::Label,
    star: gtk4::Image,
    clip: gtk4::Image,
}

struct Bound {
    item: MessageItem,
    handler: glib::SignalHandlerId,
    placeholder: bool,
}

mod row_imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageRow {
        pub(super) parts: OnceCell<Parts>,
        pub(super) bound: RefCell<Option<Bound>>,
        pub(super) probe: OnceCell<Rc<Probe>>,
        pub(super) thresholds: Cell<Option<Thresholds>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRow {
        const NAME: &'static str = "S6MessageRow";
        type Type = super::MessageRow;
        type ParentType = gtk4::Box;
    }

    impl ObjectImpl for MessageRow {}
    impl WidgetImpl for MessageRow {}
    impl BoxImpl for MessageRow {}
}

glib::wrapper! {
    pub struct MessageRow(ObjectSubclass<row_imp::MessageRow>)
        @extends gtk4::Box, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget, gtk4::Orientable;
}

thread_local! {
    static BOLD: pango::AttrList = {
        let list = pango::AttrList::new();
        list.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        list
    };
    static REGULAR: pango::AttrList = pango::AttrList::new();
}

fn label(class: &str) -> gtk4::Label {
    gtk4::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes([class])
        .build()
}

fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |word: &str| {
        word.chars()
            .next()
            .map(|c| c.to_uppercase().collect::<String>())
    };
    match words.as_slice() {
        [] => String::new(),
        [single] => first(single).unwrap_or_default(),
        [head, .., last] => format!(
            "{}{}",
            first(head).unwrap_or_default(),
            first(last).unwrap_or_default()
        ),
    }
}

fn tint_of(address: &str) -> usize {
    let hash = address
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
        });
    (hash % 7) as usize
}

fn time_label(date: i64, thresholds: &Thresholds) -> String {
    let Ok(time) = glib::DateTime::from_unix_local(date) else {
        return String::new();
    };
    let format = match thresholds.section_of(date) {
        0 | 1 => "%H:%M",
        2 => "%a",
        _ if glib::DateTime::now_local().is_ok_and(|now| now.year() == time.year()) => "%e %b",
        _ => "%d.%m.%Y",
    };
    time.format(format)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

impl MessageRow {
    pub fn new(probe: Rc<Probe>, thresholds: Thresholds) -> Self {
        let row: Self = glib::Object::builder()
            .property("orientation", gtk4::Orientation::Horizontal)
            .property("spacing", 10)
            .build();
        row.add_css_class("message-row");
        let dot = Dot::default_new();
        let dot_column = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .width_request(8)
            .margin_top(12)
            .halign(gtk4::Align::Center)
            .build();
        dot.set_halign(gtk4::Align::Center);
        dot_column.append(&dot);
        let avatar = Avatar::new(true);
        let sender = label("sender");
        sender.set_hexpand(true);
        let count = gtk4::Label::builder()
            .css_classes(["count"])
            .visible(false)
            .build();
        let time = gtk4::Label::builder().css_classes(["time"]).build();
        let top = gtk4::Box::builder().spacing(6).build();
        top.append(&sender);
        top.append(&count);
        top.append(&time);
        let subject = label("subject");
        let snippet = gtk4::Label::builder()
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .lines(2)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .css_classes(["snippet"])
            .build();
        let content = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        content.append(&top);
        content.append(&subject);
        content.append(&snippet);
        let star = gtk4::Image::builder()
            .icon_name("starred-symbolic")
            .pixel_size(13)
            .css_classes(["star"])
            .opacity(0.0)
            .build();
        let clip = gtk4::Image::builder()
            .icon_name("mail-attachment-symbolic")
            .pixel_size(13)
            .css_classes(["clip"])
            .opacity(0.0)
            .build();
        let indicators = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(8)
            .width_request(16)
            .margin_top(22)
            .build();
        indicators.append(&star);
        indicators.append(&clip);
        row.append(&dot_column);
        row.append(&avatar);
        row.append(&content);
        row.append(&indicators);
        let imp = row.imp();
        let _ = imp.parts.set(Parts {
            dot,
            avatar,
            sender,
            count,
            time,
            subject,
            snippet,
            star,
            clip,
        });
        let _ = imp.probe.set(probe);
        imp.thresholds.set(Some(thresholds));
        row
    }

    pub fn bind(&self, item: &MessageItem) {
        self.unbind();
        let handler = item.connect_loaded_notify(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |item| row.loaded(item)
        ));
        let placeholder = item.row().is_none();
        *self.imp().bound.borrow_mut() = Some(Bound {
            item: item.clone(),
            handler,
            placeholder,
        });
        if let Some(probe) = self.imp().probe.get() {
            probe.bind(placeholder);
        }
        self.show(item);
    }

    /// The sender label's laid-out text size in logical pixels.
    pub fn sender_pixel_size(&self) -> (i32, i32) {
        self.imp()
            .parts
            .get()
            .map_or((0, 0), |parts| parts.sender.layout().pixel_size())
    }

    pub fn unbind(&self) {
        if let Some(bound) = self.imp().bound.borrow_mut().take() {
            bound.item.disconnect(bound.handler);
            if let Some(probe) = self.imp().probe.get() {
                probe.unbind(bound.placeholder);
            }
        }
    }

    fn loaded(&self, item: &MessageItem) {
        let was_placeholder = {
            let mut bound = self.imp().bound.borrow_mut();
            match bound.as_mut() {
                Some(bound) if bound.item == *item => {
                    std::mem::replace(&mut bound.placeholder, false)
                }
                _ => return,
            }
        };
        if was_placeholder && let Some(probe) = self.imp().probe.get() {
            probe.resolved();
        }
        self.show(item);
    }

    fn show(&self, item: &MessageItem) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        let row = item.row();
        let Some(row) = row.as_ref() else {
            parts.dot.set_on(false);
            parts.avatar.show(None);
            parts.sender.set_text("");
            parts.time.set_text("");
            parts.subject.set_text("");
            parts.snippet.set_text("");
            parts.count.set_visible(false);
            parts.star.set_opacity(0.0);
            parts.clip.set_opacity(0.0);
            return;
        };
        let (sender_chars, subject_chars, snippet_chars) = (Some(48), Some(100), Some(140));
        let unread = row.flags & flags::SEEN == 0;
        let weight = |list: &pango::AttrList| {
            parts.sender.set_attributes(Some(list));
            parts.subject.set_attributes(Some(list));
        };
        if unread {
            BOLD.with(weight)
        } else {
            REGULAR.with(weight)
        }
        parts.sender.set_text(cut(&row.from_name, sender_chars));
        parts.subject.set_text(cut(&row.subject, subject_chars));
        parts.dot.set_on(unread);
        parts
            .avatar
            .show(Some((&initials(&row.from_name), tint_of(&row.from_addr))));
        if let Some(thresholds) = self.imp().thresholds.get() {
            parts.time.set_text(&time_label(row.date, &thresholds));
        }
        parts.snippet.set_text(cut(&row.snippet, snippet_chars));
        if row.thread_count > 1 {
            parts.count.set_text(&row.thread_count.to_string());
            parts.count.set_visible(true);
        } else {
            parts.count.set_visible(false);
        }
        parts.star.set_opacity(if row.flags & flags::FLAGGED != 0 {
            1.0
        } else {
            0.0
        });
        parts
            .clip
            .set_opacity(if row.has_attachments { 1.0 } else { 0.0 });
    }
}

impl Dot {
    fn default_new() -> Self {
        glib::Object::new()
    }
}
