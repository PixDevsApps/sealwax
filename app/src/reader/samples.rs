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

/// The files-and-remote-content samples (M5: ML-058, ML-061, ML-063), beside [`NAMES`]:
/// attachments the desktop must never open, one whose content is a script though its name
/// says text, a second message from the remote sample's sender, and a message with
/// attachments that the self-test stores as its text parts, as a large one is.
pub const FILES_NAMES: [&str; 4] = [
    "files-danger",
    "files-sniffed",
    "files-remote-again",
    "files-large",
];

/// A files sample by name, dated `date`; also any of [`NAMES`].
pub fn files_draft(name: &str, date: i64) -> Option<Draft> {
    if NAMES.contains(&name) {
        return Some(draft(name, date));
    }
    let draft = match name {
        "files-danger" => {
            let mut draft = Draft::new(
                ("Mira Holm", "mira@holm-trading.example.test"),
                "Your order documents",
                date,
                "files-danger",
            );
            draft.text = Some(
                "Hello,\n\nThe documents for order 7731 are attached. Install the viewer \
                 first.\n\nMira\n"
                    .into(),
            );
            draft.attachments = vec![
                Part::attachment(
                    "agenda.pdf",
                    "application/pdf",
                    filler(b"%PDF-1.4\n", 24_000),
                ),
                Part::attachment(
                    "invoice.pdf.desktop",
                    "application/x-desktop",
                    b"[Desktop Entry]\nType=Application\nName=Invoice\nExec=true\n".to_vec(),
                ),
                Part::attachment(
                    "Viewer\u{202E}fdp.EXE",
                    "application/octet-stream",
                    filler(b"MZ\x90\x00", 4_096),
                ),
                Part::attachment(
                    "scan\u{2024}pdf",
                    "application/octet-stream",
                    b"A fictional scan.\n".to_vec(),
                ),
            ];
            draft
        }
        "files-sniffed" => {
            let mut draft = Draft::new(
                ("Jonas Lie", "jonas@example.test"),
                "Notes from the workshop",
                date,
                "files-sniffed",
            );
            draft.text = Some("Hi,\n\nMy notes are attached.\n\nJonas\n".into());
            draft.attachments = vec![Part::attachment(
                "notes.txt",
                "text/plain",
                b"#!/bin/sh\necho 'a fictional script'\n".to_vec(),
            )];
            draft
        }
        "files-remote-again" => {
            let mut draft = Draft::new(
                ("Aker Library", "noreply@akerlibrary.example.test"),
                "Your loan is due on Friday",
                date,
                "files-remote-again",
            );
            draft.html = Some(format!(
                "<p>Hello,</p><p>'A Field Guide to Fjords' is due on Friday. You can renew it \
                 once online.</p><p><img src=\"{REMOTE_ORIGIN}/library/due.png\" width=\"120\" \
                 height=\"80\" alt=\"Library card\"></p><p>Aker Library</p>"
            ));
            draft
        }
        "files-large" => {
            let mut draft = Draft::new(
                ("Board Office", "board@fjordhavn.example.test"),
                "Board pack for October",
                date,
                "files-large",
            );
            draft.text = Some(
                "Dear members,\n\nThe board pack and last month's minutes are attached.\n\n\
                 The Board Office\n"
                    .into(),
            );
            draft.attachments = vec![
                Part::attachment(
                    "board-pack-october.pdf",
                    "application/pdf",
                    filler(b"%PDF-1.4\n", 4_400_000),
                ),
                Part::attachment(
                    "minutes-september.txt",
                    "text/plain",
                    filler(b"Minutes\n", 12_000),
                ),
            ];
            draft
        }
        _ => return None,
    };
    Some(draft)
}

/// `drafts`, oldest first, each with whether it is read, as one conversation: each answers the
/// one before it, named `name-1`, `name-2`, …
pub fn files_conversation(name: &str, drafts: Vec<(Draft, bool)>) -> Vec<Threaded> {
    thread(
        name,
        drafts
            .into_iter()
            .map(|(draft, read)| (draft, read, false))
            .collect(),
    )
}

/// The newsletter sender of [`files_thread`] and the café that answers in it.
pub const FILES_THREAD_SENDER: (&str, &str) = ("Fjord Weekly", "news@fjordweekly.example.test");
pub const FILES_THREAD_OTHER: (&str, &str) = ("Harbour Café", "hello@harbourcafe.example.test");

