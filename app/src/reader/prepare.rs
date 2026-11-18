//! Turns a stored message into what the reader shows, off the GTK thread: the body (HTML with
//! its style, remote-image flag and inline parts, plain text with format=flowed joined, or why
//! it can't be shown), the attachment cards, and its signature (ML-064). With GnuPG (#32,
//! ADR-030) encrypted mail is decrypted and PGP signatures are checked; the decrypted content
//! stays in memory, for the card and its attachments, and is never stored.

use super::engine::{InlinePart, InlineParts};
use super::protection::{Signature, Unreadable};
use gtk4::glib;
use sealwax_domain::flowed::{self, Flowed};
use sealwax_domain::html_scan;
use sealwax_domain::message;
use sealwax_domain::model::{Attachment, MessageDetail, MessageId};
use sealwax_domain::pgp::{self, Decryption, Work};
use sealwax_domain::reading::{self, Encryption, InlinePgp, Inspection, SignatureKind};
use sealwax_infrastructure::pgp::Gpg;
use sealwax_ui_model::theme::BodyStyle;
use std::collections::HashSet;

/// A message references at most this many inline parts; more are left out.
pub(super) const MAX_INLINE_PARTS: usize = 64;
/// And at most this many bytes of them.
pub(super) const MAX_INLINE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) enum Body {
    Html {
        html: String,
        style: BodyStyle,
        remote_images: bool,
        parts: InlineParts,
    },
    /// Plain text, format=flowed paragraphs joined and inline PGP armour gone.
    Plain(String),
    /// The message has no body part.
    Empty,
    /// The store has only the headers; the body comes with sync (M3).
    NotDownloaded,
    /// Encrypted and not decrypted, or sealed inside an S/MIME signature: none of it is shown.
    Unreadable(Unreadable),
}

#[derive(Debug, Clone)]
pub(super) struct Prepared {
    pub body: Body,
    /// Attachments to show as cards: everything but the inline parts the HTML shows, the
    /// signatures and PGP/MIME's version part.
    pub attachments: Vec<Attachment>,
    pub signature: Option<Signature>,
    /// The message was encrypted and is shown decrypted.
    pub decrypted: bool,
    /// The decrypted MIME content the body and the attachment cards come from: their part
    /// numbers are its, not the stored message's.
    pub content: Option<Vec<u8>>,
}

impl Prepared {
    fn new(body: Body, attachments: Vec<Attachment>, signature: Option<Signature>) -> Prepared {
        Prepared {
            body,
            attachments,
            signature,
            decrypted: false,
            content: None,
        }
    }

    pub(super) fn empty() -> Prepared {
        Prepared::new(Body::Empty, Vec::new(), None)
    }
}

/// What the reader shows of `detail`, whose raw message is `raw`; `gpg` decrypts and checks
/// signatures, and without it encrypted mail shows a notice and signatures stay unchecked.
pub(super) fn prepare(detail: &MessageDetail, raw: Option<&[u8]>, gpg: Option<&Gpg>) -> Prepared {
    let Some(raw) = raw else {
        return Prepared::new(
            if detail.html_parts.is_empty() && detail.text_parts.is_empty() {
                Body::Empty
            } else {
                Body::NotDownloaded
            },
            detail.attachments.clone(),
            None,
        );
    };
    let inspection = reading::inspect(raw);
    if inspection.encrypted == Some(Encryption::PgpMime) {
        return decrypt(detail, raw, &inspection, gpg);
    }
    clear(detail, raw, &inspection, gpg)
}

