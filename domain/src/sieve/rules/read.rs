//! Reading a script back into a [`RuleSet`]: the sections and rules [`RuleSet::render`] writes,
//! and Roundcube's own rules where they say the same. Each is taken as a rule only when writing
//! it again gives the very same commands; everything else stays as hand-written text in its
//! place, so reading and writing again never changes what a script does.

use super::super::Error;
use super::super::check::{
    AddressPart, Command, Comparator, EXTENSIONS, MatchType, Matcher, Script, Span, Test, check,
    check_test,
};
use super::super::lexer::Comment;
use super::super::parser::{Tree, parse, parse_test};
use super::{
    ALWAYS_JUNK, Action, Condition, Field, Folders, Item, Match, NEVER_JUNK, Operator, Rule,
    RuleSet, Senders, Writer, clean,
};
use std::collections::BTreeSet;
use std::ops::Range;

/// What a rule written alone is checked with.
const EVERY_EXTENSION: &str = "require [\"fileinto\",\"copy\",\"imap4flags\",\"environment\",\
                               \"imapsieve\"];\n";

impl RuleSet {
    /// Reads an account's script. The never-junk section is recognised only first, the
    /// always-junk rule only after it (or first), each under its marker comment; a rule is an
    /// `if` with no `elsif` or `else`, perhaps under `# rule:[Name]`, holding no comment but a
    /// disabled rule's test. Anything else is an [`Item::HandWritten`]. An error when the script
    /// doesn't parse or uses Sieve outside the subset.
    pub fn read(text: &str, folders: &Folders) -> Result<RuleSet, Error> {
        let tree = parse(text)?;
        let script = check(&tree)?;
        let reader = Reader {
            text,
            tree: &tree,
            script: &script,
            folders,
        };
        Ok(reader.read())
    }
}

struct Reader<'a> {
    text: &'a str,
    tree: &'a Tree,
    script: &'a Script,
    folders: &'a Folders,
}

/// Which spam sections were read so far.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sections {
    None,
    NeverJunk,
    AlwaysJunk,
}

