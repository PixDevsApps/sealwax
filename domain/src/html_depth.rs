//! How deep HTML from mail or a draft nests, so that nothing builds a tree deeper than WebKit
//! would (M9, N1 and N2 in docs/m9/hardening.md). html2text drops a render tree it didn't finish
//! recursively: under 14,563 nested lists it overflowed a 2 MiB thread, which aborts the process.
//! html5ever's tree builder and html2text also take time quadratic in the depth.
//!
//! [`Nesting`] reads html5ever's tokens in one linear pass and keeps the stack of open elements
//! the tree builder would keep, closely enough to count it: end tags HTML lets mail leave out
//! (`p`, `li`, `td`…) close as the tree builder closes them, an end tag closes only within its
//! scope, a table's implied `tbody` and `tr` count, and a formatting element closed without its
//! end tag counts until that end tag comes, as the tree builder opens it again (at most three
//! alike: its "Noah's Ark" rule). Where it can't follow (quirks, foreign content) it counts high.
//! [`too_deep`] says whether HTML nests deeper than [`MAX_DEPTH`]; the cleaner leaves out the
//! start tags deeper than that.
//!
//! [`render`] is the backstop for whatever the count misses: html2text renders a tree flattened
//! below [`MAX_TREE_DEPTH`].

use html2text::render::TextDecorator;
use html2text::{Comment, Element, Handle, RcDom};
use html5ever::LocalName;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use std::cell::{Cell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

/// The deepest nesting of elements Sealwax hands to html2text or ammonia: WebKit's limit
/// (`maximumHTMLParserDOMTreeDepth`), beyond which WebKit's own parser stops nesting.
pub const MAX_DEPTH: usize = 512;

/// The deepest tree html2text renders, whatever [`Nesting`] counted: elements below it are
/// left out and their text kept.
pub const MAX_TREE_DEPTH: usize = 2 * MAX_DEPTH;

const VOID: [&str; 19] = [
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "image", "img",
    "input", "keygen", "link", "meta", "param", "source", "track", "wbr",
];

/// Formatting elements: the tree builder keeps them in its list and opens them again.
const FORMATTING: [&str; 14] = [
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

/// Start tags that close an open `p` in button scope (headings, items and tables have rules of
/// their own).
const CLOSES_P: [&str; 28] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hgroup",
    "listing",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "search",
    "section",
    "summary",
    "ul",
];

/// Popped by "generate implied end tags" (`rtc` last: `rp` and `rt` keep it).
const IMPLIED_END: [&str; 10] = [
    "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
];

/// A table's parts: in body, outside any table, the tree builder ignores their start tags.
const TABLE_PARTS: [&str; 9] = [
    "caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr",
];

fn heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// Whether the tree builder opens the formatting elements closed without their end tag again
/// before it inserts an element for this start tag (inline elements do, blocks don't).
fn reconstructs(name: &str) -> bool {
    !(CLOSES_P.contains(&name)
        || heading(name)
        || TABLE_PARTS.contains(&name)
        || matches!(
            name,
            "li" | "dd"
                | "dt"
                | "table"
                | "hr"
                | "param"
                | "source"
                | "track"
                | "textarea"
                | "iframe"
                | "noembed"
                | "noframes"
                | "plaintext"
                | "rb"
                | "rtc"
                | "rp"
                | "rt"
                | "html"
                | "head"
                | "body"
                | "frameset"
                | "frame"
                | "script"
                | "style"
                | "title"
                | "template"
                | "base"
                | "basefont"
                | "bgsound"
                | "link"
                | "meta"
        ))
}

/// The HTML elements the specification calls special.
fn special(name: &str) -> bool {
    heading(name)
        || matches!(
            name,
            "address"
                | "applet"
                | "area"
                | "article"
                | "aside"
                | "base"
                | "basefont"
                | "bgsound"
                | "blockquote"
                | "body"
                | "br"
                | "button"
                | "caption"
                | "center"
                | "col"
                | "colgroup"
                | "dd"
                | "details"
                | "dir"
                | "div"
                | "dl"
                | "dt"
                | "embed"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "frame"
                | "frameset"
                | "head"
                | "header"
                | "hgroup"
                | "hr"
                | "html"
                | "iframe"
                | "img"
                | "input"
                | "isindex"
                | "li"
                | "link"
                | "listing"
                | "main"
                | "marquee"
                | "menu"
                | "meta"
                | "nav"
                | "noembed"
                | "noframes"
                | "noscript"
                | "object"
                | "ol"
                | "p"
                | "param"
                | "plaintext"
                | "pre"
                | "script"
                | "search"
                | "section"
                | "select"
                | "source"
                | "style"
                | "summary"
                | "table"
                | "tbody"
                | "td"
                | "template"
                | "textarea"
                | "tfoot"
                | "th"
                | "thead"
                | "title"
                | "tr"
                | "track"
                | "ul"
                | "wbr"
                | "xmp"
        )
}

/// MathML text and SVG HTML integration points: HTML inside them is parsed as HTML.
fn integration_point(name: &str) -> bool {
    matches!(
        name,
        "mi" | "mo" | "mn" | "ms" | "mtext" | "foreignobject" | "desc" | "title"
    )
}

/// HTML start tags that end foreign content.
fn breaks_out(tag: &Tag) -> bool {
    match &*tag.name {
        "b" | "big" | "blockquote" | "body" | "br" | "center" | "code" | "dd" | "div" | "dl"
        | "dt" | "em" | "embed" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "head" | "hr" | "i"
        | "img" | "li" | "listing" | "menu" | "meta" | "nobr" | "ol" | "p" | "pre" | "ruby"
        | "s" | "small" | "span" | "strong" | "strike" | "sub" | "sup" | "table" | "tt" | "u"
        | "ul" | "var" => true,
        "font" => tag
            .attrs
            .iter()
            .any(|attr| matches!(&*attr.name.local, "color" | "face" | "size")),
        _ => false,
    }
}

/// Where a search for an open element stops.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

impl Scope {
    fn stops_at(self, open: &Open) -> bool {
        let name: &str = &open.name;
        if open.foreign {
            return self != Scope::Table && integration_point(name);
        }
        match self {
            Scope::Table => matches!(name, "html" | "table" | "template"),
            _ => {
                matches!(
                    name,
                    "applet"
                        | "caption"
                        | "html"
                        | "table"
                        | "td"
                        | "th"
                        | "marquee"
                        | "object"
                        | "select"
                        | "template"
                ) || self == Scope::ListItem && matches!(name, "ol" | "ul")
                    || self == Scope::Button && name == "button"
            }
        }
    }
}

/// A formatting element's place in the tree builder's list: the order it was opened in, and a
/// signature of its attributes for the "Noah's Ark" rule.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Entry {
    seq: u64,
    signature: u64,
}

