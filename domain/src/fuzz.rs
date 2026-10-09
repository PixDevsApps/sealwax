//! Entry points for the fuzz targets in `fuzz/` (ML-093, docs/m9/security.md). Each takes
//! arbitrary bytes, runs one parser of untrusted input the way Sealwax does, and checks what its
//! documentation promises; a failed check panics like a crash. The tests at the bottom run every
//! entry point over the checked-in corpus (`fuzz/corpus/<target>/`), so a crash once found stays
//! fixed. Built only for tests and with the `fuzz` feature.

use crate::account::{AccountKey, Identity};
use crate::autoconfig::{clientconfig, dns, redirect};
use crate::compose::{self, ReplyKind, address, clean, mime, quote, reply, stored, text};
use crate::config::Config;
use crate::keymap::{Binding, Keymap};
use crate::model::{AccountId, Address, MessageId};
use crate::search::{self, Date, hints};
use crate::sieve::{self, Folders, MessageView, Outcome, RuleSet};
use crate::theme::{Palette, resolve_colors};
use crate::{
    accounts_file, attachment, flowed, html_scan, links, mailto, message, quotes, reading, snippet,
};
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Parts read again per message, as the reader does for its bodies and inline images; capped so
/// one input with thousands of parts stays fast enough to fuzz.
const MAX_PARTS: usize = 16;

/// The fictional address autodetect's parsers fill templates with.
const ADDRESS: &str = "kari.nordmann@example.test";

/// Splits `data` at its first NUL into two strings (the second empty without one).
fn two(data: &[u8]) -> (String, String) {
    let (first, second) = match data.iter().position(|&byte| byte == 0) {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (data, &[][..]),
    };
    (
        String::from_utf8_lossy(first).into_owned(),
        String::from_utf8_lossy(second).into_owned(),
    )
}

/// Raw RFC 5322 bytes through what ingest (`message::parse`) and the reader (`reading::inspect`,
/// `message::part`, format=flowed, quotes, the HTML scan, safe names) do with a message, and what
/// a reply, a forward and a draft read back from the Drafts folder take from it.
pub fn message(data: &[u8]) {
    let parsed = message::parse(data);
    assert_eq!(parsed.size, data.len() as u64);
    assert!(parsed.body_text.len() <= message::SEARCH_TEXT_LIMIT);
    assert!(parsed.snippet.chars().count() <= snippet::SNIPPET_CHARS);
    let headers = &parsed.headers;
    for field in std::iter::once(&headers.subject).chain(
        [&headers.from, &headers.to, &headers.cc, &headers.reply_to]
            .into_iter()
            .flatten()
            .flat_map(|address| [&address.name, &address.email]),
    ) {
        assert!(
            !field.chars().any(char::is_control),
            "a control character in a header field"
        );
    }
    let _ = message::base_subject(&headers.subject);

    let inspection = reading::inspect(data);
    let mut html = String::new();
    for &index in parsed.text_parts.iter().take(MAX_PARTS) {
        let Some(text) = message::part(data, index).and_then(|part| part.text) else {
            continue;
        };
        let text = match inspection.flowed.iter().find(|(part, _)| *part == index) {
            Some((_, format)) => flowed::decode(&text, *format),
            None => text,
        };
        plain_body(&text);
    }
    for &index in parsed.html_parts.iter().take(MAX_PARTS) {
        if let Some(text) = message::part(data, index).and_then(|part| part.text) {
            html.push_str(&text);
        }
    }
    if !html.is_empty() {
        let _ = html_scan::scan(&html);
        let cleaned = clean::clean(&html);
        check_cleaned(&cleaned.html);
        let _ = quote::reply_html("On a day, someone wrote:", &cleaned.html);
        let fields = quote::forward_fields(headers, None);
        let _ = quote::forward_html(&fields, &cleaned.html);
    }
    for attachment in parsed.attachments.iter().take(MAX_PARTS) {
        let _ = message::part(data, attachment.part);
        safe_name(attachment.name.as_deref(), &attachment.mime_type);
        if let Some(id) = &attachment.content_id {
            let _ = message::part_by_content_id(data, id);
        }
    }
    if let Some(draft) = stored::parse(data) {
        let _ = format!("{draft:?}");
    }
    answers(data);
}

