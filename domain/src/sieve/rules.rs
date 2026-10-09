//! The rules model the Rules dialog edits (docs/m9/sieve.md): an account's [`RuleSet`], written
//! as Sieve in Roundcube's managesieve form and read back from it.
//!
//! The script [`RuleSet::render`] writes is canonical:
//!
//! 1. `require` with what the rest uses;
//! 2. the "never junk" section, for mail arriving in the Junk folder: trusted senders' mail goes
//!    back to the Inbox with `$NotJunk`, and the section ends with `stop`, so ordinary rules
//!    never pull spam out of Junk;
//! 3. the "always junk" rule: blocked senders' mail goes to Junk with `$Junk`;
//! 4. the user's rules in order, each `# rule:[Name]` and one `if`, a disabled one
//!    `if false # <its test>`, Roundcube's way; hand-written commands keep their place and text.
//!
//! Folder names are the folders' full names as the store's `Folder::name` holds them (decoded,
//! with the server's delimiter and prefix: `INBOX.Kunder`, `Kunder/2026`). That is what Sieve's
//! `fileinto` and `imap.mailbox` take (UTF-8; the server's modified UTF-7 is the engine's), so
//! the model and the script hold the same names and need no mapping; the dialog shows each by
//! its path.

mod read;

use super::check::is_header_name;
use std::fmt;
use std::fmt::Write as _;

/// The marker comment above the "never junk" section.
const NEVER_JUNK: &str = "Sealwax: never junk";
/// The marker comment above the "always junk" rule.
const ALWAYS_JUNK: &str = "Sealwax: always junk";

/// An account's rules.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RuleSet {
    /// "Never junk": mail from them that arrives in Junk goes back to the Inbox.
    pub trusted: Senders,
    /// "Always junk": mail from them goes to Junk.
    pub blocked: Senders,
    /// The user's rules, and commands written by hand, in order.
    pub items: Vec<Item>,
    /// The `require` names of the script read, kept for its hand-written commands.
    pub requires: Vec<String>,
}

/// One entry of the rule list.
#[derive(Clone, PartialEq, Eq)]
pub enum Item {
    Rule(Rule),
    /// Sieve the model doesn't hold as a rule (a command and the comments above it, or comments
    /// alone), kept as written: the dialog shows it as "Edited by hand".
    HandWritten(String),
}

/// A rule: when its conditions hold, its actions run.
#[derive(Clone, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub enabled: bool,
    pub matching: Match,
    /// None: every message.
    pub conditions: Vec<Condition>,
    /// Written in a fixed order whatever their order here (flags, then folders, then delete,
    /// then stop: flags must be set before `fileinto` to reach the filed message, RFC 5232), and
    /// read back in that order.
    pub actions: Vec<Action>,
}

/// Whether every condition must hold (`allof`) or any (`anyof`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    All,
    Any,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Condition {
    /// A header's text against a value.
    Field {
        field: Field,
        operator: Operator,
        value: String,
    },
    /// Larger than this many bytes.
    SizeOver(u64),
    /// Smaller than this many bytes.
    SizeUnder(u64),
}

/// The header a condition reads.
#[derive(Clone, PartialEq, Eq)]
pub enum Field {
    From,
    To,
    Cc,
    /// Either To or Cc.
    ToOrCc,
    Subject,
    ListId,
    ReplyTo,
    /// Any header by name; read back in lower case.
    Header(String),
}

impl Field {
    /// The header names it reads, in lower case.
    fn headers(&self) -> Vec<&str> {
        match self {
            Field::From => vec!["from"],
            Field::To => vec!["to"],
            Field::Cc => vec!["cc"],
            Field::ToOrCc => vec!["to", "cc"],
            Field::Subject => vec!["subject"],
            Field::ListId => vec!["list-id"],
            Field::ReplyTo => vec!["reply-to"],
            Field::Header(name) => vec![name],
        }
    }

