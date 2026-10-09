//! The rule editor's state (ADR-020, docs/m9/rules-app.md): what its rows hold while the user
//! types, turned into a [`Rule`] for `domain::sieve` when every row is complete. Pure, so the
//! editor's checks are tested without a display.

use sealwax_domain::sieve::{Action, Condition, Field, Match, Operator, Reason, Rule};

/// What a condition row reads: a header, or the message's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldChoice {
    From,
    To,
    Cc,
    ToOrCc,
    Subject,
    ListId,
    ReplyTo,
    /// A header by name, typed beside it.
    Header,
    Size,
}

impl FieldChoice {
    /// In the order the field menu lists them.
    pub const ALL: [FieldChoice; 9] = [
        FieldChoice::From,
        FieldChoice::To,
        FieldChoice::Cc,
        FieldChoice::ToOrCc,
        FieldChoice::Subject,
        FieldChoice::ListId,
        FieldChoice::ReplyTo,
        FieldChoice::Header,
        FieldChoice::Size,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FieldChoice::From => "From",
            FieldChoice::To => "To",
            FieldChoice::Cc => "Cc",
            FieldChoice::ToOrCc => "To or Cc",
            FieldChoice::Subject => "Subject",
            FieldChoice::ListId => "List-Id",
            FieldChoice::ReplyTo => "Reply-To",
            FieldChoice::Header => "A header…",
            FieldChoice::Size => "Size",
        }
    }

    fn of(field: &Field) -> FieldChoice {
        match field {
            Field::From => FieldChoice::From,
            Field::To => FieldChoice::To,
            Field::Cc => FieldChoice::Cc,
            Field::ToOrCc => FieldChoice::ToOrCc,
            Field::Subject => FieldChoice::Subject,
            Field::ListId => FieldChoice::ListId,
            Field::ReplyTo => FieldChoice::ReplyTo,
            Field::Header(_) => FieldChoice::Header,
        }
    }
}

/// The operators of a text condition, in the order the operator menu lists them.
pub const OPERATORS: [Operator; 7] = [
    Operator::Contains,
    Operator::NotContains,
    Operator::Is,
    Operator::IsNot,
    Operator::BeginsWith,
    Operator::EndsWith,
    Operator::Matches,
];

pub fn operator_label(operator: Operator) -> &'static str {
    match operator {
        Operator::Contains => "contains",
        Operator::NotContains => "doesn't contain",
        Operator::Is => "is",
        Operator::IsNot => "isn't",
        Operator::BeginsWith => "begins with",
        Operator::EndsWith => "ends with",
        Operator::Matches => "matches",
    }
}

/// The unit a size is typed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeUnit {
    Bytes,
    Kilobytes,
    Megabytes,
}

impl SizeUnit {
    pub const ALL: [SizeUnit; 3] = [SizeUnit::Bytes, SizeUnit::Kilobytes, SizeUnit::Megabytes];

    pub fn label(self) -> &'static str {
        match self {
            SizeUnit::Bytes => "bytes",
            SizeUnit::Kilobytes => "KB",
            SizeUnit::Megabytes => "MB",
        }
    }

    /// Bytes in one unit; Sieve's K and M are 1,024 and 1,024² (RFC 5228 section 2.4.1).
    fn bytes(self) -> u64 {
        match self {
            SizeUnit::Bytes => 1,
            SizeUnit::Kilobytes => 1 << 10,
            SizeUnit::Megabytes => 1 << 20,
        }
    }

    /// `bytes` in the largest unit that divides it.
    pub fn split(bytes: u64) -> (u64, SizeUnit) {
        for unit in [SizeUnit::Megabytes, SizeUnit::Kilobytes] {
            if bytes != 0 && bytes.is_multiple_of(unit.bytes()) {
                return (bytes / unit.bytes(), unit);
            }
        }
        (bytes, SizeUnit::Bytes)
    }
}

/// What an action row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionChoice {
    MoveTo,
    CopyTo,
    MarkRead,
    Star,
    MoveToJunk,
    NotJunk,
    Delete,
    Stop,
}

