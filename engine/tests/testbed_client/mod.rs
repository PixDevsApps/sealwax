//! The other side of the testbed sync suites: a plain IMAP client, independent of the engine's
//! sync code, for the mutator that changes an account while the engine syncs it and for the
//! server read the cache is compared with. Every completion is checked. Test users and synthetic
//! messages only.

#![allow(dead_code)]

use super::snapshot::{Folder, Rng};
use async_imap::imap_proto::{
    AttributeValue, Capability, MailboxDatum, MessageSection, NameAttribute, Response,
    ResponseCode, SectionPath, Status,
};
use rustls_pki_types::ServerName;
use sealwax_domain::model::Flags;
use sealwax_engine::net::Trust;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

type Session = async_imap::Session<TlsStream<TcpStream>>;

pub const PASSWORD: &str = "testbed-password";
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum Failure {
    No,
    Bad,
    Other(String),
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Failure::Other(format!("io: {:?}", error.kind()))
    }
}

impl From<async_imap::error::Error> for Failure {
    fn from(error: async_imap::error::Error) -> Self {
        match error {
            async_imap::error::Error::No(_) => Failure::No,
            async_imap::error::Error::Bad(_) => Failure::Bad,
            other => Failure::Other(format!("{other:?}").chars().take(80).collect()),
        }
    }
}

struct Plain(String);

impl async_imap::Authenticator for Plain {
    type Response = Vec<u8>;

    fn process(&mut self, _: &[u8]) -> Vec<u8> {
        std::mem::take(&mut self.0).into_bytes()
    }
}

pub struct Client {
    session: Session,
    caps: HashSet<String>,
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

impl Client {
    /// TLS to 127.0.0.1:`port`, trusting exactly the testbed CA, then AUTHENTICATE PLAIN.
    pub async fn connect(port: u16, ca: &Path, user: &str) -> Result<Client, Failure> {
        let config = Trust::Roots(ca.to_owned())
            .client_config()
            .map_err(|_| Failure::Other("trust".into()))?;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
        tcp.set_nodelay(true)?;
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let stream = TlsConnector::from(config).connect(name, tcp).await?;
        let mut client = async_imap::Client::new(stream);
        client
            .read_response()
            .await?
            .ok_or(Failure::Other("no greeting".into()))?;
        let session = client
            .authenticate("PLAIN", Plain(format!("\0{user}\0{PASSWORD}")))
            .await
            .map_err(|(error, _)| Failure::from(error))?;
        let mut client = Client {
            session,
            caps: HashSet::new(),
        };
        let mut caps = HashSet::new();
        client
            .run("CAPABILITY", |response| {
                if let Response::Capabilities(found) = response {
                    caps.extend(found.iter().map(|capability| match capability {
                        Capability::Imap4rev1 => "IMAP4REV1".to_owned(),
                        Capability::Auth(mechanism) => {
                            format!("AUTH={}", mechanism.to_ascii_uppercase())
                        }
                        Capability::Atom(atom) => atom.to_ascii_uppercase(),
                    }));
                }
            })
            .await?;
        client.caps = caps;
        Ok(client)
    }

    pub fn has(&self, name: &str) -> bool {
        self.caps.contains(&name.to_ascii_uppercase())
    }

