//! Sieve's tokens (RFC 5228 section 8.1), each with where it starts and ends. Comments are
//! skipped as white space and kept aside: the rules model reads `# rule:[…]` names from them.

use super::{Error, Position};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Token {
    /// In lower case: identifiers are case-insensitive.
    Identifier(String),
    /// Without its colon, in lower case.
    Tag(String),
    Number(u64),
    /// A quoted string or a `text:` string, unescaped.
    String(String),
    LeftBracket,
    RightBracket,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    Comma,
    Semicolon,
    End,
}

impl Token {
    /// The token as an error message names it.
    pub(super) fn describe(&self) -> &'static str {
        match self {
            Token::Identifier(_) => "a name",
            Token::Tag(_) => "a tag",
            Token::Number(_) => "a number",
            Token::String(_) => "a string",
            Token::LeftBracket => "`[`",
            Token::RightBracket => "`]`",
            Token::LeftParen => "`(`",
            Token::RightParen => "`)`",
            Token::LeftBrace => "`{`",
            Token::RightBrace => "`}`",
            Token::Comma => "`,`",
            Token::Semicolon => "`;`",
            Token::End => "the end of the script",
        }
    }
}

/// A token, where it starts, and the byte offset just after it.
#[derive(Clone, Debug)]
pub(super) struct Lexed {
    pub(super) token: Token,
    pub(super) position: Position,
    pub(super) end: usize,
}

/// A comment: `# …` to the end of its line, or `/* … */`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Comment {
    /// Byte offsets of the whole comment, `#` or `/*` included (for `#`, up to its line break).
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) line: u32,
    /// `# …`; otherwise `/* … */`.
    pub(super) hash: bool,
    /// The text after `#` (without a CR before the line break) or between `/*` and `*/`.
    pub(super) text: String,
}

pub(super) struct Lexer<'a> {
    text: &'a str,
    position: Position,
    pub(super) comments: Vec<Comment>,
}

