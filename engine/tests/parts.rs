//! `Command::FetchPart` and `Command::FetchOriginal` through the engine service (M5, ML-061,
//! ML-063), offline, against a scripted IMAP server on 127.0.0.1: a part of a message stored as
//! its text parts by its section and the message whole as the server has it, each answered
//! once; asking twice asks the server once; a message or part gone, a new UIDVALIDITY, a cut
//! mid-literal (retried once, then offline) and a refused login; only `BODY.PEEK`, never
//! `STORE`; nothing stored; and, at trace level under the app's own subscriber, no content,
//! name or address in the log. Its own test binary, because it installs the global subscriber.

mod mock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Security};
use sealwax_domain::message;
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_engine::api::{BodyError, Command, Event, FetchedPart, Original, Password, SyncStatus};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{BodyState, BodyUpgrade, NewMessage, Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing_subscriber::util::SubscriberInitExt;

const PASSWORD: &str = "parts-test-password";
const ADDRESS: &str = "parts@example.test";
const WAIT: Duration = Duration::from_secs(20);
const UID: u32 = 9;
const SUBJECT: &str = "Kvartalsrapport for fjerde kvartal";
const TEXT: &str = "Vedlagt er rapporten om fjordturen.";
const NAME: &str = "kvartalsrapport-fjordtur.pdf";
const PDF: &[u8] = b"%PDF-1.4\n% kvartalsrapport\n";

fn header() -> String {
    format!(
        "From: Kari Nordmann <kari.nordmann@example.test>\r\nTo: {ADDRESS}\r\n\
         Subject: {SUBJECT}\r\nMessage-ID: <parts-9@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"mix\"\r\n\r\n"
    )
}

/// The attachment's own MIME header, as BODY[2.MIME] returns it.
fn part_header() -> String {
    format!(
        "Content-Type: application/pdf; name=\"{NAME}\"\r\n\
         Content-Disposition: attachment; filename=\"{NAME}\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n"
    )
}

/// The message as the server has it.
fn original() -> String {
    format!(
        "{}--mix\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{TEXT}\r\n\
         --mix\r\n{}{}\r\n--mix--\r\n",
        header(),
        part_header(),
        STANDARD.encode(PDF)
    )
}

/// The message as the store keeps it after a download of its text parts: the attachment empty.
fn rebuilt() -> String {
    format!(
        "{}--mix\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{TEXT}\r\n\
         --mix\r\n{}\r\n--mix--\r\n",
        header(),
        part_header()
    )
}

/// What the scripted server holds and saw.
struct Server {
    present: bool,
    uidvalidity: u32,
    refuse: bool,
    /// Body and part FETCHes still to cut mid-literal.
    cuts: u32,
    commands: Vec<String>,
}

fn literal(name: &str, data: &str) -> String {
    format!("{name} {{{}}}\r\n{data}", data.len())
}

/// IMAP over TLS with AUTHENTICATE PLAIN and one INBOX holding UID 9, as far as the first sync
/// and the downloads need.
async fn serve(tls: &mock::Tls, state: Arc<Mutex<Server>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(tls.server.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let Ok(stream) = acceptor.accept(tcp).await else {
                continue;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let mut io = BufReader::new(stream);
                let mut out = String::from("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n");
                loop {
                    if !out.is_empty() {
                        let stream = io.get_mut();
                        if stream.write_all(out.as_bytes()).await.is_err() {
                            return;
                        }
                        let _ = stream.flush().await;
                        out.clear();
                    }
                    let mut line = String::new();
                    if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim_end().to_owned();
                    let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
                    let upper = rest.to_ascii_uppercase();
                    state.lock().unwrap().commands.push(rest.to_owned());
                    out = if upper.starts_with("AUTHENTICATE PLAIN") {
                        let stream = io.get_mut();
                        let _ = stream.write_all(b"+ \r\n").await;
                        let _ = stream.flush().await;
                        let mut response = String::new();
                        let _ = io.read_line(&mut response).await;
                        let decoded = STANDARD.decode(response.trim()).unwrap_or_default();
                        let accepted =
                            !state.lock().unwrap().refuse && decoded.ends_with(PASSWORD.as_bytes());
                        if accepted {
                            format!("{tag} OK authenticated\r\n")
                        } else {
                            format!("{tag} NO [AUTHENTICATIONFAILED] Authentication failed.\r\n")
                        }
                    } else if upper.starts_with("CAPABILITY") {
                        format!("* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("NOOP") {
                        format!("{tag} OK done\r\n")
                    } else if upper.starts_with("LIST") {
                        format!("* LIST (\\HasNoChildren) \"/\" INBOX\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("STATUS") {
                        format!("* STATUS INBOX (MESSAGES 1 UNSEEN 0)\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("EXAMINE") {
                        let uidvalidity = state.lock().unwrap().uidvalidity;
                        format!(
                            "* OK [UIDVALIDITY {uidvalidity}] ok\r\n* OK [UIDNEXT 10] ok\r\n\
                             {tag} OK [READ-ONLY] done\r\n"
                        )
                    } else if upper.starts_with("UID SEARCH") {
                        format!("* SEARCH\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("UID FETCH ") && upper.ends_with("(UID FLAGS)") {
                        format!("{tag} OK done\r\n")
                    } else if let Some(fetch) = upper.strip_prefix("UID FETCH ") {
                        let (uid, items) = fetch.split_once(' ').unwrap();
                        assert_eq!(uid, UID.to_string(), "{rest}");
                        let whole = items == "(UID BODY.PEEK[])";
                        assert!(
                            whole || items == "(UID BODY.PEEK[2.MIME] BODY.PEEK[2])",
                            "{rest}"
                        );
                        let (cut, present) = {
                            let mut state = state.lock().unwrap();
                            let cut = state.cuts > 0;
                            state.cuts = state.cuts.saturating_sub(1);
                            (cut, state.present)
                        };
                        if cut {
                            let partial = format!(
                                "* 1 FETCH (UID {UID} BODY[] {{{}}}\r\n{}",
                                original().len(),
                                &original()[..60]
                            );
                            let stream = io.get_mut();
                            let _ = stream.write_all(partial.as_bytes()).await;
                            let _ = stream.flush().await;
                            return;
                        }
                        let fetched = match (present, whole) {
                            (false, _) => String::new(),
                            (true, true) => {
                                format!(
                                    "* 1 FETCH (UID {UID} {})\r\n",
                                    literal("BODY[]", &original())
                                )
                            }
                            (true, false) => format!(
                                "* 1 FETCH (UID {UID} {} {})\r\n",
                                literal("BODY[2.MIME]", &part_header()),
                                literal("BODY[2]", &format!("{}\r\n", STANDARD.encode(PDF)))
                            ),
                        };
                        format!("{fetched}{tag} OK done\r\n")
                    } else if upper.starts_with("LOGOUT") {
                        format!("* BYE bye\r\n{tag} OK done\r\n")
                    } else {
                        format!("{tag} BAD unknown\r\n")
                    };
                }
            });
        }
    });
    port
}

fn recv_within(events: &async_channel::Receiver<Event>, wait: Duration) -> Option<Event> {
    let started = Instant::now();
    loop {
        match events.try_recv() {
            Ok(event) => return Some(event),
            Err(async_channel::TryRecvError::Closed) => return None,
            Err(async_channel::TryRecvError::Empty) if started.elapsed() > wait => return None,
            Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// The next event `pick` accepts; others are skipped.
fn next<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let events = engine.events();
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no such event within {WAIT:?}");
        let event = recv_within(&events, left).expect("the engine stopped");
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

/// Waits until the server has heard no command for `quiet`.
fn settle(server: &Mutex<Server>, quiet: Duration) {
    let deadline = Instant::now() + WAIT;
    let mut seen = server.lock().unwrap().commands.len();
    let mut since = Instant::now();
    while since.elapsed() < quiet {
        assert!(Instant::now() < deadline, "the actor never went quiet");
        std::thread::sleep(Duration::from_millis(10));
        let now = server.lock().unwrap().commands.len();
        if now != seen {
            seen = now;
            since = Instant::now();
        }
    }
}

#[derive(Debug, PartialEq)]
enum Answer {
    Part(MessageId, u32, Result<FetchedPart, BodyError>),
    Original(MessageId, Result<Original, BodyError>),
}

/// Every `PartFetched` and `OriginalFetched` within `wait`.
fn answers(engine: &Engine, wait: Duration) -> Vec<Answer> {
    let events = engine.events();
    let deadline = Instant::now() + wait;
    let mut out = Vec::new();
    while let Some(event) = recv_within(&events, deadline.saturating_duration_since(Instant::now()))
    {
        match event {
            Event::PartFetched {
                message,
                part,
                result,
            } => out.push(Answer::Part(message, part, result)),
            Event::OriginalFetched { message, result } => {
                out.push(Answer::Original(message, result));
            }
            _ => {}
        }
    }
    out
}

fn fetches(server: &Mutex<Server>) -> usize {
    server
        .lock()
        .unwrap()
        .commands
        .iter()
        .filter(|command| command.contains("BODY.PEEK"))
        .count()
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn parts_and_originals_are_fetched_on_demand_and_answered_once() {
    let capture = Capture::default();
    let sink = capture.clone();
    sealwax_infrastructure::logging::subscriber(
        tracing_subscriber::EnvFilter::new("trace"),
        move || sink.clone(),
        false,
    )
    .try_init()
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let tls = mock::tls();
    let server = Arc::new(Mutex::new(Server {
        present: true,
        uidvalidity: 7,
        refuse: false,
        cuts: 0,
        commands: Vec::new(),
    }));
    let port = runtime.block_on(serve(&tls, server.clone()));
    let root = tempfile::tempdir().unwrap();
    let dirs = SealwaxDirs::under(root.path());
    let blobs = BlobStore::open(&dirs.blobs_dir()).unwrap();
    let store = Arc::new(
        Store::open(
            &dirs.store_file(),
            StoreOptions {
                blobs: Some(blobs.clone()),
                ..StoreOptions::default()
            },
        )
        .unwrap(),
    );
    let engine = Engine::start(EngineConfig {
        application: "io.github.pixdevsapps.Sealwax.Test".into(),
        store: store.clone(),
        blobs: blobs.clone(),
        paths: AccountPaths::new(&dirs),
        trust: tls.trust.clone(),
        secrets: SecretBackend::Memory,
        detect: DetectOptions::default(),
        microsoft_client_id: None,
        sign_in: SignInOptions::default(),
        sync: SyncConfig {
            account_pass: Duration::from_secs(600),
            inbox_poll: Duration::from_secs(600),
            opened_poll: Duration::from_secs(600),
            ..SyncConfig::default()
        },
    })
    .unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
        certificate: None,
    };
    let commands = engine.commands();
    commands
        .send_blocking(Command::AddAccount {
            request: 1,
            settings: AccountSettings {
                key: AccountKey(String::new()),
                name: "Parts".into(),
                label: None,
                color: None,
                address: ADDRESS.into(),
                incoming: endpoint.clone(),
                receive: sealwax_domain::account::Receive::Imap,
                smtp: endpoint,
                username: ADDRESS.into(),
                auth: AuthMethod::Password,
                signature: None,
                identities: Vec::new(),
            },
            password: Password::new(PASSWORD.into()),
            sign_in: None,
        })
        .unwrap();
    let account = next(&engine, |event| match event {
        Event::AccountAdded { request: 1, result } => Some(result.unwrap()),
        _ => None,
    });
    next(&engine, |event| match event {
        Event::AccountStatus { status, .. } => (status == SyncStatus::Synced).then_some(()),
        _ => None,
    });
    settle(&server, Duration::from_millis(300));

    // The message as a download of its text parts leaves it: the attachment empty, its section
    // recorded.
    let inbox = store
        .folders()
        .recv_blocking()
        .unwrap()
        .value
        .into_iter()
        .find(|folder| folder.account == account && folder.role == FolderRole::Inbox)
        .unwrap()
        .id;
    let id = store
        .ingest(vec![NewMessage {
            folder: inbox,
            uid: Some(UID),
            date: 1_790_000_000,
            size: 12_400_000,
            body_state: BodyState::Partial,
            ..NewMessage::default()
        }])
        .recv_blocking()
        .unwrap()
        .ids[0]
        .unwrap();
    let stored = rebuilt().into_bytes();
    let parsed = message::parse(&stored);
    let attachment = parsed.attachments[0].part;
    let text_part = parsed.text_parts[0];
    let key = blobs.put(&stored).unwrap();
    let upgraded = store
        .upgrade_body(BodyUpgrade {
            message: id,
            uid: UID,
            blob: key,
            blob_size: stored.len() as u64,
            body_state: BodyState::Text,
            body_kind: parsed.body_kind,
            has_attachments: true,
            snippet: String::new(),
            text_parts: parsed.text_parts.clone(),
            html_parts: parsed.html_parts.clone(),
            attachments: parsed.attachments.clone(),
            sections: vec![(attachment, "2".into())],
            search_text: String::new(),
            used: 1_790_000_000,
        })
        .recv_blocking()
        .unwrap();
    assert!(upgraded.found);
    server.lock().unwrap().commands.clear();
    let pdf = FetchedPart {
        mime_type: "application/pdf".into(),
        data: PDF.to_vec(),
    };

    // A part, asked for twice while out: one FETCH, one answer.
    for _ in 0..2 {
        commands
            .send_blocking(Command::FetchPart {
                message: id,
                part: attachment,
            })
            .unwrap();
    }
    assert_eq!(
        answers(&engine, Duration::from_secs(2)),
        [Answer::Part(id, attachment, Ok(pdf.clone()))]
    );
    assert_eq!(fetches(&server), 1);

    // The message whole, the server's bytes exactly; the store keeps its rebuilt copy.
    commands
        .send_blocking(Command::FetchOriginal { message: id })
        .unwrap();
    assert_eq!(
        answers(&engine, Duration::from_secs(2)),
        [Answer::Original(id, Ok(Original(original().into_bytes())))]
    );
    assert_eq!(
        store.raw_message(id).recv_blocking().unwrap(),
        Some(stored.clone()),
        "nothing is stored"
    );
    let target = store.body_targets(vec![id]).recv_blocking().unwrap();
    assert_eq!(target[0].body_state, BodyState::Text);

    // A message the store doesn't hold, and a part with no section to fetch it by.
    commands
        .send_blocking(Command::FetchPart {
            message: MessageId(999),
            part: 1,
        })
        .unwrap();
    commands
        .send_blocking(Command::FetchOriginal {
            message: MessageId(999),
        })
        .unwrap();
    commands
        .send_blocking(Command::FetchPart {
            message: id,
            part: text_part,
        })
        .unwrap();
    let mut answered = answers(&engine, Duration::from_secs(1));
    answered.sort_by_key(|answer| format!("{answer:?}"));
    assert_eq!(
        answered,
        [
            Answer::Original(MessageId(999), Err(BodyError::Gone)),
            Answer::Part(id, text_part, Err(BodyError::Other)),
            Answer::Part(MessageId(999), 1, Err(BodyError::Gone)),
        ]
    );
    assert_eq!(fetches(&server), 2, "none of them reached the server");

    // Cut mid-literal: tried once more on a new connection; cut twice, offline.
    server.lock().unwrap().cuts = 1;
    commands
        .send_blocking(Command::FetchPart {
            message: id,
            part: attachment,
        })
        .unwrap();
    assert_eq!(
        answers(&engine, Duration::from_secs(2)),
        [Answer::Part(id, attachment, Ok(pdf.clone()))]
    );
    server.lock().unwrap().cuts = 2;
    commands
        .send_blocking(Command::FetchOriginal { message: id })
        .unwrap();
    assert_eq!(
        answers(&engine, Duration::from_secs(2)),
        [Answer::Original(id, Err(BodyError::Offline))]
    );

    // Gone from the server, then a new UIDVALIDITY: gone, and no FETCH for the second.
    server.lock().unwrap().present = false;
    commands
        .send_blocking(Command::FetchOriginal { message: id })
        .unwrap();
    commands
        .send_blocking(Command::FetchPart {
            message: id,
            part: attachment,
        })
        .unwrap();
    let mut answered = answers(&engine, Duration::from_secs(2));
    answered.sort_by_key(|answer| format!("{answer:?}"));
    assert_eq!(
        answered,
        [
            Answer::Original(id, Err(BodyError::Gone)),
            Answer::Part(id, attachment, Err(BodyError::Gone)),
        ]
    );
    {
        let mut server = server.lock().unwrap();
        server.present = true;
        server.uidvalidity = 8;
    }
    let before = fetches(&server);
    commands
        .send_blocking(Command::FetchPart {
            message: id,
            part: attachment,
        })
        .unwrap();
    assert_eq!(
        answers(&engine, Duration::from_secs(2)),
        [Answer::Part(id, attachment, Err(BodyError::Gone))]
    );
    assert_eq!(fetches(&server), before);
    server.lock().unwrap().uidvalidity = 7;

    // A refused login: the reader is told to sign in again.
    server.lock().unwrap().refuse = true;
    commands.send_blocking(Command::Reconnect).unwrap();
    commands
        .send_blocking(Command::FetchOriginal { message: id })
        .unwrap();
    assert_eq!(
        answers(&engine, Duration::from_secs(2)),
        [Answer::Original(id, Err(BodyError::NeedsSignIn))]
    );

    let received = server
        .lock()
        .unwrap()
        .commands
        .join("\n")
        .to_ascii_uppercase();
    assert!(!received.contains("BODY["), "only BODY.PEEK");
    assert!(!received.contains(" STORE") && !received.starts_with("STORE"));
    engine.stop();

    let log = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    for needle in [
        PASSWORD,
        "example.test",
        "Kari",
        "Nordmann",
        SUBJECT,
        "Kvartalsrapport",
        TEXT,
        "fjordtur",
        NAME,
        "kvartal",
        "INBOX",
        "PDF-1.4",
        "JVBERi0",
        "AUTHENTICATE",
        "async_imap",
        "imap_proto",
    ] {
        assert!(!log.contains(needle), "{needle} appears in the log");
    }
    assert!(log.lines().count() > 10, "the log was captured");
    println!(
        "captured {} lines, {} bytes at trace level:\n{log}",
        log.lines().count(),
        log.len()
    );
}