/// `files-thread`, a conversation with remote images in every message (ML-058): the sender's
/// first issue, read (a collapsed row); its second issue, unread; the café's answer, unread;
/// the sender's third issue, unread and latest. The images point at [`REMOTE_ORIGIN`].
pub fn files_thread(now: i64) -> Vec<Threaded> {
    let issue = |from: (&str, &str), number: usize, date: i64| {
        let subject = if number == 1 {
            "Fjord Weekly: the autumn issue".to_owned()
        } else {
            "Re: Fjord Weekly: the autumn issue".to_owned()
        };
        let mut draft = Draft::new(from, &subject, date, &format!("files-thread-{number}"));
        draft.html = Some(format!(
            "<p>{} {number}.</p><p><img src=\"{REMOTE_ORIGIN}/thread/{number}.png\" \
             width=\"120\" height=\"40\" alt=\"Picture {number}\"></p><p>A fictional \
             message.</p>",
            from.0
        ));
        draft
    };
    files_conversation(
        "files-thread",
        vec![
            (issue(FILES_THREAD_SENDER, 1, now - 4 * 3_600), true),
            (issue(FILES_THREAD_SENDER, 2, now - 3 * 3_600), false),
            (issue(FILES_THREAD_OTHER, 3, now - 2 * 3_600), false),
            (issue(FILES_THREAD_SENDER, 4, now - 3_600), false),
        ],
    )
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

// Conversations (ML-057): threaded samples for the reader's conversation view, its self-tests,
// previews and the memory harness. Every message replies to the one before it (In-Reply-To and
// References), so the store files them as one conversation.

/// One message of a sample conversation, rendered.
#[derive(Debug, Clone)]
pub struct Threaded {
    pub name: String,
    pub raw: Vec<u8>,
    /// Already read.
    pub read: bool,
    /// The user's own message, filed in Sent.
    pub sent: bool,
}

/// The sample conversations: `trip` (six messages, plain and simple HTML, one with
/// attachments), `letters` (eight styled newsletters), `long` (150 short messages), `typical`
/// (ten: six plain or simple HTML, three styled newsletters, one with attachments).
pub const CONVERSATIONS: [&str; 5] = ["trip", "letters", "long", "typical", "links"];

/// The messages of conversation `name`, oldest first, the latest dated a little before `now`.
pub fn conversation(name: &str, now: i64) -> Vec<Threaded> {
    match name {
        "trip" => trip(now),
        "letters" => letters(now),
        "long" => long(now),
        "links" => links(now),
        _ => typical(now),
    }
}

const INGRID: (&str, &str) = ("Ingrid Solberg", "ingrid@example.test");
const TOMAS: (&str, &str) = ("Tomas Berg", "tomas@example.test");
const SANA: (&str, &str) = ("Sana Malik", "sana@example.test");
const MARIUS: (&str, &str) = ("Marius Dahl", "marius@example.test");

/// `drafts` as one conversation: each answers the one before it, the root first.
fn thread(name: &str, drafts: Vec<(Draft, bool, bool)>) -> Vec<Threaded> {
    let mut ids: Vec<String> = Vec::new();
    drafts
        .into_iter()
        .enumerate()
        .map(|(index, (draft, read, sent))| {
            let mut raw = String::from_utf8_lossy(&draft.render()).into_owned();
            if let Some(parent) = ids.last() {
                let references: Vec<String> = ids.iter().map(|id| format!("<{id}>")).collect();
                let marker = format!("Message-ID: <{}>{CRLF}", draft.message_id);
                let threading = format!(
                    "{marker}In-Reply-To: <{parent}>{CRLF}References: {}{CRLF}",
                    references.join(" ")
                );
                raw = raw.replacen(&marker, &threading, 1);
            }
            ids.push(draft.message_id.clone());
            Threaded {
                name: format!("{name}-{}", index + 1),
                raw: raw.into_bytes(),
                read,
                sent,
            }
        })
        .collect()
}

fn reply(from: (&str, &str), subject: &str, date: i64, id: &str, text: &str) -> Draft {
    let mut draft = Draft::new(from, subject, date, id);
    if from.1 == ME.1 {
        draft.to = vec![(INGRID.0.into(), INGRID.1.into())];
    }
    draft.text = Some(text.into());
    draft
}

fn trip(now: i64) -> Vec<Threaded> {
    let subject = "Cabin trip in November?";
    let at = |hours: i64| now - hours * 3_600;
    let mut sana = Draft::new(SANA, &format!("Re: {subject}"), at(30), "trip-4");
    sana.html = Some(
        "<div>Count me in! I can drive from Oslo on Friday afternoon and bring:</div><ul>\
         <li>the big pot for soup</li><li>two pairs of spare skis</li></ul>\
         <div>— Sana</div>"
            .into(),
    );
    let mut plan = reply(
        INGRID,
        &format!("Re: {subject}"),
        at(5),
        "trip-5",
        "Hi all,\n\nThe cabin is booked for the 14th–16th. The map and the packing list are \
         attached; the key is under the green bucket by the woodshed.\n\nIngrid\n",
    );
    plan.attachments = vec![
        Part::attachment(
            "cabin-map.png",
            "image/png",
            picture(320, 180, [40, 90, 70], [220, 200, 150]),
        ),
        Part::attachment(
            "packing-list.pdf",
            "application/pdf",
            filler(b"%PDF-1.4\n", 84_000),
        ),
    ];
    let re = format!("Re: {subject}");
    thread(
        "trip",
        vec![
            (
                reply(
                    INGRID,
                    subject,
                    at(52),
                    "trip-1",
                    "Hi everyone,\n\nShall we take the cabin in Hemsedal again in November? \
                     Mid-month works best for me.\n\nIngrid\n",
                ),
                true,
                false,
            ),
            (
                reply(
                    TOMAS,
                    &re,
                    at(49),
                    "trip-2",
                    "Yes please. Any weekend but the first.\n",
                ),
                true,
                false,
            ),
            (
                reply(
                    ME,
                    &re,
                    at(40),
                    "trip-3",
                    "Sounds great — I'll bring the board games and the coffee grinder.\n",
                ),
                true,
                true,
            ),
            (sana, true, false),
            (plan, false, false),
            (
                reply(
                    TOMAS,
                    &re,
                    at(1),
                    "trip-6",
                    "Perfect. I'll pick up firewood on the way; see you Friday.\n",
                ),
                false,
                false,
            ),
        ],
    )
}

/// Links and quoted history for the keyboard (M9, ML-092): a plain-text reply with three
/// links and its quote collapsed behind "…", after an HTML one with a link whose text names
/// another address and a collapsed Gmail quote.
fn links(now: i64) -> Vec<Threaded> {
    let subject = "Workshop sign-up";
    let re = format!("Re: {subject}");
    let at = |hours: i64| now - hours * 3_600;
    let mut form = Draft::new(SANA, &re, at(3), "links-5");
    form.text = Some("See the HTML part.\n".into());
    form.html = Some(
        "<div>The sign-up form is <a href=\"https://example.test/forms/workshop\">here</a>, \
         and the old page is <a href=\"https://example.test.sign-in.example/login\">\
         www.example.test/login</a>.</div><div><br></div><div>Sana</div><br>\
         <div class=\"gmail_quote\"><div class=\"gmail_attr\">On Monday, Ingrid Solberg \
         wrote:</div><blockquote class=\"gmail_quote\">Where do we sign up?</blockquote></div>"
            .into(),
    );
    thread(
        "links",
        vec![
            (
                reply(
                    INGRID,
                    subject,
                    at(52),
                    "links-1",
                    "Hi all,\n\nWhere do we sign up for the spring workshop?\n\nIngrid\n",
                ),
                true,
                false,
            ),
            (
                reply(
                    TOMAS,
                    &re,
                    at(49),
                    "links-2",
                    "Good question; I don't know.\n",
                ),
                true,
                false,
            ),
            (
                reply(ME, &re, at(40), "links-3", "Marius will know.\n"),
                true,
                true,
            ),
            (
                reply(MARIUS, &re, at(20), "links-4", "Sana has the form.\n"),
                true,
                false,
            ),
            (form, false, false),
            (
                reply(
                    TOMAS,
                    &re,
                    at(1),
                    "links-6",
                    "Thanks! The agenda is at https://example.test/agenda and the map at \
                     www.example.test/map. Questions go to workshop@example.test.\n\nTomas\n\n\
                     > The sign-up form is here, and the old page is www.example.test/login.\n\
                     > Sana\n",
                ),
                false,
                false,
            ),
        ],
    )
}

fn letters(now: i64) -> Vec<Threaded> {
    let drafts = (0..8)
        .map(|issue| {
            let date = now - (8 - issue as i64) * 7 * 86_400 - 600;
            let draft = issue_draft(issue, date, &format!("letters-{}", issue + 1));
            (draft, issue < 7, false)
        })
        .collect();
    thread("letters", drafts)
}

fn long(now: i64) -> Vec<Threaded> {
    const POINTS: [&str; 5] = [
        "the notes are reviewed",
        "the screenshots are new",
        "the translations can wait",
        "the changelog is in order",
        "the download links work",
    ];
    let people = [INGRID, TOMAS, SANA, MARIUS, ME];
    let drafts = (0..150)
        .map(|n| {
            let from = people[(n + 1) % people.len()];
            let date = now - (150 - n as i64) * 1_800;
            let text = format!(
                "Point {} of the release checklist: {}.\n",
                n + 1,
                POINTS[n % POINTS.len()]
            );
            let id = format!("long-{}", n + 1);
            let draft = reply(from, "Re: Release checklist", date, &id, &text);
            (draft, n < 149, from.1 == ME.1)
        })
        .collect();
    thread("long", drafts)
}

fn typical(now: i64) -> Vec<Threaded> {
    let subject = "Re: Spring workshop plans";
    let at = |hours: i64| now - hours * 3_600;
    let plain = |from, hours, n: usize, text: &str| {
        reply(from, subject, at(hours), &format!("typical-{n}"), text)
    };
    let simple = |from, hours, n: usize, html: &str| {
        let mut draft = Draft::new(from, subject, at(hours), &format!("typical-{n}"));
        draft.text = Some("See the HTML part.\n".into());
        draft.html = Some(html.into());
        draft
    };
    let issue = |variant: usize, hours, n: usize| {
        let mut draft = issue_draft(variant, at(hours), &format!("typical-{n}"));
        draft.subject = subject.into();
        draft
    };
    let mut files = plain(
        MARIUS,
        1,
        10,
        "Final agenda and the venue's floor plan attached. See you Thursday.\n",
    );
    files.attachments = vec![
        Part::attachment(
            "agenda.pdf",
            "application/pdf",
            filler(b"%PDF-1.4\n", 96_000),
        ),
        Part::attachment(
            "floor-plan.png",
            "image/png",
            picture(400, 260, [30, 60, 110], [230, 230, 240]),
        ),
    ];
    thread(
        "typical",
        vec![
            (
                plain(
                    MARIUS,
                    70,
                    1,
                    "Hi all,\n\nTime to plan the spring workshop. Dates, venue and speakers \
                     below; reply with what you can take on.\n\nMarius\n",
                ),
                true,
                false,
            ),
            (
                simple(
                    INGRID,
                    66,
                    2,
                    "<p>I can take the venue. Two options:</p><ol><li>The library's big room \
                     (40 seats)</li><li>The co-op hall (60 seats, a small kitchen)</li></ol>",
                ),
                true,
                false,
            ),
            (issue(1, 60, 3), false, false),
            (
                plain(ME, 50, 4, "I'll write to the speakers this week.\n"),
                true,
                true,
            ),
            (issue(4, 40, 5), false, false),
            (
                simple(
                    SANA,
                    30,
                    6,
                    "<p>Catering quote is in: <b>coffee and lunch for 50</b>, with a vegetarian \
                     option. I'll book it Friday unless someone objects.</p>",
                ),
                true,
                false,
            ),
            (
                plain(TOMAS, 20, 7, "No objection. The co-op hall it is?\n"),
                true,
                false,
            ),
            (issue(7, 10, 8), false, false),
            (
                plain(INGRID, 5, 9, "Booked the co-op hall for the 12th.\n"),
                true,
                false,
            ),
            (files, false, false),
        ],
    )
}

/// Brand palettes for the newsletters: name, primary, accent, outer background.
const BRANDS: [(&str, &str, &str, &str); 8] = [
    ("Harbor & Pine", "#1b4d3e", "#e07a1f", "#f2efe9"),
    ("Lumen Weekly", "#23395b", "#d1495b", "#eef2f7"),
    ("Fjordline Travel", "#0b5563", "#f4a261", "#e9f5f5"),
    ("Copperleaf Market", "#7a3e1d", "#2a9d8f", "#faf3ea"),
    ("Northbound Digest", "#2b2d42", "#ef233c", "#edf2f4"),
    ("Kestrel Cycles", "#3d405b", "#81b29a", "#f4f1de"),
    ("Bramble Books", "#5f0f40", "#fb8b24", "#fdf6ec"),
    ("Orbit Coffee", "#432818", "#bb9457", "#f6eee3"),
];

const TOPICS: [(&str, &str); 6] = [
    (
        "The season's quiet trails",
        "Three walks that stay empty even in high season, with the places to stop for coffee \
         and the views worth the extra climb.",
    ),
    (
        "What the workshop taught us",
        "Twelve people, one long table and a weekend of mending: notes from the repair café, \
         and what we will do differently next time.",
    ),
    (
        "New arrivals on the shelf",
        "Hand-picked this month: two travel memoirs, a cookbook built around one pot, and a \
         field guide to the birds of the coast.",
    ),
    (
        "Behind the counter",
        "Meet the roaster who starts at five every morning, and learn why the beans rest for \
         three days before they reach the shop.",
    ),
    (
        "Readers write back",
        "Your letters on slow travel, small gardens and the best way to keep bread fresh, \
         answered by the people who know.",
    ),
    (
        "Events near you",
        "Talks, markets and an open studio weekend: everything happening within an hour of \
         the city this month, sorted by date.",
    ),
];

/// One newsletter issue on its brand's paper (S2's corpus, smaller): a `cid:` logo and hero,
/// three articles, a product grid with `cid:` pictures, and a footer with remote icons and a
/// tracking pixel at the closed port.
fn issue_draft(variant: usize, date: i64, id: &str) -> Draft {
    let (brand, primary, accent, background) = BRANDS[variant % BRANDS.len()];
    let domain = brand
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    let sender = format!("news@{domain}.example.test");
    let subject = format!("{brand} — issue {}", 40 + variant);
    let mut draft = Draft::new((brand, &sender), &subject, date, id);
    let cid = |name: &str| format!("{name}-{id}@{domain}.example.test");
    let mut html = format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         body{{margin:0;background:{background}}} a{{color:{primary}}}\
         @media screen and (max-width:620px){{.container{{width:100%!important}}}}\
         </style></head><body style=\"margin:0;background:{background}\">\
         <table role=\"presentation\" width=\"100%\" bgcolor=\"{background}\" cellpadding=\"0\" \
         cellspacing=\"0\"><tr><td align=\"center\" style=\"padding:24px 0\">\
         <table role=\"presentation\" class=\"container\" width=\"600\" bgcolor=\"#ffffff\" \
         cellpadding=\"0\" cellspacing=\"0\" style=\"border-radius:6px\">\
         <tr><td align=\"center\" style=\"padding:16px 24px;border-bottom:4px solid {accent}\">\
         <img src=\"cid:{}\" width=\"180\" height=\"50\" alt=\"{brand}\" \
         style=\"display:block\"></td></tr>\
         <tr><td><img src=\"cid:{}\" width=\"600\" height=\"260\" alt=\"\" \
         style=\"display:block;width:100%;height:auto\"></td></tr>",
        cid("logo"),
        cid("hero")
    );
    for article in 0..3 {
        let (title, text) = TOPICS[(variant + article) % TOPICS.len()];
        html.push_str(&format!(
            "<tr><td style=\"padding:24px 32px;font-family:Georgia,serif\">\
             <h2 style=\"margin:0 0 12px;font-size:22px;line-height:28px;color:{primary}\">\
             {title}</h2><p style=\"margin:0 0 16px;font-size:16px;line-height:24px;\
             color:#333333\">{text} {text}</p><table role=\"presentation\" cellpadding=\"0\" \
             cellspacing=\"0\"><tr><td bgcolor=\"{accent}\" style=\"border-radius:4px\">\
             <a href=\"https://example.test/{domain}/{article}\" style=\"display:inline-block;\
             padding:12px 24px;font:bold 15px Arial,sans-serif;color:#ffffff;\
             text-decoration:none\">Read more</a></td></tr></table></td></tr>"
        ));
    }
    html.push_str(
        "<tr><td style=\"padding:8px 24px 24px\"><table role=\"presentation\" \
         width=\"100%\"><tr>",
    );
    for product in 0..3 {
        html.push_str(&format!(
            "<td width=\"33%\" valign=\"top\" style=\"padding:6px;font:14px/20px \
             Helvetica,Arial,sans-serif;color:#444444\"><img src=\"cid:{}\" width=\"170\" \
             height=\"170\" alt=\"\" style=\"display:block;width:100%;height:auto\">\
             <b>Item {}</b><br>From 249 kr</td>",
            cid(&format!("item{product}")),
            product + 1
        ));
    }
    html.push_str("</tr></table></td></tr>");
    let mut icons = String::new();
    for network in ["social-a", "social-b", "social-c"] {
        icons.push_str(&format!(
            "<img src=\"{REMOTE_ORIGIN}/{domain}/{network}.png\" width=\"24\" height=\"24\" \
             alt=\"{network}\" style=\"display:inline-block;margin:0 6px\">"
        ));
    }
    html.push_str(&format!(
        "<tr><td align=\"center\" style=\"padding:24px;background:#f0f0f0;font:12px/18px \
         Arial,sans-serif;color:#777777\">{icons}<p style=\"margin:12px 0 4px\">{brand} · \
         1 Example Street · Exampleville</p><p style=\"margin:0\">You receive this because you \
         subscribed on example.test. <a href=\"https://example.test/{domain}/unsubscribe\" \
         style=\"color:#777777\">Unsubscribe</a></p><img \
         src=\"{REMOTE_ORIGIN}/{domain}/open.gif\" width=\"1\" height=\"1\" alt=\"\">\
         </td></tr></table></td></tr></table></body></html>"
    ));
    draft.html = Some(html);
    let tint = |seed: usize| {
        let base = [
            (seed * 53 % 200) as u8 + 30,
            (seed * 97 % 200) as u8 + 30,
            (seed * 31 % 200) as u8 + 30,
        ];
        (
            base,
            [255 - base[0] / 2, 255 - base[1] / 3, 255 - base[2] / 4],
        )
    };
    let (from, to) = tint(variant + 1);
    draft.inline = vec![
        Part::inline(&cid("logo"), "image/png", picture(180, 50, from, to)),
        Part::inline(&cid("hero"), "image/png", picture(600, 260, to, from)),
    ];
    for product in 0..3 {
        let (from, to) = tint(variant * 3 + product + 2);
        draft.inline.push(Part::inline(
            &cid(&format!("item{product}")),
            "image/png",
            picture(170, 170, from, to),
        ));
    }
    draft
}

