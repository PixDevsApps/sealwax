//! Plugins (ADR-022): programs the user installs that add to Sealwax without running inside it.
//! A plugin is a folder in `$XDG_CONFIG_HOME/sealwax/plugins/` with a `plugin.toml` that names
//! a command, the actions it adds to the message menu and the command palette, and the hooks it
//! runs on. Sealwax runs the command with a JSON document on stdin, holding the account and
//! the messages with only the parts the action or hook asks for, and reads an optional JSON
//! answer from stdout. A plugin never sees Sealwax's memory, store, passwords or tokens.
//!
//! The manifest is read like `config.toml`: every mistake with its line and column, and a
//! manifest with mistakes is not used at all.

use crate::config::{FileError, article, did_you_mean, entries, parse_toml};
use crate::model::{Address, Headers};
use std::collections::BTreeSet;
use toml::de::{DeTable, DeValue};

/// The folder in Sealwax's config directory that holds the plugins.
pub const PLUGINS_DIR: &str = "plugins";
/// Each plugin's manifest, in its folder.
pub const MANIFEST_FILE: &str = "plugin.toml";
/// The version of the JSON a plugin is sent; a change that breaks plugins raises it.
pub const PROTOCOL_VERSION: u32 = 1;
/// The longest a plugin's id, an action's id and a title may be, in characters.
const MAX_NAME: usize = 64;
/// The most a plugin may write to stdout or stderr; the rest is dropped.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// The longest notice shown from a plugin, in characters.
const MAX_NOTICE: usize = 200;

const MANIFEST_SETTINGS: [&str; 6] = [
    "name",
    "description",
    "command",
    "enabled",
    "action",
    "hook",
];
const ACTION_SETTINGS: [&str; 3] = ["id", "title", "receives"];
const HOOK_SETTINGS: [&str; 2] = ["event", "receives"];

/// What of a message a plugin is sent. The headers (sender, recipients, subject, date and
/// Message-ID) always are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    /// The body as plain text (HTML converted when there's no plain part).
    Text,
    /// The whole message as it came from the server, in base64.
    Raw,
}

impl Part {
    const ALL: [(&'static str, Part); 2] = [("text", Part::Text), ("raw", Part::Raw)];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Raw => "raw",
        }
    }
}

/// When a hook runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HookEvent {
    /// For new mail in an Inbox, once per sync that brings some, with every new message.
    NewMail,
}

impl HookEvent {
    const ALL: [(&'static str, HookEvent); 1] = [("new-mail", HookEvent::NewMail)];

    pub const fn name(self) -> &'static str {
        match self {
            Self::NewMail => "new-mail",
        }
    }
}

/// A command a plugin adds to the message menu and the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub receives: BTreeSet<Part>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub event: HookEvent,
    pub receives: BTreeSet<Part>,
}

/// A plugin's `plugin.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The plugin's folder name: its id in Sealwax and in the JSON.
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// The program and its arguments. A program with a `/` is a path in the plugin's folder
    /// when relative; one without is looked up on `PATH`.
    pub command: Vec<String>,
    pub enabled: bool,
    pub actions: Vec<Action>,
    pub hooks: Vec<Hook>,
}

impl Manifest {
    /// The actions offered for the message menu and the palette, with their plugin.
    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.id == id)
    }
}

/// Whether `id` can name a plugin or an action: 1 to 64 of `a`–`z`, `0`–`9`, `-` and `_`. GTK
/// action names and the JSON both take it as it is.
pub fn is_valid_id(id: &str) -> bool {
    (1..=MAX_NAME).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
}

fn kind(value: &DeValue) -> &'static str {
    match value {
        DeValue::String(_) => "string",
        DeValue::Integer(_) => "integer",
        DeValue::Float(_) => "number",
        DeValue::Boolean(_) => "boolean",
        DeValue::Datetime(_) => "date",
        DeValue::Array(_) => "list",
        DeValue::Table(_) => "table",
    }
}

/// Text for a title, a name or a description: trimmed, without control characters.
fn plain_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.chars().any(char::is_control) && value.chars().count() <= 200)
        .then(|| value.to_owned())
}

struct Reader<'t> {
    text: &'t str,
    errors: Vec<FileError>,
}

