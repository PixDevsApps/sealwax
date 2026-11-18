//! `accounts.toml` (ADR-009): the accounts, in `$XDG_CONFIG_HOME/sealwax/`. The setup assistant
//! adds and removes entries, and the user may edit the file by hand. It is read like
//! `config.toml`: every mistake with its line and column, and a file with mistakes is not
//! applied at all. It never holds a secret: a password, secret or token field is a mistake.
//! Adding appends text and removing edits the parsed document, so the user's comments and
//! layout survive the assistant's writes.

use crate::account::{
    AccountColor, AccountKey, AccountSettings, AuthMethod, CertificatePin, Endpoint, Identity,
    OAuthProvider, PgpSettings, Pop3Policy, Protocol, Receive, Security, is_loopback, is_valid_key,
};
use crate::address::{is_sendable, looks_like_address, same};
use crate::settings_file::{FileError, article, did_you_mean, entries, parse_toml};
use std::fmt;
use std::net::Ipv6Addr;
use std::ops::Range;
use toml::Spanned;
use toml::de::{DeTable, DeValue};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value, value};

pub const ACCOUNTS_FILE: &str = "accounts.toml";

const ACCOUNT_SETTINGS: [&str; 16] = [
    "key",
    "name",
    "label",
    "color",
    "address",
    "username",
    "auth",
    "signature",
    "signature_html",
    "pgp_sign",
    "pgp_encrypt",
    "pgp_key",
    "imap",
    "pop3",
    "smtp",
    "identity",
];
const ENDPOINT_SETTINGS: [&str; 4] = ["host", "port", "security", "certificate"];
/// What `[account.pop3]` has besides the server.
const POP3_SETTINGS: [&str; 2] = ["leave_on_server", "delete_after_days"];
/// The most days `delete_after_days` takes: ten years.
pub const MAX_DELETE_AFTER_DAYS: u32 = 3650;
const IDENTITY_SETTINGS: [&str; 4] = ["name", "address", "signature", "signature_html"];
/// The longest signature, in characters.
pub const MAX_SIGNATURE_CHARS: usize = 10_000;

/// The port a connection uses when `accounts.toml` names none.
pub fn default_port(protocol: Protocol, security: Security) -> u16 {
    match (protocol, security) {
        (Protocol::Imap, Security::Tls) => 993,
        (Protocol::Imap, Security::StartTls) => 143,
        (Protocol::Smtp, Security::Tls) => 465,
        (Protocol::Smtp, Security::StartTls) => 587,
        (Protocol::Pop3, Security::Tls) => 995,
        (Protocol::Pop3, Security::StartTls) => 110,
    }
}

/// Whether `path` can name an HTML signature: relative, at most 255 characters, no control
/// characters, no `..` or empty step, and an `.html` or `.htm` file.
pub fn is_signature_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    !path.is_empty()
        && path.chars().count() <= 255
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|step| !step.is_empty() && step != "." && step != "..")
        && (lower.ends_with(".html") || lower.ends_with(".htm"))
}

/// A field name that looks like it holds a secret.
fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["pass", "secret", "token", "credential"]
        .iter()
        .any(|word| name.contains(word))
}

fn secret_message(name: &str) -> String {
    format!(
        "{name} looks like a password or token: Sealwax keeps those in the keyring, never in \
         accounts.toml; remove this line"
    )
}

/// Why `host` can't be a server name, or nothing.
fn host_fault(host: &str) -> Option<&'static str> {
    if host.is_empty() {
        return Some("host can't be empty");
    }
    if host.contains("://") {
        return Some("host is a server name like imap.example.test, without imap:// or ssl://");
    }
    if host.parse::<Ipv6Addr>().is_ok() {
        return None;
    }
    if host.contains(':') {
        return Some("the port goes in port = …, not in host");
    }
    let name = host.len() <= 253
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
        && !host.starts_with(['.', '-'])
        && !host.contains("..");
    (!name).then_some("host is a server name like imap.example.test")
}

/// Text that may stand in a name or a username: no control characters, no surrounding space.
fn is_plain_text(text: &str) -> bool {
    !text.is_empty() && text.trim() == text && !text.chars().any(char::is_control)
}

fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Imap => "imap",
        Protocol::Smtp => "smtp",
        Protocol::Pop3 => "pop3",
    }
}

fn security_name(security: Security) -> &'static str {
    match security {
        Security::Tls => "tls",
        Security::StartTls => "starttls",
    }
}

fn auth_name(auth: AuthMethod) -> &'static str {
    match auth {
        AuthMethod::Password => "password",
        AuthMethod::OAuth(OAuthProvider::Microsoft) => "microsoft",
    }
}

/// Why an account can't be written. Its message never quotes a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFault {
    Key,
    Name,
    Label,
    Address,
    /// The account's or an identity's address can't be sent from: Sealwax sends without
    /// SMTPUTF8 (ADR-007).
    NotSendable,
    Username,
    /// The incoming or the SMTP server's host, and why.
    Host(Protocol, &'static str),
    Port(Protocol),
    /// The POP3 policy, and why.
    Pop3(&'static str),
    /// The file written didn't read back as the accounts it should hold.
    NotReadBack,
    /// An HTML signature's path isn't one `signature_html` takes.
    SignaturePath,
}

impl fmt::Display for AccountFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key => f.write_str("the key takes a–z, 0–9, - and _ only, at most 40 characters"),
            Self::Name => f.write_str("the name is empty or has control characters"),
            Self::Label => f.write_str("the label is empty or has control characters"),
            Self::Address => f.write_str("the address is not an email address"),
            Self::NotSendable => f.write_str(
                "Sealwax can't send from this address yet: before the @ it may hold only English \
                 letters, digits and symbols such as . _ + -",
            ),
            Self::Username => f.write_str("the username is empty or has control characters"),
            Self::Host(protocol, fault) => write!(f, "{}: {fault}", protocol_name(*protocol)),
            Self::Port(protocol) => write!(f, "{}: port 0 is not a port", protocol_name(*protocol)),
            Self::Pop3(fault) => write!(f, "pop3: {fault}"),
            Self::NotReadBack => f.write_str("the entry did not read back as written"),
            Self::SignaturePath => {
                f.write_str("the signature's file isn't an .html file in the signatures folder")
            }
        }
    }
}

impl std::error::Error for AccountFault {}

/// Checks an account the assistant is about to write.
pub fn check(account: &AccountSettings) -> Result<(), AccountFault> {
    if !is_valid_key(account.key.as_str()) {
        return Err(AccountFault::Key);
    }
    if !is_plain_text(&account.name) {
        return Err(AccountFault::Name);
    }
    if account
        .label
        .as_deref()
        .is_some_and(|label| !is_plain_text(label))
    {
        return Err(AccountFault::Label);
    }
    if !looks_like_address(&account.address) {
        return Err(AccountFault::Address);
    }
    if !is_sendable(&account.address)
        || account
            .identities
            .iter()
            .any(|identity| !is_sendable(&identity.address))
    {
        return Err(AccountFault::NotSendable);
    }
    if !is_plain_text(&account.username) {
        return Err(AccountFault::Username);
    }
    for (protocol, endpoint) in [
        (account.receive.protocol(), &account.incoming),
        (Protocol::Smtp, &account.smtp),
    ] {
        if let Some(fault) = host_fault(&endpoint.host) {
            return Err(AccountFault::Host(protocol, fault));
        }
        if endpoint.port == 0 {
            return Err(AccountFault::Port(protocol));
        }
    }
    if let Receive::Pop3(policy) = account.receive
        && let Some(fault) = policy_fault(policy)
    {
        return Err(AccountFault::Pop3(fault));
    }
    Ok(())
}

/// Why a POP3 account's policy can't be written, or nothing.
fn policy_fault(policy: Pop3Policy) -> Option<&'static str> {
    match policy.delete_after_days {
        Some(_) if !policy.leave_on_server => {
            Some("delete_after_days needs leave_on_server = true")
        }
        Some(days) if !(1..=MAX_DELETE_AFTER_DAYS).contains(&days) => {
            Some("delete_after_days is from 1 to 3650")
        }
        _ => None,
    }
}

/// The server name an example in a message uses.
fn example_host(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Imap => "imap.example.test",
        Protocol::Smtp => "smtp.example.test",
        Protocol::Pop3 => "pop.example.test",
    }
}

/// One account of the file, where it is.
struct Located {
    account: AccountSettings,
    /// The line of its `[[account]]`.
    line: usize,
    key_span: Range<usize>,
    address_span: Range<usize>,
    /// Where each identity's address is, in the order of `account.identities`.
    identity_spans: Vec<Range<usize>>,
}

impl Located {
    /// The account's own address and its identities', each with where it is written.
    fn addresses(&self) -> impl Iterator<Item = (&str, &Range<usize>)> {
        std::iter::once((self.account.address.as_str(), &self.address_span)).chain(
            self.account
                .identities
                .iter()
                .map(|identity| identity.address.as_str())
                .zip(&self.identity_spans),
        )
    }
}

/// An identity as read, before its name defaults to the account's.
struct ReadIdentity {
    name: Option<String>,
    address: String,
    address_span: Range<usize>,
    signature: Option<String>,
    signature_html: Option<String>,
}

/// Collects the mistakes of one `accounts.toml` read.
struct Reader<'t> {
    text: &'t str,
    errors: Vec<FileError>,
}

