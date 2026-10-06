//! Encrypted and signed mail (ML-064; SECURITY.md "Known limitations"): Sealwax 1.0 neither
//! decrypts nor verifies. An encrypted body is replaced by a notice and its ciphertext is never
//! shown; a signed message shows its content with a quiet "Signed · not checked" line; the
//! signature parts stay out of the attachment cards (`prepare`). The page places both widgets:
//! the notice where the body goes, the line between the sender block and the body.

use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::prelude::*;
use sealwax_domain::reading::{Encryption, Signature};

/// What the notice says in place of an encrypted body.
pub const ENCRYPTED: &str = "This message is encrypted. Sealwax can't read encrypted mail yet.";
/// In place of an S/MIME message whose content is inside its signature.
pub const SIGNED_OPAQUE: &str =
    "This message is signed in a way Sealwax can't open yet, so its text can't be shown.";
/// The quiet line on a signed message, and its tooltip.
pub const SIGNED: &str = "Signed · not checked";
pub const SIGNED_TOOLTIP: &str = "Sealwax can't check PGP or S/MIME signatures yet";

/// A body that can't be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    Encrypted(Encryption),
    /// S/MIME signed data with the content inside the signature.
    SignedOpaque,
}

/// The notice in place of an unreadable body.
pub fn notice(unreadable: Unreadable) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(10)
        .css_classes(["sw-reader-notice", "sw-reader-protected"])
        .build();
    row.set_widget_name("reader-protected");
    let image = icon(&[
        "channel-secure-symbolic",
        "security-high-symbolic",
        "dialog-password-symbolic",
    ]);
    image.set_valign(gtk::Align::Start);
    row.append(&image);
    let text = match unreadable {
        Unreadable::Encrypted(_) => ENCRYPTED,
        Unreadable::SignedOpaque => SIGNED_OPAQUE,
    };
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["sw-secondary"])
        .build();
    row.append(&label);
    row
}

/// The quiet line on a signed message.
pub fn signed_note(_signature: Signature) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(6)
        .tooltip_text(SIGNED_TOOLTIP)
        .css_classes(["sw-small", "sw-secondary", "sw-reader-signed"])
        .build();
    row.set_widget_name("reader-signed");
    row.append(&icon(&["emblem-ok-symbolic", "object-select-symbolic"]));
    row.append(&gtk::Label::new(Some(SIGNED)));
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{SIGNED}. {SIGNED_TOOLTIP}."
    ))]);
    row
}
