//! A fake IMAP server for the sync engine's tests, on 127.0.0.1 over TLS with a throwaway CA:
//! mailboxes with UIDs, flags, keywords and mod-sequences; EXAMINE, UID SEARCH (ALL, UID,
//! SINCE), UID FETCH of headers or flags with CHANGEDSINCE and VANISHED, LIST with SPECIAL-USE
//! and STATUS, STATUS, UNSELECT, NOOP, IDLE, whole messages for body downloads; EXISTS, EXPUNGE
//! and VANISHED notices before each command's own responses and while idling. Three flavours:
//! QRESYNC, CONDSTORE only (both with IDLE), IMAP4rev1. Tests change its mailboxes between and
//! during passes, cut a connection in the middle of a FETCH, close every idling connection,
//! refuse logins or connections, and turn the network into a black hole that leaves every open
//! connection silently dead.
//! For the op queue's writes (M4): SELECT read-write (or READ-ONLY by mailbox) with
//! PERMANENTFLAGS, UID STORE, UID MOVE and UID COPY with COPYUID untagged, tagged or absent, UID
//! EXPUNGE, EXPUNGE and CLOSE, UID SEARCH UNSEEN and UNDELETED; MOVE and UIDPLUS switched on
//! and off; a command refused with a given reply, cut off before it runs, run with its reply
//! lost, or run and never answered.
//! A STORE in an EXAMINEd mailbox is answered OK and ignored, as Dovecot does (S1).
//! For the Sent copy and drafts (M6): APPEND with a synchronizing or non-synchronizing literal,
//! answered with APPENDUID where UIDPLUS is on; a connection cut part of the way into a literal;
//! `UID SEARCH HEADER Message-ID` that never matches (Stalwart, S1); FETCH by sequence number.
//! For folder management (M9): CREATE, RENAME (with the folders under it), DELETE (refused for
//! a folder with folders under it, as Dovecot does), SUBSCRIBE and UNSUBSCRIBE with the set of
//! subscriptions, LIST patterns with `*` and `%`, and a hierarchy delimiter of choice.
//! For replies async-imap can't parse (issue #20): a message whose header FETCH response has a
//! garbled BODYSTRUCTURE or garbled FLAGS ([`Garble`]); BODYSTRUCTURE only when asked for.
//! Synthetic data only.

#![allow(dead_code)]

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use sealwax_domain::account::{Endpoint, Security};
use sealwax_domain::model::Flags;
use sealwax_engine::imap::{self, ConnectOptions, Connection, Credentials};
use sealwax_engine::net::Trust;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Qresync,
    Condstore,
    Rev1,
}

impl Flavor {
    pub const ALL: [Flavor; 3] = [Flavor::Qresync, Flavor::Condstore, Flavor::Rev1];

    fn caps(self) -> &'static str {
        match self {
            Flavor::Qresync => {
                "IMAP4rev1 LITERAL+ ENABLE UNSELECT UIDPLUS CONDSTORE QRESYNC LIST-EXTENDED \
                 LIST-STATUS SPECIAL-USE IDLE"
            }
            Flavor::Condstore => "IMAP4rev1 LITERAL+ UNSELECT UIDPLUS CONDSTORE SPECIAL-USE IDLE",
            Flavor::Rev1 => "IMAP4rev1",
        }
    }

    fn modseq(self) -> bool {
        self != Flavor::Rev1
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    pub flags: Vec<String>,
    pub modseq: u64,
    /// INTERNALDATE, Unix seconds.
    pub date: i64,
    pub message_id: String,
    /// Header lines besides the fixed ones, each ending in CRLF (rules' tests, ADR-020).
    pub extra: String,
}

#[derive(Clone, Debug, Default)]
pub struct Mailbox {
    pub attributes: Vec<String>,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub modseq: u64,
    pub messages: BTreeMap<u32, Message>,
    /// UIDs expunged and the mod-sequence of their expunge.
    pub expunged: Vec<(u32, u64)>,
}

type Action = Box<dyn FnOnce(&mut World) + Send>;

pub struct World {
    pub flavor: Flavor,
    pub mailboxes: BTreeMap<String, Mailbox>,
    next_validity: u32,
    serial: u64,
    /// Every command received, without its tag.
    pub commands: Vec<String>,
    hooks: Vec<(String, Action)>,
    /// Cut the connection after `after` FETCH responses of a command containing `needle`,
    /// once `skip` such commands have passed.
    cut: Option<Cut>,
    pub cuts: usize,
    /// VANISHED (EARLIER) is never sent: a server that forgets expunges.
    pub forget_vanished: bool,
    /// No EXISTS, EXPUNGE or VANISHED notices.
    pub quiet: bool,
    /// Changes made before an IDLE starts are never announced to it (S1: Stalwart); NOOP still
    /// reports them.
    pub idle_misses_earlier: bool,
    pub refuse_login: bool,
    /// New connections are closed before TLS.
    pub refuse_connections: bool,
    /// When each connection arrived.
    pub connections: Vec<Instant>,
    /// Connections from before this epoch are silently dead.
    dead_before: u64,
    epoch: u64,
    hole: bool,
    /// Bumped to close every connection idling then, as a server restart would.
    closings: u64,
    changes: watch::Sender<u64>,
    /// MOVE is offered (off by default).
    pub moves: bool,
    /// UIDPLUS is offered (on for the QRESYNC and CONDSTORE flavours).
    pub uidplus: bool,
    /// Where MOVE and COPY put COPYUID, with UIDPLUS.
    pub copyuid: CopyUid,
    /// MOVE and COPY give the target UIDs in reverse order and still write both of COPYUID's
    /// sets ascending, so its pairs swap messages: what Stalwart 0.16 does.
    pub copyuid_unordered: bool,
    /// Mailboxes SELECT opens read-only.
    pub read_only: BTreeSet<String>,
    /// PERMANENTFLAGS of a read-write SELECT.
    pub permanent_flags: String,
    faults: Vec<(String, Fault)>,
    /// The next APPEND's connection is cut after this many bytes of its literal.
    literal_cut: Option<usize>,
    /// `UID SEARCH HEADER …` finds nothing, as Stalwart's search of Message-IDs (S1).
    pub header_search_misses: bool,
    /// The hierarchy delimiter LIST reports (`/` by default).
    pub delimiter: char,
    /// The mailboxes SUBSCRIBE named and UNSUBSCRIBE didn't.
    pub subscribed: BTreeSet<String>,
    /// Messages, by mailbox and UID, whose header FETCH responses are garbled.
    pub garbled: BTreeMap<(String, u32), Garble>,
}

/// What a message's header FETCH response gets wrong, so imap-proto can't parse it. A garbled
/// value one level of parentheses deep would be skipped as an unknown attribute; nested, the
/// whole response fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Garble {
    /// A multipart BODYSTRUCTURE with a number where a part's subtype belongs.
    Structure,
    /// A parenthesized list inside FLAGS, whatever else is asked for.
    Flags,
}

