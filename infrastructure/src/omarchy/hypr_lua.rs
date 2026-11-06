//! Hyprland's `decoration.rounding` as its Lua configuration sets it, read without running it,
//! for a Flatpak: `hyprctl` isn't in the sandbox, and Hyprland's socket is never given to it
//! because the socket takes any command, `exec` too (the owner's decision, 2026-11-06).
//!
//! The reader follows the user's `~/.config/hypr/hyprland.lua` in order, as Hyprland loads it:
//! `require("default.hypr.omarchy")` stands for Omarchy's defaults (rounding 0, in
//! `/usr/share/omarchy`, which the sandbox doesn't see) and then the theme's `hyprland.lua`;
//! `require("hypr.<name>")` is the user's `~/.config/hypr/<name>.lua`;
//! `require("default.hypr.toggles")` is Omarchy's toggles in `~/.local/state/omarchy`, in
//! sorted order (where `omarchy hyprland corners` puts its choice). The last
//! `rounding = <number>` written directly in a `decoration = { … }` table wins, and Hyprland's
//! own default, 0, when none says. It is a reading, not an evaluation: a condition around a
//! setting isn't seen, and a value that isn't a plain number (a variable, a sum) makes the
//! answer unknown, so the corners stay square.

use super::OmarchyPaths;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest file read; Omarchy's are a few kilobytes.
const MAX_FILE: u64 = 1 << 20;

/// How deep `require`s are followed from `hyprland.lua`.
const MAX_DEPTH: usize = 4;

/// Omarchy's `default/hypr/looknfeel.lua` sets `rounding = 0`.
const OMARCHY_DEFAULT: u32 = 0;

/// Legacy toggles Omarchy never loads as code (its `toggles.lua`).
const SKIPPED_TOGGLES: [&str; 2] = ["touchpad-disabled", "touchscreen-disabled"];

/// The rounding in px, `None` when `hyprland.lua` is missing or the last setting isn't a
/// plain number.
pub fn rounding(paths: &OmarchyPaths) -> Option<u32> {
    let entry = paths.hypr.join("hyprland.lua");
    let Some(source) = read(&entry) else {
        tracing::info!("no Lua configuration for Hyprland to read; corners are square");
        return None;
    };
    let mut walk = Walk {
        paths,
        seen: HashSet::from([entry]),
        settings: Vec::new(),
    };
    walk.source(&source, 0);
    match walk.settings.last() {
        None => Some(0),
        Some(Setting::Px(px)) => Some(*px),
        Some(Setting::Unknown) => {
            tracing::warn!("Hyprland's rounding isn't a plain number; corners are square");
            None
        }
    }
}

/// A `rounding = …` in a `decoration` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    Px(u32),
    Unknown,
}

struct Walk<'a> {
    paths: &'a OmarchyPaths,
    seen: HashSet<PathBuf>,
    settings: Vec<Setting>,
}

