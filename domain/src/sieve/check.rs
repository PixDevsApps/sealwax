//! The subset of Sieve Sealwax runs (docs/m9/sieve.md): [`check`] turns a parsed [`Tree`] into a
//! [`Script`] of typed commands and tests, or names the first thing outside the subset.

use super::parser::{self, Argument, Tests, Tree, Value};
use super::{Error, Position};
use std::collections::BTreeSet;
use std::fmt;
use std::iter::Peekable;
use std::slice;

/// The names a script may `require`. The two comparators are always available (RFC 5228
/// section 2.7.3); requiring them is allowed.
pub const EXTENSIONS: [&str; 7] = [
    "fileinto",
    "copy",
    "imap4flags",
    "environment",
    "imapsieve",
    "comparator-i;octet",
    "comparator-i;ascii-casemap",
];

/// Headers the `address` test reads: those that hold addresses (RFC 5228 section 5.1).
const ADDRESS_HEADERS: [&str; 22] = [
    "from",
    "sender",
    "reply-to",
    "to",
    "cc",
    "bcc",
    "resent-from",
    "resent-sender",
    "resent-reply-to",
    "resent-to",
    "resent-cc",
    "resent-bcc",
    "return-path",
    "delivered-to",
    "envelope-to",
    "x-original-to",
    "x-envelope-to",
    "errors-to",
    "mail-followup-to",
    "mail-reply-to",
    "disposition-notification-to",
    "return-receipt-to",
];

/// A script Sealwax can run: its commands checked and typed.
#[derive(Clone, PartialEq, Eq)]
pub struct Script {
    pub(super) commands: Vec<Command>,
    /// Where each top-level command is in the text (an `if` with its `elsif`s and `else`).
    pub(super) spans: Vec<Span>,
    requires: Vec<String>,
}

impl Script {
    /// The extensions the script requires, in its order.
    pub fn requires(&self) -> &[String] {
        &self.requires
    }

    /// The headers its tests read (`header`, `address` and `exists`), in lower case, sorted: the
    /// engine fetches a message's full header when one isn't among those it stores.
    pub fn headers(&self) -> Vec<String> {
        fn walk<'s>(commands: &'s [Command], names: &mut BTreeSet<&'s str>) {
            for command in commands {
                if let Command::If(branches, otherwise) = command {
                    for (test, block) in branches {
                        test.headers(names);
                        walk(block, names);
                    }
                    walk(otherwise.as_deref().unwrap_or_default(), names);
                }
            }
        }
        let mut names = BTreeSet::new();
        walk(&self.commands, &mut names);
        names.into_iter().map(str::to_owned).collect()
    }

    /// Whether it does nothing but `require`: every message is kept.
    pub fn is_empty(&self) -> bool {
        self.commands
            .iter()
            .all(|command| matches!(command, Command::Require(_)))
    }
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Script")
            .field("commands", &self.commands.len())
            .field("requires", &self.requires)
            .finish()
    }
}

