use crate::ids;
use mail_builder::MessageBuilder;
use mail_builder::encoders::base64::Base64Encoder;
use mail_builder::headers::Header;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::mime::MimePart;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mailbox {
    pub name: Option<String>,
    pub address: String,
}

impl Mailbox {
    pub fn new(name: &str, address: &str) -> Self {
        Self {
            name: Some(name.to_string()),
            address: address.to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct InlineImage {
    pub content_id: String,
    pub mime_type: String,
    pub filename: Option<String>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub from: Mailbox,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    pub subject: String,
    pub html_fragment: String,
    pub inline_images: Vec<InlineImage>,
    pub attachments: Vec<Attachment>,
}

pub struct Outgoing {
    pub message_id: String,
    pub html: String,
    pub text: String,
    /// The copy handed to SMTP. It is built without ever seeing the Bcc list.
    pub transmitted: Vec<u8>,
    /// The local Sent copy: the transmitted bytes plus one Bcc header field.
    pub sent_copy: Vec<u8>,
    pub envelope_recipients: Vec<String>,
}

pub fn build(draft: &Draft, date: i64) -> Result<Outgoing, String> {
    let message_id = ids::message_id(&draft.from.address)?;
    let html = crlf(&html_document(&draft.html_fragment));
    let text = crlf(&plain_text(&html)?);
    let transmitted = transmitted_copy(draft, &message_id, date, &html, &text)?;
    let sent_copy = with_bcc(&transmitted, &draft.bcc)?;
    let mut envelope_recipients: Vec<String> = Vec::new();
    for mailbox in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
        if !envelope_recipients
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&mailbox.address))
        {
            envelope_recipients.push(mailbox.address.clone());
        }
    }
    Ok(Outgoing {
        message_id,
        html,
        text,
        transmitted,
        sent_copy,
        envelope_recipients,
    })
}

fn transmitted_copy(
    draft: &Draft,
    message_id: &str,
    date: i64,
    html: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    let mut builder = MessageBuilder::new().from(mailbox(&draft.from));
    if draft.to.is_empty() && draft.cc.is_empty() {
        builder = builder.to(Address::new_group(
            Some("undisclosed-recipients"),
            Vec::new(),
        ));
    } else if !draft.to.is_empty() {
        builder = builder.to(mailboxes(&draft.to));
    }
    if !draft.cc.is_empty() {
        builder = builder.cc(mailboxes(&draft.cc));
    }
    builder
        .subject(draft.subject.as_str())
        .date(Date::new(date))
        .message_id(message_id)
        .body(body(html, text, &draft.inline_images, &draft.attachments))
        .write_to_vec()
        .map_err(|error| error.to_string())
}

/// `mixed{ alternative{ text, related{ html, inline… } }, attachments… }`, leaving out the
/// `related` and `mixed` levels when they would hold a single part.
fn body<'a>(
    html: &'a str,
    text: &'a str,
    images: &'a [InlineImage],
    attachments: &'a [Attachment],
) -> MimePart<'a> {
    let html_part = MimePart::new("text/html", html);
    let rich = if images.is_empty() {
        html_part
    } else {
        let mut parts = vec![html_part];
        for image in images {
            let mut disposition = ContentType::new("inline");
            if let Some(filename) = &image.filename {
                disposition = disposition.attribute("filename", filename.as_str());
            }
            parts.push(
                MimePart::new(image.mime_type.as_str(), image.data.as_slice())
                    .cid(image.content_id.as_str())
                    .header("Content-Disposition", disposition),
            );
        }
        MimePart::new(
            ContentType::new("multipart/related").attribute("type", "text/html"),
            parts,
        )
    };
    let alternative = MimePart::new(
        "multipart/alternative",
        vec![MimePart::new("text/plain", text), rich],
    );
    if attachments.is_empty() {
        return alternative;
    }
    let mut parts = vec![alternative];
    for attachment in attachments {
        let mut content_type = ContentType::new(attachment.mime_type.as_str());
        if attachment.mime_type.starts_with("text/") {
            content_type = content_type.attribute("charset", "utf-8");
        }
        let content_type = content_type.attribute("name", rfc2047_parameter(&attachment.filename));
        parts.push(
            MimePart::new(content_type, attachment.data.as_slice())
                .attachment(attachment.filename.as_str()),
        );
    }
    MimePart::new("multipart/mixed", parts)
}