impl Reader<'_> {
    fn error(&mut self, span: Range<usize>, message: impl Into<String>) {
        self.errors
            .push(FileError::spanned(self.text, span, message));
    }

    fn line(&self, span: &Range<usize>) -> usize {
        FileError::at(self.text, span.start, "").line
    }

    /// Reports every field with a secret's name inside `table`, at any depth.
    fn secrets_in(&mut self, table: &DeTable<'_>) {
        for (key, value) in entries(table) {
            let name = key.get_ref().as_ref();
            if is_secret_name(name) {
                self.error(key.span(), secret_message(name));
            } else if let Some(inner) = value.get_ref().as_table() {
                self.secrets_in(inner);
            }
        }
    }

    /// The trimmed string in `value`, or a mistake that says `name` takes one.
    fn string(&mut self, name: &str, value: &Spanned<DeValue<'_>>) -> Option<String> {
        match value.get_ref().as_str() {
            Some(text) => Some(text.trim().to_owned()),
            None => {
                self.error(
                    value.span(),
                    format!(
                        "{name} takes a string in quotes, not {}",
                        article(value.get_ref().type_str())
                    ),
                );
                None
            }
        }
    }

    /// An HTML signature's file (issue #6): a relative path under the signatures folder, of at
    /// most 255 characters, without `..`, ending in `.html` or `.htm`.
    fn signature_html(&mut self, value: &Spanned<DeValue<'_>>) -> Option<String> {
        let Some(path) = value.get_ref().as_str() else {
            self.error(
                value.span(),
                format!(
                    "signature_html takes a file name in quotes, not {}",
                    article(value.get_ref().type_str())
                ),
            );
            return None;
        };
        let path = path.trim();
        if !is_signature_path(path) {
            self.error(
                value.span(),
                "signature_html takes an .html file in the signatures folder, such as \
                 \"work/signature.html\"",
            );
            return None;
        }
        Some(path.to_owned())
    }

    /// A signature: plain text, line breaks and tabs allowed, at most [`MAX_SIGNATURE_CHARS`];
    /// white space at the ends of lines and blank lines around it dropped, `None` when empty.
    fn signature(&mut self, value: &Spanned<DeValue<'_>>) -> Option<String> {
        let Some(text) = value.get_ref().as_str() else {
            self.error(
                value.span(),
                format!(
                    "signature takes a string in quotes, or in \"\"\" for several lines, not {}",
                    article(value.get_ref().type_str())
                ),
            );
            return None;
        };
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            self.error(value.span(), "signature can't hold control characters");
            return None;
        }
        if text.chars().count() > MAX_SIGNATURE_CHARS {
            self.error(
                value.span(),
                format!("signature is at most {MAX_SIGNATURE_CHARS} characters"),
            );
            return None;
        }
        let text = text.replace("\r\n", "\n");
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        let first = lines.iter().position(|line| !line.is_empty())?;
        let last = lines.iter().rposition(|line| !line.is_empty())?;
        Some(lines[first..=last].join("\n"))
    }

    /// The `[[account.identity]]` tables of an account.
    fn identities(&mut self, value: &Spanned<DeValue<'_>>) -> Vec<ReadIdentity> {
        let tables = value.get_ref().as_array().filter(|array| {
            array
                .iter()
                .all(|element| self.text[element.span()].starts_with("[["))
        });
        let Some(tables) = tables else {
            self.error(
                value.span(),
                "identities are written as [[account.identity]] tables, one per address",
            );
            return Vec::new();
        };
        tables
            .iter()
            .filter_map(|element| {
                let table = element.get_ref().as_table()?;
                self.identity(table, element.span())
            })
            .collect()
    }

    fn identity(&mut self, table: &DeTable<'_>, header: Range<usize>) -> Option<ReadIdentity> {
        let errors_before = self.errors.len();
        let mut name = None;
        let mut address = None;
        let mut signature = None;
        let mut signature_html = None;
        for (field, value) in entries(table) {
            let field_name = field.get_ref().as_ref();
            let span = value.span();
            match field_name {
                "name" => {
                    if let Some(text) = self.string("name", value) {
                        if is_plain_text(&text) {
                            name = Some(text);
                        } else {
                            self.error(
                                span,
                                "name can't be empty or hold control characters; leave the \
                                 line out to use the account's name",
                            );
                        }
                    }
                }
                "address" => {
                    if let Some(text) = self.string("address", value) {
                        if looks_like_address(&text) {
                            address = Some((text, span));
                        } else {
                            self.error(
                                span,
                                "address is an email address, like ada@work.example.test",
                            );
                        }
                    }
                }
                "signature" => signature = self.signature(value),
                "signature_html" => signature_html = self.signature_html(value),
                _ if is_secret_name(field_name) => {
                    self.error(field.span(), secret_message(field_name));
                }
                _ => {
                    self.error(
                        field.span(),
                        format!(
                            "unknown setting {field_name} in [[account.identity]]{}",
                            did_you_mean(field_name, IDENTITY_SETTINGS)
                        ),
                    );
                    if let Some(inner) = value.get_ref().as_table() {
                        self.secrets_in(inner);
                    }
                }
            }
        }
        if address.is_none() && !table.contains_key("address") {
            self.error(
                header,
                "this identity has no address, the one it sends from; add one, like \
                 address = \"ada@work.example.test\"",
            );
        }
        if self.errors.len() != errors_before {
            return None;
        }
        let (address, address_span) = address?;
        Some(ReadIdentity {
            name,
            address,
            address_span,
            signature,
            signature_html,
        })
    }

    fn account(&mut self, table: &DeTable<'_>, header: Range<usize>) -> Option<Located> {
        let errors_before = self.errors.len();
        let mut key = None;
        let mut name = None;
        let mut label = None;
        let mut color = None;
        let mut address = None;
        let mut username = None;
        let mut auth = AuthMethod::Password;
        let mut incoming: Option<(Endpoint, Receive)> = None;
        let mut incoming_key: Option<&str> = None;
        let mut smtp = None;
        let mut signature = None;
        let mut signature_html = None;
        let mut pgp = PgpSettings::default();
        let mut identities = Vec::new();
        for (field, value) in entries(table) {
            let field_name = field.get_ref().as_ref();
            let span = value.span();
            match field_name {
                "key" => {
                    if let Some(text) = self.string("key", value) {
                        if let Ok(parsed) = AccountKey::parse(&text) {
                            key = Some((parsed, span));
                        } else {
                            self.error(
                                span,
                                "key takes a–z, 0–9, - and _ only, at most 40 characters, \
                                 like key = \"work\"",
                            );
                        }
                    }
                }
                "name" => {
                    if let Some(text) = self.string("name", value) {
                        if is_plain_text(&text) {
                            name = Some(text);
                        } else {
                            self.error(span, "name can't be empty or hold control characters");
                        }
                    }
                }
                "label" => {
                    if let Some(text) = self.string("label", value) {
                        if is_plain_text(&text) {
                            label = Some(text);
                        } else {
                            self.error(
                                span,
                                "label can't be empty or hold control characters; leave the \
                                 line out to show the address",
                            );
                        }
                    }
                }
                "color" => {
                    if let Some(text) = self.string("color", value) {
                        match AccountColor::from_name(&text) {
                            Some(found) => color = Some(found),
                            None => self.error(span, color_message()),
                        }
                    }
                }
                "address" => {
                    if let Some(text) = self.string("address", value) {
                        if looks_like_address(&text) {
                            address = Some((text, span));
                        } else {
                            self.error(span, "address is an email address, like ada@example.test");
                        }
                    }
                }
                "username" => {
                    if let Some(text) = self.string("username", value) {
                        if is_plain_text(&text) {
                            username = Some(text);
                        } else {
                            self.error(
                                span,
                                "username can't be empty; leave the line out to log in with the \
                                 address",
                            );
                        }
                    }
                }
                "auth" => match value.get_ref().as_str() {
                    Some("password") => auth = AuthMethod::Password,
                    Some("microsoft") => auth = AuthMethod::OAuth(OAuthProvider::Microsoft),
                    _ => self.error(
                        span,
                        "auth is \"password\" (a password or app password, kept in the keyring) \
                         or \"microsoft\" (Sign in with Microsoft)",
                    ),
                },
                "imap" | "pop3" => {
                    let pop3 = field_name == "pop3";
                    let protocol = if pop3 { Protocol::Pop3 } else { Protocol::Imap };
                    let read = self.server(protocol, value);
                    if let Some(first) = incoming_key {
                        self.error(
                            field.span(),
                            format!(
                                "this account already has [account.{first}]: an account \
                                 receives its mail by IMAP or by POP3, so keep one of the two \
                                 tables"
                            ),
                        );
                    } else {
                        incoming_key = Some(field_name);
                        incoming = read.map(|(endpoint, policy)| {
                            let receive = if pop3 {
                                Receive::Pop3(policy)
                            } else {
                                Receive::Imap
                            };
                            (endpoint, receive)
                        });
                    }
                }
                "smtp" => smtp = self.server(Protocol::Smtp, value).map(|(server, _)| server),
                "signature" => signature = self.signature(value),
                "signature_html" => signature_html = self.signature_html(value),
                "pgp_sign" | "pgp_encrypt" => match value.get_ref() {
                    DeValue::Boolean(on) if field_name == "pgp_sign" => pgp.sign = *on,
                    DeValue::Boolean(on) => pgp.encrypt = *on,
                    other => self.error(
                        span,
                        format!(
                            "{field_name} is true (a new message starts with {} on) or false, \
                             not {}",
                            if field_name == "pgp_sign" {
                                "Sign"
                            } else {
                                "Encrypt"
                            },
                            article(other.type_str())
                        ),
                    ),
                },
                "pgp_key" => {
                    if let Some(text) = self.string("pgp_key", value) {
                        let fingerprint: String =
                            text.chars().filter(|c| !c.is_whitespace()).collect();
                        if crate::compose::protect::is_fingerprint(&fingerprint) {
                            pgp.key = Some(fingerprint.to_ascii_uppercase());
                        } else {
                            self.error(
                                span,
                                "pgp_key is your key's full fingerprint, 40 hexadecimal digits \
                                 as gpg --list-secret-keys shows it",
                            );
                        }
                    }
                }
                "identity" => identities = self.identities(value),
                _ if is_secret_name(field_name) => {
                    self.error(field.span(), secret_message(field_name));
                }
                _ => {
                    self.error(
                        field.span(),
                        format!(
                            "unknown setting {field_name} in [[account]]{}",
                            did_you_mean(field_name, ACCOUNT_SETTINGS)
                        ),
                    );
                    if let Some(inner) = value.get_ref().as_table() {
                        self.secrets_in(inner);
                    }
                }
            }
        }
        let missing: [(bool, &str); 5] = [
            (
                key.is_none() && !table.contains_key("key"),
                "this account has no key, its name in the keyring and the store; add one of \
                 your choosing, like key = \"work\"",
            ),
            (
                name.is_none() && !table.contains_key("name"),
                "this account has no name, the sender name on its mail; add one, like \
                 name = \"Ada Lovelace\"",
            ),
            (
                address.is_none() && !table.contains_key("address"),
                "this account has no address; add one, like address = \"ada@example.test\"",
            ),
            (
                incoming_key.is_none(),
                "this account has no incoming server: add an [account.imap] table, or \
                 [account.pop3] for POP3",
            ),
            (
                smtp.is_none() && !table.contains_key("smtp"),
                "this account has no [account.smtp] table with its outgoing server",
            ),
        ];
        for (is_missing, message) in missing {
            if is_missing {
                self.error(header.clone(), message);
            }
        }
        if self.errors.len() != errors_before {
            return None;
        }
        let ((key, key_span), name, (address, address_span), (incoming, receive), smtp) =
            (key?, name?, address?, incoming?, smtp?);
        let identity_spans = identities
            .iter()
            .map(|identity: &ReadIdentity| identity.address_span.clone())
            .collect();
        let identities = identities
            .into_iter()
            .map(|identity| Identity {
                name: identity.name.unwrap_or_else(|| name.clone()),
                address: identity.address,
                signature: identity.signature,
                signature_html: identity.signature_html,
            })
            .collect();
        Some(Located {
            line: self.line(&header),
            account: AccountSettings {
                key,
                name,
                label,
                color,
                username: username.unwrap_or_else(|| address.clone()),
                address,
                incoming,
                receive,
                smtp,
                auth,
                signature,
                signature_html,
                identities,
                pgp,
            },
            key_span,
            address_span,
            identity_spans,
        })
    }

    /// An `[account.imap]`, `[account.pop3]` or `[account.smtp]` table: the server, and for
    /// POP3 what happens on it to downloaded mail (the default policy for the others).
    fn server(
        &mut self,
        protocol: Protocol,
        value: &Spanned<DeValue<'_>>,
    ) -> Option<(Endpoint, Pop3Policy)> {
        let section = format!("account.{}", protocol_name(protocol));
        let Some(table) = value.get_ref().as_table() else {
            self.error(
                value.span(),
                format!("{} is a table, like [{section}]", protocol_name(protocol)),
            );
            return None;
        };
        let errors_before = self.errors.len();
        let mut host = None;
        let mut port = None;
        let mut security = Security::Tls;
        let mut certificate = None;
        let mut policy = Pop3Policy::default();
        let mut days_span = None;
        for (field, value) in entries(table) {
            let name = field.get_ref().as_ref();
            let span = value.span();
            match name {
                "host" => {
                    if let Some(text) = self.string("host", value) {
                        match host_fault(&text) {
                            None => host = Some(text),
                            Some(fault) => self.error(span, fault),
                        }
                    }
                }
                "port" => match value.get_ref() {
                    DeValue::Integer(integer) => {
                        match i64::from_str_radix(integer.as_str(), integer.radix())
                            .ok()
                            .and_then(|number| u16::try_from(number).ok())
                            .filter(|number| *number != 0)
                        {
                            Some(number) => port = Some(number),
                            None => self.error(
                                span,
                                format!(
                                    "port is a whole number from 1 to 65535, not {}",
                                    integer.as_str()
                                ),
                            ),
                        }
                    }
                    other => self.error(
                        span,
                        format!(
                            "port is a whole number from 1 to 65535, not {}",
                            article(other.type_str())
                        ),
                    ),
                },
                "security" => match value.get_ref().as_str() {
                    Some("tls") => security = Security::Tls,
                    Some("starttls") => security = Security::StartTls,
                    other => {
                        let never = matches!(
                            other,
                            Some("none" | "plain" | "plaintext" | "off" | "insecure")
                        );
                        let hint = if never {
                            "; Sealwax never connects without TLS"
                        } else {
                            ""
                        };
                        self.error(
                            span,
                            format!(
                                "security is \"tls\" (TLS from the first byte) or \"starttls\"{hint}"
                            ),
                        );
                    }
                },
                "certificate" => {
                    if let Some(text) = self.string("certificate", value) {
                        match CertificatePin::parse(&text) {
                            Some(pin) => certificate = Some((pin, span)),
                            None => self.error(
                                span,
                                "certificate is the SHA-256 fingerprint of the server's \
                                 certificate: 32 pairs of hexadecimal digits, like \"AB:CD:…\"",
                            ),
                        }
                    }
                }
                "leave_on_server" if protocol == Protocol::Pop3 => match value.get_ref() {
                    DeValue::Boolean(on) => policy.leave_on_server = *on,
                    other => self.error(
                        span,
                        format!(
                            "leave_on_server is true (downloaded mail stays on the server) or \
                             false (it is deleted there once downloaded), not {}",
                            article(other.type_str())
                        ),
                    ),
                },
                "delete_after_days" if protocol == Protocol::Pop3 => {
                    if let Some(days) = self.days(value) {
                        policy.delete_after_days = Some(days);
                        days_span = Some(span);
                    }
                }
                _ if POP3_SETTINGS.contains(&name) => self.error(
                    field.span(),
                    format!(
                        "{name} belongs in [account.pop3], for an account that downloads its \
                         mail by POP3"
                    ),
                ),
                _ if is_secret_name(name) => self.error(field.span(), secret_message(name)),
                _ => {
                    let known = ENDPOINT_SETTINGS.into_iter().chain(
                        POP3_SETTINGS
                            .into_iter()
                            .filter(|_| protocol == Protocol::Pop3),
                    );
                    self.error(
                        field.span(),
                        format!(
                            "unknown setting {name} in [{section}]{}",
                            did_you_mean(name, known)
                        ),
                    );
                    if let Some(inner) = value.get_ref().as_table() {
                        self.secrets_in(inner);
                    }
                }
            }
        }
        if let Some(span) = days_span
            && !policy.leave_on_server
        {
            self.error(
                span,
                "delete_after_days needs leave_on_server = true: with false, mail is deleted \
                 from the server as soon as it is downloaded; keep one of the two lines",
            );
        }
        if let (Some((_, span)), Some(host)) = (&certificate, &host)
            && !is_loopback(host)
        {
            self.error(
                span.clone(),
                "certificate trusts a server's own certificate only on this computer \
                 (localhost, 127.0.0.1 or ::1); a server elsewhere needs a certificate your \
                 system trusts",
            );
        }
        if host.is_none() && !table.contains_key("host") {
            self.error(
                value.span(),
                format!(
                    "[{section}] has no host; add one, like host = \"{}\"",
                    example_host(protocol)
                ),
            );
        }
        if self.errors.len() != errors_before {
            return None;
        }
        let endpoint = Endpoint {
            host: host?,
            port: port.unwrap_or_else(|| default_port(protocol, security)),
            security,
            certificate: certificate.map(|(pin, _)| pin),
        };
        Some((endpoint, policy))
    }

    /// `delete_after_days`: whole days from 1 to [`MAX_DELETE_AFTER_DAYS`].
    fn days(&mut self, value: &Spanned<DeValue<'_>>) -> Option<u32> {
        let number = match value.get_ref() {
            DeValue::Integer(integer) => {
                i64::from_str_radix(integer.as_str(), integer.radix()).ok()
            }
            _ => None,
        };
        if let Some(days) = number.and_then(|days| u32::try_from(days).ok())
            && (1..=MAX_DELETE_AFTER_DAYS).contains(&days)
        {
            return Some(days);
        }
        let (shown, hint) = match value.get_ref() {
            DeValue::Integer(integer) if number == Some(0) => (
                integer.as_str().to_owned(),
                "; to delete mail as soon as it is downloaded, set leave_on_server = false instead",
            ),
            DeValue::Integer(integer) => (integer.as_str().to_owned(), ""),
            other => (
                article(other.type_str()),
                "; leave the line out to keep downloaded mail on the server",
            ),
        };
        self.error(
            value.span(),
            format!(
                "delete_after_days is a whole number of days from 1 to {MAX_DELETE_AFTER_DAYS}, \
                 not {shown}{hint}"
            ),
        );
        None
    }

    /// Every key and every address once.
    fn duplicates(&mut self, accounts: &[Located]) {
        for (index, later) in accounts.iter().enumerate() {
            let earlier = &accounts[..index];
            if let Some(first) = earlier.iter().find(|a| a.account.key == later.account.key) {
                self.error(
                    later.key_span.clone(),
                    format!(
                        "key \"{}\" is also the key of the account on line {}; every account \
                         needs its own",
                        later.account.key, first.line
                    ),
                );
            }
            let mut own: Vec<&str> = Vec::new();
            for (address, span) in later.addresses() {
                let elsewhere = earlier.iter().find_map(|account| {
                    account
                        .addresses()
                        .position(|(known, _)| same(known, address))
                        .map(|position| (account.line, position == 0))
                });
                if let Some((line, its_own)) = elsewhere {
                    let what = if its_own {
                        "the address"
                    } else {
                        "an identity"
                    };
                    self.error(
                        span.clone(),
                        format!("this address is also {what} of the account on line {line}"),
                    );
                } else if own.iter().any(|known| same(known, address)) {
                    self.error(
                        span.clone(),
                        "this address is already one of this account's; every identity needs \
                         an address of its own",
                    );
                }
                own.push(address);
            }
        }
    }
}

