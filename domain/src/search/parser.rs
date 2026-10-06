//! The parser behind [`super::parse`]: one pass over the text, a token at a time.

use super::{Date, Query, Term};

pub(super) fn parse(text: &str) -> Query {
    let mut query = Query::default();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return query;
        }
        let (token, after) = Token::next(rest);
        rest = after;
        token.apply(&mut query);
    }
}

/// A word, a `"phrase"` or `name:value`, perhaps negated with `-`.
struct Token<'a> {
    negated: bool,
    /// The operator's name as typed, without its colon.
    operator: Option<&'a str>,
    value: &'a str,
    quoted: bool,
}

impl<'a> Token<'a> {
    /// The token at the start of `text` (which starts with no whitespace) and what follows it.
    fn next(text: &'a str) -> (Token<'a>, &'a str) {
        let (negated, body) = match text.strip_prefix('-') {
            Some(body) if body.starts_with(|c: char| c == '"' || c.is_alphanumeric()) => {
                (true, body)
            }
            _ => (false, text),
        };
        if let Some(quoted) = body.strip_prefix('"') {
            let (value, after) = quoted_value(quoted);
            let token = Token {
                negated,
                operator: None,
                value,
                quoted: true,
            };
            return (token, after);
        }
        let name_end = body
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(body.len());
        if name_end > 0
            && let Some(tail) = body[name_end..].strip_prefix(':')
        {
            let (value, quoted, after) = match tail.strip_prefix('"') {
                Some(quoted) => {
                    let (value, after) = quoted_value(quoted);
                    (value, true, after)
                }
                None => {
                    let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
                    (&tail[..end], false, &tail[end..])
                }
            };
            let token = Token {
                negated,
                operator: Some(&body[..name_end]),
                value,
                quoted,
            };
            return (token, after);
        }
        let end = body.find(char::is_whitespace).unwrap_or(body.len());
        let token = Token {
            negated,
            operator: None,
            value: &body[..end],
            quoted: false,
        };
        (token, &body[end..])
    }

    fn apply(self, query: &mut Query) {
        let Some(name) = self.operator else {
            self.push_text(query, self.value);
            return;
        };
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "from" | "to" | "subject" if !self.negated => {
                let column = match name.as_str() {
                    "from" => &mut query.from,
                    "to" => &mut query.to,
                    _ => &mut query.subject,
                };
                column.extend(term(self.value, self.quoted));
            }
            "has" => match choice(self.value, &["attachment", "attachments"]) {
                Choice::Known(_) => query.has_attachment = Some(!self.negated),
                Choice::Typing => {}
                Choice::Other => self.push_operator_text(query, &name),
            },
            "is" => match choice(self.value, &["unread", "read", "starred", "flagged"]) {
                Choice::Known("unread") => query.unread = Some(!self.negated),
                Choice::Known("read") => query.unread = Some(self.negated),
                Choice::Known(_) => query.starred = Some(!self.negated),
                Choice::Typing => {}
                Choice::Other => self.push_operator_text(query, &name),
            },
            "before" | "after" => match date(self.value) {
                Some(day) if (name == "before") != self.negated => {
                    query.before = Some(query.before.map_or(day, |before| before.min(day)));
                }
                Some(day) => {
                    query.after = Some(query.after.map_or(day, |after| after.max(day)));
                }
                None if self
                    .value
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '-' | '/' | '.')) => {}
                None => self.push_operator_text(query, &name),
            },
            _ => self.push_operator_text(query, &name),
        }
    }

    /// The operator not understood, searched as the text it was.
    fn push_operator_text(&self, query: &mut Query, name: &str) {
        self.push_text(query, &format!("{name}:{}", self.value));
    }

    fn push_text(&self, query: &mut Query, text: &str) {
        let terms = if self.negated {
            &mut query.exclude
        } else {
            &mut query.text
        };
        terms.extend(term(text, self.quoted));
    }
}

/// The text up to the closing quote, or all of it, and what follows.
fn quoted_value(text: &str) -> (&str, &str) {
    match text.find('"') {
        Some(end) => (&text[..end], &text[end + 1..]),
        None => (text, ""),
    }
}

