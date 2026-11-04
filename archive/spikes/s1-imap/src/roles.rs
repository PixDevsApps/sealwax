//! Special-folder detection: SPECIAL-USE attributes first, then a Norwegian (bokmål and
//! nynorsk) and English name table for the roles still missing.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    Flagged,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Inbox => "inbox",
            Role::Sent => "sent",
            Role::Drafts => "drafts",
            Role::Trash => "trash",
            Role::Junk => "junk",
            Role::Archive => "archive",
            Role::All => "all",
            Role::Flagged => "flagged",
        }
    }

    pub fn parse(text: &str) -> Option<Role> {
        [
            Role::Inbox,
            Role::Sent,
            Role::Drafts,
            Role::Trash,
            Role::Junk,
            Role::Archive,
            Role::All,
            Role::Flagged,
        ]
        .into_iter()
        .find(|role| role.as_str() == text)
    }

    /// The role named by a SPECIAL-USE attribute (RFC 6154), case-insensitively.
    pub fn from_attribute(attribute: &str) -> Option<Role> {
        match attribute.to_ascii_lowercase().as_str() {
            "\\sent" => Some(Role::Sent),
            "\\drafts" => Some(Role::Drafts),
            "\\trash" => Some(Role::Trash),
            "\\junk" => Some(Role::Junk),
            "\\archive" => Some(Role::Archive),
            "\\all" => Some(Role::All),
            "\\flagged" => Some(Role::Flagged),
            _ => None,
        }
    }
}

/// Leaf names per role, lower case, in order of preference.
const NAME_TABLE: &[(Role, &[&str])] = &[
    (
        Role::Sent,
        &[
            "sent",
            "sendt",
            "sent items",
            "sendte elementer",
            "sende element",
            "sent messages",
            "sendte meldinger",
            "sende meldingar",
            "sent mail",
            "sendt e-post",
            "sendte",
        ],
    ),
    (
        Role::Drafts,
        &["drafts", "utkast", "kladd", "kladdar", "kladder", "draft"],
    ),
    (
        Role::Trash,
        &[
            "trash",
            "papirkurv",
            "deleted items",
            "slettede elementer",
            "sletta element",
            "deleted messages",
            "slettede meldinger",
            "sletta meldingar",
            "bin",
            "slettet",
        ],
    ),
    (
        Role::Junk,
        &[
            "junk",
            "spam",
            "søppelpost",
            "junk mail",
            "junk e-mail",
            "junk email",
            "uønsket e-post",
            "uønskt e-post",
            "bulk mail",
        ],
    ),
    (Role::Archive, &["archive", "arkiv", "archives"]),
];

/// One LIST entry as role detection needs it.
#[derive(Clone, Debug)]
pub struct Entry<'a> {
    /// Decoded display name.
    pub name: &'a str,
    pub delimiter: Option<char>,
    pub attributes: &'a [String],
}

/// Assigns at most one mailbox per role. INBOX by name; then SPECIAL-USE attributes; then the
/// name table, only for top-level folders or direct children of INBOX (`INBOX.Sent`) or of a
/// provider prefix (`[Gmail]/Sendt e-post`), and only for roles no attribute already claimed.
pub fn assign(entries: &[Entry<'_>]) -> Vec<Option<Role>> {
    let mut roles: Vec<Option<Role>> = vec![None; entries.len()];
    let mut taken = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        if entry.name.eq_ignore_ascii_case("INBOX") {
            roles[i] = Some(Role::Inbox);
            taken.push(Role::Inbox);
            break;
        }
    }
    for (i, entry) in entries.iter().enumerate() {
        if roles[i].is_some() || is_noselect(entry) {
            continue;
        }
        if let Some(role) = entry
            .attributes
            .iter()
            .find_map(|a| Role::from_attribute(a))
            && !taken.contains(&role)
        {
            roles[i] = Some(role);
            taken.push(role);
        }
    }
    for &(role, names) in NAME_TABLE {
        if taken.contains(&role) {
            continue;
        }
        let candidate = names.iter().find_map(|wanted| {
            entries.iter().enumerate().find(|(i, entry)| {
                roles[*i].is_none()
                    && !is_noselect(entry)
                    && eligible_depth(entry)
                    && leaf(entry).to_lowercase() == *wanted
            })
        });
        if let Some((i, _)) = candidate {
            roles[i] = Some(role);
            taken.push(role);
        }
    }
    roles
}

fn is_noselect(entry: &Entry<'_>) -> bool {
    entry
        .attributes
        .iter()
        .any(|a| a.eq_ignore_ascii_case("\\Noselect") || a.eq_ignore_ascii_case("\\NonExistent"))
}

fn leaf<'a>(entry: &Entry<'a>) -> &'a str {
    match entry.delimiter {
        Some(d) => entry.name.rsplit(d).next().unwrap_or(entry.name),
        None => entry.name,
    }
}

