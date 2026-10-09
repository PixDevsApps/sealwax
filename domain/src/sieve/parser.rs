//! Sieve's grammar (RFC 5228 section 8.2): commands with arguments, tests and blocks, read
//! without knowing what any command means; [`super::check`] knows that.

use super::lexer::{Comment, Lexed, Lexer, Token};
use super::{Error, MAX_NESTING, MAX_SCRIPT_BYTES, Position};
use std::fmt;

/// A script as written: its commands and comments.
pub struct Tree {
    pub(super) commands: Vec<Command>,
    /// In the order they appear.
    pub(super) comments: Vec<Comment>,
}

impl fmt::Debug for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tree")
            .field("commands", &self.commands.len())
            .field("comments", &self.comments.len())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub(super) struct Command {
    /// In lower case.
    pub(super) name: String,
    pub(super) position: Position,
    pub(super) arguments: Vec<Argument>,
    pub(super) tests: Option<Tests>,
    pub(super) block: Option<Vec<Command>>,
    /// The byte offset just after its `;` or `}`.
    pub(super) end: usize,
}

#[derive(Clone, Debug)]
pub(super) struct Test {
    /// In lower case.
    pub(super) name: String,
    pub(super) position: Position,
    pub(super) arguments: Vec<Argument>,
    pub(super) tests: Option<Tests>,
}

/// The test after a command's or test's arguments, or the test list in `( … )`.
#[derive(Clone, Debug)]
pub(super) struct Tests {
    pub(super) position: Position,
    /// Written as a list in parentheses.
    pub(super) list: bool,
    pub(super) tests: Vec<Test>,
}

#[derive(Clone, Debug)]
pub(super) struct Argument {
    pub(super) position: Position,
    pub(super) value: Value,
}

#[derive(Clone, Debug)]
pub(super) enum Value {
    /// A string, or a list of them in brackets (`list`).
    Strings {
        list: bool,
        strings: Vec<String>,
    },
    Number(u64),
    /// In lower case, without its colon.
    Tag(String),
}

/// Reads `text` as Sieve: errors carry the line and column of the first token that doesn't fit
/// the grammar. A script larger than [`MAX_SCRIPT_BYTES`], or nesting blocks or tests deeper than
/// [`MAX_NESTING`], is an error too.
pub fn parse(text: &str) -> Result<Tree, Error> {
    if text.len() > MAX_SCRIPT_BYTES {
        return Err(Error::at(
            Position::START,
            "the script is larger than 1 MiB",
        ));
    }
    let mut parser = Parser::new(text)?;
    let commands = parser.commands(0)?;
    if parser.current.token != Token::End {
        return Err(parser.unexpected("a command"));
    }
    Ok(Tree {
        commands,
        comments: parser.lexer.comments,
    })
}

