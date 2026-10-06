//! Round trips through mail-parser on every copy, with S7's fixture cases
//! (`spikes/s7-composer/src/fixtures.rs` and `roundtrip.rs`).

use super::*;
use crate::compose::{DraftSource, ReplyKind, reply, stored};
use crate::model::{AccountId, BlobKey, MessageId as StoredId};
use mail_parser::{Message, MessageParser, MimeHeaders, PartType};
use proptest::prelude::*;

/// 2026-10-04 12:00:00 UTC, S7's fixture date.
const DATE: i64 = 1_791_115_200;
const ID: &str = "k3j5x8m2q9wa7b4c6d2e5f8g3h@example.com";
const NORWEGIAN_SUBJECT: &str = "Blåbærsyltetøy 🫐 på brødskiva — møte i morgen?";

fn sender() -> Address {
    Address::new("Kari Nordmann", "kari@example.com")
}

fn recipients() -> (Vec<Address>, Vec<Address>, Vec<Address>) {
    (
        vec![
            Address::new("Ola Nordmann", "ola@example.org"),
            Address::new("Bjørn Ærø 🙂", "bjorn@example.net"),
        ],
        vec![Address::new("Øystein Åsen", "oystein@example.org")],
        vec![
            Address::new("Blind Kopi", "blind@example.net"),
            Address::new("Hemmelig Mottaker", "hemmelig@example.com"),
        ],
    )
}

fn image() -> Vec<u8> {
    (0..=255u8).cycle().take(700).collect()
}

fn pdf() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n".to_vec()
}

fn notes() -> Vec<u8> {
    "Notater: blåbær, multebær og tyttebær.\n"
        .as_bytes()
        .to_vec()
}

fn part(name: &str, mime_type: &str, data: &[u8], content_id: Option<&str>) -> DraftPart {
    DraftPart {
        name: name.into(),
        mime_type: mime_type.into(),
        size: data.len() as u64,
        blob: BlobKey([0; 32]),
        content_id: content_id.map(str::to_owned),
    }
}

fn draft(subject: &str, html: &str) -> Draft {
    Draft {
        account: AccountId(1),
        from: sender(),
        to: vec![Address::new("Ola Nordmann", "ola@example.org")],
        subject: subject.into(),
        html: html.into(),
        message_id: ID.into(),
        ..Draft::default()
    }
}

/// S7's "full" sample: Norwegian and emoji names and subject, Cc and Bcc, formatting, a quote
/// and an indent, an inline image, a PDF and a UTF-8 text attachment.
fn full() -> (Draft, Vec<Vec<u8>>) {
    let (to, cc, bcc) = recipients();
    let cid = "aaaaaaaaaaaaaaaaaaaaaaaaaa@example.com";
    let html = format!(
        "<div>Hei Ola,</div>\n<div><br></div>\n\
         <div>Dette er <b>fet</b>, <i>kursiv</i>, <u>understreket</u> og \
         <strike>gjennomstreket</strike> tekst med æøå, ÆØÅ og 🫐.</div>\n\
         <ul><li>Første punkt</li><li>Andre punkt</li></ul>\n\
         <ol><li>Én</li><li>To</li></ol>\n\
         <div>Lenke: <a href=\"https://example.org/bl%C3%A5b%C3%A6r?a=1&amp;b=2\">example.org</a></div>\n\
         <blockquote type=\"cite\" style=\"margin:0 0 0 0.8ex;border-left:2px solid #8a8a8a;padding-left:1ex\">\
         <div>Sitert tekst fra forrige melding.</div></blockquote>\n\
         <blockquote style=\"margin: 0 0 0 40px; border: none; padding: 0px;\">\
         <div>Innrykket avsnitt.</div></blockquote>\n\
         <div><img src=\"cid:{cid}\" alt=\"Blåbær\" width=\"120\" height=\"75\"></div>\n\
         <div>Hilsen Kari</div>"
    );
    let data = vec![image(), pdf(), notes()];
    let draft = Draft {
        to,
        cc,
        bcc,
        parts: vec![
            part("blåbær.png", "image/png", &data[0], Some(cid)),
            part(
                "Årsrapport for blåbær 🫐 2026.pdf",
                "application/pdf",
                &data[1],
                None,
            ),
            part("notater æøå.txt", "text/plain", &data[2], None),
        ],
        ..draft(NORWEGIAN_SUBJECT, &html)
    };
    (draft, data)
}