impl Walk<'_> {
    fn source(&mut self, source: &str, depth: usize) {
        let tokens = tokens(source);
        let mut tables: Vec<Option<&str>> = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            let before = |back: usize| index.checked_sub(back).map(|at| &tokens[at]);
            let after = |ahead: usize| tokens.get(index + ahead);
            match *token {
                Token::Punct('{') => {
                    let key = match (before(2), before(1)) {
                        (Some(Token::Name(key)), Some(Token::Punct('='))) => Some(*key),
                        _ => None,
                    };
                    tables.push(key);
                }
                Token::Punct('}') => {
                    tables.pop();
                }
                Token::Name("rounding")
                    if tables.last() == Some(&Some("decoration"))
                        && after(1) == Some(&Token::Punct('='))
                        && after(2) != Some(&Token::Punct('='))
                        && before(1) != Some(&Token::Punct('.')) =>
                {
                    let value = match (after(2), after(3)) {
                        (
                            Some(Token::Number(number)),
                            None | Some(Token::Punct(',' | ';' | '}')),
                        ) => number.parse().map_or(Setting::Unknown, Setting::Px),
                        _ => Setting::Unknown,
                    };
                    self.settings.push(value);
                }
                Token::Name("require") if before(1) != Some(&Token::Punct('.')) => {
                    let module = match (after(1), after(2), after(3)) {
                        (Some(Token::Str(module)), _, _) => Some(*module),
                        (
                            Some(Token::Punct('(')),
                            Some(Token::Str(module)),
                            Some(Token::Punct(')')),
                        ) => Some(*module),
                        _ => None,
                    };
                    if let Some(module) = module
                        && depth < MAX_DEPTH
                    {
                        self.module(module, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }

    fn module(&mut self, module: &str, depth: usize) {
        match module {
            "default.hypr.omarchy" => {
                self.settings.push(Setting::Px(OMARCHY_DEFAULT));
                self.file(self.paths.theme_hyprland_file(), depth);
            }
            "omarchy.current.theme.hyprland" => self.file(self.paths.theme_hyprland_file(), depth),
            "default.hypr.toggles" => {
                for toggle in toggles(&self.paths.hyprland_toggles()) {
                    self.file(toggle, depth);
                }
            }
            _ => {
                if let Some(path) = user_module(&self.paths.hypr, module) {
                    self.file(path, depth);
                }
            }
        }
    }

    fn file(&mut self, path: PathBuf, depth: usize) {
        if !self.seen.insert(path.clone()) {
            return;
        }
        if let Some(source) = read(&path) {
            self.source(&source, depth);
        }
    }
}

/// `hypr.looknfeel` → `<hypr>/looknfeel.lua`; `None` for other modules and for names that
/// could leave the folder.
fn user_module(hypr: &Path, module: &str) -> Option<PathBuf> {
    let rest = module.strip_prefix("hypr.")?;
    let mut path = hypr.to_path_buf();
    for part in rest.split('.') {
        let plain = !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
        if !plain {
            return None;
        }
        path.push(part);
    }
    path.set_extension("lua");
    Some(path)
}

/// The toggle files in `dir`, sorted by name as Omarchy's `require_all` sorts them.
fn toggles(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "lua")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| !SKIPPED_TOGGLES.contains(&stem))
        })
        .collect();
    files.sort();
    files
}

/// A file's text, when it is a readable file of at most [`MAX_FILE`] bytes.
fn read(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_FILE + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > MAX_FILE {
        tracing::warn!("a Hyprland configuration file is too large to read");
        return None;
    }
    Some(text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a str),
    Number(&'a str),
    /// A string's text, escapes left as written.
    Str(&'a str),
    Punct(char),
}

/// Lua's tokens, comments dropped. Operators of two characters come as two `Punct`s.
fn tokens(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
        } else if source[at..].starts_with("--") {
            at += 2;
            at = match long_bracket(bytes, at) {
                Some((_, end)) => end,
                None => source[at..]
                    .find('\n')
                    .map_or(bytes.len(), |line| at + line),
            };
        } else if let Some((text, end)) = long_bracket(bytes, at) {
            out.push(Token::Str(&source[text.0..text.1]));
            at = end;
        } else if byte == b'"' || byte == b'\'' {
            let start = at + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != byte && bytes[end] != b'\n' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            let end = end.min(bytes.len());
            out.push(Token::Str(&source[start..end]));
            at = (end + 1).min(bytes.len());
        } else if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            let start = at;
            while at < bytes.len() {
                let next = bytes[at];
                let exponent = matches!(next, b'+' | b'-') && matches!(bytes[at - 1], b'e' | b'E');
                if next.is_ascii_alphanumeric() || next == b'.' || next == b'_' || exponent {
                    at += 1;
                } else {
                    break;
                }
            }
            out.push(Token::Number(&source[start..at]));
        } else if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            out.push(Token::Name(&source[start..at]));
        } else {
            let character = source[at..].chars().next().unwrap_or(' ');
            out.push(Token::Punct(character));
            at += character.len_utf8();
        }
    }
    out
}