impl Reader<'_> {
    fn error(&mut self, span: std::ops::Range<usize>, message: impl Into<String>) {
        self.errors
            .push(FileError::spanned(self.text, span, message));
    }

    fn string(&mut self, value: &toml::Spanned<DeValue>, setting: &str) -> Option<String> {
        match value.get_ref() {
            DeValue::String(text) => match plain_text(text) {
                Some(text) => Some(text),
                None => {
                    self.error(
                        value.span(),
                        format!("{setting} must be text on one line, at most 200 characters"),
                    );
                    None
                }
            },
            other => {
                let found = article(kind(other));
                self.error(
                    value.span(),
                    format!("{setting} must be a string, not {found}"),
                );
                None
            }
        }
    }

    fn id(&mut self, value: &toml::Spanned<DeValue>, setting: &str) -> Option<String> {
        let id = self.string(value, setting)?;
        if is_valid_id(&id) {
            Some(id)
        } else {
            self.error(
                value.span(),
                format!("{setting} takes a–z, 0–9, - and _ only, at most 64 characters"),
            );
            None
        }
    }

    fn receives(&mut self, value: &toml::Spanned<DeValue>) -> BTreeSet<Part> {
        let DeValue::Array(items) = value.get_ref() else {
            self.error(value.span(), "receives must be a list such as [\"text\"]");
            return BTreeSet::new();
        };
        let mut parts = BTreeSet::new();
        for item in items {
            let found = match item.get_ref() {
                DeValue::String(name) => Part::ALL
                    .iter()
                    .find(|(known, _)| known == name)
                    .map(|(_, part)| *part),
                _ => None,
            };
            match found {
                Some(part) => {
                    parts.insert(part);
                }
                None => {
                    let word = match item.get_ref() {
                        DeValue::String(name) => name.to_string(),
                        _ => String::new(),
                    };
                    let hint = did_you_mean(&word, Part::ALL.iter().map(|(name, _)| *name));
                    self.error(
                        item.span(),
                        format!("receives takes \"text\" and \"raw\" (the headers are always sent){hint}"),
                    );
                }
            }
        }
        parts
    }

    fn tables<'a, 'i>(
        &mut self,
        value: &'a toml::Spanned<DeValue<'i>>,
        table: &str,
    ) -> Vec<&'a DeTable<'i>> {
        match value.get_ref() {
            DeValue::Array(items) => items
                .iter()
                .filter_map(|item| match item.get_ref() {
                    DeValue::Table(table) => Some(table),
                    _ => {
                        self.error(item.span(), format!("each {table} is a [[{table}]] table"));
                        None
                    }
                })
                .collect(),
            _ => {
                self.error(
                    value.span(),
                    format!("{table} is written as [[{table}]] tables"),
                );
                Vec::new()
            }
        }
    }

    fn unknown(&mut self, key: &toml::Spanned<std::borrow::Cow<str>>, known: &[&str], table: &str) {
        let hint = did_you_mean(key.get_ref(), known.iter().copied());
        self.error(
            key.span(),
            format!("{table} has no setting {}{hint}", key.get_ref()),
        );
    }
}

