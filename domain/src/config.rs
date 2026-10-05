//! `config.toml` (ADR-009): Sealwax's settings, in `$XDG_CONFIG_HOME/sealwax/`. The file is
//! TOML read with positions, so every mistake is reported with its line and column, and a
//! file with mistakes is not applied at all. Missing settings keep their defaults; a missing
//! file is the defaults. `keys.toml` shares the error type and helpers (`crate::keymap`).

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

pub const CONFIG_FILE: &str = "config.toml";
pub const KEYS_FILE: &str = "keys.toml";

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

    pub(crate) fn spanned(text: &str, span: Range<usize>, message: impl Into<String>) -> Self {
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
pub(crate) fn parse_toml(text: &str) -> Result<Spanned<DeTable<'_>>, FileError> {
    DeTable::parse(text).map_err(|error| {
        let message = error.message().lines().next().unwrap_or("invalid TOML");
        FileError::at(text, error.span().map_or(0, |span| span.start), message)
    })
}

pub(crate) type Entry<'a, 'i> = (&'a Spanned<Cow<'i, str>>, &'a Spanned<DeValue<'i>>);

/// A table's entries in the order the file lists them.
pub(crate) fn entries<'a, 'i>(table: &'a DeTable<'i>) -> Vec<Entry<'a, 'i>> {
    let mut entries: Vec<_> = table.iter().collect();
    entries.sort_by_key(|(key, _)| key.span().start);
    entries
}

/// The candidate closest to `word`, when it is a likely typo of it.
pub(crate) fn closest<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).clamp(1, 3);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(word, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// `; did you mean …?` for a likely typo, or nothing.
pub(crate) fn did_you_mean<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> String {
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
pub(crate) fn article(kind: &str) -> String {
    match kind.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => format!("an {kind}"),
        _ => format!("a {kind}"),
    }
}

pub use crate::theme::{
    ChromeFont, CornerRadius, MAX_RADIUS as MAX_CORNER_RADIUS, ReadingFont,
    ThemeOptions as Appearance,
};

/// An OAuth client registration that overrides the build's (ADR-008, OAUTH-SETUP.md). Only
/// the client ID: Microsoft's client is public, and no settings file holds a secret (ADR-009).
/// The section names are final once M2 builds sign-in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OAuthClient {
    pub client_id: Option<String>,
}

/// `[oauth.microsoft]`. Sign in with Google is deferred to 1.x (ADR-008 amendment).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OAuth {
    pub microsoft: OAuthClient,
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// The disk downloaded messages may use by default (a proposal, ML-044).
pub const DEFAULT_CACHE_SIZE: u64 = 2 * GIB;
/// The smallest `cache_size`: a few large messages must fit.
pub const MIN_CACHE_SIZE: u64 = 100 * MIB;
/// Days of Inbox mail downloaded in the background by default (a proposal, ML-044).
pub const DEFAULT_PREFETCH_DAYS: u32 = 14;
pub const MAX_PREFETCH_DAYS: u32 = 365;

/// `[storage]`: how much of an account's mail is kept on this computer (ML-044).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Storage {
    /// Bytes the downloaded messages may take; `None` for no limit. Past it, the messages
    /// opened or fetched longest ago go back to their headers and preview.
    pub cache_size: Option<u64>,
    /// Inbox messages from the last this many days are downloaded in the background; 0 turns
    /// it off.
    pub prefetch_days: u32,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            cache_size: Some(DEFAULT_CACHE_SIZE),
            prefetch_days: DEFAULT_PREFETCH_DAYS,
        }
    }
}

/// The settings in `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub appearance: Appearance,
    pub oauth: OAuth,
    pub storage: Storage,
}

const APPEARANCE_SETTINGS: [&str; 3] = ["corner_radius", "chrome_font", "reading_font"];
const STORAGE_SETTINGS: [&str; 2] = ["cache_size", "prefetch_days"];
const SECTIONS: [&str; 3] = ["appearance", "oauth", "storage"];
const PROVIDERS: [&str; 1] = ["microsoft"];

