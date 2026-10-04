//! The active palette: `current/theme/colors.toml` through `omarchy-theme-color --all` when it
//! is installed, else the built-in port of its cascade ([`resolve_colors`]).

use super::command;
use super::paths::{OmarchyPaths, theme_color_script};
use sealwax_domain::theme::resolve_colors;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::time::Duration;

/// How long `omarchy-theme-color` may take (about 8 ms on the development machine).
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(2);

/// How often, and how far apart, a read that lands in `omarchy-theme-set`'s swap (`theme/`
/// removed, `next-theme/` not yet renamed) looks again.
const SWAP_RETRIES: u32 = 10;
const SWAP_WAIT: Duration = Duration::from_millis(50);

/// The longest theme name kept; `theme.name` is one short line written by Omarchy.
const NAME_LIMIT: usize = 64;

/// The active Omarchy theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmarchyTheme {
    /// `current/theme.name`'s first line, or `unnamed`.
    pub name: String,
    /// The palette as `omarchy-theme-color --all` resolves it.
    pub resolved: BTreeMap<String, String>,
}

/// Reads `colors.toml` leniently, like `omarchy-theme-color`: `key = "value"` lines, `#`
/// comments and blank lines. Unknown keys are kept; malformed lines are skipped.
pub fn parse_colors(text: &str) -> BTreeMap<String, String> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let key: String = key
        .chars()
        .filter(|character| !matches!(character, '"' | '\'') && !character.is_whitespace())
        .collect();
    let valid = !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if !valid {
        return None;
    }
    let value = match value.find(['"', '\'']) {
        Some(open) => {
            let rest = &value[open + 1..];
            rest.find(['"', '\'']).map_or(rest, |close| &rest[..close])
        }
        None => value.trim().split(" #").next().unwrap_or_default(),
    };
    Some((key, value.trim().to_owned()))
}

/// Reads the `key<TAB>value` lines `omarchy-theme-color --all` prints, dropping empty values.
pub fn parse_resolved(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(key, value)| !key.is_empty() && !value.trim().is_empty())
        .map(|(key, value)| (key.to_owned(), value.trim().to_owned()))
        .collect()
}

/// The fully resolved palette for a `colors.toml`, with every fallback applied, through
/// `script` when given (a failing script falls back to the built-in resolver with a
/// warning). Errors only when `file` cannot be read.
pub fn resolve_palette_with(
    file: &Path,
    script: Option<&Path>,
) -> io::Result<BTreeMap<String, String>> {
    let text = String::from_utf8_lossy(&std::fs::read(file)?).into_owned();
    if let Some(script) = script {
        match run_script(script, file) {
            Ok(colors) => return Ok(colors),
            Err(error) => {
                tracing::warn!(%error, "omarchy-theme-color failed; using the built-in resolver")
            }
        }
    }
    let mut colors = parse_colors(&text);
    let has_mode = ["mode", "theme_type"]
        .iter()
        .any(|key| colors.get(*key).is_some_and(|value| !value.is_empty()));
    if !has_mode && file.with_file_name("light.mode").is_file() {
        colors.insert("mode".to_owned(), "light".to_owned());
    }
    Ok(resolve_colors(&colors))
}

fn run_script(script: &Path, file: &Path) -> io::Result<BTreeMap<String, String>> {
    let args = [OsStr::new("--file"), file.as_os_str(), OsStr::new("--all")];
    let colors = parse_resolved(&command::stdout(script, &args, SCRIPT_TIMEOUT)?);
    if colors.is_empty() {
        return Err(io::Error::other("printed no colours"));
    }
    Ok(colors)
}

/// The active theme, or `None` outside Omarchy (no `current/theme/colors.toml`).
pub fn read_theme(paths: &OmarchyPaths) -> Option<OmarchyTheme> {
    read_theme_with(paths, theme_color_script().as_deref())
}

/// [`read_theme`] with an explicit resolver script; `None` uses the built-in resolver.
pub fn read_theme_with(paths: &OmarchyPaths, script: Option<&Path>) -> Option<OmarchyTheme> {
    let file = paths.colors_file();
    let mut attempts = 0;
    while !file.is_file() {
        let swapping = paths.current().join("next-theme").is_dir();
        if !swapping || attempts == SWAP_RETRIES {
            return None;
        }
        attempts += 1;
        std::thread::sleep(SWAP_WAIT);
    }
    let resolved = match resolve_palette_with(&file, script) {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(%error, "could not read the Omarchy palette; using a built-in one");
            return None;
        }
    };
    let name = std::fs::read_to_string(paths.theme_name_file()).unwrap_or_default();
    Some(OmarchyTheme {
        name: theme_name(&name),
        resolved,
    })
}

