//! What an HTML body brings with it, read without running it (DESIGN.md "HTML mail inside the
//! theme"): colours of its own, so it keeps its design on the paper card, and remote images, so
//! the reader offers to show them. A small tag scanner from S2; it decides nothing about
//! security, which the WebView's layers enforce whatever it says.

/// The scan of one HTML body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scan {
    /// A `style` attribute or `<style>` block sets a text or background colour, or the body uses
    /// `bgcolor`, a `background` attribute, `<font color>` or `<body text|link|vlink|alink>`.
    pub own_colours: bool,
    /// An image, a background or a poster comes from an `http:` or `https:` URL.
    pub remote_images: bool,
    /// The first thing that made it [`Scan::own_colours`], for the corpus report.
    pub evidence: Option<Evidence>,
}

/// What gave an HTML body colours of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Evidence {
    /// A `style` attribute sets a text or background colour.
    StyleAttribute,
    /// A `<style>` block does.
    StyleBlock,
    BgcolorAttribute,
    BackgroundAttribute,
    FontColor,
    /// `text`, `link`, `vlink` or `alink` on `<body>`.
    BodyColours,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Evidence::StyleAttribute => "style-attribute",
            Evidence::StyleBlock => "style-block",
            Evidence::BgcolorAttribute => "bgcolor",
            Evidence::BackgroundAttribute => "background-attribute",
            Evidence::FontColor => "font-color",
            Evidence::BodyColours => "body-colours",
        }
    }
}

impl Scan {
    fn found(&mut self, evidence: Evidence) {
        self.own_colours = true;
        self.evidence.get_or_insert(evidence);
    }
}

pub fn scan(html: &str) -> Scan {
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut scan = Scan::default();
    let mut at = 0;
    while let Some(offset) = lower[at..].find('<') {
        let start = at + offset;
        if lower[start..].starts_with("<!--") {
            at = lower[start..]
                .find("-->")
                .map_or(lower.len(), |end| start + end + 3);
            continue;
        }
        let closing = bytes.get(start + 1) == Some(&b'/');
        let (tag, attributes, end) = scan_tag(bytes, start + 1);
        at = end;
        if closing {
            continue;
        }
        for (name, value) in &attributes {
            match name.as_str() {
                "bgcolor" => scan.found(Evidence::BgcolorAttribute),
                "background" if !value.trim().is_empty() => {
                    scan.found(Evidence::BackgroundAttribute);
                    scan.remote_images |= is_remote(value);
                }
                "color" if tag == "font" => scan.found(Evidence::FontColor),
                "text" if tag == "body" => scan.found(Evidence::BodyColours),
                "style" => {
                    if declarations_set_colours(value, tag == "a") {
                        scan.found(Evidence::StyleAttribute);
                    }
                    scan.remote_images |= css_has_remote_url(value);
                }
                "src" | "poster" | "lowsrc" => scan.remote_images |= is_remote(value),
                "srcset" => scan.remote_images |= value.split(',').any(is_remote),
                "href" | "xlink:href" if matches!(tag.as_str(), "image" | "feimage") => {
                    scan.remote_images |= is_remote(value);
                }
                _ => {}
            }
        }
        if tag == "style" {
            let close = lower[at..]
                .find("</style")
                .map_or(lower.len(), |end| at + end);
            let css = &lower[at..close];
            if sheet_sets_colours(css) {
                scan.found(Evidence::StyleBlock);
            }
            scan.remote_images |= css_has_remote_url(css);
            at = close;
        }
    }
    scan
}

