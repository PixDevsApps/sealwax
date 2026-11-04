//! What reading Sealwax's TOML settings files (`config.toml`, `keys.toml`, `accounts.toml`)
//! shares: TOML read with positions, and mistakes reported at a line and column with a
//! "did you mean" for likely typos.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

/// A mistake in a settings file, at a 1-based line and column (counted in characters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl FileError {
    /// The error at byte `byte` of `text`.
    pub fn at(text: &str, byte: usize, message: impl Into<String>) -> Self {
        let mut byte = byte.min(text.len());
        while !text.is_char_boundary(byte) {
            byte -= 1;
        }
        let line_start = text[..byte].rfind('\n').map_or(0, |at| at + 1);
        Self {
            line: text[..line_start].matches('\n').count() + 1,
            column: text[line_start..byte].chars().count() + 1,
            message: message.into(),
        }
    }

    pub fn spanned(text: &str, span: Range<usize>, message: impl Into<String>) -> Self {
        Self::at(text, span.start, message)
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for FileError {}

/// Parses TOML with positions; a syntax error becomes a [`FileError`].
pub fn parse_toml(text: &str) -> Result<Spanned<DeTable<'_>>, FileError> {
    DeTable::parse(text).map_err(|error| {
        let message = error.message().lines().next().unwrap_or("invalid TOML");
        FileError::at(text, error.span().map_or(0, |span| span.start), message)
    })
}

pub type Entry<'a, 'i> = (&'a Spanned<Cow<'i, str>>, &'a Spanned<DeValue<'i>>);

/// A table's entries in the order the file lists them.
pub fn entries<'a, 'i>(table: &'a DeTable<'i>) -> Vec<Entry<'a, 'i>> {
    let mut entries: Vec<_> = table.iter().collect();
    entries.sort_by_key(|(key, _)| key.span().start);
    entries
}

/// The candidate closest to `word`, when it is a likely typo of it.
pub fn closest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).clamp(1, 3);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(word, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// `; did you mean …?` for a likely typo, or nothing.
pub fn did_you_mean<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> String {
    closest(word, candidates)
        .map(|near| format!("; did you mean {near}?"))
        .unwrap_or_default()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(above + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// `a string`, `an integer`.
pub fn article(kind: &str) -> String {
    match kind.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => format!("an {kind}"),
        _ => format!("a {kind}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typos_get_suggestions() {
        let names = ["corner_radius", "chrome_font", "reading_font"];
        assert_eq!(closest("readingfont", names), Some("reading_font"));
        assert_eq!(closest("zzz", names), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
