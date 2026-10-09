//! Running a checked [`Script`] on one message (RFC 5228 section 2.10, RFC 5232, RFC 6785):
//! what the engine should do with it, as an [`Outcome`]. Each command runs at most once and no
//! test loops, so a run takes time linear in the script's size for a given message.

use super::check::{AddressPart, Command, Comparator, MatchType, Matcher, Script, Test};
use crate::model::Flags;
use std::fmt;

/// The message a script runs on, as the engine sees it.
pub trait MessageView {
    /// Every value of the header `name` (in lower case), in order: unfolded and RFC 2047-decoded,
    /// without the name. Empty when the message has none.
    fn header(&self, name: &str) -> Vec<&str>;

    /// The addresses in the header `name` (in lower case), in order, each as `local@domain`
    /// without its display name; group names left out. Empty when it has none.
    fn addresses(&self, name: &str) -> Vec<&str>;

    /// The message's size in bytes (RFC822.SIZE).
    fn size(&self) -> u64;

    /// Its IMAP flags and keywords (`\Seen`, `$Junk`, …); `\Recent` is ignored.
    fn flags(&self) -> Vec<&str>;

    /// The full name of the folder it arrived in, decoded as the store names it (`INBOX`,
    /// `INBOX.Kunder`): `environment "imap.mailbox"`, and where `keep` leaves it.
    fn mailbox(&self) -> &str;
}

/// What a script decided for a message. Flags start as the message's own (RFC 6785); each
/// change is relative to them, so a `setflag` is the flags it adds and removes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The message stays in the folder it arrived in: the implicit keep wasn't cancelled, or
    /// `keep` (or a `fileinto` of that very folder) ran.
    pub keep: bool,
    /// The flags of the message that stays; empty unless `keep`.
    pub flags: FlagChange,
    /// The folders it is filed into, in the order of their first `fileinto`, each once and never
    /// the folder it arrived in. When it isn't kept, the engine moves it into one of them and
    /// copies it into the rest; when it is, it copies it into each.
    pub filed: Vec<Filed>,
    /// `discard` ran. A message neither kept nor filed goes to Trash; Sealwax never expunges.
    pub discard: bool,
    /// The script ended at a `stop`.
    pub stopped: bool,
}

impl Outcome {
    /// Neither kept nor filed: the engine moves it to Trash.
    pub fn trashed(&self) -> bool {
        !self.keep && self.filed.is_empty()
    }
}

impl fmt::Debug for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outcome")
            .field("keep", &self.keep)
            .field("flags", &self.flags)
            .field("filed", &self.filed)
            .field("discard", &self.discard)
            .field("stopped", &self.stopped)
            .finish()
    }
}

/// A folder the message is filed into.
#[derive(Clone, PartialEq, Eq)]
pub struct Filed {
    /// The folder's full name as the script gives it.
    pub folder: String,
    /// Every `fileinto` of this folder had `:copy` (RFC 3894).
    pub copy: bool,
    /// The flags of the copy filed here: those at its last `fileinto`.
    pub flags: FlagChange,
}

impl fmt::Debug for Filed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Filed")
            .field(
                "folder",
                &format_args!("{} chars", self.folder.chars().count()),
            )
            .field("copy", &self.copy)
            .field("flags", &self.flags)
            .finish()
    }
}

/// Flags to add and remove, relative to the message's own.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct FlagChange {
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

impl FlagChange {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }

    /// The change as Sealwax's flag bits, plus the other keywords to add and to remove.
    pub fn to_model(&self) -> (Flags, Flags, Vec<String>, Vec<String>) {
        let (add, add_keywords) = Flags::from_imap(self.add.iter().map(String::as_str));
        let (remove, remove_keywords) = Flags::from_imap(self.remove.iter().map(String::as_str));
        (add, remove, add_keywords, remove_keywords)
    }
}

impl fmt::Debug for FlagChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlagChange")
            .field("add", &self.add.len())
            .field("remove", &self.remove.len())
            .finish()
    }
}

/// The IMAP names of Sealwax's flag bits, for [`MessageView::flags`].
pub fn flag_names(flags: Flags) -> Vec<&'static str> {
    [
        (Flags::SEEN, "\\Seen"),
        (Flags::ANSWERED, "\\Answered"),
        (Flags::FLAGGED, "\\Flagged"),
        (Flags::DELETED, "\\Deleted"),
        (Flags::DRAFT, "\\Draft"),
        (Flags::FORWARDED, "$Forwarded"),
        (Flags::JUNK, "$Junk"),
        (Flags::NOT_JUNK, "$NotJunk"),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .map(|(_, name)| name)
    .collect()
}