/// The legacy `name=` parameter as one RFC 2047 encoded word, next to the RFC 2231
/// `filename*=` that mail-builder writes; clients that predate RFC 2231 read this one.
fn rfc2047_parameter(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }
    let encoded = Base64Encoder::new()
        .encode(value.as_bytes())
        .unwrap_or_default();
    format!("=?UTF-8?B?{}?=", String::from_utf8_lossy(&encoded))
}

fn mailbox(mailbox: &Mailbox) -> Address<'_> {
    Address::new_address(mailbox.name.as_deref(), mailbox.address.as_str())
}

fn mailboxes(list: &[Mailbox]) -> Address<'_> {
    match list {
        [single] => mailbox(single),
        _ => Address::new_list(list.iter().map(mailbox).collect()),
    }
}

/// Inserts the Bcc field before Subject; everything else stays byte for byte the same.
fn with_bcc(transmitted: &[u8], bcc: &[Mailbox]) -> Result<Vec<u8>, String> {
    if bcc.is_empty() {
        return Ok(transmitted.to_vec());
    }
    let header_end = find(transmitted, b"\r\n\r\n").ok_or("the message has no header block")?;
    let subject = find(&transmitted[..header_end], b"\r\nSubject: ")
        .ok_or("the message has no Subject field")?
        + 2;
    let mut field = b"Bcc: ".to_vec();
    mailboxes(bcc).write_header(&mut field, 5);
    let mut copy = Vec::with_capacity(transmitted.len() + field.len());
    copy.extend_from_slice(&transmitted[..subject]);
    copy.extend_from_slice(&field);
    copy.extend_from_slice(&transmitted[subject..]);
    Ok(copy)
}

pub fn html_document(fragment: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta http-equiv=\"Content-Type\" \
         content=\"text/html; charset=utf-8\">\n</head>\n<body>\n{fragment}\n</body>\n</html>\n"
    )
}

pub fn plain_text(html: &str) -> Result<String, String> {
    html2text::config::plain()
        .string_from_read(html.as_bytes(), 78)
        .map_err(|error| error.to_string())
}

pub fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The header block of a message, for showing the difference between the two copies.
pub fn header_block(message: &[u8]) -> String {
    let end = find(message, b"\r\n\r\n").unwrap_or(message.len());
    String::from_utf8_lossy(&message[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(bcc: Vec<Mailbox>) -> Draft {
        Draft {
            from: Mailbox::new("Kari Nordmann", "kari@example.com"),
            to: vec![Mailbox::new("Ola Nordmann", "ola@example.org")],
            cc: Vec::new(),
            bcc,
            subject: "Test".into(),
            html_fragment: "<div>Hei</div>".into(),
            inline_images: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn bcc_only_in_the_sent_copy() {
        let outgoing = build(
            &draft(vec![Mailbox::new("Blind Kopi", "blind@example.net")]),
            1_790_000_000,
        )
        .unwrap();
        let transmitted = header_block(&outgoing.transmitted);
        let sent = header_block(&outgoing.sent_copy);
        assert!(
            !transmitted
                .split("\r\n")
                .any(|line| line.to_ascii_lowercase().starts_with("bcc:"))
        );
        assert!(!transmitted.contains("blind@example.net"));
        assert!(sent.contains("\r\nBcc: \"Blind Kopi\" <blind@example.net>\r\n"));
        assert_eq!(
            outgoing.sent_copy.len() - outgoing.transmitted.len(),
            "Bcc: \"Blind Kopi\" <blind@example.net>\r\n".len()
        );
        assert!(
            outgoing
                .envelope_recipients
                .contains(&"blind@example.net".to_string())
        );
    }

    #[test]
    fn bcc_only_message_gets_undisclosed_recipients() {
        let mut only_bcc = draft(vec![Mailbox::new("Blind Kopi", "blind@example.net")]);
        only_bcc.to.clear();
        let outgoing = build(&only_bcc, 1_790_000_000).unwrap();
        let headers = header_block(&outgoing.transmitted);
        assert!(
            headers.contains("\r\nTo: \"undisclosed-recipients\":;\r\n"),
            "{headers}"
        );
        assert!(!headers.contains("blind@example.net"));
    }

    #[test]
    fn non_ascii_name_parameter_is_one_encoded_word() {
        assert_eq!(rfc2047_parameter("report.pdf"), "report.pdf");
        let encoded = rfc2047_parameter("æøå.pdf");
        assert!(encoded.starts_with("=?UTF-8?B?") && encoded.ends_with("?="));
    }
}
