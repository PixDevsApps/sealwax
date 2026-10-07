//! The allowlist cleaner for HTML that goes into a draft without passing through the composer's
//! paste cleaner: the original's body in a reply or forward (the quick reply sends without any
//! WebView), and a draft read back from the Drafts folder. The rules are S7's paste cleaner's
//! (`spikes/s7-composer/src/helper.js`), with remote (`https:`) and `cid:` images kept: a quote
//! shows the original as it was (SECURITY.md "Composer"; the composer's CSP never loads them).
//!
//! Two passes. The first maps presentational markup to the tags S7 keeps: `strong` and `em`
//! become `b` and `i`; bold, italic, underline and line-through in a `style` become `b`, `i`,
//! `u` and `s` inside the element; Google Docs' `<b style="font-weight:normal">` wrapper is
//! unwrapped; sectioning blocks become `div`; every `blockquote` becomes a cite quote or an
//! indent in the composer's own form; images whose source isn't allowed are left out. The
//! second pass, ammonia, is the allowlist: everything it doesn't allow is gone whatever the
//! first pass made of the input.

use super::text::escape;
use ammonia::{Builder, UrlRelative};
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::{Attribute, LocalName};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::LazyLock;

/// The style of a cite quote, as the composer writes it (S7).
pub const QUOTE_STYLE: &str = "margin:0 0 0 0.8ex;border-left:2px solid #8a8a8a;padding-left:1ex";
/// WebKit's indent (S7): a `blockquote` with this style is an indent, not a quote.
pub const INDENT_STYLE: &str = "margin: 0 0 0 40px; border: none; padding: 0px;";
/// The class of the `div` that holds a signature, so the composer can replace it when the
/// sender changes. The only class the cleaner keeps.
pub const SIGNATURE_CLASS: &str = "sw-signature";

/// Removed with everything inside them.
const DROP: [&str; 47] = [
    "script",
    "style",
    "iframe",
    "frame",
    "frameset",
    "object",
    "embed",
    "applet",
    "form",
    "input",
    "button",
    "select",
    "textarea",
    "option",
    "optgroup",
    "label",
    "fieldset",
    "legend",
    "datalist",
    "output",
    "meter",
    "progress",
    "link",
    "meta",
    "base",
    "head",
    "title",
    "svg",
    "math",
    "template",
    "noscript",
    "noembed",
    "noframes",
    "audio",
    "video",
    "source",
    "track",
    "canvas",
    "picture",
    "map",
    "area",
    "dialog",
    "slot",
    "portal",
    "param",
    "xmp",
    "plaintext",
];
/// Kept, with the attributes [`builder`] allows. `strong` and `em` arrive as `b` and `i`.
const KEEP: [&str; 34] = [
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
/// Block elements that become `div`.
const BLOCKS: [&str; 17] = [
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "main",
    "aside",
    "figure",
    "figcaption",
    "address",
    "details",
    "summary",
    "center",
    "dl",
    "dt",
    "dd",
    "hgroup",
];
const VOID: [&str; 16] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr", "keygen", "frame",
];

/// Cleaned HTML and what it refers to. Its `Debug` prints lengths and counts only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub html: String,
    /// The Content-IDs its `cid:` images show (decoded, without angle brackets), each once.
    pub content_ids: Vec<String>,
    /// How many `https:` images it shows.
    pub remote_images: usize,
}

impl fmt::Debug for Cleaned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cleaned")
            .field("html_len", &self.html.len())
            .field("content_ids", &self.content_ids.len())
            .field("remote_images", &self.remote_images)
            .finish()
    }
}

/// `html` through the allowlist, every `cid:` image kept.
pub fn clean(html: &str) -> Cleaned {
    clean_with(html, |_| true)
}

/// `html` through the allowlist; a `cid:` image is kept only when `keep_cid` accepts its
/// Content-ID (the caller has that part). White space around the result (a document's line
/// breaks between `head` and `body`) is trimmed.
pub fn clean_with(html: &str, keep_cid: impl Fn(&str) -> bool) -> Cleaned {
    let cleaned = clean_once(html, &keep_cid);
    if cleaned.content_ids.iter().all(|id| keep_cid(id)) {
        return cleaned;
    }
    // The first pass reads tags as a tokenizer does, the allowlist as a tree, and markup such
    // as `<svg><p>` leaves foreign content in one but not in the other: an image the first pass
    // took for part of a dropped element can survive. The allowlist's output has no foreign
    // content, so a second round over it sees every image (found by fuzzing, ML-093).
    clean_once(&cleaned.html, &keep_cid)
}