struct Cut {
    needle: String,
    skip: usize,
    after: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyUid {
    /// In an untagged OK before the expunges (Dovecot's MOVE, RFC 6851).
    Untagged,
    /// In the tagged OK.
    Tagged,
    Absent,
}

/// What happens to the next command containing a needle.
enum Fault {
    /// Answered with this reply (after the tag) and not run; a reply starting `* BYE` is sent
    /// untagged and the connection closed.
    Refuse(String),
    /// The connection is cut before it runs.
    CutBefore,
    /// It runs, and the connection is cut before its reply.
    LoseReply,
    /// It runs, and the server never answers on that connection again.
    Hold,
}

pub const NOW: i64 = 1_791_158_400;

impl World {
    fn new(flavor: Flavor) -> Self {
        World {
            flavor,
            mailboxes: BTreeMap::new(),
            next_validity: 100,
            serial: 0,
            commands: Vec::new(),
            hooks: Vec::new(),
            cut: None,
            cuts: 0,
            forget_vanished: false,
            quiet: false,
            idle_misses_earlier: false,
            refuse_login: false,
            refuse_connections: false,
            connections: Vec::new(),
            dead_before: 0,
            epoch: 0,
            hole: false,
            closings: 0,
            changes: watch::channel(0).0,
            moves: false,
            uidplus: flavor != Flavor::Rev1,
            copyuid: CopyUid::Untagged,
            copyuid_unordered: false,
            read_only: BTreeSet::new(),
            permanent_flags: "\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*".into(),
            faults: Vec::new(),
            literal_cut: None,
            header_search_misses: false,
            delimiter: '/',
            subscribed: BTreeSet::new(),
            garbled: BTreeMap::new(),
        }
    }

    /// Whether `name` is under `parent` in the hierarchy.
    fn under(&self, name: &str, parent: &str) -> bool {
        name.strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with(self.delimiter))
    }

    /// The mailbox names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.mailboxes.keys().cloned().collect()
    }

    /// The next APPEND's connection is cut after `bytes` of its literal arrived.
    pub fn cut_literal(&mut self, bytes: usize) {
        self.literal_cut = Some(bytes);
    }

    /// The capabilities offered after login.
    fn caps(&self) -> String {
        let mut caps: Vec<&str> = self
            .flavor
            .caps()
            .split(' ')
            .filter(|cap| *cap != "UIDPLUS" || self.uidplus)
            .collect();
        if self.uidplus && !caps.contains(&"UIDPLUS") {
            caps.push("UIDPLUS");
        }
        if self.moves {
            caps.push("MOVE");
        }
        caps.join(" ")
    }

    /// The next command containing `needle` is answered `<tag> <reply>` and not run.
    pub fn refuse(&mut self, needle: &str, reply: &str) {
        self.faults
            .push((needle.to_owned(), Fault::Refuse(reply.to_owned())));
    }

    /// The connection is cut when the next command containing `needle` arrives, before it runs.
    pub fn cut_before(&mut self, needle: &str) {
        self.faults.push((needle.to_owned(), Fault::CutBefore));
    }

    /// The next command containing `needle` runs, and the connection is cut before its reply.
    pub fn lose_reply(&mut self, needle: &str) {
        self.faults.push((needle.to_owned(), Fault::LoseReply));
    }

    /// The next command containing `needle` runs, and its connection goes silent for good.
    pub fn hold(&mut self, needle: &str) {
        self.faults.push((needle.to_owned(), Fault::Hold));
    }

    fn fault(&mut self, command: &str) -> Option<Fault> {
        let at = self
            .faults
            .iter()
            .position(|(needle, _)| command.contains(needle.as_str()))?;
        Some(self.faults.remove(at).1)
    }

    /// Wakes idling sessions to announce what changed.
    fn changed(&self) {
        self.changes.send_modify(|n| *n += 1);
    }

    /// While on, every connection, open or new, stops answering without closing; once off, new
    /// connections work and the old ones stay dead (a suspend, or a network that went away).
    pub fn black_hole(&mut self, on: bool) {
        self.epoch += 1;
        if on {
            self.dead_before = self.epoch;
        }
        self.hole = on;
        self.changed();
    }

    fn dead(&self, epoch: u64) -> bool {
        epoch < self.dead_before
    }

    /// Closes every connection that is idling now, without a TLS close_notify.
    pub fn close_idling(&mut self) {
        self.closings += 1;
        self.changed();
    }

    pub fn create(&mut self, name: &str, attributes: &[&str]) {
        self.next_validity += 1;
        self.mailboxes.insert(
            name.to_owned(),
            Mailbox {
                attributes: attributes.iter().map(|a| a.to_string()).collect(),
                uidvalidity: self.next_validity,
                uidnext: 1,
                modseq: 1,
                ..Mailbox::default()
            },
        );
        self.changed();
    }

    pub fn delete(&mut self, name: &str) {
        self.mailboxes.remove(name);
        self.changed();
    }

    /// Deleted and created again: a new UIDVALIDITY, empty.
    pub fn recreate(&mut self, name: &str) {
        let attributes: Vec<String> = self.mailboxes[name].attributes.clone();
        let attributes: Vec<&str> = attributes.iter().map(String::as_str).collect();
        self.create(name, &attributes);
    }

    fn mailbox(&mut self, name: &str) -> &mut Mailbox {
        self.mailboxes
            .get_mut(name)
            .unwrap_or_else(|| panic!("no mailbox {name}"))
    }

    /// A new message `days` old; returns its UID.
    pub fn append(&mut self, name: &str, days: i64, flags: &[&str]) -> u32 {
        self.serial += 1;
        let message_id = format!("fake.{}@example.test", self.serial);
        self.append_id(name, &message_id, days, flags)
    }

    /// A new message with the given Message-ID (without angle brackets); returns its UID.
    pub fn append_id(&mut self, name: &str, message_id: &str, days: i64, flags: &[&str]) -> u32 {
        let message = Message {
            flags: flags.iter().map(|f| f.to_string()).collect(),
            modseq: 0,
            date: NOW - days * 86_400 + 3_600,
            message_id: message_id.to_owned(),
            extra: String::new(),
        };
        let uid = self.put(name, message);
        self.changed();
        uid
    }

    /// A new message with `extra` header lines (each ending in CRLF); returns its UID.
    pub fn append_extra(&mut self, name: &str, extra: &str) -> u32 {
        let uid = self.append(name, 0, &[]);
        if let Some(message) = self.mailbox(name).messages.get_mut(&uid) {
            message.extra = extra.to_owned();
        }
        uid
    }

    /// Stores `message` under the next UID with a new mod-sequence.
    fn put(&mut self, name: &str, mut message: Message) -> u32 {
        let mailbox = self.mailbox(name);
        let uid = mailbox.uidnext;
        mailbox.uidnext += 1;
        mailbox.modseq += 1;
        message.modseq = mailbox.modseq;
        mailbox.messages.insert(uid, message);
        uid
    }

    /// The Message-IDs of a mailbox's messages, by UID.
    pub fn ids(&self, name: &str) -> Vec<String> {
        self.mailboxes[name]
            .messages
            .values()
            .map(|message| message.message_id.clone())
            .collect()
    }

    /// A message's flags, as stored.
    pub fn flags(&self, name: &str, uid: u32) -> Vec<String> {
        self.mailboxes[name].messages[&uid].flags.clone()
    }

    pub fn append_many(&mut self, name: &str, count: usize, days: impl Fn(usize) -> i64) {
        for n in 0..count {
            self.append(name, days(n), &[]);
        }
    }

    pub fn set_flags(&mut self, name: &str, uid: u32, flags: &[&str]) {
        let mailbox = self.mailbox(name);
        mailbox.modseq += 1;
        let modseq = mailbox.modseq;
        let message = mailbox.messages.get_mut(&uid).expect("a stored UID");
        message.flags = flags.iter().map(|f| f.to_string()).collect();
        message.modseq = modseq;
        self.changed();
    }

    pub fn expunge(&mut self, name: &str, uid: u32) {
        let mailbox = self.mailbox(name);
        if mailbox.messages.remove(&uid).is_some() {
            mailbox.modseq += 1;
            let modseq = mailbox.modseq;
            mailbox.expunged.push((uid, modseq));
        }
        self.changed();
    }

    pub fn uids(&self, name: &str) -> Vec<u32> {
        self.mailboxes[name].messages.keys().copied().collect()
    }

    /// Runs `action` when a command containing `needle` arrives, before it is answered.
    pub fn on(&mut self, needle: &str, action: impl FnOnce(&mut World) + Send + 'static) {
        self.hooks.push((needle.to_owned(), Box::new(action)));
    }

    /// Drops the actions not run yet.
    pub fn clear_hooks(&mut self) {
        self.hooks.clear();
    }

    /// Cuts the connection after `after` FETCH responses of a command with `needle`, once
    /// `skip` such commands have been answered in full.
    pub fn cut(&mut self, needle: &str, skip: usize, after: usize) {
        self.cut = Some(Cut {
            needle: needle.to_owned(),
            skip,
            after,
        });
    }

    /// Every mailbox, copied.
    pub fn mailboxes(&self) -> BTreeMap<String, Mailbox> {
        self.mailboxes.clone()
    }

    /// A mailbox as the store should hold it: UID to flags and keywords.
    pub fn snapshot(&self, name: &str) -> BTreeMap<u32, (Flags, Vec<String>)> {
        self.mailboxes[name]
            .messages
            .iter()
            .map(|(uid, message)| {
                (
                    *uid,
                    Flags::from_imap(message.flags.iter().map(String::as_str)),
                )
            })
            .collect()
    }

    pub fn count(&self, verb: &str) -> usize {
        self.commands
            .iter()
            .filter(|command| command.contains(verb))
            .count()
    }
}

