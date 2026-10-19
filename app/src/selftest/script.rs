//! The `.sealwax-test` script format (docs/TESTING.md), from Stet: one step per line, a command
//! followed by arguments separated by spaces. `#` starts a comment line. `"double quotes"` group
//! words and expand `\n`, `\r`, `\t`, `\\`, `\"` and `\$`. `$TMP` (the run's scratch directory)
//! and `$DIR` (the script's directory) are expanded everywhere. Unknown commands and wrong
//! argument counts fail before the app starts.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// 1-based line in the script.
    pub line: usize,
    /// The line as written, for the report.
    pub source: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Commands and their argument counts (minimum, maximum). Assertions (`assert-*`) may also
/// follow `wait-until`, which polls them, and `wait-within MS`, which polls them for at most MS
/// milliseconds.
pub const COMMANDS: &[(&str, usize, usize)] = &[
    // Before the app starts: `tiling` (no window controls, as on Hyprland) or `other`.
    ("desktop", 1, 1),
    // The window.
    ("resize", 2, 2),
    ("action", 1, 1),
    ("color-scheme", 1, 1),
    // A theme from tests/theme/ (or none) in the scratch Omarchy state; corners auto or px.
    ("theme", 1, 1),
    ("corners", 1, 1),
    // A file under $TMP, such as $TMP/config/sealwax/keys.toml.
    ("write-file", 2, 2),
    ("sidebar", 1, 1),
    ("list", 1, 1),
    ("reader", 1, 1),
    ("background", 1, 1),
    ("close-window", 0, 0),
    ("wait-frames", 1, 1),
    ("sleep", 1, 1),
    ("screenshot", 1, 2),
    ("dump-layout", 0, 0),
    ("wait-until", 1, usize::MAX),
    ("wait-within", 2, usize::MAX),
    ("quit", 0, 2),
    // Other processes and D-Bus.
    ("spawn", 0, usize::MAX),
    ("spawn-expect", 1, usize::MAX),
    ("spawn-bg", 1, usize::MAX),
    ("signal", 2, 2),
    ("wait-process", 2, 2),
    ("bus-open", 1, 1),
    ("bus-activate", 0, 0),
    ("test-bus-start", 0, 0),
    ("test-bus-spawn", 1, usize::MAX),
    ("test-bus-open", 1, 1),
    ("test-bus-activate", 0, 0),
    ("test-bus-signal", 1, 1),
    ("test-bus-stop", 0, 0),
    // Assertions.
    ("assert-layout", 1, 1),
    ("assert-pane", 1, 1),
    ("assert-folder", 1, 1),
    ("assert-shown", 2, 2),
    ("assert-width", 2, 2),
    ("assert-height", 2, 2),
    ("assert-max-width", 2, 2),
    ("assert-centred", 1, 1),
    // The first widget lies wholly right of the second (the setup assistant's scrollbar beside
    // its fields).
    ("assert-right-of", 2, 2),
    ("assert-sensitive", 2, 2),
    ("assert-label", 2, 2),
    ("assert-tooltip", 2, 2),
    // The look against the applied theme: text|background colour by role, the contrast of
    // its text on its background, its corner radius in px, its font (chrome|reading).
    ("assert-color", 3, 3),
    ("assert-contrast", 2, 2),
    ("assert-radius", 2, 2),
    ("assert-font", 2, 2),
    ("assert-window", 1, 1),
    ("assert-window-controls", 1, 1),
    ("assert-labelled", 0, 0),
    ("assert-folder-shown", 2, 2),
    ("assert-more", 2, 2),
    ("assert-compose-count", 1, 1),
    ("assert-compose", 3, 3),
    ("assert-activations", 1, 1),
    ("assert-background", 1, 1),
    ("assert-bus-owner", 1, 1),
    ("assert-running", 2, 2),
    ("assert-file-contains", 2, 2),
    ("assert-file-lacks", 2, 2),
    // Every web process Sealwax started is in WebKit's sandbox (M9).
    ("assert-web-sandbox", 0, 0),
];