impl<'a> Lexer<'a> {
    pub(super) fn new(text: &'a str) -> Lexer<'a> {
        Lexer {
            text,
            position: Position::START,
            comments: Vec::new(),
        }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.position.offset..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.position.offset += c.len_utf8();
        if c == '\n' {
            self.position.line += 1;
            self.position.column = 1;
        } else {
            self.position.column += 1;
        }
        Some(c)
    }

    pub(super) fn next(&mut self) -> Result<Lexed, Error> {
        self.skip()?;
        let position = self.position;
        let Some(c) = self.peek() else {
            return Ok(self.lexed(Token::End, position));
        };
        let token = match c {
            '[' | ']' | '(' | ')' | '{' | '}' | ',' | ';' => {
                self.bump();
                match c {
                    '[' => Token::LeftBracket,
                    ']' => Token::RightBracket,
                    '(' => Token::LeftParen,
                    ')' => Token::RightParen,
                    '{' => Token::LeftBrace,
                    '}' => Token::RightBrace,
                    ',' => Token::Comma,
                    _ => Token::Semicolon,
                }
            }
            '"' => self.quoted(position)?,
            ':' => {
                self.bump();
                if !self.peek().is_some_and(starts_identifier) {
                    return Err(Error::at(position, "a `:` without a tag name after it"));
                }
                Token::Tag(self.identifier())
            }
            '0'..='9' => self.number(position)?,
            c if starts_identifier(c) => {
                let word = self.identifier();
                if word == "text" && self.peek() == Some(':') {
                    self.bump();
                    self.multiline(position)?
                } else {
                    Token::Identifier(word)
                }
            }
            _ => return Err(Error::at(position, "a character Sieve doesn't allow here")),
        };
        Ok(self.lexed(token, position))
    }

    fn lexed(&self, token: Token, position: Position) -> Lexed {
        Lexed {
            token,
            position,
            end: self.position.offset,
        }
    }

    /// White space and comments.
    fn skip(&mut self) -> Result<(), Error> {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r' | '\n') => {
                    self.bump();
                }
                Some('#') => self.hash_comment(),
                Some('/') if self.rest().starts_with("/*") => self.bracket_comment()?,
                _ => return Ok(()),
            }
        }
    }

    fn hash_comment(&mut self) {
        let start = self.position;
        self.bump();
        let line = self.rest().split('\n').next().unwrap_or_default();
        for _ in line.chars() {
            self.bump();
        }
        let text = line.strip_suffix('\r').unwrap_or(line);
        self.comments.push(Comment {
            start: start.offset,
            end: self.position.offset,
            line: start.line,
            hash: true,
            text: text.to_owned(),
        });
    }

    fn bracket_comment(&mut self) -> Result<(), Error> {
        let start = self.position;
        self.bump();
        self.bump();
        let Some(length) = self.rest().find("*/") else {
            return Err(Error::at(start, "a comment `/*` that never ends with `*/`"));
        };
        let text = &self.rest()[..length];
        for _ in text.chars() {
            self.bump();
        }
        self.bump();
        self.bump();
        self.comments.push(Comment {
            start: start.offset,
            end: self.position.offset,
            line: start.line,
            hash: false,
            text: text.to_owned(),
        });
        Ok(())
    }

    fn identifier(&mut self) -> String {
        let length = self
            .rest()
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(self.rest().len());
        let word = self.rest()[..length].to_ascii_lowercase();
        self.position.offset += length;
        self.position.column += length as u32;
        word
    }

    fn quoted(&mut self, start: Position) -> Result<Token, Error> {
        self.bump();
        let mut value = String::new();
        loop {
            match self.bump() {
                None => return Err(Error::at(start, "a string that never ends with `\"`")),
                Some('"') => return Ok(Token::String(value)),
                // An escape the RFC doesn't define is the character itself (section 2.4.2).
                Some('\\') => match self.bump() {
                    Some(c) => value.push(c),
                    None => return Err(Error::at(start, "a string that never ends with `\"`")),
                },
                Some(c) => value.push(c),
            }
        }
    }

    fn number(&mut self, start: Position) -> Result<Token, Error> {
        let mut value: u64 = 0;
        while let Some(digit) = self.peek().and_then(|c| c.to_digit(10)) {
            self.bump();
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(digit)))
                .ok_or_else(|| Error::at(start, "a number too large for Sieve"))?;
        }
        let unit: u64 = match self.peek() {
            Some('k' | 'K') => 1 << 10,
            Some('m' | 'M') => 1 << 20,
            Some('g' | 'G') => 1 << 30,
            _ => 1,
        };
        if unit > 1 {
            self.bump();
        }
        value
            .checked_mul(unit)
            .map(Token::Number)
            .ok_or_else(|| Error::at(start, "a number too large for Sieve"))
    }

    /// The lines after `text:` up to a line holding only `.`; a line starting `..` loses one dot.
    fn multiline(&mut self, start: Position) -> Result<Token, Error> {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.bump();
        }
        match self.peek() {
            Some('#') => self.hash_comment(),
            Some('\r') if self.rest().starts_with("\r\n") => {
                self.bump();
            }
            Some('\n') => {}
            _ => return Err(Error::at(start, "`text:` must end its line")),
        }
        if self.bump() != Some('\n') {
            return Err(Error::at(
                start,
                "a `text:` string without its closing `.` line",
            ));
        }
        let mut value = String::new();
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                return Err(Error::at(
                    start,
                    "a `text:` string without its closing `.` line",
                ));
            }
            let (line, newline) = match rest.find('\n') {
                Some(end) => (&rest[..end], true),
                None => (rest, false),
            };
            let finished = line.strip_suffix('\r').unwrap_or(line) == ".";
            if !finished {
                value.push_str(if line.starts_with("..") {
                    &line[1..]
                } else {
                    line
                });
                if newline {
                    value.push('\n');
                }
            }
            for _ in line.chars() {
                self.bump();
            }
            if newline {
                self.bump();
            }
            if finished {
                return Ok(Token::String(value));
            }
        }
    }
}

fn starts_identifier(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