    /// Whether it holds addresses: "is", "begins with", "ends with" and "matches" then compare
    /// each address (`address`); "contains" reads the whole header, display names too.
    fn holds_addresses(&self) -> bool {
        matches!(
            self,
            Field::From | Field::To | Field::Cc | Field::ToOrCc | Field::ReplyTo
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Contains,
    NotContains,
    Is,
    IsNot,
    BeginsWith,
    EndsWith,
    /// `*` and `?` wildcards; `\` makes the next character literal. "matches `abc*`" reads back
    /// as "begins with abc", and "matches `*abc`" as "ends with abc": the same test.
    Matches,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Action {
    /// `fileinto`.
    MoveTo(String),
    /// `fileinto :copy`.
    CopyTo(String),
    /// `addflag "\\Seen"`.
    MarkRead,
    /// `addflag "\\Flagged"`.
    Star,
    /// To the Junk folder with `$Junk` (and without `$NotJunk`), as Move to Junk does.
    MoveToJunk,
    /// To the Inbox with `$NotJunk` (and without `$Junk`), as Not Junk does.
    NotJunk,
    /// `discard`: Sealwax moves the message to Trash.
    Delete,
    /// `stop`: no later rule runs.
    Stop,
}

impl Action {
    /// Its place in the fixed order rules are written in.
    fn rank(&self) -> u8 {
        match self {
            Action::MoveToJunk => 0,
            Action::NotJunk => 1,
            Action::MarkRead => 2,
            Action::Star => 3,
            Action::CopyTo(_) => 4,
            Action::MoveTo(_) => 5,
            Action::Delete => 6,
            Action::Stop => 7,
        }
    }
}

/// Senders by address and by domain.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Senders {
    /// `local@domain`.
    pub addresses: Vec<String>,
    /// `example.test`: the domain of the From address, exactly (not its subdomains).
    pub domains: Vec<String>,
}

impl Senders {
    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty() && self.domains.is_empty()
    }

    /// Adds `address` (trimmed, without `<>`); `false` when it isn't one or is listed already
    /// (in any case).
    pub fn add_address(&mut self, address: &str) -> bool {
        add(&mut self.addresses, normal_address(address))
    }

    /// `false` when it wasn't listed.
    pub fn remove_address(&mut self, address: &str) -> bool {
        remove(&mut self.addresses, normal_address(address))
    }

    /// Adds `domain` (trimmed, without a leading `@`, in lower case); `false` when it isn't one
    /// or is listed already.
    pub fn add_domain(&mut self, domain: &str) -> bool {
        add(&mut self.domains, normal_domain(domain))
    }

    /// `false` when it wasn't listed.
    pub fn remove_domain(&mut self, domain: &str) -> bool {
        remove(&mut self.domains, normal_domain(domain))
    }

    fn validate(&self) -> Result<(), Reason> {
        let addresses = self
            .addresses
            .iter()
            .all(|address| normal_address(address).as_deref() == Some(address.as_str()));
        let domains = self
            .domains
            .iter()
            .all(|domain| is_domain(domain.trim()) && domain.trim() == domain);
        if addresses && domains {
            Ok(())
        } else {
            Err(Reason::Sender)
        }
    }
}

fn add(list: &mut Vec<String>, entry: Option<String>) -> bool {
    match entry {
        Some(entry) if !list.iter().any(|held| held.eq_ignore_ascii_case(&entry)) => {
            list.push(entry);
            true
        }
        _ => false,
    }
}

fn remove(list: &mut Vec<String>, entry: Option<String>) -> bool {
    let Some(entry) = entry else {
        return false;
    };
    let before = list.len();
    list.retain(|held| !held.eq_ignore_ascii_case(&entry));
    list.len() != before
}

fn normal_address(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(text);
    let (local, domain) = text.rsplit_once('@')?;
    let plain = text.chars().all(|c| !c.is_whitespace() && !c.is_control());
    (plain && !local.is_empty() && is_domain(domain)).then(|| text.to_owned())
}

fn normal_domain(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text.strip_prefix('@').unwrap_or(text).to_ascii_lowercase();
    is_domain(&text).then_some(text)
}

fn is_domain(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && c != '@')
}

/// The account's Inbox and Junk folders, by their full names.
#[derive(Clone, PartialEq, Eq)]
pub struct Folders {
    pub inbox: String,
    pub junk: String,
}

impl Default for Folders {
    fn default() -> Folders {
        Folders {
            inbox: "INBOX".to_owned(),
            junk: "Junk".to_owned(),
        }
    }
}

impl fmt::Debug for Folders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Folders").finish_non_exhaustive()
    }
}

/// Why a rule set can't be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid {
    /// The index in [`RuleSet::items`] of the rule at fault; `None` for the senders and the
    /// Inbox and Junk folders.
    pub item: Option<usize>,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// A header name that is empty or holds a space, a colon or a control character.
    HeaderName,
    /// A folder name that is empty or holds a control character.
    Folder,
    /// An address or a domain in the trusted or blocked senders that isn't one.
    Sender,
}