/// A reply, reply all and forward to the message, built for sending: the quote keeps only what
/// the allowlist keeps, and nothing in the original can add a header line to the sent copy.
fn answers(data: &[u8]) {
    let original = reply::Original::parse(data);
    let senders = [Identity {
        name: "Kari Nordmann".to_owned(),
        address: ADDRESS.to_owned(),
        signature: Some("Kari".to_owned()),
    }];
    for kind in [ReplyKind::Reply, ReplyKind::ReplyAll, ReplyKind::Forward] {
        let planned = reply::plan(&original, kind, &senders);
        check_cleaned(&planned.quote_html);
        assert!(!planned.subject.chars().any(char::is_control));
        for id in planned.in_reply_to.iter().chain(&planned.references) {
            assert!(!id.chars().any(|c| c.is_whitespace() || c.is_control()));
        }
        let message_id = compose::ids::message_id(ADDRESS, [7; 16]).expect("an id");
        let draft = planned.draft(
            AccountId(1),
            MessageId(1),
            message_id,
            "<div>Hi</div>",
            Vec::new(),
        );
        let Ok(outgoing) = mime::build(&draft, 1_791_360_000, &[]) else {
            continue;
        };
        let head_end = outgoing
            .transmitted
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("a header and a body");
        let head = String::from_utf8_lossy(&outgoing.transmitted[..head_end]);
        for line in head.split("\r\n") {
            if line.starts_with([' ', '\t']) {
                continue;
            }
            let name = line.split_once(':').map_or(line, |(name, _)| name);
            assert!(
                SENT_HEADERS.contains(&name),
                "a header line {name:?} in the sent message"
            );
        }
    }
}

/// The header fields Sealwax writes.
const SENT_HEADERS: [&str; 11] = [
    "From",
    "To",
    "Cc",
    "Subject",
    "Date",
    "Message-ID",
    "In-Reply-To",
    "References",
    "MIME-Version",
    "Content-Type",
    "Content-Transfer-Encoding",
];

/// A plain-text body as the reader and a reply see it.
fn plain_body(text: &str) {
    if let Some(quote) = quotes::trailing_quote(text) {
        assert!(quote.start <= text.len() && text.is_char_boundary(quote.start));
        assert!(quote.start == 0 || text[..quote.start].ends_with('\n'));
    }
    let _ = reading::inline_pgp(text);
    found_links(text);
    let _ = quote::reply_text("On a day, someone wrote:", text);
    let html = text::to_html(text);
    check_cleaned(&clean::clean(&html).html);
}

/// HTML as the reader scans it, as search and snippets read it, and through the composer's
/// allowlist cleaner, whose output must hold only what the allowlist keeps.
pub fn html(data: &[u8]) {
    let html = String::from_utf8_lossy(data);
    let _ = html_scan::scan(&html);
    let text = message::html_to_text(&html);
    let _ = snippet::snippet(&text, snippet::SNIPPET_CHARS);
    let _ = mime::plain_text(&html);
    let cleaned = clean::clean(&html);
    check_cleaned(&cleaned.html);
    check_cleaned(&clean::clean(&cleaned.html).html);
    let none = clean::clean_with(&html, |_| false);
    assert!(
        none.content_ids.is_empty(),
        "a cid: image the caller doesn't hold"
    );
}

/// Tags the cleaner keeps, and the attributes it keeps on them.
fn allowed(tag: &str, attribute: &str, value: &str) -> bool {
    // A browser strips white space around a URL, as the cleaner does before its checks.
    let lower = value.trim().to_ascii_lowercase();
    match (tag, attribute) {
        ("a", "href") => ["http:", "https:", "mailto:"]
            .iter()
            .any(|scheme| lower.starts_with(scheme)),
        ("img", "src") => {
            lower.starts_with("https:")
                || lower.starts_with("cid:")
                || lower.starts_with("data:image/")
        }
        ("img", "alt" | "width" | "height")
        | ("td" | "th", "colspan" | "rowspan")
        | ("ol", "start")
        | ("blockquote", "type" | "style") => true,
        // ammonia leaves `class=""` where it removed every class.
        ("div", "class") => value.is_empty() || value == clean::SIGNATURE_CLASS,
        _ => false,
    }
}