struct Open {
    name: LocalName,
    /// A formatting element in the list (the rule can take an open one off it).
    formatting: Option<Entry>,
    /// A cell, caption, `applet`, `marquee`, `object` or `template`: a marker in the list.
    marker: Option<u64>,
    foreign: bool,
}

/// The tree builder's stack of open elements, as counted from the tokens (the module's
/// comment). Every step is bounded by the limit, so a whole document is read in linear time.
pub struct Nesting {
    limit: usize,
    /// `noscript` holds markup (html2text parses without scripting) or text (ammonia).
    scripting: bool,
    stack: Vec<Open>,
    /// Formatting elements closed without their end tag, oldest first: the tree builder opens
    /// them again where text or an inline element comes.
    reopened: Vec<(LocalName, Entry)>,
    markers: Vec<u64>,
    seq: u64,
    /// The tree builder's form element pointer: set by a form opened outside a template, so
    /// another is ignored, and cleared by `</form>` only.
    form: bool,
}

impl Nesting {
    pub fn new(limit: usize, scripting: bool) -> Nesting {
        Nesting {
            limit,
            scripting,
            stack: Vec::new(),
            reopened: Vec::new(),
            markers: Vec::new(),
            seq: 0,
            form: false,
        }
    }

    /// The depth counted: the open elements and those the tree builder would open again.
    pub fn depth(&self) -> usize {
        self.stack.len() + self.reopened.len()
    }

