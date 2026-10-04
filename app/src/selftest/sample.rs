//! Fictional placeholder content for the self-test's screenshots, taken from the approved design
//! mock (every name, address and message there is made up): two accounts for the sidebar, a
//! message list and a message, in the theme's `sw-` classes. They stand in until the store
//! (ML-023), the list (ML-026) and the reader (ML-027) are connected.

use crate::window::panes::{classed, icon, icon_button};
use crate::window::{
    AccountStatus, FolderRole, SidebarAccount, SidebarFolder, SidebarModel, Window,
};
use gtk4 as gtk;
use gtk4::prelude::*;
use sealwax_domain::actions::ActionId;
use sealwax_domain::theme::{avatar_class, initials};

pub fn model() -> SidebarModel {
    SidebarModel {
        accounts: vec![
            account(
                "personal",
                "Personal",
                AccountStatus::Synced,
                &[12, 2, 0, 0, 0, 0],
                &[("Receipts", 0), ("Travel", 3)],
            ),
            account(
                "work",
                "Work",
                AccountStatus::Syncing,
                &[4, 0, 0, 0, 1, 0],
                &[("Projects", 1), ("Reports", 0)],
            ),
        ],
    }
}

fn account(
    key: &str,
    name: &str,
    status: AccountStatus,
    counts: &[u32; 6],
    more: &[(&str, u32)],
) -> SidebarAccount {
    let roles = [
        FolderRole::Inbox,
        FolderRole::Drafts,
        FolderRole::Sent,
        FolderRole::Archive,
        FolderRole::Junk,
        FolderRole::Trash,
    ];
    SidebarAccount {
        key: key.to_owned(),
        name: name.to_owned(),
        status,
        folders: roles
            .into_iter()
            .zip(counts)
            .map(|(role, &count)| SidebarFolder {
                key: format!("{key}/{}", role.label().to_lowercase()),
                name: role.label().to_owned(),
                role: Some(role),
                count,
            })
            .collect(),
        more: more
            .iter()
            .map(|&(folder, count)| SidebarFolder {
                key: format!("{key}/{}", folder.to_lowercase()),
                name: folder.to_owned(),
                role: None,
                count,
            })
            .collect(),
    }
}

struct Message {
    section: &'static str,
    from: &'static str,
    address: &'static str,
    time: &'static str,
    subject: &'static str,
    snippet: &'static str,
    unread: bool,
    starred: bool,
    attachment: bool,
    thread: &'static str,
}

const MESSAGES: &[Message] = &[
    Message {
        section: "Today",
        from: "Ingrid Solberg",
        address: "ingrid@fjordpost.example",
        time: "10:32",
        subject: "Cabin weekend — final plan",
        snippet: "Hi! I've booked the cabin in Hemsedal for the 17th–19th. It sleeps six, has a \
                  sauna and the road is cleared…",
        unread: true,
        starred: true,
        attachment: true,
        thread: "3",
    },
    Message {
        section: "",
        from: "Northwind Weekly",
        address: "news@northwind.example",
        time: "08:15",
        subject: "This week in design systems",
        snippet: "Tokens, typography and twelve small utilities that make a big difference — plus \
                  a reader question about dark mode.",
        unread: true,
        starred: false,
        attachment: false,
        thread: "",
    },
    Message {
        section: "",
        from: "Marius Dahl",
        address: "marius@stavtech.example",
        time: "07:50",
        subject: "Re: Quarterly report draft",
        snippet: "Looks good. Two small comments on page 4 — the churn chart uses last year's \
                  scale…",
        unread: false,
        starred: false,
        attachment: true,
        thread: "5",
    },
    Message {
        section: "Yesterday",
        from: "Fjord Cycling Club",
        address: "rides@fjordcycling.example",
        time: "18:44",
        subject: "Saturday ride: route and coffee stop",
        snippet: "We meet at 09:00 by the fountain. 62 km, mostly flat, coffee in Drøbak at the \
                  halfway point.",
        unread: false,
        starred: false,
        attachment: false,
        thread: "",
    },
    Message {
        section: "",
        from: "Leah Kim",
        address: "leah@studio-kim.example",
        time: "13:02",
        subject: "Invoice #2041",
        snippet: "Attached is the invoice for September. Payment terms are the usual 14 days — \
                  thanks again!",
        unread: true,
        starred: false,
        attachment: true,
        thread: "",
    },
    Message {
        section: "",
        from: "Aker Library",
        address: "noreply@akerlibrary.example",
        time: "09:30",
        subject: "Your reservation is ready",
        snippet: "'The Lighthouse Keeper's Atlas' is ready for pickup at the main desk until \
                  Friday.",
        unread: false,
        starred: false,
        attachment: false,
        thread: "",
    },
    Message {
        section: "This week",
        from: "Tomas Berg",
        address: "tomas.berg@example.net",
        time: "Mon",
        subject: "Photos from Saturday",
        snippet: "A few favourites — the rest are in the shared album. The one by the lighthouse \
                  is my pick.",
        unread: false,
        starred: false,
        attachment: true,
        thread: "2",
    },
    Message {
        section: "",
        from: "Sana Iqbal",
        address: "sana@example.org",
        time: "Mon",
        subject: "Lunch Thursday?",
        snippet: "Are you free at 12? There's a new ramen place on Grünerløkka I've been meaning \
                  to try.",
        unread: false,
        starred: true,
        attachment: false,
        thread: "",
    },
];

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    classed(
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
        classes,
    )
}

