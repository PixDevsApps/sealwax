//! `text/plain; format=flowed` (RFC 3676): lines that end in a space are soft breaks, so a
//! paragraph sent as several short lines reads as one line that the reader wraps itself. Quote
//! markers give each paragraph its depth, a leading space is space-stuffing, and the signature
//! separator `-- ` is never flowed.

/// The parameters of a flowed body. `delsp=yes` (DelSp) means the space before a soft break
/// was added by the sender and goes when the lines are joined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flowed {
    pub delsp: bool,
}

impl Flowed {
    /// From a `text/plain` Content-Type's `format` and `delsp` parameters; `None` unless the
    /// format is flowed.
    pub fn from_params(format: Option<&str>, delsp: Option<&str>) -> Option<Flowed> {
        let flowed = format.is_some_and(|format| format.trim().eq_ignore_ascii_case("flowed"));
        flowed.then(|| Flowed {
            delsp: delsp.is_some_and(|delsp| delsp.trim().eq_ignore_ascii_case("yes")),
        })
    }
}

/// The text with its soft breaks joined: one line per paragraph, a quoted paragraph starting
/// with its markers (`>> ` at depth 2), the signature separator on its own line.
pub fn decode(text: &str, flowed: Flowed) -> String {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    let mut paragraph = String::new();
    let mut depth = 0;
    let mut open = false;
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let line_depth = raw.bytes().take_while(|byte| *byte == b'>').count();
        let content = &raw[line_depth..];
        let content = content.strip_prefix(' ').unwrap_or(content);
        let separator = content == "-- ";
        if open && (line_depth != depth || separator) {
            finish(&mut out, depth, &mut paragraph);
        }
        depth = line_depth;
        let soft = !separator && content.ends_with(' ');
        let kept = if soft && flowed.delsp {
            &content[..content.len() - 1]
        } else {
            content
        };
        paragraph.push_str(kept);
        open = soft;
        if !open {
            finish(&mut out, depth, &mut paragraph);
        }
    }
    if open {
        finish(&mut out, depth, &mut paragraph);
    }
    out.pop();
    out
}

fn finish(out: &mut String, depth: usize, paragraph: &mut String) {
    if depth > 0 {
        out.extend(std::iter::repeat_n('>', depth));
        if !paragraph.is_empty() {
            out.push(' ');
        }
    }
    out.push_str(paragraph);
    out.push('\n');
    paragraph.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED: Flowed = Flowed { delsp: false };
    const DELSP: Flowed = Flowed { delsp: true };

    #[test]
    fn parameters_come_from_the_content_type() {
        assert_eq!(Flowed::from_params(None, None), None);
        assert_eq!(Flowed::from_params(Some("fixed"), Some("yes")), None);
        assert_eq!(Flowed::from_params(Some("Flowed"), None), Some(FIXED));
        assert_eq!(Flowed::from_params(Some("flowed"), Some("no")), Some(FIXED));
        assert_eq!(
            Flowed::from_params(Some(" FLOWED "), Some("Yes")),
            Some(DELSP)
        );
    }

    #[test]
    fn soft_breaks_join_a_paragraph() {
        let text = "The cabin sleeps six, so there's \r\nroom for everyone. Bring wool \r\nsocks.\r\n\r\nIngrid\r\n";
        assert_eq!(
            decode(text, FIXED),
            "The cabin sleeps six, so there's room for everyone. Bring wool socks.\n\nIngrid"
        );
    }

    #[test]
    fn hard_breaks_stay() {
        let text = "Line one\nLine two\n\nLine four";
        assert_eq!(decode(text, FIXED), "Line one\nLine two\n\nLine four");
    }

    #[test]
    fn delsp_removes_the_space_before_a_soft_break() {
        assert_eq!(
            decode("Hyphen-free wo \r\nrd\r\n", DELSP),
            "Hyphen-free word"
        );
        assert_eq!(
            decode("Hyphen-free wo \r\nrd\r\n", FIXED),
            "Hyphen-free wo rd"
        );
        assert_eq!(
            decode("日本語の \r\n文章\r\n", DELSP),
            "日本語の文章",
            "DelSp is how languages without spaces flow"
        );
    }

    #[test]
    fn space_stuffing_is_removed() {
        assert_eq!(decode(" From the start\r\n", FIXED), "From the start");
        assert_eq!(
            decode(" >not a quote\r\n", FIXED),
            ">not a quote",
            "a stuffed > is text"
        );
        assert_eq!(decode("  two spaces\r\n", FIXED), " two spaces");
        assert_eq!(decode(" soft \r\n stuffed\r\n", FIXED), "soft stuffed");
    }

    #[test]
    fn quoted_paragraphs_keep_their_depth() {
        let text = "> On Friday you \r\n> wrote that the \r\n> draft is ready.\r\n>> It \r\n>> is.\r\nGood.\r\n";
        assert_eq!(
            decode(text, FIXED),
            "> On Friday you wrote that the draft is ready.\n>> It is.\nGood."
        );
    }

    #[test]
    fn a_flowed_line_before_another_depth_ends_its_paragraph() {
        let text = "> quoted and flowed \r\nnot quoted\r\n";
        assert_eq!(decode(text, FIXED), "> quoted and flowed \nnot quoted");
        let text = "outer \r\n>> inner\r\n";
        assert_eq!(decode(text, FIXED), "outer \n>> inner");
    }

    #[test]
    fn empty_quoted_lines_keep_only_their_markers() {
        assert_eq!(decode(">> a\r\n>>\r\n>> b\r\n", FIXED), ">> a\n>>\n>> b");
        assert_eq!(
            decode("> \r\n> b\r\n", FIXED),
            ">\n> b",
            "a stuffed space alone is an empty line"
        );
    }

    #[test]
    fn the_signature_separator_is_never_flowed() {
        let text = "See you soon \r\n-- \r\nIngrid Solberg \r\nHemsedal\r\n";
        assert_eq!(
            decode(text, FIXED),
            "See you soon \n-- \nIngrid Solberg Hemsedal"
        );
        assert_eq!(decode("Bye\r\n-- \r\nSig\r\n", DELSP), "Bye\n-- \nSig");
        assert_eq!(decode("> -- \r\n> sig\r\n", FIXED), "> -- \n> sig");
    }

    #[test]
    fn a_flowed_line_at_the_end_ends_the_text() {
        assert_eq!(decode("last words ", FIXED), "last words ");
        assert_eq!(decode("last words \r\n", DELSP), "last words");
        assert_eq!(decode("a \r\nb \r\n", FIXED), "a b ");
    }

    #[test]
    fn mixed_depths_and_bare_line_feeds() {
        let text =
            "Reply \nhere.\n> first \n> level\n>> second \n>> level \n>> continues\n> back\n";
        assert_eq!(
            decode(text, FIXED),
            "Reply here.\n> first level\n>> second level continues\n> back"
        );
    }

    #[test]
    fn empty_and_odd_input() {
        assert_eq!(decode("", FIXED), "");
        assert_eq!(decode("\r\n", FIXED), "");
        assert_eq!(decode("\n\n", FIXED), "\n");
        assert_eq!(decode(">", FIXED), ">");
        assert_eq!(decode(" ", FIXED), "");
        assert_eq!(decode("ü \r\nö", DELSP), "üö");
    }
}
