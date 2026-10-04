//! Turns a stored message into what the reader shows, off the GTK thread: the body (HTML with
//! its style, remote-image flag and inline parts, or plain text) and the attachment cards.

use super::engine::{InlinePart, InlineParts, percent_decode};
use super::heuristic;
use gtk4::glib;
use sealwax_domain::message;
use sealwax_domain::model::{Attachment, MessageDetail};
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
    Plain(String),
    /// The message has no body part.
    Empty,
    /// The store has only the headers; the body comes with sync (M3).
    NotDownloaded,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub body: Body,
    /// Attachments to show as cards: everything but the inline parts the HTML shows.
    pub attachments: Vec<Attachment>,
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
        };
    };
    if !detail.html_parts.is_empty() {
        let html = join(raw, &detail.html_parts, "\n");
        let referenced = cid_references(&html);
        let parts = inline_parts(raw, &detail.attachments, &referenced);
        let scan = heuristic::scan(&html);
        let attachments = detail
            .attachments
            .iter()
            .filter(|attachment| !shown_inline(attachment, &referenced))
            .cloned()
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
        };
    }
    let body = if detail.text_parts.is_empty() {
        Body::Empty
    } else {
        Body::Plain(join(raw, &detail.text_parts, "\n\n"))
    };
    Prepared {
        body,
        attachments: detail.attachments.clone(),
    }
}

/// The text of the body parts at `indexes`, in order.
fn join(raw: &[u8], indexes: &[u32], separator: &str) -> String {
    indexes
        .iter()
        .filter_map(|index| message::part(raw, *index))
        .filter_map(|part| part.text)
        .collect::<Vec<_>>()
        .join(separator)
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
/// [`MAX_INLINE_BYTES`].
fn inline_parts(
    raw: &[u8],
    attachments: &[Attachment],
    referenced: &HashSet<String>,
) -> InlineParts {
    let mut parts = InlineParts::default();
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
        let part = parts.find("route@fjordcycling.example.test").unwrap();
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
