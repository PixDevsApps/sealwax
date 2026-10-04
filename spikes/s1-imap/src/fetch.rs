//! Turns FETCH responses into owned rows.

use async_imap::imap_proto::{
    Address, AttributeValue, BodyStructure, ContentDisposition, Envelope, MessageSection,
    SectionPath,
};
use chrono::DateTime;

use crate::store::{FlagUpdate, Header, split_flags};

/// The header items of the first sync, as the plan specifies them.
pub const HEADER_ITEMS: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE \
    BODY.PEEK[HEADER.FIELDS (REFERENCES IN-REPLY-TO LIST-UNSUBSCRIBE)])";
/// The same with MODSEQ, for CONDSTORE servers.
pub const HEADER_ITEMS_MODSEQ: &str = "(UID FLAGS MODSEQ INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE \
    BODY.PEEK[HEADER.FIELDS (REFERENCES IN-REPLY-TO LIST-UNSUBSCRIBE)])";

fn text(bytes: Option<&[u8]>) -> Option<String> {
    bytes
        .map(|b| String::from_utf8_lossy(b).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn address(a: &Address<'_>) -> Option<String> {
    let mailbox = a.mailbox.as_deref()?;
    let host = a.host.as_deref()?;
    Some(format!(
        "{}@{}",
        String::from_utf8_lossy(mailbox),
        String::from_utf8_lossy(host)
    ))
}

fn addresses(list: Option<&Vec<Address<'_>>>) -> String {
    list.map(|l| l.iter().filter_map(address).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

fn date(raw: Option<&[u8]>) -> Option<i64> {
    let raw = text(raw)?;
    let cleaned = raw.split(" (").next().unwrap_or(&raw).trim().to_owned();
    DateTime::parse_from_rfc2822(&cleaned)
        .ok()
        .map(|d| d.timestamp())
}

fn internal_date(raw: &str) -> Option<i64> {
    DateTime::parse_from_str(raw.trim(), "%d-%b-%Y %H:%M:%S %z")
        .ok()
        .map(|d| d.timestamp())
}

fn is_attachment(disposition: Option<&ContentDisposition<'_>>) -> bool {
    disposition.is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
}

/// True when any part is an attachment, or a leaf that isn't text or a multipart container.
pub fn has_attachments(structure: &BodyStructure<'_>) -> bool {
    match structure {
        BodyStructure::Multipart { bodies, .. } => bodies.iter().any(has_attachments),
        BodyStructure::Text { common, .. } => is_attachment(common.disposition.as_ref()),
        BodyStructure::Basic { common, .. } | BodyStructure::Message { common, .. } => {
            is_attachment(common.disposition.as_ref()) || !common.ty.ty.eq_ignore_ascii_case("text")
        }
    }
}

/// Unfolds the requested header fields into (references, in-reply-to, list-unsubscribe).
fn header_fields(block: &[u8]) -> (Option<String>, Option<String>, Option<String>) {
    let text = String::from_utf8_lossy(block);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            fields.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let get = |name: &str| {
        fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    (
        get("references"),
        get("in-reply-to"),
        get("list-unsubscribe"),
    )
}

fn apply_envelope(header: &mut Header, envelope: &Envelope<'_>) {
    header.date = date(envelope.date.as_deref());
    header.subject = text(envelope.subject.as_deref());
    if let Some(from) = envelope.from.as_ref().and_then(|f| f.first()) {
        header.from_name = text(from.name.as_deref());
        header.from_addr = address(from);
    }
    header.to_addrs = addresses(envelope.to.as_ref());
    header.cc_addrs = addresses(envelope.cc.as_ref());
    header.message_id =
        text(envelope.message_id.as_deref()).map(|m| m.trim_matches(['<', '>']).to_owned());
    if header.in_reply_to.is_none() {
        header.in_reply_to = text(envelope.in_reply_to.as_deref());
    }
}

/// A full header row from one FETCH response; `None` when the response carries no UID (an
/// unsolicited flag update), which the caller treats as a hint only.
pub fn header(attrs: &[AttributeValue<'_>]) -> Option<Header> {
    let mut header = Header::default();
    let mut uid = None;
    for attr in attrs {
        match attr {
            AttributeValue::Uid(u) => uid = Some(*u),
            AttributeValue::Flags(flags) => {
                (header.flags, header.keywords) = split_flags(flags.iter().map(|f| f.as_ref()));
            }
            AttributeValue::ModSeq(m) => header.modseq = Some(*m),
            AttributeValue::InternalDate(d) => header.internal_date = internal_date(d),
            AttributeValue::Rfc822Size(s) => header.size = Some(*s),
            AttributeValue::Envelope(envelope) => apply_envelope(&mut header, envelope),
            AttributeValue::BodyStructure(structure) => {
                header.has_attachments = has_attachments(structure)
            }
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                data,
                ..
            } => {
                let (references, in_reply_to, list_unsubscribe) =
                    header_fields(data.as_deref().unwrap_or_default());
                header.references = references;
                if in_reply_to.is_some() {
                    header.in_reply_to = in_reply_to;
                }
                header.list_unsubscribe = list_unsubscribe;
            }
            _ => {}
        }
    }
    header.uid = uid?;
    Some(header)
}

/// UID, FLAGS and MODSEQ from a FETCH response, if it has a UID and FLAGS.
pub fn flags(attrs: &[AttributeValue<'_>]) -> Option<FlagUpdate> {
    let mut uid = None;
    let mut flags = None;
    let mut modseq = None;
    for attr in attrs {
        match attr {
            AttributeValue::Uid(u) => uid = Some(*u),
            AttributeValue::Flags(f) => flags = Some(split_flags(f.iter().map(|f| f.as_ref()))),
            AttributeValue::ModSeq(m) => modseq = Some(*m),
            _ => {}
        }
    }
    let (flags, keywords) = flags?;
    Some(FlagUpdate {
        uid: uid?,
        flags,
        keywords,
        modseq,
    })
}

/// The Message-ID from a `BODY[HEADER.FIELDS (MESSAGE-ID)]` FETCH response.
pub fn message_id(attrs: &[AttributeValue<'_>]) -> Option<String> {
    attrs.iter().find_map(|attr| match attr {
        AttributeValue::BodySection {
            section: Some(SectionPath::Full(MessageSection::Header)),
            data,
            ..
        } => {
            let block = String::from_utf8_lossy(data.as_deref().unwrap_or_default()).into_owned();
            block
                .split("\r\n")
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(n, _)| n.trim().eq_ignore_ascii_case("message-id"))
                })
                .map(|(_, v)| v.trim().trim_matches(['<', '>']).to_owned())
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfolds_header_fields() {
        let block = b"References: <a@x>\r\n <b@x>\r\nIn-Reply-To: <b@x>\r\nList-Unsubscribe: <mailto:u@x>\r\n\r\n";
        let (references, in_reply_to, unsubscribe) = header_fields(block);
        assert_eq!(references.as_deref(), Some("<a@x> <b@x>"));
        assert_eq!(in_reply_to.as_deref(), Some("<b@x>"));
        assert_eq!(unsubscribe.as_deref(), Some("<mailto:u@x>"));
    }

    #[test]
    fn parses_dates() {
        assert_eq!(
            internal_date(" 1-Jan-2026 08:07:00 +0000"),
            Some(1_767_254_820)
        );
        assert_eq!(
            date(Some(b"Thu, 01 Jan 2026 08:07:00 +0000 (UTC)")),
            Some(1_767_254_820)
        );
    }
}
