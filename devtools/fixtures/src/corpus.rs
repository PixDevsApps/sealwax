//! A deterministic synthetic mailbox as raw RFC 5322 messages: fictional people at example.test,
//! Norwegian, English, Chinese and Japanese text, conversations with References, newsletters,
//! receipts with attachments, inline `cid:` images, legacy charsets, encoded names, long subjects
//! and a few malformed messages. The same seed always gives the same bytes. Message 0 is the
//! newest, so importing in order is a first sync's order and replies arrive before their parents.

use crate::mime::{self, CRLF, NameEncoding};
use crate::rng::Rng;
use crate::words::*;
use encoding_rs::Encoding;
use sealwax_domain::model::FolderRole;

const DAY: i64 = 86_400;
/// 2026-10-04 00:00 UTC: the default "now", so the corpus doesn't change with the clock.
pub const DEFAULT_NOW: i64 = 1_791_072_000;
pub const ME_NAME: &str = "Alex Berg";
pub const ME_ADDR: &str = "alex.berg@example.test";
const DOMAIN: &str = "example.test";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    No,
    En,
    Zh,
    Ja,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Conversation,
    Newsletter,
    Notice,
    Junk,
    Draft,
}

/// The folder a message belongs in when a corpus is spread over folders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Folder {
    Inbox,
    Sent,
    Archive,
    Lists,
    Junk,
    Trash,
    Drafts,
}

impl Folder {
    pub const ALL: [Folder; 7] = [
        Folder::Inbox,
        Folder::Sent,
        Folder::Archive,
        Folder::Lists,
        Folder::Junk,
        Folder::Trash,
        Folder::Drafts,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Folder::Inbox => "INBOX",
            Folder::Sent => "Sent",
            Folder::Archive => "Archive",
            Folder::Lists => "Lister",
            Folder::Junk => "Junk",
            Folder::Trash => "Trash",
            Folder::Drafts => "Drafts",
        }
    }

    pub fn role(self) -> FolderRole {
        match self {
            Folder::Inbox => FolderRole::Inbox,
            Folder::Sent => FolderRole::Sent,
            Folder::Archive => FolderRole::Archive,
            Folder::Lists => FolderRole::Other,
            Folder::Junk => FolderRole::Junk,
            Folder::Trash => FolderRole::Trash,
            Folder::Drafts => FolderRole::Drafts,
        }
    }

    /// The tag in generated file names.
    pub fn slug(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Sent => "sent",
            Folder::Archive => "archive",
            Folder::Lists => "lists",
            Folder::Junk => "junk",
            Folder::Trash => "trash",
            Folder::Drafts => "drafts",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Folder> {
        Self::ALL.into_iter().find(|folder| folder.slug() == slug)
    }
}

/// The MIME structure of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Plain,
    Alternative,
    Html,
    /// HTML with an inline `cid:` image (multipart/related).
    Related,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformation {
    NoDate,
    BadDate,
    NoMessageId,
    UnclosedMultipart,
    TruncatedBase64,
    UnknownCharset,
    RawEightBitSubject,
    GarbageHeaderLines,
    NoHeaders,
    NulBytes,
    LongHeaderLine,
}

