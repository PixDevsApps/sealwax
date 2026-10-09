//! Folder names and their levels (ADR-020): a folder's name is its whole path, its levels apart
//! by the server's hierarchy delimiter ("Kunder/Acme"), so the sidebar's tree, a new folder's
//! name and a renamed folder's subfolders all come from these. A name the user types is checked
//! here before anything is sent: [`check_name`] says why one is refused.

/// The last level of `name`: "Acme" of "Kunder/Acme".
pub fn leaf(name: &str, delimiter: Option<char>) -> &str {
    match delimiter.and_then(|delimiter| name.rsplit_once(delimiter)) {
        Some((_, leaf)) => leaf,
        None => name,
    }
}

/// The name of the level above `name`; `None` at the top.
pub fn parent(name: &str, delimiter: Option<char>) -> Option<&str> {
    delimiter
        .and_then(|delimiter| name.rsplit_once(delimiter))
        .map(|(parent, _)| parent)
}

/// Whether `name` is `ancestor` or a folder under it.
pub fn within(name: &str, ancestor: &str, delimiter: Option<char>) -> bool {
    name == ancestor
        || delimiter.is_some_and(|delimiter| {
            name.strip_prefix(ancestor)
                .is_some_and(|rest| rest.starts_with(delimiter))
        })
}

/// How many levels lie above `name`: 0 at the top.
pub fn depth(name: &str, delimiter: Option<char>) -> usize {
    delimiter.map_or(0, |delimiter| name.matches(delimiter).count())
}

/// The name of a folder called `leaf` under `parent`, or at the top.
pub fn child(parent: Option<&str>, delimiter: Option<char>, leaf: &str) -> String {
    match (parent, delimiter) {
        (Some(parent), Some(delimiter)) => format!("{parent}{delimiter}{leaf}"),
        _ => leaf.to_owned(),
    }
}

/// Which of an account's folders (`name`, `delimiter`) new folders at its top go under: the
/// Inbox, where every other folder is under it (servers that keep the personal folders under
/// `INBOX.`), else none. With the Inbox alone there is no telling, so none.
pub fn top_parent(folders: &[(&str, Option<char>)]) -> Option<usize> {
    let (inbox, (name, delimiter)) = folders
        .iter()
        .enumerate()
        .find(|(_, (name, _))| name.eq_ignore_ascii_case("INBOX"))?;
    let delimiter = (*delimiter)?;
    let length = name.len();
    let mut others = folders
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != inbox)
        .peekable();
    others.peek()?;
    others
        .all(|(_, (other, _))| {
            other.is_char_boundary(length)
                && other[..length].eq_ignore_ascii_case(name)
                && other[length..].starts_with(delimiter)
        })
        .then_some(inbox)
}

/// `name` once the folder `old` (itself or one above it) is renamed `new`: renaming "Kunder" to
/// "Klienter" makes "Kunder/Acme" "Klienter/Acme". `None` when `name` isn't within `old`.
pub fn renamed(name: &str, old: &str, new: &str, delimiter: Option<char>) -> Option<String> {
    within(name, old, delimiter).then(|| format!("{new}{}", &name[old.len()..]))
}

/// Why a typed name can't be a folder's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    /// It holds the server's hierarchy delimiter, which would make it two levels.
    Delimiter(char),
    /// It holds `*` or `%`, IMAP's wildcards in folder lists.
    Wildcard,
    /// It holds a line break or another control character.
    Control,
    /// A folder beside it has this name, whatever the case.
    Taken,
    /// Sealwax leaves folders with this name out of the list (other mail programs').
    Hidden,
}

impl NameProblem {
    /// What the dialog says.
    pub fn message(self) -> String {
        match self {
            NameProblem::Empty => "Type a name for the folder.".to_owned(),
            NameProblem::Delimiter(delimiter) => format!(
                "A folder name can't contain “{delimiter}”: it separates folders on this account."
            ),
            NameProblem::Wildcard => "A folder name can't contain “*” or “%”.".to_owned(),
            NameProblem::Control => "A folder name can't contain a line break.".to_owned(),
            NameProblem::Taken => "There is a folder with this name here already.".to_owned(),
            NameProblem::Hidden => {
                "Sealwax hides folders with this name: other mail programs keep their own \
                 things in them."
                    .to_owned()
            }
        }
    }
}

/// Where a typed name would go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placement<'a> {
    pub delimiter: Option<char>,
    /// The name of the folder above it; `None` at the top of the account.
    pub parent: Option<&'a str>,
    /// The last levels of the folders beside it, without the folder being renamed.
    pub siblings: Vec<&'a str>,
}

