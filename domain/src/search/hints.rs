//! What the search field suggests at the cursor (ML-082): operators by the word typed, the
//! values of `has:` and `is:`, people after `from:` and `to:` (the app asks the store's
//! contacts), and days after `before:` and `after:`; and how taking a suggestion changes the
//! text and the cursor. Pure.
//!
//! Positions count characters, as GTK's editables do. A word is a run of characters between
//! whitespace outside double quotes; suggestions match the part of it before the cursor, and
//! taking one replaces the whole word.

use super::{Date, OPERATORS, Operator};
use std::fmt;
use std::ops::Range;

/// Characters a word needs before operators are suggested for it: one letter would open the
/// suggestions for most words typed (`t` is `to:`'s, `a` `after:`'s).
pub const OPERATOR_PREFIX: usize = 2;

/// What a value of `has:` or `is:` finds, as its suggestion shows it.
const VALUES: &[(&str, &str)] = &[
    ("attachment", "Messages with attachments"),
    ("unread", "Unread messages"),
    ("read", "Read messages"),
    ("starred", "Starred messages"),
    (
        "subscription",
        "Newsletters and other mail you can unsubscribe from",
    ),
];

/// How far back each day suggested after `before:` and `after:` lies, with what each finds.
const DAYS: &[(Back, &str, &str)] = &[
    (Back::Days(0), "Today", "Before today"),
    (Back::Days(7), "The last 7 days", "Older than a week"),
    (Back::Months(1), "The last month", "Older than a month"),
    (Back::Months(12), "The last year", "Older than a year"),
];

#[derive(Clone, Copy)]
enum Back {
    Days(i64),
    Months(i32),
}

/// What a suggestion is, for its row's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Operator,
    Value,
    Day,
    /// How a day is typed; it can't be taken.
    DayFormat,
}

/// One row of the suggestions. Its texts are the operators' and the days', never what the user
/// typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: Kind,
    /// What the row shows first: `from:`, `has:attachment`, `after:2026-09-29`.
    pub title: String,
    /// What it finds: "Sender's name or address".
    pub detail: String,
    /// An example at the row's end (operators); empty for none.
    pub example: String,
    /// What replaces the word at the cursor; `None` for a hint that can't be taken.
    pub insert: Option<String>,
}

/// What to suggest for the word at the cursor.
#[derive(Clone, PartialEq, Eq)]
pub enum Hints {
    /// These suggestions, as they are.
    Fixed(Vec<Suggestion>),
    /// The people whose name, address or domain starts with `prefix` (never empty), from the
    /// store's contacts; taking one inserts [`person`].
    People {
        operator: &'static Operator,
        prefix: String,
    },
}

impl fmt::Debug for Hints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(suggestions) => f.debug_tuple("Fixed").field(suggestions).finish(),
            Self::People { operator, prefix } => f
                .debug_struct("People")
                .field("operator", &operator.name)
                .field("prefix", &format_args!("{} chars", prefix.chars().count()))
                .finish(),
        }
    }
}

/// The word at the cursor and what to suggest for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct At {
    /// The word, in characters: what a suggestion taken replaces.
    pub word: Range<usize>,
    pub hints: Hints,
}

/// What to suggest with the cursor at `cursor` (characters) in `text`; `today` dates the days
/// suggested. `None` when nothing fits: inside a phrase, in free text, after `subject:`, or
/// when the word is already complete.
pub fn at(text: &str, cursor: usize, today: Date) -> Option<At> {
    let chars: Vec<char> = text.chars().collect();
    let word = word_at(&chars, cursor.min(chars.len()))?;
    let typed: String = chars[word.start..cursor.min(chars.len())].iter().collect();
    let whole: String = chars[word.clone()].iter().collect();
    if typed.is_empty() || typed.starts_with('"') {
        return None;
    }
    let hints = match typed.split_once(':') {
        None if whole.contains(':') => return None,
        None => operators(&typed, OPERATOR_PREFIX)?,
        Some((name, value)) => {
            let operator = operator(name)?;
            match operator.name {
                "from:" | "to:" => {
                    let prefix = value.trim_start_matches('"').trim_end_matches('"');
                    if prefix.is_empty() {
                        return None;
                    }
                    Hints::People {
                        operator,
                        prefix: prefix.to_owned(),
                    }
                }
                "before:" | "after:" => days(operator, value, today)?,
                _ if !operator.values.is_empty() => values(operator, value)?,
                _ => return None,
            }
        }
    };
    Some(At { word, hints })
}