/// A synthetic picture (S2's): a diagonal gradient with stripes and a block, so no two rows
/// look alike, as an 8-bit RGB PNG with stored deflate blocks.
pub fn picture(width: u32, height: u32, from: [u8; 3], to: [u8; 3]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    let span = (width + height).max(1) as f32;
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            let t = (x + y) as f32 / span;
            let stripe = (x / 12 + y / 12) % 7 == 0;
            let block = x > width / 3 && x < width / 2 && y > height / 3 && y < height * 2 / 3;
            for channel in 0..3 {
                let base = f32::from(from[channel]) * (1.0 - t) + f32::from(to[channel]) * t;
                let value = if block {
                    255.0 - base
                } else if stripe {
                    base * 0.8
                } else {
                    base
                };
                raw.push(value as u8);
            }
        }
    }
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut out, b"IHDR", &header);
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = data.chunks(usize::from(u16::MAX)).peekable();
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// `count` messages that are each a conversation of their own, as a mailbox mixes them: a
/// styled newsletter every third, simple HTML every third, plain text the rest; `single-1`
/// newest, an hour apart, all read.
pub fn singles(count: usize, now: i64) -> Vec<Threaded> {
    (0..count)
        .map(|n| {
            let date = now - (n as i64 + 1) * 3_600;
            let id = format!("single-{}", n + 1);
            let people = [INGRID, TOMAS, SANA, MARIUS];
            let draft = match n % 3 {
                0 => issue_draft(n, date, &id),
                1 => {
                    let mut draft = Draft::new(people[n % 4], "Notes from today", date, &id);
                    draft.text = Some("See the HTML part.\n".into());
                    draft.html = Some(format!(
                        "<p>Notes from today, number {}:</p><ul><li>the budget is agreed</li>\
                         <li>the next meeting is on Thursday</li></ul><p>— {}</p>",
                        n + 1,
                        people[n % 4].0
                    ));
                    draft
                }
                _ => reply(
                    people[n % 4],
                    "Quick question",
                    date,
                    &id,
                    &format!(
                        "Hi,\n\nCould you send me the file from yesterday? It's number {} on \
                         the list.\n\nThanks!\n",
                        n + 1
                    ),
                ),
            };
            Threaded {
                name: id,
                raw: draft.render(),
                read: true,
                sent: false,
            }
        })
        .collect()
}