fn clean_once(html: &str, keep_cid: &dyn Fn(&str) -> bool) -> Cleaned {
    let normalized = normalize(html, keep_cid);
    let html = BUILDER
        .clean(&normalized)
        .to_string()
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .to_owned();
    let sources = image_sources(&html);
    let mut content_ids: Vec<String> = Vec::new();
    for source in &sources {
        if has_prefix(source, "cid:") {
            let id = percent_decode(&source[4..]);
            if !content_ids.contains(&id) {
                content_ids.push(id);
            }
        }
    }
    let remote_images = sources
        .iter()
        .filter(|source| has_prefix(source, "https:"))
        .count();
    Cleaned {
        html,
        content_ids,
        remote_images,
    }
}

/// The trimmed `src` of every image in serialized HTML, character references decoded.
fn image_sources(html: &str) -> Vec<String> {
    struct Sources(RefCell<Vec<String>>);

    impl TokenSink for Sources {
        type Handle = ();

        fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
            if let Token::TagToken(tag) = token
                && tag.kind == TagKind::StartTag
                && &*tag.name == "img"
                && let Some(source) = attribute(&tag.attrs, "src")
            {
                self.0.borrow_mut().push(source.trim().to_owned());
            }
            TokenSinkResult::Continue
        }
    }

    let tokenizer = Tokenizer::new(Sources(RefCell::new(Vec::new())), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.take()
}

/// RFC 2392: a `cid:` URL is the Content-ID percent-encoded.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = bytes.get(index + 1..index + 3)
            && hex.iter().all(u8::is_ascii_hexdigit)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

static BUILDER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut builder = Builder::empty();
    let attributes = |names: &[&'static str]| names.iter().copied().collect::<HashSet<_>>();
    builder
        .tags(KEEP.into_iter().collect())
        .clean_content_tags(DROP.into_iter().collect())
        .tag_attributes(HashMap::from([
            ("a", attributes(&["href"])),
            ("img", attributes(&["src", "alt", "width", "height"])),
            ("td", attributes(&["colspan", "rowspan"])),
            ("th", attributes(&["colspan", "rowspan"])),
            ("ol", attributes(&["start"])),
            ("blockquote", attributes(&["type", "style"])),
        ]))
        .allowed_classes(HashMap::from([("div", attributes(&[SIGNATURE_CLASS]))]))
        .generic_attributes(HashSet::new())
        .url_schemes(attributes(&["http", "https", "mailto", "cid", "data"]))
        .url_relative(UrlRelative::Deny)
        .link_rel(None)
        .strip_comments(true)
        .attribute_filter(|element, attribute, value| {
            allowed_value(element, attribute, value).then_some(Cow::Borrowed(value))
        });
    builder
});

fn allowed_value(element: &str, attribute: &str, value: &str) -> bool {
    let digits =
        |max: usize| (1..=max).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit());
    match (element, attribute) {
        ("a", "href") => is_link(value),
        ("img", "src") => is_image_source(value),
        ("img", "alt") => true,
        ("img", "width" | "height") => digits(5),
        ("td" | "th", "colspan" | "rowspan") => digits(3),
        ("ol", "start") => {
            let number = value.strip_prefix('-').unwrap_or(value);
            (1..=6).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit())
        }
        ("blockquote", "type") => value == "cite",
        ("blockquote", "style") => value == QUOTE_STYLE || value == INDENT_STYLE,
        ("div", "class") => true,
        _ => false,
    }
}

fn has_prefix(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// Links go to the web or to an address only.
fn is_link(value: &str) -> bool {
    let value = value.trim();
    has_prefix(value, "http://") || has_prefix(value, "https://") || has_prefix(value, "mailto:")
}

/// Images: a part of the message, an image inline as `data:` in a type every client shows, or
/// a remote `https:` image (never `http:`).
fn is_image_source(value: &str) -> bool {
    let value = value.trim();
    if has_prefix(value, "https://") {
        return value.len() > 8;
    }
    if has_prefix(value, "cid:") {
        return value.len() > 4;
    }
    let Some(rest) = ["png", "jpeg", "gif", "webp"].into_iter().find_map(|kind| {
        let prefix = format!("data:image/{kind};base64,");
        has_prefix(value, &prefix).then(|| &value[prefix.len()..])
    }) else {
        return false;
    };
    rest.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b) || b.is_ascii_whitespace())
}