/// A term for what was typed; `None` when it holds nothing searchable.
fn term(text: &str, quoted: bool) -> Option<Term> {
    if quoted {
        let phrase = text.trim();
        return phrase
            .chars()
            .any(char::is_alphanumeric)
            .then(|| Term::Phrase(phrase.to_owned()));
    }
    if !text.chars().any(char::is_alphanumeric) {
        return None;
    }
    Some(match text.strip_suffix('*') {
        Some(stem) if stem.chars().any(char::is_alphanumeric) => Term::Prefix(stem.to_owned()),
        _ => Term::Word(text.to_owned()),
    })
}

enum Choice {
    Known(&'static str),
    /// The start of a known value, or nothing yet.
    Typing,
    Other,
}

fn choice(value: &str, known: &[&'static str]) -> Choice {
    let value = value.trim().to_lowercase();
    if let Some(found) = known.iter().find(|known| **known == value) {
        Choice::Known(found)
    } else if known.iter().any(|known| known.starts_with(&value)) {
        Choice::Typing
    } else {
        Choice::Other
    }
}

/// `2026-10-01`, `2026/10/01`, `2026.10.01`, `1.10.2026`, `2026-10` or `2026`.
fn date(value: &str) -> Option<Date> {
    fn number(text: &str, digits: std::ops::RangeInclusive<usize>) -> Option<u16> {
        if digits.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit()) {
            text.parse().ok()
        } else {
            None
        }
    }
    let year = |text: &str| number(text, 4..=4).map(i32::from);
    let small = |text: &str| number(text, 1..=2).and_then(|n| u8::try_from(n).ok());
    let parts: Vec<&str> = value.trim().split(['-', '/', '.']).collect();
    match parts.as_slice() {
        [y] => Date::new(year(y)?, 1, 1),
        [y, m] => Date::new(year(y)?, small(m)?, 1),
        [y, m, d] if y.len() == 4 => Date::new(year(y)?, small(m)?, small(d)?),
        [d, m, y] if value.contains('.') => Date::new(year(y)?, small(m)?, small(d)?),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;
    use proptest::prelude::*;

    fn word(text: &str) -> Term {
        Term::Word(text.into())
    }

    fn day(year: i32, month: u8, day: u8) -> Option<Date> {
        Date::new(year, month, day)
    }

    #[test]
    fn operators_with_quoted_values_and_any_case() {
        let query = parse("From:\"Anna Berg\" SUBJECT:\"q3 plan\" to:team@example.com from:ann*");
        assert_eq!(
            query.from,
            [Term::Phrase("Anna Berg".into()), Term::Prefix("ann".into())]
        );
        assert_eq!(query.subject, [Term::Phrase("q3 plan".into())]);
        assert_eq!(query.to, [word("team@example.com")]);
        assert!(query.text.is_empty());
    }

    #[test]
    fn flags_attachments_and_their_negations() {
        let query = parse("is:Unread has:attachment IS:STARRED");
        assert_eq!(
            (query.unread, query.has_attachment, query.starred),
            (Some(true), Some(true), Some(true))
        );
        let query = parse("is:read -has:attachments -is:flagged");
        assert_eq!(
            (query.unread, query.has_attachment, query.starred),
            (Some(false), Some(false), Some(false))
        );
        assert_eq!(parse("-is:read").unread, Some(true));
        assert_eq!(parse("-is:unread").unread, Some(false));
        assert_eq!(
            parse("is:unread is:read").unread,
            Some(false),
            "the later wins"
        );
    }

    #[test]
    fn dates_in_every_form() {
        for (text, date) in [
            ("before:2026-10-01", day(2026, 10, 1)),
            ("before:2026/10/01", day(2026, 10, 1)),
            ("before:2026.10.1", day(2026, 10, 1)),
            ("before:1.10.2026", day(2026, 10, 1)),
            ("before:01.10.2026", day(2026, 10, 1)),
            ("before:2026-9", day(2026, 9, 1)),
            ("before:2026", day(2026, 1, 1)),
            ("before:2024-02-29", day(2024, 2, 29)),
        ] {
            let query = parse(text);
            assert_eq!(query.before, date, "{text}");
            assert!(query.text.is_empty(), "{text}");
        }
        let query = parse("after:2026-01-01 after:2026-06-01 before:2026-12-01 before:2026-09-01");
        assert_eq!(query.after, day(2026, 6, 1));
        assert_eq!(query.before, day(2026, 9, 1));
        let query = parse("-before:2026-03-01 -after:2026-05-01");
        assert_eq!(
            (query.after, query.before),
            (day(2026, 3, 1), day(2026, 5, 1))
        );
    }

    #[test]
    fn what_is_still_being_typed_is_left_out() {
        for text in [
            "is:",
            "is:un",
            "IS:Star",
            "has:",
            "has:att",
            "before:",
            "before:202",
            "before:2026-",
            "after:2026-02-30",
            "after:01-10-2026",
            "from:",
            "subject:\"",
            "to: ",
        ] {
            assert!(parse(text).is_empty(), "{text}: {:?}", parse(text));
        }
    }

    #[test]
    fn what_is_not_understood_is_text() {
        let query = parse("re:hello http://example.com 10:30 is:important after:yesterday");
        assert_eq!(
            query.text,
            [
                word("re:hello"),
                word("http://example.com"),
                word("10:30"),
                word("is:important"),
                word("after:yesterday"),
            ]
        );
        assert!(query.unread.is_none() && query.after.is_none());
        assert_eq!(parse("-from:anna").exclude, [word("from:anna")]);
        assert_eq!(
            parse("label:\"q3 plan\"").text,
            [Term::Phrase("label:q3 plan".into())]
        );
    }

    #[test]
    fn exclusions() {
        let query = parse("faktura -purring -\"siste varsel\" - --x");
        assert_eq!(query.text, [word("faktura"), word("--x")]);
        assert_eq!(
            query.exclude,
            [word("purring"), Term::Phrase("siste varsel".into())]
        );
    }

    #[test]
    fn days_that_do_not_exist() {
        assert_eq!(Date::new(2025, 2, 29), None);
        assert_eq!(Date::new(2026, 4, 31), None);
        assert_eq!(Date::new(2026, 13, 1), None);
        assert_eq!(Date::new(2026, 1, 0), None);
        assert_eq!(Date::new(2000, 2, 29), day(2000, 2, 29));
        assert_eq!(Date::new(1900, 2, 29), None);
    }

    fn plain_word() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9æøåÆØÅ会议]{1,12}"
    }