/// A PGP/MIME message, decrypted and shown from its content.
fn decrypt(
    detail: &MessageDetail,
    raw: &[u8],
    inspection: &Inspection,
    gpg: Option<&Gpg>,
) -> Prepared {
    let kept = kept(detail, inspection);
    let unreadable =
        |why: Unreadable| Prepared::new(Body::Unreadable(why), kept.clone(), None);
    let Some(gpg) = gpg else {
        return unreadable(Unreadable::Encrypted(Encryption::PgpMime));
    };
    let Some(Work::Decrypt(ciphertext)) = pgp::work(raw) else {
        return unreadable(Unreadable::DecryptFailed);
    };
    let answer = match gpg.decrypt(&ciphertext) {
        Ok(answer) => answer,
        Err(error) => {
            tracing::warn!(%error, "reader: GnuPG couldn't decrypt a message");
            return unreadable(Unreadable::DecryptFailed);
        }
    };
    match answer.status.decryption {
        Some(Decryption::Okay) if !answer.output.is_empty() => {}
        Some(Decryption::NoSecretKey) => return unreadable(Unreadable::NoSecretKey),
        _ => return unreadable(Unreadable::DecryptFailed),
    }
    let content = answer.output;
    let parsed = message::parse(&content);
    let inner = MessageDetail {
        body_kind: parsed.body_kind,
        text_parts: parsed.text_parts,
        html_parts: parsed.html_parts,
        attachments: parsed.attachments,
        ..detail.clone()
    };
    let inner_inspection = reading::inspect(&content);
    let mut prepared = clear(&inner, &content, &inner_inspection, Some(gpg));
    if let Some(signature) = &answer.status.signature {
        prepared.signature = Some(Signature::Checked(verdict(gpg, signature, detail)));
    }
    prepared.decrypted = true;
    prepared.content = Some(content);
    prepared
}

/// The attachments `detail` lists, but signatures and PGP/MIME's version part.
fn kept(detail: &MessageDetail, inspection: &Inspection) -> Vec<Attachment> {
    detail
        .attachments
        .iter()
        .filter(|attachment| !inspection.hidden.contains(&attachment.part))
        .cloned()
        .collect()
}

/// The sender's address, for a signature's verdict.
fn sender(detail: &MessageDetail) -> Option<&str> {
    detail.headers.from.first().map(|from| from.email.as_str())
}

/// GnuPG's word on a signature, with the key's user IDs read for the sender check.
fn verdict(gpg: &Gpg, signature: &pgp::SignatureStatus, detail: &MessageDetail) -> pgp::Verdict {
    let user_ids = match (&signature.outcome, &signature.fingerprint) {
        (pgp::Outcome::Good, Some(fingerprint)) => gpg.user_ids(fingerprint).unwrap_or_else(|error| {
            tracing::warn!(%error, "reader: GnuPG couldn't list a signing key");
            Vec::new()
        }),
        _ => Vec::new(),
    };
    pgp::verdict(signature, &user_ids, sender(detail))
}

/// A PGP/MIME signature over the whole message, checked; `None` for other signatures.
fn check_mime(gpg: Option<&Gpg>, raw: &[u8], detail: &MessageDetail) -> Option<Signature> {
    let gpg = gpg?;
    let Some(Work::Verify { signed, signature }) = pgp::work(raw) else {
        return None;
    };
    Some(Signature::Checked(
        match gpg.verify_detached(&signed, &signature) {
            Ok(status) => match &status.signature {
                Some(signature) => verdict(gpg, signature, detail),
                None => pgp::Verdict::Error,
            },
            Err(error) => {
                tracing::warn!(%error, "reader: GnuPG couldn't check a signature");
                pgp::Verdict::Error
            }
        },
    ))
}

/// How a signature the reader doesn't check is shown.
fn unchecked(kind: SignatureKind, gpg: Option<&Gpg>) -> Signature {
    Signature::NotChecked {
        kind,
        gnupg: gpg.is_some(),
    }
}