/// The first pass: presentational markup as the tags the allowlist keeps.
fn normalize(html: &str, keep_cid: &dyn Fn(&str) -> bool) -> String {
    let sink = Normalizer {
        out: RefCell::new(String::with_capacity(html.len())),
        stack: RefCell::new(Vec::new()),
        raw: Cell::new(Raw::No),
        keep_cid,
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.out.take()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Raw {
    No,
    /// Text written as it came (script, style and other raw text).
    Verbatim,
    /// Character references decoded by the tokenizer (title, textarea): escaped again.
    Escaped,
}

/// An element the first pass has open.
struct Open {
    name: LocalName,
    /// The tag written for it, if any (an unwrapped `b` writes none).
    written: Option<String>,
    /// The emphasis tags written inside it, outermost first.
    wrappers: Vec<&'static str>,
    /// Inside a dropped element nothing is changed: the allowlist removes it all.
    dropped: bool,
}

struct Normalizer<'k> {
    out: RefCell<String>,
    stack: RefCell<Vec<Open>>,
    raw: Cell<Raw>,
    keep_cid: &'k dyn Fn(&str) -> bool,
}

impl TokenSink for Normalizer<'_> {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        match token {
            Token::TagToken(tag) => match tag.kind {
                TagKind::StartTag => return self.start(&tag),
                TagKind::EndTag => self.end(&tag),
            },
            Token::CharacterTokens(text) => {
                let mut out = self.out.borrow_mut();
                match self.raw.get() {
                    Raw::Verbatim => out.push_str(&text),
                    Raw::No | Raw::Escaped => {
                        for c in text.chars() {
                            match c {
                                '&' => out.push_str("&amp;"),
                                '<' => out.push_str("&lt;"),
                                '>' => out.push_str("&gt;"),
                                c => out.push(c),
                            }
                        }
                    }
                }
            }
            Token::DoctypeToken(_)
            | Token::CommentToken(_)
            | Token::NullCharacterToken
            | Token::EOFToken
            | Token::ParseError(_) => {}
        }
        TokenSinkResult::Continue
    }
}