/// Reads a tag name and its attributes starting just after `<`; returns the position after `>`.
fn scan_tag(bytes: &[u8], mut at: usize) -> (String, Vec<(String, String)>, usize) {
    let name_start = at;
    while at < bytes.len()
        && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'/' || bytes[at] == b'!')
    {
        at += 1;
    }
    let tag = String::from_utf8_lossy(&bytes[name_start..at])
        .trim_start_matches('/')
        .to_owned();
    let mut attributes = Vec::new();
    loop {
        while at < bytes.len() && (bytes[at].is_ascii_whitespace() || bytes[at] == b'/') {
            at += 1;
        }
        if at >= bytes.len() {
            return (tag, attributes, at);
        }
        if bytes[at] == b'>' {
            return (tag, attributes, at + 1);
        }
        let attr_start = at;
        while at < bytes.len()
            && !matches!(bytes[at], b'=' | b'>' | b'/')
            && !bytes[at].is_ascii_whitespace()
        {
            at += 1;
        }
        let name = String::from_utf8_lossy(&bytes[attr_start..at]).into_owned();
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let mut value = String::new();
        if at < bytes.len() && bytes[at] == b'=' {
            at += 1;
            while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            if at < bytes.len() && matches!(bytes[at], b'"' | b'\'') {
                let quote = bytes[at];
                let value_start = at + 1;
                at = value_start;
                while at < bytes.len() && bytes[at] != quote {
                    at += 1;
                }
                value = String::from_utf8_lossy(&bytes[value_start..at]).into_owned();
                at = (at + 1).min(bytes.len());
            } else {
                let value_start = at;
                while at < bytes.len() && bytes[at] != b'>' && !bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                value = String::from_utf8_lossy(&bytes[value_start..at]).into_owned();
            }
        }
        if name.is_empty() {
            at += 1;
        } else {
            attributes.push((name, value));
        }
    }
}

/// Values that set no colour of their own; `windowtext` is the system's text colour, which
/// follows the colour scheme (Outlook writes it on every reply).
const NEUTRAL: [&str; 10] = [
    "",
    "inherit",
    "initial",
    "unset",
    "revert",
    "transparent",
    "none",
    "currentcolor",
    "auto",
    "windowtext",
];

/// Selectors that match only links. A text colour set for them alone isn't a design of the
/// message's own: themed bodies give every link the accent (M5, from Outlook's replies, which
/// colour `a:link` and `span.MsoHyperlink` and nothing else).
const LINK_SELECTORS: [&str; 10] = [
    "a",
    "a:link",
    "a:visited",
    "a:hover",
    "a:active",
    "a:focus",
    "span.msohyperlink",
    "span.msohyperlinkfollowed",
    ".msohyperlink",
    ".msohyperlinkfollowed",
];

/// Whether declarations (a `style` attribute or a rule's block) set a text or background
/// colour; with `links_only`, a text colour doesn't count.
fn declarations_set_colours(declarations: &str, links_only: bool) -> bool {
    declarations.split(';').any(|declaration| {
        let Some((property, value)) = declaration.split_once(':') else {
            return false;
        };
        let colour = match property.trim() {
            "color" => !links_only,
            "background" | "background-color" | "background-image" => true,
            _ => false,
        };
        colour && !NEUTRAL.contains(&value.replace("!important", "").trim())
    })
}

/// Whether a style sheet (lower case) sets a colour in any rule, nested ones included; the
/// text colour of rules for links alone doesn't count.
fn sheet_sets_colours(css: &str) -> bool {
    let css = without_comments(css);
    let mut open: Vec<(usize, &str)> = Vec::new();
    let mut prelude = 0;
    for (index, byte) in css.bytes().enumerate() {
        match byte {
            b'{' => {
                open.push((index + 1, css[prelude..index].trim()));
                prelude = index + 1;
            }
            b'}' => {
                if let Some((start, selectors)) = open.pop() {
                    let block = &css[start..index];
                    let links_only = selectors
                        .split(',')
                        .all(|selector| LINK_SELECTORS.contains(&selector.trim()));
                    if !block.contains('}') && declarations_set_colours(block, links_only) {
                        return true;
                    }
                }
                prelude = index + 1;
            }
            b';' if open.is_empty() => prelude = index + 1,
            _ => {}
        }
    }
    false
}

fn without_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .find("*/")
            .map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    out.push_str(rest);
    out
}

/// A URL (lower case) that the network would serve; `//host` doesn't resolve against the
/// reader's `about:blank` base, so it isn't one.
fn is_remote(url: &str) -> bool {
    let url = url.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'');
    url.starts_with("http:") || url.starts_with("https:")
}

