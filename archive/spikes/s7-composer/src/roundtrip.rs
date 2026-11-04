use crate::mime::{Draft, Mailbox, Outgoing};
use mail_parser::{Message, MessageParser, MimeHeaders, PartType};

pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Copy {
    Transmitted,
    SentCopy,
}

pub struct Expected<'a> {
    pub draft: &'a Draft,
    pub outgoing: &'a Outgoing,
}

#[derive(Default)]
struct Checks(Vec<Check>);

impl Checks {
    fn add(&mut self, name: &'static str, ok: bool, detail: impl Into<String>) {
        self.0.push(Check {
            name,
            ok,
            detail: detail.into(),
        });
    }
}

/// The machine's host names (GLib, /etc/hostname, $HOSTNAME, and their first labels), lowercase.
pub fn hostnames() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let sources = [
        Some(gtk4::glib::host_name().to_string()),
        std::fs::read_to_string("/etc/hostname").ok(),
        std::env::var("HOSTNAME").ok(),
    ];
    for source in sources.into_iter().flatten() {
        let name = source.trim().to_ascii_lowercase();
        let short = name.split('.').next().unwrap_or_default().to_string();
        for candidate in [name, short] {
            if !candidate.is_empty() && !names.contains(&candidate) {
                names.push(candidate);
            }
        }
    }
    names
}

pub fn check(raw: &[u8], copy: Copy, expected: Option<&Expected<'_>>) -> Vec<Check> {
    let mut checks = Checks::default();
    raw_checks(raw, copy, &mut checks);
    let Some(message) = MessageParser::default().parse(raw) else {
        checks.add("parses", false, "mail-parser returned None");
        return checks.0;
    };
    checks.add(
        "parses",
        true,
        format!("{} MIME parts", message.parts.len()),
    );
    structure_checks(&message, &mut checks);
    content_checks(&message, &mut checks);
    if let Some(expected) = expected {
        expected_checks(&message, copy, expected, &mut checks);
    }
    checks.0
}

fn raw_checks(raw: &[u8], copy: Copy, checks: &mut Checks) {
    let crlf_only = raw.iter().enumerate().all(|(at, &byte)| match byte {
        b'\n' => at > 0 && raw[at - 1] == b'\r',
        b'\r' => raw.get(at + 1) == Some(&b'\n'),
        _ => true,
    });
    checks.add("CRLF line endings only", crlf_only, "");
    let longest = raw
        .split(|&byte| byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).len())
        .max()
        .unwrap_or(0);
    checks.add(
        "lines <= 998 octets",
        longest <= 998,
        format!("longest line {longest}"),
    );
    checks.add(
        "7-bit clean (no 8BITMIME needed)",
        raw.iter().all(|byte| byte.is_ascii()),
        "",
    );
    let headers = crate::mime::header_block(raw).to_ascii_lowercase();
    let has_bcc = headers.split("\r\n").any(|line| line.starts_with("bcc:"));
    match copy {
        Copy::Transmitted => checks.add("no Bcc header field (transmitted)", !has_bcc, ""),
        Copy::SentCopy => checks.add("Bcc header field kept (Sent copy)", has_bcc, ""),
    }
    let lower = raw.to_ascii_lowercase();
    let leaked: Vec<String> = hostnames()
        .into_iter()
        .filter(|name| crate::mime::find(&lower, name.as_bytes()).is_some())
        .collect();
    checks.add(
        "hostname absent from raw bytes",
        leaked.is_empty(),
        if leaked.is_empty() {
            format!("checked {} host name forms", hostnames().len())
        } else {
            "found the hostname".to_string()
        },
    );
}

fn content_type(message: &Message<'_>, id: usize) -> String {
    message.parts[id]
        .content_type()
        .map(|ct| {
            format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or_default()).to_ascii_lowercase()
        })
        .unwrap_or_else(|| "text/plain".into())
}

fn children(message: &Message<'_>, id: usize) -> Vec<usize> {
    match &message.parts[id].body {
        PartType::Multipart(ids) => ids.iter().map(|&child| child as usize).collect(),
        _ => Vec::new(),
    }
}

fn disposition(message: &Message<'_>, id: usize) -> Option<String> {
    message.parts[id]
        .content_disposition()
        .map(|cd| cd.ctype().to_ascii_lowercase())
}

pub fn shape(message: &Message<'_>, id: usize) -> String {
    let kids = children(message, id);
    if kids.is_empty() {
        content_type(message, id)
    } else {
        let inner: Vec<String> = kids.into_iter().map(|kid| shape(message, kid)).collect();
        format!("{}[{}]", content_type(message, id), inner.join(", "))
    }
}