/// A message that isn't PGP/MIME encrypted (or a decrypted message's content).
fn clear(
    detail: &MessageDetail,
    raw: &[u8],
    inspection: &Inspection,
    gpg: Option<&Gpg>,
) -> Prepared {
    let kept = kept(detail, inspection);
    let unreadable = match (inspection.encrypted, inspection.signed) {
        (Some(encryption), _) => Some(Unreadable::Encrypted(encryption)),
        (None, Some(SignatureKind::SmimeOpaque))
            if detail.html_parts.is_empty() && detail.text_parts.is_empty() =>
        {
            Some(Unreadable::SignedOpaque)
        }
        _ => None,
    };
    if let Some(unreadable) = unreadable {
        return Prepared::new(Body::Unreadable(unreadable), kept, None);
    }
    let signature = match inspection.signed {
        Some(SignatureKind::PgpMime) => {
            check_mime(gpg, raw, detail).or(Some(unchecked(SignatureKind::PgpMime, gpg)))
        }
        Some(kind) => Some(unchecked(kind, gpg)),
        None => None,
    };
    if !detail.html_parts.is_empty() {
        let html = join(raw, &detail.html_parts, "\n", &[]);
        if html_is_encrypted(&html) {
            return inline_encrypted(&message::html_to_text(&html), kept, gpg, detail);
        }
        let referenced = cid_references(&html);
        let parts = inline_parts(detail.summary.id, raw, &kept, &referenced);
        let scan = html_scan::scan(&html);
        let attachments = kept
            .into_iter()
            .filter(|attachment| !shown_inline(attachment, &referenced))
            .collect();
        return Prepared::new(
            Body::Html {
                style: if scan.own_colours {
                    BodyStyle::Paper
                } else {
                    BodyStyle::Themed
                },
                remote_images: scan.remote_images,
                parts,
                html,
            },
            attachments,
            signature,
        );
    }
    if detail.text_parts.is_empty() {
        return Prepared::new(Body::Empty, kept, signature);
    }
    let text = join(raw, &detail.text_parts, "\n\n", &inspection.flowed);
    match reading::inline_pgp(&text) {
        Some(InlinePgp::Encrypted) => inline_encrypted(&text, kept, gpg, detail),
        Some(InlinePgp::Signed(shown)) => {
            let signature = signature.or_else(|| Some(inline_signed(&text, gpg, detail)));
            Prepared::new(Body::Plain(shown), kept, signature)
        }
        None => Prepared::new(Body::Plain(text), kept, signature),
    }
}

/// A clear-signed text's signature, checked when GnuPG is there.
fn inline_signed(text: &str, gpg: Option<&Gpg>, detail: &MessageDetail) -> Signature {
    let (Some(gpg), Some(pgp::Inline::Signed(block))) = (gpg, pgp::inline(text)) else {
        return unchecked(SignatureKind::InlinePgp, gpg);
    };
    Signature::Checked(match gpg.verify_inline(block.as_bytes()) {
        Ok(status) => match &status.signature {
            Some(signature) => verdict(gpg, signature, detail),
            None => pgp::Verdict::Error,
        },
        Err(error) => {
            tracing::warn!(%error, "reader: GnuPG couldn't check a signature");
            pgp::Verdict::Error
        }
    })
}

/// A text with an inline PGP message: its decrypted text, or why it can't be shown.
fn inline_encrypted(
    text: &str,
    kept: Vec<Attachment>,
    gpg: Option<&Gpg>,
    detail: &MessageDetail,
) -> Prepared {
    let unreadable = |why: Unreadable, kept: Vec<Attachment>| {
        Prepared::new(Body::Unreadable(why), kept, None)
    };
    let (Some(gpg), Some(pgp::Inline::Encrypted(block))) = (gpg, pgp::inline(text)) else {
        return unreadable(Unreadable::Encrypted(Encryption::InlinePgp), kept);
    };
    let answer = match gpg.decrypt(block.as_bytes()) {
        Ok(answer) => answer,
        Err(error) => {
            tracing::warn!(%error, "reader: GnuPG couldn't decrypt a message");
            return unreadable(Unreadable::DecryptFailed, kept);
        }
    };
    match answer.status.decryption {
        Some(Decryption::Okay) => {}
        Some(Decryption::NoSecretKey) => return unreadable(Unreadable::NoSecretKey, kept),
        _ => return unreadable(Unreadable::DecryptFailed, kept),
    }
    let signature = answer
        .status
        .signature
        .as_ref()
        .map(|signature| Signature::Checked(verdict(gpg, signature, detail)));
    let mut prepared = Prepared::new(
        Body::Plain(String::from_utf8_lossy(&answer.output).into_owned()),
        kept,
        signature,
    );
    prepared.decrypted = true;
    prepared
}