fn eligible_depth(entry: &Entry<'_>) -> bool {
    let Some(d) = entry.delimiter else {
        return true;
    };
    let parts: Vec<&str> = entry.name.split(d).collect();
    match parts.as_slice() {
        [_] => true,
        [parent, _] => {
            parent.eq_ignore_ascii_case("INBOX")
                || (parent.starts_with('[') && parent.ends_with(']'))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(list: &[(&str, &[&str])], delimiter: char) -> Vec<(String, Option<Role>)> {
        let attributes: Vec<Vec<String>> = list
            .iter()
            .map(|(_, attrs)| attrs.iter().map(|a| a.to_string()).collect())
            .collect();
        let entries: Vec<Entry<'_>> = list
            .iter()
            .zip(&attributes)
            .map(|((name, _), attrs)| Entry {
                name,
                delimiter: Some(delimiter),
                attributes: attrs,
            })
            .collect();
        list.iter()
            .map(|(n, _)| n.to_string())
            .zip(assign(&entries))
            .collect()
    }

    fn role_of(result: &[(String, Option<Role>)], name: &str) -> Option<Role> {
        result.iter().find(|(n, _)| n == name).and_then(|(_, r)| *r)
    }

    #[test]
    fn special_use_attributes_win() {
        let result = run(
            &[
                ("INBOX", &[]),
                ("Sent Items", &["\\Sent"]),
                ("Deleted Items", &["\\Trash"]),
                ("Junk Mail", &["\\Junk"]),
                ("Drafts", &["\\Drafts"]),
                ("Sendt", &[]),
                ("Papirkurv", &[]),
                ("Søppelpost", &[]),
                ("Arkiv", &[]),
            ],
            '/',
        );
        assert_eq!(role_of(&result, "INBOX"), Some(Role::Inbox));
        assert_eq!(role_of(&result, "Sent Items"), Some(Role::Sent));
        assert_eq!(role_of(&result, "Deleted Items"), Some(Role::Trash));
        assert_eq!(role_of(&result, "Junk Mail"), Some(Role::Junk));
        assert_eq!(role_of(&result, "Sendt"), None);
        assert_eq!(role_of(&result, "Papirkurv"), None);
        assert_eq!(role_of(&result, "Søppelpost"), None);
        assert_eq!(role_of(&result, "Arkiv"), Some(Role::Archive));
    }

    #[test]
    fn norwegian_names_without_special_use() {
        let result = run(
            &[
                ("INBOX", &[]),
                ("Utkast", &[]),
                ("Sendt", &[]),
                ("Søppelpost", &[]),
                ("Papirkurv", &[]),
                ("Arkiv", &[]),
                ("Prosjekter/Sendt", &[]),
            ],
            '/',
        );
        assert_eq!(role_of(&result, "Utkast"), Some(Role::Drafts));
        assert_eq!(role_of(&result, "Sendt"), Some(Role::Sent));
        assert_eq!(role_of(&result, "Søppelpost"), Some(Role::Junk));
        assert_eq!(role_of(&result, "Papirkurv"), Some(Role::Trash));
        assert_eq!(role_of(&result, "Arkiv"), Some(Role::Archive));
        assert_eq!(role_of(&result, "Prosjekter/Sendt"), None);
    }

    #[test]
    fn outlook_and_gmail_style_names() {
        let outlook = run(
            &[
                ("Inbox", &[]),
                ("Sendte elementer", &[]),
                ("Slettede elementer", &[]),
                ("Uønsket e-post", &[]),
            ],
            '/',
        );
        assert_eq!(role_of(&outlook, "Inbox"), Some(Role::Inbox));
        assert_eq!(role_of(&outlook, "Sendte elementer"), Some(Role::Sent));
        assert_eq!(role_of(&outlook, "Slettede elementer"), Some(Role::Trash));
        assert_eq!(role_of(&outlook, "Uønsket e-post"), Some(Role::Junk));

        let gmail = run(
            &[
                ("INBOX", &[]),
                ("[Gmail]", &["\\Noselect"]),
                ("[Gmail]/Alle e-poster", &["\\All"]),
                ("[Gmail]/Sendt e-post", &[]),
                ("[Gmail]/Papirkurv", &["\\Trash"]),
            ],
            '/',
        );
        assert_eq!(role_of(&gmail, "[Gmail]"), None);
        assert_eq!(role_of(&gmail, "[Gmail]/Alle e-poster"), Some(Role::All));
        assert_eq!(role_of(&gmail, "[Gmail]/Sendt e-post"), Some(Role::Sent));
        assert_eq!(role_of(&gmail, "[Gmail]/Papirkurv"), Some(Role::Trash));
    }

    #[test]
    fn courier_style_inbox_children() {
        let result = run(
            &[
                ("INBOX", &[]),
                ("INBOX.Sendt", &[]),
                ("INBOX.Kunder.Sent", &[]),
            ],
            '.',
        );
        assert_eq!(role_of(&result, "INBOX.Sendt"), Some(Role::Sent));
        assert_eq!(role_of(&result, "INBOX.Kunder.Sent"), None);
    }

    #[test]
    fn one_mailbox_per_role() {
        let result = run(
            &[
                ("INBOX", &[]),
                ("Sent", &["\\Sent"]),
                ("Sent2", &["\\Sent"]),
                ("Sendt", &[]),
            ],
            '/',
        );
        assert_eq!(role_of(&result, "Sent"), Some(Role::Sent));
        assert_eq!(role_of(&result, "Sent2"), None);
        assert_eq!(role_of(&result, "Sendt"), None);
    }

    #[test]
    fn role_names_round_trip() {
        for role in [
            Role::Inbox,
            Role::Sent,
            Role::Drafts,
            Role::Trash,
            Role::Junk,
            Role::Archive,
            Role::All,
            Role::Flagged,
        ] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
    }
}
