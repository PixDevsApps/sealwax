//! The message row (S3's recommended row, DESIGN.md "Message list"): unread dot · avatar ·
//! sender, conversation count and time · subject · two-line snippet · star and attachment.
//!
//! Binding is kept cheap, because GtkListView lays out up to ~205 rows in one frame on a jump
//! (S3): text is cut to what can show before it is set (−15 % layout), the list has no ligatures
//! (style.css), weight comes from Pango attributes, the dot and the indicators fade with opacity,
//! and nothing changes unless it differs from what the row shows. The only class that changes is
//! the avatar's tint (`sw-avatar-tint-<n>`, from the theme). While the list moves faster than
//! ~30,000 px/s rows show their placeholder look instead of text (S3).
//!
//! In the select mode (ML-054) a checkbox takes the avatar's place, so the text doesn't move;
//! it follows the row's selection and toggles it. Hovering a row shows its quick actions
//! (archive, delete, star) where the time was; they are built on the first hover.

use super::item::MessageItem;
use super::sections::LABELS;
use crate::triage::Target;
use crate::window::panes::icon_button;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{glib, pango};
use sealwax_domain::actions::ActionId;
use sealwax_domain::model::{MessageId, MessageSummary};
use sealwax_domain::theme::{avatar_index, initials};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// A row's quick actions, shown while the pointer is over it (DESIGN.md "Message list").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quick {
    Archive,
    Delete,
    Star,
}

impl Quick {
    pub const ALL: [Quick; 3] = [Quick::Archive, Quick::Delete, Quick::Star];

    pub fn action(self) -> ActionId {
        match self {
            Quick::Archive => ActionId::Archive,
            Quick::Delete => ActionId::Delete,
            Quick::Star => ActionId::Star,
        }
    }

    fn icons(self) -> &'static [&'static str] {
        match self {
            Quick::Archive => &["mail-archive-symbolic", "folder-documents-symbolic"],
            Quick::Delete => &["user-trash-symbolic"],
            Quick::Star => &["starred-symbolic"],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Quick::Archive => "archive",
            Quick::Delete => "delete",
            Quick::Star => "star",
        }
    }
}

type QuickCallback = Rc<dyn Fn(ActionId, Target)>;

