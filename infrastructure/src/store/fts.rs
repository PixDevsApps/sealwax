//! Full-text helpers: the text the index holds and the FTS5 expressions for a query.
//!
//! - unicode61 keeps a run of CJK characters as one token, so words inside unsegmented text
//!   would never match; runs are indexed as overlapping character pairs and CJK query words
//!   become phrases of those pairs (S3, ADR-005 amendment).
//! - `remove_diacritics 2` folds å to a, but not æ or ø, which have no decomposition. The index
//!   holds them as their ASCII spellings, æ as ae and ø as o, and so does every query, so a
//!   Norwegian word is found typed either way (M7). Entries written before M7 hold them as they
//!   were, so a query that spells them also asks for that spelling.
//! - The body column holds the names of the message's attachments after its text (M7).

use sealwax_domain::model::Attachment;
use sealwax_domain::search::{Query, Term};
use std::borrow::Cow;
use std::fmt;

fn is_cjk(c: char) -> bool {
    matches!(
        u32::from(c),
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xAC00..=0xD7AF
            | 0x20000..=0x2FFFF
    )
}

/// The ASCII spelling the index holds for a letter unicode61 leaves as it is.
fn ascii(c: char) -> Option<&'static str> {
    match c {
        'æ' | 'Æ' | 'ǽ' | 'Ǽ' => Some("ae"),
        'ø' | 'Ø' | 'ǿ' | 'Ǿ' => Some("o"),
        _ => None,
    }
}

/// Text for the index: CJK runs become space-separated overlapping bigrams, æ and ø their
/// ASCII spellings.
pub fn index_text(text: &str) -> Cow<'_, str> {
    convert(text, true)
}

/// Text as entries written before M7 hold it: CJK bigrams, æ and ø as they are.
fn earlier_text(text: &str) -> Cow<'_, str> {
    convert(text, false)
}

fn convert(text: &str, fold: bool) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|c| is_cjk(c) || (fold && ascii(c).is_some()))
    {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() * 2);
    let mut run = Vec::new();
    for c in text.chars() {
        if is_cjk(c) {
            run.push(c);
            continue;
        }
        flush(&mut run, &mut out);
        match ascii(c).filter(|_| fold) {
            Some(spelling) => out.push_str(spelling),
            None => out.push(c),
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

/// The body column's text: the body, then the names of the message's attachments, one a line
/// (not those of images the HTML body shows inline).
pub fn body_text<'a>(body: &'a str, attachments: &[Attachment]) -> Cow<'a, str> {
    let mut names = attachments
        .iter()
        .filter(|attachment| !attachment.is_embedded())
        .filter_map(|attachment| attachment.name.as_deref())
        .peekable();
    if names.peek().is_none() {
        return Cow::Borrowed(body);
    }
    let mut text = body.to_owned();
    for name in names {
        text.push('\n');
        text.push_str(name);
    }
    Cow::Owned(text)
}

/// Prefixes shorter than this many letters or digits are searched as words: a one-letter
/// prefix matches nearly every message, and FTS5 gathers every match of a prefix before it
/// returns any (55–110 ms at 100,000 messages).
pub const MIN_PREFIX: usize = 2;

/// A query's FTS5 expressions. Every term is quoted, so no input is an FTS5 syntax error.
/// `Debug` prints lengths only: the expressions hold the search text (ADR-018).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Expression {
    /// What a message must match: the words anywhere and the operators' words in their
    /// columns, all of them.
    pub matching: Option<String>,
    /// What it must not match: any of the excluded words.
    pub excluded: Option<String>,
    /// Whether either holds a prefix.
    pub prefix: bool,
}

impl fmt::Debug for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let length = |text: &Option<String>| text.as_ref().map(|text| text.chars().count());
        f.debug_struct("Expression")
            .field("matching", &length(&self.matching))
            .field("excluded", &length(&self.excluded))
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl Expression {
    pub fn of(query: &Query) -> Expression {
        let mut prefix = false;
        let mut matching = Vec::new();
        for (column, terms) in [
            (None, &query.text),
            (Some("sender"), &query.from),
            (Some("recipients"), &query.to),
            (Some("subject"), &query.subject),
        ] {
            for (sql, is_prefix) in terms.iter().filter_map(term) {
                prefix |= is_prefix;
                matching.push(match column {
                    Some(column) => format!("{column} : {sql}"),
                    None => sql,
                });
            }
        }
        let mut excluded = Vec::new();
        for (sql, is_prefix) in query.exclude.iter().filter_map(term) {
            prefix |= is_prefix;
            excluded.push(sql);
        }
        Expression {
            matching: (!matching.is_empty()).then(|| matching.join(" AND ")),
            excluded: (!excluded.is_empty()).then(|| excluded.join(" OR ")),
            prefix,
        }
    }
}

