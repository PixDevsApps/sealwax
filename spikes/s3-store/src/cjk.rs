//! unicode61 keeps a run of CJK characters as one token, so a word inside unsegmented text never
//! matches. Indexing overlapping bigrams of those runs, and querying them as phrases, fixes that
//! without a custom tokenizer.

use std::borrow::Cow;

pub fn is_cjk(c: char) -> bool {
    matches!(
        u32::from(c),
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xAC00..=0xD7AF
            | 0x20000..=0x2FFFF
    )
}

/// Text for the FTS index: CJK runs become space-separated overlapping bigrams.
pub fn index_text(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_cjk) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() * 2);
    let mut run = Vec::new();
    for c in text.chars() {
        if is_cjk(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    Cow::Owned(out)
}

fn flush(run: &mut Vec<char>, out: &mut String) {
    match run.len() {
        0 => {}
        1 => {
            out.push(' ');
            out.push(run[0]);
            out.push(' ');
        }
        _ => {
            out.push(' ');
            for pair in run.windows(2) {
                out.push(pair[0]);
                out.push(pair[1]);
                out.push(' ');
            }
        }
    }
    run.clear();
}

/// An FTS5 query term for user input: CJK input becomes a quoted phrase of its bigrams.
pub fn query_term(term: &str) -> String {
    if !term.chars().any(is_cjk) {
        return term.to_owned();
    }
    let indexed = index_text(term);
    let tokens: Vec<&str> = indexed.split_whitespace().collect();
    format!("\"{}\"", tokens.join(" "))
}

#[cfg(test)]
mod tests {
    use super::{index_text, query_term};

    #[test]
    fn bigrams() {
        assert_eq!(index_text("plain text"), "plain text");
        assert_eq!(
            index_text("会议安排")
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["会议", "议安", "安排"]
        );
        assert_eq!(
            index_text("Re: 会议，下周")
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["Re:", "会议", "，", "下周"]
        );
        assert_eq!(query_term("会议"), "\"会议\"");
        assert_eq!(query_term("プロジェクト"), "\"プロ ロジ ジェ ェク クト\"");
        assert_eq!(query_term("møte"), "møte");
    }
}