/// Where [`fill_conversation`] put a conversation's messages, by name.
#[derive(Debug, Clone, Default)]
pub struct Filed {
    pub ids: Vec<(String, MessageId)>,
}

impl Filed {
    pub fn id(&self, name: &str) -> Option<MessageId> {
        self.ids
            .iter()
            .find(|(sample, _)| sample == name)
            .map(|(_, id)| *id)
    }
}

/// Files `messages` into `inbox` (the user's own into `sent`) through `ingest_raw`, read or
/// unread as each says. Blocks: run it off the GTK thread.
pub fn fill_conversation(
    store: &Store,
    blobs: &BlobStore,
    inbox: FolderId,
    sent: FolderId,
    messages: Vec<Threaded>,
) -> Result<Filed, IngestError> {
    let names: Vec<String> = messages
        .iter()
        .map(|message| message.name.clone())
        .collect();
    let raw = messages
        .into_iter()
        .map(|message| RawMessage {
            folder: if message.sent { sent } else { inbox },
            flags: if message.read {
                sealwax_domain::model::Flags::SEEN
            } else {
                sealwax_domain::model::Flags::NONE
            },
            raw: message.raw,
            ..RawMessage::default()
        })
        .collect();
    let ingested = ingest::ingest_raw(store, blobs, raw)?;
    let ids = names
        .into_iter()
        .zip(ingested.ids)
        .filter_map(|(name, id)| id.map(|id| (name, id)))
        .collect();
    Ok(Filed { ids })
}

