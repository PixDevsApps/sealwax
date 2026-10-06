//! Plain text in a draft's HTML: escaping, and text as the editor's lines (one `<div>` per
//! line, as WebKit's editable view writes them).

/// `text` with the five HTML special characters escaped, for text and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Plain text as HTML lines: each line a `<div>`, an empty line `<div><br></div>`, runs of
/// spaces and tabs kept with no-break spaces, control characters dropped. Empty text is one
/// empty line.
pub fn to_html(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(text.len() + 16);
    for line in text.split('\n') {
        let line = line_html(line);
        if line.is_empty() {
            out.push_str("<div><br></div>");
        } else {
            out.push_str("<div>");
            out.push_str(&line);
            out.push_str("</div>");
        }
    }
    out
}

/// One line: a space after the line's start or after another space is a no-break space, so
/// the run survives HTML's whitespace collapsing; a trailing space too.
fn line_html(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut after_space = true;
    for c in line.chars() {
        match c {
            '\t' => out.push_str("&nbsp;&nbsp;&nbsp;&nbsp;"),
            ' ' if after_space => out.push_str("&nbsp;"),
            ' ' => out.push(' '),
            c if c.is_control() => continue,
            c => out.push_str(&escape(c.encode_utf8(&mut [0; 4]))),
        }
        after_space = matches!(c, ' ' | '\t');
    }
    if out.ends_with(' ') {
        out.pop();
        out.push_str("&nbsp;");
    }
    out
}

/// Header text without control characters (a CR LF in a subject or a name could otherwise start
/// a header line of its own), whitespace runs as one space, trimmed.
pub fn header_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split(|c: char| c.is_whitespace() || c.is_control()) {
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup() {
        assert_eq!(
            escape("<b>\"Tom & 'Jerry'\"</b>"),
            "&lt;b&gt;&quot;Tom &amp; &#39;Jerry&#39;&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn lines_become_divs() {
        assert_eq!(to_html(""), "<div><br></div>");
        assert_eq!(
            to_html("Hei <du>\r\n\r\nblåbær"),
            "<div>Hei &lt;du&gt;</div><div><br></div><div>blåbær</div>"
        );
        assert_eq!(to_html("a  b"), "<div>a &nbsp;b</div>");
        assert_eq!(to_html("  x "), "<div>&nbsp;&nbsp;x&nbsp;</div>");
        assert_eq!(to_html("\tx\u{7}"), "<div>&nbsp;&nbsp;&nbsp;&nbsp;x</div>");
    }

    #[test]
    fn header_text_has_one_line() {
        assert_eq!(header_text(" Hi\r\nBcc: x\t y "), "Hi Bcc: x y");
        assert_eq!(header_text("\u{0}"), "");
    }
}
