//! Special-folder detection (ADR-006): INBOX by name, then SPECIAL-USE attributes (RFC 6154),
//! then a Norwegian (bokmål and nynorsk) and English name table for the roles still missing.

use sealwax_domain::model::FolderRole;

/// The role a SPECIAL-USE attribute names, case-insensitively. `\Flagged` has no folder role.
pub fn from_attribute(attribute: &str) -> Option<FolderRole> {
    match attribute.to_ascii_lowercase().as_str() {
        "\\sent" => Some(FolderRole::Sent),
        "\\drafts" => Some(FolderRole::Drafts),
        "\\trash" => Some(FolderRole::Trash),
        "\\junk" => Some(FolderRole::Junk),
        "\\archive" => Some(FolderRole::Archive),
        "\\all" => Some(FolderRole::All),
        _ => None,
    }
}

/// Leaf names per role, lower case, in order of preference.
const NAME_TABLE: &[(FolderRole, &[&str])] = &[
    (
        FolderRole::Sent,
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
        FolderRole::Drafts,
        &["drafts", "utkast", "kladd", "kladdar", "kladder", "draft"],
    ),
    (
        FolderRole::Trash,
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
        FolderRole::Junk,
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
    (FolderRole::Archive, &["archive", "arkiv", "archives"]),
];

/// One LIST entry as role detection needs it.
#[derive(Clone, Debug)]
pub struct Entry<'a> {
    /// The decoded display name.
    pub name: &'a str,
    pub delimiter: Option<char>,
    pub attributes: &'a [String],
}