fn samples() -> Vec<(&'static str, Draft, Vec<Vec<u8>>)> {
    let (draft_full, data_full) = full();
    let plain = Draft {
        bcc: vec![Address::new("Blind Kopi", "blind@example.net")],
        ..draft(
            "Plain alternative test",
            "<div>Hello <b>world</b>, no images and no attachments.</div>",
        )
    };
    let bcc_only = Draft {
        to: Vec::new(),
        bcc: recipients().2,
        ..draft(
            "Kun blindkopi",
            "<div>Only Bcc recipients: the transmitted copy says undisclosed-recipients.</div>",
        )
    };
    let long_url = format!("https://example.org/{}", "a1b2c3d4e5".repeat(150));
    let long = Draft {
        bcc: vec![Address::new("Blind Kopi", "blind@example.net")],
        ..draft(
            &format!("{NORWEGIAN_SUBJECT} {NORWEGIAN_SUBJECT} {NORWEGIAN_SUBJECT}"),
            &format!(
                "<div>A line longer than 998 octets without spaces: <a href=\"{long_url}\">{long_url}</a></div>"
            ),
        )
    };
    let reply = Draft {
        in_reply_to: Some("m3@example.org".into()),
        references: vec!["m1@example.org".into(), "m3@example.org".into()],
        source: Some(DraftSource {
            message: StoredId(4),
            kind: ReplyKind::Reply,
        }),
        ..draft(
            "Re: Møte",
            "<div>Takk!</div><div><br></div><div class=\"sw-signature\"><div>--&nbsp;</div><div>Kari</div></div>",
        )
    };
    vec![
        ("full", draft_full, data_full),
        ("plain", plain, Vec::new()),
        ("bcc-only", bcc_only, Vec::new()),
        ("long", long, Vec::new()),
        ("reply", reply, Vec::new()),
    ]
}