const MALFORMATIONS: [Malformation; 11] = [
    Malformation::NoDate,
    Malformation::BadDate,
    Malformation::NoMessageId,
    Malformation::UnclosedMultipart,
    Malformation::TruncatedBase64,
    Malformation::UnknownCharset,
    Malformation::RawEightBitSubject,
    Malformation::GarbageHeaderLines,
    Malformation::NoHeaders,
    Malformation::NulBytes,
    Malformation::LongHeaderLine,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Utf8,
    Latin1,
    Legacy(&'static Encoding),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferEncoding {
    QuotedPrintable,
    Base64,
    EightBit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachmentKind {
    Pdf,
    Csv,
    Png,
    Text,
}

#[derive(Clone, Debug)]
struct AttachmentSpec {
    kind: AttachmentKind,
    name: String,
    size: usize,
    seed: u64,
}

#[derive(Clone, Debug)]
struct Person {
    name: String,
    addr: String,
    lang: Lang,
}

#[derive(Clone, Debug)]
struct Quote {
    seed: u64,
    lang: Lang,
    from: String,
    date: i64,
}

#[derive(Clone, Debug)]
struct BodySpec {
    seed: u64,
    lang: Lang,
    length: usize,
    greet: String,
    signer: String,
    quote: Option<Quote>,
}

/// One message to render.
#[derive(Clone, Debug)]
pub struct Spec {
    pub folder: Folder,
    /// When it was sent, Unix seconds.
    pub date: i64,
    pub message_id: String,
    pub malformed: Option<Malformation>,
    zone: i32,
    from: (String, String),
    to: Vec<(String, String)>,
    cc: Vec<(String, String)>,
    subject: String,
    in_reply_to: Option<String>,
    references: Vec<String>,
    body: BodySpec,
    shape: Shape,
    attachments: Vec<AttachmentSpec>,
    charset: Charset,
    transfer: TransferEncoding,
    names: NameEncoding,
    newsletter: Option<usize>,
    /// For a copy filed in Archive too: its number, which keeps its bytes unique.
    copy: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub count: usize,
    pub seed: u64,
    pub now: i64,
    pub years: f64,
}

pub struct Corpus {
    /// Newest first.
    pub specs: Vec<Spec>,
    seed: u64,
}

impl Corpus {
    pub fn generate(config: Config) -> Corpus {
        let mut rng = Rng::new(config.seed);
        let people = people(&mut rng);
        let mut builder = Builder {
            rng: rng.fork(),
            now: config.now,
            years: config.years,
            people: &people,
            specs: Vec::with_capacity(config.count + 64),
            sequence: 0,
            seed: config.seed,
        };
        let copies = config.count / 100;
        while builder.specs.len() < config.count - copies {
            let u = builder.rng.unit();
            if u < 0.58 {
                builder.conversation();
            } else if u < 0.80 {
                builder.single(Kind::Newsletter);
            } else if u < 0.955 {
                builder.single(Kind::Notice);
            } else if u < 0.99 {
                builder.single(Kind::Junk);
            } else {
                builder.single(Kind::Draft);
            }
        }
        builder.specs.truncate(config.count - copies);
        let inbox: Vec<usize> = (0..builder.specs.len())
            .filter(|index| {
                let spec = &builder.specs[*index];
                spec.folder == Folder::Inbox && spec.malformed.is_none()
            })
            .collect();
        for number in 0..if inbox.is_empty() { 0 } else { copies } {
            let source = *builder.rng.pick(&inbox);
            let mut copy = builder.specs[source].clone();
            copy.folder = Folder::Archive;
            copy.copy = Some(number as u32);
            builder.specs.push(copy);
        }
        let mut specs = builder.specs;
        let mut order: Vec<usize> = (0..specs.len()).collect();
        order.sort_by_key(|index| std::cmp::Reverse((specs[*index].date, *index)));
        let mut slots: Vec<Option<Spec>> = specs.drain(..).map(Some).collect();
        let specs = order
            .into_iter()
            .map(|index| slots[index].take().expect("each spec once"))
            .collect();
        Corpus {
            specs,
            seed: config.seed,
        }
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// The raw bytes of message `index` (0 is the newest).
    pub fn render(&self, index: usize) -> Vec<u8> {
        render(&self.specs[index], index, self.seed)
    }

    /// The file name `generate` writes message `index` under.
    pub fn file_name(&self, index: usize) -> String {
        format!("{index:06}-{}.eml", self.specs[index].folder.slug())
    }
}

struct Builder<'a> {
    rng: Rng,
    now: i64,
    years: f64,
    people: &'a [Person],
    specs: Vec<Spec>,
    sequence: u64,
    seed: u64,
}

fn ascii_fold(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            'æ' | 'Æ' => "ae".chars().collect::<Vec<_>>(),
            'ø' | 'Ø' => vec!['o'],
            'å' | 'Å' => vec!['a'],
            c => c.to_lowercase().collect(),
        })
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

fn people(rng: &mut Rng) -> Vec<Person> {
    let mut people = Vec::new();
    for index in 0..1400 {
        let (first, last, lang) = if index % 5 == 4 {
            (*rng.pick(EN_FIRST), *rng.pick(EN_LAST), Lang::En)
        } else {
            (*rng.pick(NO_FIRST), *rng.pick(NO_LAST), Lang::No)
        };
        people.push(Person {
            name: format!("{first} {last}"),
            addr: format!(
                "{}.{}{}@{DOMAIN}",
                ascii_fold(first),
                ascii_fold(last),
                index % 37
            ),
            lang,
        });
    }
    for (name, local) in ZH_NAMES {
        people.push(Person {
            name: (*name).to_owned(),
            addr: format!("{local}@{DOMAIN}"),
            lang: Lang::Zh,
        });
    }
    for (name, local) in JA_NAMES {
        people.push(Person {
            name: (*name).to_owned(),
            addr: format!("{local}@{DOMAIN}"),
            lang: Lang::Ja,
        });
    }
    people
}

/// Zipf sampler over a word list (exponent 1.0).
struct Zipf {
    cdf: Vec<f64>,
}

impl Zipf {
    fn new(n: usize) -> Self {
        let mut total = 0.0;
        let mut cdf = Vec::with_capacity(n);
        for rank in 1..=n {
            total += 1.0 / rank as f64;
            cdf.push(total);
        }
        for value in &mut cdf {
            *value /= total;
        }
        Self { cdf }
    }

    fn sample(&self, rng: &mut Rng) -> usize {
        let u = rng.unit();
        self.cdf
            .partition_point(|value| *value < u)
            .min(self.cdf.len() - 1)
    }
}

fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

fn sentence(rng: &mut Rng, words: &[&str], zipf: &Zipf, topics: &[&str], out: &mut String) {
    let length = rng.range(5, 18);
    for index in 0..length {
        if index > 0 {
            out.push(' ');
        }
        let roll = rng.unit();
        let word: String = if roll < 0.0002 {
            (*rng.pick(RARE_WORDS)).to_owned()
        } else if roll < 0.03 {
            rng.range(2, 99_999).to_string()
        } else if roll < 0.04 {
            (*rng.pick(topics)).to_owned()
        } else if roll < 0.045 {
            format!("INV-{}", rng.range(10_000, 99_999))
        } else if roll < 0.05 {
            format!(
                "https://{DOMAIN}/{}/{}",
                rng.pick(words),
                rng.range(100, 99_999)
            )
        } else {
            words[zipf.sample(rng)].to_owned()
        };
        if index == 0 {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        } else {
            out.push_str(&word);
        }
    }
    out.push_str(match rng.below(10) {
        0 => "? ",
        1 => "! ",
        _ => ". ",
    });
}

fn cjk_sentence(rng: &mut Rng, phrases: &[&str], topics: &[&str], comma: &str, out: &mut String) {
    let parts = rng.range(1, 4);
    for index in 0..parts {
        if index > 0 {
            out.push_str(comma);
        }
        if rng.chance(0.2) {
            out.push_str(rng.pick(topics));
        }
        out.push_str(rng.pick(phrases));
    }
    out.push('。');
}

/// Paragraphs of text in the spec's language, about `length` bytes; the last ones quote the
/// parent, if any, as lines starting with "> ".
fn paragraphs(spec: &BodySpec) -> (Vec<String>, Option<(String, Vec<String>)>) {
    let mut rng = Rng::new(spec.seed);
    let no = Zipf::new(NO_WORDS.len());
    let en = Zipf::new(EN_WORDS.len());
    let greet = match first_name(&spec.greet) {
        "" => String::new(),
        name => format!(" {name}"),
    };
    let mut out = vec![match spec.lang {
        Lang::No => format!("Hei{greet},"),
        Lang::En => format!("Hi{greet},"),
        Lang::Zh => format!("{}您好！", spec.greet),
        Lang::Ja => format!("{}様", spec.greet),
    }];
    let mut written = 0;
    while written < spec.length {
        let mut paragraph = String::new();
        for _ in 0..rng.range(2, 6) {
            match spec.lang {
                Lang::No => sentence(&mut rng, NO_WORDS, &no, NO_TOPICS, &mut paragraph),
                Lang::En => sentence(&mut rng, EN_WORDS, &en, EN_TOPICS, &mut paragraph),
                Lang::Zh => cjk_sentence(&mut rng, ZH_PHRASES, ZH_TOPICS, "，", &mut paragraph),
                Lang::Ja => cjk_sentence(&mut rng, JA_PHRASES, JA_TOPICS, "、", &mut paragraph),
            }
        }
        written += paragraph.len();
        out.push(paragraph.trim_end().to_owned());
    }
    out.push(match spec.lang {
        Lang::No => format!("Hilsen\n{}", first_name(&spec.signer)),
        Lang::En => format!("Best regards,\n{}", spec.signer),
        Lang::Zh => format!("谢谢！\n{}", spec.signer),
        Lang::Ja => format!("よろしくお願いいたします。\n{}", spec.signer),
    });
    let quote = spec.quote.as_ref().map(|quote| {
        let attribution = match spec.lang {
            Lang::No => format!("Den {} skrev {}:", mime::date(quote.date, 120), quote.from),
            Lang::En | Lang::Zh | Lang::Ja => {
                format!("On {}, {} wrote:", mime::date(quote.date, 0), quote.from)
            }
        };
        let (quoted, _) = paragraphs(&BodySpec {
            seed: quote.seed,
            lang: quote.lang,
            length: 200,
            greet: String::new(),
            signer: quote.from.clone(),
            quote: None,
        });
        (attribution, quoted)
    });
    (out, quote)
}

fn plain_body(spec: &BodySpec) -> String {
    let (paragraphs, quote) = paragraphs(spec);
    let mut text = paragraphs.join("\n\n");
    text.push('\n');
    if let Some((attribution, quoted)) = quote {
        text.push('\n');
        text.push_str(&attribution);
        text.push('\n');
        for line in quoted.join("\n\n").lines() {
            text.push_str("> ");
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

fn html_body(spec: &BodySpec, image: Option<&str>) -> String {
    let (paragraphs, quote) = paragraphs(spec);
    let mut html = String::from(
        "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head>\
         <body><div dir=\"ltr\">",
    );
    for paragraph in &paragraphs {
        html.push_str("<div>");
        html.push_str(&mime::html_escape(paragraph).replace('\n', "<br>"));
        html.push_str("</div><div><br></div>");
    }
    if let Some(cid) = image {
        html.push_str(&format!(
            "<div><img src=\"cid:{cid}\" alt=\"\" width=\"96\" height=\"32\"></div>"
        ));
    }
    html.push_str("</div>");
    if let Some((attribution, quoted)) = quote {
        html.push_str("<br><div class=\"gmail_quote\"><div dir=\"ltr\" class=\"gmail_attr\">");
        html.push_str(&mime::html_escape(&attribution));
        html.push_str("<br></div><blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;border-left:1px solid rgb(204,204,204);padding-left:1ex\">");
        for paragraph in &quoted {
            html.push_str("<div>");
            html.push_str(&mime::html_escape(paragraph).replace('\n', "<br>"));
            html.push_str("</div>");
        }
        html.push_str("</blockquote></div>");
    }
    html.push_str("</body></html>\n");
    html
}

const COLORS: [&str; 6] = [
    "#1f6f5c", "#8a3b12", "#284b8c", "#6b2d6b", "#a5761c", "#2e2e2e",
];

fn newsletter_html(spec: &Spec, newsletter: usize, image: Option<&str>) -> String {
    let (name, local, _) = NEWSLETTERS[newsletter];
    let mut rng = Rng::new(spec.body.seed ^ 0x5eed);
    let color = *rng.pick(&COLORS);
    let (paragraphs, _) = paragraphs(&spec.body);
    let preheader = paragraphs
        .get(1)
        .map(|text| text.chars().take(90).collect::<String>())
        .unwrap_or_default();
    let mut html = format!(
        "<!DOCTYPE html><html lang=\"no\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width\"><title>{title}</title>\
         <style>body{{margin:0;padding:0;background:#f4f1ea}}a{{color:{color}}}\
         .wrap{{max-width:600px;margin:0 auto}}@media (max-width:620px){{.wrap{{width:100%!important}}}}</style>\
         </head><body style=\"margin:0;background:#f4f1ea\">\
         <div style=\"display:none;max-height:0;overflow:hidden;opacity:0\">{preheader}{pad}</div>\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" bgcolor=\"#f4f1ea\"><tr><td align=\"center\">\
         <table role=\"presentation\" class=\"wrap\" width=\"600\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" bgcolor=\"#ffffff\">\
         <tr><td style=\"padding:24px 32px;background:{color};color:#ffffff;font:bold 22px Georgia,serif\">",
        title = mime::html_escape(&spec.subject),
        preheader = mime::html_escape(&preheader),
        pad = "&zwnj;&nbsp;".repeat(30),
    );
    match image {
        Some(cid) => html.push_str(&format!(
            "<img src=\"cid:{cid}\" alt=\"{}\" width=\"120\" height=\"40\" style=\"display:block\">",
            mime::html_escape(name)
        )),
        None => html.push_str(&format!(
            "<img src=\"https://{DOMAIN}/n/{local}/logo.png\" alt=\"{}\" width=\"120\" height=\"40\" style=\"display:block\">",
            mime::html_escape(name)
        )),
    }
    html.push_str("</td></tr>");
    for (index, paragraph) in paragraphs.iter().enumerate().skip(1) {
        let heading: String = paragraph
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        html.push_str(&format!(
            "<tr><td style=\"padding:20px 32px 0;font:16px/1.55 Georgia,serif;color:#222222\">\
             <h2 style=\"margin:0 0 8px;font:bold 19px Helvetica,Arial,sans-serif;color:{color}\">{}</h2>\
             <p style=\"margin:0 0 12px\">{}</p>\
             <a href=\"https://{DOMAIN}/n/{local}/{}\" style=\"display:inline-block;padding:10px 18px;background:{color};color:#ffffff;text-decoration:none;border-radius:4px;font:bold 14px Helvetica,Arial,sans-serif\">Les mer</a>\
             </td></tr>",
            mime::html_escape(&heading),
            mime::html_escape(paragraph),
            spec.body.seed % 100_000 + index as u64,
        ));
    }
    html.push_str(&format!(
        "<tr><td style=\"padding:28px 32px;font:12px/1.5 Helvetica,Arial,sans-serif;color:#777777\">\
         Du får denne e-posten fordi du abonnerer på {}. \
         <a href=\"https://{DOMAIN}/n/{local}/unsubscribe\" style=\"color:#777777\">Meld deg av</a></td></tr>\
         </table></td></tr></table></body></html>\n",
        mime::html_escape(name)
    ));
    html
}

/// A valid 1×1 transparent PNG.
const PNG: [u8; 67] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

fn attachment_bytes(spec: &AttachmentSpec) -> Vec<u8> {
    let mut rng = Rng::new(spec.seed);
    match spec.kind {
        AttachmentKind::Png => PNG.to_vec(),
        AttachmentKind::Pdf => {
            let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
            while out.len() < spec.size {
                out.extend_from_slice(format!("% fixture {:016x}\n", rng.next_u64()).as_bytes());
            }
            out.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
            out
        }
        AttachmentKind::Csv => {
            let mut out = String::from("dato;beskrivelse;beløp\n");
            while out.len() < spec.size {
                out.push_str(&format!(
                    "2026-{:02}-{:02};post {};{}.{:02}\n",
                    rng.range(1, 13),
                    rng.range(1, 29),
                    rng.range(1, 500),
                    rng.range(10, 99_999),
                    rng.range(0, 100)
                ));
            }
            out.into_bytes()
        }
        AttachmentKind::Text => {
            let mut out = String::from("Notater fra møtet\n\n");
            let zipf = Zipf::new(NO_WORDS.len());
            while out.len() < spec.size {
                sentence(&mut rng, NO_WORDS, &zipf, NO_TOPICS, &mut out);
                out.push('\n');
            }
            out.into_bytes()
        }
    }
}

impl Builder<'_> {
    fn message_id(&mut self) -> String {
        self.sequence += 1;
        format!(
            "{:x}.{:08x}.{:x}@{DOMAIN}",
            self.sequence,
            self.rng.next_u64() as u32,
            self.seed
        )
    }

    fn person_for(&mut self, lang: Lang) -> Person {
        match lang {
            Lang::Zh | Lang::Ja => {
                let wanted: Vec<&Person> = self.people.iter().filter(|p| p.lang == lang).collect();
                (*self.rng.pick(&wanted)).clone()
            }
            _ => self.rng.pick(&self.people[..1400]).clone(),
        }
    }

    fn date(&mut self) -> i64 {
        let span = (self.years * 365.25 * DAY as f64) as i64;
        loop {
            let u = self.rng.unit();
            let x = (-1.0 + (1.0 + 15.0 * u).sqrt()) / 3.0;
            let day_start = self.now - span + (x * span as f64) as i64;
            let day_start = day_start - day_start.rem_euclid(DAY);
            let weekday = (day_start / DAY + 3).rem_euclid(7);
            if weekday >= 5 && !self.rng.chance(0.35) {
                continue;
            }
            let hour = if self.rng.chance(0.78) {
                self.rng.range(7, 18)
            } else {
                self.rng.range(0, 24)
            };
            let moment = day_start + hour * 3600 + self.rng.range(0, 3600);
            if moment <= self.now {
                return moment;
            }
        }
    }

    fn lang(&mut self, kind: Kind) -> Lang {
        let u = self.rng.unit();
        match kind {
            Kind::Junk if u < 0.7 => Lang::En,
            Kind::Junk if u < 0.9 => Lang::No,
            Kind::Junk => Lang::Zh,
            _ if u < 0.55 => Lang::No,
            _ if u < 0.9 => Lang::En,
            _ if u < 0.95 => Lang::Zh,
            _ => Lang::Ja,
        }
    }

    fn subject(&mut self, lang: Lang, kind: Kind) -> String {
        let rng = &mut self.rng;
        let t = match lang {
            Lang::No => *rng.pick(NO_TOPICS),
            Lang::En => *rng.pick(EN_TOPICS),
            Lang::Zh => *rng.pick(ZH_TOPICS),
            Lang::Ja => *rng.pick(JA_TOPICS),
        };
        let n = rng.range(100, 9_999);
        let text = match (kind, lang) {
            (Kind::Conversation | Kind::Draft, Lang::No) => match rng.below(10) {
                0 => format!("Møte om {t}"),
                1 => format!("Oppdatering: {t}"),
                2 => format!("Spørsmål om {t}"),
                3 => format!("Ang. {t}"),
                4 => format!("Utkast til gjennomgang – {t}"),
                5 => format!("Påminnelse: {t}"),
                6 => format!("Referat fra møtet om {t}"),
                7 => "Kort spørsmål".to_owned(),
                8 => format!("Tilbakemelding på {t}"),
                _ => format!("Status for {t}"),
            },
            (Kind::Conversation | Kind::Draft, Lang::En) => match rng.below(8) {
                0 => format!("Meeting about {t}"),
                1 => format!("Quick question on {t}"),
                2 => format!("Update: {t}"),
                3 => format!("Notes from the call on {t}"),
                4 => format!("Draft for review – {t}"),
                5 => format!("Feedback on {t}"),
                6 => "Checking in".to_owned(),
                _ => format!("Next steps for {t}"),
            },
            (Kind::Conversation | Kind::Draft, Lang::Zh) => match rng.below(3) {
                0 => format!("关于{t}的会议安排"),
                1 => format!("{t}进度更新"),
                _ => format!("{t}会议纪要"),
            },
            (Kind::Conversation | Kind::Draft, Lang::Ja) => match rng.below(3) {
                0 => format!("{t}についてのご相談"),
                1 => format!("{t}の進捗について"),
                _ => format!("【リマインダー】{t}"),
            },
            (Kind::Newsletter, Lang::No) => match rng.below(3) {
                0 => format!("Ukens nyheter: {t}"),
                1 => format!("Nytt fra oss – utgave {n}"),
                _ => format!("{} tips om {t}", rng.range(3, 12)),
            },
            (Kind::Newsletter, _) => format!("Weekly digest #{n}: {}", rng.pick(EN_TOPICS)),
            (Kind::Notice, Lang::No) => match rng.below(4) {
                0 => format!("Faktura {n} forfaller snart"),
                1 => format!("Kvittering for bestilling {n}"),
                2 => format!("Pakken din er på vei ({n})"),
                _ => format!("Timebekreftelse {n}"),
            },
            (Kind::Notice, _) => match rng.below(4) {
                0 => format!("Invoice INV-{n}"),
                1 => format!("Your order {n} has shipped"),
                2 => format!("Payment received – INV-{n}"),
                _ => format!("Build #{n} passed"),
            },
            (Kind::Junk, _) => match rng.below(3) {
                0 => "Gratulerer! Du har vunnet".to_owned(),
                1 => format!("Limited offer: {}% off everything", rng.range(20, 90)),
                _ => "Exclusive opportunity".to_owned(),
            },
        };
        if rng.chance(0.02) {
            let extra = match lang {
                Lang::No => *rng.pick(NO_TOPICS),
                Lang::Zh => *rng.pick(ZH_TOPICS),
                Lang::Ja => *rng.pick(JA_TOPICS),
                Lang::En => *rng.pick(EN_TOPICS),
            };
            return format!(
                "{text} – {} – {extra} – {}",
                (0..6)
                    .map(|_| *rng.pick(EN_TOPICS))
                    .collect::<Vec<_>>()
                    .join(", "),
                (0..4)
                    .map(|_| *rng.pick(NO_TOPICS))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if rng.chance(0.01) {
            return format!("{text} ({})", rng.pick(RARE_WORDS));
        }
        text
    }

    fn attachments(&mut self, kind: Kind, lang: Lang) -> Vec<AttachmentSpec> {
        let count = 1 + usize::from(self.rng.chance(0.2));
        (0..count)
            .map(|_| {
                let n = self.rng.range(100, 99_999);
                let (kind, name) = match (kind, self.rng.below(5)) {
                    (Kind::Notice, _) | (_, 0) => (
                        AttachmentKind::Pdf,
                        match lang {
                            Lang::No => format!("faktura-{n}.pdf"),
                            Lang::Ja => format!("プロジェクト資料-{n}.pdf"),
                            Lang::Zh => format!("报告-{n}.pdf"),
                            Lang::En => format!("invoice-{n}.pdf"),
                        },
                    ),
                    (_, 1) => (AttachmentKind::Csv, format!("budsjett {n}.csv")),
                    (_, 2) => (AttachmentKind::Png, format!("skjermbilde-{n}.png")),
                    _ => (AttachmentKind::Text, format!("møtereferat {n}.txt")),
                };
                AttachmentSpec {
                    kind,
                    name,
                    size: self.rng.log_normal(6_000.0, 0.8).clamp(300.0, 40_000.0) as usize,
                    seed: self.rng.next_u64(),
                }
            })
            .collect()
    }

    fn charset(&mut self, lang: Lang) -> Charset {
        match lang {
            Lang::No if self.rng.chance(0.1) => Charset::Latin1,
            Lang::Ja if self.rng.chance(0.4) => Charset::Legacy(encoding_rs::ISO_2022_JP),
            Lang::Zh if self.rng.chance(0.3) => Charset::Legacy(encoding_rs::GBK),
            _ => Charset::Utf8,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        kind: Kind,
        lang: Lang,
        folder: Folder,
        date: i64,
        from: (String, String),
        to: Vec<(String, String)>,
        cc: Vec<(String, String)>,
        subject: String,
        parent: Option<usize>,
        has_files: bool,
    ) -> usize {
        let message_id = self.message_id();
        let (in_reply_to, references) = match parent {
            Some(parent) => {
                let parent = &self.specs[parent];
                let mut chain = parent.references.clone();
                chain.push(parent.message_id.clone());
                if chain.len() > 8 {
                    let tail = chain.split_off(chain.len() - 4);
                    chain.truncate(1);
                    chain.extend(tail);
                }
                if self.rng.chance(0.08) {
                    chain.clear();
                }
                (Some(parent.message_id.clone()), chain)
            }
            None => (None, Vec::new()),
        };
        let quote = parent.and_then(|parent| {
            let parent = &self.specs[parent];
            self.rng.chance(0.5).then(|| Quote {
                seed: parent.body.seed,
                lang: parent.body.lang,
                from: parent.from.0.clone(),
                date: parent.date,
            })
        });
        let length = match kind {
            Kind::Newsletter => self.rng.log_normal(3_000.0, 0.5),
            Kind::Notice => self.rng.log_normal(700.0, 0.5),
            Kind::Junk => self.rng.log_normal(900.0, 0.6),
            Kind::Draft => self.rng.log_normal(400.0, 0.7),
            Kind::Conversation => self.rng.log_normal(900.0, 0.9),
        }
        .clamp(120.0, 40_000.0) as usize;
        let shape = match kind {
            Kind::Newsletter => match self.rng.below(20) {
                0..=10 => Shape::Html,
                11..=17 => Shape::Alternative,
                _ => Shape::Related,
            },
            Kind::Junk => {
                if self.rng.chance(0.6) {
                    Shape::Html
                } else {
                    Shape::Plain
                }
            }
            _ => match self.rng.below(100) {
                0..=59 => Shape::Plain,
                60..=96 => Shape::Alternative,
                _ => Shape::Related,
            },
        };
        let charset = if matches!(shape, Shape::Plain) {
            self.charset(lang)
        } else {
            Charset::Utf8
        };
        let transfer = match self.rng.below(4) {
            0 | 1 => TransferEncoding::QuotedPrintable,
            2 => TransferEncoding::Base64,
            _ => TransferEncoding::EightBit,
        };
        let names = match charset {
            Charset::Legacy(encoding) => NameEncoding::Legacy(encoding),
            _ if self.rng.chance(0.4) => NameEncoding::Latin1Q,
            _ => NameEncoding::Utf8Base64,
        };
        let attachments = if has_files {
            self.attachments(kind, lang)
        } else {
            Vec::new()
        };
        let malformed = self
            .rng
            .chance(0.01)
            .then(|| *self.rng.pick(&MALFORMATIONS));
        let zone = *self.rng.pick(&[0, 60, 120, 120, 120, -300, 540]);
        let body = BodySpec {
            seed: self.rng.next_u64(),
            lang,
            length,
            greet: to.first().map(|(name, _)| name.clone()).unwrap_or_default(),
            signer: from.0.clone(),
            quote,
        };
        let newsletter =
            (kind == Kind::Newsletter).then(|| self.rng.below(NEWSLETTERS.len() as u64) as usize);
        self.specs.push(Spec {
            folder,
            date,
            message_id,
            malformed,
            zone,
            from,
            to,
            cc,
            subject,
            in_reply_to,
            references,
            body,
            shape,
            attachments,
            charset,
            transfer,
            names,
            newsletter,
            copy: None,
        });
        self.specs.len() - 1
    }

    fn conversation(&mut self) {
        let lang = self.lang(Kind::Conversation);
        let others: Vec<Person> = (0..self.rng.range(1, 5))
            .map(|_| self.person_for(lang))
            .collect();
        let me = (ME_NAME.to_owned(), ME_ADDR.to_owned());
        let size = {
            let u = self.rng.unit();
            if u < 0.45 {
                1
            } else if u < 0.65 {
                2
            } else if u < 0.77 {
                3
            } else if u < 0.87 {
                self.rng.range(4, 6) as usize
            } else if u < 0.95 {
                self.rng.range(6, 11) as usize
            } else if u < 0.99 {
                self.rng.range(11, 26) as usize
            } else {
                self.rng.range(26, 61) as usize
            }
        };
        let base = self.subject(lang, Kind::Conversation);
        let has_files = self.rng.chance(0.25);
        let mut date = self.date();
        let mut from_me = self.rng.chance(0.15);
        let mut members: Vec<usize> = Vec::new();
        for position in 0..size {
            if position > 0 {
                date += (self.rng.log_normal(3.0 * 3600.0, 1.4) as i64).clamp(60, 20 * DAY);
                if date > self.now {
                    break;
                }
                from_me = if from_me { false } else { self.rng.chance(0.4) };
            }
            let other = self.rng.pick(&others).clone();
            let other = (other.name, other.addr);
            let (from, to) = if from_me {
                (me.clone(), other)
            } else {
                (other, me.clone())
            };
            let cc: Vec<(String, String)> = others
                .iter()
                .filter(|person| person.addr != from.1 && person.addr != to.1)
                .map(|person| (person.name.clone(), person.addr.clone()))
                .collect();
            let parent = match members.as_slice() {
                [] => None,
                [.., last] if self.rng.chance(0.85) => Some(*last),
                earlier => Some(*self.rng.pick(earlier)),
            };
            let subject = if position == 0 {
                base.clone()
            } else {
                let prefix = match lang {
                    Lang::No if self.rng.chance(0.6) => "SV: ",
                    Lang::Zh => "回复：",
                    _ => "Re: ",
                };
                format!("{prefix}{base}")
            };
            let folder = if from_me {
                Folder::Sent
            } else if self.now - date > 120 * DAY && self.rng.chance(0.45) {
                Folder::Archive
            } else if self.rng.chance(0.01) {
                Folder::Trash
            } else {
                Folder::Inbox
            };
            let files = has_files && (position == 0 || self.rng.chance(0.5));
            let index = self.push(
                Kind::Conversation,
                lang,
                folder,
                date,
                from,
                vec![to],
                cc,
                subject,
                parent,
                files,
            );
            members.push(index);
        }
    }

    fn single(&mut self, kind: Kind) {
        let lang = self.lang(kind);
        let date = self.date();
        let me = (ME_NAME.to_owned(), ME_ADDR.to_owned());
        let (folder, from, to, files) = match kind {
            Kind::Newsletter => {
                let (name, local, _) = *self.rng.pick(NEWSLETTERS);
                let folder = if self.rng.chance(0.4) {
                    Folder::Lists
                } else {
                    Folder::Inbox
                };
                (
                    folder,
                    (name.to_owned(), format!("{local}@{DOMAIN}")),
                    me,
                    false,
                )
            }
            Kind::Notice => {
                let (name, local) = *self.rng.pick(SENDERS);
                let folder = if self.now - date > 120 * DAY && self.rng.chance(0.5) {
                    Folder::Archive
                } else {
                    Folder::Inbox
                };
                (
                    folder,
                    (name.to_owned(), format!("{local}@{DOMAIN}")),
                    me,
                    self.rng.chance(0.6),
                )
            }
            Kind::Junk => {
                let (name, local) = *self.rng.pick(JUNK_SENDERS);
                (
                    Folder::Junk,
                    (name.to_owned(), format!("{local}@{DOMAIN}")),
                    me,
                    false,
                )
            }
            Kind::Draft | Kind::Conversation => {
                let other = self.person_for(lang);
                (
                    Folder::Drafts,
                    me,
                    (other.name, other.addr),
                    self.rng.chance(0.1),
                )
            }
        };
        let subject = self.subject(lang, kind);
        self.push(
            kind,
            lang,
            folder,
            date,
            from,
            vec![to],
            Vec::new(),
            subject,
            None,
            files,
        );
    }
}

/// A text part's headers and body in the spec's charset and transfer encoding.
fn text_part(
    content_type: &str,
    text: &str,
    charset: Charset,
    transfer: TransferEncoding,
) -> String {
    let charset = match charset {
        Charset::Legacy(encoding) if encoding.encode(text).2 => Charset::Utf8,
        Charset::Latin1 if encoding_rs::WINDOWS_1252.encode(text).2 => Charset::Utf8,
        charset => charset,
    };
    let (label, bytes): (&str, Vec<u8>) = match charset {
        Charset::Utf8 => ("utf-8", text.as_bytes().to_vec()),
        Charset::Latin1 => (
            "iso-8859-1",
            encoding_rs::WINDOWS_1252.encode(text).0.into_owned(),
        ),
        Charset::Legacy(encoding) => (
            if encoding == encoding_rs::GBK {
                "gb2312"
            } else {
                encoding.name()
            },
            encoding.encode(text).0.into_owned(),
        ),
    };
    let crlf_bytes = String::from_utf8_lossy(&bytes).replace('\n', "\r\n");
    let (encoding, body) = if bytes.is_ascii() {
        ("7bit", crlf_bytes)
    } else {
        match (charset, transfer) {
            (Charset::Legacy(encoding), _) if encoding == encoding_rs::ISO_2022_JP => (
                "7bit",
                String::from_utf8_lossy(&bytes).replace('\n', "\r\n"),
            ),
            (_, TransferEncoding::Base64) | (Charset::Legacy(_), _) => {
                ("base64", mime::base64_lines(&bytes))
            }
            (Charset::Utf8, TransferEncoding::EightBit) => ("8bit", crlf_bytes),
            _ => ("quoted-printable", mime::quoted_printable(&bytes)),
        }
    };
    format!(
        "Content-Type: {content_type}; charset=\"{label}\"{CRLF}Content-Transfer-Encoding: {encoding}{CRLF}{CRLF}{body}{}",
        if body.ends_with(CRLF) { "" } else { CRLF }
    )
}

fn attachment_part(attachment: &AttachmentSpec) -> String {
    let mime_type = match attachment.kind {
        AttachmentKind::Pdf => "application/pdf",
        AttachmentKind::Csv => "text/csv",
        AttachmentKind::Png => "image/png",
        AttachmentKind::Text => "text/plain",
    };
    let charset = if matches!(attachment.kind, AttachmentKind::Csv | AttachmentKind::Text) {
        "; charset=\"utf-8\""
    } else {
        ""
    };
    let fallback = mime::text(&attachment.name);
    format!(
        "Content-Type: {mime_type}{charset}; name=\"{fallback}\"{CRLF}\
         Content-Disposition: attachment; filename*={}{CRLF}\
         Content-Transfer-Encoding: base64{CRLF}{CRLF}{}",
        mime::filename_star(&attachment.name),
        mime::base64_lines(&attachment_bytes(attachment))
    )
}

fn inline_image_part(cid: &str) -> String {
    format!(
        "Content-Type: image/png; name=\"logo.png\"{CRLF}Content-ID: <{cid}>{CRLF}\
         Content-Disposition: inline; filename=\"logo.png\"{CRLF}\
         Content-Transfer-Encoding: base64{CRLF}{CRLF}{}",
        mime::base64_lines(&PNG)
    )
}

fn multipart(subtype: &str, boundary: &str, parts: &[String]) -> String {
    let mut out = format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"{CRLF}{CRLF}");
    for part in parts {
        out.push_str(&format!("--{boundary}{CRLF}{part}"));
        if !part.ends_with(CRLF) {
            out.push_str(CRLF);
        }
    }
    out.push_str(&format!("--{boundary}--{CRLF}"));
    out
}

fn render(spec: &Spec, index: usize, seed: u64) -> Vec<u8> {
    let boundary = |n: u32| format!("=_sealwax_{seed:x}_{index}_{n}");
    let cid = format!("img-{index}.{seed:x}@{DOMAIN}");
    let body_text = plain_body(&spec.body);
    let html = match spec.newsletter {
        Some(newsletter) => newsletter_html(
            spec,
            newsletter,
            (spec.shape == Shape::Related).then_some(cid.as_str()),
        ),
        None => html_body(
            &spec.body,
            (spec.shape == Shape::Related).then_some(cid.as_str()),
        ),
    };
    let utf8 = |content_type: &str, text: &str| {
        text_part(
            content_type,
            text,
            Charset::Utf8,
            TransferEncoding::QuotedPrintable,
        )
    };
    let content = match spec.shape {
        Shape::Plain => text_part("text/plain", &body_text, spec.charset, spec.transfer),
        Shape::Html => utf8("text/html", &html),
        Shape::Alternative => multipart(
            "alternative",
            &boundary(1),
            &[utf8("text/plain", &body_text), utf8("text/html", &html)],
        ),
        Shape::Related => multipart(
            "related",
            &boundary(1),
            &[
                multipart(
                    "alternative",
                    &boundary(2),
                    &[utf8("text/plain", &body_text), utf8("text/html", &html)],
                ),
                inline_image_part(&cid),
            ],
        ),
    };
    let content = if spec.attachments.is_empty() {
        content
    } else {
        let mut parts = vec![content];
        parts.extend(spec.attachments.iter().map(attachment_part));
        multipart("mixed", &boundary(0), &parts)
    };

    let mut headers = String::new();
    if let Some(number) = spec.copy {
        headers.push_str(&mime::field(
            "Received",
            &format!(
                "from mx.{DOMAIN} by {DOMAIN} with LMTP id {number}; {}",
                mime::date(spec.date + 7, 0)
            ),
        ));
    }
    headers.push_str(&mime::field("Date", &mime::date(spec.date, spec.zone)));
    headers.push_str(&mime::field(
        "From",
        &mime::mailbox(&spec.from.0, &spec.from.1, spec.names),
    ));
    let list = |people: &[(String, String)]| {
        people
            .iter()
            .map(|(name, addr)| mime::mailbox(name, addr, spec.names))
            .collect::<Vec<_>>()
            .join(", ")
    };
    headers.push_str(&mime::field("To", &list(&spec.to)));
    if !spec.cc.is_empty() {
        headers.push_str(&mime::field("Cc", &list(&spec.cc)));
    }
    let subject = match spec.charset {
        Charset::Legacy(encoding)
            if !spec.subject.is_ascii() && !encoding.encode(&spec.subject).2 =>
        {
            mime::encoded_words(&spec.subject, encoding)
        }
        _ => mime::text(&spec.subject),
    };
    headers.push_str(&mime::field("Subject", &subject));
    headers.push_str(&mime::field(
        "Message-ID",
        &format!("<{}>", spec.message_id),
    ));
    if let Some(parent) = &spec.in_reply_to {
        headers.push_str(&mime::field("In-Reply-To", &format!("<{parent}>")));
    }
    if !spec.references.is_empty() {
        let references: Vec<String> = spec.references.iter().map(|id| format!("<{id}>")).collect();
        headers.push_str(&mime::field("References", &references.join(" ")));
    }
    if let Some(newsletter) = spec.newsletter {
        let (name, local, list_name) = NEWSLETTERS[newsletter];
        headers.push_str(&mime::field(
            "List-Id",
            &format!("{} <{local}.lists.{DOMAIN}>", mime::text(list_name)),
        ));
        headers.push_str(&mime::field(
            "List-Unsubscribe",
            &format!(
                "<https://{DOMAIN}/n/{local}/unsubscribe>, <mailto:unsubscribe+{local}@{DOMAIN}>"
            ),
        ));
        let _ = name;
    }
    headers.push_str(&mime::field("MIME-Version", "1.0"));
    let mut raw = headers.into_bytes();
    raw.extend_from_slice(content.as_bytes());
    match spec.malformed {
        Some(malformation) => malform(raw, malformation, spec),
        None => raw,
    }
}

fn remove_field(raw: &mut Vec<u8>, name: &str) {
    let text = String::from_utf8_lossy(raw).into_owned();
    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    let mut in_headers = true;
    for line in text.split_inclusive("\r\n") {
        if in_headers && line == "\r\n" {
            in_headers = false;
        }
        if in_headers {
            if skipping && (line.starts_with(' ') || line.starts_with('\t')) {
                continue;
            }
            skipping = line
                .to_ascii_lowercase()
                .starts_with(&format!("{}:", name.to_ascii_lowercase()));
            if skipping {
                continue;
            }
        }
        out.push_str(line);
    }
    *raw = out.into_bytes();
}

fn malform(mut raw: Vec<u8>, malformation: Malformation, spec: &Spec) -> Vec<u8> {
    match malformation {
        Malformation::NoDate => remove_field(&mut raw, "Date"),
        Malformation::BadDate => {
            remove_field(&mut raw, "Date");
            let mut out = b"Date: sometime next week, probably\r\n".to_vec();
            out.extend(raw);
            raw = out;
        }
        Malformation::NoMessageId => remove_field(&mut raw, "Message-ID"),
        Malformation::UnclosedMultipart => {
            let text = String::from_utf8_lossy(&raw).into_owned();
            let mut out = format!(
                "Content-Type: multipart/mixed; boundary=\"never-closed\"\r\n{}",
                text.replace("Content-Type:", "X-Old-Content-Type:")
            );
            out.push_str("\r\n--never-closed\r\nContent-Type: text/plain\r\n\r\ntruncated");
            raw = out.into_bytes();
        }
        Malformation::TruncatedBase64 => {
            raw.extend_from_slice(
                b"--=_broken\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK!!!%%%=\r\n",
            );
            let cut = raw.len() * 3 / 4;
            raw.truncate(cut);
        }
        Malformation::UnknownCharset => {
            let text = String::from_utf8_lossy(&raw)
                .replace("charset=\"utf-8\"", "charset=\"x-sealwax-unknown\"");
            raw = text.into_bytes();
        }
        Malformation::RawEightBitSubject => {
            remove_field(&mut raw, "Subject");
            let mut out = b"Subject: R\xe5 tekst i emnet: bl\xe5b\xe6r \xf8l\r\n".to_vec();
            out.extend(raw);
            raw = out;
        }
        Malformation::GarbageHeaderLines => {
            let mut out = b"this line has no colon\r\n:: also broken\r\nX-Empty:\r\n\t continuation of nothing\r\n".to_vec();
            out.extend(raw);
            raw = out;
        }
        Malformation::NoHeaders => {
            raw = format!("{}\r\n", plain_body(&spec.body).replace('\n', "\r\n")).into_bytes();
        }
        Malformation::NulBytes => {
            let at = raw.len() / 2;
            raw.splice(at..at, [0u8, 0, 0, b'x', 0]);
        }
        Malformation::LongHeaderLine => {
            let mut out = format!("X-Long: {}\r\n", "lang-linje ".repeat(600)).into_bytes();
            out.extend(raw);
            raw = out;
        }
    }
    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::message;
    use sealwax_domain::model::BodyKind;

    fn config(count: usize, seed: u64) -> Config {
        Config {
            count,
            seed,
            now: DEFAULT_NOW,
            years: 5.0,
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_bytes() {
        let a = Corpus::generate(config(600, 3));
        let b = Corpus::generate(config(600, 3));
        let c = Corpus::generate(config(600, 4));
        assert_eq!(a.len(), 600);
        for index in [0, 1, 299, 599] {
            assert_eq!(a.render(index), b.render(index));
            assert_eq!(a.file_name(index), b.file_name(index));
        }
        assert_ne!(a.render(0), c.render(0));
        assert!(
            a.specs.windows(2).all(|pair| pair[0].date >= pair[1].date),
            "newest first"
        );
    }

    #[test]
    fn covers_the_cases_and_stays_fictional() {
        let corpus = Corpus::generate(config(4_000, 1));
        let mut kinds = [0usize; 4];
        let mut legacy = 0;
        let mut long_subjects = 0;
        let mut malformed = 0;
        let mut replies = 0;
        let mut embedded = 0;
        let mut attached = 0;
        for index in 0..corpus.len() {
            let raw = corpus.render(index);
            let spec = &corpus.specs[index];
            let text = String::from_utf8_lossy(&raw);
            for word in text.split(|c: char| c.is_whitespace() || "<>,;\"()".contains(c)) {
                if let Some((_, domain)) = word.split_once('@') {
                    let domain = domain.trim_end_matches(['.', '>', ':']);
                    let looks_like_a_domain = domain.contains('.')
                        && domain
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
                    assert!(
                        !looks_like_a_domain
                            || domain == DOMAIN
                            || domain.ends_with(&format!(".{DOMAIN}")),
                        "{word} in message {index}"
                    );
                }
            }
            let parsed = message::parse(&raw);
            if spec.malformed.is_some() {
                malformed += 1;
                continue;
            }
            kinds[match parsed.body_kind {
                BodyKind::Plain => 0,
                BodyKind::Html => 1,
                BodyKind::Both => 2,
                BodyKind::None => 3,
            }] += 1;
            assert_eq!(
                parsed.headers.message_id.as_deref(),
                Some(spec.message_id.as_str())
            );
            assert_eq!(parsed.headers.date, Some(spec.date), "message {index}");
            assert_eq!(parsed.headers.subject, spec.subject, "message {index}");
            assert_eq!(parsed.headers.from[0].email, spec.from.1);
            assert_eq!(parsed.headers.from[0].name, spec.from.0, "message {index}");
            assert!(!parsed.snippet.is_empty(), "message {index}");
            legacy += usize::from(matches!(spec.charset, Charset::Legacy(_)));
            long_subjects += usize::from(spec.subject.chars().count() > 150);
            replies += usize::from(!parsed.headers.references.is_empty());
            embedded += usize::from(parsed.attachments.iter().any(|a| a.is_embedded()));
            attached += usize::from(parsed.has_attachments());
            if spec.shape == Shape::Related {
                assert!(parsed.attachments.iter().any(|a| a.is_embedded()));
            }
        }
        assert!(
            kinds[0] > 500 && kinds[1] > 200 && kinds[2] > 500,
            "{kinds:?}"
        );
        assert_eq!(kinds[3], 0);
        assert!(legacy > 10, "{legacy}");
        assert!(long_subjects > 20, "{long_subjects}");
        assert!(malformed > 10, "{malformed}");
        assert!(replies > 800, "{replies}");
        assert!(embedded > 50, "{embedded}");
        assert!(attached > 200, "{attached}");
    }
}
