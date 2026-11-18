//! `config.toml`'s `[[open_with]]` (issue #39, ADR-031): commands the reader's More menu and the
//! palette offer as "Open With" and a name, each handed the path of a file that holds the shown
//! message's original. A command is a list of arguments, started without a shell; `{file}` in an
//! argument is replaced by the file's path, and nothing else in it changes. The rules are here;
//! [`crate::config`] reads the file and reports a broken rule at its line and column.

use std::ffi::OsString;
use std::path::Path;

/// The most entries Sealwax offers: one command per place, `open-with-1` to `open-with-9`.
pub const MAX_ENTRIES: usize = 9;

/// The longest name, in characters: it is shown in a menu after "Open With".
pub const MAX_NAME_CHARS: usize = 40;

/// What an argument holds where the file's path goes.
pub const FILE: &str = "{file}";

/// One `[[open_with]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWith {
    /// Shown as "Open With" and the name; trimmed, unique among the entries.
    pub name: String,
    /// The program, then its arguments; at least one of them holds [`FILE`].
    pub command: Vec<String>,
}

impl OpenWith {
    /// "Open With Archive": the command's title in the menu, the palette and toasts.
    pub fn title(&self) -> String {
        format!("Open With {}", self.name)
    }

    /// The program as written: a name looked up in `PATH`, or a path.
    pub fn program(&self) -> &str {
        self.command.first().map_or("", String::as_str)
    }

    /// The program and its arguments for `file`: every [`FILE`] replaced by its path, nothing
    /// else changed. No shell reads them, so a space, quote, `;`, `$` or `*` in an argument or
    /// in the path is passed as it is, and each argument stays one argument.
    pub fn arguments(&self, file: &Path) -> Vec<OsString> {
        self.command
            .iter()
            .map(|argument| {
                let mut out = OsString::with_capacity(argument.len());
                let mut pieces = argument.split(FILE);
                if let Some(first) = pieces.next() {
                    out.push(first);
                }
                for piece in pieces {
                    out.push(file.as_os_str());
                    out.push(piece);
                }
                out
            })
            .collect()
    }
}

/// What is wrong with a name, if anything; `name` is trimmed first.
pub fn name_problem(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        Some("name can't be empty; it is shown as \"Open With\" and the name".to_owned())
    } else if name.chars().any(char::is_control) {
        Some("name can't hold a line break, tab or other control character".to_owned())
    } else if name.chars().count() > MAX_NAME_CHARS {
        Some(format!("name is at most {MAX_NAME_CHARS} characters"))
    } else {
        None
    }
}

/// What is wrong with argument `index` (0 for the program), if anything.
pub fn argument_problem(index: usize, argument: &str) -> Option<&'static str> {
    if argument.contains('\0') {
        Some("an argument can't hold a NUL character")
    } else if index > 0 {
        None
    } else if argument.trim().is_empty() {
        Some("command's first item is the program, and it can't be empty")
    } else if argument.contains(FILE) {
        Some("command's first item is the program; put \"{file}\" in a later item")
    } else if argument.starts_with('~') {
        Some(
            "command's program starts with ~, which only a shell expands; write the full path, \
             such as \"/home/you/bin/archive\"",
        )
    } else {
        None
    }
}

/// What is wrong with a whole command whose arguments are each fine, if anything.
pub fn command_problem(command: &[String]) -> Option<&'static str> {
    if command.is_empty() {
        Some("command needs at least the program, such as [\"my-archiver\", \"{file}\"]")
    } else if !command
        .iter()
        .skip(1)
        .any(|argument| argument.contains(FILE))
    {
        Some(
            "command has no \"{file}\"; put it where the message's path goes, such as \
             [\"my-archiver\", \"{file}\"]",
        )
    } else {
        None
    }
}

