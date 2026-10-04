//! Synthetic messages for the reader's self-tests and examples: plain text, simple HTML, a
//! styled newsletter, an inline `cid:` image, attachments, remote images and a Latin-1 body.
//! Every name and address is fictional (`example.test`). Remote images point at port 2 of
//! 127.0.0.1, where nothing listens, so even "Show images" never leaves the machine. (Not port
//! 9: it is on the Fetch standard's list of blocked ports, so WebKit wouldn't even try it.)

use gtk4::glib;
use sealwax_domain::model::{FolderId, FolderRole, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::ingest::{self, IngestError, RawMessage};
use sealwax_infrastructure::store::{FolderSpec, NewAccount, Store};

pub const ACCOUNT_NAME: &str = "Personal";
pub const ACCOUNT_ADDRESS: &str = "alex.berg@example.test";
const ME: (&str, &str) = ("Alex Berg", ACCOUNT_ADDRESS);
/// Where the samples' remote images point: a closed port on this machine.
pub const REMOTE_ORIGIN: &str = "https://127.0.0.1:2";
const CRLF: &str = "\r\n";

/// A 120 × 40 PNG, the newsletter's and the `cid` sample's logo.
pub const LOGO_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAHgAAAAoCAIAAAC6iKlyAAAAvklEQVR42u3asQ2DQAwFUCaBJTIOO6RnQsbICFQUVGmRkhwHRqdwPOn3ll5l2b/pnr0USIMANGgBXQn09BrTARqF3iTGHYVeCw7tIx3cB6Hzib9y882CPqbMeh90RJl1LnRcmfUO6KDy2vpzxDJPtSYX+kTlhDVo0EWgT1f+ZQ0adF3Qd986QIMGDRo0aFsHaNCgQbt1gHaPLnSPBu3D4mfoC075f3sdV98iLtNUAl2oewdam1Q/GrSABn2zvAGUnvlpkRJDMQAAAABJRU5ErkJggg==";

/// One MIME part beside the body: inline (with a Content-ID) or an attachment.
#[derive(Debug, Clone)]
pub struct Part {
    pub mime_type: String,
    pub name: Option<String>,
    pub content_id: Option<String>,
    pub data: Vec<u8>,
}

impl Part {
    pub fn inline(content_id: &str, mime_type: &str, data: Vec<u8>) -> Self {
        Self {
            mime_type: mime_type.to_owned(),
            name: None,
            content_id: Some(content_id.to_owned()),
            data,
        }
    }

    pub fn attachment(name: &str, mime_type: &str, data: Vec<u8>) -> Self {
        Self {
            mime_type: mime_type.to_owned(),
            name: Some(name.to_owned()),
            content_id: None,
            data,
        }
    }
}

/// A message to render as RFC 5322 bytes.
#[derive(Debug, Clone)]
pub struct Draft {
    pub from: (String, String),
    pub to: Vec<(String, String)>,
    pub cc: Vec<(String, String)>,
    pub subject: String,
    pub date: i64,
    pub message_id: String,
    pub text: Option<String>,
    pub html: Option<String>,
    /// The HTML body's charset; its text is encoded in it (`utf-8` or `iso-8859-1`).
    pub html_charset: &'static str,
    pub inline: Vec<Part>,
    pub attachments: Vec<Part>,
}

impl Draft {
    pub fn new(from: (&str, &str), subject: &str, date: i64, id: &str) -> Self {
        Self {
            from: (from.0.to_owned(), from.1.to_owned()),
            to: vec![(ME.0.to_owned(), ME.1.to_owned())],
            cc: Vec::new(),
            subject: subject.to_owned(),
            date,
            message_id: format!("{id}@example.test"),
            text: None,
            html: None,
            html_charset: "utf-8",
            inline: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn render(&self) -> Vec<u8> {
        let mut out = String::new();
        let mailboxes = |list: &[(String, String)]| {
            list.iter()
                .map(|(name, address)| mailbox(name, address))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!(
            "From: {}{CRLF}",
            mailbox(&self.from.0, &self.from.1)
        ));
        if !self.to.is_empty() {
            out.push_str(&format!("To: {}{CRLF}", mailboxes(&self.to)));
        }
        if !self.cc.is_empty() {
            out.push_str(&format!("Cc: {}{CRLF}", mailboxes(&self.cc)));
        }
        out.push_str(&format!("Subject: {}{CRLF}", encode_words(&self.subject)));
        out.push_str(&format!("Date: {}{CRLF}", rfc5322_date(self.date)));
        out.push_str(&format!("Message-ID: <{}>{CRLF}", self.message_id));
        out.push_str(&format!("MIME-Version: 1.0{CRLF}"));
        let text = self.text.as_deref().map(|text| {
            leaf(
                "text/plain; charset=utf-8",
                "base64",
                &base64_lines(text.as_bytes()),
                &[],
            )
        });
        let html = self.html.as_deref().map(|html| {
            let bytes = encode(html, self.html_charset);
            let html = leaf(
                &format!("text/html; charset={}", self.html_charset),
                "base64",
                &base64_lines(&bytes),
                &[],
            );
            if self.inline.is_empty() {
                html
            } else {
                let mut parts = vec![html];
                parts.extend(self.inline.iter().map(|part| part_text(part, true)));
                multipart("related", &self.boundary("rel"), &parts)
            }
        });
        let body = match (text, html) {
            (Some(text), Some(html)) => {
                multipart("alternative", &self.boundary("alt"), &[text, html])
            }
            (Some(body), None) | (None, Some(body)) => body,
            (None, None) => leaf("text/plain; charset=utf-8", "7bit", "", &[]),
        };
        let body = if self.attachments.is_empty() {
            body
        } else {
            let mut parts = vec![body];
            parts.extend(self.attachments.iter().map(|part| part_text(part, false)));
            multipart("mixed", &self.boundary("mix"), &parts)
        };
        out.push_str(&body);
        out.into_bytes()
    }

    fn boundary(&self, kind: &str) -> String {
        format!(
            "=_sealwax_{kind}_{}",
            self.message_id.replace(['@', '.'], "_")
        )
    }
}

fn encode(text: &str, charset: &str) -> Vec<u8> {
    if charset.eq_ignore_ascii_case("iso-8859-1") {
        text.chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect()
    } else {
        text.as_bytes().to_vec()
    }
}

fn mailbox(name: &str, address: &str) -> String {
    if name.is_empty() {
        address.to_owned()
    } else if name.is_ascii() {
        format!("\"{name}\" <{address}>")
    } else {
        format!("{} <{address}>", encode_words(name))
    }
}

/// RFC 2047 for non-ASCII header text.
fn encode_words(text: &str) -> String {
    if text.is_ascii() {
        text.to_owned()
    } else {
        format!("=?utf-8?b?{}?=", glib::base64_encode(text.as_bytes()))
    }
}

fn leaf(content_type: &str, encoding: &str, body: &str, extra: &[String]) -> String {
    let mut out = format!("Content-Type: {content_type}{CRLF}");
    for line in extra {
        out.push_str(line);
        out.push_str(CRLF);
    }
    out.push_str(&format!(
        "Content-Transfer-Encoding: {encoding}{CRLF}{CRLF}{body}"
    ));
    if !out.ends_with(CRLF) {
        out.push_str(CRLF);
    }
    out
}

fn part_text(part: &Part, inline: bool) -> String {
    let mut extra = Vec::new();
    if let Some(id) = &part.content_id {
        extra.push(format!("Content-ID: <{id}>"));
    }
    let disposition = if inline { "inline" } else { "attachment" };
    match &part.name {
        Some(name) => extra.push(format!(
            "Content-Disposition: {disposition}; filename=\"{name}\""
        )),
        None => extra.push(format!("Content-Disposition: {disposition}")),
    }
    let content_type = match &part.name {
        Some(name) => format!("{}; name=\"{name}\"", part.mime_type),
        None => part.mime_type.clone(),
    };
    leaf(&content_type, "base64", &base64_lines(&part.data), &extra)
}

fn multipart(subtype: &str, boundary: &str, parts: &[String]) -> String {
    let mut out = format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"{CRLF}{CRLF}");
    for part in parts {
        out.push_str(&format!("--{boundary}{CRLF}{part}"));
    }
    out.push_str(&format!("--{boundary}--{CRLF}"));
    out
}

fn base64_lines(bytes: &[u8]) -> String {
    let encoded = glib::base64_encode(bytes);
    encoded
        .as_bytes()
        .chunks(76)
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect::<Vec<_>>()
        .join(CRLF)
}

/// `Sun, 04 Oct 2026 10:00:00 +0000`, in English whatever the locale.
pub fn rfc5322_date(time: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = time.div_euclid(86_400);
    let seconds = time.rem_euclid(86_400);
    let weekday = DAYS[days.rem_euclid(7) as usize];
    let (year, month, day) = civil(days);
    format!(
        "{weekday}, {day:02} {} {year} {:02}:{:02}:{:02} +0000",
        MONTHS[month as usize - 1],
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Year, month and day of a day number counted from 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let of_era = days - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A sample message by name.
#[derive(Debug, Clone)]
pub struct Sample {
    pub name: &'static str,
    pub draft: Draft,
}

/// The sample names, in the order [`all`] returns them.
pub const NAMES: [&str; 9] = [
    "plain",
    "simple",
    "newsletter",
    "cid",
    "attachments",
    "remote",
    "latin1",
    "short",
    "dark-promo",
];

/// Every sample, dated a little before `now`.
pub fn all(now: i64) -> Vec<Sample> {
    NAMES
        .iter()
        .enumerate()
        .map(|(index, name)| Sample {
            name,
            draft: draft(name, now - 600 - 3_300 * index as i64),
        })
        .collect()
}

fn draft(name: &str, date: i64) -> Draft {
    match name {
        "plain" => {
            let mut draft = Draft::new(
                ("Ingrid Solberg", "ingrid@example.test"),
                "Cabin weekend — final plan",
                date,
                "sample-plain",
            );
            draft.cc = vec![("Tomas Berg".into(), "tomas@example.test".into())];
            draft.text = Some(
                "Hi Alex,\n\nI've booked the cabin in Hemsedal for the 17th–19th. It sleeps six, \
                 so there's room for Tomas and Sana too.\n\nThe road up is fine without chains \
                 until November. Bring wool socks; the stove takes a while.\n\n> Do we need to \
                 bring bedding?\n\nNo, it's all there.\n\nIngrid\n"
                    .into(),
            );
            draft
        }
        "simple" => {
            let mut draft = Draft::new(
                ("Marius Dahl", "marius@example.test"),
                "Re: Quarterly report draft",
                date,
                "sample-simple",
            );
            draft
                .to
                .push(("Leah Kim".into(), "leah@example.test".into()));
            draft.text = Some("Looks good. Two small comments on page 4.\n".into());
            draft.html = Some(
                "<div dir=\"ltr\"><div>Looks good. Two small comments:</div><ul>\
                 <li>Page 4: the churn chart uses last year's scale.</li>\
                 <li>The summary could lead with the support numbers.</li></ul>\
                 <div>The draft is on <a href=\"https://example.test/reports/q3\">the shared \
                 drive</a>. Otherwise ready to send on Thursday.</div><div><br></div>\
                 <div>Marius</div><br><div class=\"gmail_quote\"><div class=\"gmail_attr\">On \
                 Fri, Alex Berg wrote:</div><blockquote class=\"gmail_quote\" style=\"margin:0 0 \
                 0 .8ex;border-left:1px solid #ccc;padding-left:1ex\">Here's the first draft of \
                 the quarterly report.</blockquote></div></div>"
                    .into(),
            );
            draft
        }
        "newsletter" => {
            let mut draft = Draft::new(
                ("Northwind Weekly", "news@northwind.example.test"),
                "This week in design systems",
                date,
                "sample-newsletter",
            );
            draft.html = Some(newsletter());
            draft.inline = vec![logo("logo@northwind.example.test")];
            draft
        }
        "cid" => {
            let mut draft = Draft::new(
                ("Fjord Cycling Club", "rides@fjordcycling.example.test"),
                "Saturday ride: route and coffee stop",
                date,
                "sample-cid",
            );
            draft.html = Some(
                "<p>Hi riders,</p><p>We meet at 09:00 by the fountain. 62 km, mostly flat, with \
                 coffee in Drøbak at the halfway point.</p>\
                 <p><img src=\"cid:route@fjordcycling.example.test\" alt=\"Club logo\" \
                 width=\"120\" height=\"40\"></p><p>— Kari, ride captain</p>"
                    .into(),
            );
            draft.inline = vec![logo("route@fjordcycling.example.test")];
            draft
        }
        "attachments" => {
            let mut draft = Draft::new(
                ("Leah Kim", "leah@studio-kim.example.test"),
                "Invoice #2041",
                date,
                "sample-attachments",
            );
            draft.text = Some(
                "Hi,\n\nAttached is the invoice for September, the hours as a spreadsheet and \
                 the photo from the workshop.\n\nLeah\n"
                    .into(),
            );
            draft.attachments = vec![
                Part::attachment(
                    "Invoice-2041.pdf",
                    "application/pdf",
                    filler(b"%PDF-1.4\n", 212_000),
                ),
                Part::attachment(
                    "hours-september.csv",
                    "text/csv",
                    filler(b"date,hours\n", 2_400),
                ),
                Part::attachment(
                    "workshop.png",
                    "image/png",
                    glib::base64_decode(LOGO_PNG_BASE64),
                ),
            ];
            draft
        }
        "remote" => {
            let mut draft = Draft::new(
                ("Aker Library", "noreply@akerlibrary.example.test"),
                "Your reservation is ready",
                date,
                "sample-remote",
            );
            draft.html = Some(format!(
                "<p>Hello,</p><p>'The Lighthouse Keeper's Atlas' is ready for pickup at the main \
                 desk until Friday. <a id=\"renew\" href=\"{REMOTE_ORIGIN}/library/renew\">Renew \
                 online</a>.</p><p><img src=\"{REMOTE_ORIGIN}/library/cover.png\" \
                 width=\"160\" height=\"220\" alt=\"Book cover\"></p><p>Aker Library</p>"
            ));
            draft
        }
        "latin1" => {
            let mut draft = Draft::new(
                ("Åse Nygård", "ase@example.test"),
                "Blåbærsyltetøy til helga",
                date,
                "sample-latin1",
            );
            draft.html_charset = "iso-8859-1";
            draft.html = Some(
                "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; \
                 charset=iso-8859-1\"></head><body><p>Hei!</p><p>Jeg har laget \
                 blåbærsyltetøy, og det er nok til alle på hytta. Kåre tar med vafler.</p>\
                 <p>Hilsen Åse</p></body></html>"
                    .into(),
            );
            draft
        }
        "short" => {
            let mut draft = Draft::new(
                ("Tomas Berg", "tomas@example.test"),
                "Re: Lunch Thursday?",
                date,
                "sample-short",
            );
            draft.text = Some("Sounds good, see you at noon by the fountain.\n".into());
            draft
        }
        _ => {
            let mut draft = Draft::new(
                ("Ledger Store", "offers@ledger.example.test"),
                "Night owl sale: 30 % off",
                date,
                "sample-dark-promo",
            );
            draft.html = Some(
                "<!DOCTYPE html><html><body style=\"margin:0;background:#101418;color:#e8e3d9\">\
                 <table role=\"presentation\" width=\"100%\" bgcolor=\"#101418\"><tr><td \
                 align=\"center\" style=\"padding:32px\"><h1 style=\"font:bold 28px \
                 Georgia,serif;color:#f2c14e;margin:0 0 12px\">Night owl sale</h1><p \
                 style=\"font:16px/1.5 sans-serif;margin:0 0 20px\">Everything with a moon \
                 on it is 30 % off until Sunday.</p><a href=\"https://example.test/sale\" \
                 style=\"display:inline-block;padding:10px 18px;background:#f2c14e;\
                 color:#101418;text-decoration:none;font:bold 14px sans-serif\">See the \
                 sale</a></td></tr></table></body></html>"
                    .into(),
            );
            draft
        }
    }
}

fn logo(content_id: &str) -> Part {
    Part::inline(
        content_id,
        "image/png",
        glib::base64_decode(LOGO_PNG_BASE64),
    )
}

fn filler(head: &[u8], size: usize) -> Vec<u8> {
    let mut data = head.to_vec();
    while data.len() < size {
        data.extend_from_slice(b"sealwax sample bytes\n");
    }
    data.truncate(size);
    data
}

fn newsletter() -> String {
    let mut html = String::from(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         body{margin:0;background:#f4f1ea} a{color:#1f6f5c}\
         .wrap{max-width:600px;margin:0 auto}</style></head><body>\
         <table role=\"presentation\" width=\"100%\" bgcolor=\"#f4f1ea\" cellpadding=\"0\" \
         cellspacing=\"0\"><tr><td align=\"center\" style=\"padding:16px 0\">\
         <table role=\"presentation\" class=\"wrap\" width=\"600\" bgcolor=\"#ffffff\" \
         cellpadding=\"0\" cellspacing=\"0\">\
         <tr><td style=\"padding:20px 28px;background:#1f6f5c;color:#ffffff;\
         font:bold 20px Georgia,serif\"><img src=\"cid:logo@northwind.example.test\" \
         alt=\"Northwind\" width=\"120\" height=\"40\" style=\"display:block\"></td></tr>",
    );
    html.push_str(&format!(
        "<tr><td style=\"padding:20px 28px 0\"><img src=\"{REMOTE_ORIGIN}/northwind/hero.jpg\" \
         alt=\"\" width=\"544\" height=\"150\" style=\"display:block;background:#e7e2d8\"></td></tr>"
    ));
    for (heading, text) in [
        (
            "Tokens are having a moment",
            "This week we look at how three small teams keep colour, type and spacing in one \
             place, and what broke when they tried to support dark mode.",
        ),
        (
            "Twelve tiny utilities",
            "Small helpers for spacing, focus rings and truncation that pay for themselves \
             within a week.",
        ),
        (
            "Reader question: tinted surfaces",
            "How much contrast do you lose on a tinted card, and how do you test it?",
        ),
    ] {
        html.push_str(&format!(
            "<tr><td style=\"padding:20px 28px 0;font:16px/1.55 Georgia,serif;color:#222222\">\
             <h2 style=\"margin:0 0 8px;font:bold 19px Helvetica,Arial,sans-serif;\
             color:#1f6f5c\">{heading}</h2><p style=\"margin:0 0 12px\">{text}</p>\
             <a href=\"https://example.test/northwind/214\" style=\"display:inline-block;\
             padding:8px 16px;background:#1f6f5c;color:#ffffff;text-decoration:none;\
             font:bold 14px Helvetica,Arial,sans-serif\">Read more</a></td></tr>"
        ));
    }
    html.push_str(&format!(
        "<tr><td style=\"padding:24px 28px;font:12px/1.5 Helvetica,Arial,sans-serif;\
         color:#777777\">You receive this because you subscribed to Northwind Weekly. \
         <a href=\"https://example.test/northwind/unsubscribe\" style=\"color:#777777\">\
         Unsubscribe</a><img src=\"{REMOTE_ORIGIN}/northwind/open.gif\" width=\"1\" \
         height=\"1\" alt=\"\"></td></tr></table></td></tr></table></body></html>"
    ));
    html
}

/// The INBOX the samples went into, and each sample's id.
#[derive(Debug, Clone)]
pub struct Filled {
    pub inbox: FolderId,
    pub ids: Vec<(&'static str, MessageId)>,
}

impl Filled {
    pub fn id(&self, name: &str) -> Option<MessageId> {
        self.ids
            .iter()
            .find(|(sample, _)| *sample == name)
            .map(|(_, id)| *id)
    }
}

/// Adds the sample account and its INBOX, and ingests `messages` (name and raw bytes) through
/// `ingest_raw`. Blocks: run it off the GTK thread.
pub fn fill_store(
    store: &Store,
    blobs: &BlobStore,
    messages: Vec<(&'static str, Vec<u8>)>,
) -> Result<Filled, IngestError> {
    let account = store
        .add_account(NewAccount {
            name: ACCOUNT_NAME.into(),
            address: ACCOUNT_ADDRESS.into(),
        })
        .recv_blocking()?
        .value;
    let inbox = store
        .upsert_folder(FolderSpec::local(account, "INBOX", FolderRole::Inbox))
        .recv_blocking()?
        .value;
    let names: Vec<&'static str> = messages.iter().map(|(name, _)| *name).collect();
    let raw = messages
        .into_iter()
        .map(|(_, raw)| RawMessage {
            folder: inbox,
            raw,
            ..RawMessage::default()
        })
        .collect();
    let ingested = ingest::ingest_raw(store, blobs, raw)?;
    let ids = names
        .into_iter()
        .zip(ingested.ids)
        .filter_map(|(name, id)| id.map(|id| (name, id)))
        .collect();
    Ok(Filled { inbox, ids })
}

/// The samples as `fill_store` takes them.
pub fn messages(now: i64) -> Vec<(&'static str, Vec<u8>)> {
    all(now)
        .into_iter()
        .map(|sample| (sample.name, sample.draft.render()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::message;
    use sealwax_domain::model::BodyKind;

    #[test]
    fn dates_are_rfc5322() {
        assert_eq!(rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(
            rfc5322_date(1_791_115_200),
            "Sun, 04 Oct 2026 12:00:00 +0000"
        );
        assert_eq!(rfc5322_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 +0000");
    }

    #[test]
    fn samples_parse_as_intended() {
        let now = 1_791_115_200;
        for sample in all(now) {
            let raw = sample.draft.render();
            let parsed = message::parse(&raw);
            let name = sample.name;
            assert_eq!(parsed.headers.from[0].email, sample.draft.from.1, "{name}");
            assert_eq!(parsed.headers.subject, sample.draft.subject, "{name}");
            assert!(parsed.headers.date.is_some(), "{name}");
            let kind = match name {
                "plain" | "attachments" | "short" => BodyKind::Plain,
                "simple" => BodyKind::Both,
                _ => BodyKind::Html,
            };
            assert_eq!(parsed.body_kind, kind, "{name}");
            match name {
                "cid" | "newsletter" => {
                    let embedded: Vec<_> = parsed
                        .attachments
                        .iter()
                        .filter(|attachment| attachment.is_embedded())
                        .collect();
                    assert_eq!(embedded.len(), 1, "{name}");
                    let id = embedded[0].content_id.as_deref().unwrap();
                    let part = message::part_by_content_id(&raw, id).unwrap();
                    assert_eq!(part.data, glib::base64_decode(LOGO_PNG_BASE64).to_vec());
                }
                "attachments" => {
                    assert_eq!(parsed.attachments.len(), 3);
                    assert!(parsed.has_attachments());
                    assert_eq!(parsed.attachments[0].size, 212_000);
                }
                "latin1" => {
                    let html = message::part(&raw, parsed.html_parts[0]).unwrap();
                    assert!(html.text.unwrap().contains("blåbærsyltetøy"));
                    assert_eq!(parsed.headers.from[0].name, "Åse Nygård");
                }
                _ => {}
            }
            let bodies = [&sample.draft.html, &sample.draft.text];
            for body in bodies.into_iter().flatten() {
                for host in body.split("://").skip(1) {
                    assert!(
                        host.starts_with("example.test") || host.starts_with("127.0.0.1:2/"),
                        "{name}: {}",
                        &host[..host.len().min(30)]
                    );
                }
            }
        }
    }
}
