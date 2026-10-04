//! `shell.toml`, read the way omarchy-shell reads it (`Commons/Color.qml`): the theme's
//! generated `current/theme/shell.toml`, with the user's `~/.config/omarchy/shell.toml` (where
//! `omarchy display text size` writes `[font] base-size`) merged over it.

use super::paths::OmarchyPaths;
use sealwax_domain::theme::ShellValues;
use std::path::Path;

/// omarchy-shell's line parser (`parseShell`): `[section]` headers and `key = value` lines
/// whose value is a quoted string, a number, two to four numbers or a bare word, each with an
/// optional `#` comment. Keys outside a section and anything else are skipped.
pub fn parse_shell(text: &str) -> ShellValues {
    let mut values = ShellValues::new();
    let mut section: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some((name, after)) = rest.split_once(']')
                && is_name(name)
                && is_comment_or_empty(after)
            {
                section = Some(name);
            }
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim_end();
        if !is_name(key) {
            continue;
        }
        if let (Some(section), Some(value)) = (section, parse_value(rest.trim_start())) {
            values.insert(format!("{section}.{key}"), value);
        }
    }
    values
}

fn parse_value(rest: &str) -> Option<String> {
    if let Some(inner) = rest.strip_prefix(['"', '\'']) {
        let close = inner.find(['"', '\''])?;
        let (value, after) = inner.split_at(close);
        return (!value.is_empty() && is_comment_or_empty(&after[1..])).then(|| value.to_owned());
    }
    let value = rest.split('#').next().unwrap_or_default().trim_end();
    let numbers: Vec<&str> = value.split_whitespace().collect();
    let numeric =
        (1..=4).contains(&numbers.len()) && numbers.iter().all(|number| is_number(number));
    (numeric || is_bare(value)).then(|| value.to_owned())
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_bare(text: &str) -> bool {
    text.starts_with(|character: char| character.is_ascii_alphabetic()) && is_name(text)
}

/// `-?\d+(\.\d+)?`
fn is_number(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(integer) && digits(fraction)
}

fn is_comment_or_empty(text: &str) -> bool {
    let text = text.trim_start();
    text.is_empty() || text.starts_with('#')
}

/// The merged values; a missing or unreadable file counts as empty, as in omarchy-shell.
pub fn read_shell(paths: &OmarchyPaths) -> ShellValues {
    let mut values = read_file(&paths.theme_shell_file());
    values.extend(read_file(&paths.user_shell_file()));
    values
}

fn read_file(path: &Path) -> ShellValues {
    std::fs::read(path)
        .map(|bytes| parse_shell(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn values(pairs: &[(&str, &str)]) -> ShellValues {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn parses_like_omarchy_shell() {
        let text = "\
outside = 1
# comment
[font]  # the type scale
base-size = 14 # px
heading = \"20\"
[controls]
normal-color        = \"#bebebe\"
normal-fill-alpha   = 0.04
normal-border-width = 1 2 1 2
selected-color = accent
bad value = 1
weird = 1 2 3 4 5
empty = \"\"
mixed = '#123456\" # mismatched quotes are fine
trailing = \"x\" junk
[bad section
also = kept
[spacing]
scale = -1.5
";
        assert_eq!(
            parse_shell(text),
            values(&[
                ("font.base-size", "14"),
                ("font.heading", "20"),
                ("controls.normal-color", "#bebebe"),
                ("controls.normal-fill-alpha", "0.04"),
                ("controls.normal-border-width", "1 2 1 2"),
                ("controls.selected-color", "accent"),
                ("controls.mixed", "#123456"),
                ("controls.also", "kept"),
                ("spacing.scale", "-1.5"),
            ])
        );
    }

    #[test]
    fn the_users_file_wins_over_the_themes() {
        let root = tempfile::tempdir().unwrap();
        let paths = OmarchyPaths::new(root.path().join("state"), &root.path().join("config"));
        assert!(read_shell(&paths).is_empty());
        fs::create_dir_all(paths.theme_shell_file().parent().unwrap()).unwrap();
        fs::write(
            paths.theme_shell_file(),
            "[font]\nbase-size = 12\n[controls]\nnormal-fill-alpha = 0.04\n",
        )
        .unwrap();
        fs::create_dir_all(&paths.config).unwrap();
        fs::write(paths.user_shell_file(), "[font]\nbase-size = 16\n").unwrap();
        assert_eq!(
            read_shell(&paths),
            values(&[
                ("font.base-size", "16"),
                ("controls.normal-fill-alpha", "0.04"),
            ])
        );
    }

    #[test]
    fn the_generated_theme_file_parses() {
        let text = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/theme/shell.toml"),
        )
        .unwrap();
        let values = parse_shell(&text);
        assert_eq!(values["font.base-size"], "12");
        assert_eq!(values["controls.normal-fill-alpha"], "0.04");
        assert_eq!(values["controls.selected-fill-alpha"], "0.18");
        assert_eq!(values["controls.pressed-fill-alpha"], "0.22");
        assert_eq!(values["controls.selection-fill-alpha"], "0.35");
        assert_eq!(values["spacing.scale"], "1.0");
        assert!(!values.contains_key("spacing.xxs"));
    }
}
