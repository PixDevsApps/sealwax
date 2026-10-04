//! The io-imap 0.7.1 comparison prototype (blocking light client over our own rustls stream):
//! connect, authenticate, select, header fetch, CONDSTORE, QRESYNC, APPENDUID/COPYUID, error
//! reporting and IDLE (through its mailbox watcher). Prints counts and timings only.
//!
//! s1-ioimap <server> [--batch 500]

use std::any::Any;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use email_spike_s1::testbed::{self, Server};
use io_imap::client::{ImapClient, ImapClientStd, ImapMailboxWatchStreamOptions, ImapStream};
use io_imap::rfc3501::append::ImapMessageAppendOptions;
use io_imap::rfc3501::examine::ImapMailboxExamineOptions;
use io_imap::rfc3501::fetch::ImapMessageFetchOptions;
use io_imap::rfc3501::search::ImapMessageSearchOptions;
use io_imap::rfc3501::select::ImapMailboxSelectOptions;
use io_imap::rfc3501::store::ImapMessageStoreOptions;
use io_imap::rfc6851::r#move::ImapMessageMoveOptions;
use io_imap::sasl::auth_plain::ImapAuthPlainOptions;
use io_imap::types::command::{FetchModifier, SelectParameter};
use io_imap::types::core::{AString, Vec1};
use io_imap::types::extensions::enable::CapabilityEnable;
use io_imap::types::fetch::{
    MacroOrMessageDataItemNames, MessageDataItem, MessageDataItemName, Section,
};
use io_imap::types::flag::{Flag, StoreType};
use io_imap::types::mailbox::Mailbox;
use io_imap::types::response::Capability;
use io_imap::types::search::SearchKey;
use io_imap::types::sequence::SequenceSet;
use rustls::{ClientConnection, StreamOwned};

struct Tls(StreamOwned<ClientConnection, TcpStream>);

