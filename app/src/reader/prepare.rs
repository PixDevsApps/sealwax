//! Turns a stored message into what the reader shows, off the GTK thread: the body (HTML with
//! its style, remote-image flag and inline parts, plain text with format=flowed joined, or why
//! it can't be shown), the attachment cards, and whether it is signed (ML-064).

use super::engine::{InlinePart, InlineParts, percent_decode};
use super::heuristic;
use super::protection::Unreadable;
use gtk4::glib;
use sealwax_domain::flowed::{self, Flowed};
use sealwax_domain::message;
use sealwax_domain::model::{Attachment, MessageDetail, MessageId};
use sealwax_domain::reading::{self, Encryption, InlinePgp, Signature};
use sealwax_domain::theme::BodyStyle;
use std::collections::HashSet;

/// A message references at most this many inline parts; more are left out.
pub const MAX_INLINE_PARTS: usize = 64;
/// And at most this many bytes of them.
pub const MAX_INLINE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum Body {
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
    /// Encrypted, or sealed inside an S/MIME signature: none of it is shown.
    Unreadable(Unreadable),
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub body: Body,
    /// Attachments to show as cards: everything but the inline parts the HTML shows, the
    /// signatures and PGP/MIME's version part.
    pub attachments: Vec<Attachment>,
    /// How the message is signed; the signature isn't checked.
    pub signed: Option<Signature>,
}