const KEPT: [&str; 34] = [
    "p",
    "div",
    "br",
    "b",
    "i",
    "u",
    "s",
    "strike",
    "del",
    "sub",
    "sup",
    "a",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "code",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
    "caption",
    "img",
];

struct Checker {
    problem: RefCell<Option<String>>,
}

impl TokenSink for Checker {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let problem = match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
                let name: &str = &tag.name;
                if KEPT.contains(&name) {
                    tag.attrs
                        .iter()
                        .find(|attr| !allowed(name, &attr.name.local, &attr.value))
                        .map(|attr| format!("<{name} {}>", &*attr.name.local))
                } else {
                    Some(format!("<{name}>"))
                }
            }
            Token::TagToken(tag) if !KEPT.contains(&&*tag.name) => {
                Some(format!("</{}>", &*tag.name))
            }
            Token::CommentToken(_) => Some("a comment".to_owned()),
            _ => None,
        };
        if let Some(problem) = problem {
            self.problem.borrow_mut().get_or_insert(problem);
        }
        TokenSinkResult::Continue
    }
}

/// The cleaner's output, tokenized again: only allowlisted tags, attributes and URL schemes.
fn check_cleaned(html: &str) {
    let checker = Checker {
        problem: RefCell::new(None),
    };
    let tokenizer = Tokenizer::new(checker, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    if let Some(problem) = tokenizer.sink.problem.borrow().as_ref() {
        panic!("the cleaner kept {problem}");
    }
}

/// Plain text through format=flowed, the quote finder, snippets, links and the header helpers.
pub fn text(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    for delsp in [false, true] {
        plain_body(&flowed::decode(&text, flowed::Flowed { delsp }));
    }
    plain_body(&text);
    let snippet = snippet::snippet(&text, snippet::SNIPPET_CHARS);
    assert!(snippet.chars().count() <= snippet::SNIPPET_CHARS);
    let collapsed = snippet::collapse(&text, 80);
    assert!(collapsed.chars().count() <= 80);
    assert!(!collapsed.chars().any(char::is_control));
    let header = text::header_text(&text);
    assert!(
        !header
            .chars()
            .any(|c| c.is_control() || c == '\n' || c == '\r')
    );
    let _ = message::base_subject(&text);
    let _ = message::normalize_id(&text);
    let _ = message::thread_index(&text);
    let _ = compose::reply::reply_subject(&text);
    let _ = attachment::eml_file_name(&text);
}

/// Every link `links::find` returns is an http(s) or mailto URL over a span of the text.
fn found_links(text: &str) {
    let mut end = 0;
    for found in links::find(text) {
        assert!(found.start >= end && found.start < found.end && found.end <= text.len());
        assert!(text.is_char_boundary(found.start) && text.is_char_boundary(found.end));
        let target = found.target.to_ascii_lowercase();
        assert!(
            ["http://", "https://", "mailto:"]
                .iter()
                .any(|scheme| target.starts_with(scheme)),
            "a link to another scheme"
        );
        end = found.end;
    }
}

/// A link's visible text and its target (split at the first NUL), as the reader's mismatch
/// warning and status strip see them, and links found in text.
pub fn links(data: &[u8]) {
    let (text, url) = two(data);
    let _ = links::check(&text, &url);
    let _ = links::text_domain(&text);
    let _ = links::status_text(&url);
    if let Some(target) = links::target(&url) {
        assert!(matches!(
            target.scheme.as_str(),
            "http" | "https" | "mailto"
        ));
        assert!(!target.host.is_empty() && !target.host.ends_with('.'));
        assert_eq!(target.host, target.host.to_ascii_lowercase());
        let _ = links::registrable(&target.host);
        let _ = links::display_host(&target.host);
        let _ = links::confusable(&target.host);
    }
    found_links(&text);
    found_links(&url);
}

/// An attachment's name and MIME type as a message gives them (split at the first NUL).
pub fn attachment(data: &[u8]) {
    let (name, mime_type) = two(data);
    safe_name(Some(&name), &mime_type);
    let _ = attachment::extension(&name);
    let eml = attachment::eml_file_name(&name);
    assert!(eml.ends_with(".eml") && eml.len() <= attachment::MAX_NAME_BYTES);
    assert!(!eml.contains(['/', '\\']) && !eml.chars().any(char::is_control));
}

/// The safe name's promises, and that it never shows a type the danger check missed.
fn safe_name(name: Option<&str>, mime_type: &str) {
    let safe = attachment::safe_file_name(name, mime_type);
    assert!(
        !safe.is_empty() && safe.len() <= attachment::MAX_NAME_BYTES,
        "{safe:?}"
    );
    assert!(!safe.contains(['/', '\\']), "{safe:?}");
    assert!(!safe.chars().any(char::is_control), "{safe:?}");
    assert!(!safe.starts_with(['.', '-', ' ']), "{safe:?}");
    assert!(!safe.ends_with(['.', ' ']), "{safe:?}");
    assert!(!safe.contains(".."), "{safe:?}");
    if attachment::danger(Some(&safe), mime_type).is_some() {
        assert!(
            attachment::danger(name, mime_type).is_some(),
            "the safe name {safe:?} is dangerous but its original isn't"
        );
    }
    let unique = attachment::unique_name(&safe, |taken| taken == safe);
    assert!(unique != safe && unique.len() <= attachment::MAX_NAME_BYTES);
}

/// A clientConfig document from a domain's autoconfig or the ISPDB, and a redirect from one.
pub fn autoconfig(data: &[u8]) {
    let xml = String::from_utf8_lossy(data);
    if let Ok(config) = clientconfig::parse(&xml, ADDRESS) {
        for server in &config.servers {
            assert!(
                dns::acceptable_host(&server.host),
                "a server host autodetect refuses"
            );
        }
        let _ = config.finding();
    }
    let (template, url) = two(data);
    let _ = clientconfig::username_form(Some(&template), ADDRESS);
    let _ = dns::acceptable_host(&template);
    let _ = dns::registrable(&template);
    let _ = dns::within(&template, "example.test");
    if url.contains(ADDRESS) {
        assert!(redirect::carries(&url, ADDRESS));
    }
    let encoded: String = url
        .chars()
        .map(|c| match c {
            '@' => "%40".to_owned(),
            c => c.to_string(),
        })
        .collect();
    if url.contains(ADDRESS) {
        assert!(redirect::carries(&encoded, ADDRESS));
    }
}

/// A search query as typed, with the suggestions at every cursor position.
pub fn search(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    for query in [search::parse(&text), search::parse_typed(&text)] {
        let debug = format!("{query:?}");
        assert!(
            debug.is_ascii() && !debug.contains('@'),
            "the query's Debug shows its text"
        );
        assert!(debug.len() < 1024);
    }
    let today = Date::new(2026, 10, 7).expect("a date");
    let length = text.chars().count();
    for cursor in (0..=length).take(64) {
        if let Some(at) = hints::at(&text, cursor, today) {
            assert!(at.word.start <= at.word.end && at.word.end <= length);
            let (changed, cursor) = hints::accept(&text, at.word, "from:");
            assert!(cursor <= changed.chars().count());
        }
        let _ = hints::operators_at(&text, cursor);
    }
}

/// A `mailto:` link as the desktop hands it over, filled into a new draft, and recipients as
/// the composer's chip entry parses them.
pub fn mailto(data: &[u8]) {
    let uri = String::from_utf8_lossy(data);
    for uri in [uri.to_string(), format!("mailto:{uri}")] {
        let Ok(link) = mailto::parse(&uri) else {
            continue;
        };
        for field in [&link.subject, &link.in_reply_to].into_iter().flatten() {
            assert!(
                !field.chars().any(char::is_control),
                "a control character in a header"
            );
        }
        let mut draft = compose::Draft::default();
        link.fill(&mut draft);
        assert!(!draft.subject.chars().any(char::is_control));
        for recipient in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
            assert!(address::is_valid(&recipient.email));
        }
        let _ = format!("{link:?}");
    }
    let list = address::parse_list(&uri);
    for parsed in &list.addresses {
        let formatted = address::format(parsed);
        let again = address::parse_one(&formatted);
        assert_eq!(
            again.as_ref().map(|a| &a.email),
            Some(&parsed.email),
            "a formatted address reads back as another"
        );
        let _ = address::ascii(&parsed.email);
    }
    let _ = address::format_list(&list.addresses);
    let _ = address::dedupe(
        list.addresses
            .iter()
            .cloned()
            .chain([Address::new("", ADDRESS)]),
    );
}