pub struct Fake {
    pub port: u16,
    world: Arc<Mutex<World>>,
    _dir: tempfile::TempDir,
    pub trust: Trust,
}

impl Fake {
    pub async fn start(flavor: Flavor) -> Fake {
        let (dir, trust, server) = tls();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let world = Arc::new(Mutex::new(World::new(flavor)));
        let shared = world.clone();
        let acceptor = tokio_rustls::TlsAcceptor::from(server);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let _ = tcp.set_nodelay(true);
                let epoch = {
                    let mut world = shared.lock().unwrap_or_else(|e| e.into_inner());
                    world.connections.push(Instant::now());
                    if world.refuse_connections {
                        continue;
                    }
                    if world.hole {
                        0
                    } else {
                        world.epoch.max(world.dead_before)
                    }
                };
                let acceptor = acceptor.clone();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let boxed: Box<dyn Io> = Box::new(stream);
                    session(BufReader::new(boxed), shared, epoch).await;
                });
            }
        });
        Fake {
            port,
            world,
            _dir: dir,
            trust,
        }
    }

    pub fn world(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint {
            host: "127.0.0.1".into(),
            port: self.port,
            security: Security::Tls,
            certificate: None,
        }
    }

    pub async fn connect(&self) -> Connection {
        self.connect_with(ConnectOptions::default()).await
    }

    pub async fn connect_with(&self, options: ConnectOptions) -> Connection {
        imap::connect_with(
            &self.endpoint(),
            Credentials::Password {
                username: "test",
                password: "fake-password",
            },
            self.trust.client_config().unwrap(),
            options,
        )
        .await
        .unwrap()
    }
}

fn tls() -> (tempfile::TempDir, Trust, Arc<ServerConfig>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.signed_by(&key, &ca).unwrap();
    let server =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ca.pem");
    std::fs::write(&path, ca.pem()).unwrap();
    (dir, Trust::Roots(path), Arc::new(server))
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// One connection's view: the selected mailbox and the UIDs it was told about, in order (their
/// positions are the sequence numbers).
#[derive(Default)]
struct Session {
    selected: Option<(String, u32)>,
    /// Selected by SELECT, not EXAMINE, and not read-only.
    read_write: bool,
    known: Vec<u32>,
    qresync: bool,
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => text.to_owned(),
    }
}

fn uid_set(uids: &[u32]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < uids.len() {
        let start = uids[i];
        let mut end = start;
        while i + 1 < uids.len() && uids[i + 1] == end + 1 {
            i += 1;
            end = uids[i];
        }
        out.push(if start == end {
            start.to_string()
        } else {
            format!("{start}:{end}")
        });
        i += 1;
    }
    out.join(",")
}

/// A sequence set as ranges, `*` being `star`.
fn parse_set(text: &str, star: u32) -> Vec<(u32, u32)> {
    text.split(',')
        .map(|part| {
            let number = |n: &str| if n == "*" { star } else { n.parse().unwrap() };
            match part.split_once(':') {
                Some((a, b)) => {
                    let (a, b) = (number(a), number(b));
                    (a.min(b), a.max(b))
                }
                None => (number(part), number(part)),
            }
        })
        .collect()
}

fn in_set(set: &[(u32, u32)], uid: u32) -> bool {
    set.iter().any(|(a, b)| (*a..=*b).contains(&uid))
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn civil(unix: i64) -> (i64, usize, i64, i64) {
    let days = unix.div_euclid(86_400);
    let seconds = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, (month - 1) as usize, day, seconds)
}

fn internal_date(unix: i64) -> String {
    let (year, month, day, seconds) = civil(unix);
    format!(
        "{day:02}-{}-{year} {:02}:{:02}:{:02} +0000",
        MONTHS[month],
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// `7-Jul-2026` as the Unix seconds of that day's start.
fn search_date(text: &str) -> i64 {
    let mut parts = text.split('-');
    let day: i64 = parts.next().unwrap().parse().unwrap();
    let month = parts.next().unwrap();
    let month = MONTHS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(month))
        .unwrap() as i64
        + 1;
    let year: i64 = parts.next().unwrap().parse().unwrap();
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400
}

fn header(message: &Message, uid: u32) -> String {
    format!(
        "Date: Mon, 5 Oct 2026 09:00:00 +0000\r\nFrom: \"Fake Sender\" <sender@example.test>\r\n\
         To: <test@example.test>\r\nSubject: Fake message {uid}\r\nMessage-ID: <{}>\r\n{}\
         MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n",
        message.message_id, message.extra
    )
}

const TEXT: &str = "Synthetic body text of a fake message.\r\n";

/// The header fields of `full` that `fields` names, with the blank line that ends a header.
fn header_fields(full: &str, fields: &str) -> String {
    let wanted: Vec<String> = fields
        .split_whitespace()
        .map(|f| f.to_ascii_lowercase())
        .collect();
    let mut out = String::new();
    for line in full.split("\r\n").filter(|line| !line.is_empty()) {
        if let Some((name, _)) = line.split_once(':')
            && wanted.contains(&name.to_ascii_lowercase())
        {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out.push_str("\r\n");
    out
}

fn flag_list(message: &Message) -> String {
    message.flags.join(" ")
}

/// The text of a parenthesized group starting at `text`'s first byte, and what follows it.
fn group(text: &str) -> (&str, &str) {
    let mut depth = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return (&text[1..i], text[i + 1..].trim_start());
                }
            }
            _ => {}
        }
    }
    (text, "")
}

/// EXISTS, EXPUNGE and VANISHED for what changed in the selected mailbox since the session last
/// heard.
/// The session's selected mailbox was deleted, or deleted and created again.
fn selected_gone(world: &World, session: &Session) -> bool {
    session.selected.as_ref().is_some_and(|(name, validity)| {
        world
            .mailboxes
            .get(name)
            .is_none_or(|mailbox| mailbox.uidvalidity != *validity)
    })
}

