//! The header FETCH of the first sync and the store rows it becomes: flags, INTERNALDATE,
//! RFC822.SIZE, the header fields a list row and threading need, the body structure for the
//! attachment clip, and the start of the text for the snippet.

use async_imap::imap_proto::{AttributeValue, BodyStructure, MessageSection, SectionPath};
use mail_parser::{MessageParser, MimeHeaders};
use sealwax_domain::message;
use sealwax_domain::model::{BodyKind, Flags, FolderId};
use sealwax_infrastructure::store::{BodyState, NewMessage};

/// Bytes of the body's start fetched for the snippet.
pub const PREVIEW_BYTES: u32 = 1024;

/// The header fields mail-parser reads into `Headers`, plus what decoding the preview needs.
const FIELDS: &str = "DATE FROM SENDER TO CC REPLY-TO SUBJECT MESSAGE-ID IN-REPLY-TO \
    REFERENCES THREAD-INDEX LIST-ID MIME-VERSION CONTENT-TYPE CONTENT-TRANSFER-ENCODING";

/// The FETCH items of the first sync.
pub(crate) fn items() -> String {
    format!(
        "(UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS ({FIELDS})] \
         BODY.PEEK[TEXT]<0.{PREVIEW_BYTES}>)"
    )
}

/// What the body structure tells the list row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Structure {
    pub plain: bool,
    pub html: bool,
    /// Any part the list shows the clip for: not a body text and not an image the HTML embeds.
    pub attachments: bool,
}

/// One message's FETCH data, owned, so parsing can leave the connection's task.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fetched {
    pub uid: u32,
    pub flags: Vec<String>,
    pub internal_date: Option<i64>,
    pub size: Option<u32>,
    pub header: Vec<u8>,
    pub preview: Vec<u8>,
    pub structure: Structure,
}

/// The FETCH data of one message; `None` without a UID or the header section, as for the
/// unsolicited flag updates a server may send meanwhile (hints only).
pub(crate) fn fetched(attributes: &[AttributeValue<'_>]) -> Option<Fetched> {
    let mut out = Fetched::default();
    let mut uid = None;
    let mut header = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(found) => uid = Some(*found),
            AttributeValue::Flags(flags) => {
                out.flags = flags.iter().map(|flag| flag.to_string()).collect();
            }
            AttributeValue::InternalDate(date) => out.internal_date = internal_date(date),
            AttributeValue::Rfc822Size(size) => out.size = Some(*size),
            AttributeValue::BodyStructure(body) => structure(body, &mut out.structure),
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                data,
                ..
            } => header = Some(data.as_deref().unwrap_or_default().to_vec()),
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Text)),
                data,
                ..
            } => out.preview = data.as_deref().unwrap_or_default().to_vec(),
            _ => {}
        }
    }
    out.uid = uid?;
    out.header = header?;
    Some(out)
}

fn is_attachment(common: &async_imap::imap_proto::BodyContentCommon<'_>) -> bool {
    common
        .disposition
        .as_ref()
        .is_some_and(|disposition| disposition.ty.eq_ignore_ascii_case("attachment"))
}

fn structure(body: &BodyStructure<'_>, out: &mut Structure) {
    match body {
        BodyStructure::Multipart { bodies, .. } => {
            for part in bodies {
                structure(part, out);
            }
        }
        BodyStructure::Text { common, .. } if !is_attachment(common) => {
            match common.ty.subtype.to_ascii_lowercase().as_str() {
                "plain" => out.plain = true,
                "html" => out.html = true,
                _ => out.attachments = true,
            }
        }
        BodyStructure::Basic { common, other, .. } => {
            let embedded = common.ty.ty.eq_ignore_ascii_case("image")
                && other.id.is_some()
                && !is_attachment(common);
            out.attachments |= !embedded;
        }
        BodyStructure::Text { .. } | BodyStructure::Message { .. } => out.attachments = true,
    }
}

