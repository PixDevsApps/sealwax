//! Encrypted and signed mail (ML-064; #32, ADR-030; SECURITY.md "PGP"). With GnuPG, PGP mail
//! is decrypted through the user's keyring and gpg-agent and PGP signatures are checked; the
//! quiet line above the body says what was found, and says "checked" only when the signature
//! is good, the keyring vouches for the key and the key is the sender's. A body that can't be
//! shown is replaced by a notice and its ciphertext is never shown; the signature parts stay out
//! of the attachment cards (`prepare`). S/MIME is neither decrypted nor checked.

use crate::window::panes::icon;
use gtk4 as gtk;
use gtk4::prelude::*;
use sealwax_domain::pgp::Verdict;
use sealwax_domain::reading::{Encryption, SignatureKind};

/// How every notice in place of an encrypted body starts.
pub const ENCRYPTED: &str = "This message is encrypted.";
/// In place of an S/MIME message whose content is inside its signature.
pub(super) const SIGNED_OPAQUE: &str =
    "This message is signed in a way Sealwax can't open yet, so its text can't be shown.";
/// The quiet line on a signed message that isn't checked.
pub const SIGNED: &str = "Signed · not checked";

/// A body that can't be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// Not decrypted: S/MIME, or PGP without GnuPG.
    Encrypted(Encryption),
    /// None of the secret keys in the user's keyring is one it was encrypted to.
    NoSecretKey,
    /// GnuPG tried and failed: cancelled, a wrong passphrase, damaged data.
    DecryptFailed,
    /// S/MIME signed data with the content inside the signature.
    SignedOpaque,
}

impl Unreadable {
    /// Trying again may work: the passphrase was cancelled, or the agent wasn't reachable.
    pub(super) fn retry(self) -> bool {
        self == Unreadable::DecryptFailed
    }
}

/// A message's signature, as far as the reader can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Signature {
    /// S/MIME, or PGP without GnuPG (`gnupg` is false).
    NotChecked { kind: SignatureKind, gnupg: bool },
    Checked(Verdict),
}

/// What the notice says (printing uses it too).
pub(super) fn text(unreadable: Unreadable) -> String {
    match unreadable {
        Unreadable::Encrypted(Encryption::Smime) => {
            format!("{ENCRYPTED} Sealwax can't read S/MIME encrypted mail yet.")
        }
        Unreadable::Encrypted(_) if sealwax_infrastructure::sandbox::is_flatpak() => format!(
            "{ENCRYPTED} In the Flatpak, Sealwax can't reach your GnuPG keyring to decrypt it; \
             the Arch package can."
        ),
        Unreadable::Encrypted(_) => {
            format!("{ENCRYPTED} Install GnuPG to read it: Sealwax decrypts with your keyring.")
        }
        Unreadable::NoSecretKey => format!(
            "{ENCRYPTED} None of the secret keys in your GnuPG keyring can open it."
        ),
        Unreadable::DecryptFailed => format!(
            "{ENCRYPTED} GnuPG couldn't decrypt it: the passphrase was cancelled, or the \
             message is damaged."
        ),
        Unreadable::SignedOpaque => SIGNED_OPAQUE.to_owned(),
    }
}

/// The notice in place of an unreadable body.
pub(super) fn notice(unreadable: Unreadable) -> gtk::Box {
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
    let label = gtk::Label::builder()
        .label(text(unreadable))
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["sw-secondary"])
        .build();
    row.append(&label);
    row
}

