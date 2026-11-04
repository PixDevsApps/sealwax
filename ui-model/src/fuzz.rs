//! Entry points for the fuzz targets in `fuzz/` that reach the UI model (ML-093,
//! docs/m9/security.md), as `sealwax_domain::fuzz` does for the mail logic: arbitrary bytes
//! through one parser, the documented promises checked. The tests at the bottom run them over
//! the checked-in corpus (`fuzz/corpus/<target>/`). Built only for tests and with the `fuzz`
//! feature.

use crate::keymap::{Binding, Keymap};
use crate::search_hints as hints;
use crate::theme::{Palette, resolve_colors};
use sealwax_domain::search::Date;
use std::collections::BTreeMap;

/// `keys.toml` as a user may write it, and each of its first lines as one binding.
pub fn keys(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let _ = Keymap::parse(&text);
    for line in text.lines().take(16) {
        let _ = Binding::parse(line);
    }
}

/// A search query as typed, with the suggestions at every cursor position.
pub fn search_hints(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let today = Date::new(2026, 10, 7).expect("a date");
    let length = text.chars().count();
    for cursor in (0..=length).take(64) {
        if let Some(at) = hints::at(&text, cursor, today) {
            assert!(at.word.start <= at.word.end && at.word.end <= length);
            let (changed, cursor) = hints::accept(&text, at.word, "from:");
            assert!(cursor <= changed.chars().count());
        }
        let _ = hints::operators_at(&text, cursor);
    }
}

/// A theme's `colors.toml` values (`key = value` lines) through the palette Sealwax derives.
pub fn theme(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let raw: BTreeMap<String, String> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            (
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            )
        })
        .collect();
    let resolved = resolve_colors(&raw);
    let _ = Palette::from_resolved(&resolved);
    let _ = Palette::from_colors(&raw);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Runs `target` over every file in `fuzz/corpus/<name>/`.
    fn corpus(name: &str, target: fn(&[u8])) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fuzz/corpus")
            .join(name);
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("a corpus entry").path();
            let data = std::fs::read(&path).expect("a corpus file");
            let result = std::panic::catch_unwind(|| target(&data));
            assert!(result.is_ok(), "{name}: {} fails", path.display());
            files += 1;
        }
        assert!(files > 0, "{name}: an empty corpus");
    }

    #[test]
    fn config_corpus_as_keys() {
        corpus("config", super::keys);
    }

    #[test]
    fn search_corpus_hints() {
        corpus("search", super::search_hints);
    }

    #[test]
    fn theme_corpus() {
        corpus("theme", super::theme);
    }
}
