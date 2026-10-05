//! `Command::FetchBodies` through the engine service, offline, against a scripted IMAP server
//! on 127.0.0.1: one `BodyFetched` per message, requests coalesced, a vanished message `Gone`,
//! a refused login `NeedsSignIn`, and `Command::Storage` enforcing the disk cap.

mod mock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sealwax_domain::account::{AccountKey, AccountSettings, AuthMethod, Endpoint, Security};
use sealwax_domain::config::Storage;
use sealwax_domain::model::{FolderRole, MessageId};
use sealwax_engine::api::{BodyError, Command, Event, Password};
use sealwax_engine::autoconfig::DetectOptions;
use sealwax_engine::service::{Engine, EngineConfig, SecretBackend, SignInOptions, SyncConfig};
use sealwax_infrastructure::accounts::AccountPaths;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{BodyState, NewMessage, Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const PASSWORD: &str = "body-test-password";
const WAIT: Duration = Duration::from_secs(20);

fn raw(uid: u32) -> String {
    format!(
        "From: Avsender <avsender@example.test>\r\nTo: test@example.test\r\n\
         Subject: Melding {uid}\r\nMessage-ID: <service-{uid}@example.test>\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nHele meldingen nummer {uid}.\r\n"
    )
}

/// What the scripted server holds and saw.
#[derive(Default)]
struct Server {
    /// UID → message in INBOX.
    messages: HashMap<u32, String>,
    refuse: bool,
    commands: Vec<String>,
}

fn uids(set: &str) -> Vec<u32> {
    set.split(',')
        .flat_map(|range| match range.split_once(':') {
            Some((from, to)) => {
                (from.parse::<u32>().unwrap()..=to.parse::<u32>().unwrap()).collect::<Vec<_>>()
            }
            None => vec![range.parse().unwrap()],
        })
        .collect()
}

/// IMAP over TLS with AUTHENTICATE PLAIN and one INBOX, as far as the first sync and the body
/// downloads need.
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
                    } else if upper.starts_with("LIST") {
                        format!("* LIST (\\HasNoChildren) \"/\" INBOX\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("STATUS") {
                        let count = state.lock().unwrap().messages.len();
                        format!("* STATUS INBOX (MESSAGES {count} UNSEEN 0)\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("EXAMINE") {
                        format!(
                            "* OK [UIDVALIDITY 7] ok\r\n* OK [UIDNEXT 10] ok\r\n\
                             {tag} OK [READ-ONLY] done\r\n"
                        )
                    } else if upper.starts_with("UID SEARCH") {
                        format!("* SEARCH\r\n{tag} OK done\r\n")
                    } else if upper.starts_with("UID FETCH ") && upper.ends_with("(UID FLAGS)") {
                        // The sync actor's flag scan of the empty INBOX.
                        format!("{tag} OK done\r\n")
                    } else if let Some(fetch) = upper.strip_prefix("UID FETCH ") {
                        let (set, items) = fetch.split_once(' ').unwrap();
                        assert_eq!(items, "(UID BODY.PEEK[])");
                        let state = state.lock().unwrap();
                        let mut reply = String::new();
                        for (n, uid) in uids(set).into_iter().enumerate() {
                            if let Some(message) = state.messages.get(&uid) {
                                reply.push_str(&format!(
                                    "* {} FETCH (UID {uid} BODY[] {{{}}}\r\n{message})\r\n",
                                    n + 1,
                                    message.len()
                                ));
                            }
                        }
                        format!("{reply}{tag} OK done\r\n")
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

/// Every `BodyFetched` within `wait`.
fn bodies_fetched(engine: &Engine, wait: Duration) -> Vec<(MessageId, Result<(), BodyError>)> {
    let events = engine.events();
    let deadline = Instant::now() + wait;
    let mut out = Vec::new();
    while let Some(event) = recv_within(&events, deadline.saturating_duration_since(Instant::now()))
    {
        if let Event::BodyFetched { message, result } = event {
            out.push((message, result));
        }
    }
    out
}

#[test]
fn bodies_are_fetched_once_answered_per_message_and_kept_under_the_cap() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let tls = mock::tls();
    let server = Arc::new(Mutex::new(Server {
        messages: [(1, raw(1)), (2, raw(2)), (4, raw(4))]
            .into_iter()
            .collect(),
        ..Server::default()
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
        sync: SyncConfig::default(),
    })
    .unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port,
        security: Security::Tls,
    };
    let settings = AccountSettings {
        key: AccountKey(String::new()),
        name: "Bodies".into(),
        address: "bodies@example.test".into(),
        imap: endpoint.clone(),
        smtp: endpoint,
        username: "bodies@example.test".into(),
        auth: AuthMethod::Password,
    };
    let commands = engine.commands();
    commands
        .send_blocking(Command::AddAccount {
            request: 1,
            settings,
            password: Password::new(PASSWORD.into()),
            sign_in: None,
        })
        .unwrap();
    let account = next(&engine, |event| match event {
        Event::AccountAdded { request: 1, result } => Some(result.unwrap()),
        _ => None,
    });
    next(&engine, |event| match event {
        Event::AccountStatus { status, .. } => {
            (status == sealwax_engine::api::SyncStatus::Synced).then_some(())
        }
        _ => None,
    });
    let inbox = store
        .folders()
        .recv_blocking()
        .unwrap()
        .value
        .into_iter()
        .find(|folder| folder.account == account && folder.role == FolderRole::Inbox)
        .unwrap()
        .id;
    let ids: Vec<MessageId> = store
        .ingest(
            [1, 2, 3, 4]
                .into_iter()
                .map(|uid| NewMessage {
                    folder: inbox,
                    uid: Some(uid),
                    date: 1_790_000_000 + i64::from(uid),
                    size: raw(uid).len() as u64,
                    snippet: format!("Preview {uid}"),
                    body_state: BodyState::Partial,
                    ..NewMessage::default()
                })
                .collect(),
        )
        .recv_blocking()
        .unwrap()
        .ids
        .into_iter()
        .map(Option::unwrap)
        .collect();
    server.lock().unwrap().commands.clear();

    commands
        .send_blocking(Command::FetchBodies {
            messages: vec![ids[1], ids[0], ids[2], MessageId(999)],
        })
        .unwrap();
    commands
        .send_blocking(Command::FetchBodies {
            messages: vec![ids[1]],
        })
        .unwrap();
    let mut answers = bodies_fetched(&engine, Duration::from_secs(2));
    answers.sort_by_key(|(id, _)| *id);
    assert_eq!(
        answers,
        [
            (ids[0], Ok(())),
            (ids[1], Ok(())),
            (ids[2], Err(BodyError::Gone)),
            (MessageId(999), Err(BodyError::Gone)),
        ],
        "one answer per message, the repeated request coalesced"
    );
    let fetches: Vec<String> = server
        .lock()
        .unwrap()
        .commands
        .iter()
        .filter(|command| command.contains("FETCH"))
        .cloned()
        .collect();
    assert_eq!(fetches, ["UID FETCH 1:3 (UID BODY.PEEK[])"]);
    for (id, uid) in [(ids[0], 1), (ids[1], 2)] {
        assert_eq!(
            store.raw_message(id).recv_blocking().unwrap(),
            Some(raw(uid).into_bytes())
        );
    }
    commands
        .send_blocking(Command::FetchBodies {
            messages: vec![ids[0]],
        })
        .unwrap();
    assert_eq!(
        bodies_fetched(&engine, Duration::from_millis(500)),
        [(ids[0], Ok(()))],
        "a stored body answers at once"
    );
    assert_eq!(
        server
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|command| command.contains("FETCH"))
            .count(),
        1
    );

    commands
        .send_blocking(Command::Storage {
            storage: Storage {
                cache_size: Some(1),
                prefetch_days: 0,
            },
        })
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while store.body_usage().recv_blocking().unwrap().bodies > 0 {
        assert!(Instant::now() < deadline, "the cap never evicted");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        blobs.disk_usage().unwrap().files,
        0,
        "evicted blobs are deleted"
    );
    let target = store.body_targets(vec![ids[0]]).recv_blocking().unwrap();
    assert_eq!(
        (target[0].body_state, target[0].blob),
        (BodyState::Partial, None)
    );

    // The download connection stays open a while; a reconnect makes the next batch log in again.
    server.lock().unwrap().refuse = true;
    commands.send_blocking(Command::Reconnect).unwrap();
    commands
        .send_blocking(Command::FetchBodies {
            messages: vec![ids[3]],
        })
        .unwrap();
    assert_eq!(
        bodies_fetched(&engine, Duration::from_secs(2)),
        [(ids[3], Err(BodyError::NeedsSignIn))]
    );
    let received = server
        .lock()
        .unwrap()
        .commands
        .join("\n")
        .to_ascii_uppercase();
    assert!(!received.contains("BODY[") && !received.contains(" STORE"));
    engine.stop();
}