impl Reader<'_> {
    fn read(&self) -> RuleSet {
        let mut set = RuleSet::default();
        let mut needs = BTreeSet::new();
        let mut statements = false;
        let mut sections = Sections::None;
        // Where the hand-written command the last item ends with ends.
        let mut open: Option<usize> = None;
        let mut previous = 0;
        for (command, span) in self.script.commands.iter().zip(&self.script.spans) {
            let gap = previous..span.start;
            previous = span.end;
            let continues = open.take() == Some(gap.start);
            if let Command::Require(_) = command {
                self.comments(&mut set, gap, None);
                continue;
            }
            if !statements
                && sections == Sections::None
                && let Some((senders, marker)) = self.section(command, span, &gap, NEVER_JUNK)
            {
                self.comments(&mut set, gap, Some(marker));
                set.trusted = senders;
                sections = Sections::NeverJunk;
                continue;
            }
            if !statements
                && sections < Sections::AlwaysJunk
                && let Some((senders, marker)) = self.section(command, span, &gap, ALWAYS_JUNK)
            {
                self.comments(&mut set, gap, Some(marker));
                set.blocked = senders;
                sections = Sections::AlwaysJunk;
                continue;
            }
            statements = true;
            if let Some((rule, name)) = self.rule(command, span, &gap) {
                self.comments(&mut set, gap, name);
                set.items.push(Item::Rule(rule));
                continue;
            }
            uses(command, &mut needs);
            let text = &self.text[gap.start..span.end];
            match set.items.last_mut() {
                Some(Item::HandWritten(held)) if continues => held.push_str(text),
                _ => hand_written(&mut set, text.trim_start()),
            }
            open = Some(span.end);
        }
        let trailing = self.text[previous..].trim();
        if !trailing.is_empty() {
            hand_written(&mut set, trailing);
        }
        if set.is_hand_written() {
            let required = self.script.requires();
            set.requires = EXTENSIONS
                .iter()
                .filter(|name| {
                    needs.contains(*name)
                        || (name.starts_with("comparator-") && required.iter().any(|r| r == *name))
                })
                .map(|name| (*name).to_owned())
                .collect();
        }
        set
    }

    /// The comments between two commands, but `skip` (a marker or a rule's name), as
    /// hand-written text when anything is left.
    fn comments(&self, set: &mut RuleSet, gap: Range<usize>, skip: Option<Range<usize>>) {
        let kept = match skip {
            Some(skip) => format!(
                "{}{}",
                &self.text[gap.start..skip.start],
                &self.text[skip.end..gap.end]
            ),
            None => self.text[gap].to_owned(),
        };
        let kept = kept.trim();
        if !kept.is_empty() {
            hand_written(set, kept);
        }
    }

    /// The never-junk section or the always-junk rule under its `marker`: its senders, and where
    /// the marker is.
    fn section(
        &self,
        command: &Command,
        span: &Span,
        gap: &Range<usize>,
        marker: &str,
    ) -> Option<(Senders, Range<usize>)> {
        let found = comments_in(&self.tree.comments, gap.clone())
            .iter()
            .rfind(|comment| comment.hash && comment.text.trim() == marker)?;
        if !comments_in(&self.tree.comments, span.start..span.end).is_empty() {
            return None;
        }
        let Command::If(branches, None) = command else {
            return None;
        };
        let [(test, block)] = branches.as_slice() else {
            return None;
        };
        let never_junk = marker == NEVER_JUNK;
        let senders = if never_junk {
            match block.as_slice() {
                [Command::Stop] => Senders::default(),
                [Command::If(inner, None), Command::Stop] => match inner.as_slice() {
                    [(test, _)] => senders_of(test)?,
                    _ => return None,
                },
                _ => return None,
            }
        } else {
            senders_of(test)?
        };
        senders.validate().ok()?;
        let mut writer = Writer::new(self.folders);
        if never_junk {
            writer.never_junk(&senders);
        } else {
            writer.always_junk(&senders);
        }
        let (_, written) = written(&writer.out)?;
        (written.commands.get(1) == Some(command)).then_some((senders, found.start..found.end))
    }

    /// A rule, and where its `# rule:[Name]` comment is.
    fn rule(
        &self,
        command: &Command,
        span: &Span,
        gap: &Range<usize>,
    ) -> Option<(Rule, Option<Range<usize>>)> {
        let Command::If(branches, None) = command else {
            return None;
        };
        let [(test, block)] = branches.as_slice() else {
            return None;
        };
        let inside = comments_in(&self.tree.comments, span.start..span.end).len();
        let (enabled, test) = match (test, inside) {
            (Test::False, 1) => (
                false,
                disabled_test(&self.tree.comments, span, self.script.requires())?,
            ),
            (test, 0) => (true, test.clone()),
            _ => return None,
        };
        let (matching, conditions) = conditions_of(&test)?;
        let mut actions = actions_of(block, self.folders)?;
        actions.sort_by_key(Action::rank);
        let name = comments_in(&self.tree.comments, gap.clone())
            .iter()
            .filter(|comment| comment.hash)
            .filter_map(|comment| Some((rule_name(&comment.text)?, comment.start..comment.end)))
            .next_back();
        let rule = Rule {
            name: name
                .as_ref()
                .map(|(name, _)| name.clone())
                .unwrap_or_default(),
            enabled,
            matching,
            conditions,
            actions,
        };
        rule.validate().ok()?;
        let mut writer = Writer::new(self.folders);
        writer.rule(&rule);
        let (tree, written) = written(&writer.out)?;
        if written.commands.get(1) != Some(command) {
            return None;
        }
        if !enabled && disabled_test(&tree.comments, &written.spans[1], written.requires())? != test
        {
            return None;
        }
        Some((rule, name.map(|(_, at)| at)))
    }
}

/// The extensions a hand-written command uses.
fn uses(command: &Command, needs: &mut BTreeSet<&'static str>) {
    match command {
        Command::If(branches, otherwise) => {
            for (test, block) in branches {
                test_uses(test, needs);
                for command in block {
                    uses(command, needs);
                }
            }
            for command in otherwise.iter().flatten() {
                uses(command, needs);
            }
        }
        Command::FileInto { copy, .. } => {
            needs.insert("fileinto");
            if *copy {
                needs.insert("copy");
            }
        }
        Command::AddFlag(_) | Command::SetFlag(_) | Command::RemoveFlag(_) => {
            needs.insert("imap4flags");
        }
        _ => {}
    }
}

