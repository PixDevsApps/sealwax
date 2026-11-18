//! Attached messages in the reader (#40): Open on a `message/rfc822` attachment shows the
//! message inside it as the reader shows any message (its HTML through the same hardened view,
//! remote images held back, its attachments with Open and Save), under a bar naming it with
//! Back. Back, or Back to List (Escape, `u`), goes up a level, and from the first level back to
//! the stored message. The bytes stay in memory ([`super::memory`]).

use super::files;
use super::memory::{self, is_attached};
use super::Inner;
use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::{gio, glib};
use gtk4::prelude::*;
use sealwax_domain::model::MessageId;
use std::rc::Rc;

/// What the bar says.
pub const ATTACHED: &str = "Attached message";

/// Whether an attachment of type `mime_type` opens in the reader.
pub(super) fn is_message(mime_type: &str) -> bool {
    matches!(
        mime_type.to_ascii_lowercase().as_str(),
        "message/rfc822" | "message/global"
    )
}

impl Inner {
    /// Shows `raw`, the attachment `name` of message `from`, in the reader; `false` when it
    /// doesn't read as a message.
    pub(super) async fn open_attached(
        self: &Rc<Self>,
        from: MessageId,
        raw: Vec<u8>,
        name: &str,
    ) -> bool {
        let read = gio::spawn_blocking(move || {
            sealwax_domain::message::is_message(&raw).then(|| (memory::detail(&raw), raw))
        })
        .await
        .ok()
        .flatten();
        let Some((detail, raw)) = read else {
            return false;
        };
        let Some(id) = self.memory.open(from, raw, detail, name) else {
            files::toast(self, "That message is nested too deep to open here.");
            return true;
        };
        tracing::debug!("reader: showing an attached message");
        self.show_id(id);
        true
    }

    /// Goes back from the attached message on screen; `false` when none is.
    pub(super) fn back_from_attached(self: &Rc<Self>) -> bool {
        let Some(shown) = self
            .shown
            .borrow()
            .as_ref()
            .map(|page| page.selected)
            .filter(|id| is_attached(*id))
        else {
            return false;
        };
        match self.memory.back(shown) {
            Some(to) => self.show_id(to),
            None => self.memory.close_attached(),
        }
        true
    }

    /// The bar above an attached message: what it is, and Back.
    pub(super) fn attached_bar(self: &Rc<Self>, id: MessageId, name: &str) -> gtk::Box {
        let root = gtk::Box::builder()
            .spacing(12)
            .css_classes(["sw-banner", "sw-reader-banner", "sw-reader-attached"])
            .build();
        root.set_widget_name("reader-attached-bar");
        root.update_property(&[gtk::accessible::Property::Label(ATTACHED)]);
        let image = icon(&["mail-attachment-symbolic", "mail-message-new-symbolic"]);
        image.add_css_class("sw-accent");
        root.append(&image);
        let text = gtk::Label::builder()
            .label(format!("{ATTACHED} · {name}"))
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .build();
        text.set_widget_name("reader-attached-name");
        root.append(&text);
        let back = gtk::Button::builder()
            .label("Back")
            .valign(gtk::Align::Center)
            .tooltip_text("Back to the message it's attached to")
            .css_classes(["sw-control"])
            .build();
        back.set_widget_name("reader-attached-back");
        let weak = Rc::downgrade(self);
        back.connect_clicked(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(inner) = weak.upgrade()
                    && inner.target.get() == Some(id)
                {
                    inner.back_from_attached();
                }
            });
        });
        root.append(&back);
        root
    }
}