/// Whether two names are the same entry's: compared trimmed and ignoring case, as a reader of
/// the menu would.
pub fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    fn entry(command: &[&str]) -> OpenWith {
        OpenWith {
            name: "Archive".to_owned(),
            command: command
                .iter()
                .map(|&argument| argument.to_owned())
                .collect(),
        }
    }

    fn arguments(command: &[&str], file: &str) -> Vec<String> {
        entry(command)
            .arguments(Path::new(file))
            .into_iter()
            .map(|argument| argument.into_string().unwrap())
            .collect()
    }

    #[test]
    fn the_path_takes_the_placeholders_place() {
        assert_eq!(
            arguments(&["my-archiver", "--import", "{file}"], "/run/a/b.eml"),
            ["my-archiver", "--import", "/run/a/b.eml"]
        );
        assert_eq!(
            arguments(
                &["tool", "--file={file}", "{file}.done", "{file}{file}"],
                "/x.eml"
            ),
            ["tool", "--file=/x.eml", "/x.eml.done", "/x.eml/x.eml"]
        );
        // Only `{file}` is replaced; other braces are the user's.
        assert_eq!(
            arguments(&["tool", "{File}", "{path}", "{}", "{{file}}"], "/x.eml"),
            ["tool", "{File}", "{path}", "{}", "{/x.eml}"]
        );
    }

    #[test]
    fn nothing_is_read_as_a_shell_would() {
        let file = "/run/user/1000/app/open-with/9f/Re: \"Q3\" $HOME; rm -rf * `id` & 'x'.eml";
        let got = arguments(
            &[
                "prog with space",
                "a b",
                "\"quoted\"",
                "x;y",
                "$HOME",
                "*",
                "{file}",
            ],
            file,
        );
        assert_eq!(
            got,
            [
                "prog with space",
                "a b",
                "\"quoted\"",
                "x;y",
                "$HOME",
                "*",
                file
            ]
        );
        assert_eq!(got.len(), 7, "each argument stays one argument");
    }

    #[test]
    fn a_path_that_isnt_utf8_is_passed_as_it_is() {
        let path = Path::new(OsStr::from_bytes(b"/tmp/\xff.eml"));
        let got = entry(&["tool", "--in={file}"]).arguments(path);
        assert_eq!(got[1].as_bytes(), b"--in=/tmp/\xff.eml");
    }

    #[test]
    fn titles_and_programs() {
        let open = entry(&["my-archiver", "{file}"]);
        assert_eq!(open.title(), "Open With Archive");
        assert_eq!(open.program(), "my-archiver");
    }

    #[test]
    fn names_are_short_plain_and_present() {
        assert_eq!(name_problem("Archive"), None);
        assert_eq!(name_problem(&"x".repeat(MAX_NAME_CHARS)), None);
        assert!(name_problem("").is_some());
        assert!(name_problem("  ").is_some());
        assert!(name_problem("two\nlines").is_some());
        assert!(name_problem("tab\there").is_some());
        assert!(name_problem(&"x".repeat(MAX_NAME_CHARS + 1)).is_some());
        assert!(same_name("Archive", " archive "));
        assert!(!same_name("Archive", "Archiver"));
    }

    #[test]
    fn the_program_is_first_and_plain() {
        assert_eq!(argument_problem(0, "my-archiver"), None);
        assert_eq!(argument_problem(0, "/usr/bin/my-archiver"), None);
        assert!(argument_problem(0, "").is_some());
        assert!(argument_problem(0, " ").is_some());
        assert!(argument_problem(0, "{file}").is_some());
        assert!(argument_problem(0, "~/bin/archive").is_some());
        assert_eq!(argument_problem(1, "~/notes"), None);
        assert_eq!(argument_problem(1, ""), None);
        assert!(argument_problem(1, "a\0b").is_some());
    }

    #[test]
    fn a_command_hands_over_the_file() {
        let strings =
            |items: &[&str]| -> Vec<String> { items.iter().map(|&item| item.to_owned()).collect() };
        assert_eq!(command_problem(&strings(&["tool", "{file}"])), None);
        assert_eq!(command_problem(&strings(&["tool", "--in={file}"])), None);
        assert!(command_problem(&[]).is_some());
        assert!(command_problem(&strings(&["tool"])).is_some());
        assert!(command_problem(&strings(&["tool", "{path}"])).is_some());
    }
}