/// One term as an FTS5 phrase, and whether it is a prefix; `None` when nothing in it is
/// searchable.
fn term(term: &Term) -> Option<(String, bool)> {
    let text = term.text();
    if !text.chars().any(char::is_alphanumeric) {
        return None;
    }
    let prefix = match term {
        Term::Prefix(stem) => {
            stem.chars().filter(|c| c.is_alphanumeric()).count() >= MIN_PREFIX
                && !stem.chars().any(is_cjk)
        }
        Term::Word(_) | Term::Phrase(_) => false,
    };
    let star = if prefix { "*" } else { "" };
    let folded = quote(&index_text(text));
    let earlier = quote(&earlier_text(text));
    let sql = if folded == earlier {
        format!("{folded}{star}")
    } else {
        format!("({earlier}{star} OR {folded}{star})")
    };
    Some((sql, prefix))
}

fn quote(tokens: &str) -> String {
    format!("\"{}\"", tokens.trim().replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::search::parse;

    fn matching(text: &str) -> Option<String> {
        Expression::of(&parse(text)).matching
    }

    #[test]
    fn indexes_cjk_as_bigrams() {
        assert_eq!(index_text("plain text"), "plain text");
        assert!(matches!(index_text("plain text"), Cow::Borrowed(_)));
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
    fn folds_ae_and_o_with_stroke() {
        assert_eq!(index_text("Blåbærsyltetøy"), "Blåbaersyltetoy");
        assert_eq!(index_text("ÆRØ Ærlig øl"), "aeRo aerlig ol");
        assert_eq!(earlier_text("Blåbærsyltetøy"), "Blåbærsyltetøy");
        assert_eq!(index_text("Møte om 会议"), "Mote om  会议 ");
    }

    #[test]
    fn attachment_names_follow_the_body() {
        let file = |name: &str, embedded: bool| Attachment {
            part: 1,
            name: Some(name.into()),
            size: 10,
            mime_type: if embedded {
                "image/png"
            } else {
                "application/pdf"
            }
            .into(),
            content_id: embedded.then(|| "logo".into()),
            inline: embedded,
        };
        assert_eq!(body_text("Hei", &[]), "Hei");
        assert_eq!(
            body_text(
                "Hei",
                &[file("faktura-1.pdf", false), file("logo.png", true)]
            ),
            "Hei\nfaktura-1.pdf"
        );
    }

    #[test]
    fn quotes_every_term() {
        assert_eq!(matching("mote").as_deref(), Some("\"mote\""));
        assert_eq!(
            matching("budsjett  kontrakt").as_deref(),
            Some("\"budsjett\" AND \"kontrakt\"")
        );
        assert_eq!(
            matching("\"project update\" prosjekt*").as_deref(),
            Some("\"project update\" AND \"prosjekt\"*")
        );
        assert_eq!(matching("会议").as_deref(), Some("\"会议\""));
        assert_eq!(
            matching("プロジェクト").as_deref(),
            Some("\"プロ ロジ ジェ ェク クト\"")
        );
        assert_eq!(
            matching("a\"b OR NEAR(x) y").as_deref(),
            Some("\"a\"\"b\" AND \"OR\" AND \"NEAR(x)\" AND \"y\"")
        );
        assert_eq!(matching("  - * \"\" "), None);
        assert_eq!(
            matching("\"unterminated phrase").as_deref(),
            Some("\"unterminated phrase\"")
        );
    }

    #[test]
    fn asks_for_both_spellings_of_ae_and_o_with_stroke() {
        assert_eq!(matching("møte").as_deref(), Some("(\"møte\" OR \"mote\")"));
        assert_eq!(matching("bjø*").as_deref(), Some("(\"bjø\"* OR \"bjo\"*)"));
        assert_eq!(
            matching("blabaersyltetoy").as_deref(),
            Some("\"blabaersyltetoy\"")
        );
    }

    #[test]
    fn operators_search_their_columns() {
        assert_eq!(
            matching("from:anna to:\"kari nordmann\" subject:faktu* plan").as_deref(),
            Some(
                "\"plan\" AND sender : \"anna\" AND recipients : \"kari nordmann\" \
                 AND subject : \"faktu\"*"
            )
        );
        let expression = Expression::of(&parse("budsjett -utkast -\"gammel versjon\""));
        assert_eq!(expression.matching.as_deref(), Some("\"budsjett\""));
        assert_eq!(
            expression.excluded.as_deref(),
            Some("\"utkast\" OR \"gammel versjon\"")
        );
        assert!(!expression.prefix);
        assert!(Expression::of(&parse("-utk*")).prefix);
    }

    #[test]
    fn debug_prints_no_text() {
        let printed = format!("{:?}", Expression::of(&parse("anna -hemmelig")));
        assert!(
            !printed.contains("anna") && !printed.contains("hemmelig"),
            "{printed}"
        );
    }

    #[test]
    fn short_prefixes_are_words() {
        assert_eq!(matching("a*").as_deref(), Some("\"a\""));
        assert_eq!(matching("ab*").as_deref(), Some("\"ab\"*"));
        assert!(!Expression::of(&parse("a*")).prefix);
        assert_eq!(matching("会*").as_deref(), Some("\"会\""));
    }
}