/// Every operator that starts with the word at the cursor, however short (with none typed, all
/// of them): what Down in the field asks for. `None` in a phrase, once the word has a colon, or
/// when no operator starts with it.
pub fn operators_at(text: &str, cursor: usize) -> Option<At> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let (word, typed) = match word_at(&chars, cursor) {
        Some(word) => {
            let whole: String = chars[word.clone()].iter().collect();
            if whole.contains(':') || whole.starts_with('"') {
                return None;
            }
            let typed: String = chars[word.start..cursor].iter().collect();
            (word, typed)
        }
        None => (cursor..cursor, String::new()),
    };
    let hints = operators(&typed, 0)?;
    Some(At { word, hints })
}

/// What taking a person after `operator` inserts: `from:anna@example.com`.
pub fn person(operator: &Operator, address: &str) -> String {
    format!("{}{}", operator.name, address.trim())
}

/// The text and the cursor (characters) once `insert` replaces `word`. An operator (`from:`)
/// leaves the cursor after its colon, for its value; anything else is a whole term and is
/// followed by a space, so the next word can be typed at once.
pub fn accept(text: &str, word: Range<usize>, insert: &str) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let start = word.start.min(chars.len());
    let end = word.end.clamp(start, chars.len());
    let mut out: String = chars[..start].iter().collect();
    out.push_str(insert);
    let mut cursor = start + insert.chars().count();
    let rest = &chars[end..];
    if !insert.ends_with(':') {
        if rest.first().is_some_and(|c| c.is_whitespace()) {
            cursor += 1;
        } else {
            out.push(' ');
            cursor += 1;
        }
    }
    out.extend(rest);
    (out, cursor)
}

/// A day as `before:` and `after:` take it: `2026-10-01`.
pub fn day_text(date: Date) -> String {
    format!("{:04}-{:02}-{:02}", date.year, date.month, date.day)
}

/// The word around `cursor`: whitespace outside double quotes ends a word. `None` when the
/// cursor is in whitespace or the text is empty there.
fn word_at(chars: &[char], cursor: usize) -> Option<Range<usize>> {
    let mut quoted = false;
    let mut start = None;
    for (index, c) in chars.iter().enumerate() {
        if c.is_whitespace() && !quoted {
            if let Some(begun) = start.take()
                && (begun..=index).contains(&cursor)
            {
                return Some(begun..index);
            }
            continue;
        }
        if *c == '"' {
            quoted = !quoted;
        }
        start.get_or_insert(index);
    }
    start
        .filter(|begun| (*begun..=chars.len()).contains(&cursor))
        .map(|begun| begun..chars.len())
}

fn operator(name: &str) -> Option<&'static Operator> {
    let name = name.to_lowercase();
    OPERATORS
        .iter()
        .find(|operator| operator.name.strip_suffix(':') == Some(name.as_str()))
}

/// The operators whose name starts with `typed`, once it has `minimum` letters.
fn operators(typed: &str, minimum: usize) -> Option<Hints> {
    if typed.chars().count() < minimum || !typed.chars().all(char::is_alphabetic) {
        return None;
    }
    let typed = typed.to_lowercase();
    let found: Vec<Suggestion> = OPERATORS
        .iter()
        .filter(|operator| operator.name.starts_with(&typed))
        .map(|operator| Suggestion {
            kind: Kind::Operator,
            title: operator.name.to_owned(),
            detail: operator.description.to_owned(),
            example: operator.example.to_owned(),
            insert: Some(operator.name.to_owned()),
        })
        .collect();
    (!found.is_empty()).then_some(Hints::Fixed(found))
}

/// The values of `has:` and `is:` that start with what was typed; none once one is complete.
fn values(operator: &'static Operator, typed: &str) -> Option<Hints> {
    let typed = typed.to_lowercase();
    if operator.values.contains(&typed.as_str()) {
        return None;
    }
    let found: Vec<Suggestion> = operator
        .values
        .iter()
        .filter(|value| value.starts_with(&typed))
        .map(|value| Suggestion {
            kind: Kind::Value,
            title: format!("{}{value}", operator.name),
            detail: VALUES
                .iter()
                .find(|(known, _)| known == value)
                .map_or(operator.description, |(_, detail)| *detail)
                .to_owned(),
            example: String::new(),
            insert: Some(format!("{}{value}", operator.name)),
        })
        .collect();
    (!found.is_empty()).then_some(Hints::Fixed(found))
}