/// Reads the manifest of the plugin in folder `id`.
pub fn parse_manifest(id: &str, text: &str) -> Result<Manifest, Vec<FileError>> {
    let mut reader = Reader {
        text,
        errors: Vec::new(),
    };
    if !is_valid_id(id) {
        reader.error(
            0..0,
            "the plugin's folder name takes a–z, 0–9, - and _ only, at most 64 characters",
        );
    }
    let document = parse_toml(text).map_err(|error| vec![error])?;
    let mut name = None;
    let mut description = None;
    let mut command = Vec::new();
    let mut enabled = true;
    let mut actions: Vec<Action> = Vec::new();
    let mut hooks: Vec<Hook> = Vec::new();
    let mut command_seen = false;
    for (key, value) in entries(document.get_ref()) {
        match key.get_ref().as_ref() {
            "name" => name = reader.string(value, "name"),
            "description" => description = reader.string(value, "description"),
            "enabled" => match value.get_ref() {
                DeValue::Boolean(on) => enabled = *on,
                other => {
                    let found = article(kind(other));
                    reader.error(
                        value.span(),
                        format!("enabled must be true or false, not {found}"),
                    );
                }
            },
            "command" => {
                command_seen = true;
                match value.get_ref() {
                    DeValue::Array(items) if !items.is_empty() => {
                        for item in items {
                            match item.get_ref() {
                                DeValue::String(word) if !word.is_empty() && !word.contains('\0') => {
                                    command.push(word.to_string());
                                }
                                _ => reader.error(item.span(), "each word of command is a non-empty string"),
                            }
                        }
                    }
                    _ => reader.error(
                        value.span(),
                        "command is the program and its arguments, as a list: [\"my-plugin\", \"--from-sealwax\"]",
                    ),
                }
            }
            "action" => {
                for table in reader.tables(value, "action") {
                    let (mut id, mut title, mut receives) = (None, None, BTreeSet::new());
                    for (key, value) in entries(table) {
                        match key.get_ref().as_ref() {
                            "id" => id = reader.id(value, "id"),
                            "title" => title = reader.string(value, "title"),
                            "receives" => receives = reader.receives(value),
                            _ => reader.unknown(key, &ACTION_SETTINGS, "[[action]]"),
                        }
                    }
                    match (id, title) {
                        (Some(id), Some(_)) if actions.iter().any(|a| a.id == id) => {
                            reader.error(key.span(), format!("two actions have the id {id}"));
                        }
                        (Some(id), Some(title)) => actions.push(Action {
                            id,
                            title,
                            receives,
                        }),
                        _ => reader.error(key.span(), "each [[action]] needs an id and a title"),
                    }
                }
            }
            "hook" => {
                for table in reader.tables(value, "hook") {
                    let (mut event, mut receives) = (None, BTreeSet::new());
                    for (key, value) in entries(table) {
                        match key.get_ref().as_ref() {
                            "event" => match value.get_ref() {
                                DeValue::String(name) => {
                                    event = HookEvent::ALL
                                        .iter()
                                        .find(|(known, _)| known == name)
                                        .map(|(_, e)| *e);
                                    if event.is_none() {
                                        let hint = did_you_mean(
                                            name,
                                            HookEvent::ALL.iter().map(|(n, _)| *n),
                                        );
                                        reader.error(
                                            value.span(),
                                            format!("event takes \"new-mail\"{hint}"),
                                        );
                                    }
                                }
                                _ => reader.error(value.span(), "event takes \"new-mail\""),
                            },
                            "receives" => receives = reader.receives(value),
                            _ => reader.unknown(key, &HOOK_SETTINGS, "[[hook]]"),
                        }
                    }
                    match event {
                        Some(event) => hooks.push(Hook { event, receives }),
                        None if reader.errors.is_empty() => {
                            reader.error(key.span(), "each [[hook]] needs an event")
                        }
                        None => {}
                    }
                }
            }
            _ => reader.unknown(key, &MANIFEST_SETTINGS, "plugin.toml"),
        }
    }
    if !command_seen {
        reader.error(0..0, "plugin.toml needs command = [\"program\", …]");
    }
    if actions.is_empty() && hooks.is_empty() && reader.errors.is_empty() {
        reader.error(
            0..0,
            "plugin.toml has no [[action]] and no [[hook]], so it does nothing",
        );
    }
    if !reader.errors.is_empty() {
        return Err(reader.errors);
    }
    Ok(Manifest {
        name: name.unwrap_or_else(|| id.to_owned()),
        id: id.to_owned(),
        description,
        command,
        enabled,
        actions,
        hooks,
    })
}

/// Why a plugin runs: one of its actions, chosen by the user, or a hook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    Action(String),
    Hook(HookEvent),
}

/// The account the messages are in, as a plugin is told it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub key: String,
    pub address: String,
    pub label: String,
}

/// One message for a plugin: what the store knows, and the parts asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageInfo {
    /// The folder's name on the server, e.g. `INBOX`.
    pub folder: String,
    pub headers: Headers,
    pub text: Option<String>,
    pub raw: Option<Vec<u8>>,
}