fn notices(world: &World, session: &mut Session, out: &mut String) {
    let Some((name, validity)) = session.selected.clone() else {
        return;
    };
    let Some(mailbox) = world
        .mailboxes
        .get(&name)
        .filter(|m| m.uidvalidity == validity)
    else {
        return;
    };
    if world.quiet {
        return;
    }
    let gone: Vec<(usize, u32)> = session
        .known
        .iter()
        .enumerate()
        .filter(|(_, uid)| !mailbox.messages.contains_key(uid))
        .map(|(i, uid)| (i + 1, *uid))
        .collect();
    if !gone.is_empty() {
        if session.qresync {
            let uids: Vec<u32> = gone.iter().map(|(_, uid)| *uid).collect();
            out.push_str(&format!("* VANISHED {}\r\n", uid_set(&uids)));
        } else {
            for (seq, _) in gone.iter().rev() {
                out.push_str(&format!("* {seq} EXPUNGE\r\n"));
            }
        }
        session
            .known
            .retain(|uid| mailbox.messages.contains_key(uid));
    }
    let last = session.known.last().copied().unwrap_or(0);
    let new: Vec<u32> = mailbox
        .messages
        .keys()
        .copied()
        .filter(|uid| *uid > last)
        .collect();
    if !new.is_empty() {
        session.known.extend(new);
        out.push_str(&format!("* {} EXISTS\r\n", session.known.len()));
    }
}

fn status_line(world: &World, name: &str, items: &str) -> Option<String> {
    let mailbox = world.mailboxes.get(name)?;
    let mut parts = Vec::new();
    for item in items.split_whitespace() {
        match item.to_ascii_uppercase().as_str() {
            "MESSAGES" => parts.push(format!("MESSAGES {}", mailbox.messages.len())),
            "UNSEEN" => parts.push(format!(
                "UNSEEN {}",
                mailbox
                    .messages
                    .values()
                    .filter(|m| !m.flags.iter().any(|f| f.eq_ignore_ascii_case("\\Seen")))
                    .count()
            )),
            "UIDNEXT" => parts.push(format!("UIDNEXT {}", mailbox.uidnext)),
            "UIDVALIDITY" => parts.push(format!("UIDVALIDITY {}", mailbox.uidvalidity)),
            "HIGHESTMODSEQ" if world.flavor.modseq() => {
                parts.push(format!("HIGHESTMODSEQ {}", mailbox.modseq));
            }
            _ => {}
        }
    }
    Some(format!(
        "* STATUS {} ({})\r\n",
        quoted(name),
        parts.join(" ")
    ))
}

fn selectable(mailbox: &Mailbox) -> bool {
    !mailbox
        .attributes
        .iter()
        .any(|a| a.eq_ignore_ascii_case("\\Noselect"))
}

/// A command's arguments: quoted strings and atoms, up to the first parenthesis.
fn arguments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.trim_start().chars().peekable();
    while let Some(&first) = chars.peek() {
        match first {
            ' ' => {
                chars.next();
            }
            '(' => break,
            '"' => {
                chars.next();
                let mut word = String::new();
                while let Some(ch) = chars.next() {
                    match ch {
                        '\\' => word.extend(chars.next()),
                        '"' => break,
                        other => word.push(other),
                    }
                }
                out.push(word);
            }
            _ => {
                let mut word = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch == ' ' || ch == '(' {
                        break;
                    }
                    word.push(ch);
                    chars.next();
                }
                out.push(word);
            }
        }
    }
    out
}

/// LIST's pattern: `*` matches anything, `%` anything but the delimiter.
fn matches_pattern(pattern: &str, name: &str, delimiter: char) -> bool {
    fn go(pattern: &[char], name: &[char], delimiter: char) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some(('*', rest)) => (0..=name.len()).any(|skip| go(rest, &name[skip..], delimiter)),
            Some(('%', rest)) => (0..=name.len())
                .take_while(|skip| *skip == 0 || name[skip - 1] != delimiter)
                .any(|skip| go(rest, &name[skip..], delimiter)),
            Some((ch, rest)) => name
                .split_first()
                .is_some_and(|(first, tail)| first == ch && go(rest, tail, delimiter)),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    go(&pattern, &name, delimiter)
}

/// CREATE, DELETE, RENAME, SUBSCRIBE and UNSUBSCRIBE; the refusal's text after the tag.
fn manage(world: &mut World, verb: &str, names: &[String]) -> Result<(), String> {
    let exists = |world: &World, name: &str| {
        world.mailboxes.keys().any(|known| {
            known == name
                || (known.eq_ignore_ascii_case("INBOX") && known.eq_ignore_ascii_case(name))
        })
    };
    let name = names.first().ok_or("BAD missing mailbox")?.clone();
    match verb {
        "CREATE" => {
            if exists(world, &name) {
                return Err("NO [ALREADYEXISTS] Mailbox already exists".into());
            }
            world.create(&name, &[]);
        }
        "DELETE" => {
            if name.eq_ignore_ascii_case("INBOX") {
                return Err("NO [CANNOT] INBOX can't be deleted".into());
            }
            if !exists(world, &name) {
                return Err("NO [NONEXISTENT] Mailbox doesn't exist".into());
            }
            if world
                .mailboxes
                .keys()
                .any(|other| world.under(other, &name))
            {
                return Err("NO Mailbox has children, delete them first".into());
            }
            world.delete(&name);
        }
        "RENAME" => {
            let to = names.get(1).ok_or("BAD missing new name")?.clone();
            if !exists(world, &name) {
                return Err("NO [NONEXISTENT] Mailbox doesn't exist".into());
            }
            if exists(world, &to) {
                return Err("NO [ALREADYEXISTS] Target mailbox already exists".into());
            }
            let moving: Vec<String> = world
                .mailboxes
                .keys()
                .filter(|known| **known == name || world.under(known, &name))
                .cloned()
                .collect();
            for old in moving {
                let mailbox = world.mailboxes.remove(&old).expect("listed");
                world
                    .mailboxes
                    .insert(format!("{to}{}", &old[name.len()..]), mailbox);
            }
            world.changed();
        }
        "SUBSCRIBE" => {
            world.subscribed.insert(name);
        }
        _ => {
            world.subscribed.remove(&name);
        }
    }
    Ok(())
}

enum Reply {
    Text(String),
    /// Send the text, then drop the connection without a reply.
    Cut(String),
    /// Answer nothing more on this connection.
    Silent,
}