/// A long bracket opening at `at` (`[[`, `[==[`): the text's byte range and the index after
/// its close (the end of the source when it never closes).
fn long_bracket(bytes: &[u8], at: usize) -> Option<((usize, usize), usize)> {
    if bytes.get(at) != Some(&b'[') {
        return None;
    }
    let level = bytes[at + 1..]
        .iter()
        .take_while(|&&byte| byte == b'=')
        .count();
    let open_end = at + 1 + level;
    if bytes.get(open_end) != Some(&b'[') {
        return None;
    }
    let start = open_end + 1;
    let mut close = Vec::with_capacity(level + 2);
    close.push(b']');
    close.extend(std::iter::repeat_n(b'=', level));
    close.push(b']');
    let found = bytes
        .get(start..)?
        .windows(close.len())
        .position(|window| window == close.as_slice());
    Some(match found {
        Some(offset) => ((start, start + offset), start + offset + close.len()),
        None => ((start, bytes.len()), bytes.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Desktop {
        _root: tempfile::TempDir,
        paths: OmarchyPaths,
    }

    impl Desktop {
        fn new(hyprland: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let paths = OmarchyPaths::new(
                root.path().join("state/omarchy"),
                &root.path().join("config"),
            );
            fs::create_dir_all(&paths.hypr).unwrap();
            fs::create_dir_all(paths.theme_hyprland_file().parent().unwrap()).unwrap();
            fs::create_dir_all(paths.hyprland_toggles()).unwrap();
            fs::write(paths.hypr.join("hyprland.lua"), hyprland).unwrap();
            Self { _root: root, paths }
        }

        fn user(&self, name: &str, text: &str) -> &Self {
            fs::write(self.paths.hypr.join(format!("{name}.lua")), text).unwrap();
            self
        }

        fn theme(&self, text: &str) -> &Self {
            fs::write(self.paths.theme_hyprland_file(), text).unwrap();
            self
        }

        fn toggle(&self, name: &str, text: &str) -> &Self {
            fs::write(
                self.paths.hyprland_toggles().join(format!("{name}.lua")),
                text,
            )
            .unwrap();
            self
        }

        fn rounding(&self) -> Option<u32> {
            rounding(&self.paths)
        }
    }

    /// The shape of Omarchy's own `~/.config/hypr/hyprland.lua`.
    const OMARCHY_HYPRLAND: &str = r#"
-- Learn how to configure Hyprland: https://wiki.hypr.land/Configuring/Start/
dofile((os.getenv("OMARCHY_PATH") or "/usr/share/omarchy") .. "/default/hypr/bootstrap.lua")
require("default.hypr.omarchy")
require("hypr.monitors")
require("hypr.input")
require("hypr.bindings")
require("hypr.looknfeel")
require("hypr.autostart")
require("default.hypr.toggles")
"#;

    /// This machine's `looknfeel.lua`, shortened.
    const ROUNDED_LOOKNFEEL: &str = r#"
-- Change the default Omarchy look'n'feel.
o.window(".*", { opacity = "1 override 1 override 1 override", opaque = true })
hl.config({
  general = {
    gaps_in = 2,
    gaps_out = 5,
  },
  decoration = {
    rounding = 8,
    blur = {
      enabled = true,
    },
  },
})
-- hl.config({
--   decoration = {
--     -- Use round window corners.
--     rounding = 12,
--   },
-- })
"#;

    #[test]
    fn stock_omarchy_is_square() {
        let desktop = Desktop::new(OMARCHY_HYPRLAND);
        desktop.user("looknfeel", "-- Change the default Omarchy look'n'feel.\n");
        assert_eq!(desktop.rounding(), Some(0));
    }

    #[test]
    fn reads_the_users_looknfeel() {
        let desktop = Desktop::new(OMARCHY_HYPRLAND);
        desktop.user("looknfeel", ROUNDED_LOOKNFEEL);
        assert_eq!(desktop.rounding(), Some(8));
    }

    #[test]
    fn later_files_win_as_hyprland_loads_them() {
        let desktop = Desktop::new(OMARCHY_HYPRLAND);
        desktop.theme("hl.config({ decoration = { rounding = 10 } })\n");
        assert_eq!(desktop.rounding(), Some(10));
        desktop.user("looknfeel", ROUNDED_LOOKNFEEL);
        assert_eq!(desktop.rounding(), Some(8));
        // `omarchy hyprland corners rounded` copies this file into the toggles.
        desktop.toggle(
            "corners-rounded",
            "hl.config({\n  decoration = {\n    rounding = 6,\n    rounding_power = 3,\n  },\n})\n",
        );
        desktop.toggle("bar-glassy", "-- nothing about corners\n");
        assert_eq!(desktop.rounding(), Some(6));
        desktop.toggle(
            "corners-sharp",
            "hl.config({ decoration = { rounding = 0 } })\n",
        );
        assert_eq!(desktop.rounding(), Some(0), "sharp sorts after rounded");
    }

    #[test]
    fn the_theme_counts_only_after_omarchys_defaults() {
        let desktop =
            Desktop::new("require(\"hypr.looknfeel\")\nrequire(\"default.hypr.omarchy\")\n");
        desktop.user("looknfeel", ROUNDED_LOOKNFEEL);
        assert_eq!(
            desktop.rounding(),
            Some(0),
            "the defaults load after the user's file"
        );
        desktop.theme("hl.config({ decoration = { rounding = 4 } })\n");
        assert_eq!(desktop.rounding(), Some(4));
    }

    #[test]
    fn only_decorations_rounding_counts() {
        let desktop = Desktop::new(OMARCHY_HYPRLAND);
        desktop.user(
            "looknfeel",
            r#"
o.window({ tag = "pop" }, { rounding = 8 })
hl.config({ general = { rounding = 9 }, decoration = { rounding_power = 2, shadow = { rounding = 7 } } })
local rounding = 5
hl.config({ group = { groupbar = { gradient_rounding = 3 } } })
if decoration.rounding == 4 then end
"#,
        );
        assert_eq!(desktop.rounding(), Some(0));
    }

    #[test]
    fn values_it_cant_read_mean_square() {
        let desktop = Desktop::new(OMARCHY_HYPRLAND);
        for unreadable in ["corner", "4 + 4", "-3", "8.5", "0x10", "math.floor(8)"] {
            desktop.user(
                "looknfeel",
                &format!("hl.config({{ decoration = {{ rounding = {unreadable} }} }})\n"),
            );
            assert_eq!(desktop.rounding(), None, "{unreadable}");
        }
        desktop.user("input", "");
        desktop.toggle(
            "corners-rounded",
            "hl.config({ decoration = { rounding = 6 } })\n",
        );
        assert_eq!(desktop.rounding(), Some(6), "a later plain number wins");
    }

    #[test]
    fn comments_and_strings_are_not_settings() {
        let desktop = Desktop::new(OMARCHY_HYPRLAND);
        desktop.user(
            "looknfeel",
            "--[[ hl.config({ decoration = { rounding = 20 } }) ]]\n\
             --[==[ decoration = { rounding = 21 } ]==]\n\
             local text = \"decoration = { rounding = 22 }\"\n\
             local long = [[ decoration = { rounding = 23 } ]]\n\
             hl.config({ decoration = { rounding = 3 } }) -- rounding = 24\n",
        );
        assert_eq!(desktop.rounding(), Some(3));
    }

    #[test]
    fn requires_stay_in_the_hypr_folder() {
        let desktop = Desktop::new(
            "require(\"default.hypr.omarchy\")\nrequire \"hypr.looks.corners\"\n\
             require(\"hypr..secret\")\nrequire(\"hypr.a/b\")\nrequire(\"other.module\")\n",
        );
        fs::create_dir_all(desktop.paths.hypr.join("looks")).unwrap();
        fs::write(
            desktop.paths.hypr.join("looks/corners.lua"),
            "hl.config({ decoration = { rounding = 5 } })\n",
        )
        .unwrap();
        assert_eq!(desktop.rounding(), Some(5));
        assert_eq!(user_module(Path::new("/h"), "hypr..x"), None);
        assert_eq!(user_module(Path::new("/h"), "hypr.a/b"), None);
        assert_eq!(user_module(Path::new("/h"), "default.hypr.x"), None);
        assert_eq!(
            user_module(Path::new("/h"), "hypr.looknfeel"),
            Some(PathBuf::from("/h/looknfeel.lua"))
        );
    }

    #[test]
    fn a_module_that_requires_itself_is_read_once() {
        let desktop = Desktop::new("require(\"hypr.loop\")\n");
        desktop.user(
            "loop",
            "require(\"hypr.loop\")\nhl.config({ decoration = { rounding = 2 } })\n",
        );
        assert_eq!(desktop.rounding(), Some(2));
    }

    #[test]
    fn legacy_toggles_are_skipped() {
        let desktop = Desktop::new(OMARCHY_HYPRLAND);
        desktop.toggle(
            "touchpad-disabled",
            "hl.config({ decoration = { rounding = 9 } })\n",
        );
        assert_eq!(desktop.rounding(), Some(0));
    }

    #[test]
    fn without_hyprland_lua_there_is_no_answer() {
        let desktop = Desktop::new("");
        fs::remove_file(desktop.paths.hypr.join("hyprland.lua")).unwrap();
        assert_eq!(desktop.rounding(), None);
    }

    #[test]
    fn tokens_survive_unfinished_text() {
        for source in [
            "--[[ never closed",
            "\"never closed",
            "[==[ never",
            "x = 1e-",
            "{",
        ] {
            let _ = tokens(source);
        }
        assert_eq!(
            tokens("a.b = { 1e+3, 'x' }"),
            [
                Token::Name("a"),
                Token::Punct('.'),
                Token::Name("b"),
                Token::Punct('='),
                Token::Punct('{'),
                Token::Number("1e+3"),
                Token::Punct(','),
                Token::Str("x"),
                Token::Punct('}'),
            ]
        );
    }
}
