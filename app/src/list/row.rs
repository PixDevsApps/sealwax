//! The message row (S3's recommended row, DESIGN.md "Message list"): unread dot · avatar ·
//! sender, conversation count and time · subject · two-line snippet · star and attachment.
//!
//! Binding is kept cheap, because GtkListView lays out up to ~205 rows in one frame on a jump
//! (S3): text is cut to what can show before it is set (−15 % layout), the list has no ligatures
//! (style.css), weight comes from Pango attributes, the dot and the indicators fade with opacity,
//! and nothing changes unless it differs from what the row shows. The only class that changes is
//! the avatar's tint (`sw-avatar-tint-<n>`, from the theme). While the list moves faster than
//! ~30,000 px/s rows show their placeholder look instead of text (S3).

use super::item::MessageItem;
use super::sections::LABELS;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{glib, pango};
use sealwax_domain::model::{MessageId, MessageSummary};
use sealwax_domain::theme::{avatar_index, initials};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// Characters a row shows at most, at a 400 px list (S3): sender, subject, snippet.
const CUT: (usize, usize, usize) = (48, 100, 140);

const TINTS: [&str; 7] = [
    "sw-avatar-tint-0",
    "sw-avatar-tint-1",
    "sw-avatar-tint-2",
    "sw-avatar-tint-3",
    "sw-avatar-tint-4",
    "sw-avatar-tint-5",
    "sw-avatar-tint-6",
];

/// What the rows of one list share.
pub struct RowContext {
    /// Rows bound now show their placeholder look: the list moves too fast to read.
    pub skeleton: Cell<bool>,
    /// How much more text fits than at 400 px (the list's width / 400, in half steps, ≥ 1).
    pub scale: Cell<f32>,
    /// This year, for dates.
    pub year: Cell<i32>,
    pub bound: RefCell<Vec<MessageRow>>,
    pub headers: RefCell<Vec<SectionHeader>>,
    /// Rows bound so far.
    pub binds: Cell<u64>,
    /// Row widgets created so far.
    pub rows_created: Cell<u64>,
}

impl RowContext {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            skeleton: Cell::new(false),
            scale: Cell::new(1.0),
            year: Cell::new(
                glib::DateTime::now_local()
                    .map(|now| now.year())
                    .unwrap_or_default(),
            ),
            bound: RefCell::new(Vec::new()),
            headers: RefCell::new(Vec::new()),
            binds: Cell::new(0),
            rows_created: Cell::new(0),
        })
    }

    fn limits(&self) -> (usize, usize, usize) {
        let scale = self.scale.get();
        let cut = |chars: usize| (chars as f32 * scale) as usize;
        (cut(CUT.0), cut(CUT.1), cut(CUT.2))
    }
}