impl Rule {
    /// Whether it can be written: what [`RuleSet::render`] refuses, for the editor to say.
    pub fn validate(&self) -> Result<(), Reason> {
        let header = |condition: &Condition| match condition {
            Condition::Field {
                field: Field::Header(name),
                ..
            } => !is_header_name(name),
            _ => false,
        };
        let folder = |action: &Action| match action {
            Action::MoveTo(folder) | Action::CopyTo(folder) => !is_folder(folder),
            _ => false,
        };
        if self.conditions.iter().any(header) {
            Err(Reason::HeaderName)
        } else if self.actions.iter().any(folder) {
            Err(Reason::Folder)
        } else {
            Ok(())
        }
    }
}

fn is_folder(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(char::is_control)
}

impl RuleSet {
    /// The user's rules, without the hand-written items.
    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.items.iter().filter_map(|item| match item {
            Item::Rule(rule) => Some(rule),
            Item::HandWritten(_) => None,
        })
    }

    /// Whether part of the script was written by hand.
    pub fn is_hand_written(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, Item::HandWritten(_)))
    }

    /// Always Junk for `address`: added to the blocked senders, taken out of the trusted ones.
    /// `false` when it isn't an address or nothing changed.
    pub fn always_junk(&mut self, address: &str) -> bool {
        let trusted = self.trusted.remove_address(address);
        self.blocked.add_address(address) || trusted
    }

    /// Never Junk for `address`: added to the trusted senders, taken out of the blocked ones.
    pub fn never_junk(&mut self, address: &str) -> bool {
        let blocked = self.blocked.remove_address(address);
        self.trusted.add_address(address) || blocked
    }

    /// Always Junk for every address at `domain`.
    pub fn always_junk_domain(&mut self, domain: &str) -> bool {
        let trusted = self.trusted.remove_domain(domain);
        self.blocked.add_domain(domain) || trusted
    }

    /// Never Junk for every address at `domain`.
    pub fn never_junk_domain(&mut self, domain: &str) -> bool {
        let blocked = self.blocked.remove_domain(domain);
        self.trusted.add_domain(domain) || blocked
    }

    /// The canonical script (see the module's documentation). Control characters in names and
    /// values are written as spaces.
    pub fn render(&self, folders: &Folders) -> Result<String, Invalid> {
        let invalid = |item, reason| Invalid { item, reason };
        if !is_folder(&folders.inbox) || !is_folder(&folders.junk) {
            return Err(invalid(None, Reason::Folder));
        }
        self.trusted
            .validate()
            .and(self.blocked.validate())
            .map_err(|reason| invalid(None, reason))?;
        for (index, item) in self.items.iter().enumerate() {
            if let Item::Rule(rule) = item {
                rule.validate()
                    .map_err(|reason| invalid(Some(index), reason))?;
            }
        }
        let mut writer = Writer::new(folders);
        writer.never_junk(&self.trusted);
        if !self.blocked.is_empty() {
            writer.always_junk(&self.blocked);
        }
        let mut hand_written = false;
        for item in &self.items {
            match item {
                Item::Rule(rule) => writer.rule(rule),
                Item::HandWritten(text) => {
                    hand_written = true;
                    writer.out.push_str(text);
                    if !text.ends_with('\n') {
                        writer.out.push('\n');
                    }
                }
            }
        }
        let mut requires = writer.needs.names();
        if hand_written {
            for name in &self.requires {
                if !requires.contains(&name.as_str()) {
                    requires.push(name);
                }
            }
        }
        let mut script = String::from("require ");
        list(&mut script, &requires);
        script.push_str(";\n");
        script.push_str(&writer.out);
        Ok(script)
    }
}

/// The extensions a written script uses.
#[derive(Default)]
struct Needs {
    fileinto: bool,
    copy: bool,
    imap4flags: bool,
    environment: bool,
}

impl Needs {
    fn names(&self) -> Vec<&'static str> {
        [
            (self.fileinto, "fileinto"),
            (self.copy, "copy"),
            (self.imap4flags, "imap4flags"),
            (self.environment, "environment"),
            (self.environment, "imapsieve"),
        ]
        .into_iter()
        .filter(|(used, _)| *used)
        .map(|(_, name)| name)
        .collect()
    }
}

/// Writes rules as Sieve, in Roundcube's layout.
struct Writer<'f> {
    folders: &'f Folders,
    out: String,
    needs: Needs,
}