/// What `color` takes, every name listed.
fn color_message() -> String {
    let names: Vec<String> = AccountColor::ALL
        .iter()
        .map(|color| format!("\"{}\"", color.name()))
        .collect();
    let (last, rest) = names.split_last().expect("there are colours");
    format!("color is one of {} or {last}", rest.join(", "))
}

/// Where a setting written outside an account belongs.
fn section_of(name: &str) -> Option<&'static str> {
    if ACCOUNT_SETTINGS.contains(&name) {
        Some("an [[account]] table")
    } else if ENDPOINT_SETTINGS.contains(&name) {
        Some("[account.imap], [account.pop3] or [account.smtp]")
    } else if POP3_SETTINGS.contains(&name) {
        Some("[account.pop3]")
    } else {
        None
    }
}

fn read(text: &str) -> Result<Vec<Located>, Vec<FileError>> {
    let root = parse_toml(text).map_err(|error| vec![error])?;
    let mut reader = Reader {
        text,
        errors: Vec::new(),
    };
    let mut accounts = Vec::new();
    for (key, value) in entries(root.get_ref()) {
        let name = key.get_ref().as_ref();
        if name == "account" {
            let tables = value.get_ref().as_array().filter(|array| {
                array
                    .iter()
                    .all(|element| text[element.span()].starts_with("[["))
            });
            let Some(tables) = tables else {
                reader.error(
                    value.span(),
                    "accounts are written as [[account]] tables, one per account",
                );
                continue;
            };
            for element in tables.iter() {
                if let Some(table) = element.get_ref().as_table()
                    && let Some(account) = reader.account(table, element.span())
                {
                    accounts.push(account);
                }
            }
        } else if is_secret_name(name) {
            reader.error(key.span(), secret_message(name));
        } else if let Some(section) = section_of(name) {
            reader.error(
                key.span(),
                format!("{name} belongs in {section}; put it under that line"),
            );
        } else {
            let kind = if value.get_ref().as_table().is_some() {
                "section"
            } else {
                "setting"
            };
            reader.error(
                key.span(),
                format!("unknown {kind} {name}{}", did_you_mean(name, ["account"])),
            );
            if let Some(inner) = value.get_ref().as_table() {
                reader.secrets_in(inner);
            }
        }
    }
    reader.duplicates(&accounts);
    if reader.errors.is_empty() {
        Ok(accounts)
    } else {
        reader
            .errors
            .sort_by_key(|error| (error.line, error.column));
        Err(reader.errors)
    }
}

/// Reads `accounts.toml`. Every mistake is listed, in the order of the file; an empty file has
/// no accounts.
pub fn parse(text: &str) -> Result<Vec<AccountSettings>, Vec<FileError>> {
    read(text).map(|accounts| accounts.into_iter().map(|a| a.account).collect())
}

/// Why the assistant could not change `accounts.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The file has mistakes; it is fixed by hand first, since a file with mistakes isn't applied.
    Invalid(Vec<FileError>),
    /// The account to add is incomplete.
    Account(AccountFault),
    KeyExists,
    AddressExists,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(errors) => write!(
                f,
                "accounts.toml has {} mistake(s); the first: {}",
                errors.len(),
                errors.first().map(ToString::to_string).unwrap_or_default()
            ),
            Self::Account(fault) => write!(f, "the account can't be written: {fault}"),
            Self::KeyExists => f.write_str("accounts.toml already has an account with this key"),
            Self::AddressExists => {
                f.write_str("accounts.toml already has an account with this address")
            }
        }
    }
}