fn slices(data: &[Vec<u8>]) -> Vec<&[u8]> {
    data.iter().map(Vec::as_slice).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Which {
    Transmitted,
    Sent,
    Drafts,
}

fn header_block(raw: &[u8]) -> String {
    let end = find(raw, b"\r\n\r\n").unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

/// The machine's host names (/etc/hostname, $HOSTNAME and their first labels), lower case;
/// names under 4 characters are left out, as they turn up in encoded text by chance.
fn hostnames() -> Vec<String> {
    let mut names = Vec::new();
    let sources = [
        std::fs::read_to_string("/etc/hostname").ok(),
        std::env::var("HOSTNAME").ok(),
    ];
    for source in sources.into_iter().flatten() {
        let name = source.trim().to_ascii_lowercase();
        let short = name.split('.').next().unwrap_or_default().to_owned();
        for candidate in [name, short] {
            if candidate.len() >= 4 && !names.contains(&candidate) {
                names.push(candidate);
            }
        }
    }
    names
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

fn shape(message: &Message<'_>, id: usize) -> String {
    let kids = children(message, id);
    if kids.is_empty() {
        content_type(message, id)
    } else {
        let inner: Vec<String> = kids.into_iter().map(|kid| shape(message, kid)).collect();
        format!("{}[{}]", content_type(message, id), inner.join(", "))
    }
}

fn check_alternative(message: &Message<'_>, id: usize) {
    assert_eq!(content_type(message, id), "multipart/alternative");
    let [text, rich] = children(message, id)[..] else {
        panic!("alternative holds two parts: {}", shape(message, 0));
    };
    assert_eq!(content_type(message, text), "text/plain");
    match content_type(message, rich).as_str() {
        "text/html" => {}
        "multipart/related" => {
            let root = message.parts[rich]
                .content_type()
                .and_then(|ct| ct.attribute("type"));
            assert_eq!(root, Some("text/html"));
            let kids = children(message, rich);
            assert_eq!(content_type(message, kids[0]), "text/html");
            for &inline in &kids[1..] {
                assert!(message.parts[inline].content_id().is_some());
                assert_eq!(disposition(message, inline).as_deref(), Some("inline"));
            }
        }
        other => panic!("rich part {other}"),
    }
}

fn mailboxes(value: Option<&mail_parser::Address<'_>>) -> Vec<(String, String)> {
    value
        .map(|value| {
            value
                .iter()
                .map(|a| {
                    (
                        a.name().unwrap_or_default().to_owned(),
                        a.address().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn expected(list: &[Address]) -> Vec<(String, String)> {
    list.iter()
        .map(|a| (header_text(&a.name), address::ascii(&a.email).unwrap()))
        .collect()
}

fn normalized(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_owned()
}

/// S7's round-trip checks on one copy.
fn check(raw: &[u8], which: Which, draft: &Draft, data: &[Vec<u8>]) {
    let crlf_only = raw.iter().enumerate().all(|(at, &byte)| match byte {
        b'\n' => at > 0 && raw[at - 1] == b'\r',
        b'\r' => raw.get(at + 1) == Some(&b'\n'),
        _ => true,
    });
    assert!(crlf_only, "CRLF only");
    let longest = raw
        .split(|&byte| byte == b'\n')
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0);
    assert!(longest <= 1_000, "longest line {longest}");
    assert!(raw.is_ascii(), "7-bit");
    let headers = header_block(raw).to_ascii_lowercase();
    let has = |name: &str| headers.split("\r\n").any(|line| line.starts_with(name));
    assert_eq!(
        has("bcc:"),
        which != Which::Transmitted && !draft.bcc.is_empty(),
        "{which:?}"
    );
    for never in [
        "user-agent:",
        "x-mailer:",
        "received:",
        "x-originating",
        "sender:",
    ] {
        assert!(!has(never), "{never}");
    }
    let lower = raw.to_ascii_lowercase();
    for name in hostnames() {
        assert!(
            find(&lower, name.as_bytes()).is_none(),
            "the host name is in the {which:?} copy"
        );
    }
    assert!(find(&lower, b"localhost").is_none());

    let message = MessageParser::default().parse(raw).expect("parses");
    match content_type(&message, 0).as_str() {
        "multipart/mixed" => {
            let kids = children(&message, 0);
            check_alternative(&message, kids[0]);
            for &kid in &kids[1..] {
                assert!(children(&message, kid).is_empty());
                assert_eq!(disposition(&message, kid).as_deref(), Some("attachment"));
            }
        }
        "multipart/alternative" => check_alternative(&message, 0),
        other => panic!("root {other}"),
    }
    assert_eq!(
        message.subject().unwrap_or_default(),
        header_text(&draft.subject)
    );
    assert_eq!(
        mailboxes(message.from()),
        expected(std::slice::from_ref(&draft.from))
    );
    if draft.to.is_empty() && draft.cc.is_empty() && which == Which::Transmitted {
        assert!(
            headers.contains("\r\nto: \"undisclosed-recipients\":;\r\n"),
            "{headers}"
        );
    } else {
        assert_eq!(mailboxes(message.to()), expected(&draft.to));
    }
    assert_eq!(mailboxes(message.cc()), expected(&draft.cc));
    let bcc = mailboxes(message.bcc());
    if which == Which::Transmitted {
        assert!(bcc.is_empty());
    } else {
        assert_eq!(bcc, expected(&draft.bcc));
    }
    assert_eq!(message.message_id(), Some(draft.message_id.as_str()));
    assert_eq!(message.date().map(|date| date.to_timestamp()), Some(DATE));
    assert!(
        headers.contains("\r\ndate: sun, 4 oct 2026 12:00:00 +0000"),
        "UTC: {headers}"
    );
    match &draft.in_reply_to {
        Some(id) => assert_eq!(message.in_reply_to().as_text(), Some(id.as_str())),
        None => assert!(!has("in-reply-to:")),
    }
    let references: Vec<&str> = match message.references() {
        mail_parser::HeaderValue::Text(id) => vec![id.as_ref()],
        mail_parser::HeaderValue::TextList(ids) => ids.iter().map(|id| id.as_ref()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(
        references,
        draft
            .references
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );

    let html = message
        .html_part(0)
        .and_then(|part| part.text_contents())
        .unwrap();
    let document = html_document(&draft.html);
    assert_eq!(normalized(html), normalized(&document));
    let text = message
        .text_part(0)
        .and_then(|part| part.text_contents())
        .unwrap();
    assert_eq!(normalized(text), normalized(&plain_text(&crlf(&document))));
    assert!(!text.trim().is_empty());

    let references: Vec<String> = html
        .match_indices("src=\"cid:")
        .map(|(at, _)| {
            let rest = &html[at + 9..];
            rest[..rest.find('"').unwrap()].to_owned()
        })
        .collect();
    let inline: Vec<&str> = message
        .parts
        .iter()
        .filter(|part| {
            part.content_disposition()
                .is_some_and(|cd| cd.ctype() == "inline")
        })
        .filter_map(|part| part.content_id())
        .collect();
    assert_eq!(references.len(), inline.len());
    for reference in &references {
        assert!(inline.contains(&reference.as_str()), "{reference}");
    }
    for (part, bytes) in draft.parts.iter().zip(data) {
        match &part.content_id {
            Some(id) => {
                let found = message
                    .parts
                    .iter()
                    .find(|p| p.content_id() == Some(id.as_str()))
                    .expect("inline part");
                assert_eq!(found.contents(), bytes.as_slice());
                let (kind, subtype) = part.mime_type.split_once('/').unwrap();
                assert!(found.is_content_type(kind, subtype));
            }
            None => {
                let found = message
                    .parts
                    .iter()
                    .find(|p| p.attachment_name() == Some(part.name.as_str()))
                    .unwrap_or_else(|| panic!("{} by name", part.name));
                assert_eq!(found.contents(), bytes.as_slice());
                let name = found.content_type().and_then(|ct| ct.attribute("name"));
                assert_eq!(name, Some(part.name.as_str()), "name= (RFC 2047)");
                if part.mime_type == "text/plain" {
                    assert_eq!(
                        found.content_type().and_then(|ct| ct.attribute("charset")),
                        Some("utf-8")
                    );
                }
            }
        }
    }
}

#[test]
fn s7_samples_round_trip_on_every_copy() {
    for (name, draft, data) in samples() {
        let built = build(&draft, DATE, &slices(&data)).unwrap_or_else(|e| panic!("{name}: {e}"));
        check(&built.transmitted, Which::Transmitted, &draft, &data);
        check(&built.sent_copy, Which::Sent, &draft, &data);
        let drafts = drafts_copy(&draft, DATE, &slices(&data)).unwrap();
        check(&drafts, Which::Drafts, &draft, &data);
    }
}

#[test]
fn the_sent_copy_is_the_transmitted_bytes_plus_bcc() {
    let (draft, data) = full();
    let built = build(&draft, DATE, &slices(&data)).unwrap();
    let transmitted = header_block(&built.transmitted);
    let sent = header_block(&built.sent_copy);
    assert!(!transmitted.to_ascii_lowercase().contains("bcc"));
    assert!(!transmitted.contains("blind@") && !transmitted.contains("hemmelig@"));
    let added: Vec<&str> = sent
        .split("\r\n")
        .filter(|line| !transmitted.split("\r\n").any(|known| known == *line))
        .collect();
    assert_eq!(
        added,
        [
            "Bcc: \"Blind Kopi\" <blind@example.net>, \"Hemmelig Mottaker\"",
            " <hemmelig@example.com>"
        ]
    );
    let at = find(&built.transmitted, b"\r\nSubject: ").unwrap();
    let bcc_len = built.sent_copy.len() - built.transmitted.len();
    assert_eq!(built.sent_copy[..at + 2], built.transmitted[..at + 2]);
    assert_eq!(
        built.sent_copy[at + 2 + bcc_len..],
        built.transmitted[at + 2..]
    );
    assert_eq!(built.envelope.mail_from, "kari@example.com");
    assert_eq!(
        built.envelope.recipients,
        [
            "ola@example.org",
            "bjorn@example.net",
            "oystein@example.org",
            "blind@example.net",
            "hemmelig@example.com"
        ]
    );
}

#[test]
fn encoding_as_s7_measured() {
    let (draft, data) = full();
    let built = build(&draft, DATE, &slices(&data)).unwrap();
    let text = String::from_utf8(built.transmitted.clone()).unwrap();
    assert!(text.contains("\r\nSubject: =?utf-8?B?"), "RFC 2047 subject");
    assert!(text.contains("name=\"=?UTF-8?B?"), "RFC 2047 name=");
    assert!(
        text.contains("filename*0*=UTF-8''%C3%85rsrapport"),
        "RFC 2231 filename*"
    );
    assert!(text.contains("Content-ID: <aaaaaaaaaaaaaaaaaaaaaaaaaa@example.com>"));
    assert!(text.contains("Content-Disposition: inline; filename*=UTF-8''bl%C3%A5b%C3%A6r.png"));
    assert!(text.contains("\r\nMessage-ID: <k3j5x8m2q9wa7b4c6d2e5f8g3h@example.com>\r\n"));
    assert!(text.contains("MIME-Version: 1.0"));
}

#[test]
fn the_plain_part_has_the_signature_separator() {
    let reply = samples().remove(4).1;
    let built = build(&reply, DATE, &[]).unwrap();
    let message = MessageParser::default().parse(&built.transmitted).unwrap();
    let text = message
        .text_part(0)
        .and_then(|p| p.text_contents())
        .unwrap();
    assert!(text.contains("\r\n-- \r\nKari"), "{text:?}");
}

#[test]
fn idn_domains_go_out_in_ascii() {
    let draft = Draft {
        from: Address::new("Ola", "ola@blåbær.example"),
        to: vec![Address::new("Kari", "kari@Øl.example")],
        message_id: "aaaaaaaaaaaaaaaaaaaaaaaaaa@xn--blbr-roah.example".into(),
        ..draft("x", "<div>x</div>")
    };
    let built = build(&draft, DATE, &[]).unwrap();
    assert_eq!(built.envelope.mail_from, "ola@xn--blbr-roah.example");
    assert_eq!(built.envelope.recipients, ["kari@xn--l-4ga.example"]);
    let headers = header_block(&built.transmitted);
    assert!(headers.contains("<kari@xn--l-4ga.example>"), "{headers}");
    assert!(built.transmitted.is_ascii());
}

#[test]
fn refuses_what_it_cant_send() {
    let base = draft("x", "<div>x</div>");
    let with = |change: &dyn Fn(&mut Draft)| {
        let mut draft = base.clone();
        change(&mut draft);
        build(&draft, DATE, &[]).err()
    };
    assert_eq!(with(&|d| d.to.clear()), Some(BuildError::NoRecipients));
    assert_eq!(
        with(&|d| d.cc = vec![Address::new("", "æ@example.org")]),
        Some(BuildError::Address(Field::Cc))
    );
    assert_eq!(
        with(&|d| d.bcc = vec![Address::new("", "not an address")]),
        Some(BuildError::Address(Field::Bcc))
    );
    assert_eq!(
        with(&|d| d.from = Address::new("", "kari@example.org")),
        Some(BuildError::MessageId),
        "a Message-ID on another identity's domain"
    );
    assert_eq!(with(&|d| d.message_id.clear()), Some(BuildError::MessageId));
    assert_eq!(
        with(&|d| d.message_id = "a b@example.com".into()),
        Some(BuildError::MessageId)
    );
    let mut parted = base.clone();
    parted.parts = vec![part("a.pdf", "application/pdf", b"abc", None)];
    assert_eq!(build(&parted, DATE, &[]).err(), Some(BuildError::Parts));
    assert_eq!(
        build(&parted, DATE, &[b"ab"]).err(),
        Some(BuildError::Parts)
    );
    assert!(build(&parted, DATE, &[b"abc"]).is_ok());
    assert_eq!(
        drafts_copy(
            &Draft {
                to: Vec::new(),
                ..base.clone()
            },
            DATE,
            &[]
        )
        .map(|raw| raw.is_empty()),
        Ok(false),
        "a draft needs no recipients"
    );
}

#[test]
fn hostile_header_text_stays_one_field() {
    let draft = Draft {
        from: Address::new("Kari\r\nBcc: x@example.org", "kari@example.com"),
        in_reply_to: Some("bad id@example.org".into()),
        references: vec!["ok@example.org".into(), "<bad>@example.org".into()],
        parts: vec![part("a\r\nb.bin", "text/plain\r\nX-Evil: 1", b"x", None)],
        ..draft("Hi\r\nBcc: evil@example.org", "<div>x</div>")
    };
    let built = build(&draft, DATE, &[b"x"]).unwrap();
    let raw = String::from_utf8(built.transmitted).unwrap();
    assert!(!raw.contains("\r\nBcc:") && !raw.contains("\r\nX-Evil"));
    assert!(raw.contains("Content-Type: application/octet-stream"));
    let message = MessageParser::default().parse(raw.as_bytes()).unwrap();
    assert_eq!(message.subject(), Some("Hi Bcc: evil@example.org"));
    assert!(message.in_reply_to().is_empty());
    assert_eq!(message.references().as_text(), Some("ok@example.org"));
}

#[test]
fn inline_parts_the_html_dropped_are_left_out() {
    let data = vec![image()];
    let draft = Draft {
        parts: vec![part(
            "x.png",
            "image/png",
            &data[0],
            Some("gone@example.com"),
        )],
        ..draft("x", "<div>no image now</div>")
    };
    let built = build(&draft, DATE, &slices(&data)).unwrap();
    let message = MessageParser::default().parse(&built.transmitted).unwrap();
    assert_eq!(
        shape(&message, 0),
        "multipart/alternative[text/plain, text/html]"
    );
}

#[test]
fn the_estimate_is_a_little_above_the_size() {
    for (name, draft, data) in samples() {
        let size = build(&draft, DATE, &slices(&data))
            .unwrap()
            .transmitted
            .len() as u64;
        let estimate = estimate(&draft);
        assert!(estimate >= size, "{name}: {estimate} < {size}");
        assert!(
            estimate <= size + size / 4 + 4_096,
            "{name}: {estimate} for {size}"
        );
    }
    let big = vec![vec![7u8; 3_000_000]];
    let draft = Draft {
        parts: vec![part("big.bin", "application/octet-stream", &big[0], None)],
        ..draft("x", "<div>x</div>")
    };
    let size = build(&draft, DATE, &slices(&big))
        .unwrap()
        .transmitted
        .len() as u64;
    let estimate = estimate(&draft);
    assert!(
        estimate >= size && estimate - size < 8_192,
        "{estimate} for {size}"
    );
}

#[test]
fn a_planned_reply_builds_and_reads_back_as_a_draft() {
    let raw = "From: Marius Dahl <marius@example.org>\r\nTo: Kari <kari@example.com>\r\n\
               Subject: Møte\r\nMessage-ID: <m1@example.org>\r\nDate: Sun, 04 Oct 2026 07:50:00 +0200\r\n\
               Content-Type: text/html; charset=utf-8\r\n\r\n<p>Kommer du?</p><script>x()</script>\r\n";
    let senders = vec![crate::account::Identity {
        name: "Kari Nordmann".into(),
        address: "kari@example.com".into(),
        signature: Some("Kari".into()),
    }];
    let plan = reply::plan(
        &reply::Original::parse(raw.as_bytes()),
        ReplyKind::Reply,
        &senders,
    );
    let draft = Draft {
        bcc: vec![Address::new("Arkiv", "arkiv@example.com")],
        ..plan.draft(
            AccountId(1),
            StoredId(2),
            ID.into(),
            "<div>Ja!</div>",
            Vec::new(),
        )
    };
    let built = build(&draft, DATE, &[]).unwrap();
    check(&built.transmitted, Which::Transmitted, &draft, &[]);
    let saved = drafts_copy(&draft, DATE, &[]).unwrap();
    let back = stored::parse(&saved).unwrap();
    assert_eq!(back.from.as_ref(), Some(&draft.from));
    assert_eq!(
        (back.to.clone(), back.bcc.clone()),
        (draft.to.clone(), draft.bcc.clone())
    );
    assert_eq!(back.subject, "Re: Møte");
    assert_eq!(back.message_id.as_deref(), Some(ID));
    assert_eq!(back.in_reply_to.as_deref(), Some("m1@example.org"));
    assert_eq!(back.references, ["m1@example.org"]);
    assert_eq!(
        back.html, draft.html,
        "the cleaner keeps what the composer writes"
    );
}

#[test]
fn a_stored_draft_keeps_its_parts() {
    let (draft, data) = full();
    let saved = drafts_copy(&draft, DATE, &slices(&data)).unwrap();
    let back = stored::parse(&saved).unwrap();
    assert_eq!(back.bcc, draft.bcc);
    assert_eq!(back.cc, draft.cc);
    assert_eq!(back.subject, NORWEGIAN_SUBJECT);
    let parts: Vec<(&str, &str, Option<&str>, &[u8])> = back
        .parts
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                p.mime_type.as_str(),
                p.content_id.as_deref(),
                p.data.as_slice(),
            )
        })
        .collect();
    let expected: Vec<(&str, &str, Option<&str>, &[u8])> = draft
        .parts
        .iter()
        .zip(&data)
        .map(|(p, bytes)| {
            (
                p.name.as_str(),
                p.mime_type.as_str(),
                p.content_id.as_deref(),
                bytes.as_slice(),
            )
        })
        .collect();
    assert_eq!(parts, expected);
    let again = back.draft(
        AccountId(1),
        back.from.clone().unwrap(),
        back.message_id.clone().unwrap(),
        back.parts
            .iter()
            .map(|p| p.draft_part(BlobKey([2; 32])))
            .collect(),
    );
    assert_eq!(again.parts.len(), 3);
    assert!(build(&again, DATE, &slices(&data)).is_ok());
    let printed = format!("{back:?} {:?}", back.parts[0]);
    assert!(
        !printed.contains("blåbær") && !printed.contains("Kari"),
        "{printed}"
    );
}

#[test]
fn debug_prints_no_content() {
    let (draft, data) = full();
    let built = build(&draft, DATE, &slices(&data)).unwrap();
    let printed = format!("{built:?}");
    assert!(!printed.contains('@'), "{printed}");
    assert!(printed.contains("recipients: 5"));
}

fn arbitrary_name() -> impl Strategy<Value = String> {
    "[A-Za-z0-9æøåÆØÅ🙂 ,.;:\"()<>@\\\\\\[\\]=?_-]{0,30}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn whatever_is_typed_round_trips(
        subject in "\\PC{0,120}",
        name in arbitrary_name(),
        body in "\\PC{0,300}",
        bcc in proptest::bool::ANY,
    ) {
        let draft = Draft {
            from: Address::new(name.clone(), "kari@example.com"),
            to: vec![Address::new(name, "ola@example.org")],
            bcc: if bcc { vec![Address::new("B", "b@example.net")] } else { Vec::new() },
            ..draft(&subject, &crate::compose::text::to_html(&body))
        };
        let built = build(&draft, DATE, &[]).unwrap();
        check(&built.transmitted, Which::Transmitted, &draft, &[]);
        check(&built.sent_copy, Which::Sent, &draft, &[]);
    }
}