fn cut(text: &str, chars: usize) -> &str {
    match text.char_indices().nth(chars) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// "10:32" today and yesterday, the weekday this week, "4 Oct" this year, else "04.10.2025".
pub fn time_text(date: i64, section: usize, year: i32) -> String {
    let Ok(time) = glib::DateTime::from_unix_local(date) else {
        return String::new();
    };
    let format = match section {
        0 | 1 => "%H:%M",
        2 => "%a",
        _ if time.year() == year => "%e %b",
        _ => "%d.%m.%Y",
    };
    time.format(format)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

fn sender(row: &MessageSummary) -> &str {
    if row.sender_name.trim().is_empty() {
        &row.sender_email
    } else {
        &row.sender_name
    }
}

/// What a screen reader says for the row: state in words as well as in weight and colour.
fn accessible_text(row: &MessageSummary, time: &str) -> String {
    let mut text = String::new();
    if !row.flags.is_seen() {
        text.push_str("Unread, ");
    }
    text.push_str(sender(row));
    text.push_str(", ");
    text.push_str(if row.subject.is_empty() {
        "no subject"
    } else {
        &row.subject
    });
    text.push_str(", ");
    text.push_str(time);
    if row.thread_count > 1 {
        text.push_str(&format!(", {} messages", row.thread_count));
    }
    if row.flags.is_flagged() {
        text.push_str(", starred");
    }
    if row.has_attachments {
        text.push_str(", has attachments");
    }
    text
}

thread_local! {
    static BOLD: pango::AttrList = {
        let list = pango::AttrList::new();
        list.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        list
    };
}

struct Parts {
    dot: gtk::Box,
    avatar: gtk::Label,
    sender: gtk::Label,
    count: gtk::Label,
    time: gtk::Label,
    subject: gtk::Label,
    snippet: gtk::Label,
    star: gtk::Image,
    clip: gtk::Image,
}

struct Bound {
    item: MessageItem,
    list_item: glib::WeakRef<gtk::ListItem>,
    handlers: [glib::SignalHandlerId; 2],
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageRow {
        pub(super) parts: OnceCell<Parts>,
        pub(super) context: OnceCell<Rc<RowContext>>,
        pub(super) bound: RefCell<Option<Bound>>,
        /// Shows the placeholder look although its row is loaded (the list was moving fast).
        pub(super) skeleton: Cell<bool>,
        pub(super) bold: Cell<bool>,
        pub(super) tint: Cell<Option<usize>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRow {
        const NAME: &'static str = "SealwaxMessageRow";
        type Type = super::MessageRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for MessageRow {}
    impl WidgetImpl for MessageRow {}
    impl BoxImpl for MessageRow {}
}

glib::wrapper! {
    pub struct MessageRow(ObjectSubclass<imp::MessageRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

fn line(class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .single_line_mode(true)
        .build();
    label.add_css_class(class);
    label
}

impl MessageRow {
    pub fn new(context: Rc<RowContext>) -> Self {
        context.rows_created.set(context.rows_created.get() + 1);
        let row: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .build();
        row.add_css_class("sw-message-row");
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Center)
            .opacity(0.0)
            .build();
        dot.add_css_class("sw-unread-dot");
        let avatar = gtk::Label::builder()
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Center)
            .xalign(0.5)
            .build();
        avatar.add_css_class("sw-avatar");
        let sender = line("sw-sender");
        sender.set_hexpand(true);
        let count = gtk::Label::builder().visible(false).build();
        count.add_css_class("sw-thread-count");
        let time = gtk::Label::new(None);
        time.add_css_class("sw-time");
        let top = gtk::Box::builder().spacing(6).build();
        top.append(&sender);
        top.append(&count);
        top.append(&time);
        let subject = line("sw-subject");
        let snippet = gtk::Label::builder()
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .lines(2)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .build();
        snippet.add_css_class("sw-snippet");
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        content.append(&top);
        content.append(&subject);
        content.append(&snippet);
        let star = gtk::Image::builder()
            .icon_name("starred-symbolic")
            .opacity(0.0)
            .build();
        star.add_css_class("sw-star");
        let clip = gtk::Image::builder()
            .icon_name("mail-attachment-symbolic")
            .opacity(0.0)
            .build();
        clip.add_css_class("sw-indicator");
        let indicators = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Start)
            .build();
        indicators.add_css_class("sw-row-indicators");
        indicators.append(&star);
        indicators.append(&clip);
        row.append(&dot);
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
        let _ = imp.context.set(context);
        row
    }

    fn context(&self) -> &Rc<RowContext> {
        self.imp()
            .context
            .get()
            .expect("rows are built with a context")
    }

    pub fn bind(&self, list_item: &gtk::ListItem, item: &MessageItem) {
        self.unbind();
        let changed = item.connect_changed(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_| row.show()
        ));
        let section = item.connect_section_changed(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_| row.show()
        ));
        self.imp().bound.replace(Some(Bound {
            item: item.clone(),
            list_item: list_item.downgrade(),
            handlers: [changed, section],
        }));
        let context = self.context();
        context.binds.set(context.binds.get() + 1);
        context.bound.borrow_mut().push(self.clone());
        self.show();
    }

    pub fn unbind(&self) {
        let Some(bound) = self.imp().bound.take() else {
            return;
        };
        for handler in bound.handlers {
            bound.item.disconnect(handler);
        }
        self.context().bound.borrow_mut().retain(|row| row != self);
    }

    pub fn item(&self) -> Option<MessageItem> {
        self.imp()
            .bound
            .borrow()
            .as_ref()
            .map(|bound| bound.item.clone())
    }

    /// The row's position in the list now.
    pub fn position(&self) -> Option<u32> {
        let list_item = self
            .imp()
            .bound
            .borrow()
            .as_ref()
            .and_then(|bound| bound.list_item.upgrade())?;
        Some(list_item.position()).filter(|position| *position != gtk::INVALID_LIST_POSITION)
    }

    pub fn id(&self) -> Option<MessageId> {
        self.item().and_then(|item| item.id())
    }

    /// Whether the row shows its placeholder look: its row isn't loaded, or the list moved fast.
    pub fn is_placeholder(&self) -> bool {
        self.imp().skeleton.get() || self.item().is_none_or(|item| !item.is_loaded())
    }

    /// Shows what the row is bound to again, as after a fast scroll or a resize.
    pub fn refresh(&self) {
        self.show();
    }

    pub fn shows_skeleton(&self) -> bool {
        self.imp().skeleton.get()
    }

    /// The texts the row shows: sender, subject, snippet, time (for the self-test).
    pub fn texts(&self) -> [String; 4] {
        let Some(parts) = self.imp().parts.get() else {
            return Default::default();
        };
        [
            parts.sender.text().to_string(),
            parts.subject.text().to_string(),
            parts.snippet.text().to_string(),
            parts.time.text().to_string(),
        ]
    }

    fn show(&self) {
        let Some(item) = self.item() else {
            return;
        };
        let skeleton = self.context().skeleton.get();
        let row = item.row();
        match row.as_ref() {
            Some(summary) if !skeleton => {
                self.imp().skeleton.set(false);
                self.show_message(summary, item.section());
            }
            Some(_) => {
                self.imp().skeleton.set(true);
                self.show_placeholder();
            }
            None => {
                self.imp().skeleton.set(false);
                self.show_placeholder();
            }
        }
    }

    fn set_bold(&self, parts: &Parts, bold: bool) {
        if self.imp().bold.replace(bold) == bold {
            return;
        }
        if bold {
            BOLD.with(|list| {
                parts.sender.set_attributes(Some(list));
                parts.subject.set_attributes(Some(list));
            });
        } else {
            parts.sender.set_attributes(None);
            parts.subject.set_attributes(None);
        }
    }

    fn set_tint(&self, parts: &Parts, tint: usize) {
        let previous = self.imp().tint.replace(Some(tint));
        if previous == Some(tint) {
            return;
        }
        if let Some(previous) = previous {
            parts.avatar.remove_css_class(TINTS[previous]);
        }
        parts.avatar.add_css_class(TINTS[tint]);
    }

    fn show_message(&self, row: &MessageSummary, section: usize) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        let (sender_chars, subject_chars, snippet_chars) = self.context().limits();
        let unread = !row.flags.is_seen();
        self.set_bold(parts, unread);
        set_opacity(&parts.dot, unread);
        set_opacity(&parts.avatar, true);
        self.set_tint(parts, avatar_index(&row.sender_email));
        parts
            .avatar
            .set_text(&initials(&row.sender_name, &row.sender_email));
        parts.sender.set_text(cut(sender(row), sender_chars));
        let time = time_text(row.date, section, self.context().year.get());
        parts.time.set_text(&time);
        parts.subject.set_text(if row.subject.is_empty() {
            "(no subject)"
        } else {
            cut(&row.subject, subject_chars)
        });
        parts.snippet.set_text(cut(&row.snippet, snippet_chars));
        if row.thread_count > 1 {
            parts.count.set_text(&row.thread_count.to_string());
            parts.count.set_visible(true);
        } else {
            parts.count.set_visible(false);
        }
        set_opacity(&parts.star, row.flags.is_flagged());
        set_opacity(&parts.clip, row.has_attachments);
        if let Some(list_item) = self
            .imp()
            .bound
            .borrow()
            .as_ref()
            .and_then(|bound| bound.list_item.upgrade())
        {
            list_item.set_accessible_label(&accessible_text(row, &time));
        }
    }

    /// The row's shape without its text: a row that is loading, or one the list moves past too
    /// fast to read. Single spaces keep every line's height, so rows don't change size.
    fn show_placeholder(&self) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        self.set_bold(parts, false);
        set_opacity(&parts.dot, false);
        parts.avatar.set_text("");
        parts.avatar.set_opacity(0.4);
        parts.sender.set_text(" ");
        parts.time.set_text(" ");
        parts.subject.set_text(" ");
        parts.snippet.set_text(" ");
        parts.count.set_visible(false);
        set_opacity(&parts.star, false);
        set_opacity(&parts.clip, false);
    }
}