/// A stand-in for the message list: the search field, the filter chips and rows, nothing
/// behind them. The field takes no focus, so screenshots show it at rest; the real list
/// (ML-026) decides what has the focus first.
pub fn list() -> gtk::Widget {
    let search = classed(
        gtk::SearchEntry::builder()
            .placeholder_text("Search — from: has:attachment is:unread")
            .can_focus(false)
            .build(),
        &["sw-search"],
    );
    let chips = gtk::Box::builder().spacing(6).build();
    for (text, active) in [
        ("All", true),
        ("Unread", false),
        ("Starred", false),
        ("Attachments", false),
    ] {
        chips.append(&classed(
            gtk::ToggleButton::builder()
                .label(text)
                .active(active)
                .build(),
            &["sw-chip"],
        ));
    }
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_start(14)
        .margin_end(14)
        .margin_bottom(4)
        .build();
    header.append(&search);
    header.append(&chips);

    let rows = classed(
        gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build(),
        &["sw-message-list"],
    );
    let mut selected = None;
    for message in MESSAGES {
        if !message.section.is_empty() {
            rows.append(
                &gtk::ListBoxRow::builder()
                    .activatable(false)
                    .selectable(false)
                    .child(&label(message.section, &["sw-section-header"]))
                    .build(),
            );
        }
        let row = gtk::ListBoxRow::builder()
            .child(&message_row(message))
            .build();
        if message.from == "Marius Dahl" {
            selected = Some(row.clone());
        }
        rows.append(&row);
    }
    rows.select_row(selected.as_ref());
    let list = classed(
        gtk::Box::new(gtk::Orientation::Vertical, 0),
        &["sw-message-list"],
    );
    list.append(&header);
    list.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&rows)
            .build(),
    );
    list.upcast()
}

fn avatar(name: &str, address: &str, size: &str) -> gtk::Label {
    let avatar = classed(
        gtk::Label::builder()
            .label(initials(name, address))
            .valign(gtk::Align::Start)
            .build(),
        &["sw-avatar", &avatar_class(address)],
    );
    if !size.is_empty() {
        avatar.add_css_class(size);
    }
    avatar
}

fn message_row(message: &Message) -> gtk::Box {
    let row = classed(
        gtk::Box::new(gtk::Orientation::Horizontal, 0),
        &["sw-message-row"],
    );
    if message.unread {
        row.add_css_class("sw-unread");
    }
    let dot = classed(
        gtk::Box::builder()
            .valign(gtk::Align::Start)
            .margin_top(12)
            .build(),
        &["sw-unread-dot"],
    );
    dot.set_opacity(if message.unread { 1.0 } else { 0.0 });
    let top = gtk::Box::builder().spacing(8).build();
    let sender = label(message.from, &["sw-sender"]);
    sender.set_hexpand(true);
    top.append(&sender);
    if !message.thread.is_empty() {
        top.append(&classed(
            gtk::Label::new(Some(message.thread)),
            &["sw-thread-count"],
        ));
    }
    top.append(&label(message.time, &["sw-time"]));
    let snippet = label(message.snippet, &["sw-snippet"]);
    snippet.set_wrap(true);
    snippet.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    snippet.set_lines(2);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    text.append(&top);
    text.append(&label(message.subject, &["sw-subject"]));
    text.append(&snippet);
    let indicators = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(22)
        .width_request(16)
        .build();
    if message.starred {
        indicators.append(&classed(icon(&["starred-symbolic"]), &["sw-star"]));
    }
    if message.attachment {
        indicators.append(&classed(
            icon(&["mail-attachment-symbolic"]),
            &["sw-indicator"],
        ));
    }
    row.append(&dot);
    row.append(&avatar(message.from, message.address, ""));
    row.append(&text);
    row.append(&indicators);
    row
}