impl std::error::Error for EditError {}

/// The `[[account]]` entry the assistant writes, with every field spelled out.
pub fn entry(account: &AccountSettings) -> String {
    let endpoint = |endpoint: &Endpoint| {
        let mut table = Table::new();
        table["host"] = value(&endpoint.host);
        table["port"] = value(i64::from(endpoint.port));
        table["security"] = value(security_name(endpoint.security));
        if let Some(pin) = endpoint.certificate {
            table["certificate"] = value(pin.to_string());
        }
        table
    };
    let mut incoming = endpoint(&account.incoming);
    if let Receive::Pop3(policy) = account.receive {
        incoming["leave_on_server"] = value(policy.leave_on_server);
        if let Some(days) = policy.delete_after_days {
            incoming["delete_after_days"] = value(i64::from(days));
        }
    }
    let mut table = Table::new();
    table["key"] = value(account.key.as_str());
    table["name"] = value(&account.name);
    table["address"] = value(&account.address);
    if let Some(label) = &account.label {
        table["label"] = value(label);
    }
    if let Some(color) = account.color {
        table["color"] = value(color.name());
    }
    table["username"] = value(&account.username);
    table["auth"] = value(auth_name(account.auth));
    table[protocol_name(account.receive.protocol())] = Item::Table(incoming);
    table["smtp"] = Item::Table(endpoint(&account.smtp));
    let mut tables = ArrayOfTables::new();
    tables.push(table);
    let mut document = DocumentMut::new();
    document["account"] = Item::ArrayOfTables(tables);
    document.to_string()
}

/// `text` with `account` appended as a new entry. The text before it is kept byte for byte;
/// a file with mistakes, or one that already has the key or the address, is refused.
pub fn add(text: &str, account: &AccountSettings) -> Result<String, EditError> {
    check(account).map_err(EditError::Account)?;
    let before = parse(text).map_err(EditError::Invalid)?;
    if before.iter().any(|known| known.key == account.key) {
        return Err(EditError::KeyExists);
    }
    if before
        .iter()
        .any(|known| same(&known.address, &account.address))
    {
        return Err(EditError::AddressExists);
    }
    let mut output = text.to_owned();
    if !output.trim().is_empty() {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if !output.ends_with("\n\n") {
            output.push('\n');
        }
    }
    output.push_str(&entry(account));
    let after = parse(&output).map_err(EditError::Invalid)?;
    let expected: Vec<_> = before.into_iter().chain([account.clone()]).collect();
    if after != expected {
        return Err(EditError::Account(AccountFault::NotReadBack));
    }
    Ok(output)
}

/// Splits a table's leading comments at the last blank line: what is above it stays in the
/// file (a file header, a note about the previous account), what is below it goes with the
/// table.
fn split_prefix(prefix: &str) -> (&str, &str) {
    let mut end = 0;
    let mut offset = 0;
    for line in prefix.split_inclusive('\n') {
        offset += line.len();
        if line.trim().is_empty() {
            end = offset;
        }
    }
    prefix.split_at(end)
}

/// Joins comments kept from a removed table to the text that follows them, with at most one
/// blank line between.
fn join(kept: &str, next: &str) -> String {
    let next = if kept.ends_with("\n\n") || kept == "\n" {
        next.trim_start_matches('\n')
    } else {
        next
    };
    format!("{kept}{next}")
}

/// `text` without the account with `key`, or `None` when the file has no such account. Comments
/// right above its `[[account]]` and inside it go with it; everything else stays. A file that
/// isn't valid TOML is refused; other mistakes don't stop a removal.
pub fn remove(text: &str, key: &AccountKey) -> Result<Option<String>, EditError> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|_| EditError::Invalid(parse_toml(text).err().into_iter().collect()))?;
    let trailing = document.trailing().as_str().unwrap_or_default().to_owned();
    let Some(tables) = document
        .get_mut("account")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return Ok(None);
    };
    // The key as `parse` reads it: white space around it is no part of it (found by fuzzing,
    // ML-093: such an entry stayed while the account's data went).
    let Some(index) = tables.iter().position(|table| {
        table.get("key").and_then(Item::as_str).map(str::trim) == Some(key.as_str())
    }) else {
        return Ok(None);
    };
    let removed = tables.remove(index);
    let prefix = removed
        .decor()
        .prefix()
        .and_then(|raw| raw.as_str())
        .unwrap_or_default();
    let (kept, _) = split_prefix(prefix);
    let kept = kept.to_owned();
    match tables.get_mut(index) {
        Some(next) => {
            let next_prefix = next
                .decor()
                .prefix()
                .and_then(|raw| raw.as_str())
                .unwrap_or_default()
                .to_owned();
            next.decor_mut().set_prefix(join(&kept, &next_prefix));
        }
        None => {
            if tables.is_empty() {
                document.remove("account");
            }
            let trailing = if trailing.is_empty() {
                let kept = kept.trim_end();
                if kept.is_empty() {
                    String::new()
                } else {
                    format!("{kept}\n")
                }
            } else {
                join(&kept, &trailing)
            };
            document.set_trailing(trailing);
        }
    }
    let output = document.to_string();
    let output = if output.trim().is_empty() {
        String::new()
    } else {
        output
    };
    Ok(Some(output))
}

/// `text` with the account `key`'s `label` and `color` set as given, `None` taking the line
/// out; `None` when the file has no such account. A value already written is replaced in place,
/// its comment kept; a new one goes after the address (the colour after the label). Everything
/// else stays byte for byte. A file with mistakes, or a label that couldn't be read back, is
/// refused.
pub fn relabel(
    text: &str,
    key: &AccountKey,
    label: Option<&str>,
    color: Option<AccountColor>,
) -> Result<Option<String>, EditError> {
    if label.is_some_and(|label| !is_plain_text(label)) {
        return Err(EditError::Account(AccountFault::Label));
    }
    let before = parse(text).map_err(EditError::Invalid)?;
    if !before.iter().any(|account| &account.key == key) {
        return Ok(None);
    }
    let mut document: DocumentMut = text
        .parse()
        .map_err(|_| EditError::Invalid(parse_toml(text).err().into_iter().collect()))?;
    let Some(table) = document
        .get_mut("account")
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|tables| {
            tables.iter_mut().find(|table| {
                table.get("key").and_then(Item::as_str).map(str::trim) == Some(key.as_str())
            })
        })
    else {
        return Ok(None);
    };
    set_value(table, "label", label, "address");
    let after_label = if table.contains_key("label") {
        "label"
    } else {
        "address"
    };
    set_value(table, "color", color.map(AccountColor::name), after_label);
    let output = document.to_string();
    let expected: Vec<AccountSettings> = before
        .into_iter()
        .map(|mut account| {
            if &account.key == key {
                account.label = label.map(str::to_owned);
                account.color = color;
            }
            account
        })
        .collect();
    if parse(&output).map_err(EditError::Invalid)? != expected {
        return Err(EditError::Account(AccountFault::NotReadBack));
    }
    Ok(Some(output))
}

/// `text` with the HTML signature of `address`, the account `key`'s own address or one of its
/// identities', set to `path` (`None` takes the line out); `None` when the file has no such
/// account or address. A value already written is replaced in place, its comment kept; a new
/// one goes after the address's `signature`, else after its address. Everything else stays byte
/// for byte. A file with mistakes, or one that doesn't read back, is refused.
pub fn set_signature_html(
    text: &str,
    key: &AccountKey,
    address: &str,
    path: Option<&str>,
) -> Result<Option<String>, EditError> {
    if path.is_some_and(|path| !is_signature_path(path)) {
        return Err(EditError::Account(AccountFault::SignaturePath));
    }
    let before = parse(text).map_err(EditError::Invalid)?;
    let Some(account) = before.iter().find(|account| &account.key == key) else {
        return Ok(None);
    };
    let own = crate::address::same(&account.address, address);
    let identity = account
        .identities
        .iter()
        .position(|identity| crate::address::same(&identity.address, address));
    if !own && identity.is_none() {
        return Ok(None);
    }
    let mut document: DocumentMut = text
        .parse()
        .map_err(|_| EditError::Invalid(parse_toml(text).err().into_iter().collect()))?;
    let Some(table) = document
        .get_mut("account")
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|tables| {
            tables.iter_mut().find(|table| {
                table.get("key").and_then(Item::as_str).map(str::trim) == Some(key.as_str())
            })
        })
    else {
        return Ok(None);
    };
    let target = if own {
        Some(table)
    } else {
        table
            .get_mut("identity")
            .and_then(Item::as_array_of_tables_mut)
            .and_then(|identities| {
                identities.iter_mut().find(|identity| {
                    identity
                        .get("address")
                        .and_then(Item::as_str)
                        .is_some_and(|known| crate::address::same(known.trim(), address))
                })
            })
    };
    let Some(target) = target else {
        return Ok(None);
    };
    let after = if target.contains_key("signature") {
        "signature"
    } else {
        "address"
    };
    set_value(target, "signature_html", path, after);
    let output = document.to_string();
    let expected: Vec<AccountSettings> = before
        .into_iter()
        .map(|mut account| {
            if &account.key == key {
                let path = path.map(str::to_owned);
                match identity.filter(|_| !own) {
                    Some(index) => account.identities[index].signature_html = path,
                    None => account.signature_html = path,
                }
            }
            account
        })
        .collect();
    if parse(&output).map_err(EditError::Invalid)? != expected {
        return Err(EditError::Account(AccountFault::NotReadBack));
    }
    Ok(Some(output))
}

/// Sets the string `name` in `table`: in place when it is there (its comment kept), else right
/// after `after`; `None` removes it.
fn set_value(table: &mut Table, name: &str, text: Option<&str>, after: &str) {
    let Some(text) = text else {
        table.remove(name);
        return;
    };
    if let Some(existing) = table.get_mut(name).and_then(Item::as_value_mut) {
        let decor = existing.decor().clone();
        *existing = Value::from(text);
        *existing.decor_mut() = decor;
        return;
    }
    let order: Vec<String> = table.iter().map(|(key, _)| key.to_owned()).collect();
    let rank = |key: &str| {
        let at = |key: &str| order.iter().position(|known| known == key);
        match at(key) {
            Some(index) => index as f64,
            None => at(after).map_or(order.len() as f64, |index| index as f64 + 0.5),
        }
    };
    table.insert(name, value(text));
    table.sort_values_by(|a, _, b, _| rank(a.get()).total_cmp(&rank(b.get())));
}