/// Today, a week, a month and a year back, those that start with what was typed; else how a day
/// is typed. None once a whole day is typed.
fn days(operator: &'static Operator, typed: &str, today: Date) -> Option<Hints> {
    if parse_day(typed).is_some() {
        return None;
    }
    let after = operator.name == "after:";
    let mut found: Vec<Suggestion> = DAYS
        .iter()
        .map(|(back, since, older)| {
            let day = day_text(back_from(today, *back));
            Suggestion {
                kind: Kind::Day,
                title: format!("{}{day}", operator.name),
                detail: (if after { since } else { older }).to_string(),
                example: String::new(),
                insert: Some(format!("{}{day}", operator.name)),
            }
        })
        .filter(|suggestion| suggestion.title[operator.name.len()..].starts_with(typed))
        .collect();
    if found.is_empty() {
        found.push(Suggestion {
            kind: Kind::DayFormat,
            title: format!("{}YYYY-MM-DD", operator.name),
            detail: format!("A day, such as {}", operator.example),
            example: String::new(),
            insert: None,
        });
    }
    Some(Hints::Fixed(found))
}

/// `2026-10-01` as a day that exists.
fn parse_day(text: &str) -> Option<Date> {
    let mut parts = text.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || year.len() != 4
        || month.len() != 2
        || day.len() != 2
        || ![year, month, day]
            .iter()
            .all(|part| part.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let date = Date {
        year: year.parse().ok()?,
        month: month.parse().ok()?,
        day: day.parse().ok()?,
    };
    ((1..=12).contains(&date.month)
        && (1..=month_length(date.year, date.month)).contains(&date.day))
    .then_some(date)
}

fn back_from(today: Date, back: Back) -> Date {
    match back {
        Back::Days(days) => from_days(to_days(today) - days),
        Back::Months(months) => {
            let index = today.year * 12 + i32::from(today.month) - 1 - months;
            let (year, month) = (index.div_euclid(12), (index.rem_euclid(12) + 1) as u8);
            Date {
                year,
                month,
                day: today.day.min(month_length(year, month)),
            }
        }
    }
}

fn month_length(year: i32, month: u8) -> u8 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn to_days(date: Date) -> i64 {
    let year = i64::from(date.year) - i64::from(date.month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(date.month);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(date.day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The day `days` after 1970-01-01 (`civil_from_days`).
fn from_days(days: i64) -> Date {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    Date {
        year: (yoe + era * 400 + i64::from(month <= 2)) as i32,
        month: month as u8,
        day: day as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: Date = Date {
        year: 2026,
        month: 10,
        day: 6,
    };

    fn hints(text: &str) -> Option<Hints> {
        at(text, text.chars().count(), TODAY).map(|at| at.hints)
    }

    fn titles(text: &str) -> Vec<String> {
        match hints(text) {
            Some(Hints::Fixed(found)) => found.into_iter().map(|found| found.title).collect(),
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn operators_are_suggested_by_the_word_typed() {
        assert_eq!(titles("fr"), ["from:"]);
        assert_eq!(titles("invoice Su"), ["subject:"]);
        assert_eq!(titles("is"), ["is:"]);
        assert_eq!(titles("from"), ["from:"]);
        assert_eq!(hints("f"), None, "one letter suggests nothing");
        assert_eq!(hints("the"), None);
        assert_eq!(hints("2026"), None);
        assert_eq!(hints(""), None);
        assert_eq!(hints("invoice "), None, "nothing at a space");
        let Some(Hints::Fixed(found)) = hints("af") else {
            panic!("after:")
        };
        assert_eq!(
            found,
            [Suggestion {
                kind: Kind::Operator,
                title: "after:".into(),
                detail: "Sent on or after a day".into(),
                example: "after:2026-09-01".into(),
                insert: Some("after:".into()),
            }]
        );
    }

    #[test]
    fn down_lists_every_operator_that_starts_with_the_word() {
        let all = |text: &str, cursor| match operators_at(text, cursor) {
            Some(At {
                word,
                hints: Hints::Fixed(found),
            }) => (word, found.len()),
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(all("", 0), (0..0, OPERATORS.len()));
        assert_eq!(all("budget ", 7), (7..7, OPERATORS.len()));
        assert_eq!(all("budget  møte", 7), (7..7, OPERATORS.len()));
        assert_eq!(all("budget f", 8), (7..8, 1));
        assert_eq!(all("from", 2), (0..4, 1));
        assert_eq!(operators_at("budget", 6), None);
        assert_eq!(operators_at("is:un", 5), None);
        assert_eq!(operators_at("\"project ", 9), None);
        assert_eq!(
            accept("budget  møte", 7..7, "from:"),
            ("budget from: møte".into(), 12)
        );
    }

    #[test]
    fn values_of_has_and_is() {
        assert_eq!(titles("has:"), ["has:attachment"]);
        assert_eq!(
            titles("is:"),
            ["is:unread", "is:read", "is:starred", "is:subscription"]
        );
        assert_eq!(titles("IS:R"), ["is:read"]);
        assert_eq!(titles("is:s"), ["is:starred", "is:subscription"]);
        assert_eq!(titles("is:su"), ["is:subscription"]);
        assert_eq!(hints("is:unread"), None, "a complete value");
        assert_eq!(hints("is:x"), None);
        let Some(Hints::Fixed(found)) = hints("has:att") else {
            panic!("has:")
        };
        assert_eq!(found[0].detail, "Messages with attachments");
        assert_eq!(found[0].insert.as_deref(), Some("has:attachment"));
        assert_eq!(found[0].kind, Kind::Value);
    }

    #[test]
    fn people_after_from_and_to() {
        let found = at("budget from:an", 14, TODAY).unwrap();
        assert_eq!(found.word, 7..14);
        let Hints::People { operator, prefix } = found.hints else {
            panic!("people")
        };
        assert_eq!((operator.name, prefix.as_str()), ("from:", "an"));
        let Some(Hints::People { operator, prefix }) = hints("To:\"Kari N") else {
            panic!("a quoted name")
        };
        assert_eq!((operator.name, prefix.as_str()), ("to:", "Kari N"));
        assert_eq!(hints("from:"), None, "nothing to look up yet");
        assert_eq!(
            person(operator, " kari@example.com "),
            "to:kari@example.com"
        );
    }

    #[test]
    fn days_after_before_and_after() {
        assert_eq!(
            titles("after:"),
            [
                "after:2026-10-06",
                "after:2026-09-29",
                "after:2026-09-06",
                "after:2025-10-06"
            ]
        );
        let Some(Hints::Fixed(found)) = hints("before:") else {
            panic!("before:")
        };
        assert_eq!(found[1].detail, "Older than a week");
        assert_eq!(
            titles("after:2026-09"),
            ["after:2026-09-29", "after:2026-09-06"]
        );
        let Some(Hints::Fixed(found)) = hints("before:2024") else {
            panic!("the format")
        };
        assert_eq!(found[0].kind, Kind::DayFormat);
        assert_eq!(found[0].title, "before:YYYY-MM-DD");
        assert_eq!(found[0].detail, "A day, such as before:2026-10-01");
        assert_eq!(found[0].insert, None);
        assert_eq!(hints("before:2024-02-29"), None, "a whole day");
        assert!(matches!(hints("before:2023-02-29"), Some(Hints::Fixed(_))));
    }

    #[test]
    fn nothing_in_phrases_free_text_or_subjects() {
        assert_eq!(hints("\"from"), None);
        assert_eq!(hints("\"project fr"), None);
        assert_eq!(hints("subject:inv"), None);
        assert_eq!(hints("http://example.com"), None);
        assert_eq!(at("from:anna", 2, TODAY), None, "the cursor before a colon");
    }

    #[test]
    fn the_word_is_the_one_at_the_cursor() {
        let text = "\"project update\" is: møte";
        let found = at(text, 20, TODAY).unwrap();
        assert_eq!(found.word, 17..20);
        assert!(matches!(found.hints, Hints::Fixed(_)));
        assert_eq!(at(text, 17, TODAY), None, "nothing typed before the cursor");
        let found = at("fr møte", 2, TODAY).unwrap();
        assert_eq!(found.word, 0..2);
    }

    #[test]
    fn taking_a_suggestion() {
        assert_eq!(
            accept("budget fr", 7..9, "from:"),
            ("budget from:".into(), 12)
        );
        assert_eq!(
            accept("from:an", 0..7, "from:anna@example.com"),
            ("from:anna@example.com ".into(), 22)
        );
        assert_eq!(
            accept("is:u møte", 0..4, "is:unread"),
            ("is:unread møte".into(), 10),
            "an existing space is kept"
        );
        assert_eq!(
            accept("æ is:s ø", 2..6, "is:starred"),
            ("æ is:starred ø".into(), 13)
        );
    }

    #[test]
    fn days_move_back_across_months_and_years() {
        let date = |year, month, day| Date { year, month, day };
        assert_eq!(
            back_from(date(2026, 3, 31), Back::Months(1)),
            date(2026, 2, 28)
        );
        assert_eq!(
            back_from(date(2024, 2, 29), Back::Months(12)),
            date(2023, 2, 28)
        );
        assert_eq!(
            back_from(date(2026, 1, 3), Back::Days(7)),
            date(2025, 12, 27)
        );
        assert_eq!(
            back_from(date(2026, 1, 15), Back::Months(1)),
            date(2025, 12, 15)
        );
        for days in [-1, 0, 59, 10_000, 20_734] {
            assert_eq!(to_days(from_days(days)), days);
        }
        assert_eq!(day_text(date(987, 1, 2)), "0987-01-02");
    }

    #[test]
    fn debug_prints_no_typed_text() {
        let printed = format!("{:?}", at("from:secretname", 15, TODAY));
        assert!(!printed.contains("secret"), "{printed}");
        assert!(printed.contains("10 chars"), "{printed}");
    }
}