fn css_has_remote_url(css: &str) -> bool {
    css.match_indices("url(")
        .any(|(index, open)| is_remote(&css[index + open.len()..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_markup_takes_the_theme() {
        let html = "<div dir=ltr>Hi,<br><b>see</b> <a href='https://example.test'>this</a>\
                    <blockquote style='margin:0 0 0 .8ex;border-left:1px solid;padding-left:1ex'>q</blockquote></div>";
        assert_eq!(scan(html), Scan::default());
    }

    #[test]
    fn colours_in_any_form_mean_paper() {
        for html in [
            "<table bgcolor=\"#fff\"><tr><td>x</td></tr></table>",
            "<p style=\"color: #333333\">x</p>",
            "<p style='font-size:12px;background-color:red !important'>x</p>",
            "<style>a{text-decoration:none} .x{color:#123}</style><p class=x>x</p>",
            "<font color=red>x</font>",
            "<body text=\"#000000\">x</body>",
            "<td background=\"cid:bg@x\">x</td>",
        ] {
            assert!(scan(html).own_colours, "{html}");
        }
    }

    #[test]
    fn link_colours_alone_take_the_theme() {
        for html in [
            "<style><!--\np.MsoNormal {margin:0cm; font-family:\"Calibri\",sans-serif;}\n\
             a:link, span.MsoHyperlink {mso-style-priority:99; color:#0563C1;}\n\
             a:visited, span.MsoHyperlinkFollowed {color:#954F72;}\n\
             span.EmailStyle17 {font-family:\"Calibri\",sans-serif; color:windowtext;}\n--></style>\
             <body lang=EN-US link=\"#0563C1\" vlink=\"#954F72\"><p class=MsoNormal>Hi</p></body>",
            "<style>@media screen { a { color: #1155cc } }</style><p>x</p>",
            "<p><a href=\"https://example.test\" style=\"color:rgb(17,85,204)\">x</a></p>",
            "<style>/* a { } p { color: red } */ a:hover{color:red}</style>",
        ] {
            assert_eq!(scan(html).evidence, None, "{html}");
        }
        for (html, evidence) in [
            (
                "<style>a { color: #1155cc } p { color: #222 }</style>",
                Evidence::StyleBlock,
            ),
            (
                "<style>a.button { color: #fff; background: #22bc66 }</style>",
                Evidence::StyleBlock,
            ),
            (
                "<style>a, p { color: #1155cc }</style>",
                Evidence::StyleBlock,
            ),
            (
                "<style>@media (prefers-color-scheme: dark) { body { background: #333 } }</style>",
                Evidence::StyleBlock,
            ),
            (
                "<a href=\"#\" style=\"background:#22bc66\">x</a>",
                Evidence::StyleAttribute,
            ),
            ("<body text=\"#000000\">x</body>", Evidence::BodyColours),
        ] {
            assert_eq!(scan(html).evidence, Some(evidence), "{html}");
        }
    }

    #[test]
    fn neutral_values_and_comments_are_ignored() {
        let html = "<!-- <p style='color:red'><img src='https://example.test/a.png'> -->\
                    <p style=\"color: inherit; background: transparent\">x</p>\
                    <p>color: red and https://example.test/b.png in text</p>";
        assert_eq!(scan(html), Scan::default());
    }

    #[test]
    fn markup_after_a_style_block_is_read() {
        let html = "<style>p{margin:0}</style><p>x</p><img src=\"https://example.test/a.png\">\
                    <style>.y{border:0}</style><font color=red>y</font>";
        assert_eq!(
            scan(html),
            Scan {
                own_colours: true,
                remote_images: true,
                evidence: Some(Evidence::FontColor),
            }
        );
    }

    #[test]
    fn remote_images_in_any_form_are_found() {
        for html in [
            "<img src=\"https://example.test/p.gif\" width=1 height=1>",
            "<IMG SRC=' HTTP://example.test/p.gif'>",
            "<img srcset=\"cid:a 1x, https://example.test/b.png 2x\">",
            "<td background=\"http://example.test/bg.png\">x</td>",
            "<div style=\"background:url('https://example.test/bg.png')\">x</div>",
            "<style>.x{background-image:url( https://example.test/x.png )}</style>",
            "<video poster=\"https://example.test/poster.png\"></video>",
            "<svg><image href=\"https://example.test/i.png\"/></svg>",
            "<input type=image src=https://example.test/go.png>",
        ] {
            assert!(scan(html).remote_images, "{html}");
        }
    }

    #[test]
    fn local_images_and_links_are_not_remote() {
        let html = "<img src=\"cid:logo@example.test\"><img src=\"data:image/png;base64,AA==\">\
                    <a href=\"https://example.test/\">link</a><img src=\"//example.test/x.png\">\
                    <link rel=stylesheet href=\"https://example.test/s.css\">\
                    <div style=\"background:url(cid:bg@example.test)\">x</div>";
        assert!(!scan(html).remote_images);
    }
}