/// A row's quick actions: their box and each button.
type QuickActions = (gtk::Box, Vec<(Quick, gtk::Button)>);

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
    /// Rows show a checkbox in the avatar's place.
    pub select_mode: Cell<bool>,
    quick: RefCell<Option<QuickCallback>>,
    /// Each quick action's tooltip, with its keys.
    quick_tooltips: RefCell<Vec<(Quick, String)>>,
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
            select_mode: Cell::new(false),
            quick: RefCell::new(None),
            quick_tooltips: RefCell::new(Vec::new()),
        })
    }

    /// Runs `callback` when a row's quick action is clicked.
    pub fn set_quick(&self, callback: impl Fn(ActionId, Target) + 'static) {
        self.quick.replace(Some(Rc::new(callback)));
    }

    pub fn set_quick_tooltips(&self, tooltips: Vec<(Quick, String)>) {
        self.quick_tooltips.replace(tooltips);
        for row in self.bound.borrow().iter() {
            row.update_quick_tooltips();
        }
    }

    fn quick_tooltip(&self, quick: Quick) -> String {
        self.quick_tooltips
            .borrow()
            .iter()
            .find(|(candidate, _)| *candidate == quick)
            .map_or_else(
                || quick.action().title().to_owned(),
                |(_, text)| text.clone(),
            )
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
    check: gtk::CheckButton,
    top: gtk::Box,
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
    selected: Option<glib::SignalHandlerId>,
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
        /// The quick actions, once the row was first hovered.
        pub(super) quick: RefCell<Option<QuickActions>>,
        pub(super) hovered: Cell<bool>,
        /// The checkbox is being set from the selection, not clicked.
        pub(super) syncing: Cell<bool>,
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
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Center)
            .focusable(false)
            .visible(false)
            .build();
        check.add_css_class("sw-row-check");
        check.update_property(&[gtk::accessible::Property::Label("Selected")]);
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
        row.append(&check);
        row.append(&content);
        row.append(&indicators);
        check.connect_toggled(glib::clone!(
            #[weak]
            row,
            move |_| row.check_toggled()
        ));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter(glib::clone!(
            #[weak]
            row,
            move |_, _, _| row.set_hovered(true)
        ));
        motion.connect_leave(glib::clone!(
            #[weak]
            row,
            move |_| row.set_hovered(false)
        ));
        row.add_controller(motion);
        let imp = row.imp();
        let _ = imp.parts.set(Parts {
            dot,
            avatar,
            check,
            top,
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

    /// The user clicked the checkbox: the row's selection follows, the others stay.
    fn check_toggled(&self) {
        if self.imp().syncing.get() {
            return;
        }
        let Some(position) = self.position() else {
            return;
        };
        if self
            .activate_action(
                "list.select-item",
                Some(&(position, true, false).to_variant()),
            )
            .is_err()
        {
            tracing::warn!("a row's checkbox has no list.select-item");
        }
    }

    /// The checkbox shows whether the row is selected.
    fn sync_check(&self) {
        let (Some(parts), Some(list_item)) = (
            self.imp().parts.get(),
            self.imp()
                .bound
                .borrow()
                .as_ref()
                .and_then(|bound| bound.list_item.upgrade()),
        ) else {
            return;
        };
        let selected = list_item.is_selected();
        if parts.check.is_active() != selected {
            self.imp().syncing.set(true);
            parts.check.set_active(selected);
            self.imp().syncing.set(false);
        }
    }

    fn set_hovered(&self, hovered: bool) {
        if self.imp().hovered.replace(hovered) == hovered {
            return;
        }
        self.show_quick();
    }

    /// The quick actions show while the pointer is over a loaded row, outside the select mode,
    /// in the time's place.
    fn show_quick(&self) {
        let Some(parts) = self.imp().parts.get() else {
            return;
        };
        let shown =
            self.imp().hovered.get() && !self.context().select_mode.get() && !self.is_placeholder();
        if shown && self.imp().quick.borrow().is_none() {
            let quick = gtk::Box::builder().spacing(2).visible(false).build();
            quick.add_css_class("sw-quick-actions");
            let mut buttons = Vec::new();
            for action in Quick::ALL {
                let button = icon_button(action.icons(), &self.context().quick_tooltip(action));
                button.add_css_class("sw-quick-action");
                button.set_focusable(false);
                button.set_widget_name(&format!("quick-{}", action.name()));
                button.connect_clicked(glib::clone!(
                    #[weak(rename_to = row)]
                    self,
                    move |_| row.quick_clicked(action)
                ));
                quick.append(&button);
                buttons.push((action, button));
            }
            parts.top.append(&quick);
            self.imp().quick.replace(Some((quick, buttons)));
        }
        if let Some((quick, _)) = self.imp().quick.borrow().as_ref() {
            quick.set_visible(shown);
        }
        parts.time.set_visible(!shown);
    }

    fn update_quick_tooltips(&self) {
        if let Some((_, buttons)) = self.imp().quick.borrow().as_ref() {
            for (action, button) in buttons {
                let text = self.context().quick_tooltip(*action);
                button.set_tooltip_text(Some(&text));
                button.update_property(&[gtk::accessible::Property::Label(&text)]);
            }
        }
    }

    fn quick_clicked(&self, action: Quick) {
        let target = self.item().and_then(|item| {
            item.row().as_ref().map(|row| Target {
                id: row.id,
                folder: row.folder,
                flags: row.flags,
            })
        });
        let callback = self.context().quick.borrow().clone();
        if let (Some(target), Some(callback)) = (target, callback) {
            callback(action.action(), target);
        }
    }

    /// Whether the row shows its quick actions now (for the self-test).
    pub fn shows_quick(&self) -> bool {
        self.imp()
            .quick
            .borrow()
            .as_ref()
            .is_some_and(|(quick, _)| quick.is_visible())
    }

    /// The quick action button for `action`, once the row has been hovered.
    pub fn quick_button(&self, action: Quick) -> Option<gtk::Button> {
        self.imp().quick.borrow().as_ref().and_then(|(_, buttons)| {
            buttons
                .iter()
                .find(|(candidate, _)| *candidate == action)
                .map(|(_, button)| button.clone())
        })
    }

    /// The checkbox, while the row shows it (the self-test clicks it).
    pub fn check_button(&self) -> Option<gtk::CheckButton> {
        let parts = self.imp().parts.get()?;
        parts.check.is_visible().then(|| parts.check.clone())
    }

    /// Whether the row shows its checkbox, and whether it is ticked.
    pub fn check_state(&self) -> Option<bool> {
        let parts = self.imp().parts.get()?;
        parts.check.is_visible().then(|| parts.check.is_active())
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
        let selected = list_item.connect_selected_notify(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_| row.sync_check()
        ));
        self.imp().bound.replace(Some(Bound {
            item: item.clone(),
            list_item: list_item.downgrade(),
            handlers: [changed, section],
            selected: Some(selected),
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
        if let (Some(list_item), Some(handler)) = (bound.list_item.upgrade(), bound.selected) {
            list_item.disconnect(handler);
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
        self.show_check(parts, self.context().select_mode.get());
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
        self.show_quick();
    }

    /// In the select mode the checkbox takes the avatar's place.
    fn show_check(&self, parts: &Parts, select_mode: bool) {
        if parts.check.is_visible() != select_mode {
            parts.check.set_visible(select_mode);
            parts.avatar.set_visible(!select_mode);
        }
        if select_mode {
            self.sync_check();
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
        self.show_check(parts, false);
        self.show_quick();
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