/// The `accounts.toml` the assistant starts from: a header and a commented example, so the
/// file explains itself to someone who opens it.
pub fn default_accounts_file() -> String {
    "# Sealwax accounts (accounts.toml). The setup assistant adds and removes accounts here, and\n\
     # you can edit the file by hand: Sealwax keeps your comments and layout when it writes, and\n\
     # applies the file when you save it. A mistake is shown with its line and column, and the\n\
     # previous accounts stay until the file is fixed. Passwords and tokens are never in this\n\
     # file; they live in the keyring.\n\
     #\n\
     # An account looks like this:\n\
     #\n\
     # [[account]]\n\
     # key = \"work\"                   # its name in the keyring and the store; keep it\n\
     # name = \"Ada Lovelace\"          # the sender name on your mail\n\
     # address = \"ada@example.test\"\n\
     # label = \"Work\"                 # optional: the account's name in Sealwax; the address\n\
     #                                # by default\n\
     # color = \"sage\"                 # optional: its colour in the sidebar: rose, clay,\n\
     #                                # olive, sage, teal, slate, iris or mauve\n\
     # username = \"ada@example.test\"  # optional: the address by default\n\
     # auth = \"password\"              # or \"microsoft\" for Sign in with Microsoft\n\
     # signature = \"\"\"\n\
     # Ada Lovelace\n\
     # Analytical Engines Ltd.\"\"\"       # optional: plain text under your mail, after \"-- \"\n\
     #\n\
     # [account.imap]\n\
     # host = \"imap.example.test\"\n\
     # port = 993                     # optional: 993 for \"tls\", 143 for \"starttls\"\n\
     # security = \"tls\"               # or \"starttls\"; Sealwax never connects without TLS\n\
     #\n\
     # [account.pop3]                 # in place of [account.imap], for an account that\n\
     #                                # downloads its mail by POP3 into folders on this\n\
     #                                # computer: Inbox, Sent, Drafts, Junk, Trash, Archive\n\
     # host = \"pop.example.test\"\n\
     # port = 995                     # optional: 995 for \"tls\", 110 for \"starttls\"\n\
     # security = \"tls\"\n\
     # leave_on_server = true         # optional: true keeps downloaded mail on the server;\n\
     #                                # false deletes it there once it is downloaded\n\
     # delete_after_days = 30         # optional: deletes it there 30 days after the\n\
     #                                # download; left out, it stays\n\
     #\n\
     # [account.smtp]\n\
     # host = \"smtp.example.test\"\n\
     # port = 465                     # optional: 465 for \"tls\", 587 for \"starttls\"\n\
     # security = \"tls\"\n\
     #\n\
     # [[account.identity]]           # optional: another address it sends from, one table each;\n\
     #                                # a reply goes out from the address it was sent to\n\
     # name = \"Ada at the board\"      # optional: the account's name by default\n\
     # address = \"board@example.test\"\n\
     # signature = \"Ada, for the board\"  # optional\n"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(key: &str, address: &str) -> AccountSettings {
        AccountSettings {
            key: AccountKey::parse(key).unwrap(),
            name: "Ada Lovelace".to_owned(),
            label: None,
            color: None,
            address: address.to_owned(),
            incoming: Endpoint {
                host: "imap.example.test".to_owned(),
                port: 993,
                security: Security::Tls,
                certificate: None,
            },
            receive: crate::account::Receive::Imap,
            smtp: Endpoint {
                host: "smtp.example.test".to_owned(),
                port: 587,
                security: Security::StartTls,
                certificate: None,
            },
            username: address.to_owned(),
            auth: AuthMethod::Password,
            signature: None,
            signature_html: None,
            identities: Vec::new(),
            pgp: Default::default(),
        }
    }

    fn errors(text: &str) -> Vec<String> {
        parse(text)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect()
    }

    const ONE: &str = "[[account]]\nkey = \"work\"\nname = \"Ada Lovelace\"\n\
                       address = \"ada@example.test\"\n\n[account.imap]\n\
                       host = \"imap.example.test\"\n\n[account.smtp]\n\
                       host = \"smtp.example.test\"\nsecurity = \"starttls\"\n";

    #[test]
    fn an_empty_file_has_no_accounts() {
        assert_eq!(parse(""), Ok(Vec::new()));
        assert_eq!(parse(&default_accounts_file()), Ok(Vec::new()));
    }

    #[test]
    fn reads_an_account_with_defaults() {
        let accounts = parse(ONE).unwrap();
        let expected = AccountSettings {
            smtp: Endpoint {
                port: 587,
                ..account("work", "ada@example.test").smtp
            },
            ..account("work", "ada@example.test")
        };
        assert_eq!(accounts, [expected]);
    }

    #[test]
    fn pgp_settings_are_read_and_checked() {
        let fingerprint = "0123 4567 89ab CDEF 0123  4567 89AB CDEF 0123 4567";
        let text = ONE.replace(
            "address = \"ada@example.test\"\n",
            &format!(
                "address = \"ada@example.test\"\npgp_sign = true\npgp_encrypt = false\n\
                 pgp_key = \"{fingerprint}\"\n"
            ),
        );
        let account = &parse(&text).unwrap()[0];
        assert_eq!(
            account.pgp,
            PgpSettings {
                sign: true,
                encrypt: false,
                key: Some("0123456789ABCDEF0123456789ABCDEF01234567".into()),
            }
        );
        assert_eq!(parse(ONE).unwrap()[0].pgp, PgpSettings::default());
        let wrong = ONE.replace(
            "address = \"ada@example.test\"\n",
            "address = \"ada@example.test\"\npgp_sign = \"yes\"\npgp_key = \"ABCD\"\n",
        );
        let errors = errors(&wrong);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("pgp_sign is true")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("full fingerprint")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_certificate_is_pinned_only_on_this_computer() {
        let pin = CertificatePin([0xab; 32]);
        let bridge = format!(
            "[[account]]\nkey = \"proton\"\nname = \"Ada\"\naddress = \"ada@proton.example.test\"\n\
             [account.imap]\nhost = \"127.0.0.1\"\nport = 1143\nsecurity = \"starttls\"\n\
             certificate = \"{pin}\"\n\
             [account.smtp]\nhost = \"localhost\"\nport = 1025\nsecurity = \"starttls\"\n\
             certificate = \"sha256:{}\"\n",
            pin.hex("").to_lowercase()
        );
        let account = &parse(&bridge).unwrap()[0];
        assert_eq!(account.incoming.certificate, Some(pin));
        assert_eq!(account.smtp.certificate, Some(pin));
        let written = entry(account);
        assert!(
            written.contains(&format!("certificate = \"{pin}\"")),
            "{written}"
        );
        assert_eq!(parse(&written).unwrap()[0], *account);

        let elsewhere = bridge.replace("127.0.0.1", "imap.example.test");
        assert_eq!(
            errors(&elsewhere),
            [
                "line 9, column 15: certificate trusts a server's own certificate only on this computer (localhost, \
              127.0.0.1 or ::1); a server elsewhere needs a certificate your system trusts"
            ]
        );
        let broken = bridge.replace(&pin.to_string(), "AB:CD");
        assert_eq!(
            errors(&broken),
            [
                "line 9, column 15: certificate is the SHA-256 fingerprint of the server's certificate: 32 pairs of \
              hexadecimal digits, like \"AB:CD:…\""
            ]
        );
        assert!(!entry(&account_without_pins(account)).contains("certificate"));
    }

    fn account_without_pins(account: &AccountSettings) -> AccountSettings {
        let mut account = account.clone();
        account.incoming.certificate = None;
        account.smtp.certificate = None;
        account
    }

    #[test]
    fn reads_every_field() {
        let text = "[[account]]\nkey = \"k3j5x8m2q9wa\"\nname = \" Ada \"\n\
                    address = \"ada@example.test\"\nusername = \"ada\"\nauth = \"microsoft\"\n\
                    [account.imap]\nhost = \"127.0.0.1\"\nport = 1993\nsecurity = \"starttls\"\n\
                    [account.smtp]\nhost = \"::1\"\nport = 0x24b\nsecurity = \"tls\"\n";
        let account = &parse(text).unwrap()[0];
        assert_eq!(account.key.as_str(), "k3j5x8m2q9wa");
        assert_eq!(account.name, "Ada");
        assert_eq!(account.username, "ada");
        assert_eq!(account.auth, AuthMethod::OAuth(OAuthProvider::Microsoft));
        assert_eq!(
            account.incoming,
            Endpoint {
                host: "127.0.0.1".to_owned(),
                port: 1993,
                security: Security::StartTls,
                certificate: None,
            }
        );
        assert_eq!(
            (account.smtp.host.as_str(), account.smtp.port),
            ("::1", 587)
        );
    }

    #[test]
    fn the_label_and_the_color_are_optional() {
        let account = &parse(ONE).unwrap()[0];
        assert_eq!((account.label.as_deref(), account.color), (None, None));
        assert_eq!(
            account.title(),
            "ada@example.test",
            "the address by default"
        );
        let text = ONE.replace(
            "name = \"Ada Lovelace\"\n",
            "name = \"Ada Lovelace\"\nlabel = \" Work \"\ncolor = \"Teal\"\n",
        );
        let account = &parse(&text).unwrap()[0];
        assert_eq!(account.title(), "Work");
        assert_eq!(account.color, Some(AccountColor::Teal));
        assert_eq!(account.name, "Ada Lovelace", "the sender name stays");
    }

    #[test]
    fn a_wrong_label_or_color_is_a_mistake() {
        let text = ONE.replace(
            "name = \"Ada Lovelace\"\n",
            "name = \"Ada Lovelace\"\nlabel = \"  \"\ncolor = \"green\"\n",
        );
        assert_eq!(
            errors(&text),
            [
                "line 4, column 9: label can't be empty or hold control characters; leave the \
                 line out to show the address",
                "line 5, column 9: color is one of \"rose\", \"clay\", \"olive\", \"sage\", \
                 \"teal\", \"slate\", \"iris\" or \"mauve\"",
            ]
        );
        let text = ONE.replace(
            "name = \"Ada Lovelace\"\n",
            "name = \"Ada\"\ncolour = \"sage\"\n",
        );
        let found = errors(&text);
        assert!(
            found[0].contains("unknown setting colour in [[account]]; did you mean color?"),
            "{found:?}"
        );
    }

    #[test]
    fn the_assistant_writes_the_label_and_the_color() {
        let mut new = account("k3j5x8m2q9wa", "ada@example.test");
        new.label = Some("Work".to_owned());
        new.color = Some(AccountColor::Iris);
        let written = add("", &new).unwrap();
        assert!(
            written.contains("label = \"Work\"\ncolor = \"iris\"\n"),
            "{written}"
        );
        assert_eq!(parse(&written), Ok(vec![new.clone()]));
        new.label = None;
        new.color = None;
        let written = add("", &new).unwrap();
        assert!(
            !written.contains("label") && !written.contains("color"),
            "{written}"
        );
        new.label = Some("Work\n".to_owned());
        assert_eq!(add("", &new), Err(EditError::Account(AccountFault::Label)));
    }

    #[test]
    fn relabel_sets_the_label_and_the_color_after_the_address() {
        let text = format!("# my accounts\n\n{ONE}");
        let work = AccountKey::parse("work").unwrap();
        let named = relabel(&text, &work, Some("Work"), Some(AccountColor::Sage))
            .unwrap()
            .unwrap();
        assert_eq!(
            named,
            text.replace(
                "address = \"ada@example.test\"\n",
                "address = \"ada@example.test\"\nlabel = \"Work\"\ncolor = \"sage\"\n"
            )
        );
        let account = &parse(&named).unwrap()[0];
        assert_eq!(
            (account.title(), account.color),
            ("Work", Some(AccountColor::Sage))
        );
        // Changed again: in place; taken out: the file as it was.
        let renamed = relabel(&named, &work, Some("Jobb"), Some(AccountColor::Teal))
            .unwrap()
            .unwrap();
        assert!(
            renamed.contains("label = \"Jobb\"\ncolor = \"teal\"\n"),
            "{renamed}"
        );
        assert_eq!(relabel(&named, &work, None, None).unwrap().unwrap(), text);
    }

    #[test]
    fn the_html_signature_is_set_for_the_account_or_an_identity() {
        let text = "[[account]]\nkey = \"work\"\nname = \"Ada\"\naddress = \"ada@example.test\"\n\
                    signature = \"Ada\"  # mine\n\n[account.imap]\nhost = \"imap.example.test\"\n\n\
                    [account.smtp]\nhost = \"smtp.example.test\"\n\n[[account.identity]]\n\
                    address = \"board@example.test\"\n";
        let work = AccountKey::parse("work").unwrap();
        let own = set_signature_html(text, &work, "ADA@example.test", Some("work/ada.html"))
            .unwrap()
            .unwrap();
        assert_eq!(
            own,
            text.replace(
                "signature = \"Ada\"  # mine\n",
                "signature = \"Ada\"  # mine\nsignature_html = \"work/ada.html\"\n"
            )
        );
        let both = set_signature_html(&own, &work, "board@example.test", Some("work/board.html"))
            .unwrap()
            .unwrap();
        let account = &parse(&both).unwrap()[0];
        assert_eq!(account.signature_html.as_deref(), Some("work/ada.html"));
        assert_eq!(
            account.identities[0].signature_html.as_deref(),
            Some("work/board.html")
        );
        assert!(
            both.ends_with(
                "address = \"board@example.test\"\nsignature_html = \"work/board.html\"\n"
            )
        );
        // Taken out again: the file as it was.
        let back = set_signature_html(&both, &work, "board@example.test", None)
            .unwrap()
            .unwrap();
        assert_eq!(
            set_signature_html(&back, &work, "ada@example.test", None)
                .unwrap()
                .unwrap(),
            text
        );
        assert_eq!(
            set_signature_html(text, &work, "other@example.test", None),
            Ok(None)
        );
        assert_eq!(
            set_signature_html(
                text,
                &AccountKey::parse("gone").unwrap(),
                "ada@example.test",
                None
            ),
            Ok(None)
        );
        assert_eq!(
            set_signature_html(text, &work, "ada@example.test", Some("../x.html")),
            Err(EditError::Account(AccountFault::SignaturePath))
        );
    }

    #[test]
    fn relabel_keeps_comments_and_the_other_accounts() {
        let text = "[[account]]\nkey = \"work\"\nname = \"Ada\"\nlabel = \"Old\"   # mine\n\
                    address = \"ada@example.test\"\n\n[account.imap]\nhost = \"imap.example.test\"\n\n\
                    [account.smtp]\nhost = \"smtp.example.test\"\n\n# the club\n[[account]]\n\
                    key = \"club\"\nname = \"Ada\"\naddress = \"club@example.test\"\n\n\
                    [account.imap]\nhost = \"imap.example.test\"\n\n[account.smtp]\n\
                    host = \"smtp.example.test\"\n";
        let club = AccountKey::parse("club").unwrap();
        let changed = relabel(text, &AccountKey::parse("work").unwrap(), Some("New"), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            changed,
            text.replace("\"Old\"   # mine", "\"New\"   # mine")
        );
        let colored = relabel(text, &club, None, Some(AccountColor::Iris))
            .unwrap()
            .unwrap();
        assert_eq!(
            colored,
            text.replace(
                "address = \"club@example.test\"\n",
                "address = \"club@example.test\"\ncolor = \"iris\"\n"
            )
        );
        assert_eq!(parse(&colored).unwrap()[0].title(), "Old");
    }

    #[test]
    fn relabel_refuses_what_it_cant_write() {
        let work = AccountKey::parse("work").unwrap();
        assert_eq!(
            relabel(ONE, &AccountKey::parse("nobody").unwrap(), Some("X"), None),
            Ok(None)
        );
        assert_eq!(relabel("", &work, Some("X"), None), Ok(None));
        assert_eq!(
            relabel(ONE, &work, Some(" "), None),
            Err(EditError::Account(AccountFault::Label))
        );
        assert!(matches!(
            relabel(&format!("{ONE}oops = 1\n"), &work, Some("X"), None),
            Err(EditError::Invalid(_))
        ));
    }

    /// The default file's example, uncommented, without the block of the incoming server
    /// `[account.<leave_out>]`: it shows both, IMAP's and POP3's.
    fn default_example(leave_out: &str) -> String {
        let file = default_accounts_file();
        let example: String = file
            .lines()
            .skip_while(|line| *line != "# [[account]]")
            .map(|line| line.strip_prefix("# ").unwrap_or(""))
            .map(|line| format!("{line}\n"))
            .collect();
        let header = format!("[account.{leave_out}]");
        example
            .split("\n\n")
            .filter(|block| !block.starts_with(&header))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn the_default_file_explains_a_valid_account() {
        let example = default_example("pop3");
        assert!(
            !example
                .lines()
                .any(|line| line.starts_with("[account.pop3]"))
        );
        let accounts = parse(&example).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].key.as_str(), "work");
        assert_eq!(accounts[0].title(), "Work");
        assert_eq!(accounts[0].color, Some(AccountColor::Sage));
        assert_eq!(accounts[0].receive, Receive::Imap);
        assert_eq!(accounts[0].incoming.port, 993);
        assert_eq!(accounts[0].smtp.port, 465);
        assert_eq!(
            accounts[0].signature.as_deref(),
            Some("Ada Lovelace\nAnalytical Engines Ltd.")
        );
        assert_eq!(
            accounts[0].identities,
            [Identity {
                name: "Ada at the board".to_owned(),
                address: "board@example.test".to_owned(),
                signature: Some("Ada, for the board".to_owned()),
                signature_html: None,
            }]
        );
    }

    #[test]
    fn the_default_file_explains_a_pop3_account() {
        let example = default_example("imap");
        assert!(
            !example
                .lines()
                .any(|line| line.starts_with("[account.imap]"))
        );
        let accounts = parse(&example).unwrap();
        assert_eq!(
            (&accounts[0].incoming, accounts[0].receive),
            (
                &Endpoint {
                    host: "pop.example.test".to_owned(),
                    port: 995,
                    security: Security::Tls,
                    certificate: None,
                },
                Receive::Pop3(Pop3Policy {
                    leave_on_server: true,
                    delete_after_days: Some(30),
                })
            )
        );
        assert_eq!(accounts[0].identities.len(), 1);
        // Both incoming servers uncommented: one too many, said where the second one starts.
        let both = default_example("none");
        let found = errors(&both);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("this account already has [account.imap]"),
            "{found:?}"
        );
    }

    fn pop3_account(key: &str, address: &str, policy: Pop3Policy) -> AccountSettings {
        AccountSettings {
            incoming: Endpoint {
                host: "pop.example.test".to_owned(),
                port: 995,
                security: Security::Tls,
                certificate: None,
            },
            receive: Receive::Pop3(policy),
            ..account(key, address)
        }
    }

    const POP3: &str = "[[account]]\nkey = \"home\"\nname = \"Ada Lovelace\"\n\
                        address = \"ada@example.test\"\n\n[account.pop3]\n\
                        host = \"pop.example.test\"\n\n[account.smtp]\n\
                        host = \"smtp.example.test\"\nsecurity = \"starttls\"\n";

    #[test]
    fn reads_a_pop3_account_with_the_safe_defaults() {
        let accounts = parse(POP3).unwrap();
        let expected = AccountSettings {
            smtp: Endpoint {
                port: 587,
                ..account("home", "ada@example.test").smtp
            },
            ..pop3_account("home", "ada@example.test", Pop3Policy::default())
        };
        assert_eq!(accounts, [expected]);
        assert_eq!(
            accounts[0].receive,
            Receive::Pop3(Pop3Policy {
                leave_on_server: true,
                delete_after_days: None,
            }),
            "downloaded mail stays on the server and is never deleted there"
        );
        let starttls = POP3.replace(
            "host = \"pop.example.test\"\n",
            "host = \"pop.example.test\"\nsecurity = \"starttls\"\nleave_on_server = false\n",
        );
        let account = &parse(&starttls).unwrap()[0];
        assert_eq!(
            (account.incoming.port, account.incoming.security),
            (110, Security::StartTls)
        );
        assert_eq!(
            account.receive,
            Receive::Pop3(Pop3Policy {
                leave_on_server: false,
                delete_after_days: None,
            })
        );
        let days = POP3.replace(
            "host = \"pop.example.test\"\n",
            "host = \"pop.example.test\"\nport = 1995\nleave_on_server = true\n\
             delete_after_days = 14\n",
        );
        let account = &parse(&days).unwrap()[0];
        assert_eq!(account.incoming.port, 1995);
        assert_eq!(
            account.receive,
            Receive::Pop3(Pop3Policy {
                leave_on_server: true,
                delete_after_days: Some(14),
            })
        );
        let longest = POP3.replace(
            "host = \"pop.example.test\"\n",
            "host = \"pop.example.test\"\ndelete_after_days = 3650\n",
        );
        assert!(parse(&longest).is_ok());
    }

    #[test]
    fn pop3_mistakes_say_where_they_are() {
        let pop3 = |line: &str| {
            POP3.replace(
                "host = \"pop.example.test\"\n",
                &format!("host = \"pop.example.test\"\n{line}\n"),
            )
        };
        let cases: [(String, &str); 12] = [
            (
                ONE.replace(
                    "[account.smtp]",
                    "[account.pop3]\nhost = \"pop.example.test\"\n\n[account.smtp]",
                ),
                "line 9, column 10: this account already has [account.imap]: an account \
                 receives its mail by IMAP or by POP3, so keep one of the two tables",
            ),
            (
                POP3.replace("[account.pop3]\nhost = \"pop.example.test\"\n\n", ""),
                "line 1, column 1: this account has no incoming server: add an [account.imap] \
                 table, or [account.pop3] for POP3",
            ),
            (
                POP3.replace("host = \"pop.example.test\"\n", "port = 995\n"),
                "line 6, column 1: [account.pop3] has no host; add one, like host = \
                 \"pop.example.test\"",
            ),
            (
                pop3("leave_on_server = false\ndelete_after_days = 30"),
                "line 9, column 21: delete_after_days needs leave_on_server = true: with false, \
                 mail is deleted from the server as soon as it is downloaded",
            ),
            (
                pop3("delete_after_days = 0"),
                "line 8, column 21: delete_after_days is a whole number of days from 1 to 3650, \
                 not 0; to delete mail as soon as it is downloaded, set leave_on_server = false \
                 instead",
            ),
            (
                pop3("delete_after_days = 3651"),
                "line 8, column 21: delete_after_days is a whole number of days from 1 to 3650, \
                 not 3651",
            ),
            (
                pop3("delete_after_days = -7"),
                "line 8, column 21: delete_after_days is a whole number of days from 1 to 3650, \
                 not -7",
            ),
            (
                pop3("delete_after_days = \"never\""),
                "line 8, column 21: delete_after_days is a whole number of days from 1 to 3650, \
                 not a string; leave the line out to keep downloaded mail on the server",
            ),
            (
                pop3("leave_on_server = \"yes\""),
                "line 8, column 19: leave_on_server is true (downloaded mail stays on the server) \
                 or false (it is deleted there once downloaded), not a string",
            ),
            (
                pop3("leave_on_sever = true"),
                "line 8, column 1: unknown setting leave_on_sever in [account.pop3]; did you \
                 mean leave_on_server?",
            ),
            (
                ONE.replace("security = \"starttls\"", "leave_on_server = true"),
                "line 11, column 1: leave_on_server belongs in [account.pop3], for an account \
                 that downloads its mail by POP3",
            ),
            (
                format!("delete_after_days = 30\n{POP3}"),
                "line 1, column 1: delete_after_days belongs in [account.pop3]; put it under \
                 that line",
            ),
        ];
        for (text, expected) in cases {
            let found = errors(&text);
            assert!(
                found
                    .first()
                    .is_some_and(|error| error.starts_with(expected)),
                "{expected:?} not in {found:?}"
            );
        }
        let imap_after = POP3.replace(
            "[account.smtp]",
            "[account.imap]\nhost = \"imap.example.test\"\n\n[account.smtp]",
        );
        let found = errors(&imap_after);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("already has [account.pop3]"), "{found:?}");
        // A misspelt table: what it may have meant, and the incoming server it then lacks.
        assert_eq!(
            errors(&POP3.replace("[account.pop3]", "[account.pop]")),
            [
                "line 1, column 1: this account has no incoming server: add an [account.imap] \
                 table, or [account.pop3] for POP3",
                "line 6, column 10: unknown setting pop in [[account]]; did you mean pop3?",
            ]
        );
    }

    #[test]
    fn writes_what_it_reads_for_pop3() {
        let policies = [
            Pop3Policy::default(),
            Pop3Policy {
                leave_on_server: true,
                delete_after_days: Some(30),
            },
            Pop3Policy {
                leave_on_server: false,
                delete_after_days: None,
            },
        ];
        for policy in policies {
            let account = pop3_account("home", "ada@example.test", policy);
            let text = entry(&account);
            assert!(
                text.contains(
                    "\n[account.pop3]\nhost = \"pop.example.test\"\nport = 995\n\
                     security = \"tls\"\n"
                ),
                "{text}"
            );
            assert!(!text.contains("[account.imap]"), "{text}");
            assert!(
                text.contains(&format!("leave_on_server = {}\n", policy.leave_on_server)),
                "{text}"
            );
            assert_eq!(
                text.contains("delete_after_days"),
                policy.delete_after_days.is_some()
            );
            assert_eq!(parse(&text).unwrap(), std::slice::from_ref(&account));
        }
        let header = default_accounts_file();
        let work = add(&header, &account("work", "ada@work.example.test")).unwrap();
        let home = pop3_account("home", "ada@example.test", Pop3Policy::default());
        let both = add(&work, &home).unwrap();
        assert!(both.starts_with(&work));
        let read = parse(&both).unwrap();
        assert_eq!(
            read.iter()
                .map(|a| a.receive.protocol())
                .collect::<Vec<_>>(),
            [Protocol::Imap, Protocol::Pop3]
        );
        let back = remove(&both, &AccountKey::parse("home").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(back, work);
        let only_home = remove(&both, &AccountKey::parse("work").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(parse(&only_home).unwrap(), [home]);
    }

    #[test]
    fn a_pop3_policy_the_file_would_refuse_is_never_written() {
        for policy in [
            Pop3Policy {
                leave_on_server: false,
                delete_after_days: Some(30),
            },
            Pop3Policy {
                leave_on_server: true,
                delete_after_days: Some(0),
            },
            Pop3Policy {
                leave_on_server: true,
                delete_after_days: Some(MAX_DELETE_AFTER_DAYS + 1),
            },
        ] {
            let account = pop3_account("home", "ada@example.test", policy);
            assert!(check(&account).is_err(), "{policy:?}");
            assert!(matches!(add("", &account), Err(EditError::Account(_))));
        }
        let fine = pop3_account("home", "ada@example.test", Pop3Policy::default());
        assert_eq!(check(&fine), Ok(()));
        let mut bad_host = fine;
        bad_host.incoming.host = "pop.example.test:995".to_owned();
        assert_eq!(
            check(&bad_host),
            Err(AccountFault::Host(
                Protocol::Pop3,
                "the port goes in port = …, not in host"
            ))
        );
    }

    #[test]
    fn reads_signatures_and_identities() {
        let text = ONE.replace(
            "address = \"ada@example.test\"\n",
            "address = \"ada@example.test\"\nsignature = \"\"\"\n\n  Ada  \r\n\tLovelace\n\n\"\"\"\n",
        ) + "\n[[account.identity]]\naddress = \"alias@example.test\"\n\n\
               [[account.identity]]\nname = \"Board\"\naddress = \"board@example.test\"\n\
               signature = \"B\"\n";
        let account = &parse(&text).unwrap()[0];
        assert_eq!(account.signature.as_deref(), Some("  Ada\n\tLovelace"));
        assert_eq!(
            account.identities,
            [
                Identity {
                    name: "Ada Lovelace".to_owned(),
                    address: "alias@example.test".to_owned(),
                    signature: None,
                    signature_html: None,
                },
                Identity {
                    name: "Board".to_owned(),
                    address: "board@example.test".to_owned(),
                    signature: Some("B".to_owned()),
                    signature_html: None,
                },
            ]
        );
        let blank = ONE.replace("name = ", "signature = \" \\n \"\nname = ");
        assert_eq!(parse(&blank).unwrap()[0].signature, None);
    }

    #[test]
    fn identity_mistakes_say_where_they_are() {
        let identity = |body: &str| format!("{ONE}\n[[account.identity]]\n{body}");
        let cases: [(String, &str); 8] = [
            (
                identity("name = \"x\"\n"),
                "line 13, column 1: this identity has no address",
            ),
            (
                identity("address = \"nope\"\n"),
                "line 14, column 11: address is an email address, like ada@work.example.test",
            ),
            (
                identity("address = \"a@example.test\"\nadress = \"b@example.test\"\n"),
                "line 15, column 1: unknown setting adress in [[account.identity]]; did you mean \
                 address?",
            ),
            (
                identity("address = \"a@example.test\"\npassword = \"x\"\n"),
                "line 15, column 1: password looks like a password or token",
            ),
            (
                identity("address = \"ADA@example.test\"\n"),
                "line 14, column 11: this address is already one of this account's",
            ),
            (
                identity("address = \"a@example.test\"\nsignature = 3\n"),
                "line 15, column 13: signature takes a string in quotes",
            ),
            (
                identity("address = \"a@example.test\"\nsignature = \"a\\u0007b\"\n"),
                "line 15, column 13: signature can't hold control characters",
            ),
            (
                ONE.replace(
                    "name = ",
                    "identity = { address = \"a@example.test\" }\nname = ",
                ),
                "line 3, column 12: identities are written as [[account.identity]] tables",
            ),
        ];
        for (text, expected) in cases {
            let found = errors(&text);
            assert!(
                found
                    .first()
                    .is_some_and(|error| error.starts_with(expected)),
                "{expected:?} not in {found:?}"
            );
        }
        let long = ONE.replace(
            "name = ",
            &format!(
                "signature = \"{}\"\nname = ",
                "x".repeat(MAX_SIGNATURE_CHARS + 1)
            ),
        );
        assert!(errors(&long)[0].contains("signature is at most 10000 characters"));
        let home = ONE
            .replace("\"work\"", "\"home\"")
            .replace("ada@example.test", "home@example.test");
        let shared = format!(
            "{}\n{home}\n[[account.identity]]\naddress = \"Board@example.test\"\n",
            identity("address = \"board@example.test\"\n")
        );
        let found = errors(&shared);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("this address is also an identity of the account on line 1"),
            "{found:?}"
        );
        assert!(!found[0].contains("board@"), "never quotes the address");
    }

    #[test]
    fn identities_go_with_their_account() {
        let text = format!(
            "{ONE}\n[[account.identity]]\naddress = \"alias@example.test\"\n\n{}",
            ONE.replace("\"work\"", "\"home\"")
                .replace("ada@example.test", "home@example.test")
        );
        let accounts = parse(&text).unwrap();
        assert_eq!(accounts[0].identities.len(), 1);
        assert!(accounts[1].identities.is_empty());
        let without = remove(&text, &AccountKey::parse("work").unwrap())
            .unwrap()
            .unwrap();
        assert!(!without.contains("alias@"), "{without}");
        assert_eq!(parse(&without).unwrap()[0].key.as_str(), "home");
        let added = add(&text, &account("third", "third@example.test")).unwrap();
        assert_eq!(parse(&added).unwrap()[0].identities.len(), 1);
        assert!(!entry(&account("third", "third@example.test")).contains("signature"));
    }

    #[test]
    fn secrets_are_refused_with_the_reason() {
        let found = errors(&ONE.replace(
            "name = ",
            "password = \"hunter2\"\napp_password = \"x\"\nname = ",
        ));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("line 3, column 1: password looks like a password or token"));
        assert!(found[0].ends_with("never in accounts.toml; remove this line"));
        assert!(found[1].starts_with("line 4, column 1: app_password"));
        for text in [
            "token = \"x\"\n".to_owned(),
            ONE.replace("security = \"starttls\"", "client_secret = \"x\""),
            ONE.replace(
                "[account.smtp]",
                "[account.oauth]\nrefresh_token = \"x\"\n[account.smtp]",
            ),
        ] {
            let found = errors(&text);
            assert!(
                found.iter().any(|error| error.contains("keyring")),
                "{found:?}"
            );
            assert!(found.iter().all(|error| !error.contains("\"x\"")));
        }
    }

    #[test]
    fn mistakes_say_where_they_are() {
        let cases: [(String, &str); 14] = [
            (
                ONE.replace("key = \"work\"", "key = \"Work Mail\""),
                "line 2, column 7: key takes a–z, 0–9, - and _ only",
            ),
            (
                ONE.replace("key = \"work\"\n", ""),
                "line 1, column 1: this account has no key",
            ),
            (
                ONE.replace("name = \"Ada Lovelace\"", "name = 3"),
                "line 3, column 8: name takes a string in quotes, not an integer",
            ),
            (
                ONE.replace("address = \"ada@example.test\"", "address = \"ada\""),
                "line 4, column 11: address is an email address",
            ),
            (
                ONE.replace(
                    "host = \"imap.example.test\"",
                    "host = \"imap.example.test:993\"",
                ),
                "line 7, column 8: the port goes in port = …, not in host",
            ),
            (
                ONE.replace(
                    "host = \"imap.example.test\"",
                    "host = \"imaps://imap.example.test\"",
                ),
                "line 7, column 8: host is a server name like imap.example.test, without",
            ),
            (
                ONE.replace("security = \"starttls\"", "security = \"none\""),
                "line 11, column 12: security is \"tls\" (TLS from the first byte) or \
                 \"starttls\"; Sealwax never connects without TLS",
            ),
            (
                ONE.replace("security = \"starttls\"", "port = 70000"),
                "line 11, column 8: port is a whole number from 1 to 65535, not 70000",
            ),
            (
                ONE.replace("security = \"starttls\"", "portt = 25"),
                "line 11, column 1: unknown setting portt in [account.smtp]; did you mean port?",
            ),
            (
                ONE.replace("name = ", "adress = \"x@example.test\"\nname = "),
                "line 3, column 1: unknown setting adress in [[account]]; did you mean address?",
            ),
            (
                ONE.replace("name = ", "auth = \"google\"\nname = "),
                "line 3, column 8: auth is \"password\"",
            ),
            (
                format!("host = \"x\"\n{ONE}"),
                "line 1, column 1: host belongs in [account.imap], [account.pop3] or [account.smtp]",
            ),
            (
                format!("[acount]\n{ONE}"),
                "line 1, column 2: unknown section acount; did you mean account?",
            ),
            (
                "account = [{ key = \"work\" }]\n".to_owned(),
                "line 1, column 11: accounts are written as [[account]] tables",
            ),
        ];
        for (text, expected) in cases {
            let found = errors(&text);
            assert!(
                found
                    .first()
                    .is_some_and(|error| error.starts_with(expected)),
                "{expected:?} not in {found:?}"
            );
        }
        let found = errors(&ONE.replace("[account.smtp]", "[account.junk]"));
        assert_eq!(
            found,
            [
                "line 1, column 1: this account has no [account.smtp] table with its outgoing \
                 server",
                "line 9, column 10: unknown setting junk in [[account]]",
            ]
        );
    }

    #[test]
    fn keys_and_addresses_are_unique() {
        let second = ONE.replace("Ada Lovelace", "Ada at work");
        let found = errors(&format!("{ONE}\n{second}"));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with(
            "line 14, column 7: key \"work\" is also the key of the account on line 1"
        ));
        assert!(found[1].starts_with(
            "line 16, column 11: this address is also the address of the account on line 1"
        ));
        let other = ONE
            .replace("\"work\"", "\"home\"")
            .replace("ada@example.test", "ADA@Example.test");
        let found = errors(&format!("{ONE}\n{other}"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(!found[0].contains("ada@"), "never quotes the address");
    }

    #[test]
    fn syntax_errors_have_positions() {
        let found = errors("[[account]]\nkey = \n");
        assert!(found[0].starts_with("line 2, column 7:"), "{found:?}");
    }

    #[test]
    fn writes_what_it_reads() {
        let account = account("k3j5x8m2q9wa", "ada@example.test");
        let text = entry(&account);
        assert!(
            text.starts_with("[[account]]\nkey = \"k3j5x8m2q9wa\"\n"),
            "{text}"
        );
        assert!(text.contains("\n[account.imap]\nhost = \"imap.example.test\"\nport = 993\n"));
        assert_eq!(parse(&text).unwrap(), std::slice::from_ref(&account));
        let mut odd = account;
        odd.name = "Ada \"the\" 'Countess' \\ Lovelace".to_owned();
        assert_eq!(parse(&entry(&odd)).unwrap(), [odd]);
    }

    #[test]
    fn adding_keeps_the_text_before() {
        let header = default_accounts_file();
        let first = add(&header, &account("work", "ada@example.test")).unwrap();
        assert!(first.starts_with(&header));
        assert!(
            first[header.len()..].starts_with("\n[[account]]\n"),
            "{first}"
        );
        let edited = first.replace("name = \"Ada Lovelace\"", "name = \"Ada\"   # mine\n# note");
        let second = add(&edited, &account("home", "ada@home.example.test")).unwrap();
        assert!(second.starts_with(&edited));
        assert_eq!(parse(&second).unwrap().len(), 2);
        assert_eq!(
            add("", &account("a", "a@example.test")).unwrap(),
            entry(&account("a", "a@example.test"))
        );
        let unterminated = "# no newline at the end";
        let added = add(unterminated, &account("a", "a@example.test")).unwrap();
        assert!(added.starts_with("# no newline at the end\n\n[[account]]"));
    }

    /// An address Sealwax can't send from is refused when the assistant writes an account, and
    /// read when the file holds one: the account still gets its mail.
    #[test]
    fn writes_refuse_an_address_that_cant_be_sent_from_and_reads_keep_it() {
        let jorg = account("jorg", "jörg@example.test");
        assert_eq!(check(&jorg), Err(AccountFault::NotSendable));
        assert_eq!(
            add("", &jorg),
            Err(EditError::Account(AccountFault::NotSendable))
        );
        let mut alias = account("work", "ada@example.test");
        alias.identities.push(Identity {
            name: "Åse".to_owned(),
            address: "åse@example.test".to_owned(),
            signature: None,
            signature_html: None,
        });
        assert_eq!(check(&alias), Err(AccountFault::NotSendable));
        let written = entry(&account("jorg", "jorg@example.test"))
            .replace("jorg@example.test", "jörg@example.test");
        let read = parse(&written).expect("the file reads");
        assert_eq!(read[0].address, "jörg@example.test");
    }

    #[test]
    fn adding_refuses_duplicates_and_faulty_files() {
        let text = add("", &account("work", "ada@example.test")).unwrap();
        assert_eq!(
            add(&text, &account("work", "other@example.test")),
            Err(EditError::KeyExists)
        );
        assert_eq!(
            add(&text, &account("home", "Ada@Example.Test")),
            Err(EditError::AddressExists)
        );
        assert!(matches!(
            add("[[account]]\nkey = 1\n", &account("a", "a@example.test")),
            Err(EditError::Invalid(_))
        ));
        let mut bad = account("a", "a@example.test");
        bad.incoming.host = "imap.example.test:993".to_owned();
        assert!(matches!(add("", &bad), Err(EditError::Account(_))));
        bad = account("a", "a@example.test");
        bad.key = AccountKey::unassigned();
        assert!(matches!(add("", &bad), Err(EditError::Account(_))));
    }

    #[test]
    fn removing_keeps_other_comments() {
        let header = "# my accounts\n# kept\n\n";
        let work = "# the work account\n[[account]]\nkey = \"work\"\nname = \"W\"\n\
                    address = \"w@example.test\"\n# inside work\n[account.imap]\nhost = \"i.example.test\"\n\
                    [account.smtp]\nhost = \"s.example.test\"\n\n";
        let home = "# the home account\n[[account]] # trailing\nkey = \"home\"\nname = \"H\"\n\
                    address = \"h@example.test\"\n[account.imap]\nhost = \"i.example.test\"\n\
                    [account.smtp]\nhost = \"s.example.test\"\n";
        let tail = "# the end\n";
        let text = format!("{header}{work}{home}{tail}");
        assert_eq!(parse(&text).unwrap().len(), 2);

        let without_work = remove(&text, &AccountKey::parse("work").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(without_work, format!("{header}{home}{tail}"));
        let without_home = remove(&text, &AccountKey::parse("home").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(without_home, format!("{header}{work}{tail}"));
        let neither = remove(&without_home, &AccountKey::parse("work").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(neither, format!("{header}{tail}"));
        assert_eq!(parse(&neither), Ok(Vec::new()));
        assert_eq!(remove(&text, &AccountKey::parse("gone").unwrap()), Ok(None));
        assert_eq!(remove("", &AccountKey::parse("gone").unwrap()), Ok(None));
        assert!(matches!(
            remove("[[account]\n", &AccountKey::parse("x").unwrap()),
            Err(EditError::Invalid(_))
        ));
    }

    #[test]
    fn add_then_remove_gives_the_file_back() {
        let header = default_accounts_file();
        let one = add(&header, &account("work", "ada@example.test")).unwrap();
        let two = add(&one, &account("home", "ada@home.example.test")).unwrap();
        let back = remove(&two, &AccountKey::parse("home").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(back, one);
        let back = remove(&back, &AccountKey::parse("work").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(back, header);
        let only_home = remove(&two, &AccountKey::parse("work").unwrap())
            .unwrap()
            .unwrap();
        assert!(only_home.starts_with(&header));
        assert_eq!(parse(&only_home).unwrap()[0].key.as_str(), "home");
    }

    /// Found by fuzzing (ML-093): `parse` reads a key without the white space around it, and
    /// removing it by that key found nothing, so the entry stayed while the account's data went.
    #[test]
    fn a_key_written_with_spaces_is_removed_by_its_key() {
        let text = "[[account]]\nkey = \" work \"\nname = \"W\"\naddress = \"w@example.test\"\n\
                    [account.imap]\nhost = \"i.example.test\"\n[account.smtp]\nhost = \"s.example.test\"\n";
        assert_eq!(parse(text).unwrap()[0].key.as_str(), "work");
        let removed = remove(text, &AccountKey::parse("work").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(parse(&removed), Ok(Vec::new()));
    }

    #[test]
    fn keys_and_hosts() {
        for host in [
            "imap.example.test",
            "127.0.0.1",
            "::1",
            "mail-1.example.test",
        ] {
            assert_eq!(host_fault(host), None, "{host}");
        }
        for host in ["", "a b", ".x", "x..y", "x:1", "imap://x", "x/y"] {
            assert!(host_fault(host).is_some(), "{host}");
        }
        assert_eq!(default_port(Protocol::Smtp, Security::StartTls), 587);
        assert_eq!(default_port(Protocol::Pop3, Security::Tls), 995);
        assert_eq!(default_port(Protocol::Pop3, Security::StartTls), 110);
    }
}
