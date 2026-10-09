//! The Sieve module's tests: the grammar and the subset (`language`), running scripts
//! (`running`), and the rules model with its spam sections (`rules`). Every name and address is
//! fictional.

mod language;
mod rules;
mod running;

use super::{Error, MessageView, Outcome, compile, evaluate};

/// A message as the engine would show it to a script.
#[derive(Clone)]
pub(super) struct Message {
    /// Lower-case names and decoded values, in order.
    headers: Vec<(String, String)>,
    size: u64,
    flags: Vec<String>,
    mailbox: String,
}

const ADDRESS_HEADERS: [&str; 6] = ["from", "sender", "to", "cc", "bcc", "reply-to"];

impl Message {
    pub(super) fn new() -> Message {
        Message {
            headers: Vec::new(),
            size: 2_000,
            flags: Vec::new(),
            mailbox: "INBOX".to_owned(),
        }
    }

    pub(super) fn header(mut self, name: &str, value: &str) -> Message {
        self.headers
            .push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    pub(super) fn from(self, value: &str) -> Message {
        self.header("From", value)
    }

    pub(super) fn size(mut self, size: u64) -> Message {
        self.size = size;
        self
    }

    pub(super) fn flags(mut self, flags: &[&str]) -> Message {
        self.flags = flags.iter().map(|flag| (*flag).to_owned()).collect();
        self
    }

    pub(super) fn mailbox(mut self, mailbox: &str) -> Message {
        self.mailbox = mailbox.to_owned();
        self
    }
}

impl MessageView for Message {
    fn header(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// Each comma-separated part's `<…>`, or the part itself when it has an `@`.
    fn addresses(&self, name: &str) -> Vec<&str> {
        if !ADDRESS_HEADERS.contains(&name) {
            return Vec::new();
        }
        self.header(name)
            .into_iter()
            .flat_map(|value| value.split(','))
            .filter_map(|part| match (part.find('<'), part.rfind('>')) {
                (Some(open), Some(close)) if open < close => Some(&part[open + 1..close]),
                _ => Some(part.trim()).filter(|part| part.contains('@')),
            })
            .collect()
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn flags(&self) -> Vec<&str> {
        self.flags.iter().map(String::as_str).collect()
    }

    fn mailbox(&self) -> &str {
        &self.mailbox
    }
}

/// `script` compiled, or a panic showing its error in full.
pub(super) fn script(text: &str) -> super::Script {
    compile(text).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

pub(super) fn run(text: &str, message: &Message) -> Outcome {
    evaluate(&script(text), message)
}

/// The error `text` gives, as `(line, column, message)`.
pub(super) fn error(text: &str) -> (u32, u32, String) {
    match compile(text) {
        Ok(_) => panic!("no error from:\n{text}"),
        Err(Error {
            line,
            column,
            message,
        }) => (line, column, message),
    }
}

/// The folders an outcome files into.
pub(super) fn filed(outcome: &Outcome) -> Vec<&str> {
    outcome
        .filed
        .iter()
        .map(|filed| filed.folder.as_str())
        .collect()
}

pub(super) fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| (*text).to_owned()).collect()
}