#[cfg(test)]
mod conversation_tests {
    use super::*;
    use sealwax_domain::message;
    use sealwax_domain::model::BodyKind;

    #[test]
    fn each_conversation_threads_and_stays_fictional() {
        let now = 1_791_115_200;
        for name in CONVERSATIONS {
            let messages = conversation(name, now);
            assert!(messages.len() >= 6, "{name}");
            let mut previous: Option<String> = None;
            for threaded in &messages {
                let parsed = message::parse(&threaded.raw);
                let id = parsed.headers.message_id.clone().unwrap();
                match &previous {
                    Some(parent) => {
                        assert_eq!(
                            parsed.headers.in_reply_to,
                            std::slice::from_ref(parent),
                            "{}",
                            threaded.name
                        );
                        assert!(parsed.headers.references.contains(parent));
                    }
                    None => assert!(parsed.headers.in_reply_to.is_empty()),
                }
                previous = Some(id);
                let text = String::from_utf8_lossy(&threaded.raw);
                for host in text.split("://").skip(1) {
                    assert!(
                        host.starts_with("example.test") || host.starts_with("127.0.0.1:2/"),
                        "{}: {}",
                        threaded.name,
                        &host[..host.len().min(30)]
                    );
                }
            }
        }
        let typical = conversation("typical", now);
        let html = typical
            .iter()
            .filter(|threaded| message::parse(&threaded.raw).body_kind == BodyKind::Html)
            .count();
        assert_eq!((typical.len(), html), (10, 3));
        assert_eq!(conversation("long", now).len(), 150);
        assert_eq!(conversation("letters", now).len(), 8);
    }

    #[test]
    fn pictures_are_pngs() {
        let png = picture(4, 3, [0, 0, 0], [255, 255, 255]);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