fn set_opacity(widget: &impl IsA<gtk::Widget>, shown: bool) {
    let opacity = if shown { 1.0 } else { 0.0 };
    if widget.opacity() != opacity {
        widget.set_opacity(opacity);
    }
}

mod header_imp {
    use super::*;

    #[derive(Default)]
    pub struct SectionHeader {
        pub(super) bound: RefCell<Option<(MessageItem, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SectionHeader {
        const NAME: &'static str = "SealwaxSectionHeader";
        type Type = super::SectionHeader;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for SectionHeader {}
    impl WidgetImpl for SectionHeader {}
    impl BoxImpl for SectionHeader {}
}

glib::wrapper! {
    /// "TODAY", "YESTERDAY", …: the header GtkListView shows above each section.
    pub struct SectionHeader(ObjectSubclass<header_imp::SectionHeader>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for SectionHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl SectionHeader {
    pub fn new() -> Self {
        let header: Self = glib::Object::new();
        let label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        label.add_css_class("sw-section-header");
        header.append(&label);
        header
    }

    fn label(&self) -> Option<gtk::Label> {
        self.first_child().and_downcast::<gtk::Label>()
    }

    pub fn bind(&self, item: &MessageItem, context: &RowContext) {
        self.unbind(context);
        let handler = item.connect_section_changed(glib::clone!(
            #[weak(rename_to = header)]
            self,
            move |item| header.show(item.section())
        ));
        self.imp().bound.replace(Some((item.clone(), handler)));
        context.headers.borrow_mut().push(self.clone());
        self.show(item.section());
    }

    pub fn unbind(&self, context: &RowContext) {
        if let Some((item, handler)) = self.imp().bound.take() {
            item.disconnect(handler);
            context.headers.borrow_mut().retain(|header| header != self);
        }
    }

    fn show(&self, section: usize) {
        if let Some(label) = self.label() {
            label.set_text(&LABELS[section].to_uppercase());
        }
    }

    pub fn text(&self) -> String {
        self.label()
            .map(|label| label.text().to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{Flags, FolderId, ThreadId};
    use sealwax_domain::theme::avatar_class;

    #[test]
    fn text_is_cut_at_characters_not_bytes() {
        assert_eq!(cut("blåbærsyltetøy", 5), "blåbæ");
        assert_eq!(cut("会议纪要", 2), "会议");
        assert_eq!(cut("short", 48), "short");
    }

    #[test]
    fn tint_classes_are_the_themes() {
        for address in ["ingrid@example.test", "", "LEAH@example.test"] {
            assert_eq!(TINTS[avatar_index(address)], avatar_class(address));
        }
    }

    #[test]
    fn rows_say_their_state_in_words() {
        let row = MessageSummary {
            id: MessageId(1),
            folder: FolderId(1),
            thread: ThreadId(1),
            sender_name: String::new(),
            sender_email: "leah@example.test".into(),
            subject: String::new(),
            snippet: String::new(),
            date: 0,
            flags: Flags::FLAGGED,
            has_attachments: true,
            thread_count: 3,
        };
        assert_eq!(
            accessible_text(&row, "10:32"),
            "Unread, leah@example.test, no subject, 10:32, 3 messages, starred, has attachments"
        );
    }
}