fn answer(world: &mut World, session: &mut Session, tag: &str, command: &str) -> Reply {
    let mut out = String::new();
    notices(world, session, &mut out);
    let upper = command.to_ascii_uppercase();
    let verb = upper.split_whitespace().next().unwrap_or("").to_owned();
    let ok = |out: &mut String, text: &str| out.push_str(&format!("{tag} OK {text}\r\n"));
    match verb.as_str() {
        "CAPABILITY" => {
            out.push_str(&format!("* CAPABILITY {}\r\n", world.caps()));
            ok(&mut out, "done");
        }
        "ENABLE" => {
            if world.flavor == Flavor::Qresync && upper.contains("QRESYNC") {
                session.qresync = true;
                out.push_str("* ENABLED QRESYNC\r\n");
            }
            ok(&mut out, "enabled");
        }
        "NOOP" => ok(&mut out, "done"),
        "UNSELECT" => {
            session.selected = None;
            session.read_write = false;
            session.known.clear();
            ok(&mut out, "done");
        }
        "LIST" => {
            let status = upper
                .find("STATUS (")
                .map(|at| group(&command[at + 7..]).0.to_owned());
            let pattern = arguments(&command[4..])
                .get(1)
                .cloned()
                .unwrap_or_else(|| "*".to_owned());
            for (name, mailbox) in &world.mailboxes {
                if !matches_pattern(&pattern, name, world.delimiter) {
                    continue;
                }
                let mut attributes = mailbox.attributes.clone();
                let children = world.mailboxes.keys().any(|other| world.under(other, name));
                attributes.push(if children {
                    "\\HasChildren".into()
                } else {
                    "\\HasNoChildren".into()
                });
                out.push_str(&format!(
                    "* LIST ({}) {} {}\r\n",
                    attributes.join(" "),
                    quoted(&world.delimiter.to_string()),
                    quoted(name)
                ));
                if let Some(items) = &status
                    && selectable(mailbox)
                {
                    out.push_str(&status_line(world, name, items).unwrap());
                }
            }
            ok(&mut out, "listed");
        }
        "STATUS" => {
            let rest = command[6..].trim_start();
            let at = rest.rfind('(').unwrap();
            let name = unquote(&rest[..at]);
            let items = group(&rest[at..]).0;
            match status_line(world, &name, items) {
                Some(line) => {
                    out.push_str(&line);
                    ok(&mut out, "done");
                }
                None => out.push_str(&format!("{tag} NO [NONEXISTENT] no such mailbox\r\n")),
            }
        }
        "EXAMINE" | "SELECT" => {
            let rest = command[verb.len()..].trim();
            let name = match rest.find(" (") {
                Some(at) => unquote(&rest[..at]),
                None => unquote(rest),
            };
            session.selected = None;
            session.read_write = false;
            session.known.clear();
            let Some(mailbox) = world.mailboxes.get(&name).filter(|m| selectable(m)) else {
                out.push_str(&format!("{tag} NO [NONEXISTENT] no such mailbox\r\n"));
                return Reply::Text(out);
            };
            let read_write = verb == "SELECT" && !world.read_only.contains(&name);
            session.known = mailbox.messages.keys().copied().collect();
            session.selected = Some((name, mailbox.uidvalidity));
            session.read_write = read_write;
            out.push_str("* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n");
            if read_write {
                out.push_str(&format!(
                    "* OK [PERMANENTFLAGS ({})] flags\r\n",
                    world.permanent_flags
                ));
            } else {
                out.push_str("* OK [PERMANENTFLAGS ()] read-only\r\n");
            }
            out.push_str(&format!(
                "* {} EXISTS\r\n* 0 RECENT\r\n",
                session.known.len()
            ));
            out.push_str(&format!(
                "* OK [UIDVALIDITY {}] ok\r\n* OK [UIDNEXT {}] ok\r\n",
                mailbox.uidvalidity, mailbox.uidnext
            ));
            if world.flavor.modseq() {
                out.push_str(&format!("* OK [HIGHESTMODSEQ {}] ok\r\n", mailbox.modseq));
            }
            if read_write {
                out.push_str(&format!("{tag} OK [READ-WRITE] selected\r\n"));
            } else {
                out.push_str(&format!("{tag} OK [READ-ONLY] examined\r\n"));
            }
        }
        "EXPUNGE" | "CLOSE" => {
            let Some((name, _)) = session.selected.clone() else {
                out.push_str(&format!("{tag} BAD no mailbox selected\r\n"));
                return Reply::Text(out);
            };
            if session.read_write {
                let deleted: Vec<u32> = world.mailboxes[&name]
                    .messages
                    .iter()
                    .filter(|(_, message)| has_flag(message, "\\Deleted"))
                    .map(|(uid, _)| *uid)
                    .collect();
                for uid in deleted {
                    world.expunge(&name, uid);
                }
            }
            if verb == "CLOSE" {
                session.selected = None;
                session.read_write = false;
                session.known.clear();
            } else {
                notices(world, session, &mut out);
            }
            ok(&mut out, "done");
        }
        "UID" => return uid_command(world, session, tag, command, out),
        "FETCH" => {
            let rest = command[5..].trim_start();
            let (set_text, items) = rest.split_once(' ').unwrap();
            let set = parse_set(set_text, session.known.len() as u32);
            let uids: Vec<u32> = session
                .known
                .iter()
                .enumerate()
                .filter(|(i, _)| in_set(&set, *i as u32 + 1))
                .map(|(_, uid)| *uid)
                .collect();
            if uids.is_empty() {
                ok(&mut out, "fetched");
                return Reply::Text(out);
            }
            let command = format!("UID FETCH {} {items}", uid_set(&uids));
            return uid_command(world, session, tag, &command, out);
        }
        "LOGOUT" => {
            out.push_str("* BYE bye\r\n");
            ok(&mut out, "done");
        }
        "CREATE" | "DELETE" | "RENAME" | "SUBSCRIBE" | "UNSUBSCRIBE" => {
            let names = arguments(&command[verb.len()..]);
            match manage(world, &verb, &names) {
                Ok(()) => ok(&mut out, "done"),
                Err(refusal) => out.push_str(&format!("{tag} {refusal}\r\n")),
            }
        }
        _ => out.push_str(&format!("{tag} BAD unknown command\r\n")),
    }
    Reply::Text(out)
}