/// One test alone, as a disabled rule's comment holds it (`if false # <test>`).
pub(super) fn parse_test(text: &str) -> Result<Test, Error> {
    let mut parser = Parser::new(text)?;
    if !matches!(parser.current.token, Token::Identifier(_)) {
        return Err(parser.unexpected("a test"));
    }
    let test = parser.test(1)?;
    if parser.current.token != Token::End {
        return Err(parser.unexpected("the end of the test"));
    }
    Ok(test)
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    current: Lexed,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Result<Parser<'a>, Error> {
        let mut lexer = Lexer::new(text);
        let current = lexer.next()?;
        Ok(Parser { lexer, current })
    }

    /// Moves to the next token and returns the one it was on.
    fn bump(&mut self) -> Result<Lexed, Error> {
        let next = self.lexer.next()?;
        Ok(std::mem::replace(&mut self.current, next))
    }

    fn unexpected(&self, expected: &str) -> Error {
        Error::at(
            self.current.position,
            format!(
                "{expected} is expected here, not {}",
                self.current.token.describe()
            ),
        )
    }

    fn identifier(&mut self) -> Result<(String, Position), Error> {
        let lexed = self.bump()?;
        match lexed.token {
            Token::Identifier(name) => Ok((name, lexed.position)),
            _ => unreachable!("called on an identifier"),
        }
    }

    /// Commands up to a `}` or the end; `depth` is how many blocks hold them.
    fn commands(&mut self, depth: usize) -> Result<Vec<Command>, Error> {
        let mut commands = Vec::new();
        while matches!(self.current.token, Token::Identifier(_)) {
            commands.push(self.command(depth)?);
        }
        Ok(commands)
    }

    fn command(&mut self, depth: usize) -> Result<Command, Error> {
        let (name, position) = self.identifier()?;
        let (arguments, tests) = self.arguments(0)?;
        let (block, end) = match self.current.token {
            Token::Semicolon => (None, self.bump()?.end),
            Token::LeftBrace => {
                if depth >= MAX_NESTING {
                    return Err(Error::at(
                        self.current.position,
                        format!("blocks nest deeper than {MAX_NESTING} levels"),
                    ));
                }
                let open = self.bump()?.position;
                let block = self.commands(depth + 1)?;
                if self.current.token != Token::RightBrace {
                    return Err(Error::at(
                        self.current.position,
                        format!(
                            "a command or the `}}` closing the block from line {} is expected \
                             here, not {}",
                            open.line,
                            self.current.token.describe()
                        ),
                    ));
                }
                (Some(block), self.bump()?.end)
            }
            _ => {
                return Err(self.unexpected(&format!("`;` or a block after `{name}`")));
            }
        };
        Ok(Command {
            name,
            position,
            arguments,
            tests,
            block,
            end,
        })
    }

    /// Arguments, then at most one test or test list; `depth` is how many tests hold them.
    fn arguments(&mut self, depth: usize) -> Result<(Vec<Argument>, Option<Tests>), Error> {
        let mut arguments = Vec::new();
        loop {
            let position = self.current.position;
            let value = match self.current.token {
                Token::String(_) => {
                    let Token::String(string) = self.bump()?.token else {
                        unreachable!()
                    };
                    Value::Strings {
                        list: false,
                        strings: vec![string],
                    }
                }
                Token::LeftBracket => self.string_list()?,
                Token::Number(number) => {
                    self.bump()?;
                    Value::Number(number)
                }
                Token::Tag(_) => {
                    let Token::Tag(tag) = self.bump()?.token else {
                        unreachable!()
                    };
                    Value::Tag(tag)
                }
                Token::Identifier(_) => {
                    let test = self.test(depth + 1)?;
                    let tests = Tests {
                        position,
                        list: false,
                        tests: vec![test],
                    };
                    return Ok((arguments, Some(tests)));
                }
                Token::LeftParen => {
                    let tests = self.test_list(depth + 1)?;
                    return Ok((arguments, Some(tests)));
                }
                _ => return Ok((arguments, None)),
            };
            arguments.push(Argument { position, value });
        }
    }

    fn string_list(&mut self) -> Result<Value, Error> {
        self.bump()?;
        let mut strings = Vec::new();
        loop {
            if !matches!(self.current.token, Token::String(_)) {
                return Err(self.unexpected("a string"));
            }
            let Token::String(string) = self.bump()?.token else {
                unreachable!()
            };
            strings.push(string);
            match self.current.token {
                Token::Comma => {
                    self.bump()?;
                }
                Token::RightBracket => {
                    self.bump()?;
                    return Ok(Value::Strings {
                        list: true,
                        strings,
                    });
                }
                _ => return Err(self.unexpected("`,` or `]`")),
            }
        }
    }

    fn test(&mut self, depth: usize) -> Result<Test, Error> {
        if depth > MAX_NESTING {
            return Err(Error::at(
                self.current.position,
                format!("tests nest deeper than {MAX_NESTING} levels"),
            ));
        }
        let (name, position) = self.identifier()?;
        let (arguments, tests) = self.arguments(depth)?;
        Ok(Test {
            name,
            position,
            arguments,
            tests,
        })
    }

    fn test_list(&mut self, depth: usize) -> Result<Tests, Error> {
        let position = self.bump()?.position;
        let mut tests = Vec::new();
        loop {
            if !matches!(self.current.token, Token::Identifier(_)) {
                return Err(self.unexpected("a test"));
            }
            tests.push(self.test(depth)?);
            match self.current.token {
                Token::Comma => {
                    self.bump()?;
                }
                Token::RightParen => {
                    self.bump()?;
                    return Ok(Tests {
                        position,
                        list: true,
                        tests,
                    });
                }
                _ => return Err(self.unexpected("`,` or `)`")),
            }
        }
    }
}