/// Runs `script` on `message`.
pub fn evaluate(script: &Script, message: &(impl MessageView + ?Sized)) -> Outcome {
    let initial = FlagSet::new(
        message
            .flags()
            .into_iter()
            .filter(|flag| !flag.eq_ignore_ascii_case("\\recent")),
    );
    let mut run = Run {
        message,
        flags: initial.clone(),
        implicit_keep: true,
        kept: None,
        filed: Vec::new(),
        discard: false,
    };
    let stopped = run.block(&script.commands) == Flow::Stop;
    let kept = if run.implicit_keep {
        Some(run.flags)
    } else {
        run.kept
    };
    Outcome {
        keep: kept.is_some(),
        flags: kept.map(|flags| initial.change(&flags)).unwrap_or_default(),
        filed: run
            .filed
            .into_iter()
            .map(|(folder, copy, flags)| Filed {
                folder,
                copy,
                flags: initial.change(&flags),
            })
            .collect(),
        discard: run.discard,
        stopped,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

struct Run<'m, M: ?Sized> {
    message: &'m M,
    /// RFC 5232's internal variable.
    flags: FlagSet,
    implicit_keep: bool,
    /// The flags at the last explicit keep.
    kept: Option<FlagSet>,
    /// Each folder once: its name, whether every `fileinto` had `:copy`, its flags.
    filed: Vec<(String, bool, FlagSet)>,
    discard: bool,
}

impl<M: MessageView + ?Sized> Run<'_, M> {
    fn block(&mut self, commands: &[Command]) -> Flow {
        for command in commands {
            match command {
                Command::Require(_) => {}
                Command::If(branches, otherwise) => {
                    let chosen = branches
                        .iter()
                        .find(|(test, _)| self.test(test))
                        .map(|(_, block)| block)
                        .or(otherwise.as_ref());
                    if let Some(block) = chosen
                        && self.block(block) == Flow::Stop
                    {
                        return Flow::Stop;
                    }
                }
                Command::Stop => return Flow::Stop,
                Command::Keep => {
                    self.kept = Some(self.flags.clone());
                    self.implicit_keep = false;
                }
                Command::Discard => {
                    self.discard = true;
                    self.implicit_keep = false;
                }
                Command::FileInto { folder, copy } => self.file(folder, *copy),
                Command::AddFlag(flags) => self.flags.add(flags),
                Command::SetFlag(flags) => self.flags = FlagSet::new(flags),
                Command::RemoveFlag(flags) => self.flags.remove(flags),
            }
        }
        Flow::Continue
    }

    fn file(&mut self, folder: &str, copy: bool) {
        if !copy {
            self.implicit_keep = false;
        }
        if same_folder(folder, self.message.mailbox()) {
            self.kept = Some(self.flags.clone());
            return;
        }
        match self
            .filed
            .iter_mut()
            .find(|(filed, ..)| same_folder(filed, folder))
        {
            Some((_, all_copies, flags)) => {
                *all_copies &= copy;
                *flags = self.flags.clone();
            }
            None => self
                .filed
                .push((folder.to_owned(), copy, self.flags.clone())),
        }
    }

    fn test(&self, test: &Test) -> bool {
        match test {
            Test::True => true,
            Test::False => false,
            Test::Not(test) => !self.test(test),
            Test::AllOf(tests) => tests.iter().all(|test| self.test(test)),
            Test::AnyOf(tests) => tests.iter().any(|test| self.test(test)),
            Test::Header {
                matcher,
                headers,
                keys,
            } => headers.iter().any(|name| {
                self.message
                    .header(name)
                    .into_iter()
                    .any(|value| any_key(matcher, value.trim(), keys))
            }),
            Test::Address {
                part,
                matcher,
                headers,
                keys,
            } => headers.iter().any(|name| {
                self.message
                    .addresses(name)
                    .into_iter()
                    .any(|address| any_key(matcher, address_part(address, *part), keys))
            }),
            Test::Exists(headers) => headers
                .iter()
                .all(|name| !self.message.header(name).is_empty()),
            Test::Size { over, limit } => {
                let size = self.message.size();
                if *over { size > *limit } else { size < *limit }
            }
            Test::HasFlag { matcher, keys } => {
                self.flags.0.iter().any(|flag| any_key(matcher, flag, keys))
            }
            // RFC 5183: an item Sealwax doesn't know doesn't exist, and the test is false.
            Test::Environment {
                matcher,
                name,
                keys,
            } => name == "imap.mailbox" && any_key(matcher, self.message.mailbox(), keys),
        }
    }
}