fn uid_command(
    world: &mut World,
    session: &mut Session,
    tag: &str,
    command: &str,
    mut out: String,
) -> Reply {
    let Some((name, _)) = session.selected.clone() else {
        out.push_str(&format!("{tag} BAD no mailbox selected\r\n"));
        return Reply::Text(out);
    };
    let mailbox = world.mailboxes[&name].clone();
    let star = mailbox.messages.keys().last().copied();
    let rest = command[3..].trim_start();
    let upper = rest.to_ascii_uppercase();
    if upper.starts_with("SEARCH") {
        let tokens: Vec<&str> = rest[6..].split_whitespace().collect();
        let mut set = None;
        let mut since = None;
        let mut unseen = false;
        let mut undeleted = false;
        let mut header = None;
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].to_ascii_uppercase().as_str() {
                "HEADER" => {
                    header = Some(unquote(tokens[i + 2]));
                    i += 2;
                }
                "UID" => {
                    set = Some(tokens[i + 1]);
                    i += 1;
                }
                "SINCE" => {
                    since = Some(search_date(tokens[i + 1]));
                    i += 1;
                }
                "UNSEEN" => unseen = true,
                "UNDELETED" => undeleted = true,
                _ => {}
            }
            i += 1;
        }
        let found: Vec<String> = match star {
            None => Vec::new(),
            Some(star) => {
                let set = set.map(|set| parse_set(set, star));
                mailbox
                    .messages
                    .iter()
                    .filter(|(uid, _)| set.as_ref().is_none_or(|set| in_set(set, **uid)))
                    .filter(|(_, message)| since.is_none_or(|since| message.date >= since))
                    .filter(|(_, message)| !unseen || !has_flag(message, "\\Seen"))
                    .filter(|(_, message)| !undeleted || !has_flag(message, "\\Deleted"))
                    .filter(|(_, message)| {
                        header.as_ref().is_none_or(|needle| {
                            !world.header_search_misses
                                && format!("<{}>", message.message_id).contains(needle.as_str())
                        })
                    })
                    .map(|(uid, _)| uid.to_string())
                    .collect()
            }
        };
        let line = if found.is_empty() {
            "* SEARCH\r\n".to_owned()
        } else {
            format!("* SEARCH {}\r\n", found.join(" "))
        };
        out.push_str(&line);
        out.push_str(&format!("{tag} OK searched\r\n"));
        return Reply::Text(out);
    }
    if upper.starts_with("STORE") {
        return store(world, session, tag, &name, &rest[5..], out);
    }
    if upper.starts_with("MOVE") || upper.starts_with("COPY") {
        return transfer(world, session, tag, &name, rest, out);
    }
    if upper.starts_with("EXPUNGE") {
        if !world.uidplus {
            out.push_str(&format!("{tag} BAD UID EXPUNGE without UIDPLUS\r\n"));
            return Reply::Text(out);
        }
        if !session.read_write {
            out.push_str(&format!("{tag} NO [READ-ONLY] mailbox is read-only\r\n"));
            return Reply::Text(out);
        }
        let set = parse_set(rest[7..].trim(), star.unwrap_or(0));
        let deleted: Vec<u32> = mailbox
            .messages
            .iter()
            .filter(|(uid, message)| in_set(&set, **uid) && has_flag(message, "\\Deleted"))
            .map(|(uid, _)| *uid)
            .collect();
        for uid in deleted {
            world.expunge(&name, uid);
        }
        notices(world, session, &mut out);
        out.push_str(&format!("{tag} OK expunged\r\n"));
        return Reply::Text(out);
    }
    if !upper.starts_with("FETCH") {
        out.push_str(&format!("{tag} BAD unknown UID command\r\n"));
        return Reply::Text(out);
    }
    let rest = rest[5..].trim_start();
    let (set_text, rest) = rest.split_once(' ').unwrap();
    let (items, modifiers) = group(rest);
    let modifiers = if modifiers.starts_with('(') {
        group(modifiers).0.to_ascii_uppercase()
    } else {
        String::new()
    };
    let mut changedsince = None;
    if !modifiers.is_empty() {
        if !world.flavor.modseq() {
            out.push_str(&format!("{tag} BAD no CONDSTORE\r\n"));
            return Reply::Text(out);
        }
        let mut words = modifiers.split_whitespace();
        while let Some(word) = words.next() {
            if word == "CHANGEDSINCE" {
                changedsince = words.next().map(|m| m.parse::<u64>().unwrap());
            }
        }
    }
    let vanished = modifiers.contains("VANISHED");
    if vanished && !session.qresync {
        out.push_str(&format!("{tag} BAD VANISHED without QRESYNC\r\n"));
        return Reply::Text(out);
    }
    let set = parse_set(set_text, star.unwrap_or(0));
    if let (Some(since), true) = (changedsince, vanished)
        && !world.forget_vanished
    {
        let explicit = parse_set(set_text, u32::MAX);
        let mut gone: Vec<u32> = mailbox
            .expunged
            .iter()
            .filter(|(uid, modseq)| *modseq > since && in_set(&explicit, *uid))
            .map(|(uid, _)| *uid)
            .collect();
        gone.sort_unstable();
        gone.dedup();
        if !gone.is_empty() {
            out.push_str(&format!("* VANISHED (EARLIER) {}\r\n", uid_set(&gone)));
        }
    }
    let whole = items.to_ascii_uppercase().contains("BODY.PEEK[]");
    let headers = items.to_ascii_uppercase().contains("HEADER.FIELDS");
    let header_only = items.to_ascii_uppercase().contains("BODY.PEEK[HEADER]");
    let with_structure = items.to_ascii_uppercase().contains("BODYSTRUCTURE");
    let fields = items
        .find("HEADER.FIELDS (")
        .map(|at| group(&items[at + 14..]).0.to_owned())
        .unwrap_or_default();
    let mut cut = None;
    if let Some(armed) = world
        .cut
        .as_mut()
        .filter(|armed| command.contains(armed.needle.as_str()))
    {
        if armed.skip > 0 {
            armed.skip -= 1;
        } else {
            cut = Some(armed.after);
            world.cut = None;
        }
    }
    let mut sent = 0;
    for (uid, message) in &mailbox.messages {
        if star.is_none() || !in_set(&set, *uid) {
            continue;
        }
        if changedsince.is_some_and(|since| message.modseq <= since) {
            continue;
        }
        let Some(seq) = session.known.iter().position(|known| known == uid) else {
            continue;
        };
        let seq = seq + 1;
        if whole {
            let full = format!("{}{TEXT}", header(message, *uid));
            out.push_str(&format!(
                "* {seq} FETCH (UID {uid} BODY[] {{{}}}\r\n{full})\r\n",
                full.len()
            ));
        } else if header_only {
            let head = header(message, *uid);
            out.push_str(&format!(
                "* {seq} FETCH (UID {uid} BODY[HEADER] {{{}}}\r\n{head})\r\n",
                head.len()
            ));
        } else if headers {
            let full = header(message, *uid);
            let head = header_fields(&full, &fields);
            let size = full.len() + TEXT.len();
            let garble = world.garbled.get(&(name.clone(), *uid)).copied();
            let flags = match garble {
                Some(Garble::Flags) => "\\Seen (garbled)".to_owned(),
                _ => flag_list(message),
            };
            let structure = match (with_structure, garble) {
                (false, _) => String::new(),
                (true, Some(Garble::Structure)) => "BODYSTRUCTURE ((\"text\" 7 1)\
                     (\"text\" \"html\" NIL NIL NIL \"7bit\" 1 1) \"alternative\") "
                    .to_owned(),
                (true, _) => format!(
                    "BODYSTRUCTURE (\"text\" \"plain\" (\"charset\" \"utf-8\") NIL NIL \"7bit\" {} 1) ",
                    TEXT.len()
                ),
            };
            out.push_str(&format!(
                "* {seq} FETCH (UID {uid} FLAGS ({flags}) INTERNALDATE \"{}\" RFC822.SIZE {size} \
                 {structure}BODY[HEADER.FIELDS ({fields})] {{{}}}\r\n{head} BODY[TEXT]<0> {{{}}}\r\n{TEXT})\r\n",
                internal_date(message.date),
                head.len(),
                TEXT.len(),
            ));
        } else if changedsince.is_some() {
            out.push_str(&format!(
                "* {seq} FETCH (UID {uid} FLAGS ({}) MODSEQ ({}))\r\n",
                flag_list(message),
                message.modseq
            ));
        } else {
            out.push_str(&format!(
                "* {seq} FETCH (UID {uid} FLAGS ({}))\r\n",
                flag_list(message)
            ));
        }
        sent += 1;
        if cut.is_some_and(|after| sent >= after) {
            world.cuts += 1;
            return Reply::Cut(out);
        }
    }
    out.push_str(&format!("{tag} OK fetched\r\n"));
    Reply::Text(out)
}

fn has_flag(message: &Message, flag: &str) -> bool {
    message.flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
}

/// `UID STORE <set> (+|-)FLAGS[.SILENT] (<flags>)` on the selected mailbox. In a mailbox that
/// isn't selected read-write it answers OK and changes nothing, as Dovecot does (S1).
fn store(
    world: &mut World,
    session: &mut Session,
    tag: &str,
    name: &str,
    rest: &str,
    mut out: String,
) -> Reply {
    let (set_text, rest) = rest.trim_start().split_once(' ').unwrap();
    let (operation, list) = rest.split_once(' ').unwrap();
    let operation = operation.to_ascii_uppercase();
    let flags: Vec<String> = group(list)
        .0
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if !session.read_write {
        out.push_str(&format!(
            "{tag} OK Store ignored with read-only mailbox.\r\n"
        ));
        return Reply::Text(out);
    }
    let star = world.mailboxes[name].messages.keys().last().copied();
    let set = parse_set(set_text, star.unwrap_or(0));
    let uids: Vec<u32> = world.mailboxes[name]
        .messages
        .keys()
        .copied()
        .filter(|uid| in_set(&set, *uid))
        .collect();
    for uid in uids {
        let mailbox = world.mailbox(name);
        let next = mailbox.modseq + 1;
        let message = mailbox.messages.get_mut(&uid).unwrap();
        let mut now = message.flags.clone();
        if operation.starts_with('+') {
            for flag in &flags {
                if !now.iter().any(|f| f.eq_ignore_ascii_case(flag)) {
                    now.push(flag.clone());
                }
            }
        } else if operation.starts_with('-') {
            now.retain(|f| !flags.iter().any(|flag| flag.eq_ignore_ascii_case(f)));
        } else {
            now = flags.clone();
        }
        let changed = now != message.flags;
        if changed {
            message.flags = now;
            message.modseq = next;
        }
        let line = message.flags.join(" ");
        if changed {
            mailbox.modseq = next;
        }
        if !operation.ends_with(".SILENT")
            && let Some(seq) = session.known.iter().position(|known| *known == uid)
        {
            out.push_str(&format!(
                "* {} FETCH (UID {uid} FLAGS ({line}))\r\n",
                seq + 1
            ));
        }
    }
    world.changed();
    out.push_str(&format!("{tag} OK stored\r\n"));
    Reply::Text(out)
}

