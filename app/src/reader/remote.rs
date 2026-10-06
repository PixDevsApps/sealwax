//! Remote content (ML-058; SECURITY.md "Message rendering", rule 9): the banner that says a
//! message's images are hidden, with Show images and Always for this sender. Nothing remote
//! loads until the user allows it for that message or for its sender.

use super::Reader;
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::prelude::*;
use sealwax_domain::model::MessageId;
use std::cell::RefCell;
use std::collections::HashSet;

/// Whose remote images load: the messages the user allowed this session (nothing is stored),
/// and, from ML-058, the senders the user always allows.
#[derive(Default)]
pub struct Policy {
    messages: RefCell<HashSet<MessageId>>,
}

impl Policy {
    /// Whether `message`, from `sender` (its From address, lower case), may load remote images.
    pub fn allows(&self, message: MessageId, _sender: &str) -> bool {
        self.messages.borrow().contains(&message)
    }

    /// The user allowed `message`'s remote images until Sealwax quits.
    pub fn allow_message(&self, message: MessageId) {
        self.messages.borrow_mut().insert(message);
    }
}

/// The remote-content banner of one message.
pub struct Banner {
    pub root: gtk::Box,
    pub show_images: gtk::Button,
}

/// "Images are hidden" with Show images; "Always for this sender" is M5.
pub fn banner() -> Banner {
    let root = gtk::Box::builder()
        .spacing(12)
        .css_classes(["sw-banner", "sw-reader-banner"])
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

/// Images from the shown message's sender load from now on, or are hidden again.
pub(super) fn set_always(_reader: &Reader, _on: bool) {}