/// Checks `typed` as the last level of a folder at `placement`; `hidden` says whether Sealwax
/// would leave a folder of that whole name out of the list. Returns the name trimmed.
pub fn check_name(
    typed: &str,
    placement: &Placement<'_>,
    hidden: impl Fn(&str) -> bool,
) -> Result<String, NameProblem> {
    let name = typed.trim();
    if name.is_empty() {
        return Err(NameProblem::Empty);
    }
    if name.chars().any(char::is_control) {
        return Err(NameProblem::Control);
    }
    if let Some(delimiter) = placement.delimiter
        && name.contains(delimiter)
    {
        return Err(NameProblem::Delimiter(delimiter));
    }
    if name.contains(['*', '%']) {
        return Err(NameProblem::Wildcard);
    }
    let lower = name.to_lowercase();
    if placement
        .siblings
        .iter()
        .any(|sibling| sibling.to_lowercase() == lower)
    {
        return Err(NameProblem::Taken);
    }
    if hidden(&child(placement.parent, placement.delimiter, name)) {
        return Err(NameProblem::Hidden);
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLASH: Option<char> = Some('/');
    const DOT: Option<char> = Some('.');

    #[test]
    fn levels_by_the_delimiter() {
        assert_eq!(leaf("Kunder/Acme", SLASH), "Acme");
        assert_eq!(leaf("Kunder/Acme", DOT), "Kunder/Acme");
        assert_eq!(leaf("INBOX.Kunder.Acme", DOT), "Acme");
        assert_eq!(leaf("Kunder", SLASH), "Kunder");
        assert_eq!(leaf("Kunder/Acme", None), "Kunder/Acme");
        assert_eq!(parent("Kunder/Acme/2026", SLASH), Some("Kunder/Acme"));
        assert_eq!(parent("Kunder", SLASH), None);
        assert_eq!(parent("Kunder/Acme", None), None);
        assert_eq!(depth("Kunder/Acme/2026", SLASH), 2);
        assert_eq!(depth("Kunder", SLASH), 0);
        assert_eq!(depth("a/b", None), 0);
        assert_eq!(child(Some("Kunder"), SLASH, "Acme"), "Kunder/Acme");
        assert_eq!(child(None, SLASH, "Acme"), "Acme");
        assert_eq!(child(Some("INBOX"), DOT, "Høst 📬"), "INBOX.Høst 📬");
    }

    #[test]
    fn new_top_folders_go_under_an_inbox_that_holds_every_folder() {
        let inbox_server = [("INBOX", DOT), ("INBOX.Sent", DOT), ("inbox.Kunder", DOT)];
        assert_eq!(top_parent(&inbox_server), Some(0));
        let flat = [("INBOX", SLASH), ("Sent", SLASH), ("INBOX/Kunder", SLASH)];
        assert_eq!(top_parent(&flat), None);
        assert_eq!(top_parent(&[("INBOX", DOT)]), None, "the Inbox alone");
        assert_eq!(top_parent(&[("INBOX", None), ("INBOXES", None)]), None);
        assert_eq!(
            top_parent(&[("Sent", DOT), ("INBOX", DOT), ("INBOXé.x", DOT)]),
            None
        );
    }

    #[test]
    fn within_and_renamed_follow_whole_levels() {
        assert!(within("Kunder", "Kunder", SLASH));
        assert!(within("Kunder/Acme", "Kunder", SLASH));
        assert!(!within("Kunderservice", "Kunder", SLASH));
        assert!(!within("Kunder/Acme", "Kunder", None));
        assert_eq!(
            renamed("Kunder/Acme/2026", "Kunder", "Klienter", SLASH).as_deref(),
            Some("Klienter/Acme/2026")
        );
        assert_eq!(
            renamed("Kunder", "Kunder", "Klienter", SLASH).as_deref(),
            Some("Klienter")
        );
        assert_eq!(renamed("Kunderservice", "Kunder", "Klienter", SLASH), None);
    }

    fn placement<'a>(parent: Option<&'a str>, siblings: &[&'a str]) -> Placement<'a> {
        Placement {
            delimiter: SLASH,
            parent,
            siblings: siblings.to_vec(),
        }
    }

    #[test]
    fn names_are_checked_before_anything_is_sent() {
        let never = |_: &str| false;
        let here = placement(Some("Kunder"), &["Acme", "Fjord AS"]);
        assert_eq!(
            check_name("  Nordlys  ", &here, never),
            Ok("Nordlys".into())
        );
        assert_eq!(check_name("   ", &here, never), Err(NameProblem::Empty));
        assert_eq!(
            check_name("Nord/lys", &here, never),
            Err(NameProblem::Delimiter('/'))
        );
        assert_eq!(check_name("acme", &here, never), Err(NameProblem::Taken));
        assert_eq!(
            check_name("FJORD AS", &here, never),
            Err(NameProblem::Taken)
        );
        assert_eq!(
            check_name("Nord*", &here, never),
            Err(NameProblem::Wildcard)
        );
        assert_eq!(check_name("50%", &here, never), Err(NameProblem::Wildcard));
        assert_eq!(
            check_name("Nord\nlys", &here, never),
            Err(NameProblem::Control)
        );
        // Another delimiter's character is just a character.
        assert_eq!(check_name("v1.2", &here, never), Ok("v1.2".into()));
        let flat = Placement {
            delimiter: None,
            ..placement(None, &[])
        };
        assert_eq!(check_name("a/b", &flat, never), Ok("a/b".into()));
    }

    #[test]
    fn hidden_names_are_judged_by_the_whole_name() {
        let seen = std::cell::RefCell::new(Vec::new());
        let hidden = |name: &str| {
            seen.borrow_mut().push(name.to_owned());
            name.eq_ignore_ascii_case("Sync Issues")
        };
        let top = placement(None, &["INBOX"]);
        assert_eq!(
            check_name("sync issues", &top, hidden),
            Err(NameProblem::Hidden)
        );
        let under = placement(Some("Kunder"), &[]);
        assert_eq!(
            check_name("Sync Issues", &under, hidden),
            Ok("Sync Issues".into())
        );
        assert_eq!(*seen.borrow(), ["sync issues", "Kunder/Sync Issues"]);
        for problem in [
            NameProblem::Empty,
            NameProblem::Delimiter('.'),
            NameProblem::Wildcard,
            NameProblem::Control,
            NameProblem::Taken,
            NameProblem::Hidden,
        ] {
            assert!(problem.message().ends_with('.'), "{problem:?}");
        }
    }
}