    /// How the tokenizer reads the content of an element named `name` that holds raw text.
    pub fn raw_text(&self, name: &str) -> Option<TokenSinkResult<()>> {
        match name {
            "script" => Some(TokenSinkResult::RawData(RawKind::ScriptData)),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                Some(TokenSinkResult::RawData(RawKind::Rawtext))
            }
            "noscript" if self.scripting => Some(TokenSinkResult::RawData(RawKind::Rawtext)),
            "title" | "textarea" => Some(TokenSinkResult::RawData(RawKind::Rcdata)),
            "plaintext" => Some(TokenSinkResult::Plaintext),
            _ => None,
        }
    }

    /// A start tag. `false` when it would open an element at the limit: it is left out.
    pub fn start(&mut self, tag: &Tag) -> bool {
        let name: &str = &tag.name;
        if self.in_foreign() {
            if !breaks_out(tag) {
                return tag.self_closing || self.depth() < self.limit && self.push(tag, true);
            }
            while self.in_foreign() {
                self.pop();
            }
        }
        if VOID.contains(&name) {
            match name {
                "hr" => self.close_p(),
                "col" if self.table_context().is_some() && self.depth() < self.limit => {
                    self.clear_to_table(false);
                    self.phantom("colgroup");
                }
                _ if reconstructs(name) => self.reconstruct(),
                _ => {}
            }
            return true;
        }
        let foreign = matches!(name, "svg" | "math");
        if matches!(name, "html" | "head" | "body" | "frameset")
            || name == "form" && self.form && !self.in_template()
            || foreign && tag.self_closing
        {
            return true;
        }
        let table = TABLE_PARTS.contains(&name) || name == "table";
        let context = if table { self.table_context() } else { None };
        if TABLE_PARTS.contains(&name) && context.is_none() {
            return true;
        }
        if self.depth() >= self.limit {
            return false;
        }
        if self
            .stack
            .last()
            .is_some_and(|open| &*open.name == "colgroup")
        {
            self.pop();
        }
        match name {
            "caption" | "colgroup" | "tbody" | "thead" | "tfoot" => self.clear_to_table(false),
            "tr" => {
                if !self.clear_to_any(&["tbody", "thead", "tfoot"]) {
                    self.clear_to_table(false);
                    self.phantom("tbody");
                }
            }
            "td" | "th" => {
                if !self.clear_to_any(&["tr"]) {
                    if !self.clear_to_any(&["tbody", "thead", "tfoot"]) {
                        self.clear_to_table(false);
                        self.phantom("tbody");
                    }
                    self.phantom("tr");
                }
            }
            "table" => {
                if matches!(
                    context,
                    Some("table" | "tbody" | "thead" | "tfoot" | "tr" | "colgroup")
                ) {
                    self.clear_to_table(true);
                }
            }
            "li" => self.close_item(&["li"]),
            "dd" | "dt" => self.close_item(&["dd", "dt"]),
            _ if heading(name) => {
                self.close_p();
                if self.stack.last().is_some_and(|open| heading(&open.name)) {
                    self.pop();
                }
            }
            "button" => {
                if let Some(index) = self.find(Scope::Default, |open| open == "button") {
                    self.pop_to(index);
                }
            }
            "select" => {
                if let Some(index) = self.find(Scope::Default, |open| open == "select") {
                    self.pop_to(index);
                    return true;
                }
            }
            "option" | "optgroup" => {
                if self.find(Scope::Default, |open| open == "select").is_some() {
                    self.pop_implied(&IMPLIED_END);
                } else if self
                    .stack
                    .last()
                    .is_some_and(|open| &*open.name == "option")
                {
                    self.pop();
                }
            }
            "rb" | "rtc" | "rp" | "rt" => {
                if self.find(Scope::Default, |open| open == "ruby").is_some() {
                    let keep_rtc = usize::from(matches!(name, "rp" | "rt"));
                    self.pop_implied(&IMPLIED_END[..IMPLIED_END.len() - keep_rtc]);
                }
            }
            "nobr" => {
                if self.in_list(name) {
                    self.end_formatting(name);
                }
            }
            "a" => {
                let open = self
                    .latest_active(name)
                    .and_then(|at| self.stack[at].formatting);
                let reopened = self.latest_reopened(name).map(|at| self.reopened[at].1);
                let newest = [open, reopened]
                    .into_iter()
                    .flatten()
                    .max_by_key(|entry| entry.seq);
                if let Some(entry) = newest {
                    self.end_formatting(name);
                    self.reopened.retain(|(_, e)| *e != entry);
                    if let Some(at) = self
                        .stack
                        .iter()
                        .position(|open| open.formatting == Some(entry))
                    {
                        self.remove(at);
                    }
                }
            }
            _ if CLOSES_P.contains(&name) => self.close_p(),
            _ => {}
        }
        if reconstructs(name) {
            self.reconstruct();
        }
        self.push(tag, foreign)
    }

    /// Text: the tree builder opens again the formatting elements closed without their end tag
    /// (not in raw text).
    pub fn text(&mut self) {
        if !self
            .stack
            .last()
            .is_some_and(|open| self.raw_text(&open.name).is_some())
        {
            self.reconstruct();
        }
    }

    /// An end tag.
    pub fn end(&mut self, tag: &Tag) {
        let name: &str = &tag.name;
        if matches!(name, "p" | "br") {
            while self.in_foreign() {
                self.pop();
            }
        }
        if self.in_foreign()
            && let Some(index) = self
                .stack
                .iter()
                .rposition(|open| !open.foreign || &*open.name == name)
            && self.stack[index].foreign
        {
            self.pop_to(index);
            return;
        }
        if self.raw_text(name).is_some() || name == "noscript" {
            if self.stack.last().is_some_and(|open| &*open.name == name) {
                self.pop();
            }
            return;
        }
        if FORMATTING.contains(&name) {
            self.end_formatting(name);
            return;
        }
        if name == "form" {
            let template = self.in_template();
            let pointer = self.form;
            self.form &= template;
            if !template && !pointer {
                return;
            }
            if let Some(index) = self.find(Scope::Default, |open| open == "form") {
                if template {
                    self.pop_to(index);
                } else {
                    self.pop_implied(&IMPLIED_END);
                    self.remove(index);
                }
            }
            return;
        }
        if heading(name) {
            if let Some(index) = self.find(Scope::Default, heading) {
                self.pop_to(index);
            }
            return;
        }
        let scope = match name {
            "p" => Scope::Button,
            "li" => Scope::ListItem,
            "table" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th" | "caption" => Scope::Table,
            "body" | "html" | "br" | "col" | "colgroup" => return self.end_other(name),
            _ if special(name) => Scope::Default,
            _ => return self.end_other(name),
        };
        if let Some(index) = self.find(scope, |open| open == name) {
            self.pop_to(index);
            if matches!(name, "applet" | "marquee" | "object") {
                self.clear_to_marker();
            }
        }
    }

    fn in_foreign(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|open| open.foreign && !integration_point(&open.name))
    }

    /// The innermost table part open, if a table is (a template starts over).
    fn table_context(&self) -> Option<&'static str> {
        const PARTS: [&str; 10] = [
            "table", "tbody", "thead", "tfoot", "tr", "td", "th", "caption", "colgroup", "template",
        ];
        self.stack
            .iter()
            .rev()
            .filter(|open| !open.foreign)
            .find_map(|open| PARTS.into_iter().find(|part| *part == &*open.name))
            .filter(|part| *part != "template")
    }

    fn push(&mut self, tag: &Tag, foreign: bool) -> bool {
        let name: &str = &tag.name;
        let mut open = Open {
            name: tag.name.clone(),
            formatting: None,
            marker: None,
            foreign,
        };
        if !foreign && FORMATTING.contains(&name) {
            let mut hasher = DefaultHasher::new();
            for attr in &tag.attrs {
                attr.name.local.hash(&mut hasher);
                attr.value.hash(&mut hasher);
            }
            let entry = Entry {
                seq: self.next_seq(),
                signature: hasher.finish(),
            };
            self.noahs_ark(&tag.name, entry.signature);
            open.formatting = Some(entry);
        } else if !foreign
            && matches!(
                name,
                "td" | "th" | "caption" | "applet" | "marquee" | "object" | "template"
            )
        {
            let seq = self.next_seq();
            self.markers.push(seq);
            open.marker = Some(seq);
        }
        if name == "form" && !self.in_template() {
            self.form = true;
        }
        self.stack.push(open);
        true
    }

    fn in_template(&self) -> bool {
        self.stack
            .iter()
            .any(|open| !open.foreign && &*open.name == "template")
    }

    /// An element the tree builder inserts by itself (a table's `tbody` and `tr`).
    fn phantom(&mut self, name: &'static str) {
        self.stack.push(Open {
            name: LocalName::from(name),
            formatting: None,
            marker: None,
            foreign: false,
        });
    }

    /// "Reconstruct the active formatting elements": those closed without their end tag after
    /// the last open one and the last marker are opened again, oldest first.
    fn reconstruct(&mut self) {
        if self.reopened.is_empty() {
            return;
        }
        let open = self
            .stack
            .iter()
            .filter_map(|open| open.formatting.map(|entry| entry.seq))
            .max()
            .unwrap_or(0);
        let after = open.max(self.last_marker());
        let at = self
            .reopened
            .partition_point(|(_, entry)| entry.seq <= after);
        let reopened: Vec<_> = self.reopened.drain(at..).collect();
        self.stack
            .extend(reopened.into_iter().map(|(name, entry)| Open {
                name,
                formatting: Some(entry),
                marker: None,
                foreign: false,
            }));
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn last_marker(&self) -> u64 {
        self.markers.last().copied().unwrap_or(0)
    }

    /// The "Noah's Ark" rule: a fourth formatting element alike after the last marker takes the
    /// oldest off the list.
    fn noahs_ark(&mut self, name: &LocalName, signature: u64) {
        let marker = self.last_marker();
        let alike = |n: &LocalName, entry: &Entry| {
            n == name && entry.signature == signature && entry.seq > marker
        };
        let mut found: Vec<u64> = self
            .stack
            .iter()
            .filter_map(|open| open.formatting.map(|entry| (&open.name, entry)))
            .chain(self.reopened.iter().map(|(n, entry)| (n, *entry)))
            .filter(|(n, entry)| alike(n, entry))
            .map(|(_, entry)| entry.seq)
            .collect();
        if found.len() < 3 {
            return;
        }
        found.sort_unstable();
        let oldest = found[0];
        if let Some(index) = self.reopened.iter().position(|(_, e)| e.seq == oldest) {
            self.reopened.remove(index);
        } else if let Some(open) = self
            .stack
            .iter_mut()
            .find(|open| open.formatting.is_some_and(|e| e.seq == oldest))
        {
            open.formatting = None;
        }
    }

    /// Takes the top element off.
    fn pop(&mut self) {
        if let Some(open) = self.stack.pop() {
            self.closed(open);
        }
    }

    /// An element left the stack: a formatting element stays in the list, and a cell, caption
    /// or template closing clears the list to the last marker (the tree builder clears it for
    /// `applet`, `marquee` and `object` only at their own end tag; otherwise their marker stays).
    fn closed(&mut self, open: Open) {
        if let Some(entry) = open.formatting {
            let at = self.reopened.partition_point(|(_, e)| e.seq < entry.seq);
            self.reopened.insert(at, (open.name.clone(), entry));
        }
        if open.marker.is_some() && matches!(&*open.name, "td" | "th" | "caption" | "template") {
            self.clear_to_marker();
        }
    }

    /// "Clear the list of active formatting elements up to the last marker".
    fn clear_to_marker(&mut self) {
        let Some(marker) = self.markers.pop() else {
            return;
        };
        let at = self.reopened.partition_point(|(_, e)| e.seq < marker);
        self.reopened.truncate(at);
        for open in &mut self.stack {
            if open.formatting.is_some_and(|entry| entry.seq > marker) {
                open.formatting = None;
            }
        }
    }

    /// Pops up to and including `index`.
    fn pop_to(&mut self, index: usize) {
        while self.stack.len() > index {
            self.pop();
        }
    }

    /// Takes the element at `index` out of the stack and out of the list.
    fn remove(&mut self, index: usize) {
        let mut open = self.stack.remove(index);
        open.formatting = None;
        self.closed(open);
    }

    /// The topmost open HTML element `matches` accepts, searching down to `scope`'s boundary.
    fn find(&self, scope: Scope, matches: impl Fn(&str) -> bool) -> Option<usize> {
        for (index, open) in self.stack.iter().enumerate().rev() {
            if !open.foreign && matches(&open.name) {
                return Some(index);
            }
            if scope.stops_at(open) {
                return None;
            }
        }
        None
    }

    fn close_p(&mut self) {
        if let Some(index) = self.find(Scope::Button, |open| open == "p") {
            self.pop_to(index);
        }
    }

    /// `li`, `dd` and `dt`: the previous item closes unless a special element other than
    /// `address`, `div` or `p` comes first; then an open `p` closes.
    fn close_item(&mut self, items: &[&str]) {
        for index in (0..self.stack.len()).rev() {
            let name: &str = &self.stack[index].name;
            if items.contains(&name) {
                self.pop_to(index);
                break;
            }
            if special(name) && !matches!(name, "address" | "div" | "p") {
                break;
            }
        }
        self.close_p();
    }

    /// Pops the elements in `names` from the top.
    fn pop_implied(&mut self, names: &[&str]) {
        while self
            .stack
            .last()
            .is_some_and(|open| names.contains(&&*open.name))
        {
            self.pop();
        }
    }

    /// Pops back to the innermost open table, popping it too with `table`.
    fn clear_to_table(&mut self, table: bool) {
        if let Some(index) = self.find(Scope::Table, |open| open == "table") {
            self.pop_to(if table { index } else { index + 1 });
        }
    }

    /// Pops back to the innermost of `names` inside the innermost table, keeping it; `false`
    /// when none is open.
    fn clear_to_any(&mut self, names: &[&str]) -> bool {
        match self.find(Scope::Table, |open| names.contains(&open)) {
            Some(index) => {
                self.pop_to(index + 1);
                true
            }
            None => false,
        }
    }

    /// The topmost open formatting element named `name` still in the list, after the last
    /// marker.
    fn latest_active(&self, name: &str) -> Option<usize> {
        let marker = self.last_marker();
        self.stack.iter().rposition(|open| {
            &*open.name == name && open.formatting.is_some_and(|entry| entry.seq > marker)
        })
    }

    /// The newest formatting element named `name` closed without its end tag, after the last
    /// marker.
    fn latest_reopened(&self, name: &str) -> Option<usize> {
        let marker = self.last_marker();
        self.reopened
            .iter()
            .rposition(|(open, entry)| &**open == name && entry.seq > marker)
    }

    fn in_list(&self, name: &str) -> bool {
        self.latest_active(name).is_some() || self.latest_reopened(name).is_some()
    }

    /// A formatting element's end tag, the adoption agency algorithm in outline. The newest
    /// element of its name in the list closes with what is open above it. With special elements
    /// above it, those stay open: it moves past each (up to eight), the elements between them
    /// that aren't formatting elements in the list close, and it closes with what is above the
    /// last.
    fn end_formatting(&mut self, name: &str) {
        let open = self.latest_active(name);
        let reopened = self.latest_reopened(name);
        let open_seq = open.and_then(|index| self.stack[index].formatting.map(|entry| entry.seq));
        let index = match (open, reopened) {
            (_, Some(index)) if Some(self.reopened[index].1.seq) > open_seq => {
                self.reopened.remove(index);
                return;
            }
            (Some(index), _) => index,
            _ => return self.end_other(name),
        };
        if self.stack[index + 1..]
            .iter()
            .any(|open| Scope::Default.stops_at(open))
        {
            return;
        }
        let blocks = |stack: &[Open]| -> Vec<usize> {
            (index + 1..stack.len())
                .filter(|&at| !stack[at].foreign && special(&stack[at].name))
                .take(8)
                .collect()
        };
        let found = blocks(&self.stack);
        let Some(&last) = found.last() else {
            self.stack[index].formatting = None;
            self.pop_to(index);
            return;
        };
        let moves_on = found.len() == 8;
        for at in (index + 1..last).rev() {
            let open = &self.stack[at];
            if open.foreign || open.formatting.is_none() && !special(&open.name) {
                self.stack.remove(at);
            }
        }
        let last = blocks(&self.stack).last().copied().unwrap_or(index);
        if moves_on {
            let open = self.stack.remove(index);
            self.stack.insert(last, open);
        } else {
            self.pop_to(last + 1);
            self.remove(index);
        }
    }

    /// Any other end tag: closes the topmost element of its name, unless a special element
    /// comes first.
    fn end_other(&mut self, name: &str) {
        for index in (0..self.stack.len()).rev() {
            let open: &str = &self.stack[index].name;
            if open == name {
                self.pop_to(index);
                return;
            }
            if special(open) {
                return;
            }
        }
    }
}