impl MessageInfo {
    /// The message as a plugin is sent it, from its raw source: the parts in `receives`.
    pub fn from_raw(folder: &str, raw: &[u8], receives: &BTreeSet<Part>) -> Self {
        let parsed = crate::message::parse(raw);
        Self {
            folder: folder.to_owned(),
            headers: parsed.headers,
            // Lines end in \n, whatever the message used.
            text: receives
                .contains(&Part::Text)
                .then(|| parsed.body_text.replace("\r\n", "\n").replace('\r', "\n")),
            raw: receives.contains(&Part::Raw).then(|| raw.to_vec()),
        }
    }
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_addresses(addresses: &[Address]) -> String {
    let items: Vec<String> = addresses
        .iter()
        .map(|a| {
            format!(
                "{{\"name\": {}, \"address\": {}}}",
                json_string(&a.name),
                json_string(&a.email)
            )
        })
        .collect();
    format!("[{}]", items.join(", "))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(TABLE[(n >> shift) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The JSON a plugin is sent on stdin (protocol version 1). Written by hand, in a fixed order,
/// so the protocol is exactly what this function shows.
pub fn payload(
    plugin: &str,
    trigger: &Trigger,
    account: &AccountInfo,
    messages: &[MessageInfo],
) -> String {
    let (event, action) = match trigger {
        Trigger::Action(id) => ("action", json_string(id)),
        Trigger::Hook(event) => (event.name(), "null".to_owned()),
    };
    let messages: Vec<String> = messages
        .iter()
        .map(|message| {
            let h = &message.headers;
            let optional = |value: &Option<String>| value.as_deref().map_or("null".to_owned(), json_string);
            format!(
                "{{\"folder\": {}, \"message_id\": {}, \"subject\": {}, \"from\": {}, \"to\": {}, \"cc\": {}, \
                 \"reply_to\": {}, \"date\": {}, \"text\": {}, \"raw\": {}}}",
                json_string(&message.folder),
                optional(&h.message_id),
                json_string(&h.subject),
                json_addresses(&h.from),
                json_addresses(&h.to),
                json_addresses(&h.cc),
                json_addresses(&h.reply_to),
                h.date.map_or("null".to_owned(), |date| date.to_string()),
                optional(&message.text),
                message.raw.as_deref().map_or("null".to_owned(), |raw| json_string(&base64(raw))),
            )
        })
        .collect();
    format!(
        "{{\"version\": {PROTOCOL_VERSION}, \"event\": {}, \"plugin\": {}, \"action\": {action}, \
         \"account\": {{\"key\": {}, \"address\": {}, \"label\": {}}}, \"messages\": [{}]}}\n",
        json_string(event),
        json_string(plugin),
        json_string(&account.key),
        json_string(&account.address),
        json_string(&account.label),
        messages.join(", ")
    )
}

/// What a plugin answered on stdout: a notice to show, or nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub notice: Option<String>,
}

/// Reads a plugin's stdout. An empty answer is fine; so is any JSON object, of which only
/// `"notice"` is used. Anything else is the plugin's mistake, reported as text.
pub fn parse_reply(stdout: &str) -> Result<Reply, String> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Reply::default());
    }
    if !text.starts_with('{') {
        return Err("its answer on stdout isn't a JSON object".to_owned());
    }
    // The one field read, without a JSON dependency: "notice": "…" at the top level.
    let notice = text.find("\"notice\"").and_then(|at| {
        let rest = text[at + 8..].trim_start().strip_prefix(':')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        let mut value = String::new();
        let mut characters = rest.chars();
        while let Some(character) = characters.next() {
            match character {
                '"' => return Some(value),
                '\\' => match characters.next()? {
                    'n' => value.push(' '),
                    't' => value.push(' '),
                    'u' => {
                        let code: String = characters.by_ref().take(4).collect();
                        value.push(
                            u32::from_str_radix(&code, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .unwrap_or('\u{fffd}'),
                        );
                    }
                    other => value.push(other),
                },
                c if c.is_control() => value.push(' '),
                c => value.push(c),
            }
        }
        None
    });
    Ok(Reply {
        notice: notice
            .map(|notice| notice.trim().chars().take(MAX_NOTICE).collect::<String>())
            .filter(|notice| !notice.is_empty()),
    })
}

/// The first line of a failing plugin's stderr, cut short, for a message.
pub fn first_line(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            line.chars()
                .filter(|c| !c.is_control())
                .take(MAX_NOTICE)
                .collect()
        })
}

#[cfg(test)]
mod tests;