/// Where a setting written at the top level belongs.
fn section_of(name: &str) -> Option<&'static str> {
    if APPEARANCE_SETTINGS.contains(&name) {
        Some("[appearance]")
    } else if name == "client_id" {
        Some("[oauth.microsoft]")
    } else if STORAGE_SETTINGS.contains(&name) {
        Some("[storage]")
    } else {
        None
    }
}

/// A size such as `2 GiB`, `500 MB` or `1.5GB`: decimal units are powers of 1000, binary ones
/// of 1024.
fn size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let scale: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "b" => 1,
        "kb" => 1000,
        "mb" => 1000 * 1000,
        "gb" => 1000 * 1000 * 1000,
        "tb" => 1000 * 1000 * 1000 * 1000,
        "kib" => 1024,
        "mib" => MIB,
        "gib" => GIB,
        "tib" => 1024 * GIB,
        _ => return None,
    };
    let bytes = number * scale as f64;
    (bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes as u64)
}

/// Collects the mistakes of one `config.toml` read.
struct Reader<'t> {
    text: &'t str,
    errors: Vec<FileError>,
}

impl Reader<'_> {
    fn error(&mut self, span: Range<usize>, message: impl Into<String>) {
        self.errors
            .push(FileError::spanned(self.text, span, message));
    }

    /// The table in `value`, or a mistake that says what `name` should look like.
    fn table<'a, 'i>(
        &mut self,
        value: &'a Spanned<DeValue<'i>>,
        expected: &str,
    ) -> Option<&'a DeTable<'i>> {
        let table = value.get_ref().as_table();
        if table.is_none() {
            self.error(
                value.span(),
                format!("{expected} is a table, like [{expected}]"),
            );
        }
        table
    }

    fn appearance(&mut self, table: &DeTable<'_>, appearance: &mut Appearance) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            let span = value.span();
            match name {
                "corner_radius" => match corner_radius(value.get_ref()) {
                    Some(radius) => appearance.corner_radius = radius,
                    None => self.error(
                        span,
                        format!(
                            "corner_radius is \"auto\" or a whole number of pixels from 0 to \
                             {MAX_CORNER_RADIUS}, not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                "chrome_font" => match value.get_ref().as_str() {
                    Some("omarchy") => appearance.chrome_font = ChromeFont::Omarchy,
                    Some("sans") => appearance.chrome_font = ChromeFont::Sans,
                    _ => self.error(
                        span,
                        format!(
                            "chrome_font is \"omarchy\" or \"sans\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                "reading_font" => match value.get_ref().as_str() {
                    Some("sans") => appearance.reading_font = ReadingFont::Sans,
                    Some("mono") => appearance.reading_font = ReadingFont::Mono,
                    _ => self.error(
                        span,
                        format!(
                            "reading_font is \"sans\" or \"mono\", not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [appearance]{}",
                        did_you_mean(name, APPEARANCE_SETTINGS)
                    ),
                ),
            }
        }
    }

    fn oauth(&mut self, table: &DeTable<'_>, oauth: &mut OAuth) {
        for (key, value) in entries(table) {
            let provider = key.get_ref().as_ref();
            let (client, settings): (&mut OAuthClient, &[&str]) = match provider {
                "microsoft" => (&mut oauth.microsoft, &["client_id"]),
                _ => {
                    self.error(
                        key.span(),
                        format!(
                            "unknown provider oauth.{provider}{}; Sealwax signs in with \
                             microsoft",
                            did_you_mean(provider, PROVIDERS)
                        ),
                    );
                    continue;
                }
            };
            let section = format!("oauth.{provider}");
            let Some(table) = self.table(value, &section) else {
                continue;
            };
            for (key, value) in entries(table) {
                let name = key.get_ref().as_ref();
                if !settings.contains(&name) {
                    let hint = if name == "client_secret" {
                        "; Microsoft's client is public and has none, and no settings file \
                         holds a secret"
                            .to_owned()
                    } else {
                        did_you_mean(name, settings.iter().copied())
                    };
                    self.error(
                        key.span(),
                        format!("unknown setting {name} in [{section}]{hint}"),
                    );
                    continue;
                }
                let Some(text) = value.get_ref().as_str() else {
                    self.error(
                        value.span(),
                        format!(
                            "{name} takes a string in quotes, not {}",
                            article(value.get_ref().type_str())
                        ),
                    );
                    continue;
                };
                let text = Some(text.trim().to_owned()).filter(|text| !text.is_empty());
                client.client_id = text;
            }
        }
    }

    fn storage(&mut self, table: &DeTable<'_>, storage: &mut Storage) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            let span = value.span();
            match name {
                "cache_size" => match value.get_ref() {
                    DeValue::String(text) if text.trim().eq_ignore_ascii_case("unlimited") => {
                        storage.cache_size = None;
                    }
                    DeValue::String(text) => match size(text) {
                        Some(bytes) if bytes >= MIN_CACHE_SIZE => {
                            storage.cache_size = Some(bytes);
                        }
                        Some(_) => self.error(
                            span,
                            format!(
                                "cache_size is at least 100 MiB, not {}",
                                shown(value.get_ref())
                            ),
                        ),
                        None => self.error(
                            span,
                            format!(
                                "cache_size is a size such as \"2 GiB\" or \"500 MB\", or \
                                 \"unlimited\", not {}",
                                shown(value.get_ref())
                            ),
                        ),
                    },
                    other => self.error(
                        span,
                        format!(
                            "cache_size is a size in quotes such as \"2 GiB\", not {}",
                            shown(other)
                        ),
                    ),
                },
                "prefetch_days" => match whole_number(value.get_ref())
                    .and_then(|days| u32::try_from(days).ok())
                    .filter(|days| *days <= MAX_PREFETCH_DAYS)
                {
                    Some(days) => storage.prefetch_days = days,
                    None => self.error(
                        span,
                        format!(
                            "prefetch_days is a whole number of days from 0 to \
                             {MAX_PREFETCH_DAYS}, not {}",
                            shown(value.get_ref())
                        ),
                    ),
                },
                _ => self.error(
                    key.span(),
                    format!(
                        "unknown setting {name} in [storage]{}",
                        did_you_mean(name, STORAGE_SETTINGS)
                    ),
                ),
            }
        }
    }
}

fn whole_number(value: &DeValue<'_>) -> Option<i64> {
    match value {
        DeValue::Integer(integer) => i64::from_str_radix(integer.as_str(), integer.radix()).ok(),
        _ => None,
    }
}

fn corner_radius(value: &DeValue<'_>) -> Option<CornerRadius> {
    match value {
        DeValue::String(text) if text == "auto" => Some(CornerRadius::Auto),
        DeValue::Integer(integer) => i64::from_str_radix(integer.as_str(), integer.radix())
            .ok()
            .and_then(|pixels| u32::try_from(pixels).ok())
            .filter(|pixels| *pixels <= MAX_CORNER_RADIUS)
            .map(CornerRadius::Px),
        _ => None,
    }
}

/// A value as a mistake quotes it: a short string in quotes, or its type.
fn shown(value: &DeValue<'_>) -> String {
    match value {
        DeValue::String(text) if text.chars().count() <= 24 => format!("\"{text}\""),
        DeValue::Integer(integer) => integer.as_str().to_owned(),
        other => article(other.type_str()),
    }
}

impl Config {
    /// Reads `config.toml`. Every mistake is listed, in the order of the file.
    pub fn parse(text: &str) -> Result<Self, Vec<FileError>> {
        let root = parse_toml(text).map_err(|error| vec![error])?;
        let mut config = Self::default();
        let mut reader = Reader {
            text,
            errors: Vec::new(),
        };
        for (key, value) in entries(root.get_ref()) {
            let name = key.get_ref().as_ref();
            match name {
                "appearance" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.appearance(table, &mut config.appearance);
                    }
                }
                "oauth" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.oauth(table, &mut config.oauth);
                    }
                }
                "storage" => {
                    if let Some(table) = reader.table(value, name) {
                        reader.storage(table, &mut config.storage);
                    }
                }
                _ => match section_of(name) {
                    Some(section) => reader.error(
                        key.span(),
                        format!("{name} belongs in {section}; put it under that line"),
                    ),
                    None => {
                        let kind = if value.get_ref().as_table().is_some() {
                            "section"
                        } else {
                            "setting"
                        };
                        reader.error(
                            key.span(),
                            format!("unknown {kind} {name}{}", did_you_mean(name, SECTIONS)),
                        );
                    }
                },
            }
        }
        if reader.errors.is_empty() {
            Ok(config)
        } else {
            Err(reader.errors)
        }
    }
}

