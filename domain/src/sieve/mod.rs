//! Mail rules in Sieve (RFC 5228), run by Sealwax itself on the mail it syncs ([ADR-020],
//! docs/m9/folders-rules.md, docs/m9/sieve.md). Pure: [`parse`] reads the whole grammar,
//! [`check`] accepts only the subset Sealwax runs and names anything outside it, [`evaluate`]
//! runs a checked [`Script`] on one message, and [`rules`] is the model the Rules dialog edits,
//! written in Roundcube's `# rule:[…]` form and read back.
//!
//! Scripts hold addresses and folder names, which never reach a log: the `Debug` of every type
//! here that holds script text prints counts and kinds only, and an [`Error`]'s `Debug` leaves
//! out its message.
//!
//! [ADR-020]: ../../../docs/DECISIONS.md

mod check;
mod evaluate;
mod lexer;
mod parser;
pub mod rules;

#[cfg(test)]
mod tests;

pub use check::{EXTENSIONS, Script, check};
pub use evaluate::{Filed, FlagChange, MessageView, Outcome, evaluate, flag_names};
pub use parser::{Tree, parse};
pub use rules::{
    Action, Condition, Field, Folders, Invalid, Item, Match, Operator, Reason, Rule, RuleSet,
    Senders,
};

use std::fmt;

/// The largest script Sealwax reads, in bytes.
pub const MAX_SCRIPT_BYTES: usize = 1 << 20;

/// How deep blocks may nest, and tests inside tests.
pub const MAX_NESTING: usize = 32;

/// Parses and checks `text`: the script Sealwax runs, or the first error in it.
pub fn compile(text: &str) -> Result<Script, Error> {
    check(&parse(text)?)
}

/// What is wrong with a script, and where.
#[derive(Clone, PartialEq, Eq)]
pub struct Error {
    /// 1-based.
    pub line: u32,
    /// 1-based, in characters.
    pub column: u32,
    /// What is wrong, in plain words. It can name a command, a test, a tag or an extension the
    /// script used, never a string's text; log only the line and column.
    pub message: String,
}

impl Error {
    fn at(position: Position, message: impl Into<String>) -> Error {
        Error {
            line: position.line,
            column: position.column,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("line", &self.line)
            .field("column", &self.column)
            .finish_non_exhaustive()
    }
}

impl std::error::Error for Error {}

/// A place in the script's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Position {
    line: u32,
    column: u32,
    /// In bytes from the start.
    offset: usize,
}

impl Position {
    const START: Position = Position {
        line: 1,
        column: 1,
        offset: 0,
    };
}