/// `config.toml`, `keys.toml` and `accounts.toml` as a user may write them, and the assistant's
/// edits of `accounts.toml`.
pub fn config(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let _ = Config::parse(&text);
    let _ = Keymap::parse(&text);
    for line in text.lines().take(16) {
        let _ = Binding::parse(line);
    }
    let Ok(accounts) = accounts_file::parse(&text) else {
        return;
    };
    for account in &accounts {
        let without = accounts_file::remove(&text, &account.key)
            .expect("a file that reads removes")
            .expect("an account it holds");
        let left = accounts_file::parse(&without).expect("a removal leaves a file that reads");
        let expected: Vec<_> = accounts
            .iter()
            .filter(|a| a.key != account.key)
            .cloned()
            .collect();
        assert_eq!(left, expected, "a removal changed another account");
        let mut written = account.clone();
        written.signature = None;
        written.identities.clear();
        if let Err(accounts_file::EditError::Account(reason)) =
            accounts_file::add(&without, &written)
        {
            assert!(
                accounts_file::check(&written).is_err(),
                "an account that reads didn't write back: {reason}"
            );
        }
    }
    assert!(matches!(
        accounts_file::remove(&text, &AccountKey("no-such-key-anywhere".to_owned())),
        Ok(None)
    ));
}