pub fn prepare(detail: &MessageDetail, raw: Option<&[u8]>) -> Prepared {
    let Some(raw) = raw else {
        return Prepared {
            body: if detail.html_parts.is_empty() && detail.text_parts.is_empty() {
                Body::Empty
            } else {
                Body::NotDownloaded
            },
            attachments: detail.attachments.clone(),
            signed: None,
        };
    };
    let inspection = reading::inspect(raw);
    let kept: Vec<Attachment> = detail
        .attachments
        .iter()
        .filter(|attachment| !inspection.hidden.contains(&attachment.part))
        .cloned()
        .collect();
    let unreadable = match (inspection.encrypted, inspection.signed) {
        (Some(encryption), _) => Some(Unreadable::Encrypted(encryption)),
        (None, Some(Signature::SmimeOpaque))
            if detail.html_parts.is_empty() && detail.text_parts.is_empty() =>
        {
            Some(Unreadable::SignedOpaque)
        }
        _ => None,
    };
    if let Some(unreadable) = unreadable {
        return Prepared {
            body: Body::Unreadable(unreadable),
            attachments: kept,
            signed: None,
        };
    }
    if !detail.html_parts.is_empty() {
        let html = join(raw, &detail.html_parts, "\n", &[]);
        if html_is_encrypted(&html) {
            return Prepared {
                body: Body::Unreadable(Unreadable::Encrypted(Encryption::InlinePgp)),
                attachments: kept,
                signed: None,
            };
        }
        let referenced = cid_references(&html);
        let parts = inline_parts(detail.summary.id, raw, &kept, &referenced);
        let scan = heuristic::scan(&html);
        let attachments = kept
            .into_iter()
            .filter(|attachment| !shown_inline(attachment, &referenced))
            .collect();
        return Prepared {
            body: Body::Html {
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
            signed: inspection.signed,
        };
    }
    if detail.text_parts.is_empty() {
        return Prepared {
            body: Body::Empty,
            attachments: kept,
            signed: inspection.signed,
        };
    }
    let text = join(raw, &detail.text_parts, "\n\n", &inspection.flowed);
    let (body, signed) = match reading::inline_pgp(&text) {
        Some(InlinePgp::Encrypted) => (
            Body::Unreadable(Unreadable::Encrypted(Encryption::InlinePgp)),
            None,
        ),
        Some(InlinePgp::Signed(text)) => (
            Body::Plain(text),
            inspection.signed.or(Some(Signature::InlinePgp)),
        ),
        None => (Body::Plain(text), inspection.signed),
    };
    Prepared {
        body,
        attachments: kept,
        signed,
    }
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
pub fn cid_references(html: &str) -> HashSet<String> {
    let lower = html.to_ascii_lowercase();
    lower
        .match_indices("cid:")
        .map(|(index, scheme)| {
            let rest = &html[index + scheme.len()..];
            let end = rest
                .find(|c: char| c.is_whitespace() || "\"'()<>,;\\".contains(c))
                .unwrap_or(rest.len());
            percent_decode(&rest[..end]).to_ascii_lowercase()
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
    use crate::reader::samples;
    use sealwax_domain::model::{BodyKind, Flags, FolderId, MessageId, MessageSummary, ThreadId};

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
        let prepared = prepare(&detail, Some(raw.as_bytes()));
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
        let prepared = prepare(&detail(&raw), Some(&raw));
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
        let prepared = prepare(&detail(&pgp_mime), Some(&pgp_mime));
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
        let prepared = prepare(&detail(&inline), Some(&inline));
        assert!(matches!(
            prepared.body,
            Body::Unreadable(Unreadable::Encrypted(Encryption::InlinePgp))
        ));
        let html = raw(
            "From: Ada <ada@example.com>\nSubject: x\nContent-Type: text/html\n\n\
            <pre>-----BEGIN PGP MESSAGE-----\n\nhQEMA0lv\n-----END PGP MESSAGE-----</pre>\n",
        );
        let prepared = prepare(&detail(&html), Some(&html));
        assert!(matches!(prepared.body, Body::Unreadable(_)));
        let smime = raw(
            "From: Ada <ada@example.com>\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=smime.p7m\n\
            Content-Transfer-Encoding: base64\n\nMIAGCSqGSIb3DQEHA6CAMIACAQAxggE=\n",
        );
        let prepared = prepare(&detail(&smime), Some(&smime));
        assert!(matches!(
            prepared.body,
            Body::Unreadable(Unreadable::Encrypted(Encryption::Smime))
        ));
        let opaque = raw(
            "From: Ada <ada@example.com>\nSubject: x\nMIME-Version: 1.0\n\
            Content-Type: application/pkcs7-mime; smime-type=signed-data; name=smime.p7m\n\
            Content-Transfer-Encoding: base64\n\nMIAGCSqGSIb3DQEHA6CAMIACAQAxggE=\n",
        );
        let prepared = prepare(&detail(&opaque), Some(&opaque));
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
        let prepared = prepare(&detail(&signed), Some(&signed));
        assert!(matches!(&prepared.body, Body::Plain(text) if text.trim() == "The minutes."));
        assert!(prepared.attachments.is_empty());
        assert_eq!(prepared.signed, Some(Signature::PgpMime));
        let clear = raw(
            "From: Ada <ada@example.com>\nSubject: x\nContent-Type: text/plain\n\n\
            -----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nMeeting moved.\n\
            -----BEGIN PGP SIGNATURE-----\n\niQEz\n-----END PGP SIGNATURE-----\n",
        );
        let prepared = prepare(&detail(&clear), Some(&clear));
        assert!(matches!(&prepared.body, Body::Plain(text) if text == "Meeting moved."));
        assert_eq!(prepared.signed, Some(Signature::InlinePgp));
    }

    #[test]
    fn html_with_an_inline_image_keeps_it_out_of_the_cards() {
        let raw = sample("cid");
        let prepared = prepare(&detail(&raw), Some(&raw));
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

    #[test]
    fn newsletters_go_on_paper_and_flag_remote_images() {
        let raw = sample("newsletter");
        let prepared = prepare(&detail(&raw), Some(&raw));
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

    #[test]
    fn plain_text_keeps_every_attachment() {
        let raw = sample("attachments");
        let detail = detail(&raw);
        assert_eq!(detail.body_kind, BodyKind::Plain);
        let prepared = prepare(&detail, Some(&raw));
        assert!(matches!(&prepared.body, Body::Plain(text) if text.contains("Attached is")));
        assert_eq!(prepared.attachments.len(), 3);
    }

    #[test]
    fn alternatives_show_the_html_and_latin1_is_decoded() {
        let raw = sample("simple");
        let prepared = prepare(&detail(&raw), Some(&raw));
        assert!(matches!(prepared.body, Body::Html { .. }));
        let raw = sample("latin1");
        let prepared = prepare(&detail(&raw), Some(&raw));
        assert!(matches!(&prepared.body, Body::Html { html, .. } if html.contains("Kåre")));
    }

    #[test]
    fn without_the_raw_message_nothing_is_parsed() {
        let raw = sample("plain");
        let prepared = prepare(&detail(&raw), None);
        assert!(matches!(prepared.body, Body::NotDownloaded));
        let empty = MessageDetail {
            text_parts: Vec::new(),
            html_parts: Vec::new(),
            ..detail(&raw)
        };
        assert!(matches!(prepare(&empty, None).body, Body::Empty));
    }
}
