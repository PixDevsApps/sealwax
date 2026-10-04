//! The main window: S3's three panes (folders, the INBOX list, a reader) in an
//! `adw::ApplicationWindow`, plus a banner for forwarded `mailto:` drafts and an instructions
//! panel for the manual checks.

use crate::model::{MessageItem, MessageList};
use crate::probe::Probe;
use crate::row::MessageRow;
use crate::store::ReadRequest;
use email_spike_s3::sections::{LABELS, Thresholds};
use email_spike_s3::store::ListRow;
use gtk4::prelude::*;
use gtk4::{glib, pango};
use libadwaita as adw;
use std::rc::Rc;

#[derive(Clone)]
pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    pub list: gtk4::ListView,
    pub selection: gtk4::SingleSelection,
    pub model: MessageList,
    pub probe: Rc<Probe>,
    banner: adw::Banner,
    subject: gtk4::Label,
    meta: gtk4::Label,
    body: gtk4::Label,
    instructions: gtk4::Box,
    instructions_title: gtk4::Label,
    instructions_text: gtk4::Label,
}

impl MainWindow {
    pub fn new(
        app: &adw::Application,
        reads: async_channel::Sender<ReadRequest>,
        thresholds: Thresholds,
    ) -> Self {
        let probe = Probe::new();
        let model = MessageList::new(reads);
        let selection = gtk4::SingleSelection::builder()
            .model(&model)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let factory = gtk4::SignalListItemFactory::new();
        let row_probe = probe.clone();
        factory.connect_setup(move |_, object| {
            if let Some(item) = object.downcast_ref::<gtk4::ListItem>() {
                item.set_child(Some(&MessageRow::new(row_probe.clone(), thresholds)));
            }
        });
        factory.connect_bind(|_, object| {
            let Some(item) = object.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            if let (Some(row), Some(message)) = (
                item.child().and_downcast::<MessageRow>(),
                item.item().and_downcast::<MessageItem>(),
            ) {
                row.bind(&message);
            }
        });
        factory.connect_unbind(|_, object| {
            if let Some(row) = object
                .downcast_ref::<gtk4::ListItem>()
                .and_then(|item| item.child().and_downcast::<MessageRow>())
            {
                row.unbind();
            }
        });
        let headers = gtk4::SignalListItemFactory::new();
        headers.connect_setup(|_, object| {
            if let Some(header) = object.downcast_ref::<gtk4::ListHeader>() {
                header.set_child(Some(
                    &gtk4::Label::builder()
                        .xalign(0.0)
                        .css_classes(["section-header"])
                        .build(),
                ));
            }
        });
        headers.connect_bind(|_, object| {
            let Some(header) = object.downcast_ref::<gtk4::ListHeader>() else {
                return;
            };
            if let (Some(label), Some(item)) = (
                header.child().and_downcast::<gtk4::Label>(),
                header.item().and_downcast::<MessageItem>(),
            ) {
                label.set_text(&LABELS[usize::from(item.section())].to_uppercase());
            }
        });
        let list = gtk4::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .header_factory(&headers)
            .css_classes(["message-list", "noliga"])
            .build();
        let scrolled = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let pane = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        pane.add_css_class("list-pane");
        pane.set_size_request(400, -1);
        pane.append(&list_head("Inbox"));
        pane.append(&scrolled);

        let subject = gtk4::Label::builder()
            .label("Hytteturen – endelig plan")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["reader-subject"])
            .build();
        let meta = gtk4::Label::builder()
            .label("Ingrid Solberg <ingrid@fjord.example> · to me · 10:32")
            .xalign(0.0)
            .css_classes(["reader-meta"])
            .build();
        let body = gtk4::Label::builder()
            .label(
                "Hei!\n\nJeg har booket hytta for helgen. Den har plass til seks, badstue og \
                 brøytet vei helt fram. Ta med blåbærsyltetøy og kanelboller.\n\nHilsen\nIngrid",
            )
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["reader-body"])
            .build();
        let instructions_title = gtk4::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["instructions-title"])
            .build();
        let instructions_text = gtk4::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["instructions"])
            .build();
        let instructions = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(12)
            .css_classes(["reader-card"])
            .visible(false)
            .build();
        instructions.append(&instructions_title);
        instructions.append(&instructions_text);
        let reader = reader_pane(&subject, &meta, &body, &instructions);

        let panes = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        panes.add_css_class("chrome");
        panes.set_vexpand(true);
        panes.append(&sidebar());
        panes.append(&pane);
        panes.append(&reader);
        let banner = adw::Banner::new("");
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.append(&banner);
        root.append(&panes);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Mail spike S6")
            .default_width(1600)
            .default_height(1000)
            .content(&root)
            .build();
        Self {
            window,
            list,
            selection,
            model,
            probe,
            banner,
            subject,
            meta,
            body,
            instructions,
            instructions_title,
            instructions_text,
        }
    }

    pub fn show_banner(&self, text: &str) {
        self.banner.set_title(text);
        self.banner.set_revealed(true);
    }

    pub fn set_instructions(&self, title: &str, text: &str) {
        self.instructions_title.set_text(title);
        self.instructions_text.set_text(text);
        self.instructions.set_visible(true);
    }

    /// Shows a cached message in the reader and selects it in the list when it is loaded there.
    pub fn show_message(&self, row: &ListRow) {
        self.subject.set_text(&row.subject);
        let when = glib::DateTime::from_unix_local(row.date)
            .ok()
            .and_then(|time| time.format("%e %b %Y %H:%M").ok())
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        self.meta
            .set_text(&format!("{} <{}> · {when}", row.from_name, row.from_addr));
        self.body.set_text(&row.snippet);
        if let Some(position) = self.model.position_of(row.id) {
            self.selection.set_selected(position);
            self.list
                .scroll_to(position, gtk4::ListScrollFlags::NONE, None);
        }
    }

    /// Shows a message that arrived over IMAP in background mode (no cache row).
    pub fn show_text(&self, subject: &str, meta: &str, body: &str) {
        self.subject.set_text(subject);
        self.meta.set_text(meta);
        self.body.set_text(body);
    }

    /// Keeps the frame clock running for a few frames, so GDK receives (and reports) the
    /// presentation feedback of a frame that was already painted.
    pub fn keep_ticking(&self, frames: u32) {
        let left = std::cell::Cell::new(frames);
        self.window.add_tick_callback(move |_, _| {
            let n = left.get();
            if n == 0 {
                return glib::ControlFlow::Break;
            }
            left.set(n - 1);
            glib::ControlFlow::Continue
        });
    }

    /// Pixel metrics of the first realized row, for the GDK_SCALE comparison.
    pub fn row_metrics(&self) -> Option<(i32, i32, i32)> {
        let mut child = self.list.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.first_child().and_downcast::<MessageRow>()
                && row.height() > 0
            {
                let (text_width, text_height) = row.sender_pixel_size();
                return Some((row.height(), text_width, text_height));
            }
            child = widget.next_sibling();
        }
        None
    }
}

