//! HTML bodies as plain text for snippets and the search index: html2text with a decorator that
//! keeps only the text, marking quoted blocks with "> " so the snippet can leave them out.

use super::truncate;
use html2text::render::TextDecorator;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The most HTML Sealwax parses into a tree (html5ever, beneath html2text and the cleaner) for
/// anything but the reader: the tree builder's time grows with the square of the nesting depth,
/// so 1 MB of nested lists took 37.9 s and 256 KiB 2.56 s (M9, F14). HTML beyond it is cut
/// before [`html_to_text`] reads it, and a reply or forward quotes a larger original as text.
pub const HTML_PARSE_LIMIT: usize = 256 * 1024;
const WIDTH: usize = 200;

#[derive(Clone, Copy, Debug, Default)]
struct TextOnly;

impl TextDecorator for TextOnly {
    type Annotation = ();

    fn decorate_link_start(&mut self, _url: &str) -> (String, ()) {
        (String::new(), ())
    }

    fn decorate_link_end(&mut self) -> String {
        String::new()
    }

    fn decorate_em_start(&self) -> (String, ()) {
        (String::new(), ())
    }

    fn decorate_em_end(&self) -> String {
        String::new()
    }

    fn decorate_strong_start(&self) -> (String, ()) {
        (String::new(), ())
    }

    fn decorate_strong_end(&self) -> String {
        String::new()
    }

    fn decorate_strikeout_start(&self) -> (String, ()) {
        (String::new(), ())
    }

    fn decorate_strikeout_end(&self) -> String {
        String::new()
    }

    fn decorate_code_start(&self) -> (String, ()) {
        (String::new(), ())
    }

    fn decorate_code_end(&self) -> String {
        String::new()
    }

    fn decorate_preformat_first(&self) {}

    fn decorate_preformat_cont(&self) {}

    fn decorate_image(&mut self, _src: &str, title: &str) -> (String, ()) {
        (title.to_owned(), ())
    }

    fn header_prefix(&self, _level: usize) -> String {
        String::new()
    }

    fn quote_prefix(&self) -> String {
        "> ".to_owned()
    }

    fn unordered_item_prefix(&self) -> String {
        String::new()
    }

    fn ordered_item_prefix(&self, _i: i64) -> String {
        String::new()
    }

    fn make_subblock_decorator(&self) -> Self {
        TextOnly
    }
}

/// The text of an HTML body's first [`HTML_PARSE_LIMIT`] bytes, one rendered line per line,
/// quoted blocks prefixed with "> ". Falls back to mail-parser's simple converter if html2text
/// fails.
pub fn html_to_text(html: &str) -> String {
    let html = truncate(html, HTML_PARSE_LIMIT);
    let converted = catch_unwind(AssertUnwindSafe(|| {
        html2text::config::with_decorator(TextOnly)
            .link_footnotes(false)
            .no_table_borders()
            .allow_width_overflow()
            .string_from_read(html.as_bytes(), WIDTH)
    }));
    match converted {
        Ok(Ok(text)) => text,
        _ => mail_parser::decoders::html::html_to_text(html),
    }
}

#[cfg(test)]
mod tests {
    use super::{HTML_PARSE_LIMIT, html_to_text};
    use crate::snippet::snippet;

    #[test]
    fn reads_only_the_first_256_kib() {
        let filler = "<p>fyll</p>".repeat(HTML_PARSE_LIMIT / 11);
        let html = format!("<p>tidlig</p>{filler}<p>sent</p>");
        assert!(html.len() > HTML_PARSE_LIMIT);
        let text = html_to_text(&html);
        assert!(text.starts_with("tidlig"), "{:?}", &text[..40]);
        assert!(!text.contains("sent"));
        let within = format!("<p>tidlig</p>{}<p>sent</p>", "<p>fyll</p>".repeat(1_000));
        assert!(html_to_text(&within).contains("sent"));
        let euros = html_to_text(&"€".repeat(HTML_PARSE_LIMIT));
        assert_eq!(euros.matches('€').count(), HTML_PARSE_LIMIT / 3);
    }

    #[test]
    fn keeps_the_text_and_marks_quotes() {
        let html = "<html><head><style>p{color:red}</style><title>Ignored</title></head><body>\
            <p>Hei <b>Kari</b>, se <a href=\"https://example.test/x\">lenken</a>.</p>\
            <img src=\"cid:logo@example.test\" alt=\"\">\
            <blockquote><p>Gammel tekst</p></blockquote></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hei Kari, se lenken."), "{text:?}");
        assert!(!text.contains("example.test/x"), "{text:?}");
        assert!(!text.contains("color:red"), "{text:?}");
        assert!(text.contains("> Gammel tekst"), "{text:?}");
        assert_eq!(snippet(&text, 200), "Hei Kari, se lenken.");
    }

    #[test]
    fn survives_hostile_markup() {
        let deep = "<table><tr><td>".repeat(400) + "dypt" + &"</td></tr></table>".repeat(400);
        assert!(html_to_text(&deep).contains("dypt"));
        let wide = format!("<table><tr>{}</tr></table>", "<td>celle</td>".repeat(2_000));
        let _ = html_to_text(&wide);
        for input in [
            "",
            "<",
            "<<>>",
            "<p",
            "&#xFFFFFFFF;",
            "<svg><script>x</script>",
            "\0\0",
        ] {
            let _ = html_to_text(input);
        }
    }
}