/// The line, its tooltip, and whether it warns.
fn signature_text(signature: &Signature) -> (String, String, bool) {
    let keyring = "Check the key's fingerprint with the sender, then certify it in GnuPG.";
    match signature {
        Signature::NotChecked {
            kind: SignatureKind::Smime | SignatureKind::SmimeOpaque,
            ..
        } => (
            SIGNED.to_owned(),
            "Sealwax can't check S/MIME signatures yet".to_owned(),
            false,
        ),
        Signature::NotChecked { gnupg: false, .. } => (
            SIGNED.to_owned(),
            if sealwax_infrastructure::sandbox::is_flatpak() {
                "In the Flatpak, Sealwax can't reach your GnuPG keyring".to_owned()
            } else {
                "Install GnuPG to check PGP signatures".to_owned()
            },
            false,
        ),
        Signature::NotChecked { gnupg: true, .. } => (
            SIGNED.to_owned(),
            "Only part of this message is signed, so the signature isn't checked".to_owned(),
            false,
        ),
        Signature::Checked(verdict) => match verdict {
            Verdict::Checked { signer } => (
                format!("Signed by {signer} · checked"),
                "The signature is good, and your GnuPG keyring vouches for the sender's key"
                    .to_owned(),
                false,
            ),
            Verdict::KeyNotVerified { signer } => (
                format!("Signed by {signer} · key not verified"),
                format!(
                    "The signature is good, but your GnuPG keyring doesn't vouch for the key. \
                     {keyring}"
                ),
                false,
            ),
            Verdict::OtherSigner { signer } => (
                format!("Signed by {signer}, not by the sender"),
                "The signature is good, but the key isn't the sender's address".to_owned(),
                true,
            ),
            Verdict::KeyDistrusted { signer } => (
                format!("Signed by {signer} · a key you don't trust"),
                "Your GnuPG keyring says not to trust this key".to_owned(),
                true,
            ),
            Verdict::Bad => (
                "The signature doesn't match: the message may have been changed".to_owned(),
                "GnuPG says the content isn't what was signed".to_owned(),
                true,
            ),
            Verdict::Expired { signer } => (
                format!("Signed by {signer} · expired"),
                "The key or the signature has expired".to_owned(),
                false,
            ),
            Verdict::Revoked { signer } => (
                format!("Signed by {signer} · the key is revoked"),
                "The key's owner has revoked it: don't rely on this signature".to_owned(),
                true,
            ),
            Verdict::MissingKey { key } => (
                "Signed · the key isn't in your keyring".to_owned(),
                format!("Import key {key} into GnuPG to check the signature"),
                false,
            ),
            Verdict::Error => (
                "Signed · couldn't be checked".to_owned(),
                "GnuPG couldn't check this signature".to_owned(),
                false,
            ),
        },
    }
}

/// The quiet line on a signed message.
pub(super) fn signature_note(signature: &Signature) -> gtk::Box {
    let (text, tooltip, warns) = signature_text(signature);
    let row = gtk::Box::builder()
        .spacing(6)
        .tooltip_text(&tooltip)
        .css_classes(["sw-small", "sw-secondary", "sw-reader-signed"])
        .build();
    if warns {
        row.add_css_class("sw-alert");
    }
    row.set_widget_name("reader-signed");
    row.append(&icon(if warns {
        &["dialog-warning-symbolic"]
    } else {
        &["emblem-ok-symbolic", "object-select-symbolic"]
    }));
    let label = gtk::Label::builder()
        .label(&text)
        .xalign(0.0)
        .wrap(true)
        .build();
    row.append(&label);
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{text}. {tooltip}."
    ))]);
    row
}

/// The line on a message shown decrypted.
pub const DECRYPTED: &str = "Encrypted · decrypted with your key";

pub(super) fn decrypted_note() -> gtk::Box {
    let tooltip = "The decrypted text stays in memory: it isn't saved, searched or quoted in \
                   replies";
    let row = gtk::Box::builder()
        .spacing(6)
        .tooltip_text(tooltip)
        .css_classes(["sw-small", "sw-secondary", "sw-reader-signed"])
        .build();
    row.set_widget_name("reader-decrypted");
    row.append(&icon(&[
        "channel-secure-symbolic",
        "security-high-symbolic",
        "dialog-password-symbolic",
    ]));
    row.append(&gtk::Label::new(Some(DECRYPTED)));
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{DECRYPTED}. {tooltip}."
    ))]);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_checked_signature_says_checked() {
        let checked = |verdict: Verdict| signature_text(&Signature::Checked(verdict)).0;
        let signer = || "Ada <ada@example.com>".to_owned();
        assert!(checked(Verdict::Checked { signer: signer() }).ends_with("· checked"));
        for verdict in [
            Verdict::KeyNotVerified { signer: signer() },
            Verdict::OtherSigner { signer: signer() },
            Verdict::KeyDistrusted { signer: signer() },
            Verdict::Bad,
            Verdict::Expired { signer: signer() },
            Verdict::Revoked { signer: signer() },
            Verdict::MissingKey { key: "AB".into() },
            Verdict::Error,
        ] {
            assert!(!checked(verdict).ends_with("· checked"));
        }
        assert!(
            signature_text(&Signature::Checked(Verdict::Bad)).2,
            "a bad signature warns"
        );
    }

    #[test]
    fn every_encrypted_notice_starts_alike() {
        for unreadable in [
            Unreadable::Encrypted(Encryption::PgpMime),
            Unreadable::Encrypted(Encryption::Smime),
            Unreadable::NoSecretKey,
            Unreadable::DecryptFailed,
        ] {
            assert!(text(unreadable).starts_with(ENCRYPTED));
        }
    }
}
