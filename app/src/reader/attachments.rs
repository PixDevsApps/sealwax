//! Attachment cards (ML-061; DESIGN.md "Reader"): type, name and size under "N attachments".
//! Display only until M5 adds Open, Save and Download All.

use super::format;
use gtk4 as gtk;
use gtk4::prelude::*;
use sealwax_domain::model::Attachment;

pub fn attachment_cards(attachments: &[Attachment]) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["sw-reader-attachments"])
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

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(classes.to_vec())
        .build()
}