/// A stand-in for the reader: one fictional message on a card, with its attachment. The
/// reply buttons name their keys through the window.
pub fn reader(window: &Window) -> gtk::Widget {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .build();
    let subject = label("Re: Quarterly report draft", &["sw-subject-heading"]);
    subject.set_wrap(true);
    column.append(&subject);
    let tags = gtk::Box::builder().spacing(6).build();
    for tag in ["Inbox", "Work"] {
        tags.append(&classed(gtk::Label::new(Some(tag)), &["sw-tag"]));
    }
    column.append(&tags);

    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let from = gtk::Box::builder().spacing(6).build();
    from.append(&label("Marius Dahl", &["sw-strong", "sw-subtitle"]));
    from.append(&label("<marius@stavtech.example>", &["sw-address"]));
    names.append(&from);
    names.append(&label("to me", &["sw-secondary", "sw-small"]));
    let header = classed(
        gtk::Box::new(gtk::Orientation::Horizontal, 0),
        &["sw-message-header"],
    );
    header.append(&avatar(
        "Marius Dahl",
        "marius@stavtech.example",
        "sw-avatar-large",
    ));
    header.append(&names);
    header.append(&label("07:50 (3 h ago)", &["sw-time"]));
    let actions = gtk::Box::builder()
        .spacing(2)
        .valign(gtk::Align::Center)
        .build();
    for (icons, text, id) in [
        (
            &["mail-reply-sender-symbolic"][..],
            "Reply",
            ActionId::Reply,
        ),
        (
            &["mail-reply-all-symbolic"][..],
            "Reply all",
            ActionId::ReplyAll,
        ),
        (&["mail-forward-symbolic"][..], "Forward", ActionId::Forward),
    ] {
        let button = icon_button(icons, text);
        window.add_shortcut_tooltip(&button, text, id);
        actions.append(&button);
    }
    header.append(&actions);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    for paragraph in [
        "Looks good. Two small comments:",
        "Page 4: the churn chart uses last year's scale — can we align it with the other \
         charts?",
        "The summary could lead with the support numbers; they're the headline this quarter.",
        "Otherwise ready to send on Thursday.\nMarius",
    ] {
        let text = label(paragraph, &["sw-reading"]);
        text.set_wrap(true);
        text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.set_ellipsize(gtk::pango::EllipsizeMode::None);
        body.append(&text);
    }

    let file = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    file.append(&label("Q3-report-v2.pdf", &["sw-attachment-name"]));
    file.append(&label("1.4 MB · Open · Save", &["sw-attachment-meta"]));
    let attachment = classed(
        gtk::Box::builder().halign(gtk::Align::Start).build(),
        &["sw-attachment-card"],
    );
    attachment.append(&classed(
        gtk::Label::new(Some("PDF")),
        &["sw-attachment-type"],
    ));
    attachment.append(&file);

    let card = classed(
        gtk::Box::new(gtk::Orientation::Vertical, 0),
        &["sw-message-card"],
    );
    card.append(&header);
    card.append(&body);
    card.append(&attachment);
    column.append(&card);
    column.upcast()
}

/// A stand-in for the reader's actions: buttons with nothing behind them, whose tooltips name
/// the registry's keys.
pub fn reader_toolbar(window: &Window) -> gtk::Widget {
    let bar = gtk::Box::builder().spacing(2).build();
    let button = |icons: &[&str], text: &str, id: Option<ActionId>| {
        let button = icon_button(icons, text);
        if let Some(id) = id {
            window.add_shortcut_tooltip(&button, text, id);
        }
        button
    };
    for (icons, text, id) in [
        (
            &["mail-archive-symbolic", "folder-documents-symbolic"][..],
            "Archive",
            ActionId::Archive,
        ),
        (&["user-trash-symbolic"][..], "Delete", ActionId::Delete),
        (
            &["mail-mark-junk-symbolic"][..],
            "Move to Junk",
            ActionId::Junk,
        ),
        (&["folder-symbolic"][..], "Move to…", ActionId::MoveTo),
    ] {
        bar.append(&button(icons, text, Some(id)));
    }
    bar.append(&classed(
        gtk::Separator::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build(),
        &["sw-toolbar-separator"],
    ));
    bar.append(&button(
        &["mail-unread-symbolic"],
        "Mark as unread",
        Some(ActionId::MarkUnread),
    ));
    bar.append(&button(&["starred-symbolic"], "Star", Some(ActionId::Star)));
    bar.append(&button(
        &["view-more-horizontal-symbolic", "view-more-symbolic"],
        "More actions",
        None,
    ));
    let position = label("3 of 128", &["sw-secondary"]);
    position.set_hexpand(true);
    position.set_xalign(1.0);
    bar.append(&position);
    bar.append(&button(
        &["pan-start-symbolic"],
        "Previous message",
        Some(ActionId::PreviousMessage),
    ));
    bar.append(&button(
        &["pan-end-symbolic"],
        "Next message",
        Some(ActionId::NextMessage),
    ));
    bar.upcast()
}