/// `UID MOVE` or `UID COPY <set> <mailbox>`: the messages get the target's next UIDs in UID
/// order, with COPYUID where [`World::copyuid`] puts it (with UIDPLUS); a MOVE expunges them from
/// the source and announces that before its tagged OK.
fn transfer(
    world: &mut World,
    session: &mut Session,
    tag: &str,
    name: &str,
    rest: &str,
    mut out: String,
) -> Reply {
    let moving = rest.to_ascii_uppercase().starts_with("MOVE");
    if moving && !world.moves {
        out.push_str(&format!("{tag} BAD MOVE isn't offered\r\n"));
        return Reply::Text(out);
    }
    let (set_text, target) = rest[4..].trim_start().split_once(' ').unwrap();
    let target = unquote(target);
    if moving && !session.read_write {
        out.push_str(&format!("{tag} NO [READ-ONLY] mailbox is read-only\r\n"));
        return Reply::Text(out);
    }
    let Some(validity) = world
        .mailboxes
        .get(&target)
        .filter(|mailbox| selectable(mailbox))
        .map(|mailbox| mailbox.uidvalidity)
    else {
        out.push_str(&format!("{tag} NO [TRYCREATE] no such mailbox\r\n"));
        return Reply::Text(out);
    };
    if target == name {
        out.push_str(&format!("{tag} NO [CANNOT] same mailbox\r\n"));
        return Reply::Text(out);
    }
    let star = world.mailboxes[name].messages.keys().last().copied();
    let set = parse_set(set_text, star.unwrap_or(0));
    let mut picked: Vec<(u32, Message)> = world.mailboxes[name]
        .messages
        .iter()
        .filter(|(uid, _)| in_set(&set, **uid))
        .map(|(uid, message)| (*uid, message.clone()))
        .collect();
    if world.copyuid_unordered {
        picked.reverse();
    }
    let (mut sources, mut targets) = (Vec::new(), Vec::new());
    for (uid, message) in picked {
        targets.push(world.put(&target, message));
        sources.push(uid);
        if moving {
            world.expunge(name, uid);
        }
    }
    world.changed();
    if world.copyuid_unordered {
        sources.sort_unstable();
        targets.sort_unstable();
    }
    let code =
        (!sources.is_empty() && world.uidplus && world.copyuid != CopyUid::Absent).then(|| {
            format!(
                "[COPYUID {validity} {} {}]",
                uid_set(&sources),
                uid_set(&targets)
            )
        });
    match code {
        Some(code) if world.copyuid == CopyUid::Untagged => {
            out.push_str(&format!("* OK {code} done\r\n"));
            notices(world, session, &mut out);
            out.push_str(&format!("{tag} OK done\r\n"));
        }
        Some(code) => {
            notices(world, session, &mut out);
            out.push_str(&format!("{tag} OK {code} done\r\n"));
        }
        None => {
            notices(world, session, &mut out);
            out.push_str(&format!("{tag} OK done\r\n"));
        }
    }
    Reply::Text(out)
}

/// Logs a command and runs the actions waiting for it, before it is answered.
fn arrived(world: &mut World, command: &str) {
    world.commands.push(command.to_owned());
    let due: Vec<usize> = world
        .hooks
        .iter()
        .enumerate()
        .filter(|(_, (needle, _))| command.contains(needle.as_str()))
        .map(|(i, _)| i)
        .collect();
    for i in due.into_iter().rev() {
        let (_, action) = world.hooks.remove(i);
        action(world);
    }
}

/// A dead connection: it reads and drops whatever comes until the client closes it.
async fn silent(mut io: BufReader<Box<dyn Io>>) {
    let mut sink = [0u8; 4096];
    while io.read(&mut sink).await.unwrap_or(0) > 0 {}
}

/// How an IDLE ended on the server's side.
enum Idle {
    Done,
    /// The connection went silent (a black hole).
    Dead,
    /// The server closed the connection.
    Closed,
}

/// IDLE until DONE: announces changes as they happen.
async fn idle(
    io: &mut BufReader<Box<dyn Io>>,
    world: &Arc<Mutex<World>>,
    state: &mut Session,
    changes: &mut watch::Receiver<u64>,
    tag: &str,
    epoch: u64,
) -> Idle {
    let (first, closings) = {
        let world = world.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = String::from("+ idling\r\n");
        if world.idle_misses_earlier {
            // Mark what changed before IDLE as told, without telling.
            let mut ignored = String::new();
            notices(&world, state, &mut ignored);
        } else {
            notices(&world, state, &mut out);
        }
        (out, world.closings)
    };
    if write(io, &first).await.is_err() {
        return Idle::Dead;
    }
    changes.borrow_and_update();
    loop {
        let mut line = String::new();
        tokio::select! {
            read = io.read_line(&mut line) => {
                if read.unwrap_or(0) == 0 {
                    return Idle::Dead;
                }
                let out = {
                    let mut world = world.lock().unwrap_or_else(|e| e.into_inner());
                    if world.dead(epoch) {
                        return Idle::Dead;
                    }
                    world.commands.push(line.trim_end().to_owned());
                    let mut out = String::new();
                    notices(&world, state, &mut out);
                    out.push_str(&format!("{tag} OK idle done\r\n"));
                    out
                };
                return if write(io, &out).await.is_ok() { Idle::Done } else { Idle::Dead };
            }
            changed = changes.changed() => {
                if changed.is_err() {
                    return Idle::Dead;
                }
                let (out, gone) = {
                    let world = world.lock().unwrap_or_else(|e| e.into_inner());
                    if world.dead(epoch) {
                        return Idle::Dead;
                    }
                    if world.closings > closings {
                        return Idle::Closed;
                    }
                    if selected_gone(&world, state) {
                        // As Dovecot does when the selected mailbox is deleted.
                        ("* BYE the selected mailbox was deleted\r\n".to_owned(), true)
                    } else {
                        let mut out = String::new();
                        notices(&world, state, &mut out);
                        (out, false)
                    }
                };
                if !out.is_empty() && write(io, &out).await.is_err() {
                    return Idle::Dead;
                }
                if gone {
                    return Idle::Closed;
                }
            }
        }
    }
}