/// Assigns at most one folder per role. INBOX by name; then SPECIAL-USE attributes; then the
/// name table, only for top-level folders or direct children of INBOX (`INBOX.Sent`) or of a
/// provider prefix (`[Gmail]/Sendt e-post`), and only for roles no attribute already claimed.
/// Folders without a role are `Other`.
pub fn assign(entries: &[Entry<'_>]) -> Vec<FolderRole> {
    let mut roles = vec![FolderRole::Other; entries.len()];
    let mut taken = Vec::new();
    if let Some(inbox) = entries
        .iter()
        .position(|entry| entry.name.eq_ignore_ascii_case("INBOX"))
    {
        roles[inbox] = FolderRole::Inbox;
        taken.push(FolderRole::Inbox);
    }
    for (i, entry) in entries.iter().enumerate() {
        if roles[i] != FolderRole::Other || !selectable(entry.attributes) {
            continue;
        }
        if let Some(role) = entry.attributes.iter().find_map(|a| from_attribute(a))
            && !taken.contains(&role)
        {
            roles[i] = role;
            taken.push(role);
        }
    }
    for &(role, names) in NAME_TABLE {
        if taken.contains(&role) {
            continue;
        }
        let candidate = names.iter().find_map(|wanted| {
            entries.iter().enumerate().position(|(i, entry)| {
                roles[i] == FolderRole::Other
                    && selectable(entry.attributes)
                    && eligible_depth(entry)
                    && leaf(entry).to_lowercase() == *wanted
            })
        });
        if let Some(i) = candidate {
            roles[i] = role;
            taken.push(role);
        }
    }
    roles
}

/// False for `\Noselect` and `\NonExistent` (LIST-EXTENDED) folders.
pub fn selectable(attributes: &[String]) -> bool {
    !attributes.iter().any(|attribute| {
        attribute.eq_ignore_ascii_case("\\Noselect")
            || attribute.eq_ignore_ascii_case("\\NonExistent")
    })
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

    fn run(list: &[(&str, &[&str])], delimiter: char) -> Vec<(String, FolderRole)> {
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

    fn role_of(result: &[(String, FolderRole)], name: &str) -> FolderRole {
        result.iter().find(|(n, _)| n == name).unwrap().1
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
        assert_eq!(role_of(&result, "INBOX"), FolderRole::Inbox);
        assert_eq!(role_of(&result, "Sent Items"), FolderRole::Sent);
        assert_eq!(role_of(&result, "Deleted Items"), FolderRole::Trash);
        assert_eq!(role_of(&result, "Junk Mail"), FolderRole::Junk);
        assert_eq!(role_of(&result, "Sendt"), FolderRole::Other);
        assert_eq!(role_of(&result, "Papirkurv"), FolderRole::Other);
        assert_eq!(role_of(&result, "Søppelpost"), FolderRole::Other);
        assert_eq!(role_of(&result, "Arkiv"), FolderRole::Archive);
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
        assert_eq!(role_of(&result, "Utkast"), FolderRole::Drafts);
        assert_eq!(role_of(&result, "Sendt"), FolderRole::Sent);
        assert_eq!(role_of(&result, "Søppelpost"), FolderRole::Junk);
        assert_eq!(role_of(&result, "Papirkurv"), FolderRole::Trash);
        assert_eq!(role_of(&result, "Arkiv"), FolderRole::Archive);
        assert_eq!(role_of(&result, "Prosjekter/Sendt"), FolderRole::Other);
    }

    #[test]
    fn nynorsk_outlook_and_gmail_style_names() {
        let nynorsk = run(
            &[
                ("Inbox", &[]),
                ("Sende element", &[]),
                ("Sletta element", &[]),
                ("Uønskt e-post", &[]),
                ("Kladdar", &[]),
            ],
            '/',
        );
        assert_eq!(role_of(&nynorsk, "Inbox"), FolderRole::Inbox);
        assert_eq!(role_of(&nynorsk, "Sende element"), FolderRole::Sent);
        assert_eq!(role_of(&nynorsk, "Sletta element"), FolderRole::Trash);
        assert_eq!(role_of(&nynorsk, "Uønskt e-post"), FolderRole::Junk);
        assert_eq!(role_of(&nynorsk, "Kladdar"), FolderRole::Drafts);

        let gmail = run(
            &[
                ("INBOX", &[]),
                ("[Gmail]", &["\\Noselect"]),
                ("[Gmail]/Alle e-poster", &["\\All"]),
                ("[Gmail]/Sendt e-post", &[]),
                ("[Gmail]/Papirkurv", &["\\Trash"]),
                ("[Gmail]/Stjernemerket", &["\\Flagged"]),
            ],
            '/',
        );
        assert_eq!(role_of(&gmail, "[Gmail]"), FolderRole::Other);
        assert_eq!(role_of(&gmail, "[Gmail]/Alle e-poster"), FolderRole::All);
        assert_eq!(role_of(&gmail, "[Gmail]/Sendt e-post"), FolderRole::Sent);
        assert_eq!(role_of(&gmail, "[Gmail]/Papirkurv"), FolderRole::Trash);
        assert_eq!(role_of(&gmail, "[Gmail]/Stjernemerket"), FolderRole::Other);
    }

    #[test]
    fn courier_style_inbox_children_and_one_folder_per_role() {
        let courier = run(
            &[
                ("INBOX", &[]),
                ("INBOX.Sendt", &[]),
                ("INBOX.Kunder.Sent", &[]),
            ],
            '.',
        );
        assert_eq!(role_of(&courier, "INBOX.Sendt"), FolderRole::Sent);
        assert_eq!(role_of(&courier, "INBOX.Kunder.Sent"), FolderRole::Other);

        let twice = run(
            &[
                ("INBOX", &[]),
                ("Sent", &["\\Sent"]),
                ("Sent2", &["\\Sent"]),
                ("Sendt", &[]),
                ("Gone", &["\\NonExistent", "\\Trash"]),
            ],
            '/',
        );
        assert_eq!(role_of(&twice, "Sent"), FolderRole::Sent);
        assert_eq!(role_of(&twice, "Sent2"), FolderRole::Other);
        assert_eq!(role_of(&twice, "Sendt"), FolderRole::Other);
        assert_eq!(role_of(&twice, "Gone"), FolderRole::Other);
    }
}