/// The `config.toml` that Preferences creates: every setting commented out with its default,
/// so the file changes nothing until a line is uncommented.
pub fn default_config_file() -> String {
    format!(
        "# Sealwax settings (config.toml). Uncomment a line and change it; Sealwax applies the\n\
         # file when you save it. A mistake is shown with its line and column, and the previous\n\
         # settings stay until the file is fixed.\n\
         \n\
         [appearance]\n\
         # Corners of rows, chips, cards and buttons: \"auto\" follows Hyprland's rounding (0\n\
         # outside Hyprland), or a number of pixels from 0 to {MAX_CORNER_RADIUS}.\n\
         # corner_radius = \"auto\"\n\
         # The sidebar, list and toolbars: \"omarchy\" (the Omarchy font) or \"sans\".\n\
         # chrome_font = \"omarchy\"\n\
         # Message text: \"sans\" or \"mono\".\n\
         # reading_font = \"sans\"\n\
         \n\
         # Your own OAuth client for Sign in with Microsoft, in place of the one Sealwax was\n\
         # built with (docs/OAUTH-SETUP.md).\n\
         [oauth.microsoft]\n\
         # client_id = \"00000000-0000-0000-0000-000000000000\"\n\
         \n\
         [storage]\n\
         # Disk space for messages downloaded from your accounts: a size such as \"2 GiB\" or\n\
         # \"500 MB\" (at least 100 MiB), or \"unlimited\". When it is full, the messages opened\n\
         # longest ago keep only their headers and preview, and download again when opened.\n\
         # cache_size = \"2 GiB\"\n\
         # Inbox messages from this many days download in the background, so they open\n\
         # without waiting; 0 turns it off.\n\
         # prefetch_days = {DEFAULT_PREFETCH_DAYS}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(text: &str) -> Vec<String> {
        Config::parse(text)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect()
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(Config::parse(""), Ok(Config::default()));
        assert_eq!(Config::parse("# only comments\n"), Ok(Config::default()));
        let defaults = Config::default();
        assert_eq!(defaults.appearance.corner_radius, CornerRadius::Auto);
        assert_eq!(defaults.appearance.chrome_font, ChromeFont::Omarchy);
        assert_eq!(defaults.appearance.reading_font, ReadingFont::Sans);
        assert_eq!(defaults.oauth, OAuth::default());
    }

    #[test]
    fn reads_every_setting() {
        let text = "[appearance]\ncorner_radius = 8\nchrome_font = \"sans\"\n\
                    reading_font = \"mono\"\n\n[oauth.microsoft]\nclient_id = \" id.example \"\n";
        let config = Config::parse(text).unwrap();
        assert_eq!(
            config.appearance,
            Appearance {
                corner_radius: CornerRadius::Px(8),
                chrome_font: ChromeFont::Sans,
                reading_font: ReadingFont::Mono,
            }
        );
        assert_eq!(
            config.oauth.microsoft.client_id.as_deref(),
            Some("id.example")
        );
        let config = Config::parse("[oauth.microsoft]\nclient_id = \"\"\n").unwrap();
        assert_eq!(config.oauth.microsoft, OAuthClient::default());
        let config = Config::parse("appearance.corner_radius = \"auto\"\n").unwrap();
        assert_eq!(config.appearance.corner_radius, CornerRadius::Auto);
        let config = Config::parse("[appearance]\ncorner_radius = 0x40\n").unwrap();
        assert_eq!(config.appearance.corner_radius, CornerRadius::Px(64));
    }

    #[test]
    fn mistakes_say_where_they_are() {
        assert_eq!(
            errors("[appearance]\ncorner_radius = 65\n"),
            [
                "line 2, column 17: corner_radius is \"auto\" or a whole number of pixels from 0 \
                 to 64, not 65"
            ]
        );
        assert_eq!(
            errors("[appearance]\ncorner_radius = \"round\"\n"),
            [
                "line 2, column 17: corner_radius is \"auto\" or a whole number of pixels from 0 \
                 to 64, not \"round\""
            ]
        );
        assert_eq!(
            errors("[appearance]\ncorner_radius = -1\n"),
            [
                "line 2, column 17: corner_radius is \"auto\" or a whole number of pixels from 0 \
                 to 64, not -1"
            ]
        );
        assert_eq!(
            errors("[appearance]\nchrome_font = \"serif\"\n"),
            ["line 2, column 15: chrome_font is \"omarchy\" or \"sans\", not \"serif\""]
        );
        assert_eq!(
            errors("[appearance]\nreading_font = true\n"),
            ["line 2, column 16: reading_font is \"sans\" or \"mono\", not a boolean"]
        );
        assert_eq!(
            errors("[appearance]\ncorner_radios = 4\n"),
            [
                "line 2, column 1: unknown setting corner_radios in [appearance]; did you mean \
                 corner_radius?"
            ]
        );
        assert_eq!(
            errors("[apperance]\n"),
            ["line 1, column 2: unknown section apperance; did you mean appearance?"]
        );
        assert_eq!(
            errors("corner_radius = 4\n"),
            ["line 1, column 1: corner_radius belongs in [appearance]; put it under that line"]
        );
        assert_eq!(
            errors("appearance = 3\n"),
            ["line 1, column 14: appearance is a table, like [appearance]"]
        );
        assert_eq!(
            errors("[oauth.yahoo]\n"),
            ["line 1, column 8: unknown provider oauth.yahoo; Sealwax signs in with microsoft"]
        );
        assert_eq!(
            errors("[oauth.microsoft]\nclient_secret = \"x\"\n"),
            [
                "line 2, column 1: unknown setting client_secret in [oauth.microsoft]; \
                 Microsoft's client is public and has none, and no settings file holds a secret"
            ]
        );
        assert_eq!(
            errors("[oauth.microsoft]\nclient_id = 7\n"),
            ["line 2, column 13: client_id takes a string in quotes, not an integer"]
        );
    }

    #[test]
    fn every_mistake_is_listed_in_file_order() {
        let found = errors("zzz = 1\n[appearance]\ncorner_radius = 900\nchrome_font = \"x\"\n");
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("line 1, column 1: unknown setting zzz"));
        assert!(found[1].starts_with("line 3,"));
        assert!(found[2].starts_with("line 4,"));
    }

    #[test]
    fn syntax_errors_have_positions() {
        let found = errors("[appearance]\ncorner_radius = \n");
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("line 2, column 17:"), "{found:?}");
        let found = errors("[appearance]\nchrome_font = \"unterminated\n");
        assert!(found[0].starts_with("line 2,"), "{found:?}");
        let found = errors("[appearance]\nchrome_font = \"sans\"\nchrome_font = \"sans\"\n");
        assert!(found[0].starts_with("line 3,"), "{found:?}");
    }

    #[test]
    fn positions_count_characters() {
        let error = FileError::at("ø = 1\nab", 8, "x");
        assert_eq!((error.line, error.column), (2, 2));
        let error = FileError::at("ø", 1, "inside a character");
        assert_eq!((error.line, error.column), (1, 1));
    }

    #[test]
    fn the_default_file_changes_nothing_and_every_line_is_valid() {
        let file = default_config_file();
        assert_eq!(Config::parse(&file), Ok(Config::default()));
        let uncommented: String = file
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(setting)
                    if setting
                        .split_once(" = ")
                        .is_some_and(|(key, _)| !key.contains(' ')) =>
                {
                    setting
                }
                _ => line,
            })
            .map(|line| format!("{line}\n"))
            .collect();
        let config = Config::parse(&uncommented).unwrap();
        assert_eq!(config.appearance, Appearance::default());
        assert!(config.oauth.microsoft.client_id.is_some());
    }

    #[test]
    fn storage_takes_sizes_with_units_and_days() {
        assert_eq!(Config::default().storage, Storage::default());
        assert_eq!(Storage::default().cache_size, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(Storage::default().prefetch_days, 14);
        let storage = |text: &str| Config::parse(text).unwrap().storage;
        assert_eq!(
            storage("[storage]\ncache_size = \"500 MB\"\nprefetch_days = 0\n"),
            Storage {
                cache_size: Some(500_000_000),
                prefetch_days: 0,
            }
        );
        assert_eq!(
            storage("[storage]\ncache_size = \"1.5GiB\"\n").cache_size,
            Some(1_610_612_736)
        );
        assert_eq!(
            storage("[storage]\ncache_size = \" Unlimited \"\n").cache_size,
            None
        );
        assert_eq!(storage("storage.prefetch_days = 365\n").prefetch_days, 365);
        assert_eq!(
            errors("[storage]\ncache_size = \"50 MiB\"\n"),
            ["line 2, column 14: cache_size is at least 100 MiB, not \"50 MiB\""]
        );
        assert_eq!(
            errors("[storage]\ncache_size = \"lots\"\n"),
            [
                "line 2, column 14: cache_size is a size such as \"2 GiB\" or \"500 MB\", or \
                 \"unlimited\", not \"lots\""
            ]
        );
        assert_eq!(
            errors("[storage]\ncache_size = 2000\n"),
            ["line 2, column 14: cache_size is a size in quotes such as \"2 GiB\", not 2000"]
        );
        assert_eq!(
            errors("[storage]\nprefetch_days = 400\n"),
            ["line 2, column 17: prefetch_days is a whole number of days from 0 to 365, not 400"]
        );
        assert_eq!(
            errors("[storage]\nprefetch_days = -1\n"),
            ["line 2, column 17: prefetch_days is a whole number of days from 0 to 365, not -1"]
        );
        assert_eq!(
            errors("[storage]\nprefech_days = 3\n"),
            [
                "line 2, column 1: unknown setting prefech_days in [storage]; did you mean \
                 prefetch_days?"
            ]
        );
        assert_eq!(
            errors("cache_size = \"1 GiB\"\n"),
            ["line 1, column 1: cache_size belongs in [storage]; put it under that line"]
        );
    }

    #[test]
    fn sizes_read_decimal_and_binary_units() {
        assert_eq!(size("2 GiB"), Some(2 * GIB));
        assert_eq!(size("2GB"), Some(2_000_000_000));
        assert_eq!(size("100 mib"), Some(100 * MIB));
        assert_eq!(size("0.5 KB"), Some(500));
        assert_eq!(size("12 B"), Some(12));
        assert_eq!(size("1 TiB"), Some(1024 * GIB));
        for bad in ["", "GiB", "2", "2 GiBs", "1e9 B", "-1 MB", "1..2 MB"] {
            assert_eq!(size(bad), None, "{bad}");
        }
    }

    #[test]
    fn typos_get_suggestions() {
        assert_eq!(
            closest("readingfont", APPEARANCE_SETTINGS),
            Some("reading_font")
        );
        assert_eq!(closest("zzz", APPEARANCE_SETTINGS), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