fn structure_checks(message: &Message<'_>, checks: &mut Checks) {
    let result = match content_type(message, 0).as_str() {
        "multipart/mixed" => {
            let kids = children(message, 0);
            match kids.split_first() {
                Some((&first, rest)) => validate_alternative(message, first).and_then(|()| {
                    rest.iter().try_for_each(|&kid| {
                        if !children(message, kid).is_empty() {
                            Err("an attachment is a multipart".into())
                        } else if disposition(message, kid).as_deref() != Some("attachment") {
                            Err("an attachment lacks Content-Disposition: attachment".into())
                        } else {
                            Ok(())
                        }
                    })
                }),
                None => Err("empty multipart/mixed".into()),
            }
        }
        "multipart/alternative" => validate_alternative(message, 0),
        other => Err(format!("unexpected root {other}")),
    };
    checks.add(
        "structure mixed{alternative{text, related{html, inline}}, attachments}",
        result.is_ok(),
        match result {
            Ok(()) => shape(message, 0),
            Err(error) => format!("{error}; shape {}", shape(message, 0)),
        },
    );
}

fn validate_alternative(message: &Message<'_>, id: usize) -> Result<(), String> {
    if content_type(message, id) != "multipart/alternative" {
        return Err("first part is not multipart/alternative".into());
    }
    match children(message, id).as_slice() {
        [text, rich] => {
            if content_type(message, *text) != "text/plain" {
                return Err("alternative does not start with text/plain".into());
            }
            match content_type(message, *rich).as_str() {
                "text/html" => Ok(()),
                "multipart/related" => validate_related(message, *rich),
                other => Err(format!("unexpected rich part {other}")),
            }
        }
        _ => Err("multipart/alternative must hold exactly two parts".into()),
    }
}

fn validate_related(message: &Message<'_>, id: usize) -> Result<(), String> {
    let root_type = message.parts[id]
        .content_type()
        .and_then(|ct| ct.attribute("type"))
        .map(str::to_ascii_lowercase);
    if root_type.as_deref() != Some("text/html") {
        return Err("multipart/related lacks type=\"text/html\"".into());
    }
    let kids = children(message, id);
    let Some((&html, inline)) = kids.split_first() else {
        return Err("empty multipart/related".into());
    };
    if content_type(message, html) != "text/html" {
        return Err("multipart/related does not start with text/html".into());
    }
    for &part in inline {
        if message.parts[part].content_id().is_none() {
            return Err("an inline part has no Content-ID".into());
        }
        if disposition(message, part).as_deref() != Some("inline") {
            return Err("an inline part lacks Content-Disposition: inline".into());
        }
    }
    Ok(())
}