impl<'f> Writer<'f> {
    fn new(folders: &'f Folders) -> Writer<'f> {
        Writer {
            folders,
            out: String::new(),
            needs: Needs::default(),
        }
    }

    fn never_junk(&mut self, trusted: &Senders) {
        self.needs.environment = true;
        let _ = write!(
            self.out,
            "# {NEVER_JUNK}\nif environment :is \"imap.mailbox\" {}\n{{\n",
            quote(&self.folders.junk)
        );
        if !trusted.is_empty() {
            self.out.push_str("\tif ");
            self.out.push_str(&senders(trusted));
            self.out.push_str("\n\t{\n");
            let folders = self.folders;
            for line in self.junk_lines(false, &folders.inbox) {
                let _ = writeln!(self.out, "\t\t{line};");
            }
            self.out.push_str("\t\tstop;\n\t}\n");
        }
        self.out.push_str("\tstop;\n}\n");
    }

    fn always_junk(&mut self, blocked: &Senders) {
        let _ = write!(self.out, "# {ALWAYS_JUNK}\nif {}\n{{\n", senders(blocked));
        let folders = self.folders;
        for line in self.junk_lines(true, &folders.junk) {
            let _ = writeln!(self.out, "\t{line};");
        }
        self.out.push_str("\tstop;\n}\n");
    }

    /// The flags and the `fileinto` of a move to Junk (`junk`) or back to the Inbox.
    fn junk_lines(&mut self, junk: bool, folder: &str) -> Vec<String> {
        let mut lines = self.junk_flags(junk);
        lines.push(self.file(folder, false));
        lines
    }

    fn junk_flags(&mut self, junk: bool) -> Vec<String> {
        self.needs.imap4flags = true;
        let (remove, add) = if junk {
            ("$NotJunk", "$Junk")
        } else {
            ("$Junk", "$NotJunk")
        };
        vec![
            format!("removeflag {}", quote(remove)),
            format!("addflag {}", quote(add)),
        ]
    }

    fn file(&mut self, folder: &str, copy: bool) -> String {
        self.needs.fileinto = true;
        if copy {
            self.needs.copy = true;
            format!("fileinto :copy {}", quote(folder))
        } else {
            format!("fileinto {}", quote(folder))
        }
    }

    fn rule(&mut self, rule: &Rule) {
        let name = clean(&rule.name);
        if !name.is_empty() {
            let _ = writeln!(self.out, "# rule:[{name}]");
        }
        let test = conditions(rule.matching, &rule.conditions);
        if rule.enabled {
            let _ = write!(self.out, "if {test}\n{{\n");
        } else {
            let _ = write!(self.out, "if false # {test}\n{{\n");
        }
        for line in self.actions(&rule.actions) {
            let _ = writeln!(self.out, "\t{line};");
        }
        self.out.push_str("}\n");
    }

    fn actions(&mut self, actions: &[Action]) -> Vec<String> {
        let mut sorted: Vec<&Action> = actions.iter().collect();
        sorted.sort_by_key(|action| action.rank());
        let mut lines = Vec::new();
        for action in &sorted {
            match action {
                Action::MoveToJunk => lines.extend(self.junk_flags(true)),
                Action::NotJunk => lines.extend(self.junk_flags(false)),
                Action::MarkRead | Action::Star => {
                    self.needs.imap4flags = true;
                    let flag = if **action == Action::MarkRead {
                        "\\Seen"
                    } else {
                        "\\Flagged"
                    };
                    lines.push(format!("addflag {}", quote(flag)));
                }
                _ => {}
            }
        }
        let folders = self.folders;
        for action in &sorted {
            let line = match action {
                Action::MoveToJunk => self.file(&folders.junk, false),
                Action::NotJunk => self.file(&folders.inbox, false),
                Action::CopyTo(folder) => self.file(folder, true),
                Action::MoveTo(folder) => self.file(folder, false),
                _ => continue,
            };
            lines.push(line);
        }
        for action in &sorted {
            match action {
                Action::Delete => lines.push("discard".to_owned()),
                Action::Stop => lines.push("stop".to_owned()),
                _ => {}
            }
        }
        lines
    }
}

/// `address :is "from" […]`, `address :domain :is "from" […]`, or both in an `anyof`.
fn senders(senders: &Senders) -> String {
    let mut addresses = String::from("address :is \"from\" ");
    list(&mut addresses, &senders.addresses);
    let mut domains = String::from("address :domain :is \"from\" ");
    list(&mut domains, &senders.domains);
    match (senders.addresses.is_empty(), senders.domains.is_empty()) {
        (false, true) => addresses,
        (true, false) => domains,
        _ => format!("anyof ({addresses}, {domains})"),
    }
}

/// The test of a rule's conditions, Roundcube's way: `true` for none, one test alone for any of
/// one, otherwise `allof (…)` or `anyof (…)`.
fn conditions(matching: Match, conditions: &[Condition]) -> String {
    let tests: Vec<String> = conditions.iter().map(condition).collect();
    match (matching, tests.as_slice()) {
        (_, []) => "true".to_owned(),
        (Match::Any, [test]) => test.clone(),
        (Match::All, _) => format!("allof ({})", tests.join(", ")),
        (Match::Any, _) => format!("anyof ({})", tests.join(", ")),
    }
}

fn condition(condition: &Condition) -> String {
    let (field, operator, value) = match condition {
        Condition::SizeOver(bytes) => return format!("size :over {}", size(*bytes)),
        Condition::SizeUnder(bytes) => return format!("size :under {}", size(*bytes)),
        Condition::Field {
            field,
            operator,
            value,
        } => (field, *operator, value),
    };
    let (not, match_type, key) = match operator {
        Operator::Contains => ("", ":contains", value.clone()),
        Operator::NotContains => ("not ", ":contains", value.clone()),
        Operator::Is => ("", ":is", value.clone()),
        Operator::IsNot => ("not ", ":is", value.clone()),
        Operator::BeginsWith => ("", ":matches", format!("{}*", escape_wildcards(value))),
        Operator::EndsWith => ("", ":matches", format!("*{}", escape_wildcards(value))),
        Operator::Matches => ("", ":matches", value.clone()),
    };
    let address =
        field.holds_addresses() && !matches!(operator, Operator::Contains | Operator::NotContains);
    let test = if address { "address" } else { "header" };
    let mut text = format!("{not}{test} {match_type} ");
    list(&mut text, &field.headers());
    text.push(' ');
    text.push_str(&quote(&key));
    text
}

/// Bytes with Sieve's K, M or G when they divide evenly.
fn size(bytes: u64) -> String {
    for (unit, suffix) in [(1 << 30, "G"), (1 << 20, "M"), (1 << 10, "K")] {
        if bytes != 0 && bytes.is_multiple_of(unit) {
            return format!("{}{suffix}", bytes / unit);
        }
    }
    bytes.to_string()
}

/// `value` with `*`, `?` and `\` made literal for `:matches`.
fn escape_wildcards(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '*' | '?' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Control characters as spaces.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// A Sieve quoted string.
fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in clean(text).chars() {
        if matches!(c, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// One string alone, several in brackets, as Roundcube writes them.
fn list<S: AsRef<str>>(out: &mut String, strings: &[S]) {
    match strings {
        [one] => out.push_str(&quote(one.as_ref())),
        _ => {
            out.push('[');
            for (index, string) in strings.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&quote(string.as_ref()));
            }
            out.push(']');
        }
    }
}

impl fmt::Debug for RuleSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuleSet")
            .field("trusted", &self.trusted)
            .field("blocked", &self.blocked)
            .field("items", &self.items)
            .field("requires", &self.requires)
            .finish()
    }
}

