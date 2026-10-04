//! Fictional placeholder content for the self-test's screenshots, taken from the approved design
//! mock (every name, address and message there is made up): two accounts for the sidebar, a
//! message list and a message. They stand in until the store (ML-023), the list (ML-026) and the
//! reader (ML-027) are connected.

use crate::window::panes::{classed, icon_button};
use crate::window::{AccountStatus, FolderRole, SidebarAccount, SidebarFolder, SidebarModel};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;

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

const MESSAGES: &[(&str, &str, &str, &str, &str, bool)] = &[
    (
        "Today",
        "Ingrid Solberg",
        "10:32",
        "Cabin weekend — final plan",
        "Hi! I've booked the cabin in Hemsedal for the 17th–19th. It sleeps six.",
        true,
    ),
    (
        "",
        "Northwind Weekly",
        "08:15",
        "This week in design systems",
        "Tokens, typography and twelve small utilities that make a big difference.",
        true,
    ),
    (
        "",
        "Marius Dahl",
        "07:50",
        "Re: Quarterly report draft",
        "Looks good. Two small comments on page 4 — the churn chart uses last year's scale.",
        false,
    ),
    (
        "Yesterday",
        "Fjord Cycling Club",
        "18:44",
        "Saturday ride: route and coffee stop",
        "We meet at 09:00 by the fountain. 62 km, mostly flat.",
        false,
    ),
    (
        "",
        "Leah Kim",
        "13:02",
        "Invoice #2041",
        "Attached is the invoice for September. Payment terms are the usual 14 days.",
        true,
    ),
    (
        "",
        "Aker Library",
        "09:30",
        "Your reservation is ready",
        "Ready for pickup at the main desk until Friday.",
        false,
    ),
];

/// A stand-in for the message list: a search field and rows, nothing behind them.
pub fn list() -> gtk::Widget {
    let rows = classed(
        gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build(),
        &["navigation-sidebar"],
    );
    let mut selected = None;
    for &(section, from, time, subject, snippet, unread) in MESSAGES {
        if !section.is_empty() {
            let header = gtk::ListBoxRow::builder()
                .activatable(false)
                .selectable(false)
                .child(
                    &gtk::Label::builder()
                        .label(section)
                        .xalign(0.0)
                        .margin_top(8)
                        .css_classes(["caption-heading", "dim-label"])
                        .build(),
                )
                .build();
            rows.append(&header);
        }
        let row = gtk::ListBoxRow::builder()
            .child(&message_row(from, time, subject, snippet, unread))
            .build();
        if from == "Marius Dahl" {
            selected = Some(row.clone());
        }
        rows.append(&row);
    }
    rows.select_row(selected.as_ref());
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search — from: has:attachment is:unread")
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(6)
        .build();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.append(&search);
    list.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&rows)
            .build(),
    );
    list.upcast()
}

fn message_row(from: &str, time: &str, subject: &str, snippet: &str, unread: bool) -> gtk::Box {
    let line = |text: &str, classes: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(classes.to_vec())
            .build()
    };
    let weight = if unread { "heading" } else { "body" };
    let top = gtk::Box::builder().spacing(8).build();
    let sender = line(from, &[weight]);
    sender.set_hexpand(true);
    top.append(&sender);
    top.append(&line(time, &["caption", "dim-label"]));
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    text.append(&top);
    text.append(&line(subject, &[weight]));
    text.append(&line(snippet, &["caption", "dim-label"]));
    let dot = gtk::Label::builder()
        .label(if unread { "●" } else { "" })
        .width_chars(1)
        .valign(gtk::Align::Start)
        .css_classes(["accent", "caption"])
        .build();
    let row = gtk::Box::builder()
        .spacing(10)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    row.append(&dot);
    row.append(&adw::Avatar::new(32, Some(from), true));
    row.append(&text);
    row
}

/// A stand-in for the reader: one fictional message.
pub fn reader() -> gtk::Widget {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .build();
    column.append(
        &gtk::Label::builder()
            .label("Re: Quarterly report draft")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-2"])
            .build(),
    );
    let sender = gtk::Box::builder().spacing(12).build();
    sender.append(&adw::Avatar::new(36, Some("Marius Dahl"), true));
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .build();
    for (text, classes) in [
        ("Marius Dahl", &["heading"][..]),
        ("to me, Leah · 07:50", &["caption", "dim-label"][..]),
    ] {
        names.append(
            &gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .css_classes(classes.to_vec())
                .build(),
        );
    }
    sender.append(&names);
    column.append(&sender);
    for paragraph in [
        "Looks good. Two small comments:",
        "Page 4: the churn chart uses last year's scale — can we align it with the other \
         charts?",
        "The summary could lead with the support numbers; they're the headline this quarter.",
        "Otherwise ready to send on Thursday.\nMarius",
    ] {
        column.append(
            &gtk::Label::builder()
                .label(paragraph)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .build(),
        );
    }
    column.upcast()
}

/// A stand-in for the reader's actions: buttons with nothing behind them.
pub fn reader_toolbar() -> gtk::Widget {
    let bar = gtk::Box::builder().spacing(2).build();
    for (icons, label) in [
        (
            &["mail-archive-symbolic", "folder-documents-symbolic"][..],
            "Archive",
        ),
        (&["user-trash-symbolic"][..], "Delete"),
        (&["mail-mark-junk-symbolic"][..], "Junk"),
        (&["folder-symbolic"][..], "Move to…"),
        (&["mail-unread-symbolic"][..], "Mark unread"),
        (&["starred-symbolic"][..], "Star"),
    ] {
        bar.append(&icon_button(icons, label));
    }
    let position = gtk::Label::builder()
        .label("3 of 128")
        .hexpand(true)
        .xalign(1.0)
        .css_classes(["dim-label", "numeric"])
        .build();
    bar.append(&position);
    bar.append(&icon_button(&["go-up-symbolic"], "Previous message"));
    bar.append(&icon_button(&["go-down-symbolic"], "Next message"));
    bar.upcast()
}
