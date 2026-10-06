//! Shared by M6's sending suites: S7's fixture messages with their envelopes, a Message-ID of a
//! run's own, Mailpit's HTTP API (127.0.0.1 only, a generic User-Agent), whole messages read
//! back over IMAP, mail-parser's round trip and synthetic messages of a given size. Test data
//! and dummy addresses only.

#![allow(dead_code)]

use super::testbed_client::Client;
use async_imap::imap_proto::{AttributeValue, Response, SectionPath};
use mail_parser::{MessageParser, MimeHeaders};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// One of S7's fixtures: the transmitted copy and its envelope (To and Cc from it, Bcc from
/// the Sent copy, which carries the one Bcc field).
#[derive(Clone, Debug)]
pub struct Fixture {
    pub name: String,
    pub transmitted: Vec<u8>,
    pub sent_copy: Vec<u8>,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
}

fn addresses(address: Option<&mail_parser::Address<'_>>) -> Vec<String> {
    address
        .map(|address| {
            address
                .iter()
                .filter_map(|addr| addr.address().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// S7's fixtures (`spikes/s7-composer/fixtures/*.transmitted.eml`), by name.
pub fn fixtures() -> Vec<Fixture> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spikes/s7-composer/fixtures");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.strip_suffix(".transmitted.eml").map(str::to_owned)
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let transmitted = std::fs::read(dir.join(format!("{name}.transmitted.eml"))).unwrap();
            let sent_copy = std::fs::read(dir.join(format!("{name}.sent-copy.eml"))).unwrap();
            let parser = MessageParser::default();
            let sent = parser.parse(&transmitted).unwrap();
            let copy = parser.parse(&sent_copy).unwrap();
            Fixture {
                from: addresses(sent.from()).remove(0),
                to: addresses(sent.to()),
                cc: addresses(sent.cc()),
                bcc: addresses(copy.bcc()),
                name,
                transmitted,
                sent_copy,
            }
        })
        .collect()
}

/// A run's own tag: Stalwart drops a second delivery of a Message-ID it delivered before to
/// the same mailbox, so every run sends ids no earlier run used.
pub fn run_tag() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos:x}")
}

/// `bytes` with its Message-ID replaced by `<id>`.
pub fn with_id(bytes: &[u8], id: &str) -> Vec<u8> {
    let start = find(bytes, b"\r\nMessage-ID: <").unwrap() + 2;
    let end = start + find(&bytes[start..], b"\r\n").unwrap();
    let mut out = bytes[..start].to_vec();
    out.extend_from_slice(format!("Message-ID: <{id}>").as_bytes());
    out.extend_from_slice(&bytes[end..]);
    out
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

/// This machine's host name, for the scans.
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap()
        .trim()
        .to_owned()
}

/// The header lines a server put in front of the bytes it was given; `None` when `received`
/// doesn't end with exactly `sent` or what comes before isn't header lines.
pub fn prepended<'a>(received: &'a [u8], sent: &[u8]) -> Option<&'a [u8]> {
    let prefix = received.strip_suffix(sent)?;
    let lines_ok = prefix.is_empty()
        || (prefix.ends_with(b"\r\n")
            && prefix[..prefix.len() - 2]
                .split(|b| *b == b'\n')
                .all(|line| {
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    line.first().is_some_and(|b| *b == b' ' || *b == b'\t')
                        || line
                            .iter()
                            .position(|b| *b == b':')
                            .is_some_and(|at| at > 0)
                }));
    lines_ok.then_some(prefix)
}

/// What a reader sees of a message, through mail-parser.
#[derive(Debug, PartialEq, Eq)]
pub struct Seen {
    pub subject: Option<String>,
    pub message_id: Option<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub texts: Vec<String>,
    pub htmls: Vec<String>,
    pub attachments: Vec<(Option<String>, usize)>,
}

pub fn seen(bytes: &[u8]) -> Seen {
    let message = MessageParser::default().parse(bytes).unwrap();
    Seen {
        subject: message.subject().map(str::to_owned),
        message_id: message.message_id().map(str::to_owned),
        from: addresses(message.from()),
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        texts: (0..message.text_body_count())
            .filter_map(|i| message.body_text(i).map(|t| t.into_owned()))
            .collect(),
        htmls: (0..message.html_body_count())
            .filter_map(|i| message.body_html(i).map(|t| t.into_owned()))
            .collect(),
        attachments: message
            .attachments()
            .map(|part| {
                (
                    part.attachment_name().map(str::to_owned),
                    part.contents().len(),
                )
            })
            .collect(),
    }
}

/// The Bcc field of a message's own header block, if any (not the server's prepended lines).
pub fn has_bcc_header(bytes: &[u8]) -> bool {
    let head = &bytes[..find(bytes, b"\r\n\r\n").unwrap_or(bytes.len())];
    head.split(|b| *b == b'\n')
        .any(|line| line.len() >= 4 && line[..4].eq_ignore_ascii_case(b"bcc:"))
}