impl fmt::Debug for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Rule(rule) => rule.fmt(f),
            Item::HandWritten(text) => write!(f, "HandWritten({} bytes)", text.len()),
        }
    }
}

impl fmt::Debug for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rule")
            .field("name", &format_args!("{} chars", self.name.chars().count()))
            .field("enabled", &self.enabled)
            .field("matching", &self.matching)
            .field("conditions", &self.conditions)
            .field("actions", &self.actions)
            .finish()
    }
}

impl fmt::Debug for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Condition::Field {
                field,
                operator,
                value,
            } => f
                .debug_struct("Field")
                .field("field", field)
                .field("operator", operator)
                .field("value", &format_args!("{} chars", value.chars().count()))
                .finish(),
            Condition::SizeOver(bytes) => write!(f, "SizeOver({bytes})"),
            Condition::SizeUnder(bytes) => write!(f, "SizeUnder({bytes})"),
        }
    }
}

impl fmt::Debug for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Field::From => "From",
            Field::To => "To",
            Field::Cc => "Cc",
            Field::ToOrCc => "ToOrCc",
            Field::Subject => "Subject",
            Field::ListId => "ListId",
            Field::ReplyTo => "ReplyTo",
            Field::Header(_) => "Header(..)",
        })
    }
}

impl fmt::Debug for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Action::MoveTo(_) => "MoveTo(..)",
            Action::CopyTo(_) => "CopyTo(..)",
            Action::MarkRead => "MarkRead",
            Action::Star => "Star",
            Action::MoveToJunk => "MoveToJunk",
            Action::NotJunk => "NotJunk",
            Action::Delete => "Delete",
            Action::Stop => "Stop",
        })
    }
}

impl fmt::Debug for Senders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Senders")
            .field("addresses", &self.addresses.len())
            .field("domains", &self.domains.len())
            .finish()
    }
}