fn test_uses(test: &Test, needs: &mut BTreeSet<&'static str>) {
    match test {
        Test::Not(test) => test_uses(test, needs),
        Test::AllOf(tests) | Test::AnyOf(tests) => {
            for test in tests {
                test_uses(test, needs);
            }
        }
        Test::HasFlag { .. } => {
            needs.insert("imap4flags");
        }
        Test::Environment { .. } => {
            needs.extend(["environment", "imapsieve"]);
        }
        _ => {}
    }
}

/// Adds hand-written text: to the last item when it is hand-written too (a line between), so
/// that hand-written text read back after writing makes the same items.
fn hand_written(set: &mut RuleSet, text: &str) {
    match set.items.last_mut() {
        Some(Item::HandWritten(held)) => {
            held.push('\n');
            held.push_str(text);
        }
        _ => set.items.push(Item::HandWritten(text.to_owned())),
    }
}

/// A rule or section written alone, parsed and checked with every extension required.
fn written(text: &str) -> Option<(Tree, Script)> {
    let tree = parse(&format!("{EVERY_EXTENSION}{text}")).ok()?;
    let script = check(&tree).ok()?;
    Some((tree, script))
}

/// The comments that start within `range`.
fn comments_in(comments: &[Comment], range: Range<usize>) -> &[Comment] {
    let first = comments.partition_point(|comment| comment.start < range.start);
    let last = comments.partition_point(|comment| comment.start < range.end);
    &comments[first..last.max(first)]
}

/// `rule:[Name]`, Roundcube's comment above a rule.
fn rule_name(comment: &str) -> Option<String> {
    let name = comment.trim().strip_prefix("rule:[")?.strip_suffix(']')?;
    Some(clean(name))
}

/// The test in a disabled rule's comment, `if false # <test>`: the only comment inside the
/// `if`, on its first line.
fn disabled_test(comments: &[Comment], span: &Span, requires: &[String]) -> Option<Test> {
    let [comment] = comments_in(comments, span.start..span.end) else {
        return None;
    };
    if !comment.hash || comment.line != span.line {
        return None;
    }
    check_test(&parse_test(&comment.text).ok()?, requires).ok()
}

/// `address :is "from" […]`, `address :domain :is "from" […]`, or both in an `anyof`.
fn senders_of(test: &Test) -> Option<Senders> {
    let from = |headers: &[String], matcher: &Matcher| {
        headers.len() == 1 && headers[0] == "from" && *matcher == Matcher::DEFAULT
    };
    match test {
        Test::Address {
            part: AddressPart::All,
            matcher,
            headers,
            keys,
        } if from(headers, matcher) => Some(Senders {
            addresses: keys.clone(),
            domains: Vec::new(),
        }),
        Test::Address {
            part: AddressPart::Domain,
            matcher,
            headers,
            keys,
        } if from(headers, matcher) => Some(Senders {
            addresses: Vec::new(),
            domains: keys.clone(),
        }),
        Test::AnyOf(tests) => match tests.as_slice() {
            [addresses, domains] => {
                let addresses = senders_of(addresses)?;
                let domains = senders_of(domains)?;
                (addresses.domains.is_empty() && domains.addresses.is_empty()).then_some(Senders {
                    addresses: addresses.addresses,
                    domains: domains.domains,
                })
            }
            _ => None,
        },
        _ => None,
    }
}

fn conditions_of(test: &Test) -> Option<(Match, Vec<Condition>)> {
    let all = |tests: &[Test]| tests.iter().map(condition_of).collect::<Option<Vec<_>>>();
    match test {
        Test::True => Some((Match::All, Vec::new())),
        Test::AllOf(tests) => Some((Match::All, all(tests)?)),
        Test::AnyOf(tests) => Some((Match::Any, all(tests)?)),
        test => Some((Match::Any, vec![condition_of(test)?])),
    }
}