impl ActionChoice {
    /// In the order the action menu lists them.
    pub const ALL: [ActionChoice; 8] = [
        ActionChoice::MoveTo,
        ActionChoice::CopyTo,
        ActionChoice::MarkRead,
        ActionChoice::Star,
        ActionChoice::MoveToJunk,
        ActionChoice::NotJunk,
        ActionChoice::Delete,
        ActionChoice::Stop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ActionChoice::MoveTo => "Move to folder",
            ActionChoice::CopyTo => "Copy to folder",
            ActionChoice::MarkRead => "Mark as read",
            ActionChoice::Star => "Star",
            ActionChoice::MoveToJunk => "Move to Junk",
            ActionChoice::NotJunk => "Not junk",
            ActionChoice::Delete => "Delete",
            ActionChoice::Stop => "Stop processing more rules",
        }
    }

    /// Whether the row also chooses a folder.
    pub fn takes_folder(self) -> bool {
        matches!(self, ActionChoice::MoveTo | ActionChoice::CopyTo)
    }

    fn of(action: &Action) -> (ActionChoice, Option<String>) {
        match action {
            Action::MoveTo(folder) => (ActionChoice::MoveTo, Some(folder.clone())),
            Action::CopyTo(folder) => (ActionChoice::CopyTo, Some(folder.clone())),
            Action::MarkRead => (ActionChoice::MarkRead, None),
            Action::Star => (ActionChoice::Star, None),
            Action::MoveToJunk => (ActionChoice::MoveToJunk, None),
            Action::NotJunk => (ActionChoice::NotJunk, None),
            Action::Delete => (ActionChoice::Delete, None),
            Action::Stop => (ActionChoice::Stop, None),
        }
    }
}

/// A condition row as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftCondition {
    pub field: FieldChoice,
    /// The header's name, for [`FieldChoice::Header`].
    pub header: String,
    pub operator: Operator,
    pub value: String,
    /// For [`FieldChoice::Size`]: over (true) or under.
    pub over: bool,
    pub size: String,
    pub unit: SizeUnit,
}

impl Default for DraftCondition {
    fn default() -> Self {
        Self {
            field: FieldChoice::From,
            header: String::new(),
            operator: Operator::Contains,
            value: String::new(),
            over: true,
            size: String::new(),
            unit: SizeUnit::Megabytes,
        }
    }
}

/// An action row as chosen; `folder` is a folder's full name, `None` until one is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftAction {
    pub choice: ActionChoice,
    pub folder: Option<String>,
}

/// The editor's rule, row by row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    pub enabled: bool,
    pub matching: Match,
    pub conditions: Vec<DraftCondition>,
    pub actions: Vec<DraftAction>,
}

/// Why the editor can't save yet, in its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    NoAction,
    NoFolder,
    NoHeaderName,
    BadHeaderName,
    NoValue,
    BadSize,
}

impl Problem {
    pub fn text(self) -> &'static str {
        match self {
            Problem::NoAction => "Add an action",
            Problem::NoFolder => "Choose a folder for each move and copy",
            Problem::NoHeaderName => "Type the header's name",
            Problem::BadHeaderName => "A header's name can't hold spaces, colons or accents",
            Problem::NoValue => "Type a value for each condition",
            Problem::BadSize => "Type the size as a whole number",
        }
    }
}

impl Default for Draft {
    /// A new rule: one empty condition and one move to a folder not chosen yet.
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            matching: Match::All,
            conditions: vec![DraftCondition::default()],
            actions: vec![DraftAction {
                choice: ActionChoice::MoveTo,
                folder: None,
            }],
        }
    }
}

impl Draft {
    /// The rows of an existing rule.
    pub fn from_rule(rule: &Rule) -> Draft {
        let conditions = rule
            .conditions
            .iter()
            .map(|condition| match condition {
                Condition::Field {
                    field,
                    operator,
                    value,
                } => DraftCondition {
                    field: FieldChoice::of(field),
                    header: match field {
                        Field::Header(name) => name.clone(),
                        _ => String::new(),
                    },
                    operator: *operator,
                    value: value.clone(),
                    ..DraftCondition::default()
                },
                Condition::SizeOver(bytes) | Condition::SizeUnder(bytes) => {
                    let (amount, unit) = SizeUnit::split(*bytes);
                    DraftCondition {
                        field: FieldChoice::Size,
                        over: matches!(condition, Condition::SizeOver(_)),
                        size: amount.to_string(),
                        unit,
                        ..DraftCondition::default()
                    }
                }
            })
            .collect();
        let actions = rule
            .actions
            .iter()
            .map(|action| {
                let (choice, folder) = ActionChoice::of(action);
                DraftAction { choice, folder }
            })
            .collect();
        Draft {
            name: rule.name.clone(),
            enabled: rule.enabled,
            matching: rule.matching,
            conditions,
            actions,
        }
    }