fn sidebar() -> gtk4::Box {
    let bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(1)
        .width_request(232)
        .css_classes(["sidebar"])
        .build();
    bar.append(
        &gtk4::Button::builder()
            .label("Compose")
            .css_classes(["compose"])
            .build(),
    );
    let folder = |icon: &str, name: &str, current: bool| {
        let row = gtk4::Box::builder()
            .spacing(10)
            .css_classes(["folder"])
            .build();
        if current {
            row.add_css_class("current");
        }
        row.append(
            &gtk4::Image::builder()
                .icon_name(icon)
                .pixel_size(15)
                .build(),
        );
        row.append(
            &gtk4::Label::builder()
                .label(name)
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        row
    };
    bar.append(&folder("mail-inbox-symbolic", "All Inboxes", false));
    bar.append(
        &gtk4::Label::builder()
            .label("PERSONAL")
            .xalign(0.0)
            .css_classes(["account"])
            .build(),
    );
    for (icon, name, current) in [
        ("mail-inbox-symbolic", "Inbox", true),
        ("mail-send-symbolic", "Sent", false),
        ("folder-documents-symbolic", "Drafts", false),
        ("folder-symbolic", "Archive", false),
        ("mail-mark-junk-symbolic", "Junk", false),
        ("user-trash-symbolic", "Trash", false),
    ] {
        bar.append(&folder(icon, name, current));
    }
    bar
}

fn list_head(title: &str) -> gtk4::Box {
    let head = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(10)
        .css_classes(["list-head"])
        .build();
    head.append(
        &gtk4::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["list-title"])
            .build(),
    );
    head.append(
        &gtk4::SearchEntry::builder()
            .placeholder_text("Search — from: has:attachment is:unread")
            .build(),
    );
    let chips = gtk4::Box::builder().spacing(6).build();
    let all = gtk4::ToggleButton::builder()
        .label("All")
        .active(true)
        .css_classes(["chip"])
        .build();
    chips.append(&all);
    for name in ["Unread", "Starred", "Attachments"] {
        chips.append(
            &gtk4::ToggleButton::builder()
                .label(name)
                .css_classes(["chip"])
                .group(&all)
                .build(),
        );
    }
    head.append(&chips);
    head
}

fn reader_pane(
    subject: &gtk4::Label,
    meta: &gtk4::Label,
    body: &gtk4::Label,
    instructions: &gtk4::Box,
) -> gtk4::Box {
    let pane = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .hexpand(true)
        .css_classes(["reader"])
        .build();
    let bar = gtk4::Box::builder()
        .spacing(2)
        .css_classes(["reader-bar"])
        .build();
    for icon in [
        "mail-archive-symbolic",
        "user-trash-symbolic",
        "mail-mark-junk-symbolic",
        "mail-unread-symbolic",
        "starred-symbolic",
        "view-more-symbolic",
    ] {
        bar.append(
            &gtk4::Button::builder()
                .icon_name(icon)
                .css_classes(["flat"])
                .build(),
        );
    }
    pane.append(&bar);
    let column = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(18)
        .margin_top(28)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk4::Align::Center)
        .width_request(760)
        .build();
    column.append(instructions);
    column.append(subject);
    let card = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .css_classes(["reader-card"])
        .build();
    card.append(meta);
    card.append(body);
    column.append(&card);
    pane.append(&column);
    pane
}