    async fn finish(
        &mut self,
        id: &async_imap::imap_proto::RequestId,
        mut on: impl FnMut(&Response<'_>),
    ) -> Result<(), Failure> {
        loop {
            let response = tokio::time::timeout(TIMEOUT, self.session.read_response())
                .await
                .map_err(|_| Failure::Other("timeout".into()))??
                .ok_or(Failure::Other("connection lost".into()))?;
            match response.parsed() {
                Response::Done { tag, status, .. } if tag == id => {
                    return match status {
                        Status::Ok => Ok(()),
                        Status::No => Err(Failure::No),
                        _ => Err(Failure::Bad),
                    };
                }
                Response::Data {
                    status: Status::Bye,
                    ..
                } => return Err(Failure::Other("BYE".into())),
                other => on(other),
            }
        }
    }

    /// One command; every untagged response goes to `on`.
    pub async fn run(
        &mut self,
        command: &str,
        on: impl FnMut(&Response<'_>),
    ) -> Result<(), Failure> {
        let id = self.session.run_command(command).await?;
        self.finish(&id, on).await
    }

    /// APPEND with a non-synchronizing literal where offered.
    pub async fn append(
        &mut self,
        name_raw: &str,
        flags: &str,
        message: &[u8],
    ) -> Result<(), Failure> {
        let plus = if self.has("LITERAL+") { "+" } else { "" };
        let id = self
            .session
            .run_command(format!(
                "APPEND {} ({flags}) {{{}{plus}}}",
                quote(name_raw),
                message.len()
            ))
            .await?;
        if plus.is_empty() {
            let response = tokio::time::timeout(TIMEOUT, self.session.read_response())
                .await
                .map_err(|_| Failure::Other("timeout".into()))??
                .ok_or(Failure::Other("connection lost".into()))?;
            if !matches!(response.parsed(), Response::Continue { .. }) {
                return Err(Failure::No);
            }
        }
        let stream = self.session.as_mut();
        stream.write_all(message).await?;
        stream.write_all(b"\r\n").await?;
        stream.flush().await?;
        self.finish(&id, |_| {}).await
    }

    /// SELECT (read-write: STORE needs it); returns EXISTS.
    pub async fn select(&mut self, name_raw: &str) -> Result<u32, Failure> {
        let mut exists = 0;
        self.run(&format!("SELECT {}", quote(name_raw)), |response| {
            if let Response::MailboxData(MailboxDatum::Exists(n)) = response {
                exists = *n;
            }
        })
        .await?;
        Ok(exists)
    }

    /// EXAMINE; returns UIDVALIDITY.
    pub async fn uidvalidity(&mut self, name_raw: &str) -> Result<u32, Failure> {
        let mut found = 0;
        self.run(&format!("EXAMINE {}", quote(name_raw)), |response| {
            if let Response::Data {
                status: Status::Ok,
                outcome,
            } = response
                && let Some(ResponseCode::UidValidity(value)) = outcome.code
            {
                found = value;
            }
        })
        .await?;
        Ok(found)
    }

    /// The UIDs of the selected mailbox.
    pub async fn uids(&mut self) -> Result<Vec<u32>, Failure> {
        let mut uids = Vec::new();
        self.run("UID SEARCH ALL", |response| {
            if let Response::MailboxData(MailboxDatum::Search(found)) = response {
                uids.extend_from_slice(found);
            }
        })
        .await?;
        uids.sort_unstable();
        Ok(uids)
    }

    /// Every mailbox's wire name and attributes.
    pub async fn list(&mut self) -> Result<Vec<(String, bool)>, Failure> {
        let mut out = Vec::new();
        self.run("LIST \"\" \"*\"", |response| {
            if let Response::MailboxData(MailboxDatum::List(list)) = response {
                let selectable = !list.name_attributes.iter().any(|a| {
                    matches!(a, NameAttribute::NoSelect)
                        || matches!(a, NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent"))
                });
                out.push((list.name.to_string(), selectable));
            }
        })
        .await?;
        Ok(out)
    }

    pub async fn create(&mut self, name_raw: &str) -> Result<(), Failure> {
        match self
            .run(&format!("CREATE {}", quote(name_raw)), |_| {})
            .await
        {
            Ok(()) | Err(Failure::No) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Empties a mailbox.
    pub async fn empty(&mut self, name_raw: &str) -> Result<(), Failure> {
        if self.select(name_raw).await? > 0 {
            self.run("UID STORE 1:* +FLAGS.SILENT (\\Deleted)", |_| {})
                .await?;
            self.run("EXPUNGE", |_| {}).await?;
        }
        Ok(())
    }

    /// The server's state, read independently of the sync code: every selectable mailbox with
    /// its UIDVALIDITY and each message's UID, flags, keywords and Message-ID.
    pub async fn snapshot(&mut self) -> Result<BTreeMap<String, Folder>, Failure> {
        let mut out = BTreeMap::new();
        for (name, selectable) in self.list().await? {
            if !selectable {
                continue;
            }
            let uidvalidity = self.uidvalidity(&name).await?;
            let mut messages = BTreeMap::new();
            self.run(
                "UID FETCH 1:* (UID FLAGS BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
                |response| {
                    if let Response::Fetch(_, attributes) = response
                        && let Some((uid, entry)) = entry(attributes)
                    {
                        messages.insert(uid, entry);
                    }
                },
            )
            .await?;
            out.insert(
                name,
                Folder {
                    uidvalidity: Some(uidvalidity),
                    floor: 1,
                    synced: true,
                    messages,
                },
            );
        }
        Ok(out)
    }

    pub async fn logout(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(2), self.run("LOGOUT", |_| {})).await;
    }

    pub async fn store(&mut self, uids: &[u32], change: &str) -> Result<(), Failure> {
        self.run(&format!("UID STORE {} {change}", uid_set(uids)), |_| {})
            .await
    }
}

/// UID, flags and Message-ID from a FETCH response.
fn entry(attributes: &[AttributeValue<'_>]) -> Option<(u32, super::snapshot::Entry)> {
    let mut uid = None;
    let mut flags = None;
    let mut message_id = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(found) => uid = Some(*found),
            AttributeValue::Flags(found) => {
                flags = Some(Flags::from_imap(found.iter().map(|f| f.as_ref())));
            }
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                data,
                ..
            } => {
                let block =
                    String::from_utf8_lossy(data.as_deref().unwrap_or_default()).into_owned();
                message_id = block.split("\r\n").find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("message-id"))
                        .map(|(_, value)| value.trim().trim_matches(['<', '>']).to_owned())
                });
            }
            _ => {}
        }
    }
    let (flags, keywords) = flags?;
    Some((uid?, (flags, keywords, message_id)))
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "kilo",
    "lima", "mike", "oscar", "papa", "romeo", "sierra", "tango", "fjord", "bre", "skog", "elv",
    "hytte", "ski", "vann",
];

/// A synthetic message with the given Message-ID (without angle brackets).
pub fn message(message_id: &str, rng: &mut Rng) -> Vec<u8> {
    let sender = rng.below(40);
    let words = 20 + rng.below(200);
    let mut body = String::new();
    for i in 0..words {
        body.push_str(rng.pick(WORDS));
        body.push_str(if i % 12 == 11 { "\r\n" } else { " " });
    }
    let reply = if rng.chance(25) {
        format!(
            "In-Reply-To: <parent.{sender}@example.test>\r\nReferences: <parent.{sender}@example.test>\r\n"
        )
    } else {
        String::new()
    };
    format!(
        "From: \"Synthetic Sender {sender}\" <sender{sender}@example.test>\r\n\
         To: <test@example.test>\r\n\
         Subject: Synthetic {message_id}\r\n\
         Date: Mon, 5 Oct 2026 12:00:00 +0000\r\n\
         Message-ID: <{message_id}>\r\n\
         {reply}MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}
