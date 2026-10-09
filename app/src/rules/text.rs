//! The rules' words in the app (ADR-020): a rule's one-line summary for the Rules dialog, the
//! toasts of Apply Rules, Always Junk and Never Junk, and why a rules file can't run. Pure, so
//! each is tested without a display; the user reviews them (docs/m9/rules-app.md).

use super::draft::{SizeUnit, operator_label};
use crate::triage::folder_title;
use sealwax_domain::folders;
use sealwax_domain::model::{Folder, FolderRole};
use sealwax_domain::sieve::{self, Action, Condition, Field, Match, Rule};
use sealwax_engine::api::RulesError;

/// What a folder is called in a rule's words: a role folder by its role's name, another by its
/// path, the folders above it first ("Inbox / Kunder", a role folder above by its role's
/// name); a name the account doesn't have is marked missing (a rule filing into it keeps the
/// message where it is).
pub fn folder_label(name: &str, folders: &[Folder]) -> String {
    let Some(folder) = folders.iter().find(|folder| folder.name == name) else {
        return format!("{name} (missing)");
    };
    if !matches!(folder.role, FolderRole::Other | FolderRole::All) {
        return folder_title(folder);
    }
    let mut levels = vec![folders::leaf(&folder.name, folder.delimiter).to_owned()];
    let mut current = folder.name.as_str();
    while let Some(parent) = folders::parent(current, folder.delimiter) {
        levels.push(
            folders
                .iter()
                .find(|candidate| candidate.name == parent)
                .filter(|parent| !matches!(parent.role, FolderRole::Other | FolderRole::All))
                .map_or_else(
                    || folders::leaf(parent, folder.delimiter).to_owned(),
                    folder_title,
                ),
        );
        current = parent;
    }
    levels.reverse();
    levels.join(" / ")
}

/// A size in the largest unit that divides it: "5 MB", "300 KB", "1500 bytes".
pub fn size_text(bytes: u64) -> String {
    let (amount, unit) = SizeUnit::split(bytes);
    format!("{amount} {}", unit.label())
}

fn field_label(field: &Field) -> &str {
    match field {
        Field::From => "From",
        Field::To => "To",
        Field::Cc => "Cc",
        Field::ToOrCc => "To or Cc",
        Field::Subject => "Subject",
        Field::ListId => "List-Id",
        Field::ReplyTo => "Reply-To",
        Field::Header(name) => name,
    }
}

/// One condition: "From contains kari@example.test", "Size over 5 MB".
pub fn condition_text(condition: &Condition) -> String {
    match condition {
        Condition::Field {
            field,
            operator,
            value,
        } => format!(
            "{} {} {value}",
            field_label(field),
            operator_label(*operator)
        ),
        Condition::SizeOver(bytes) => format!("Size over {}", size_text(*bytes)),
        Condition::SizeUnder(bytes) => format!("Size under {}", size_text(*bytes)),
    }
}

/// One action: "Move to Kunder", "Mark as read", "Stop".
pub fn action_text(action: &Action, folders: &[Folder]) -> String {
    match action {
        Action::MoveTo(folder) => format!("Move to {}", folder_label(folder, folders)),
        Action::CopyTo(folder) => format!("Copy to {}", folder_label(folder, folders)),
        Action::MarkRead => "Mark as read".to_owned(),
        Action::Star => "Star".to_owned(),
        Action::MoveToJunk => "Move to Junk".to_owned(),
        Action::NotJunk => "Not junk".to_owned(),
        Action::Delete => "Delete".to_owned(),
        Action::Stop => "Stop".to_owned(),
    }
}