impl Read for Tls {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Tls {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl ImapStream for Tls {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.get_ref().set_read_timeout(timeout)
    }
}

fn connect(server: Server, user: &str) -> Result<(ImapClientStd, Vec<Capability<'static>>)> {
    let config = email_spike_s1::tls::with_roots(&testbed::ca_path())?;
    let tcp = TcpStream::connect(("127.0.0.1", server.tls_port))?;
    tcp.set_nodelay(true)?;
    let tls = ClientConnection::new(config, "localhost".try_into()?)?;
    let mut client = ImapClientStd::new(Tls(StreamOwned::new(tls, tcp)));
    let greeting = client.greeting()?;
    let sasl_ir = greeting
        .capability
        .iter()
        .any(|c| matches!(c, Capability::SaslIr));
    let options = ImapAuthPlainOptions {
        initial_request: sasl_ir,
        ensure_capabilities: true,
        auto_id: None,
    };
    let caps = client.auth_plain(None, user, testbed::PASSWORD, options)?;
    Ok((client, caps))
}

fn header_items() -> Result<MacroOrMessageDataItemNames<'static>> {
    let fields = Vec1::try_from(vec![
        AString::try_from("REFERENCES")?,
        AString::try_from("IN-REPLY-TO")?,
        AString::try_from("LIST-UNSUBSCRIBE")?,
    ])?;
    Ok(MacroOrMessageDataItemNames::MessageDataItemNames(vec![
        MessageDataItemName::Uid,
        MessageDataItemName::Flags,
        MessageDataItemName::InternalDate,
        MessageDataItemName::Rfc822Size,
        MessageDataItemName::Envelope,
        MessageDataItemName::BodyStructure,
        MessageDataItemName::BodyExt {
            section: Some(Section::HeaderFields(None, fields)),
            partial: None,
            peek: true,
        },
    ]))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let server =
        testbed::server(&args.next().context("missing server")?).context("unknown server")?;
    let batch: usize = match (args.next().as_deref(), args.next()) {
        (Some("--batch"), Some(n)) => n.parse()?,
        _ => 500,
    };
    let runs = 3;
    println!(
        "s1-ioimap on {} ({} build)",
        server.name,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    let t = Instant::now();
    let (mut client, caps) = connect(server, "perf")?;
    println!(
        "connect + greeting + AUTHENTICATE PLAIN: {:.1} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    let condstore = caps.iter().any(|c| matches!(c, Capability::CondStore));
    let qresync = caps.iter().any(|c| matches!(c, Capability::QResync));

    let parameters = if condstore {
        vec![SelectParameter::CondStore]
    } else {
        vec![]
    };
    let data = client.examine(Mailbox::Inbox, ImapMailboxExamineOptions { parameters })?;
    println!(
        "EXAMINE INBOX: exists {:?}, HIGHESTMODSEQ present {}",
        data.exists,
        data.highest_mod_seq.is_some()
    );

    for run in 1..=runs {
        let t = Instant::now();
        let mut uids = client.search(
            Vec1::from(SearchKey::All),
            ImapMessageSearchOptions { uid: true },
        )?;
        uids.sort_unstable();
        uids.reverse();
        let mut fetched = 0usize;
        for chunk in uids.chunks(batch) {
            let set = SequenceSet::try_from(chunk)?;
            let result = client.fetch(
                set,
                header_items()?,
                ImapMessageFetchOptions {
                    uid: true,
                    modifiers: vec![],
                },
            )?;
            fetched += result
                .values()
                .filter(|items| {
                    items
                        .as_ref()
                        .iter()
                        .any(|i| matches!(i, MessageDataItem::Envelope(_)))
                })
                .count();
        }
        println!(
            "run {run}: header fetch of {fetched} messages in batches of {batch}, decoded into imap-types (no store): {:.1} ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
    client.logout().ok();

    let (mut client, caps) = connect(server, "spike")?;
    if qresync {
        let enabled = client.enable(Vec1::from(CapabilityEnable::try_from("QRESYNC")?))?;
        println!("ENABLE QRESYNC: {enabled:?}");
    }
    let parameters = if condstore {
        vec![SelectParameter::CondStore]
    } else {
        vec![]
    };
    let selected = client.select(Mailbox::Inbox, ImapMailboxSelectOptions { parameters })?;
    let modseq = selected.highest_mod_seq;
    let uidvalidity = selected.uid_validity.context("UIDVALIDITY")?;

    let (mut other, _) = connect(server, "spike")?;
    other.select(Mailbox::Inbox, ImapMailboxSelectOptions::default())?;
    let echoes = other.store(
        SequenceSet::try_from("1:20")?,
        StoreType::Add,
        vec![Flag::Flagged],
        ImapMessageStoreOptions { uid: true },
    )?;
    println!(
        "STORE +FLAGS on UIDs 1:20 by a second client: {} FETCH echoes",
        echoes.len()
    );

    if let Some(modseq) = modseq.and_then(NonZeroU64::new) {
        let changed = client.fetch(
            SequenceSet::try_from("1:*")?,
            MacroOrMessageDataItemNames::MessageDataItemNames(vec![
                MessageDataItemName::Uid,
                MessageDataItemName::Flags,
            ]),
            ImapMessageFetchOptions {
                uid: true,
                modifiers: vec![FetchModifier::ChangedSince(modseq), FetchModifier::Vanished],
            },
        )?;
        println!(
            "UID FETCH 1:* (UID FLAGS) (CHANGEDSINCE m VANISHED): {} changed (VANISHED lines are not returned by fetch)",
            changed.len()
        );
    }

    let missing = client.select(
        Mailbox::try_from("Finnes ikke")?,
        ImapMailboxSelectOptions::default(),
    );
    println!(
        "SELECT of a missing mailbox: {:?}",
        missing.map(|_| "OK").map_err(|e| e.to_string())
    );
    let raw = client.raw(b"X1 UID FETCH 1 (FLAGS BOGUS)\r\n")?;
    println!(
        "raw BAD command: returned a string of {} bytes, BAD seen: {}",
        raw.len(),
        raw.contains("X1 BAD")
    );
    client.select(Mailbox::Inbox, ImapMailboxSelectOptions::default())?;

    let message_id = format!("ioimap.{}@example.test", std::process::id());
    let message = format!(
        "From: <a@example.test>\r\nSubject: io-imap\r\nMessage-ID: <{message_id}>\r\n\r\nbody\r\n"
    );
    let literal_plus = caps.iter().any(|c| matches!(c, Capability::LiteralPlus));
    let (_, appenduid) = client.append(
        Mailbox::Inbox,
        message.as_bytes(),
        ImapMessageAppendOptions {
            flags: vec![],
            date: None,
            non_sync: literal_plus,
        },
    )?;
    println!("APPEND: APPENDUID {appenduid:?}");
    if let Some((_, uid)) = appenduid
        && caps.iter().any(|c| matches!(c, Capability::Move))
    {
        let target = Mailbox::try_from("Arkiv")?;
        let copyuid = client.r#move(
            SequenceSet::try_from(uid)?,
            target,
            ImapMessageMoveOptions { uid: true },
        )?;
        println!("UID MOVE: COPYUID {copyuid:?}");
    }

    if let (Some(modseq), true) = (modseq.and_then(NonZeroU64::new), qresync) {
        other.store(
            SequenceSet::try_from("1:3")?,
            StoreType::Add,
            vec![Flag::Deleted],
            ImapMessageStoreOptions { uid: true },
        )?;
        other.expunge()?;
        let data = client.select_qresync(Mailbox::Inbox, uidvalidity, modseq.get(), &caps)?;
        println!(
            "SELECT (QRESYNC): VANISHED (EARLIER) {} UIDs, {} changed messages",
            data.vanished_earlier.len(),
            data.changed.len()
        );
    }

    let idle = caps.iter().any(|c| matches!(c, Capability::Idle));
    if idle {
        let watch = client.watch_mailbox(
            Mailbox::Inbox,
            &caps,
            ImapMailboxWatchStreamOptions {
                shutdown_poll: Duration::from_millis(200),
                idle_timeout: None,
                poll: None,
            },
        )?;
        std::thread::sleep(Duration::from_millis(500));
        let mut latencies = Vec::new();
        for i in 0..10 {
            let message = format!(
                "From: <a@example.test>\r\nSubject: watch {i}\r\nMessage-ID: <w{i}.{message_id}>\r\n\r\nx\r\n"
            );
            let (_, appended) = other.append(
                Mailbox::Inbox,
                message.as_bytes(),
                ImapMessageAppendOptions::default(),
            )?;
            let appended_at = Instant::now();
            let want = appended.map(|(_, uid)| uid);
            loop {
                match watch.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(io_imap::watch::ImapMailboxWatchEvent::EnvelopeAdded {
                        uid, ..
                    })) if Some(uid.get()) == want => {
                        latencies.push(appended_at.elapsed().as_secs_f64() * 1000.0);
                        break;
                    }
                    Ok(Ok(_)) => {}
                    other => {
                        println!(
                            "watch ended: {:?}",
                            other.map(|r| r.map(|_| ()).map_err(|e| e.to_string()))
                        );
                        break;
                    }
                }
            }
        }
        println!(
            "IDLE watch: APPEND OK -> EnvelopeAdded event: {}",
            email_spike_s1::perf::summary(&latencies)
        );
        watch.close()?;
    }
    other.logout().ok();
    Ok(())
}