/// A top-level command's place in the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Require(Vec<String>),
    /// `if` and its `elsif`s, each a test and a block; then the `else` block.
    If(Vec<(Test, Vec<Command>)>, Option<Vec<Command>>),
    Keep,
    Discard,
    Stop,
    FileInto {
        folder: String,
        copy: bool,
    },
    /// Flags split at spaces, system flags and the junk keywords spelled as RFC 5232 does.
    AddFlag(Vec<String>),
    SetFlag(Vec<String>),
    RemoveFlag(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Test {
    True,
    False,
    Not(Box<Test>),
    AllOf(Vec<Test>),
    AnyOf(Vec<Test>),
    /// Header names in lower case.
    Address {
        part: AddressPart,
        matcher: Matcher,
        headers: Vec<String>,
        keys: Vec<String>,
    },
    Header {
        matcher: Matcher,
        headers: Vec<String>,
        keys: Vec<String>,
    },
    Exists(Vec<String>),
    Size {
        over: bool,
        limit: u64,
    },
    /// Keys split at spaces.
    HasFlag {
        matcher: Matcher,
        keys: Vec<String>,
    },
    /// The item's name in lower case.
    Environment {
        matcher: Matcher,
        name: String,
        keys: Vec<String>,
    },
}

impl Test {
    fn headers<'s>(&'s self, names: &mut BTreeSet<&'s str>) {
        match self {
            Test::Not(test) => test.headers(names),
            Test::AllOf(tests) | Test::AnyOf(tests) => {
                for test in tests {
                    test.headers(names);
                }
            }
            Test::Address { headers, .. }
            | Test::Header { headers, .. }
            | Test::Exists(headers) => {
                names.extend(headers.iter().map(String::as_str));
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Matcher {
    pub(super) kind: MatchType,
    pub(super) comparator: Comparator,
}

impl Matcher {
    pub(super) const DEFAULT: Matcher = Matcher {
        kind: MatchType::Is,
        comparator: Comparator::AsciiCasemap,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MatchType {
    Is,
    Contains,
    Matches,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Comparator {
    Octet,
    AsciiCasemap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AddressPart {
    All,
    LocalPart,
    Domain,
}

/// Checks a parsed script against the subset Sealwax runs: `require` names only [`EXTENSIONS`]
/// and comes first; every command, test and tag is one Sealwax knows, with the arguments it
/// takes, and its extension is required. The error names what is outside the subset.
pub fn check(tree: &Tree) -> Result<Script, Error> {
    let mut checker = Checker {
        requires: Vec::new(),
    };
    let mut commands = Vec::new();
    let mut spans = Vec::new();
    let mut rest = tree.commands.iter().peekable();
    let mut leading = true;
    while let Some(command) = rest.next() {
        let start = command.position;
        leading &= command.name == "require";
        let (typed, end) = if leading {
            (Command::Require(checker.require(command)?), command.end)
        } else {
            checker.statement(command, &mut rest)?
        };
        commands.push(typed);
        spans.push(Span {
            start: start.offset,
            end,
            line: start.line,
        });
    }
    Ok(Script {
        commands,
        spans,
        requires: checker.requires,
    })
}

/// One test alone, with the extensions `requires` names (a disabled rule's comment).
pub(super) fn check_test(test: &parser::Test, requires: &[String]) -> Result<Test, Error> {
    let checker = Checker {
        requires: requires.to_vec(),
    };
    checker.test(test)
}

struct Checker {
    requires: Vec<String>,
}

type Rest<'t> = Peekable<slice::Iter<'t, parser::Command>>;

/// Tags a command or test takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Takes {
    Matching,
    AddressPart,
    Size,
    Copy,
}

/// The tagged arguments read, and the positional ones left in order.
#[derive(Default)]
struct Arguments<'t> {
    match_type: Option<MatchType>,
    comparator: Option<Comparator>,
    part: Option<AddressPart>,
    /// `:over` (`true`) or `:under`.
    over: Option<bool>,
    copy: bool,
    positional: Vec<&'t Argument>,
}

impl Arguments<'_> {
    fn matcher(&self) -> Matcher {
        Matcher {
            kind: self.match_type.unwrap_or(MatchType::Is),
            comparator: self.comparator.unwrap_or(Comparator::AsciiCasemap),
        }
    }
}

impl Checker {
    /// `require`'s names, added to those required so far.
    fn require(&mut self, command: &parser::Command) -> Result<Vec<String>, Error> {
        no_block(command)?;
        no_test_after(command)?;
        let [argument] = command.arguments.as_slice() else {
            return Err(Error::at(
                command.position,
                "`require` takes one string or list of extension names",
            ));
        };
        let names = strings(argument)?;
        for name in names {
            if !EXTENSIONS.contains(&name.as_str()) {
                return Err(Error::at(argument.position, unsupported_extension(name)));
            }
            if !self.requires.contains(name) {
                self.requires.push(name.clone());
            }
        }
        Ok(names.clone())
    }

    fn need(&self, extension: &str, position: Position, what: &str) -> Result<(), Error> {
        if self.requires.iter().any(|name| name == extension) {
            Ok(())
        } else {
            Err(Error::at(
                position,
                format!("{what} needs `require \"{extension}\";` at the start of the script"),
            ))
        }
    }

    /// A command, with the `elsif`s and `else` that follow an `if`; and where it ends.
    fn statement(
        &self,
        command: &parser::Command,
        rest: &mut Rest<'_>,
    ) -> Result<(Command, usize), Error> {
        match command.name.as_str() {
            "if" => {
                let mut branches = vec![self.branch(command)?];
                let mut otherwise = None;
                let mut end = command.end;
                while let Some(&next) = rest.peek() {
                    match next.name.as_str() {
                        "elsif" => branches.push(self.branch(next)?),
                        "else" => {
                            no_arguments(next)?;
                            no_test_after(next)?;
                            otherwise = Some(self.block(next)?);
                        }
                        _ => break,
                    }
                    end = next.end;
                    let last = next.name == "else";
                    rest.next();
                    if last {
                        break;
                    }
                }
                Ok((Command::If(branches, otherwise), end))
            }
            "elsif" | "else" => Err(Error::at(
                command.position,
                format!("`{}` without an `if` before it", command.name),
            )),
            "require" => Err(Error::at(
                command.position,
                "`require` must come before every other command",
            )),
            _ => Ok((self.action(command)?, command.end)),
        }
    }

    fn commands(&self, commands: &[parser::Command]) -> Result<Vec<Command>, Error> {
        let mut typed = Vec::new();
        let mut rest = commands.iter().peekable();
        while let Some(command) = rest.next() {
            typed.push(self.statement(command, &mut rest)?.0);
        }
        Ok(typed)
    }

    fn block(&self, command: &parser::Command) -> Result<Vec<Command>, Error> {
        match &command.block {
            Some(block) => self.commands(block),
            None => Err(Error::at(
                command.position,
                format!("`{}` needs a block in `{{ }}`", command.name),
            )),
        }
    }

    /// An `if`'s or `elsif`'s test and block.
    fn branch(&self, command: &parser::Command) -> Result<(Test, Vec<Command>), Error> {
        no_arguments(command)?;
        let test = match &command.tests {
            None => {
                return Err(Error::at(
                    command.position,
                    format!("`{}` needs a test", command.name),
                ));
            }
            Some(tests) if tests.list => {
                return Err(Error::at(
                    tests.position,
                    format!(
                        "`{}` takes one test; use allof or anyof for several",
                        command.name
                    ),
                ));
            }
            Some(tests) => self.test(&tests.tests[0])?,
        };
        Ok((test, self.block(command)?))
    }

    fn action(&self, command: &parser::Command) -> Result<Command, Error> {
        let name = command.name.as_str();
        match name {
            "keep" | "discard" | "stop" | "fileinto" | "addflag" | "setflag" | "removeflag" => {}
            _ => return Err(Error::at(command.position, unsupported_command(name))),
        }
        no_block(command)?;
        no_test_after(command)?;
        match name {
            "keep" | "discard" | "stop" => {
                no_arguments(command)?;
                Ok(match name {
                    "keep" => Command::Keep,
                    "discard" => Command::Discard,
                    _ => Command::Stop,
                })
            }
            "fileinto" => {
                self.need("fileinto", command.position, "`fileinto`")?;
                let arguments = self.arguments(name, &command.arguments, &[Takes::Copy])?;
                let [folder] = arguments.positional.as_slice() else {
                    return Err(Error::at(
                        command.position,
                        "`fileinto` takes one folder name",
                    ));
                };
                let folder_name = string(folder)?;
                if folder_name.is_empty() || folder_name.chars().any(char::is_control) {
                    return Err(Error::at(
                        folder.position,
                        "a folder name can't be empty or hold control characters",
                    ));
                }
                Ok(Command::FileInto {
                    folder: folder_name.clone(),
                    copy: arguments.copy,
                })
            }
            _ => {
                self.need("imap4flags", command.position, &format!("`{name}`"))?;
                let arguments = self.arguments(name, &command.arguments, &[])?;
                let flags = match arguments.positional.as_slice() {
                    [flags] => strings(flags)?,
                    [_, _] => {
                        return Err(Error::at(
                            command.position,
                            format!(
                                "`{name}` with a variable name needs the variables extension, \
                                 which Sealwax doesn't support"
                            ),
                        ));
                    }
                    _ => {
                        return Err(Error::at(
                            command.position,
                            format!("`{name}` takes one string or list of flags"),
                        ));
                    }
                };
                let position = arguments.positional[0].position;
                let flags = set_flags(flags, name != "removeflag", position)?;
                Ok(match name {
                    "addflag" => Command::AddFlag(flags),
                    "setflag" => Command::SetFlag(flags),
                    _ => Command::RemoveFlag(flags),
                })
            }
        }
    }

    fn test(&self, test: &parser::Test) -> Result<Test, Error> {
        let name = test.name.as_str();
        match name {
            "true" | "false" => {
                if let Some(argument) = self.leaf(test, &[])?.positional.first() {
                    return Err(Error::at(
                        argument.position,
                        format!("`{name}` takes no arguments"),
                    ));
                }
                Ok(if name == "true" {
                    Test::True
                } else {
                    Test::False
                })
            }
            "not" => {
                if let Some(argument) = test.arguments.first() {
                    return Err(Error::at(argument.position, "`not` takes only a test"));
                }
                match &test.tests {
                    Some(tests) if !tests.list => {
                        Ok(Test::Not(Box::new(self.test(&tests.tests[0])?)))
                    }
                    Some(tests) => Err(Error::at(
                        tests.position,
                        "`not` takes one test, not a list; use allof or anyof inside it",
                    )),
                    None => Err(Error::at(test.position, "`not` needs a test")),
                }
            }
            "allof" | "anyof" => {
                if let Some(argument) = test.arguments.first() {
                    return Err(Error::at(
                        argument.position,
                        format!("`{name}` takes only a list of tests"),
                    ));
                }
                let tests = match &test.tests {
                    Some(tests) if tests.list => &tests.tests,
                    _ => {
                        return Err(Error::at(
                            test.position,
                            format!("`{name}` needs a list of tests in `( )`"),
                        ));
                    }
                };
                let tests = tests
                    .iter()
                    .map(|test| self.test(test))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(if name == "allof" {
                    Test::AllOf(tests)
                } else {
                    Test::AnyOf(tests)
                })
            }
            "address" => {
                let arguments = self.leaf(test, &[Takes::Matching, Takes::AddressPart])?;
                let [headers, keys] = two(test, &arguments, "header names and keys")?;
                let headers = header_names(headers)?;
                if let Some(other) = headers
                    .iter()
                    .find(|header| !ADDRESS_HEADERS.contains(&header.as_str()))
                {
                    let shown = if other.len() <= 40 {
                        other.as_str()
                    } else {
                        "that header"
                    };
                    return Err(Error::at(
                        arguments.positional[0].position,
                        format!(
                            "`address` reads only headers that hold addresses, such as From, To \
                             and Cc, not {shown}; use `header`"
                        ),
                    ));
                }
                Ok(Test::Address {
                    part: arguments.part.unwrap_or(AddressPart::All),
                    matcher: arguments.matcher(),
                    headers,
                    keys: strings(keys)?.clone(),
                })
            }
            "header" => {
                let arguments = self.leaf(test, &[Takes::Matching])?;
                let [headers, keys] = two(test, &arguments, "header names and keys")?;
                Ok(Test::Header {
                    matcher: arguments.matcher(),
                    headers: header_names(headers)?,
                    keys: strings(keys)?.clone(),
                })
            }
            "exists" => {
                let arguments = self.leaf(test, &[])?;
                let [headers] = arguments.positional.as_slice() else {
                    return Err(Error::at(
                        test.position,
                        "`exists` takes a list of header names",
                    ));
                };
                Ok(Test::Exists(header_names(headers)?))
            }
            "size" => {
                let arguments = self.leaf(test, &[Takes::Size])?;
                let (Some(over), [limit]) = (arguments.over, arguments.positional.as_slice())
                else {
                    return Err(Error::at(
                        test.position,
                        "`size` takes `:over` or `:under` and a number",
                    ));
                };
                let Value::Number(limit) = limit.value else {
                    return Err(Error::at(limit.position, "a number is expected here"));
                };
                Ok(Test::Size { over, limit })
            }
            "hasflag" => {
                self.need("imap4flags", test.position, "`hasflag`")?;
                let arguments = self.leaf(test, &[Takes::Matching])?;
                match arguments.positional.as_slice() {
                    [keys] => Ok(Test::HasFlag {
                        matcher: arguments.matcher(),
                        keys: split_flags(strings(keys)?),
                    }),
                    [_, _] => Err(Error::at(
                        test.position,
                        "`hasflag` with a variable name needs the variables extension, which \
                         Sealwax doesn't support",
                    )),
                    _ => Err(Error::at(
                        test.position,
                        "`hasflag` takes one string or list of flags",
                    )),
                }
            }
            "environment" => {
                self.need("environment", test.position, "`environment`")?;
                let arguments = self.leaf(test, &[Takes::Matching])?;
                let [item, keys] = two(test, &arguments, "an item's name and keys")?;
                Ok(Test::Environment {
                    matcher: arguments.matcher(),
                    name: string(item)?.to_ascii_lowercase(),
                    keys: strings(keys)?.clone(),
                })
            }
            _ => Err(Error::at(test.position, unsupported_test(name))),
        }
    }

    /// A test that takes no test of its own: its arguments.
    fn leaf<'t>(&self, test: &'t parser::Test, takes: &[Takes]) -> Result<Arguments<'t>, Error> {
        no_tests(&test.name, test.tests.as_ref())?;
        self.arguments(&test.name, &test.arguments, takes)
    }

    fn arguments<'t>(
        &self,
        name: &str,
        arguments: &'t [Argument],
        takes: &[Takes],
    ) -> Result<Arguments<'t>, Error> {
        let mut read = Arguments::default();
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let Value::Tag(tag) = &argument.value else {
                read.positional.push(argument);
                continue;
            };
            let position = argument.position;
            let takes = |what: Takes| takes.contains(&what);
            let twice = || Error::at(position, format!("`{name}` has `:{tag}` or its like twice"));
            match tag.as_str() {
                "is" | "contains" | "matches" if takes(Takes::Matching) => {
                    let kind = match tag.as_str() {
                        "is" => MatchType::Is,
                        "contains" => MatchType::Contains,
                        _ => MatchType::Matches,
                    };
                    if read.match_type.replace(kind).is_some() {
                        return Err(twice());
                    }
                }
                "comparator" if takes(Takes::Matching) => {
                    let Some(value) = rest.next() else {
                        return Err(Error::at(
                            position,
                            "`:comparator` needs a comparator's name",
                        ));
                    };
                    let comparator = match string(value)?.to_ascii_lowercase().as_str() {
                        "i;octet" => Comparator::Octet,
                        "i;ascii-casemap" => Comparator::AsciiCasemap,
                        other => {
                            return Err(Error::at(
                                value.position,
                                format!(
                                    "Sealwax supports the comparators \"i;ascii-casemap\" and \
                                     \"i;octet\", not {}",
                                    shown_name(other, "that one")
                                ),
                            ));
                        }
                    };
                    if read.comparator.replace(comparator).is_some() {
                        return Err(twice());
                    }
                }
                "all" | "localpart" | "domain" if takes(Takes::AddressPart) => {
                    let part = match tag.as_str() {
                        "all" => AddressPart::All,
                        "localpart" => AddressPart::LocalPart,
                        _ => AddressPart::Domain,
                    };
                    if read.part.replace(part).is_some() {
                        return Err(twice());
                    }
                }
                "over" | "under" if takes(Takes::Size) => {
                    if read.over.replace(tag == "over").is_some() {
                        return Err(twice());
                    }
                }
                "copy" if takes(Takes::Copy) => {
                    self.need("copy", position, "`:copy`")?;
                    read.copy = true;
                }
                _ => return Err(Error::at(position, unsupported_tag(name, tag))),
            }
        }
        Ok(read)
    }
}

