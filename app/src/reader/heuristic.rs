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
                "bgcolor" => scan.own_colours = true,
                "background" if !value.trim().is_empty() => {
                    scan.own_colours = true;
                    scan.remote_images |= is_remote(value);
                }
                "color" if tag == "font" => scan.own_colours = true,
                "text" | "link" | "vlink" | "alink" if tag == "body" => scan.own_colours = true,
                "style" => {
                    scan.own_colours |= css_sets_colours(value);
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
            scan.own_colours |= css_sets_colours(css);
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

const NEUTRAL: [&str; 9] = [
    "",
    "inherit",
    "initial",
    "unset",
    "revert",
    "transparent",
    "none",
    "currentcolor",
    "auto",
];

fn css_sets_colours(css: &str) -> bool {
    css.split([';', '{', '}']).any(|declaration| {
        let Some((property, value)) = declaration.split_once(':') else {
            return false;
        };
        let property = property.trim();
        let value = value.replace("!important", "");
        matches!(
            property,
            "color" | "background" | "background-color" | "background-image"
        ) && !NEUTRAL.contains(&value.trim())
    })
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
                remote_images: true
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
