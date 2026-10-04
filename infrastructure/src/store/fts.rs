//! Full-text helpers. unicode61 keeps a run of CJK characters as one token, so words inside
//! unsegmented text would never match; runs are indexed as overlapping character pairs and CJK
//! query words become phrases of those pairs (S3, ADR-005 amendment).

use std::borrow::Cow;

fn is_cjk(c: char) -> bool {
    matches!(
        u32::from(c),
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xAC00..=0xD7AF
            | 0x20000..=0x2FFFF
    )
}

/// Text for the index: CJK runs become space-separated overlapping bigrams.
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

/// An FTS5 MATCH expression for what the user typed: every word must match; `"…"` is a phrase;
/// a trailing `*` matches a prefix. Every term is quoted, so no input is an FTS5 syntax error.
/// `None` when nothing searchable is left.
pub fn match_query(text: &str) -> Option<String> {
    let mut terms = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let (term, phrase, after) = if let Some(quoted) = rest.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (&quoted[..end], true, &quoted[end + 1..]),
                None => (quoted, true, ""),
            }
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            (&rest[..end], false, &rest[end..])
        };
        rest = after;
        let (term, prefix) = match term.strip_suffix('*') {
            Some(stem) if !phrase => (stem, true),
            _ => (term, false),
        };
        if !term.chars().any(char::is_alphanumeric) {
            continue;
        }
        let tokens = index_text(term);
        let quoted = format!("\"{}\"", tokens.trim().replace('"', "\"\""));
        if prefix && !term.chars().any(is_cjk) {
            terms.push(quoted + "*");
        } else {
            terms.push(quoted);
        }
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_cjk_as_bigrams() {
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
    }

    #[test]
    fn quotes_every_term() {
        assert_eq!(match_query("møte").as_deref(), Some("\"møte\""));
        assert_eq!(
            match_query("budsjett  kontrakt").as_deref(),
            Some("\"budsjett\" \"kontrakt\"")
        );
        assert_eq!(
            match_query("\"project update\" prosjekt*").as_deref(),
            Some("\"project update\" \"prosjekt\"*")
        );
        assert_eq!(match_query("会议").as_deref(), Some("\"会议\""));
        assert_eq!(
            match_query("プロジェクト").as_deref(),
            Some("\"プロ ロジ ジェ ェク クト\"")
        );
        assert_eq!(
            match_query("a\"b OR NEAR(x) -y").as_deref(),
            Some("\"a\"\"b\" \"OR\" \"NEAR(x)\" \"-y\"")
        );
        assert_eq!(match_query("  - * \"\" "), None);
        assert_eq!(
            match_query("\"unterminated phrase").as_deref(),
            Some("\"unterminated phrase\"")
        );
    }
}