/// The store row for a fetched message. The header fields and the start of the text are
/// parsed together as one truncated message, so mail-parser decodes RFC 2047 words, charsets
/// and transfer encodings the same way as for whole messages.
pub(crate) fn message(folder: FolderId, fetched: Fetched) -> NewMessage {
    let mut raw = fetched.header;
    if !raw.ends_with(b"\r\n\r\n") {
        raw.extend_from_slice(if raw.ends_with(b"\r\n") {
            b"\r\n"
        } else {
            b"\r\n\r\n"
        });
    }
    let cut = fetched.preview.len() as u32 >= PREVIEW_BYTES;
    let preview = match fetched.preview.iter().rposition(|&byte| byte == b'\n') {
        Some(end) if cut => &fetched.preview[..=end],
        _ => &fetched.preview[..],
    };
    raw.extend_from_slice(preview);
    if cut {
        close_multiparts(&mut raw);
    }
    let parsed = message::parse(&raw);
    let (flags, keywords) = Flags::from_imap(fetched.flags.iter().map(String::as_str));
    NewMessage {
        folder,
        uid: Some(fetched.uid),
        flags,
        keywords,
        date: fetched.internal_date.or(parsed.headers.date).unwrap_or(0),
        headers: parsed.headers,
        snippet: parsed.snippet,
        body_kind: BodyKind::from_parts(fetched.structure.plain, fetched.structure.html),
        has_attachments: fetched.structure.attachments,
        size: fetched.size.map_or(parsed.size, u64::from),
        body_state: if preview.is_empty() {
            BodyState::Headers
        } else {
            BodyState::Partial
        },
        search_text: parsed.body_text,
        ..NewMessage::default()
    }
}