    proptest! {
        #[test]
        fn never_panics(text in "\\PC{0,80}") {
            check(&parse(&text))?;
        }

        #[test]
        fn never_panics_on_syntax(text in "[a-zæø会 \"*:\\-./0-9]{0,40}") {
            check(&parse(&text))?;
        }

        #[test]
        fn words_survive(words in prop::collection::vec(plain_word(), 0..8), gap in "[ \t\n]{1,3}") {
            let query = parse(&words.join(&gap));
            let expected: Vec<Term> = words.iter().map(|text| word(text)).collect();
            prop_assert_eq!(&query.text, &expected);
            prop_assert_eq!(query, Query { text: expected, ..Query::default() });
        }

        #[test]
        fn operator_values_survive(text in plain_word(), operator in 0usize..3) {
            let name = ["from", "to", "subject"][operator];
            let query = parse(&format!("{name}:{text}"));
            let column = [&query.from, &query.to, &query.subject][operator];
            prop_assert_eq!(column, &vec![word(&text)]);
            prop_assert!(query.text.is_empty());
        }

        #[test]
        fn debug_prints_no_words(words in prop::collection::vec("[æøå][a-zæøå]{5,10}", 1..5)) {
            let text = words.iter().enumerate().map(|(n, text)| match n % 4 {
                0 => text.clone(),
                1 => format!("from:{text}"),
                2 => format!("-\"{text} {text}\""),
                _ => format!("subject:{text}*"),
            }).collect::<Vec<_>>().join(" ");
            let printed = format!("{:?}", parse(&text));
            for text in &words {
                prop_assert!(!printed.contains(text.as_str()), "{}", printed);
            }
        }
    }

    /// Every term holds something searchable, and a word or prefix holds no whitespace.
    fn check(query: &Query) -> Result<(), TestCaseError> {
        for term in [
            &query.text,
            &query.exclude,
            &query.from,
            &query.to,
            &query.subject,
        ]
        .into_iter()
        .flatten()
        {
            prop_assert!(term.text().chars().any(char::is_alphanumeric), "{:?}", term);
            if !matches!(term, Term::Phrase(_)) {
                prop_assert!(!term.text().contains(char::is_whitespace));
            }
        }
        Ok(())
    }
}