/// The text of the body parts at `indexes`, in order, format=flowed parts joined.
fn join(raw: &[u8], indexes: &[u32], separator: &str, flowed: &[(u32, Flowed)]) -> String {
    indexes
        .iter()
        .filter_map(|index| {
            let text = message::part(raw, *index)?.text?;
            Some(match flowed.iter().find(|(part, _)| part == index) {
                Some((_, format)) => flowed::decode(&text, *format),
                None => text,
            })
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// An HTML body that carries an inline PGP message (some mailers wrap the armour in HTML).
fn html_is_encrypted(html: &str) -> bool {
    html.contains("-----BEGIN PGP MESSAGE-----")
        && reading::inline_pgp(&message::html_to_text(html)) == Some(InlinePgp::Encrypted)
}

/// Every Content-ID the HTML names in a `cid:` URL, decoded and in lower case.
pub(super) fn cid_references(html: &str) -> HashSet<String> {
    let lower = html.to_ascii_lowercase();
    lower
        .match_indices("cid:")
        .map(|(index, scheme)| {
            let rest = &html[index + scheme.len()..];
            let end = rest
                .find(|c: char| c.is_whitespace() || "\"'()<>,;\\".contains(c))
                .unwrap_or(rest.len());
            sealwax_domain::percent::decode(&rest[..end]).to_ascii_lowercase()
        })
        .filter(|id| !id.is_empty())
        .collect()
}

fn shown_inline(attachment: &Attachment, referenced: &HashSet<String>) -> bool {
    attachment
        .content_id
        .as_ref()
        .is_some_and(|id| referenced.contains(&id.to_ascii_lowercase()))
}

/// The parts the HTML references, decoded, within [`MAX_INLINE_PARTS`] and
/// [`MAX_INLINE_BYTES`]. A large message stored as its text parts has its images empty
/// (ML-044); the engine's part loader fetches them when the body asks.
fn inline_parts(
    message: MessageId,
    raw: &[u8],
    attachments: &[Attachment],
    referenced: &HashSet<String>,
) -> InlineParts {
    let mut parts = InlineParts::new(message);
    let mut bytes = 0;
    for attachment in attachments
        .iter()
        .filter(|attachment| shown_inline(attachment, referenced))
    {
        let Some(id) = &attachment.content_id else {
            continue;
        };
        if parts.len() >= MAX_INLINE_PARTS {
            tracing::debug!(cap = MAX_INLINE_PARTS, "reader: inline parts left out");
            break;
        }
        let Some(part) = message::part(raw, attachment.part) else {
            continue;
        };
        if part.data.is_empty() {
            parts.insert_missing(id.clone(), attachment.part, attachment.mime_type.clone());
            continue;
        }
        bytes += part.data.len();
        if bytes > MAX_INLINE_BYTES {
            tracing::debug!("reader: inline parts over the size cap left out");
            break;
        }
        parts.insert(
            id.clone(),
            InlinePart {
                mime_type: attachment.mime_type.clone(),
                data: glib::Bytes::from_owned(part.data),
            },
        );
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "test-hooks")]
    use crate::reader::samples;
    #[cfg(feature = "test-hooks")]
    use sealwax_domain::model::BodyKind;
    use sealwax_domain::model::{Flags, FolderId, MessageId, MessageSummary, ThreadId};

    fn detail(raw: &[u8]) -> MessageDetail {
        let parsed = message::parse(raw);
        MessageDetail {
            summary: MessageSummary {
                id: MessageId(1),
                folder: FolderId(1),
                thread: ThreadId(1),
                sender_name: String::new(),
                sender_email: String::new(),
                subject: parsed.headers.subject.clone(),
                snippet: parsed.snippet.clone(),
                date: 0,
                flags: Flags::NONE,
                has_attachments: parsed.has_attachments(),
                thread_count: 1,
                subscription: false,
                pinned: false,
            },
            headers: parsed.headers,
            body_kind: parsed.body_kind,
            text_parts: parsed.text_parts,
            html_parts: parsed.html_parts,
            attachments: parsed.attachments,
            keywords: Vec::new(),
            size: parsed.size,
            blob: None,
        }
    }

    #[cfg(feature = "test-hooks")]
    fn sample(name: &str) -> Vec<u8> {
        samples::all(1_791_115_200)
            .into_iter()
            .find(|sample| sample.name == name)
            .unwrap()
            .draft
            .render()
    }

    #[test]
    fn cid_references_are_decoded_and_case_folded() {
        let refs = cid_references(
            "<img src=\"cid:Logo@Example.test\"><img src='CID:image001.png%4001D9'>\
             <div style=\"background:url(cid:bg@x)\"></div><p>cid: in text</p>",
        );
        let mut refs: Vec<_> = refs.into_iter().collect();
        refs.sort();
        assert_eq!(refs, ["bg@x", "image001.png@01d9", "logo@example.test"]);
    }

    #[test]
    fn an_image_left_empty_by_a_text_only_download_goes_to_the_part_loader() {
        let raw = "Subject: x\r\nContent-Type: multipart/related; boundary=\"r\"\r\n\r\n\
                   --r\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:pic@x\">\r\n\
                   --r\r\nContent-Type: image/png\r\nContent-ID: <pic@x>\r\n\
                   Content-Transfer-Encoding: base64\r\n\r\n\r\n--r--\r\n";
        let detail = detail(raw.as_bytes());
        assert_eq!(detail.attachments.len(), 1, "the image is a part");
        let prepared = prepare(&detail, Some(raw.as_bytes()), None);
        let Body::Html { parts, .. } = prepared.body else {
            panic!("not HTML");
        };
        assert_eq!((parts.len(), parts.missing()), (1, 1));
        assert!(parts.ready("pic@x").is_none());
        assert!(
            prepared.attachments.is_empty(),
            "still no card: the HTML shows it"
        );
    }

    fn raw(text: &str) -> Vec<u8> {
        text.replace('\n', "\r\n").into_bytes()
    }

    #[test]
    fn flowed_text_is_joined() {
        let raw = raw(
            "From: Ada <ada@example.com>\nSubject: x\nMIME-Version: 1.0\n\
                       Content-Type: text/plain; charset=utf-8; format=flowed\n\n\
                       The cabin sleeps six, so there is \nroom for every\nfriend.\n\n> quoted \n> on two\n",
        );
        let prepared = prepare(&detail(&raw), Some(&raw), None);
        assert!(
            matches!(&prepared.body, Body::Plain(text)
                if text == "The cabin sleeps six, so there is room for every\nfriend.\n\n> quoted on two"),
            "{:?}",
            prepared.body
        );
    }

    #[test]
    fn encrypted_mail_never_shows_its_ciphertext() {
        let pgp_mime = raw(
            "From: Ada <ada@example.com>\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"\n\n\
            --e\nContent-Type: application/pgp-encrypted\n\nVersion: 1\n\n\
            --e\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\n\n\
            -----BEGIN PGP MESSAGE-----\n\nhQEMA0lv\n-----END PGP MESSAGE-----\n--e--\n",
        );
        let prepared = prepare(&detail(&pgp_mime), Some(&pgp_mime), None);
        assert!(matches!(
            prepared.body,
            Body::Unreadable(Unreadable::Encrypted(Encryption::PgpMime))
        ));
        let names: Vec<_> = prepared
            .attachments
            .iter()
            .map(|card| card.name.as_deref())
            .collect();
        assert_eq!(names, [Some("encrypted.asc")], "the version part is hidden");
        let inline = raw(
            "From: Ada <ada@example.com>\nSubject: x\nContent-Type: text/plain\n\n\
            Hi\n-----BEGIN PGP MESSAGE-----\n\nhQEMA0lv\n-----END PGP MESSAGE-----\n",
        );
        let prepared = prepare(&detail(&inline), Some(&inline), None);
        assert!(matches!(
            prepared.body,
            Body::Unreadable(Unreadable::Encrypted(Encryption::InlinePgp))
        ));
        let html = raw(
            "From: Ada <ada@example.com>\nSubject: x\nContent-Type: text/html\n\n\
            <pre>-----BEGIN PGP MESSAGE-----\n\nhQEMA0lv\n-----END PGP MESSAGE-----</pre>\n",
        );
        let prepared = prepare(&detail(&html), Some(&html), None);
        assert!(matches!(prepared.body, Body::Unreadable(_)));
        let smime = raw(
            "From: Ada <ada@example.com>\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=smime.p7m\n\
            Content-Transfer-Encoding: base64\n\nMIAGCSqGSIb3DQEHA6CAMIACAQAxggE=\n",
        );
        let prepared = prepare(&detail(&smime), Some(&smime), None);
        assert!(matches!(
            prepared.body,
            Body::Unreadable(Unreadable::Encrypted(Encryption::Smime))
        ));
        let opaque = raw(
            "From: Ada <ada@example.com>\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: application/pkcs7-mime; smime-type=signed-data; name=smime.p7m\n\
            Content-Transfer-Encoding: base64\n\nMIAGCSqGSIb3DQEHA6CAMIACAQAxggE=\n",
        );
        let prepared = prepare(&detail(&opaque), Some(&opaque), None);
        assert!(matches!(
            prepared.body,
            Body::Unreadable(Unreadable::SignedOpaque)
        ));
    }

    #[test]
    fn signed_mail_shows_its_text_without_the_signature_card() {
        let signed = raw(
            "From: Ada <ada@example.com>\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=\"s\"\n\n\
            --s\nContent-Type: text/plain\n\nThe minutes.\n\
            --s\nContent-Type: application/pgp-signature; name=\"signature.asc\"\n\nsig\n--s--\n",
        );
        let prepared = prepare(&detail(&signed), Some(&signed), None);
        assert!(matches!(&prepared.body, Body::Plain(text) if text.trim() == "The minutes."));
        assert!(prepared.attachments.is_empty());
        assert_eq!(
            prepared.signature,
            Some(Signature::NotChecked {
                kind: SignatureKind::PgpMime,
                gnupg: false
            })
        );
        let clear = raw(
            "From: Ada <ada@example.com>\nSubject: x\nContent-Type: text/plain\n\n\
            -----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nMeeting moved.\n\
            -----BEGIN PGP SIGNATURE-----\n\niQEz\n-----END PGP SIGNATURE-----\n",
        );
        let prepared = prepare(&detail(&clear), Some(&clear), None);
        assert!(matches!(&prepared.body, Body::Plain(text) if text == "Meeting moved."));
        assert_eq!(
            prepared.signature,
            Some(Signature::NotChecked {
                kind: SignatureKind::InlinePgp,
                gnupg: false
            })
        );
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn html_with_an_inline_image_keeps_it_out_of_the_cards() {
        let raw = sample("cid");
        let prepared = prepare(&detail(&raw), Some(&raw), None);
        let Body::Html {
            style,
            remote_images,
            parts,
            html,
        } = prepared.body
        else {
            panic!("not HTML");
        };
        assert_eq!(style, BodyStyle::Themed);
        assert!(!remote_images);
        assert_eq!(parts.len(), 1);
        let part = parts.ready("route@fjordcycling.example.test").unwrap();
        assert_eq!(part.mime_type, "image/png");
        assert!(part.data.starts_with(b"\x89PNG"));
        assert!(html.contains("Drøbak"));
        assert!(prepared.attachments.is_empty());
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn newsletters_go_on_paper_and_flag_remote_images() {
        let raw = sample("newsletter");
        let prepared = prepare(&detail(&raw), Some(&raw), None);
        let Body::Html {
            style,
            remote_images,
            ..
        } = prepared.body
        else {
            panic!("not HTML");
        };
        assert_eq!(style, BodyStyle::Paper);
        assert!(remote_images);
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn plain_text_keeps_every_attachment() {
        let raw = sample("attachments");
        let detail = detail(&raw);
        assert_eq!(detail.body_kind, BodyKind::Plain);
        let prepared = prepare(&detail, Some(&raw), None);
        assert!(matches!(&prepared.body, Body::Plain(text) if text.contains("Attached is")));
        assert_eq!(prepared.attachments.len(), 3);
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn alternatives_show_the_html_and_latin1_is_decoded() {
        let raw = sample("simple");
        let prepared = prepare(&detail(&raw), Some(&raw), None);
        assert!(matches!(prepared.body, Body::Html { .. }));
        let raw = sample("latin1");
        let prepared = prepare(&detail(&raw), Some(&raw), None);
        assert!(matches!(&prepared.body, Body::Html { html, .. } if html.contains("Kåre")));
    }

    #[cfg(feature = "test-hooks")]
    #[test]
    fn without_the_raw_message_nothing_is_parsed() {
        let raw = sample("plain");
        let prepared = prepare(&detail(&raw), None, None);
        assert!(matches!(prepared.body, Body::NotDownloaded));
        let empty = MessageDetail {
            text_parts: Vec::new(),
            html_parts: Vec::new(),
            ..detail(&raw)
        };
        assert!(matches!(prepare(&empty, None, None).body, Body::Empty));
    }

    /// A throwaway keyring with Ada's key (no passphrase), or `None` without GnuPG.
    fn keyring() -> Option<(std::path::PathBuf, Gpg)> {
        let program = std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|folder| folder.join("gpg"))
            .find(|path| path.is_file())?;
        let home = std::path::PathBuf::from(format!(
            "/tmp/sw-{}-{}",
            std::process::id(),
            glib::random_int()
        ));
        std::fs::create_dir_all(&home).ok()?;
        let gpg = |arguments: &[&str], input: &[u8]| -> Vec<u8> {
            use std::io::Write;
            let mut child = std::process::Command::new(&program)
                .env("GNUPGHOME", &home)
                .args(["--batch", "--yes", "--quiet", "--no-tty"])
                .args(arguments)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap_or_else(|error| panic!("gpg: {error}"));
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input);
            }
            child
                .wait_with_output()
                .map(|output| output.stdout)
                .unwrap_or_default()
        };
        gpg(
            &[
                "--passphrase",
                "",
                "--quick-gen-key",
                "Ada <ada@example.com>",
                "future-default",
                "default",
                "never",
            ],
            b"",
        );
        let reader = Gpg::new(program.clone(), Some(home.clone()), &home.join("scratch"));
        ENCRYPTED.with(|cell| {
            cell.replace(gpg(
                &[
                    "--local-user",
                    "ada@example.com",
                    "--recipient",
                    "ada@example.com",
                    "--armor",
                    "--sign",
                    "--encrypt",
                ],
                CONTENT.as_bytes(),
            ))
        });
        Some((home, reader))
    }

    thread_local! {
        static ENCRYPTED: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    const CONTENT: &str = "Content-Type: multipart/mixed; boundary=\"m\"\r\n\r\n\
        --m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe original, decrypted.\r\n\
        --m\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\n\
        Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
        Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--m--\r\n";

    #[test]
    fn encrypted_mail_is_decrypted_with_its_attachments_and_signature() {
        let Some((home, gpg)) = keyring() else {
            eprintln!("GnuPG isn't installed: skipped");
            return;
        };
        let ciphertext = ENCRYPTED.with(|cell| cell.borrow().clone());
        let mut raw = b"From: Ada <ada@example.com>\r\nSubject: Secret\r\nMIME-Version: 1.0\r\n\
            Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"e\"\r\n\r\n\
            --e\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\r\n\
            --e\r\nContent-Type: application/octet-stream\r\n\r\n"
            .to_vec();
        raw.extend_from_slice(&ciphertext);
        raw.extend_from_slice(b"\r\n--e--\r\n");
        let prepared = prepare(&detail(&raw), Some(&raw), Some(&gpg));
        let _ = std::process::Command::new("gpgconf")
            .env("GNUPGHOME", &home)
            .args(["--kill", "all"])
            .status();
        let _ = std::fs::remove_dir_all(&home);
        assert!(
            matches!(&prepared.body, Body::Plain(text) if text.trim() == "The original, decrypted."),
            "{:?}",
            prepared.body
        );
        assert!(prepared.decrypted);
        let names: Vec<_> = prepared
            .attachments
            .iter()
            .map(|card| card.name.as_deref())
            .collect();
        assert_eq!(names, [Some("report.pdf")]);
        let content = prepared.content.unwrap_or_default();
        let part = prepared.attachments[0].part;
        assert_eq!(
            message::part(&content, part).map(|part| part.data),
            Some(b"%PDF-1.4\n".to_vec()),
            "the card's part is read from the decrypted content"
        );
        assert!(matches!(
            prepared.signature,
            Some(Signature::Checked(pgp::Verdict::Checked { .. }))
        ));
        let without = prepare(&detail(&raw), Some(&raw), None);
        assert!(matches!(
            without.body,
            Body::Unreadable(Unreadable::Encrypted(Encryption::PgpMime))
        ));
    }
}
