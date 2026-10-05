//! A fake IMAP server for the sync engine's tests, on 127.0.0.1 over TLS with a throwaway CA:
//! mailboxes with UIDs, flags, keywords and mod-sequences; EXAMINE, UID SEARCH (ALL, UID,
//! SINCE), UID FETCH of headers or flags with CHANGEDSINCE and VANISHED, LIST with SPECIAL-USE
//! and STATUS, STATUS, UNSELECT, NOOP, IDLE, whole messages for body downloads; EXISTS, EXPUNGE
//! and VANISHED notices before each command's own responses and while idling. Three flavours:
//! QRESYNC, CONDSTORE only (both with IDLE), IMAP4rev1. Tests change its mailboxes between and
//! during passes, cut a connection in the middle of a FETCH, close every idling connection,
//! refuse logins or connections, and turn the network into a black hole that leaves every open
//! connection silently dead.
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
use sealwax_engine::imap::{self, Connection, Credentials};
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
}

struct Cut {
    needle: String,
    skip: usize,
    after: usize,
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
        }
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
        let mailbox = self.mailbox(name);
        let uid = mailbox.uidnext;
        mailbox.uidnext += 1;
        mailbox.modseq += 1;
        let modseq = mailbox.modseq;
        mailbox.messages.insert(
            uid,
            Message {
                flags: flags.iter().map(|f| f.to_string()).collect(),
                modseq,
                date: NOW - days * 86_400 + 3_600,
                message_id,
            },
        );
        self.changed();
        uid
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
        }
    }

    pub async fn connect(&self) -> Connection {
        imap::connect(
            &self.endpoint(),
            Credentials::Password {
                username: "test",
                password: "fake-password",
            },
            self.trust.client_config().unwrap(),
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
         To: <test@example.test>\r\nSubject: Fake message {uid}\r\nMessage-ID: <{}>\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n",
        message.message_id
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

enum Reply {
    Text(String),
    /// Send the text, then drop the connection without a reply.
    Cut(String),
}

fn answer(world: &mut World, session: &mut Session, tag: &str, command: &str) -> Reply {
    let mut out = String::new();
    notices(world, session, &mut out);
    let upper = command.to_ascii_uppercase();
    let verb = upper.split_whitespace().next().unwrap_or("").to_owned();
    let ok = |out: &mut String, text: &str| out.push_str(&format!("{tag} OK {text}\r\n"));
    match verb.as_str() {
        "CAPABILITY" => {
            out.push_str(&format!("* CAPABILITY {}\r\n", world.flavor.caps()));
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
            session.known.clear();
            ok(&mut out, "done");
        }
        "LIST" => {
            let status = upper
                .find("STATUS (")
                .map(|at| group(&command[at + 7..]).0.to_owned());
            for (name, mailbox) in &world.mailboxes {
                let mut attributes = mailbox.attributes.clone();
                attributes.push("\\HasNoChildren".into());
                out.push_str(&format!(
                    "* LIST ({}) \"/\" {}\r\n",
                    attributes.join(" "),
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
            session.known.clear();
            let Some(mailbox) = world.mailboxes.get(&name).filter(|m| selectable(m)) else {
                out.push_str(&format!("{tag} NO [NONEXISTENT] no such mailbox\r\n"));
                return Reply::Text(out);
            };
            session.known = mailbox.messages.keys().copied().collect();
            session.selected = Some((name, mailbox.uidvalidity));
            out.push_str("* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n");
            out.push_str("* OK [PERMANENTFLAGS ()] read-only\r\n");
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
            out.push_str(&format!("{tag} OK [READ-ONLY] examined\r\n"));
        }
        "UID" => return uid_command(world, session, tag, command, out),
        "LOGOUT" => {
            out.push_str("* BYE bye\r\n");
            ok(&mut out, "done");
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
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].to_ascii_uppercase().as_str() {
                "UID" => {
                    set = Some(tokens[i + 1]);
                    i += 1;
                }
                "SINCE" => {
                    since = Some(search_date(tokens[i + 1]));
                    i += 1;
                }
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
        } else if headers {
            let full = header(message, *uid);
            let head = header_fields(&full, &fields);
            let size = full.len() + TEXT.len();
            out.push_str(&format!(
                "* {seq} FETCH (UID {uid} FLAGS ({}) INTERNALDATE \"{}\" RFC822.SIZE {size} \
                 BODYSTRUCTURE (\"text\" \"plain\" (\"charset\" \"utf-8\") NIL NIL \"7bit\" {} 1) \
                 BODY[HEADER.FIELDS ({fields})] {{{}}}\r\n{head} BODY[TEXT]<0> {{{}}}\r\n{TEXT})\r\n",
                flag_list(message),
                internal_date(message.date),
                TEXT.len(),
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
                let out = {
                    let world = world.lock().unwrap_or_else(|e| e.into_inner());
                    if world.dead(epoch) {
                        return Idle::Dead;
                    }
                    if world.closings > closings {
                        return Idle::Closed;
                    }
                    let mut out = String::new();
                    notices(&world, state, &mut out);
                    out
                };
                if !out.is_empty() && write(io, &out).await.is_err() {
                    return Idle::Dead;
                }
            }
        }
    }
}

async fn session(mut io: BufReader<Box<dyn Io>>, world: Arc<Mutex<World>>, epoch: u64) {
    let (caps, mut changes, dead) = {
        let world = world.lock().unwrap_or_else(|e| e.into_inner());
        (
            world.flavor.caps(),
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
            answer(&mut world, &mut state, tag, command)
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
        }
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