fn cid_references(html: &str) -> Vec<String> {
    let mut references = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("src=\"cid:") {
        rest = &rest[at + 9..];
        if let Some(end) = rest.find('"') {
            references.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    references
}

fn content_checks(message: &Message<'_>, checks: &mut Checks) {
    let text = message
        .text_part(0)
        .filter(|part| part.is_content_type("text", "plain"))
        .and_then(|part| part.text_contents());
    checks.add(
        "text/plain part present",
        text.is_some_and(|text| !text.trim().is_empty()),
        format!("{} chars", text.map(str::len).unwrap_or(0)),
    );
    let html = message
        .html_part(0)
        .filter(|part| part.is_content_type("text", "html"))
        .and_then(|part| part.text_contents());
    checks.add(
        "text/html part present",
        html.is_some_and(|html| html.contains("<body>")),
        format!("{} chars", html.map(str::len).unwrap_or(0)),
    );
    let references = html.map(cid_references).unwrap_or_default();
    let inline_ids: Vec<&str> = message
        .parts
        .iter()
        .filter(|part| {
            part.content_disposition()
                .is_some_and(|cd| cd.ctype() == "inline")
        })
        .filter_map(|part| part.content_id())
        .collect();
    let missing: Vec<&String> = references
        .iter()
        .filter(|reference| !inline_ids.contains(&reference.as_str()))
        .collect();
    let unused: Vec<&&str> = inline_ids
        .iter()
        .filter(|id| !references.iter().any(|reference| reference == **id))
        .collect();
    checks.add(
        "every cid: reference has an inline part, and every inline part is referenced",
        missing.is_empty() && unused.is_empty(),
        format!(
            "{} references, {} inline parts, {} missing, {} unused",
            references.len(),
            inline_ids.len(),
            missing.len(),
            unused.len()
        ),
    );
    let id = message.message_id().unwrap_or_default();
    let domain = id
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .unwrap_or_default();
    checks.add(
        "Message-ID present with a domain",
        domain.contains('.'),
        format!("@{domain}"),
    );
    checks.add("Date present", message.date().is_some(), "");
}

fn mailboxes(address: Option<&mail_parser::Address<'_>>) -> Vec<(Option<String>, String)> {
    address
        .map(|address| {
            address
                .iter()
                .map(|addr| {
                    (
                        addr.name().map(str::to_string),
                        addr.address().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn expected_mailboxes(list: &[Mailbox]) -> Vec<(Option<String>, String)> {
    list.iter()
        .map(|mailbox| (mailbox.name.clone(), mailbox.address.clone()))
        .collect()
}

fn normalized(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_string()
}

fn expected_checks(
    message: &Message<'_>,
    copy: Copy,
    expected: &Expected<'_>,
    checks: &mut Checks,
) {
    let draft = expected.draft;
    checks.add(
        "Subject decodes to the original (RFC 2047)",
        message.subject() == Some(draft.subject.as_str()),
        format!("{:?}", message.subject().unwrap_or_default()),
    );
    let from = mailboxes(message.from());
    checks.add(
        "From decodes to the original",
        from == expected_mailboxes(std::slice::from_ref(&draft.from)),
        format!("{from:?}"),
    );
    checks.add(
        "To decodes to the original",
        mailboxes(message.to()) == expected_mailboxes(&draft.to),
        format!("{} addresses", mailboxes(message.to()).len()),
    );
    checks.add(
        "Cc decodes to the original",
        mailboxes(message.cc()) == expected_mailboxes(&draft.cc),
        format!("{} addresses", mailboxes(message.cc()).len()),
    );
    let bcc = mailboxes(message.bcc());
    match copy {
        Copy::Transmitted => {
            checks.add("mail-parser sees no Bcc (transmitted)", bcc.is_empty(), "")
        }
        Copy::SentCopy => checks.add(
            "mail-parser reads the Bcc list (Sent copy)",
            bcc == expected_mailboxes(&draft.bcc),
            format!("{} addresses", bcc.len()),
        ),
    }
    checks.add(
        "Message-ID is ours",
        message.message_id() == Some(expected.outgoing.message_id.as_str()),
        "",
    );
    let text = message.text_part(0).and_then(|part| part.text_contents());
    checks.add(
        "text/plain round-trips (html2text output)",
        text.map(normalized) == Some(normalized(&expected.outgoing.text)),
        "",
    );
    let html = message.html_part(0).and_then(|part| part.text_contents());
    checks.add(
        "text/html round-trips",
        html.map(normalized) == Some(normalized(&expected.outgoing.html)),
        "",
    );
    let mut image_problems = Vec::new();
    for image in &draft.inline_images {
        let part = message
            .parts
            .iter()
            .find(|part| part.content_id() == Some(image.content_id.as_str()));
        match part {
            None => image_problems.push(format!("{} missing", image.content_id)),
            Some(part) => {
                if part.contents() != image.data.as_slice() {
                    image_problems.push("bytes differ".into());
                }
                if !part.is_content_type(
                    image.mime_type.split('/').next().unwrap_or_default(),
                    image.mime_type.split('/').nth(1).unwrap_or_default(),
                ) {
                    image_problems.push("content type differs".into());
                }
            }
        }
    }
    checks.add(
        "inline images: Content-ID, type and bytes",
        image_problems.is_empty(),
        if image_problems.is_empty() {
            format!("{} images", draft.inline_images.len())
        } else {
            image_problems.join("; ")
        },
    );
    let mut attachment_problems = Vec::new();
    for attachment in &draft.attachments {
        let part = message
            .parts
            .iter()
            .find(|part| part.attachment_name() == Some(attachment.filename.as_str()));
        match part {
            None => {
                attachment_problems.push(format!("{:?} not found by name", attachment.filename))
            }
            Some(part) => {
                if part.contents() != attachment.data.as_slice() {
                    attachment_problems.push(format!("{:?} bytes differ", attachment.filename));
                }
                let name = part.content_type().and_then(|ct| ct.attribute("name"));
                if name != Some(attachment.filename.as_str()) {
                    attachment_problems.push(format!(
                        "{:?} Content-Type name= decodes to {name:?}",
                        attachment.filename
                    ));
                }
            }
        }
    }
    checks.add(
        "attachment names (RFC 2231 filename*, RFC 2047 name=) and bytes",
        attachment_problems.is_empty(),
        if attachment_problems.is_empty() {
            format!("{} attachments", draft.attachments.len())
        } else {
            attachment_problems.join("; ")
        },
    );
}

pub fn print(label: &str, checks: &[Check]) -> bool {
    let ok = checks.iter().all(|check| check.ok);
    println!("{} {label}", if ok { "PASS" } else { "FAIL" });
    for check in checks {
        let mark = if check.ok { "ok  " } else { "FAIL" };
        if check.detail.is_empty() {
            println!("  {mark} {}", check.name);
        } else {
            println!("  {mark} {} — {}", check.name, check.detail);
        }
    }
    ok
}