/// Ends the multiparts a cut preview leaves open, innermost first. Without its closing
/// boundary mail-parser keeps a text part only inside an alternative, so the start of a long
/// first part of a mixed message would give no snippet.
fn close_multiparts(raw: &mut Vec<u8>) {
    let Some(parsed) = MessageParser::default().parse(raw.as_slice()) else {
        return;
    };
    let boundaries: Vec<String> = parsed
        .parts
        .iter()
        .filter_map(|part| part.content_type())
        .filter(|kind| kind.ctype().eq_ignore_ascii_case("multipart"))
        .filter_map(|kind| kind.attribute("boundary"))
        .map(str::to_owned)
        .collect();
    for boundary in boundaries.iter().rev() {
        raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// INTERNALDATE (`05-Oct-2026 09:12:00 +0200`, the day possibly space-padded) as Unix seconds.
pub(crate) fn internal_date(text: &str) -> Option<i64> {
    let (date, rest) = text.trim().split_once(' ')?;
    let mut parts = date.split('-');
    let day: i64 = parts.next()?.trim().parse().ok()?;
    let month = parts.next()?.to_ascii_lowercase();
    let month = MONTHS.iter().position(|name| *name == month)? as i64 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let (time, zone) = rest.trim().split_once(' ')?;
    let mut clock = time.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let offset: i64 =
        zone.get(1..3)?.parse::<i64>().ok()? * 3600 + zone.get(3..5)?.parse::<i64>().ok()? * 60;
    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - sign * offset,
    )
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_internal_dates() {
        assert_eq!(
            internal_date(" 1-Jan-2026 08:07:00 +0000"),
            Some(1_767_254_820)
        );
        assert_eq!(
            internal_date("05-Oct-2026 11:12:00 +0200"),
            internal_date("05-Oct-2026 09:12:00 +0000")
        );
        assert_eq!(
            internal_date("29-Feb-2024 23:59:59 -0130"),
            Some(1_709_251_199 + 5_400)
        );
        assert_eq!(internal_date("1-Foo-2026 08:07:00 +0000"), None);
        assert_eq!(internal_date("garbage"), None);
    }

    fn fetched(header: &str, preview: &str) -> Fetched {
        Fetched {
            uid: 42,
            flags: vec!["\\Seen".into(), "$Label1".into(), "\\Recent".into()],
            internal_date: Some(1_790_000_000),
            size: Some(4_321),
            header: header.as_bytes().to_vec(),
            preview: preview.as_bytes().to_vec(),
            structure: Structure {
                plain: true,
                html: true,
                attachments: false,
            },
        }
    }

    #[test]
    fn a_multipart_preview_decodes_into_a_snippet() {
        let header = "From: =?UTF-8?Q?Bj=C3=B8rn?= <bjorn@example.test>\r\n\
                      To: a@example.test\r\nSubject: =?UTF-8?B?SGlsc2VuIGZyYSBCw6ZydW0=?=\r\n\
                      Message-ID: <m42@example.test>\r\nIn-Reply-To: <m41@example.test>\r\n\
                      References: <m40@example.test> <m41@example.test>\r\n\
                      Content-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n";
        let preview = "This is a multi-part message.\r\n\r\n--b1\r\n\
                       Content-Type: text/plain; charset=utf-8\r\n\
                       Content-Transfer-Encoding: quoted-printable\r\n\r\n\
                       Hei, bl=C3=A5b=C3=A6r til alle.\r\n\r\n--b1\r\nContent-Type: text/ht";
        let row = message(FolderId(7), fetched(header, preview));
        assert_eq!(row.uid, Some(42));
        assert_eq!(row.flags, Flags::SEEN);
        assert_eq!(row.keywords, ["$Label1"]);
        assert_eq!(row.date, 1_790_000_000);
        assert_eq!(row.size, 4_321);
        assert_eq!(row.headers.subject, "Hilsen fra Bærum");
        assert_eq!(row.headers.from[0].name, "Bjørn");
        assert_eq!(row.headers.message_id.as_deref(), Some("m42@example.test"));
        assert_eq!(row.headers.references.len(), 2);
        assert_eq!(row.snippet, "Hei, blåbær til alle.");
        assert_eq!(row.body_kind, BodyKind::Both);
        assert_eq!(row.body_state, BodyState::Partial);
    }

    #[test]
    fn a_long_first_part_of_a_mixed_message_still_gives_a_snippet() {
        let header = "Subject: x\r\nMIME-Version: 1.0\r\n\
                      Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n";
        let body = "alpha bravo charlie delta echo foxtrot golf hotel india\r\n".repeat(30);
        let text = format!(
            "--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n--b1\r\n\
             Content-Type: application/octet-stream\r\n\r\nAAAA\r\n--b1--\r\n"
        );
        let mut cut = fetched(header, "");
        cut.preview = text.as_bytes()[..PREVIEW_BYTES as usize].to_vec();
        let row = message(FolderId(1), cut);
        assert!(row.snippet.starts_with("alpha bravo"), "{row:?}");

        let nested = format!(
            "--b1\r\nContent-Type: multipart/alternative;\r\n boundary=\"b2\"\r\n\r\n\
             --b2\r\nContent-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--b2--\r\n--b1--\r\n",
            "YWxwaGEgYnJhdm8gY2hhcmxpZSBkZWx0YSBlY2hvIGZveHRyb3QgZ29sZiBob3RlbCBpbmRpYQ0K\r\n"
                .repeat(30)
        );
        let mut cut = fetched(header, "");
        cut.preview = nested.as_bytes()[..PREVIEW_BYTES as usize].to_vec();
        let row = message(FolderId(1), cut);
        assert!(row.snippet.starts_with("alpha bravo"), "{row:?}");
    }

    #[test]
    fn a_cut_preview_drops_its_last_partial_line() {
        let header = "Subject: x\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n";
        let mut cut = fetched(header, "");
        cut.preview = "Første linjer.\r\n".repeat(80).into_bytes();
        cut.preview.truncate(PREVIEW_BYTES as usize - 1);
        cut.preview.push(0xc3);
        let row = message(FolderId(1), cut);
        assert!(row.search_text.trim_end().ends_with("Første linjer."));
        assert!(!row.search_text.contains('\u{fffd}'));
        let empty = message(FolderId(1), fetched("Subject: y\r\n", ""));
        assert_eq!(empty.headers.subject, "y");
        assert_eq!(empty.body_state, BodyState::Headers);
        assert_eq!(empty.snippet, "");
    }
}
