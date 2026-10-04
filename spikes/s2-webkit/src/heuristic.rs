//! Does an HTML body bring its own colours? If it sets any text or background colour (in
//! `style` attributes, `<style>` blocks or the legacy colour attributes), it is shown as
//! designed on a light paper card; otherwise it takes the theme. A small tag scanner is
//! enough for the spike; production would feed the same rules from lol_html (ADR-011).

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Which rules matched, in document order (at most one entry per rule kind).
    pub reasons: Vec<&'static str>,
}

impl Verdict {
    pub fn own_colours(&self) -> bool {
        !self.reasons.is_empty()
    }
}

pub fn analyse(html: &str) -> Verdict {
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut verdict = Verdict::default();
    let mut note = |reason: &'static str| {
        if !verdict.reasons.contains(&reason) {
            verdict.reasons.push(reason);
        }
    };
    let mut at = 0;
    while let Some(offset) = lower[at..].find('<') {
        let start = at + offset;
        if lower[start..].starts_with("<!--") {
            at = lower[start..]
                .find("-->")
                .map_or(lower.len(), |end| start + end + 3);
            continue;
        }
        let (tag, attributes, end) = scan_tag(bytes, start + 1);
        at = end;
        for (name, value) in &attributes {
            match name.as_str() {
                "bgcolor" => note("bgcolor attribute"),
                "background" if !value.trim().is_empty() => note("background attribute"),
                "color" if tag == "font" => note("font color"),
                "text" | "link" | "vlink" | "alink" if tag == "body" => {
                    note("body colour attribute")
                }
                "style" if css_sets_colours(value) => note("style attribute"),
                _ => {}
            }
        }
        if tag == "style" {
            let close = lower[at..]
                .find("</style")
                .map_or(lower.len(), |end| at + end);
            if css_sets_colours(&lower[at..close]) {
                note("style block");
            }
            at = close;
        }
    }
    verdict
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_markup_takes_the_theme() {
        let html = "<div dir=ltr>Hi,<br><b>see</b> <a href='https://example.com'>this</a>\
                    <blockquote style='margin:0 0 0 .8ex;border-left:1px solid;padding-left:1ex'>q</blockquote></div>";
        assert!(!analyse(html).own_colours(), "{:?}", analyse(html));
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
            assert!(analyse(html).own_colours(), "{html}");
        }
    }

    #[test]
    fn neutral_values_and_comments_are_ignored() {
        let html = "<!-- <p style='color:red'> --><p style=\"color: inherit; background: transparent\">x</p>\
                    <p>color: red in text</p>";
        assert!(!analyse(html).own_colours(), "{:?}", analyse(html));
    }
}