/// A rule in one line: its conditions, an arrow, its actions. "From contains kari@example.test
/// → Move to Kunder"; "Every message → Star" for a rule without conditions.
pub fn summary(rule: &Rule, folders: &[Folder]) -> String {
    let joiner = match rule.matching {
        Match::All => " and ",
        Match::Any => " or ",
    };
    let conditions = if rule.conditions.is_empty() {
        "Every message".to_owned()
    } else {
        rule.conditions
            .iter()
            .map(condition_text)
            .collect::<Vec<_>>()
            .join(joiner)
    };
    let actions = if rule.actions.is_empty() {
        "nothing".to_owned()
    } else {
        rule.actions
            .iter()
            .map(|action| action_text(action, folders))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{conditions} → {actions}")
}

/// A rule's name in the dialog; an unnamed rule has none in the file.
pub fn rule_title(rule: &Rule) -> &str {
    if rule.name.trim().is_empty() {
        "Unnamed rule"
    } else {
        &rule.name
    }
}

/// Where a script is wrong, as the editor and the banner say it: "Line 3, column 7: …".
pub fn script_error(error: &sieve::Error) -> String {
    format!(
        "Line {}, column {}: {}",
        error.line, error.column, error.message
    )
}

/// Why an account's rules can't run, after the account's name in the banner's title.
pub fn problem_text(error: &RulesError) -> String {
    match error {
        RulesError::Script(error) => script_error(error),
        RulesError::File => {
            "The rules file can't be read: it must be UTF-8 text of at most 1 MiB".to_owned()
        }
        RulesError::Store => "The mail store couldn't be read".to_owned(),
    }
}

/// The banner's title for an account whose rules can't run.
pub fn failed_title(account: &str) -> String {
    format!("Rules in {account} can't run")
}

/// The banner's text under it.
pub fn failed_body(problem: &str) -> String {
    format!("{problem}. No rule runs until the rules file is fixed.")
}

fn messages(count: u32) -> String {
    match count {
        1 => "1 message".to_owned(),
        count => format!("{count} messages"),
    }
}

/// Apply Rules' toast while it runs.
pub fn applying_text(folder: &str) -> String {
    format!("Applying the rules to {folder}…")
}

/// Apply Rules' toast: what the rules changed in `folder`, or why they didn't run.
pub fn applied_text(result: &Result<u32, RulesError>, folder: &str) -> String {
    match result {
        Ok(0) => format!("No rule matched in {folder}"),
        Ok(count) => format!("Rules changed {} in {folder}", messages(*count)),
        Err(error) => format!("The rules didn't run. {}", problem_text(error)),
    }
}

/// What Always Junk or Never Junk did to the sender's list, as a sentence of its own.
pub fn listed_text(always: bool, address: &str, changed: bool) -> String {
    match (always, changed) {
        (true, true) => format!("Mail from {address} goes to Junk from now on"),
        (true, false) => format!("Mail from {address} already goes to Junk"),
        (false, true) => format!("Mail from {address} stays out of Junk from now on"),
        (false, false) => format!("Mail from {address} already stays out of Junk"),
    }
}

/// A command's title in a menu and the palette: "Always Junk Messages from kari@example.test".
pub fn sender_title(always: bool, address: Option<&str>) -> String {
    let start = if always {
        "Always Junk Messages from"
    } else {
        "Never Junk Messages from"
    };
    match address {
        Some(address) => format!("{start} {address}"),
        None => format!("{start} Sender"),
    }
}

/// The Rules dialog's title for an account.
pub fn dialog_title(account: &str) -> String {
    format!("Rules for {account}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::model::{AccountId, FolderId};
    use sealwax_domain::sieve::Operator;

    fn folders() -> Vec<Folder> {
        [
            (1, "INBOX", FolderRole::Inbox),
            (2, "Junk", FolderRole::Junk),
            (3, "INBOX.Kunder", FolderRole::Other),
            (4, "INBOX.Kunder.Acme", FolderRole::Other),
        ]
        .into_iter()
        .map(|(id, name, role)| Folder {
            id: FolderId(id),
            account: AccountId(1),
            name: name.to_owned(),
            delimiter: Some('.'),
            role,
            selectable: true,
            inferiors: true,
            total: 0,
            unread: 0,
        })
        .collect()
    }

    fn from_contains(value: &str) -> Condition {
        Condition::Field {
            field: Field::From,
            operator: Operator::Contains,
            value: value.to_owned(),
        }
    }

    #[test]
    fn summaries_say_the_conditions_then_the_actions() {
        let folders = folders();
        let rule = Rule {
            name: "Kunder".to_owned(),
            enabled: true,
            matching: Match::All,
            conditions: vec![from_contains("kari@example.test")],
            actions: vec![Action::MoveTo("INBOX.Kunder".to_owned())],
        };
        assert_eq!(
            summary(&rule, &folders),
            "From contains kari@example.test → Move to Inbox / Kunder"
        );
        assert_eq!(
            folder_label("INBOX.Kunder.Acme", &folders),
            "Inbox / Kunder / Acme"
        );
        let any = Rule {
            matching: Match::Any,
            conditions: vec![
                from_contains("kari@example.test"),
                Condition::Field {
                    field: Field::Header("x-spam-flag".to_owned()),
                    operator: Operator::Is,
                    value: "YES".to_owned(),
                },
                Condition::SizeOver(5 << 20),
            ],
            actions: vec![
                Action::MarkRead,
                Action::CopyTo("INBOX".to_owned()),
                Action::MoveTo("Gone".to_owned()),
                Action::Stop,
            ],
            ..rule.clone()
        };
        assert_eq!(
            summary(&any, &folders),
            "From contains kari@example.test or x-spam-flag is YES or Size over 5 MB → Mark \
             as read, Copy to Inbox, Move to Gone (missing), Stop"
        );
        let every = Rule {
            conditions: Vec::new(),
            actions: vec![Action::Star, Action::NotJunk, Action::Delete],
            ..rule.clone()
        };
        assert_eq!(
            summary(&every, &folders),
            "Every message → Star, Not junk, Delete"
        );
        assert_eq!(rule_title(&rule), "Kunder");
        let unnamed = Rule {
            name: " ".to_owned(),
            ..rule
        };
        assert_eq!(rule_title(&unnamed), "Unnamed rule");
        assert_eq!(size_text(1500), "1500 bytes");
        assert_eq!(size_text(300 << 10), "300 KB");
    }

    #[test]
    fn problems_name_the_line_and_column() {
        let error = sieve::compile("if header :contains \"subject\" \"x\" { fileinto \"A\"; }")
            .err()
            .unwrap();
        let text = problem_text(&RulesError::Script(error.clone()));
        assert!(
            text.starts_with(&format!("Line {}, column {}: ", error.line, error.column)),
            "{text}"
        );
        assert_eq!(failed_title("Work"), "Rules in Work can't run");
        assert_eq!(
            failed_body("Line 3, column 7: unknown command `x`"),
            "Line 3, column 7: unknown command `x`. No rule runs until the rules file is fixed."
        );
        assert_eq!(
            problem_text(&RulesError::File),
            "The rules file can't be read: it must be UTF-8 text of at most 1 MiB"
        );
    }

    #[test]
    fn toasts_count_what_changed() {
        assert_eq!(applied_text(&Ok(0), "Inbox"), "No rule matched in Inbox");
        assert_eq!(
            applied_text(&Ok(1), "Inbox"),
            "Rules changed 1 message in Inbox"
        );
        assert_eq!(
            applied_text(&Ok(4), "Inbox"),
            "Rules changed 4 messages in Inbox"
        );
        assert_eq!(
            applied_text(&Err(RulesError::Store), "Inbox"),
            "The rules didn't run. The mail store couldn't be read"
        );
        assert_eq!(applying_text("Inbox"), "Applying the rules to Inbox…");
        assert_eq!(
            listed_text(true, "kari@example.test", true),
            "Mail from kari@example.test goes to Junk from now on"
        );
        assert_eq!(
            listed_text(false, "kari@example.test", false),
            "Mail from kari@example.test already stays out of Junk"
        );
        assert_eq!(
            sender_title(true, Some("kari@example.test")),
            "Always Junk Messages from kari@example.test"
        );
        assert_eq!(sender_title(false, None), "Never Junk Messages from Sender");
        assert_eq!(dialog_title("Work"), "Rules for Work");
    }
}
