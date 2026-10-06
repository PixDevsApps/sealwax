//! The GTK side of one expanded message (DESIGN.md "Reader", the approved mock): a card with the
//! sender block (avatar, name and address, "to me" or the recipients, the date with relative
//! time, expandable details), its own remote-content banner, the body and the attachment cards.
//! The sender block is a button that collapses the card. The banner is [`super::remote`]'s, the
//! cards [`super::attachments`]'s and a plain-text body [`super::plain`]'s (M5's owners); each
//! card calls them in one place, [`card`].

use super::attachments::attachment_cards;
use super::format;
use super::remote::{Banner, banner};
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::{Attachment, MessageDetail};
use sealwax_domain::theme::{avatar_class, initials};

/// The widgets of one message's card that the reader changes after building it.
pub struct CardWidgets {
    pub root: gtk::Box,
    /// The sender block: a click, Enter or Space collapses the card.
    pub header: gtk::Button,
    /// Where the body goes.
    pub body: gtk::Box,
    pub banner: Option<Banner>,
    /// What takes the focus when the reader keeps it across messages: the body once it has
    /// one, else the details button.
    pub focus: Option<gtk::Widget>,
}

pub struct Header<'a> {
    pub detail: &'a MessageDetail,
    /// The user's own addresses, for "to me".
    pub mine: &'a [String],
    pub now: i64,
}

pub fn card(header: &Header<'_>, attachments: &[Attachment], remote_images: bool) -> CardWidgets {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .css_classes(["sw-message-card", "sw-reader-card"])
        .build();
    root.set_widget_name("reader-card");
    let (sender, header_button, details) = sender_block(header);
    root.append(&sender);
    let banner = remote_images.then(banner);
    if let Some(banner) = &banner {
        root.append(&banner.root);
    }
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sw-reader-body"])
        .build();
    body.set_widget_name("reader-body");
    root.append(&body);
    if !attachments.is_empty() {
        root.append(&attachment_cards(attachments));
    }
    CardWidgets {
        root,
        header: header_button,
        body,
        banner,
        focus: Some(details.upcast()),
    }
}

fn selectable(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .selectable(true)
        .css_classes(classes.to_vec())
        .build()
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(classes.to_vec())
        .build()
}

/// The sender block, the button it makes (avatar, names and date), and its details button.
fn sender_block(header: &Header<'_>) -> (gtk::Box, gtk::Button, gtk::ToggleButton) {
    let headers = &header.detail.headers;
    let sender = headers.from.first().cloned().unwrap_or_default();
    let name = if sender.name.trim().is_empty() {
        sender.email.clone()
    } else {
        sender.name.clone()
    };
    let avatar = gtk::Label::builder()
        .label(initials(&sender.name, &sender.email))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes([
            "sw-avatar".to_owned(),
            "sw-avatar-large".to_owned(),
            avatar_class(&sender.email),
        ])
        .build();
    avatar.set_widget_name("reader-avatar");
    let name_label = label(&name, &["sw-strong", "sw-subtitle"]);
    name_label.set_widget_name("reader-sender");
    let first_line = gtk::Box::builder().spacing(6).build();
    first_line.append(&name_label);
    if !sender.email.is_empty() && sender.email != name {
        let address = label(&format!("<{}>", sender.email), &["sw-address"]);
        address.set_widget_name("reader-address");
        first_line.append(&address);
    }
    let recipients = label(
        &format::recipients_line(&headers.to, &headers.cc, header.mine),
        &["sw-small", "sw-secondary"],
    );
    recipients.set_widget_name("reader-recipients");
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    names.append(&first_line);
    names.append(&recipients);
    let date = header
        .detail
        .headers
        .date
        .unwrap_or(header.detail.summary.date);
    let zone = glib::TimeZone::local();
    let date_text = format::date_line(date, header.now, &zone);
    let when = label(&date_text, &["sw-small", "sw-secondary", "numeric"]);
    when.set_widget_name("reader-date");
    when.set_valign(gtk::Align::Center);
    when.set_tooltip_text(Some(&format::full_date(date, &zone)));
    let details = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&details_grid(header.detail, date, &zone))
        .build();
    let toggle = gtk::ToggleButton::builder()
        .child(&icon(&["pan-down-symbolic"]))
        .tooltip_text("Details")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "sw-icon-button"])
        .build();
    toggle.set_widget_name("reader-details-toggle");
    toggle.update_property(&[gtk::accessible::Property::Label("Details")]);
    toggle
        .bind_property("active", &details, "reveal-child")
        .sync_create()
        .build();
    let who = gtk::Box::builder().spacing(12).build();
    who.append(&avatar);
    who.append(&names);
    who.append(&when);
    let button = gtk::Button::builder()
        .child(&who)
        .hexpand(true)
        .css_classes(["flat", "sw-reader-card-header"])
        .build();
    button.set_widget_name("reader-card-header");
    button.update_property(&[gtk::accessible::Property::Label(&format!(
        "{name}, {date_text}, expanded"
    ))]);
    button.update_state(&[gtk::accessible::State::Expanded(Some(true))]);
    let row = gtk::Box::builder()
        .spacing(12)
        .css_classes(["sw-message-header"])
        .build();
    row.append(&button);
    row.append(&toggle);
    let block = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    block.append(&row);
    block.append(&details);
    (block, button, toggle)
}

fn details_grid(detail: &MessageDetail, date: i64, zone: &glib::TimeZone) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(4)
        .margin_start(50)
        .margin_top(8)
        .css_classes(["sw-reader-details"])
        .build();
    grid.set_widget_name("reader-details");
    let headers = &detail.headers;
    let rows = [
        ("From", format::address_list(&headers.from)),
        ("Reply to", format::address_list(&headers.reply_to)),
        ("To", format::address_list(&headers.to)),
        ("Cc", format::address_list(&headers.cc)),
        ("Date", format::full_date(date, zone)),
        ("List", headers.list_id.clone().unwrap_or_default()),
    ];
    for (row, (title, value)) in rows
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .enumerate()
    {
        let row = row as i32;
        grid.attach(&label(title, &["sw-small", "sw-secondary"]), 0, row, 1, 1);
        let value = selectable(&value, &["sw-small"]);
        value.set_wrap(true);
        value.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        value.set_hexpand(true);
        grid.attach(&value, 1, row, 1, 1);
    }
    grid
}

/// A short line in place of a body: no text, not downloaded yet, or not readable.
pub fn notice(text: &str) -> gtk::Label {
    let notice = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["sw-secondary", "sw-reader-notice"])
        .build();
    notice.set_widget_name("reader-notice");
    notice
}
