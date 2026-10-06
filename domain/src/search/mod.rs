//! The search field's query language (PRD M9, ML-082): words and phrases, plus `from:`, `to:`,
//! `subject:`, `has:attachment`, `is:unread`, `before:` and `after:`. Pure: the store turns a
//! [`Query`] into SQL against the full-text index, and the list's search field suggests from
//! [`OPERATORS`].
//!
//! M7's contract: the types and the operator list. [`parse`] understands only words, phrases
//! and prefixes until the search agent's parser replaces it (phase A).

use std::fmt;

/// A day as typed (`before:2026-10-01`). The store places it in the local time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    /// 1–12.
    pub month: u8,
    /// 1–31.
    pub day: u8,
}

/// One full-text term.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Term {
    Word(String),
    /// `budsj*`: words that start with the stem.
    Prefix(String),
    /// `"project update"`: the words next to each other, in order.
    Phrase(String),
}

impl Term {
    pub fn text(&self) -> &str {
        match self {
            Self::Word(text) | Self::Prefix(text) | Self::Phrase(text) => text,
        }
    }
}

impl fmt::Debug for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Word(_) => "Word",
            Self::Prefix(_) => "Prefix",
            Self::Phrase(_) => "Phrase",
        };
        write!(f, "{kind}({} chars)", self.text().chars().count())
    }
}

/// What the user typed, understood. Every condition must hold; terms under an operator match
/// only that field. `Debug` prints counts only: search text can hold names and addresses, which
/// never reach a log (ADR-018).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Words and phrases matched anywhere: subject, sender, recipients, body.
    pub text: Vec<Term>,
    /// `from:`, the sender's name or address.
    pub from: Vec<Term>,
    /// `to:`, a recipient's name or address (To and Cc).
    pub to: Vec<Term>,
    /// `subject:`.
    pub subject: Vec<Term>,
    /// `has:attachment`.
    pub has_attachment: Option<bool>,
    /// `is:unread` (`Some(true)`) and `is:read` (`Some(false)`).
    pub unread: Option<bool>,
    /// `is:starred`.
    pub starred: Option<bool>,
    /// `before:`: messages dated before the start of this day.
    pub before: Option<Date>,
    /// `after:`: messages dated on or after the start of this day.
    pub after: Option<Date>,
}

impl Query {
    /// Nothing to search for: the list shows the folder instead.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl fmt::Debug for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Query")
            .field("text", &self.text.len())
            .field("from", &self.from.len())
            .field("to", &self.to.len())
            .field("subject", &self.subject.len())
            .field("has_attachment", &self.has_attachment)
            .field("unread", &self.unread)
            .field("starred", &self.starred)
            .field("before", &self.before.is_some())
            .field("after", &self.after.is_some())
            .finish()
    }
}

/// One operator as the search field suggests it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operator {
    /// What is typed, with its colon: `from:`.
    pub name: &'static str,
    /// The values it takes when they are a fixed set (`has:attachment`); empty when free.
    pub values: &'static [&'static str],
    /// An example to show next to it.
    pub example: &'static str,
    /// What it finds, as the suggestion shows it.
    pub description: &'static str,
}

/// The operators of PRD M9 (plus `is:read` and `is:starred`, which the chips already filter),
/// in the order the search field suggests them.
pub const OPERATORS: &[Operator] = &[
    Operator {
        name: "from:",
        values: &[],
        example: "from:anna",
        description: "Sender's name or address",
    },
    Operator {
        name: "to:",
        values: &[],
        example: "to:team@example.com",
        description: "Recipient's name or address",
    },
    Operator {
        name: "subject:",
        values: &[],
        example: "subject:invoice",
        description: "Words in the subject",
    },
    Operator {
        name: "has:",
        values: &["attachment"],
        example: "has:attachment",
        description: "Messages with attachments",
    },
    Operator {
        name: "is:",
        values: &["unread", "read", "starred"],
        example: "is:unread",
        description: "Unread, read or starred messages",
    },
    Operator {
        name: "before:",
        values: &[],
        example: "before:2026-10-01",
        description: "Sent before a day",
    },
    Operator {
        name: "after:",
        values: &[],
        example: "after:2026-09-01",
        description: "Sent on or after a day",
    },
];

/// What the user typed, as a [`Query`]. Never fails: anything not understood is searched as
/// text. The contract's version reads words, `"phrases"` and `prefix*` only.
pub fn parse(text: &str) -> Query {
    let mut query = Query::default();
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
        if !term.chars().any(char::is_alphanumeric) {
            continue;
        }
        query.text.push(match term.strip_suffix('*') {
            _ if phrase => Term::Phrase(term.trim().to_owned()),
            Some(stem) => Term::Prefix(stem.to_owned()),
            None => Term::Word(term.to_owned()),
        });
    }
    query
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_phrases_and_prefixes() {
        let query = parse("  møte \"project update\" budsj* ");
        assert_eq!(
            query.text,
            [
                Term::Word("møte".into()),
                Term::Phrase("project update".into()),
                Term::Prefix("budsj".into()),
            ]
        );
        assert!(parse(" - * \"\" ").is_empty());
        assert!(!query.is_empty());
    }

    #[test]
    fn debug_prints_no_text() {
        let query = parse("anna@example.com \"secret plan\"");
        let printed = format!("{query:?}");
        assert!(
            !printed.contains("anna") && !printed.contains("secret"),
            "{printed}"
        );
        assert!(printed.contains("text: 2"), "{printed}");
    }

    #[test]
    fn operators_are_listed_once_with_colons() {
        let mut names: Vec<_> = OPERATORS.iter().map(|operator| operator.name).collect();
        assert!(names.iter().all(|name| name.ends_with(':')));
        for operator in OPERATORS {
            assert!(operator.example.starts_with(operator.name), "{operator:?}");
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), OPERATORS.len());
    }
}