/// Folder names compare exactly, but the Inbox's in any case (RFC 3501).
fn same_folder(a: &str, b: &str) -> bool {
    a == b || (a.eq_ignore_ascii_case("INBOX") && b.eq_ignore_ascii_case("INBOX"))
}

fn address_part(address: &str, part: AddressPart) -> &str {
    let (local, domain) = address.rsplit_once('@').unwrap_or((address, ""));
    match part {
        AddressPart::All => address,
        AddressPart::LocalPart => local,
        AddressPart::Domain => domain,
    }
}

fn any_key(matcher: &Matcher, value: &str, keys: &[String]) -> bool {
    keys.iter().any(|key| matches(matcher, value, key))
}

/// RFC 5228 section 2.7: `:is`, `:contains` and `:matches` under `i;octet` or
/// `i;ascii-casemap` (ASCII letters fold; other characters compare exactly).
pub(super) fn matches(matcher: &Matcher, value: &str, key: &str) -> bool {
    let fold = matcher.comparator == Comparator::AsciiCasemap;
    match matcher.kind {
        MatchType::Is if fold => value.eq_ignore_ascii_case(key),
        MatchType::Is => value == key,
        MatchType::Contains if fold => {
            key.is_empty()
                || value
                    .as_bytes()
                    .windows(key.len())
                    .any(|window| window.eq_ignore_ascii_case(key.as_bytes()))
        }
        MatchType::Contains => value.contains(key),
        MatchType::Matches => wildcard(value, key, fold),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Piece {
    Char(char),
    /// `?`: one character.
    One,
    /// `*`: any run of characters.
    Any,
}

/// `:matches`: `*` and `?` over characters, `\` making the next character literal. Greedy with
/// one point to go back to, so at most `value × pattern` steps.
fn wildcard(value: &str, pattern: &str, fold: bool) -> bool {
    let mut pieces = Vec::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        pieces.push(match c {
            '*' => Piece::Any,
            '?' => Piece::One,
            '\\' => Piece::Char(chars.next().unwrap_or('\\')),
            c => Piece::Char(c),
        });
    }
    let value: Vec<char> = value.chars().collect();
    let same = |a: char, b: char| a == b || (fold && a.eq_ignore_ascii_case(&b));
    let (mut v, mut p) = (0, 0);
    let mut back: Option<(usize, usize)> = None;
    while v < value.len() {
        match pieces.get(p) {
            Some(Piece::Any) => {
                back = Some((p, v));
                p += 1;
            }
            Some(Piece::One) => {
                v += 1;
                p += 1;
            }
            Some(Piece::Char(c)) if same(*c, value[v]) => {
                v += 1;
                p += 1;
            }
            _ => match back {
                Some((star, from)) => {
                    p = star + 1;
                    v = from + 1;
                    back = Some((star, from + 1));
                }
                None => return false,
            },
        }
    }
    pieces[p..].iter().all(|piece| *piece == Piece::Any)
}

/// Flags compared without regard to case, each once, in the order first set.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FlagSet(Vec<String>);

impl FlagSet {
    fn new<S: AsRef<str>>(flags: impl IntoIterator<Item = S>) -> FlagSet {
        let mut set = FlagSet(Vec::new());
        for flag in flags {
            set.insert(flag.as_ref());
        }
        set
    }

    fn contains(&self, flag: &str) -> bool {
        self.0.iter().any(|held| held.eq_ignore_ascii_case(flag))
    }

    fn insert(&mut self, flag: &str) {
        if !flag.is_empty() && !self.contains(flag) {
            self.0.push(flag.to_owned());
        }
    }

    fn add(&mut self, flags: &[String]) {
        for flag in flags {
            self.insert(flag);
        }
    }

    fn remove(&mut self, flags: &[String]) {
        self.0
            .retain(|held| !flags.iter().any(|flag| held.eq_ignore_ascii_case(flag)));
    }

    /// From these flags to `after`.
    fn change(&self, after: &FlagSet) -> FlagChange {
        FlagChange {
            add: after
                .0
                .iter()
                .filter(|flag| !self.contains(flag))
                .cloned()
                .collect(),
            remove: self
                .0
                .iter()
                .filter(|flag| !after.contains(flag))
                .cloned()
                .collect(),
        }
    }
}