fn no_block(command: &parser::Command) -> Result<(), Error> {
    match command.block {
        Some(_) => Err(Error::at(
            command.position,
            format!("`{}` ends with `;`, not a block", command.name),
        )),
        None => Ok(()),
    }
}

/// A command that takes no test: one after its arguments is most likely the next command
/// without a `;` before it, or `else if`.
fn no_test_after(command: &parser::Command) -> Result<(), Error> {
    match &command.tests {
        Some(tests) if command.name == "else" => Err(Error::at(
            tests.position,
            "`else` takes no test; use `elsif` for another test",
        )),
        Some(tests) => Err(Error::at(
            tests.position,
            format!(
                "`{}` takes no test; a `;` may be missing before this",
                command.name
            ),
        )),
        None => Ok(()),
    }
}

fn no_tests(name: &str, tests: Option<&Tests>) -> Result<(), Error> {
    match tests {
        Some(tests) => Err(Error::at(tests.position, format!("`{name}` takes no test"))),
        None => Ok(()),
    }
}

fn no_arguments(command: &parser::Command) -> Result<(), Error> {
    match command.arguments.first() {
        Some(argument) => Err(Error::at(
            argument.position,
            format!("`{}` takes no arguments", command.name),
        )),
        None => Ok(()),
    }
}