/// Reads tags into [`Nesting`] until one would nest deeper than its limit.
struct Measure {
    nesting: RefCell<Nesting>,
    deep: Cell<bool>,
}

impl TokenSink for Measure {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let mut nesting = self.nesting.borrow_mut();
        let tag = match token {
            Token::TagToken(tag) => tag,
            Token::CharacterTokens(_) => {
                nesting.text();
                return TokenSinkResult::Continue;
            }
            _ => return TokenSinkResult::Continue,
        };
        match tag.kind {
            TagKind::StartTag => {
                if !nesting.start(&tag) {
                    self.deep.set(true);
                    // Pauses the tokenizer: the answer is known.
                    return TokenSinkResult::Script(());
                }
                nesting
                    .raw_text(&tag.name)
                    .unwrap_or(TokenSinkResult::Continue)
            }
            TagKind::EndTag => {
                nesting.end(&tag);
                TokenSinkResult::Continue
            }
        }
    }
}

/// Whether `html` nests deeper than [`MAX_DEPTH`] as html2text's parser (without scripting)
/// would build it. Stops reading at the first element too deep.
pub fn too_deep(html: &str) -> bool {
    let sink = Measure {
        nesting: RefCell::new(Nesting::new(MAX_DEPTH, false)),
        deep: Cell::new(false),
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    if !tokenizer.sink.deep.get() {
        tokenizer.end();
    }
    tokenizer.sink.deep.get()
}

/// html2text's text of `html` at `width` columns, from a tree at most [`MAX_TREE_DEPTH`] deep:
/// `string_from_read`'s steps, with the tree flattened below the limit before html2text reads
/// it.
pub fn render<D: TextDecorator>(
    config: html2text::config::Config<D>,
    html: &str,
    width: usize,
) -> Result<String, html2text::Error> {
    let dom = config.parse_html(html.as_bytes())?;
    flatten(&dom, MAX_TREE_DEPTH);
    let tree = config.dom_to_render_tree(&dom)?;
    config.render_to_string(tree, width)
}

/// Replaces what is below each node at `depth` with the text in it, in order, as WebKit keeps
/// content and drops nesting past its limit. Iterative: the tree may be arbitrarily deep.
fn flatten(dom: &RcDom, depth: usize) {
    let mut pending: Vec<(Handle, usize)> = vec![(dom.document.clone(), 0)];
    while let Some((node, level)) = pending.pop() {
        if level < depth {
            pending.extend(
                node.children
                    .borrow()
                    .iter()
                    .map(|child| (child.clone(), level + 1)),
            );
            continue;
        }
        let mut below: Vec<Handle> = std::mem::take(&mut *node.children.borrow_mut());
        below.reverse();
        let mut text = Vec::new();
        while let Some(child) = below.pop() {
            match child.data {
                Element { .. } => {
                    let mut inside = std::mem::take(&mut *child.children.borrow_mut());
                    inside.reverse();
                    below.extend(inside);
                }
                Comment { .. } => {}
                _ => {
                    child.parent.set(Some(Rc::downgrade(&node)));
                    text.push(child);
                }
            }
        }
        *node.children.borrow_mut() = text;
    }
}

#[cfg(test)]
mod tests;