/// `theme.name`'s first line without control characters, at most 64 characters.
fn theme_name(text: &str) -> String {
    let name: String = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(NAME_LIMIT)
        .collect();
    if name.is_empty() {
        "unnamed".to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::omarchy::test_support::{script, spawn_lock};
    use std::fs;
    use std::path::PathBuf;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/theme")
    }

    fn fixture_names() -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(fixtures())
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn parses_quoted_values_and_skips_noise() {
        let text = "\u{feff}# Theme\n\nmode = \"dark\"\naccent = '#89b4fa' # inline\n\
                    \"background\" = \"#1e1e2e\"\r\nmy_extra-key=\"rgba(26a269ee) 45deg\"\n\
                    [section]\nno equals sign\nbad key = \"#ffffff\"\n# muted = \"#000000\"\n";
        assert_eq!(
            parse_colors(text),
            map(&[
                ("mode", "dark"),
                ("accent", "#89b4fa"),
                ("background", "#1e1e2e"),
                ("my_extra-key", "rgba(26a269ee) 45deg"),
                ("badkey", "#ffffff"),
            ])
        );
    }

    #[test]
    fn parses_unquoted_and_empty_values() {
        let text = "foreground = #cdd6f4 # comment\nmuted =  #585b70  \nselection = \"\"\nred =\n";
        assert_eq!(
            parse_colors(text),
            map(&[
                ("foreground", "#cdd6f4"),
                ("muted", "#585b70"),
                ("selection", ""),
                ("red", ""),
            ])
        );
    }

    #[test]
    fn parses_script_output() {
        let output = "# header\naccent\t#89b4fa\nblue\t\nbroken line\nmode\tdark\n\tlonely\n";
        assert_eq!(
            parse_resolved(output),
            map(&[("accent", "#89b4fa"), ("mode", "dark")])
        );
    }

    /// The built-in cascade gives exactly what `omarchy-theme-color --all` printed for every
    /// stock theme (stored in `tests/theme/`), so the fallback never drifts.
    #[test]
    fn builtin_resolver_matches_the_stored_script_output() {
        let names = fixture_names();
        assert_eq!(names.len(), 22);
        for name in names {
            let dir = fixtures().join(&name);
            let builtin = resolve_palette_with(&dir.join("colors.toml"), None).unwrap();
            let stored = parse_resolved(&fs::read_to_string(dir.join("resolved.tsv")).unwrap());
            assert_eq!(builtin, stored, "{name}");
        }
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = resolve_palette_with(&dir.path().join("colors.toml"), None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn light_mode_file_sets_the_mode_unless_the_theme_does() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "light.mode", "");
        let file = write(dir.path(), "colors.toml", "background = \"#101010\"\n");
        assert_eq!(resolve_palette_with(&file, None).unwrap()["mode"], "light");
        write(dir.path(), "colors.toml", "theme_type = \"dark\"\n");
        assert_eq!(resolve_palette_with(&file, None).unwrap()["mode"], "dark");
    }

    #[test]
    fn uses_the_script_output_when_it_succeeds() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "colors.toml", "accent = \"#000000\"\n");
        let resolver = script(
            dir.path(),
            "resolver",
            "[ \"$1\" = --file ] && [ \"$3\" = --all ] && printf 'accent\\t#123456\\nmode\\tlight\\n'",
        );
        assert_eq!(
            resolve_palette_with(&file, Some(&resolver)).unwrap(),
            map(&[("accent", "#123456"), ("mode", "light")])
        );
    }

    #[test]
    fn falls_back_when_the_script_fails_or_prints_nothing() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "colors.toml", "accent = \"#abcdef\"\n");
        for (name, body) in [("exits", "exit 3"), ("silent", "true")] {
            let resolver = script(dir.path(), name, body);
            let colors = resolve_palette_with(&file, Some(&resolver)).unwrap();
            assert_eq!(colors["accent"], "#abcdef", "{body}");
        }
        let missing = dir.path().join("no-such-script");
        let colors = resolve_palette_with(&file, Some(&missing)).unwrap();
        assert_eq!(colors["accent"], "#abcdef");
    }

    /// The installed `omarchy-theme-color` agrees with the stored output. Skipped outside
    /// Omarchy; reads only the fixtures.
    #[test]
    fn the_installed_script_agrees_with_the_fixtures() {
        let _spawning = spawn_lock();
        let Some(omarchy) = theme_color_script() else {
            return;
        };
        for name in ["matte-black", "catppuccin-latte", "gruvbox"] {
            let dir = fixtures().join(name);
            let live = resolve_palette_with(&dir.join("colors.toml"), Some(&omarchy)).unwrap();
            let stored = parse_resolved(&fs::read_to_string(dir.join("resolved.tsv")).unwrap());
            assert_eq!(live, stored, "{name}");
        }
    }

    #[test]
    fn reads_the_theme_and_its_name() {
        let state = tempfile::tempdir().unwrap();
        let paths = OmarchyPaths::new(state.path().to_path_buf(), state.path());
        assert_eq!(read_theme_with(&paths, None), None);
        write(
            state.path(),
            "current/theme/colors.toml",
            "mode = \"light\"\naccent = \"#1e66f5\"\n",
        );
        write(state.path(), "current/theme.name", "catppuccin-latte\n");
        let theme = read_theme_with(&paths, None).unwrap();
        assert_eq!(theme.name, "catppuccin-latte");
        assert_eq!(theme.resolved["mode"], "light");
        assert_eq!(theme.resolved["accent"], "#1e66f5");

        write(
            state.path(),
            "current/theme.name",
            "\u{1b}[31mred\u{7}\nsecond line\n",
        );
        assert_eq!(read_theme_with(&paths, None).unwrap().name, "[31mred");
        fs::remove_file(paths.theme_name_file()).unwrap();
        assert_eq!(read_theme_with(&paths, None).unwrap().name, "unnamed");
    }

    #[test]
    fn a_read_during_the_swap_waits_for_it() {
        let state = tempfile::tempdir().unwrap();
        let paths = OmarchyPaths::new(state.path().to_path_buf(), state.path());
        write(
            state.path(),
            "current/next-theme/colors.toml",
            "mode = \"dark\"\n",
        );
        write(state.path(), "current/theme.name", "matte-black\n");
        let current = paths.current();
        let swap = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            fs::rename(current.join("next-theme"), current.join("theme")).unwrap();
        });
        let theme = read_theme_with(&paths, None);
        swap.join().unwrap();
        assert_eq!(
            theme.map(|theme| theme.name).as_deref(),
            Some("matte-black")
        );
    }
}