/// The two positional arguments a test takes.
fn two<'t>(
    test: &parser::Test,
    arguments: &Arguments<'t>,
    what: &str,
) -> Result<[&'t Argument; 2], Error> {
    match arguments.positional.as_slice() {
        [first, second] => Ok([first, second]),
        _ => Err(Error::at(
            test.position,
            format!("`{}` takes two arguments: {what}", test.name),
        )),
    }
}

/// A single string, not a list.
fn string(argument: &Argument) -> Result<&String, Error> {
    match &argument.value {
        Value::Strings {
            list: false,
            strings,
        } => Ok(&strings[0]),
        Value::Strings { list: true, .. } => Err(Error::at(
            argument.position,
            "one string is expected here, not a list",
        )),
        _ => Err(Error::at(argument.position, "a string is expected here")),
    }
}

/// A string or a list of them.
fn strings(argument: &Argument) -> Result<&Vec<String>, Error> {
    match &argument.value {
        Value::Strings { strings, .. } => Ok(strings),
        _ => Err(Error::at(
            argument.position,
            "a string or a list of strings is expected here",
        )),
    }
}

/// Header names, in lower case; each must be a valid field name (RFC 5322: printable ASCII
/// without a colon).
fn header_names(argument: &Argument) -> Result<Vec<String>, Error> {
    strings(argument)?
        .iter()
        .map(|name| {
            if is_header_name(name) {
                Ok(name.to_ascii_lowercase())
            } else {
                Err(Error::at(
                    argument.position,
                    "a header name can't be empty or hold spaces, colons or control characters",
                ))
            }
        })
        .collect()
}