fn arity(command: &str) -> Option<(usize, usize)> {
    COMMANDS
        .iter()
        .chain(super::a11y::COMMANDS)
        .chain(super::bodies::COMMANDS)
        .chain(super::composer::COMMANDS)
        .chain(super::conversation::COMMANDS)
        .chain(super::files::COMMANDS)
        .chain(super::folders::COMMANDS)
        .chain(super::mail::COMMANDS)
        .chain(super::perf::COMMANDS)
        .chain(super::reader::COMMANDS)
        .chain(super::reply::COMMANDS)
        .chain(super::rules::COMMANDS)
        .chain(super::outbox::COMMANDS)
        .chain(super::notify::COMMANDS)
        .chain(super::search::COMMANDS)
        .chain(super::background::COMMANDS)
        .chain(super::setup::COMMANDS)
        .chain(super::triage::COMMANDS)
        .find(|(name, _, _)| *name == command)
        .map(|&(_, min, max)| (min, max))
}

/// Parses a script. `vars` are the `$NAME` expansions.
pub fn parse(text: &str, vars: &[(&str, &str)]) -> Result<Vec<Step>, ParseError> {
    let mut steps = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let source = raw.trim();
        if source.is_empty() || source.starts_with('#') {
            continue;
        }
        let error = |message: String| ParseError { line, message };
        let mut words = tokenize(source, vars).map_err(error)?;
        let command = words.remove(0);
        check_arity(&command, words.len()).map_err(error)?;
        if command == "desktop" && !steps.is_empty() {
            return Err(error("desktop must be the first step".to_owned()));
        }
        if steps
            .last()
            .is_some_and(|step: &Step| step.command == "quit")
        {
            return Err(error("quit must be the last step".to_owned()));
        }
        if command == "wait-until" {
            if !words[0].starts_with("assert-") {
                return Err(error(format!(
                    "wait-until needs an assertion, not {}",
                    words[0]
                )));
            }
            check_arity(&words[0], words.len() - 1).map_err(error)?;
        }
        if command == "wait-within" {
            if words[0].parse::<u64>().is_err() {
                return Err(error(format!(
                    "wait-within needs milliseconds, not {}",
                    words[0]
                )));
            }
            if !words[1].starts_with("assert-") {
                return Err(error(format!(
                    "wait-within needs an assertion, not {}",
                    words[1]
                )));
            }
            check_arity(&words[1], words.len() - 2).map_err(error)?;
        }
        steps.push(Step {
            line,
            source: source.to_owned(),
            command,
            args: words,
        });
    }
    Ok(steps)
}

fn check_arity(command: &str, count: usize) -> Result<(), String> {
    let (min, max) = arity(command).ok_or_else(|| format!("unknown command {command}"))?;
    if count < min || count > max {
        let expected = match (min, max) {
            (min, max) if min == max => format!("{min}"),
            (min, usize::MAX) => format!("at least {min}"),
            (min, max) => format!("{min} to {max}"),
        };
        return Err(format!("{command} takes {expected} arguments, got {count}"));
    }
    Ok(())
}

fn tokenize(line: &str, vars: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else {
            break;
        };
        let mut word = String::new();
        if first == '"' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some('n') => word.push('\n'),
                        Some('r') => word.push('\r'),
                        Some('t') => word.push('\t'),
                        Some('\\') => word.push('\\'),
                        Some('"') => word.push('"'),
                        Some('$') => word.push('\u{0}'),
                        other => {
                            return Err(format!("unknown escape \\{}", other.unwrap_or(' ')));
                        }
                    },
                    c => word.push(c),
                }
            }
            if !closed {
                return Err("unterminated quote".to_owned());
            }
            if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                return Err("a quoted word must end at a space".to_owned());
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
        }
        words.push(expand(&word, vars)?);
    }
    if words.is_empty() {
        return Err("empty step".to_owned());
    }
    Ok(words)
}