/// A theme's `colors.toml` values (`key = value` lines) through the palette Sealwax derives.
pub fn theme(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let raw: BTreeMap<String, String> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            (
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            )
        })
        .collect();
    let resolved = resolve_colors(&raw);
    let _ = Palette::from_resolved(&resolved);
    let _ = Palette::from_colors(&raw);
}

/// A message for the Sieve target: `name: value` lines; an address header's addresses are each
/// comma-separated part's `<…>`, or the part itself when it holds an `@`.
struct SieveMessage<'a> {
    headers: Vec<(String, &'a str)>,
    size: u64,
    mailbox: &'static str,
}

impl<'a> SieveMessage<'a> {
    fn parse(text: &'a str, mailbox: &'static str) -> SieveMessage<'a> {
        let headers = text
            .lines()
            .take(64)
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim()))
            .collect();
        SieveMessage {
            headers,
            size: text.len() as u64,
            mailbox,
        }
    }
}

impl MessageView for SieveMessage<'_> {
    fn header(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(header, _)| header == name)
            .map(|(_, value)| *value)
            .collect()
    }

    fn addresses(&self, name: &str) -> Vec<&str> {
        self.header(name)
            .into_iter()
            .flat_map(|value| value.split(','))
            .filter_map(|part| match (part.find('<'), part.rfind('>')) {
                (Some(open), Some(close)) if open < close => Some(&part[open + 1..close]),
                _ => Some(part.trim()).filter(|part| part.contains('@')),
            })
            .collect()
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn flags(&self) -> Vec<&str> {
        vec!["\\Seen", "$Forwarded"]
    }

    fn mailbox(&self) -> &str {
        self.mailbox
    }
}