pub(super) fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| (33..=126).contains(&byte) && byte != b':')
}

fn split_flags(strings: &[String]) -> Vec<String> {
    strings
        .iter()
        .flat_map(|string| string.split(' '))
        .filter(|flag| !flag.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The flags of `addflag`, `setflag` or `removeflag`, split at spaces and spelled as RFC 5232
/// spells them; each must be a system flag Sealwax sets or an IMAP keyword. `setting`: `\Deleted`
/// is refused (rules never delete; `discard` moves to Trash).
fn set_flags(strings: &[String], setting: bool, position: Position) -> Result<Vec<String>, Error> {
    split_flags(strings)
        .into_iter()
        .map(|flag| {
            if let Some(system) = flag.strip_prefix('\\') {
                return match system.to_ascii_lowercase().as_str() {
                    "seen" => Ok("\\Seen".to_owned()),
                    "answered" => Ok("\\Answered".to_owned()),
                    "flagged" => Ok("\\Flagged".to_owned()),
                    "draft" => Ok("\\Draft".to_owned()),
                    "deleted" if !setting => Ok("\\Deleted".to_owned()),
                    "deleted" => Err(Error::at(
                        position,
                        "Sealwax's rules never set \\Deleted; use `discard`, which moves the \
                         message to Trash",
                    )),
                    _ => Err(Error::at(
                        position,
                        "a system flag other than \\Seen, \\Answered, \\Flagged or \\Draft",
                    )),
                };
            }
            if !is_keyword(&flag) {
                return Err(Error::at(
                    position,
                    "a flag must be an IMAP keyword: letters, digits and punctuation, without \
                     spaces, brackets, quotes, `%`, `*` or backslashes",
                ));
            }
            Ok(match flag.to_ascii_lowercase().as_str() {
                "$junk" => "$Junk".to_owned(),
                "$notjunk" => "$NotJunk".to_owned(),
                "$forwarded" => "$Forwarded".to_owned(),
                _ => flag,
            })
        })
        .collect()
}

/// An IMAP atom (RFC 3501's `flag-keyword`).
fn is_keyword(flag: &str) -> bool {
    flag.bytes()
        .all(|byte| (33..=126).contains(&byte) && !b"(){%*\"\\]".contains(&byte))
}

/// `name` quoted when it looks like an extension's or a comparator's name; otherwise `instead`
/// (an error message never quotes arbitrary text from a script).
fn shown_name(name: &str, instead: &str) -> String {
    let plain = !name.is_empty()
        && name.len() <= 40
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b";:._+-".contains(&byte));
    if plain {
        format!("\"{name}\"")
    } else {
        instead.to_owned()
    }
}

fn unsupported_extension(name: &str) -> String {
    let shown = shown_name(name, "");
    if shown.is_empty() {
        return "an extension Sealwax doesn't know".to_owned();
    }
    match name {
        "vacation" | "vacation-seconds" | "reject" | "ereject" | "enotify" | "notify"
        | "redirect" => format!(
            "Sealwax doesn't support the Sieve extension {shown}: its rules never send mail"
        ),
        _ => format!("Sealwax doesn't support the Sieve extension {shown}"),
    }
}

fn unsupported_command(name: &str) -> String {
    let extension = match name {
        "redirect" | "reject" | "ereject" | "vacation" | "notify" | "denotify" => {
            return format!("Sealwax doesn't support `{name}`: its rules never send mail");
        }
        "set" => "variables",
        "include" | "return" | "global" => "include",
        "addheader" | "deleteheader" => "editheader",
        "foreverypart" | "break" | "replace" | "enclose" | "extracttext" => "mime",
        "convert" => "convert",
        "error" => "ihave",
        "fileinto" | "keep" | "discard" | "stop" => unreachable!("supported"),
        _ => return format!("unknown command `{name}`"),
    };
    format!("Sealwax doesn't support `{name}` (the {extension} extension)")
}

fn unsupported_test(name: &str) -> String {
    let extension = match name {
        "envelope" => "envelope",
        "body" => "body",
        "date" | "currentdate" => "date",
        "spamtest" | "spamtestplus" | "virustest" => "spamtest",
        "string" => "variables",
        "mailboxexists" => "mailbox",
        "metadata" | "metadataexists" | "servermetadata" | "servermetadataexists" => "mboxmetadata",
        "ihave" => "ihave",
        "duplicate" => "duplicate",
        "valid_notify_method" | "notify_method_capability" => "enotify",
        "valid_ext_list" => "extlists",
        "specialuse_exists" => "special-use",
        _ => return format!("unknown test `{name}`"),
    };
    format!("Sealwax doesn't support the `{name}` test (the {extension} extension)")
}

fn unsupported_tag(name: &str, tag: &str) -> String {
    let extension = match tag {
        "regex" => "regex",
        "count" | "value" => "relational",
        "user" | "detail" => "subaddress",
        "create" => "mailbox",
        "specialuse" => "special-use",
        "mime" | "anychild" | "type" | "subtype" | "contenttype" | "param" => "mime",
        "raw" | "content" | "text" => "body",
        "index" | "last" => "index",
        "zone" | "originalzone" => "date",
        "list" => "extlists",
        "flags" => {
            return format!(
                "Sealwax doesn't support `:flags` on `{name}`; use addflag before `{name}`"
            );
        }
        _ => return format!("`{name}` has no tag `:{tag}`"),
    };
    format!("Sealwax doesn't support `:{tag}` (the {extension} extension)")
}