/// Expands `$NAME`; an escaped `\$` was stored as NUL and becomes a literal `$`.
fn expand(word: &str, vars: &[(&str, &str)]) -> Result<String, String> {
    let mut out = String::with_capacity(word.len());
    let mut rest = word;
    while let Some(index) = rest.find('$') {
        out.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        let length = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..length];
        let value = vars
            .iter()
            .find(|(var, _)| *var == name)
            .map(|(_, value)| *value)
            .ok_or_else(|| format!("unknown variable ${name}"))?;
        out.push_str(value);
        rest = &after[length..];
    }
    out.push_str(rest);
    Ok(out.replace('\u{0}', "$"))
}

/// `true`/`false`.
pub fn boolean(word: &str) -> Result<bool, String> {
    match word {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected true or false, got {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VARS: &[(&str, &str)] = &[("TMP", "/tmp/run"), ("DIR", "/repo/tests")];

    #[test]
    fn parses_steps_skipping_comments() {
        let steps = parse(
            "# a comment\n\nresize 1440 900\n  spawn-expect 0 \"mailto:a@example.com?subject=Hi\\tthere\"\n",
            VARS,
        )
        .unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].line, 3);
        assert_eq!(steps[0].command, "resize");
        assert_eq!(steps[0].args, ["1440", "900"]);
        assert_eq!(
            steps[1].args,
            ["0", "mailto:a@example.com?subject=Hi\tthere"]
        );
    }

    #[test]
    fn expands_variables_and_keeps_escaped_dollars() {
        let steps = parse("screenshot \"$DIR/../\\$x-$TMP.png\"", VARS).unwrap();
        assert_eq!(steps[0].args, ["/repo/tests/../$x-/tmp/run.png"]);
    }

    #[test]
    fn rejects_mistakes_before_running() {
        for (script, message) in [
            ("frobnicate", "unknown command frobnicate"),
            ("resize 10", "resize takes 2 arguments, got 1"),
            (
                "spawn-expect",
                "spawn-expect takes at least 1 arguments, got 0",
            ),
            ("quit a b c", "quit takes 0 to 2 arguments, got 3"),
            ("screenshot \"open", "unterminated quote"),
            ("screenshot \"a\"b", "a quoted word must end at a space"),
            ("screenshot $HOME/x", "unknown variable $HOME"),
            ("screenshot \"\\q\"", "unknown escape \\q"),
            (
                "wait-until sleep 5",
                "wait-until needs an assertion, not sleep",
            ),
            (
                "wait-until assert-layout",
                "assert-layout takes 1 arguments, got 0",
            ),
            (
                "wait-within soon assert-layout x",
                "wait-within needs milliseconds, not soon",
            ),
            (
                "wait-within 500 sleep 5",
                "wait-within needs an assertion, not sleep",
            ),
        ] {
            let error = parse(script, VARS).unwrap_err();
            assert_eq!(error.message, message, "{script}");
            assert_eq!(error.line, 1);
        }
    }

    #[test]
    fn desktop_comes_first_and_quit_last() {
        assert!(parse("desktop other\nresize 800 600", VARS).is_ok());
        let error = parse("resize 800 600\ndesktop other", VARS).unwrap_err();
        assert_eq!(
            (error.line, error.message.as_str()),
            (2, "desktop must be the first step")
        );
        assert!(parse("resize 800 600\nquit signal TERM", VARS).is_ok());
        let error = parse("quit\nresize 800 600", VARS).unwrap_err();
        assert_eq!(
            (error.line, error.message.as_str()),
            (2, "quit must be the last step")
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(boolean("true"), Ok(true));
        assert!(boolean("yes").is_err());
    }

    #[test]
    fn every_script_in_the_repository_parses() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/selftest");
        let mut scripts = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "sealwax-test") {
                let text = std::fs::read_to_string(&path).unwrap();
                parse(&text, VARS).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                scripts += 1;
            }
        }
        assert!(scripts > 0, "no scripts in {}", dir.display());
    }
}
