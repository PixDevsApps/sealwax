//! The GTK side of one message (DESIGN.md "Reader", the approved mock): the subject as a
//! heading, the remote-content banner, and a card with the sender block (avatar, name and
//! address, "to me" or the recipients, the date with relative time, expandable details), the
//! body and the attachment cards. Display only: Open, Save and Download All are M5.

use super::format;
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use sealwax_domain::model::{Attachment, MessageDetail};
use sealwax_domain::theme::{avatar_class, initials};

/// The widgets of one message that the reader changes after building it.
pub struct PageWidgets {
    pub root: gtk::Box,
    /// Where the body goes, inside the card.
    pub body: gtk::Box,
    pub banner: Option<Banner>,
}

pub struct Banner {
    pub root: gtk::Box,
    pub show_images: gtk::Button,
}

pub struct Header<'a> {
    pub detail: &'a MessageDetail,
    /// The user's own addresses, for "to me".
    pub mine: &'a [String],
    pub now: i64,
}

pub fn build(header: &Header<'_>, attachments: &[Attachment], remote_images: bool) -> PageWidgets {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .css_classes(["sealwax-reader-page"])
        .build();
    root.set_widget_name("reader-page");
    let subject = header.detail.headers.subject.trim();
    let heading = selectable(
        if subject.is_empty() {
            "(No subject)"
        } else {
            subject
        },
        &["sw-subject-heading"],
    );
    heading.set_widget_name("reader-subject");
    heading.set_wrap(true);
    heading.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    root.append(&heading);
    let banner = remote_images.then(banner);
    if let Some(banner) = &banner {
        root.append(&banner.root);
    }
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .css_classes(["sw-message-card", "sealwax-reader-card"])
        .build();
    card.append(&sender_block(header));
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sealwax-reader-body"])
        .build();
    body.set_widget_name("reader-body");
    card.append(&body);
    if !attachments.is_empty() {
        card.append(&attachment_cards(attachments));
    }
    root.append(&card);
    PageWidgets { root, body, banner }
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

fn sender_block(header: &Header<'_>) -> gtk::Box {
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
    let when = label(
        &format::date_line(date, header.now, &zone),
        &["sw-small", "sw-secondary", "numeric"],
    );
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
    let row = gtk::Box::builder()
        .spacing(12)
        .css_classes(["sw-message-header"])
        .build();
    row.append(&avatar);
    row.append(&names);
    row.append(&when);
    row.append(&toggle);
    let block = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    block.append(&row);
    block.append(&details);
    block
}

fn details_grid(detail: &MessageDetail, date: i64, zone: &glib::TimeZone) -> gtk::Grid {
    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(4)
        .margin_start(50)
        .margin_top(8)
        .css_classes(["sealwax-reader-details"])
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

/// "Images are hidden" with Show images; "Always for this sender" is M5.
fn banner() -> Banner {
    let root = gtk::Box::builder()
        .spacing(12)
        .css_classes(["sw-banner", "sealwax-reader-banner"])
        .build();
    root.set_widget_name("reader-banner");
    root.update_property(&[gtk::accessible::Property::Label("Remote images")]);
    let image = icon(&["image-missing-symbolic", "image-x-generic-symbolic"]);
    image.add_css_class("sw-accent");
    root.append(&image);
    let text = gtk::Label::builder()
        .label("Images are hidden so the sender can't tell you opened this.")
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    root.append(&text);
    let show_images = gtk::Button::builder()
        .label("Show images")
        .valign(gtk::Align::Center)
        .css_classes(["sw-control"])
        .build();
    show_images.set_widget_name("reader-show-images");
    root.append(&show_images);
    let always = gtk::Button::builder()
        .label("Always for this sender")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .tooltip_text("Not available yet")
        .css_classes(["sw-control"])
        .build();
    root.append(&always);
    Banner { root, show_images }
}

fn attachment_cards(attachments: &[Attachment]) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["sealwax-reader-attachments"])
        .build();
    section.set_widget_name("reader-attachments");
    section.append(&label(
        &format::attachment_count(attachments.len()),
        &["sw-secondary"],
    ));
    let cards = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .column_spacing(10)
        .row_spacing(10)
        .build();
    for attachment in attachments {
        let card = attachment_card(attachment);
        let child = gtk::FlowBoxChild::builder()
            .child(&card)
            .focusable(false)
            .build();
        cards.append(&child);
    }
    section.append(&cards);
    section
}

fn attachment_card(attachment: &Attachment) -> gtk::Box {
    let name = attachment
        .name
        .clone()
        .unwrap_or_else(|| "Unnamed attachment".to_owned());
    let kind = format::type_label(attachment.name.as_deref(), &attachment.mime_type);
    let size = format::size(attachment.size);
    let card = gtk::Box::builder()
        .spacing(10)
        .css_classes(["sw-attachment-card"])
        .build();
    card.set_widget_name("reader-attachment");
    card.update_property(&[gtk::accessible::Property::Label(&format!(
        "{name}, {kind}, {size}"
    ))]);
    let badge = gtk::Label::builder()
        .label(&kind)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["sw-attachment-type"])
        .build();
    card.append(&badge);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let title = label(&name, &["sw-attachment-name"]);
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    title.set_tooltip_text(Some(&name));
    text.append(&title);
    text.append(&label(&size, &["sw-attachment-meta"]));
    card.append(&text);
    card
}

/// A plain-text body in the reading font, selectable, wrapped, with no WebView.
pub fn plain_text(text: &str) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .accepts_tab(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .css_classes(["sw-reading", "sealwax-reader-plain"])
        .build();
    view.set_widget_name("reader-plain");
    view.update_property(&[gtk::accessible::Property::Label("Message text")]);
    view.buffer().set_text(text.trim_end());
    view
}

/// A short line in place of a body: no text, not downloaded yet, or not readable.
pub fn notice(text: &str) -> gtk::Label {
    let notice = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["sw-secondary", "sealwax-reader-notice"])
        .build();
    notice.set_widget_name("reader-notice");
    notice
}
