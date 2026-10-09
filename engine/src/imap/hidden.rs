//! Folders left out of the folder list (the user's request, 2026-10-09): other mail programs'
//! housekeeping, which holds none of the user's mail. They are never stored or synced, and a
//! store that holds them loses them at the next account pass, as when a folder leaves the server.
//!
//! - **Outlook's sync logs**, "Sync Issues" and the folders under it, on any server: Outlook on
//!   Windows writes them to Exchange mailboxes. The English and Norwegian names, as seen; other
//!   languages' names join when seen.
//! - **Notes and Outbox on Microsoft's servers** (Outlook.com and Microsoft 365): Outlook's sticky
//!   notes and the mail Microsoft's own apps haven't sent yet. Elsewhere such a folder is the
//!   user's.
//! - **Mailspring's folder**, "Mailspring" at the top or under INBOX, where Mailspring keeps the
//!   messages it snoozes, and the folders under it, each while it holds no message: mail snoozed
//!   there shows it again.

/// Outlook's sync-log folder at the top of the mailbox, lower case.
const SYNC_ISSUES: &[&str] = &["sync issues", "synkroniseringsfeil"];
/// Outlook's folders at the top of a mailbox on Microsoft's servers, lower case.
const MICROSOFT: &[&str] = &["notes", "outbox"];
const MAILSPRING: &str = "mailspring";

/// Microsoft's servers, by host suffix: `outlook.office365.com`, `imap-mail.outlook.com`.
pub fn microsoft_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["office365.com", "outlook.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// One LIST entry as the rule needs it.
#[derive(Clone, Debug)]
pub struct Entry<'a> {
    /// The decoded display name.
    pub name: &'a str,
    pub delimiter: Option<char>,
    pub selectable: bool,
    /// MESSAGES from STATUS, `None` when the server didn't say.
    pub messages: Option<u32>,
}

/// Whether the folder stays out of the list; `microsoft` for an account on Microsoft's servers.
pub fn hidden(entry: &Entry<'_>, microsoft: bool) -> bool {
    let parts: Vec<String> = match entry.delimiter {
        Some(d) => entry.name.split(d).map(str::to_lowercase).collect(),
        None => vec![entry.name.to_lowercase()],
    };
    let top = parts[0].as_str();
    if SYNC_ISSUES.contains(&top) {
        return true;
    }
    if microsoft && parts.len() == 1 && MICROSOFT.contains(&top) {
        return true;
    }
    let mailspring = top == MAILSPRING
        || (top == "inbox" && parts.get(1).is_some_and(|part| part == MAILSPRING));
    mailspring && (!entry.selectable || entry.messages == Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, delimiter: char, messages: Option<u32>) -> Entry<'_> {
        Entry {
            name,
            delimiter: Some(delimiter),
            selectable: true,
            messages,
        }
    }

    #[test]
    fn outlooks_sync_logs_go_on_any_server() {
        for name in [
            "Synkroniseringsfeil",
            "Synkroniseringsfeil/Konflikter",
            "Synkroniseringsfeil/Lokale feil",
            "Synkroniseringsfeil/Serverfeil",
            "Sync Issues",
            "Sync Issues/Conflicts",
            "Sync Issues/Local Failures",
            "Sync Issues/Server Failures",
        ] {
            assert!(hidden(&entry(name, '/', Some(3)), false), "{name}");
            assert!(hidden(&entry(name, '/', Some(0)), true), "{name}");
        }
        assert!(!hidden(
            &entry("Prosjekter/Sync Issues", '/', Some(0)),
            true
        ));
        assert!(!hidden(&entry("Synkronisering", '/', Some(0)), true));
    }

    #[test]
    fn notes_and_outbox_go_only_on_microsofts_servers() {
        for name in ["Notes", "Outbox", "notes"] {
            assert!(hidden(&entry(name, '/', Some(2)), true), "{name}");
            assert!(!hidden(&entry(name, '/', Some(0)), false), "{name}");
        }
        assert!(!hidden(&entry("Prosjekter/Notes", '/', Some(0)), true));
        assert!(!hidden(&entry("Notater", '/', Some(0)), true));
    }

    #[test]
    fn mailsprings_folder_goes_while_it_is_empty() {
        for name in [
            "INBOX.Mailspring",
            "INBOX.Mailspring.Snoozed",
            "Mailspring.Snoozed",
        ] {
            assert!(hidden(&entry(name, '.', Some(0)), false), "{name}");
            assert!(
                !hidden(&entry(name, '.', Some(1)), false),
                "{name}: snoozed mail"
            );
            assert!(!hidden(&entry(name, '.', None), false), "{name}: no STATUS");
        }
        let parent = Entry {
            selectable: false,
            ..entry("Mailspring", '/', None)
        };
        assert!(hidden(&parent, false), "\\Noselect holds nothing");
        assert!(!hidden(
            &entry("INBOX.Kunder.Mailspring", '.', Some(0)),
            false
        ));
        assert!(!hidden(&entry("Mailspring2", '/', Some(0)), false));
    }

    #[test]
    fn microsofts_hosts_by_suffix() {
        assert!(microsoft_host("outlook.office365.com"));
        assert!(microsoft_host("imap-mail.outlook.com."));
        assert!(microsoft_host("Outlook.Office365.com"));
        assert!(!microsoft_host("imap.example.com"));
        assert!(!microsoft_host("notoutlook.com"));
        assert!(!microsoft_host("127.0.0.1"));
    }
}