/// A whole message as a server holds it.
#[derive(Clone, Debug)]
pub struct Held {
    pub uid: u32,
    pub flags: Vec<String>,
    pub raw: Vec<u8>,
}

/// Every message of `folder`, whole (BODY.PEEK: nothing is marked).
pub async fn messages(client: &mut Client, folder: &str) -> Vec<Held> {
    let mut out = Vec::new();
    if client.select(folder).await.unwrap() == 0 {
        return out;
    }
    client
        .run("UID FETCH 1:* (UID FLAGS BODY.PEEK[])", |response| {
            let Response::Fetch(_, attributes) = response else {
                return;
            };
            let mut held = Held {
                uid: 0,
                flags: Vec::new(),
                raw: Vec::new(),
            };
            for attribute in attributes {
                match attribute {
                    AttributeValue::Uid(uid) => held.uid = *uid,
                    AttributeValue::Flags(flags) => {
                        held.flags = flags
                            .iter()
                            .map(|f| f.to_string())
                            .filter(|f| !f.eq_ignore_ascii_case("\\Recent"))
                            .collect();
                    }
                    AttributeValue::BodySection {
                        section: None | Some(SectionPath::Full(_)),
                        data,
                        ..
                    } => held.raw = data.as_deref().unwrap_or_default().to_vec(),
                    _ => {}
                }
            }
            out.push(held);
        })
        .await
        .unwrap();
    out.sort_by_key(|held| held.uid);
    out
}

/// The messages of `folder` whose Message-ID is `id`, waiting up to `wait` for at least one
/// (delivery is asynchronous after the server's 250).
pub async fn arrived(client: &mut Client, folder: &str, id: &str, wait: Duration) -> Vec<Held> {
    let started = Instant::now();
    loop {
        let found: Vec<Held> = messages(client, folder)
            .await
            .into_iter()
            .filter(|held| seen(&held.raw).message_id.as_deref() == Some(id))
            .collect();
        if !found.is_empty() || started.elapsed() >= wait {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A synthetic message of about `size` bytes: text lines of 76 characters, with `marker` in
/// the middle line and `end_marker` in the last one.
pub fn synthetic(id: &str, size: usize, marker: &str, end_marker: &str) -> Vec<u8> {
    let mut out = format!(
        "From: <outbox@example.test>\r\nTo: <someone@example.test>\r\nSubject: Synthetic send\r\n\
         Date: Tue, 6 Oct 2026 12:00:00 +0000\r\nMessage-ID: <{id}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=us-ascii\r\n\r\n"
    )
    .into_bytes();
    let line = "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789ab\r\n";
    let lines = size.saturating_sub(out.len()) / line.len();
    for n in 0..lines.max(3) {
        if n == lines.max(3) / 2 {
            out.extend_from_slice(format!("{marker}\r\n").as_bytes());
        } else {
            out.extend_from_slice(line.as_bytes());
        }
    }
    out.extend_from_slice(format!("{end_marker}\r\n").as_bytes());
    out
}

/// Mailpit's HTTP API on 127.0.0.1:17080, over HTTP/1.0 (no chunked replies).
pub struct Mailpit;

impl Mailpit {
    async fn request(method: &str, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", 17080)).await.unwrap();
        let request = format!(
            "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1:17080\r\n\
             User-Agent: mail-dev (automated)\r\nAccept: */*\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut reply))
            .await
            .unwrap()
            .unwrap();
        let split = find(&reply, b"\r\n\r\n").unwrap();
        let status = String::from_utf8_lossy(&reply[..split]).into_owned();
        assert!(status.starts_with("HTTP/1.0 200") || status.starts_with("HTTP/1.1 200"));
        reply[split + 4..].to_vec()
    }

    /// Deletes every message Mailpit caught.
    pub async fn clear() {
        Mailpit::request("DELETE", "/api/v1/messages").await;
    }

    /// Mailpit's IDs of the messages it caught with `message_id`.
    pub async fn ids(message_id: &str) -> Vec<String> {
        let body = Mailpit::request("GET", "/api/v1/messages?limit=500").await;
        let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
        list["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["MessageID"] == message_id)
            .map(|message| message["ID"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The raw message as Mailpit holds it.
    pub async fn raw(id: &str) -> Vec<u8> {
        Mailpit::request("GET", &format!("/api/v1/message/{id}/raw")).await
    }

    /// Waits up to `wait` for a message with `message_id`.
    pub async fn caught(message_id: &str, wait: Duration) -> Vec<String> {
        let started = Instant::now();
        loop {
            let ids = Mailpit::ids(message_id).await;
            if !ids.is_empty() || started.elapsed() >= wait {
                return ids;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// p50 and p95 by nearest rank.
pub fn percentiles(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    let rank = |p: f64| {
        let index = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1;
        samples[index]
    };
    (rank(0.50), rank(0.95))
}