/// A Sieve script, and after its first NUL a message as `name: value` lines, through the rules'
/// parser, checker and evaluator (in the Inbox and in Junk); then the rules model read from the
/// script, written again and read back: the same rules, a script that checks, and for mail in
/// the Inbox the same outcome as the script read.
pub fn sieve(data: &[u8]) {
    let (text, message) = two(data);
    let script = match sieve::compile(&text) {
        Ok(script) => script,
        Err(error) => {
            let lines = text.split('\n').count();
            assert!(
                error.line >= 1 && error.line as usize <= lines && error.column >= 1,
                "an error outside the script"
            );
            return;
        }
    };
    let _ = script.headers();
    let inbox = SieveMessage::parse(&message, "INBOX");
    for message in [&inbox, &SieveMessage::parse(&message, "Junk")] {
        sieve_outcome(&sieve::evaluate(&script, message), message.mailbox);
    }
    let folders = Folders::default();
    let rules = RuleSet::read(&text, &folders).expect("a script that checks reads as rules");
    let written = rules
        .render(&folders)
        .expect("rules read from a script write");
    let rewritten = sieve::compile(&written).expect("written rules check");
    let again = RuleSet::read(&written, &folders).expect("written rules read");
    assert!(again == rules, "rules read back differently");
    assert!(
        sieve::evaluate(&script, &inbox) == sieve::evaluate(&rewritten, &inbox),
        "writing the rules again changed what they do"
    );
}

/// What an outcome promises: the message goes somewhere, each folder once and never the one it
/// is in, flags are IMAP flags, and no rule sets `\Deleted`.
fn sieve_outcome(outcome: &Outcome, mailbox: &str) {
    assert!(
        outcome.keep || !outcome.filed.is_empty() || outcome.discard,
        "a message neither kept, filed nor discarded"
    );
    assert!(outcome.keep || outcome.flags.is_empty());
    for (index, filed) in outcome.filed.iter().enumerate() {
        assert!(!filed.folder.is_empty() && !filed.folder.chars().any(char::is_control));
        assert!(filed.folder != mailbox, "filed into the folder it is in");
        assert!(
            outcome.filed[..index]
                .iter()
                .all(|other| other.folder != filed.folder),
            "filed twice into a folder"
        );
    }
    for change in std::iter::once(&outcome.flags).chain(outcome.filed.iter().map(|f| &f.flags)) {
        for flag in change.add.iter().chain(&change.remove) {
            assert!(!flag.is_empty() && !flag.chars().any(|c| c == ' ' || c.is_control()));
        }
        assert!(
            !change
                .add
                .iter()
                .any(|flag| flag.eq_ignore_ascii_case("\\Deleted")),
            "a rule set \\Deleted"
        );
        assert!(
            change.add.iter().all(|flag| !change
                .remove
                .iter()
                .any(|other| other.eq_ignore_ascii_case(flag))),
            "a flag both added and removed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Runs `target` over every file in `fuzz/corpus/<name>/`.
    fn corpus(name: &str, target: fn(&[u8])) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fuzz/corpus")
            .join(name);
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("a corpus entry").path();
            let data = std::fs::read(&path).expect("a corpus file");
            let result = std::panic::catch_unwind(|| target(&data));
            assert!(result.is_ok(), "{name}: {} fails", path.display());
            files += 1;
        }
        assert!(files > 0, "{name}: an empty corpus");
    }

    #[test]
    fn message_corpus() {
        corpus("message", super::message);
    }

    #[test]
    fn html_corpus() {
        corpus("html", super::html);
    }

    #[test]
    fn text_corpus() {
        corpus("text", super::text);
    }

    #[test]
    fn links_corpus() {
        corpus("links", super::links);
    }

    #[test]
    fn attachment_corpus() {
        corpus("attachment", super::attachment);
    }

    #[test]
    fn autoconfig_corpus() {
        corpus("autoconfig", super::autoconfig);
    }

    #[test]
    fn search_corpus() {
        corpus("search", super::search);
    }

    #[test]
    fn mailto_corpus() {
        corpus("mailto", super::mailto);
    }

    #[test]
    fn config_corpus() {
        corpus("config", super::config);
    }

    #[test]
    fn theme_corpus() {
        corpus("theme", super::theme);
    }

    #[test]
    fn sieve_corpus() {
        corpus("sieve", super::sieve);
    }
}