impl Normalizer<'_> {
    fn start(&self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        let void = VOID.contains(&name);
        let in_drop = self.stack.borrow().iter().any(|open| open.dropped);
        let raw = match name {
            "script" => Some((Raw::Verbatim, TokenSinkResult::RawData(RawKind::ScriptData))),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
                Some((Raw::Verbatim, TokenSinkResult::RawData(RawKind::Rawtext)))
            }
            "title" | "textarea" => Some((Raw::Escaped, TokenSinkResult::RawData(RawKind::Rcdata))),
            "plaintext" => Some((Raw::Verbatim, TokenSinkResult::Plaintext)),
            _ => None,
        };
        if in_drop || DROP.contains(&name) || raw.is_some() {
            self.write_tag(name, &tag.attrs, false);
            if !void {
                self.stack.borrow_mut().push(Open {
                    name: tag.name.clone(),
                    written: Some(name.to_owned()),
                    wrappers: Vec::new(),
                    dropped: true,
                });
            }
            return match raw {
                Some((kind, result)) => {
                    self.raw.set(kind);
                    result
                }
                None => TokenSinkResult::Continue,
            };
        }
        if name == "img" && !self.image_kept(&tag.attrs) {
            return TokenSinkResult::Continue;
        }
        let style = attribute(&tag.attrs, "style").unwrap_or_default();
        let (emphasis, normal_weight) = emphasis(style);
        let semantic = semantic(name);
        let written = (semantic != Some("b") || !normal_weight).then(|| renamed(name).to_owned());
        if let Some(written) = &written {
            self.write_tag(written, &tag.attrs, true);
        }
        if void {
            return TokenSinkResult::Continue;
        }
        let wrappers: Vec<&'static str> = emphasis
            .into_iter()
            .filter(|wrapper| Some(*wrapper) != semantic)
            .collect();
        {
            let mut out = self.out.borrow_mut();
            for wrapper in &wrappers {
                out.push('<');
                out.push_str(wrapper);
                out.push('>');
            }
        }
        self.stack.borrow_mut().push(Open {
            name: tag.name.clone(),
            written,
            wrappers,
            dropped: false,
        });
        TokenSinkResult::Continue
    }

    /// Closes the element the end tag names, with what the first pass wrote inside it and
    /// inside the elements it implicitly closes; an end tag with no open element passes on.
    fn end(&self, tag: &Tag) {
        self.raw.set(Raw::No);
        let found = self
            .stack
            .borrow()
            .iter()
            .rposition(|open| open.name == tag.name);
        let mut out = self.out.borrow_mut();
        let Some(index) = found else {
            out.push_str("</");
            out.push_str(renamed(&tag.name));
            out.push('>');
            return;
        };
        let closed = self.stack.borrow_mut().split_off(index);
        for (depth, open) in closed.iter().enumerate().rev() {
            for wrapper in open.wrappers.iter().rev() {
                out.push_str("</");
                out.push_str(wrapper);
                out.push('>');
            }
            if depth == 0
                && let Some(written) = &open.written
            {
                out.push_str("</");
                out.push_str(written);
                out.push('>');
            }
        }
    }

    fn image_kept(&self, attrs: &[Attribute]) -> bool {
        let Some(source) = attribute(attrs, "src") else {
            return false;
        };
        if !is_image_source(source) {
            return false;
        }
        let source = source.trim();
        !has_prefix(source, "cid:") || (self.keep_cid)(&percent_decode(&source[4..]))
    }

    /// Writes a start tag. A `kept` tag (outside dropped elements) loses its `style`, which the
    /// emphasis tags replace, and a blockquote takes the composer's form; the allowlist decides
    /// about every other attribute.
    fn write_tag(&self, name: &str, attrs: &[Attribute], kept: bool) {
        let mut out = self.out.borrow_mut();
        out.push('<');
        out.push_str(name);
        if kept && name == "blockquote" {
            let style = attribute(attrs, "style").unwrap_or_default();
            if style.contains("margin") && style.contains("40px") {
                out.push_str(" style=\"");
                out.push_str(INDENT_STYLE);
            } else {
                out.push_str(" type=\"cite\" style=\"");
                out.push_str(QUOTE_STYLE);
            }
            out.push_str("\">");
            return;
        }
        for attr in attrs {
            let attribute_name: &str = &attr.name.local;
            let plain = !attribute_name.is_empty()
                && attribute_name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b));
            if !plain || kept && attribute_name == "style" {
                continue;
            }
            if kept && attribute_name == "class" {
                if name == "div" && attr.value.split_whitespace().any(|c| c == SIGNATURE_CLASS) {
                    out.push_str(" class=\"");
                    out.push_str(SIGNATURE_CLASS);
                    out.push('"');
                }
                continue;
            }
            out.push(' ');
            out.push_str(attribute_name);
            out.push_str("=\"");
            out.push_str(&escape(&attr.value));
            out.push('"');
        }
        out.push('>');
    }
}

/// The tag a kept element is written as.
fn renamed(name: &str) -> &str {
    match name {
        "strong" => "b",
        "em" => "i",
        _ if BLOCKS.contains(&name) => "div",
        _ => name,
    }
}

fn attribute<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| &*attr.value)
}

/// The emphasis a tag means by itself.
fn semantic(name: &str) -> Option<&'static str> {
    match name {
        "b" | "strong" => Some("b"),
        "i" | "em" => Some("i"),
        "u" => Some("u"),
        "s" | "strike" | "del" => Some("s"),
        _ => None,
    }
}

/// The emphasis an inline style asks for (bold, italic, underline, line-through, in that
/// order), and whether it sets the weight to normal.
fn emphasis(style: &str) -> (Vec<&'static str>, bool) {
    let mut bold = false;
    let mut normal = false;
    let mut italic = false;
    let mut underline = false;
    let mut strike = false;
    let heavy = |value: &str| {
        matches!(value, "bold" | "bolder") || value.parse::<u32>().is_ok_and(|weight| weight >= 600)
    };
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        let value = value.replace("!important", "");
        let value = value.trim();
        match property.as_str() {
            "font-weight" => {
                bold = heavy(value);
                normal = matches!(value, "normal" | "400" | "lighter");
            }
            "font-style" => italic = value.starts_with("italic") || value.starts_with("oblique"),
            "text-decoration" | "text-decoration-line" => {
                underline |= value.contains("underline");
                strike |= value.contains("line-through");
            }
            "font" => {
                for word in value.split_whitespace() {
                    bold |= heavy(word);
                    italic |= matches!(word, "italic" | "oblique");
                }
            }
            _ => {}
        }
    }
    let wrappers = [(bold, "b"), (italic, "i"), (underline, "u"), (strike, "s")]
        .into_iter()
        .filter_map(|(on, tag)| on.then_some(tag))
        .collect();
    (wrappers, normal && !bold)
}

#[cfg(test)]
mod tests;