    /// Create Rule from Message: From is the sender's address, or List-Id contains the list's
    /// identifier when the message came from a list; a move to a folder not chosen yet. The
    /// name is the list's identifier, else the sender's name, else the address.
    pub fn from_message(sender: &str, sender_name: &str, list_id: Option<&str>) -> Draft {
        let list_id = list_id.map(str::trim).filter(|id| !id.is_empty());
        let (name, condition) = match list_id {
            Some(id) => (
                id.to_owned(),
                DraftCondition {
                    field: FieldChoice::ListId,
                    operator: Operator::Contains,
                    value: id.to_owned(),
                    ..DraftCondition::default()
                },
            ),
            None => {
                let name = sender_name.trim();
                (
                    if name.is_empty() { sender } else { name }.to_owned(),
                    DraftCondition {
                        field: FieldChoice::From,
                        operator: Operator::Is,
                        value: sender.to_owned(),
                        ..DraftCondition::default()
                    },
                )
            }
        };
        Draft {
            name,
            conditions: vec![condition],
            ..Draft::default()
        }
    }

    /// The rule, or the first thing that keeps it from being saved.
    pub fn to_rule(&self) -> Result<Rule, Problem> {
        if self.actions.is_empty() {
            return Err(Problem::NoAction);
        }
        let mut conditions = Vec::with_capacity(self.conditions.len());
        for row in &self.conditions {
            let field = match row.field {
                FieldChoice::Size => {
                    let amount: u64 = row.size.trim().parse().map_err(|_| Problem::BadSize)?;
                    let bytes = amount
                        .checked_mul(row.unit.bytes())
                        .ok_or(Problem::BadSize)?;
                    conditions.push(if row.over {
                        Condition::SizeOver(bytes)
                    } else {
                        Condition::SizeUnder(bytes)
                    });
                    continue;
                }
                FieldChoice::From => Field::From,
                FieldChoice::To => Field::To,
                FieldChoice::Cc => Field::Cc,
                FieldChoice::ToOrCc => Field::ToOrCc,
                FieldChoice::Subject => Field::Subject,
                FieldChoice::ListId => Field::ListId,
                FieldChoice::ReplyTo => Field::ReplyTo,
                FieldChoice::Header => {
                    let name = row.header.trim();
                    if name.is_empty() {
                        return Err(Problem::NoHeaderName);
                    }
                    Field::Header(name.to_ascii_lowercase())
                }
            };
            if row.value.trim().is_empty() {
                return Err(Problem::NoValue);
            }
            conditions.push(Condition::Field {
                field,
                operator: row.operator,
                value: row.value.trim().to_owned(),
            });
        }
        let mut actions = Vec::with_capacity(self.actions.len());
        for row in &self.actions {
            let folder = || row.folder.clone().ok_or(Problem::NoFolder);
            actions.push(match row.choice {
                ActionChoice::MoveTo => Action::MoveTo(folder()?),
                ActionChoice::CopyTo => Action::CopyTo(folder()?),
                ActionChoice::MarkRead => Action::MarkRead,
                ActionChoice::Star => Action::Star,
                ActionChoice::MoveToJunk => Action::MoveToJunk,
                ActionChoice::NotJunk => Action::NotJunk,
                ActionChoice::Delete => Action::Delete,
                ActionChoice::Stop => Action::Stop,
            });
        }
        let rule = Rule {
            name: self.name.trim().to_owned(),
            enabled: self.enabled,
            matching: self.matching,
            conditions,
            actions,
        };
        rule.validate().map_err(|reason| match reason {
            Reason::HeaderName => Problem::BadHeaderName,
            Reason::Folder | Reason::Sender => Problem::NoFolder,
        })?;
        Ok(rule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn condition(field: FieldChoice, operator: Operator, value: &str) -> DraftCondition {
        DraftCondition {
            field,
            operator,
            value: value.to_owned(),
            ..DraftCondition::default()
        }
    }

    #[test]
    fn a_new_rule_waits_for_a_value_and_a_folder() {
        let mut draft = Draft::default();
        assert_eq!(draft.to_rule().err(), Some(Problem::NoValue));
        draft.conditions[0].value = "kari@example.test".to_owned();
        assert_eq!(draft.to_rule().err(), Some(Problem::NoFolder));
        draft.actions[0].folder = Some("INBOX.Kunder".to_owned());
        let rule = draft.to_rule().unwrap();
        assert!(
            rule.actions == [Action::MoveTo("INBOX.Kunder".to_owned())],
            "the folder's full name"
        );
        assert!(rule.enabled);
        draft.actions.clear();
        assert_eq!(draft.to_rule().err(), Some(Problem::NoAction));
    }

    #[test]
    fn headers_and_sizes_are_checked() {
        let mut draft = Draft {
            conditions: vec![DraftCondition {
                field: FieldChoice::Header,
                value: "YES".to_owned(),
                ..DraftCondition::default()
            }],
            actions: vec![DraftAction {
                choice: ActionChoice::MoveToJunk,
                folder: None,
            }],
            ..Draft::default()
        };
        assert_eq!(draft.to_rule().err(), Some(Problem::NoHeaderName));
        draft.conditions[0].header = "X Spam".to_owned();
        assert_eq!(draft.to_rule().err(), Some(Problem::BadHeaderName));
        draft.conditions[0].header = " X-Spam-Flag ".to_owned();
        let rule = draft.to_rule().unwrap();
        assert!(matches!(
            &rule.conditions[0],
            Condition::Field { field: Field::Header(name), value, .. }
                if name == "x-spam-flag" && value == "YES"
        ));
        draft.conditions = vec![DraftCondition {
            field: FieldChoice::Size,
            size: "5,5".to_owned(),
            ..DraftCondition::default()
        }];
        assert_eq!(draft.to_rule().err(), Some(Problem::BadSize));
        draft.conditions[0].size = "5".to_owned();
        assert!(matches!(
            draft.to_rule().unwrap().conditions[0],
            Condition::SizeOver(5_242_880)
        ));
        draft.conditions[0].over = false;
        draft.conditions[0].unit = SizeUnit::Kilobytes;
        assert!(matches!(
            draft.to_rule().unwrap().conditions[0],
            Condition::SizeUnder(5_120)
        ));
        draft.conditions[0].size = u64::MAX.to_string();
        assert_eq!(draft.to_rule().err(), Some(Problem::BadSize));
        draft.conditions.clear();
        assert!(
            draft.to_rule().unwrap().conditions.is_empty(),
            "no condition: every message"
        );
    }

    #[test]
    fn rules_come_back_row_for_row() {
        let rule = Rule {
            name: "Kunder".to_owned(),
            enabled: false,
            matching: Match::Any,
            conditions: vec![
                Condition::Field {
                    field: Field::From,
                    operator: Operator::Contains,
                    value: "kari@example.test".to_owned(),
                },
                Condition::Field {
                    field: Field::Header("x-priority".to_owned()),
                    operator: Operator::Is,
                    value: "1".to_owned(),
                },
                Condition::SizeUnder(1_500),
                Condition::SizeOver(3 << 20),
            ],
            actions: vec![
                Action::MarkRead,
                Action::CopyTo("Arkiv".to_owned()),
                Action::MoveTo("INBOX.Kunder".to_owned()),
                Action::Stop,
            ],
        };
        let draft = Draft::from_rule(&rule);
        assert_eq!(draft.conditions[1].field, FieldChoice::Header);
        assert_eq!(draft.conditions[1].header, "x-priority");
        assert_eq!(
            (draft.conditions[2].size.as_str(), draft.conditions[2].unit),
            ("1500", SizeUnit::Bytes)
        );
        assert_eq!(
            (draft.conditions[3].size.as_str(), draft.conditions[3].unit),
            ("3", SizeUnit::Megabytes)
        );
        assert!(draft.to_rule().unwrap() == rule, "the same rule back");
    }

    #[test]
    fn a_message_gives_its_sender_or_its_list() {
        let draft = Draft::from_message("kari@example.test", "Kari Nordmann", None);
        assert_eq!(draft.name, "Kari Nordmann");
        assert_eq!(
            draft.conditions,
            [condition(
                FieldChoice::From,
                Operator::Is,
                "kari@example.test"
            )]
        );
        assert_eq!(draft.actions[0].choice, ActionChoice::MoveTo);
        assert_eq!(
            draft.actions[0].folder, None,
            "the folder is the user's to choose"
        );
        assert_eq!(draft.to_rule().err(), Some(Problem::NoFolder));
        let unnamed = Draft::from_message("kari@example.test", " ", None);
        assert_eq!(unnamed.name, "kari@example.test");
        let list = Draft::from_message("news@example.test", "News", Some("news.example.test"));
        assert_eq!(list.name, "news.example.test");
        assert_eq!(
            list.conditions,
            [condition(
                FieldChoice::ListId,
                Operator::Contains,
                "news.example.test"
            )]
        );
    }

    #[test]
    fn every_choice_has_a_label() {
        for field in FieldChoice::ALL {
            assert!(!field.label().is_empty());
        }
        for operator in OPERATORS {
            assert!(!operator_label(operator).is_empty());
        }
        for action in ActionChoice::ALL {
            assert!(!action.label().is_empty());
        }
        assert_eq!(SizeUnit::split(0), (0, SizeUnit::Bytes));
        assert_eq!(SizeUnit::split(2048), (2, SizeUnit::Kilobytes));
    }
}