async fn session(mut io: BufReader<Box<dyn Io>>, world: Arc<Mutex<World>>, epoch: u64) {
    let (caps, mut changes, dead) = {
        let world = world.lock().unwrap_or_else(|e| e.into_inner());
        (
            world.caps(),
            world.changes.subscribe(),
            world.dead(epoch) || epoch == 0 && world.hole,
        )
    };
    if dead {
        return silent(io).await;
    }
    let greeting = format!("* OK [CAPABILITY {caps} AUTH=PLAIN] fake ready\r\n");
    if write(&mut io, &greeting).await.is_err() {
        return;
    }
    let mut state = Session::default();
    loop {
        let mut line = String::new();
        match io.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if world.lock().unwrap_or_else(|e| e.into_inner()).dead(epoch) {
            return silent(io).await;
        }
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        let Some((tag, command)) = line.split_once(' ') else {
            continue;
        };
        if command.to_ascii_uppercase().starts_with("AUTHENTICATE") {
            if write(&mut io, "+ \r\n").await.is_err() {
                return;
            }
            let mut response = String::new();
            if io.read_line(&mut response).await.unwrap_or(0) == 0 {
                return;
            }
            let refused = {
                let mut world = world.lock().unwrap_or_else(|e| e.into_inner());
                world.commands.push("AUTHENTICATE PLAIN".into());
                world.refuse_login
            };
            let reply = if refused {
                format!("{tag} NO [AUTHENTICATIONFAILED] Authentication failed.\r\n")
            } else {
                format!("{tag} OK authenticated\r\n")
            };
            if write(&mut io, &reply).await.is_err() {
                return;
            }
            continue;
        }
        if command.to_ascii_uppercase().starts_with("APPEND ")
            && let Some((size, plus)) = literal(command)
        {
            match append(&mut io, &world, &mut state, tag, command, size, plus).await {
                Some(true) => continue,
                Some(false) => return silent(io).await,
                None => return,
            }
        }
        if command.eq_ignore_ascii_case("IDLE") && caps.contains("IDLE") {
            arrived(
                &mut world.lock().unwrap_or_else(|e| e.into_inner()),
                command,
            );
            match idle(&mut io, &world, &mut state, &mut changes, tag, epoch).await {
                Idle::Done => continue,
                Idle::Dead => return silent(io).await,
                Idle::Closed => return,
            }
        }
        let reply = {
            let mut world = world.lock().unwrap_or_else(|e| e.into_inner());
            arrived(&mut world, command);
            match world.fault(command) {
                Some(Fault::Refuse(text)) if text.starts_with("* BYE") => {
                    Reply::Cut(format!("{text}\r\n"))
                }
                Some(Fault::Refuse(text)) => Reply::Text(format!("{tag} {text}\r\n")),
                Some(Fault::CutBefore) => {
                    world.cuts += 1;
                    Reply::Cut(String::new())
                }
                Some(Fault::LoseReply) => {
                    answer(&mut world, &mut state, tag, command);
                    world.cuts += 1;
                    Reply::Cut(String::new())
                }
                Some(Fault::Hold) => {
                    answer(&mut world, &mut state, tag, command);
                    world.cuts += 1;
                    Reply::Silent
                }
                None => answer(&mut world, &mut state, tag, command),
            }
        };
        match reply {
            Reply::Text(text) => {
                if write(&mut io, &text).await.is_err() {
                    return;
                }
                if command.eq_ignore_ascii_case("LOGOUT") {
                    return;
                }
            }
            Reply::Cut(text) => {
                let _ = write(&mut io, &text).await;
                return;
            }
            Reply::Silent => return silent(io).await,
        }
    }
}

/// The size of the literal a command line ends with, and whether it is non-synchronizing.
fn literal(command: &str) -> Option<(usize, bool)> {
    let inner = command.strip_suffix('}')?;
    let open = inner.rfind('{')?;
    let size = &inner[open + 1..];
    let (size, plus) = match size.strip_suffix('+') {
        Some(size) => (size, true),
        None => (size, false),
    };
    Some((size.parse().ok()?, plus))
}

/// `APPEND <mailbox> [(<flags>)] {n}` and its literal. `Some(true)` goes on with the next
/// command, `Some(false)` leaves the connection silent, `None` cuts it.
async fn append(
    io: &mut BufReader<Box<dyn Io>>,
    world: &Arc<Mutex<World>>,
    state: &mut Session,
    tag: &str,
    command: &str,
    size: usize,
    plus: bool,
) -> Option<bool> {
    let (fault, cut) = {
        let mut world = world.lock().unwrap_or_else(|e| e.into_inner());
        arrived(&mut world, command);
        let fault = world.fault(command);
        let cut = world.literal_cut.take();
        if matches!(fault, Some(Fault::CutBefore)) {
            world.cuts += 1;
        }
        (fault, cut)
    };
    match &fault {
        Some(Fault::CutBefore) => return None,
        Some(Fault::Refuse(text)) if !plus => {
            return write(io, &format!("{tag} {text}\r\n"))
                .await
                .ok()
                .map(|()| true);
        }
        _ => {}
    }
    if !plus && write(io, "+ go ahead\r\n").await.is_err() {
        return None;
    }
    let mut data = vec![0u8; size];
    if let Some(after) = cut.filter(|after| *after < size) {
        let _ = io.read_exact(&mut data[..after]).await;
        world.lock().unwrap_or_else(|e| e.into_inner()).cuts += 1;
        return None;
    }
    io.read_exact(&mut data).await.ok()?;
    let mut end = String::new();
    io.read_line(&mut end).await.ok()?;
    if let Some(Fault::Refuse(text)) = fault {
        return write(io, &format!("{tag} {text}\r\n"))
            .await
            .ok()
            .map(|()| true);
    }
    let reply = {
        let mut world = world.lock().unwrap_or_else(|e| e.into_inner());
        let rest = command[7..].trim_start();
        let (name, rest) = if let Some(quoted) = rest.strip_prefix('"') {
            let close = quoted.find('"').unwrap();
            (unquote(&rest[..close + 2]), &quoted[close + 1..])
        } else {
            let (name, rest) = rest.split_once(' ').unwrap();
            (name.to_owned(), rest)
        };
        let rest = rest.trim_start();
        let flags: Vec<String> = if rest.starts_with('(') {
            group(rest)
                .0
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        if world.mailboxes.get(&name).is_none_or(|m| !selectable(m)) {
            format!("{tag} NO [TRYCREATE] no such mailbox\r\n")
        } else {
            let message_id = sealwax_domain::message::parse(&data)
                .headers
                .message_id
                .unwrap_or_default();
            let uid = world.put(
                &name,
                Message {
                    flags,
                    modseq: 0,
                    date: NOW,
                    message_id,
                    extra: String::new(),
                },
            );
            world.changed();
            let mut out = String::new();
            notices(&world, state, &mut out);
            let uidvalidity = world.mailboxes[&name].uidvalidity;
            if world.uidplus {
                out.push_str(&format!(
                    "{tag} OK [APPENDUID {uidvalidity} {uid}] appended\r\n"
                ));
            } else {
                out.push_str(&format!("{tag} OK appended\r\n"));
            }
            out
        }
    };
    match fault {
        Some(Fault::LoseReply) => {
            world.lock().unwrap_or_else(|e| e.into_inner()).cuts += 1;
            None
        }
        Some(Fault::Hold) => {
            world.lock().unwrap_or_else(|e| e.into_inner()).cuts += 1;
            Some(false)
        }
        _ => write(io, &reply).await.ok().map(|()| true),
    }
}

async fn write(io: &mut BufReader<Box<dyn Io>>, text: &str) -> std::io::Result<()> {
    let stream = io.get_mut();
    stream.write_all(text.as_bytes()).await?;
    stream.flush().await
}

/// Mailbox names in `world`, as LIST shows them.
pub fn names(world: &World) -> BTreeSet<String> {
    world.mailboxes.keys().cloned().collect()
}