fn condition_of(test: &Test) -> Option<Condition> {
    match test {
        Test::Not(inner) => match condition_of(inner)? {
            Condition::Field {
                field,
                operator,
                value,
            } => {
                let operator = match operator {
                    Operator::Contains => Operator::NotContains,
                    Operator::Is => Operator::IsNot,
                    _ => return None,
                };
                Some(Condition::Field {
                    field,
                    operator,
                    value,
                })
            }
            _ => None,
        },
        Test::Header {
            matcher,
            headers,
            keys,
        }
        | Test::Address {
            part: AddressPart::All,
            matcher,
            headers,
            keys,
        } if matcher.comparator == Comparator::AsciiCasemap => {
            let [key] = keys.as_slice() else {
                return None;
            };
            let (operator, value) = operator_of(matcher.kind, key);
            Some(Condition::Field {
                field: field_of(headers)?,
                operator,
                value,
            })
        }
        Test::Size { over: true, limit } => Some(Condition::SizeOver(*limit)),
        Test::Size { over: false, limit } => Some(Condition::SizeUnder(*limit)),
        _ => None,
    }
}

fn field_of(headers: &[String]) -> Option<Field> {
    match headers {
        [header] => Some(match header.as_str() {
            "from" => Field::From,
            "to" => Field::To,
            "cc" => Field::Cc,
            "subject" => Field::Subject,
            "list-id" => Field::ListId,
            "reply-to" => Field::ReplyTo,
            other => Field::Header(other.to_owned()),
        }),
        [to, cc] if to == "to" && cc == "cc" => Some(Field::ToOrCc),
        _ => None,
    }
}

/// `:matches "abc*"` is "begins with abc" and `:matches "*abc"` "ends with abc".
fn operator_of(kind: MatchType, key: &str) -> (Operator, String) {
    match kind {
        MatchType::Is => (Operator::Is, key.to_owned()),
        MatchType::Contains => (Operator::Contains, key.to_owned()),
        MatchType::Matches => {
            if let Some(value) = key.strip_suffix('*').and_then(literal)
                && !value.is_empty()
            {
                (Operator::BeginsWith, value)
            } else if let Some(value) = key.strip_prefix('*').and_then(literal)
                && !value.is_empty()
            {
                (Operator::EndsWith, value)
            } else {
                (Operator::Matches, key.to_owned())
            }
        }
    }
}

/// A `:matches` pattern without wildcards, as the text it matches.
fn literal(pattern: &str) -> Option<String> {
    let mut value = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '*' | '?' => return None,
            '\\' => value.push(chars.next()?),
            c => value.push(c),
        }
    }
    Some(value)
}

/// A rule's block as actions: flags, `fileinto`s, `discard` and `stop`, the junk moves as the
/// writer writes them.
fn actions_of(block: &[Command], folders: &Folders) -> Option<Vec<Action>> {
    let one = |flags: &[String], name: &str| flags.len() == 1 && flags[0] == name;
    let mut actions = Vec::new();
    let (mut junk, mut not_junk) = (0_usize, 0_usize);
    let mut commands = block.iter();
    while let Some(command) = commands.next() {
        match command {
            Command::RemoveFlag(flags) if one(flags, "$NotJunk") => match commands.next() {
                Some(Command::AddFlag(flags)) if one(flags, "$Junk") => junk += 1,
                _ => return None,
            },
            Command::RemoveFlag(flags) if one(flags, "$Junk") => match commands.next() {
                Some(Command::AddFlag(flags)) if one(flags, "$NotJunk") => not_junk += 1,
                _ => return None,
            },
            Command::AddFlag(flags) if one(flags, "\\Seen") => actions.push(Action::MarkRead),
            Command::AddFlag(flags) if one(flags, "\\Flagged") => actions.push(Action::Star),
            Command::FileInto {
                folder,
                copy: false,
            } if junk > 0 && *folder == folders.junk => {
                junk -= 1;
                actions.push(Action::MoveToJunk);
            }
            Command::FileInto {
                folder,
                copy: false,
            } if not_junk > 0 && *folder == folders.inbox => {
                not_junk -= 1;
                actions.push(Action::NotJunk);
            }
            Command::FileInto { folder, copy: true } => {
                actions.push(Action::CopyTo(folder.clone()))
            }
            Command::FileInto {
                folder,
                copy: false,
            } => {
                actions.push(Action::MoveTo(folder.clone()));
            }
            Command::Discard => actions.push(Action::Delete),
            Command::Stop => actions.push(Action::Stop),
            _ => return None,
        }
    }
    (junk == 0 && not_junk == 0).then_some(actions)
}
