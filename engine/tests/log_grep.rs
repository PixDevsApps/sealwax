//! ADR-018's log-grep test for M2's, M3's and M4's code: the login check, the folder list, the
//! first sync, account passes over the windowed folders of the rand account, older mail, and
//! the op queue's writes (flags, a move retried, deleting for good, a folder marked read) and
//! the op runner (triage queued in the store and run by the engine's actor) as the ops account,
//! and M6's sending (a fixture submitted through Stalwart and Mailpit with the size limit read
//! first; the Sent copy appended, retried and found, and a draft replaced, as the outbox
//! account) run on the testbed at trace level under the app's own subscriber, and the
//! captured log holds no password, AUTH line, address, user name, folder name, subject or
//! Message-ID. A control run without the protocol-crate caps shows the grep would catch
//! async-imap's trace of the AUTHENTICATE line. Its own test binary, because it installs the
//! global subscriber.
//!
//! bash tools/testbed/up.sh && bash tools/testbed/seed.sh
//! cargo test -p sealwax-engine --test log_grep -- --ignored

mod common;
mod outgoing;
mod snapshot;
mod testbed_client;
mod testbed_engine;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::*;
use sealwax_domain::account::Security;
use sealwax_domain::model::{Flags, FolderRole};
use sealwax_domain::sync::Window;
use sealwax_domain::triage::Triage;
use sealwax_engine::api::{Command, Password};
use sealwax_engine::imap::write::{self, Move, Moving, OpFolder};
use sealwax_engine::imap::{self, FolderLocks, SyncOptions};
use sealwax_engine::service::SyncConfig;
use sealwax_engine::smtp::submit;
use sealwax_engine::verify::verify;
use sealwax_infrastructure::logging;
use sealwax_infrastructure::store::QueueTriage;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn flows() {
    let trust = trust();
    let right = Password::new(PASSWORD.to_owned());
    let wrong = Password::new("wrong-password".to_owned());
    for server in SERVERS {
        for security in [Security::Tls, Security::StartTls] {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let account = settings(imap(server, security), submission(security), "alice");
            verify(&account, None, &right, &trust).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let account = settings(
            imap(server, Security::Tls),
            submission(Security::Tls),
            "alice",
        );
        verify(&account, None, &wrong, &trust).await.unwrap_err();

        let (_dir, store, account) = store();
        let mut conn = login(server, Security::Tls, "alice").await;
        let inbox = imap::sync_folders(&mut conn, &store, account)
            .await
            .unwrap()
            .inbox
            .unwrap();
        imap::first_sync(&mut conn, &store, inbox, |_| {})
            .await
            .unwrap();
        conn.logout().await;
        store.close().unwrap();
    }
    for server in SERVERS {
        let (_dir, store, account) = store();
        let mut conn = login(server, Security::Tls, "rand").await;
        let options = SyncOptions {
            window: Window {
                days: 90,
                max: 30,
                min: 10,
            },
            older: 10,
            ..SyncOptions::default()
        };
        let locks = FolderLocks::default();
        for _ in 0..2 {
            imap::sync_account(&mut conn, &store, account, &options, &locks, |_, _| {})
                .await
                .unwrap();
        }
        let folders = store.account_folders(account).recv().await.unwrap();
        for state in folders.iter().filter(|state| state.selectable) {
            imap::sync_folder(&mut conn, &store, state.folder, &options, &locks, |_| {})
                .await
                .unwrap();
            imap::load_older(&mut conn, &store, state.folder, &options, &locks)
                .await
                .unwrap();
        }
        conn.logout().await;
        store.close().unwrap();
    }
    for server in SERVERS {
        writes(server).await;
        runner(server).await;
        appends(server).await;
    }
    submissions().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let account = settings(
        imap(DOVECOT, Security::Tls),
        submission(Security::Tls),
        "nobody",
    );
    verify(&account, Some("alice"), &right, &trust)
        .await
        .unwrap_err();
}

/// The op queue's writes as `ops`: flags with keywords, a move to the archive as a retry with a
/// mark, deleting for good and a folder marked read; the folders are emptied afterwards.
async fn writes(server: Server) {
    let folders = ["INBOX", "Arkiv", "Papirkurv"];
    let mut test = testbed_client::Client::connect(server.tls, &ca(), "ops")
        .await
        .unwrap();
    for folder in folders {
        test.create(folder).await.unwrap();
        test.empty(folder).await.unwrap();
    }
    let mut rng = snapshot::Rng(3);
    let ids: Vec<String> = (0..4).map(|n| format!("logs.{n}@example.test")).collect();
    for id in &ids {
        let raw = testbed_client::message(id, &mut rng);
        test.append("INBOX", "", &raw).await.unwrap();
    }
    test.select("INBOX").await.unwrap();
    let uids = test.uids().await.unwrap();
    let mut conn = login(server, Security::Tls, "ops").await;
    let inbox = OpFolder {
        name_raw: "INBOX",
        uidvalidity: write::select(&mut conn, "INBOX").await.unwrap().uidvalidity,
    };
    let add = Flags::FLAGGED | Flags::JUNK;
    write::store_flags(&mut conn, inbox, &uids, add, Flags::NOT_JUNK)
        .await
        .unwrap();
    let mark = write::mark_target(&mut conn, "Arkiv").await.unwrap();
    let messages: Vec<Moving<'_>> = uids[..2]
        .iter()
        .zip(&ids)
        .map(|(uid, id)| Moving {
            uid: *uid,
            message_id: Some(id.as_str()),
        })
        .collect();
    let done = write::move_messages(
        &mut conn,
        Move {
            from: inbox,
            to: "Arkiv",
            messages: &messages,
            add: Flags::NONE,
            remove: Flags::NONE,
            mark: Some(mark),
            retry: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(done.moved.len(), 2, "{}", server.name);
    write::expunge(&mut conn, inbox, &uids[2..3]).await.unwrap();
    write::mark_folder_read(&mut conn, inbox, None)
        .await
        .unwrap();
    conn.logout().await;
    for folder in folders {
        test.empty(folder).await.unwrap();
    }
    test.logout().await;
}

/// S7's plain fixture with a Message-ID of this run: through Stalwart's submission port to bob
/// and carol, with the limit read first, and through Mailpit; a refused recipient on Stalwart.
async fn submissions() {
    let fixture = outgoing::fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "plain")
        .unwrap();
    let tag = outgoing::run_tag();
    let to = vec![
        "bob@example.test".to_owned(),
        "carol@example.test".to_owned(),
    ];
    let alice = imap::Credentials::Password {
        username: "alice",
        password: PASSWORD,
    };
    for (n, endpoint) in [submission(Security::StartTls), mailpit()]
        .into_iter()
        .enumerate()
    {
        let bytes = outgoing::with_id(&fixture.transmitted, &format!("logs.{tag}.{n}@example.com"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        submit::submission_limit(&endpoint, &trust(), submit::Timeouts::default())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut submitter = submit::Submitter::connect(&endpoint, alice, &trust())
            .await
            .unwrap();
        let envelope = submit::Envelope {
            from: "alice@example.test",
            to: &to,
        };
        submitter
            .prepare(envelope, &bytes)
            .await
            .unwrap()
            .send()
            .await
            .unwrap();
        let refused = vec!["nobody-here@example.test".to_owned()];
        let envelope = submit::Envelope {
            from: "alice@example.test",
            to: &refused,
        };
        let refusal = submitter.prepare(envelope, &bytes).await.err();
        assert_eq!(refusal.is_some(), n == 0, "only Stalwart refuses it");
        submitter.quit().await;
    }
    outgoing::Mailpit::clear().await;
    for user in ["bob", "carol"] {
        let mut test = testbed_client::Client::connect(STALWART.tls, &ca(), user)
            .await
            .unwrap();
        test.empty("INBOX").await.unwrap();
        test.logout().await;
    }
}

/// The Sent copy as `outbox`: appended, retried with its mark and found; a draft replaced.
async fn appends(server: Server) {
    let mut test = testbed_client::Client::connect(server.tls, &ca(), "outbox")
        .await
        .unwrap();
    for folder in ["Sendt", "Utkast"] {
        test.create(folder).await.unwrap();
        test.empty(folder).await.unwrap();
    }
    let tag = outgoing::run_tag();
    let id = format!("logs.{tag}@example.com");
    let fixture = outgoing::fixtures().remove(0);
    let bytes = outgoing::with_id(&fixture.sent_copy, &id);
    let mut conn = login(server, Security::Tls, "outbox").await;
    let mark = write::mark_target(&mut conn, "Sendt").await.unwrap();
    let request = write::Append {
        folder: "Sendt",
        message: &bytes,
        message_id: &id,
        flags: Flags::SEEN,
        mark,
        retry: false,
    };
    write::append(&mut conn, request).await.unwrap();
    let again = write::append(
        &mut conn,
        write::Append {
            retry: true,
            ..request
        },
    )
    .await
    .unwrap();
    assert!(again.earlier, "{}", server.name);
    write::find(&mut conn, "Sendt", &id, None).await.unwrap();
    let draft_mark = write::mark_target(&mut conn, "Utkast").await.unwrap();
    let draft = write::Append {
        folder: "Utkast",
        flags: Flags::DRAFT | Flags::SEEN,
        mark: draft_mark,
        ..request
    };
    let first = write::replace_draft(&mut conn, draft, None)
        .await
        .unwrap()
        .appended
        .unwrap();
    let old = [first.uid.unwrap()];
    let second = outgoing::with_id(&fixture.transmitted, &id);
    let draft_mark = write::mark_target(&mut conn, "Utkast").await.unwrap();
    write::replace_draft(
        &mut conn,
        write::Append {
            message: &second,
            mark: draft_mark,
            ..draft
        },
        Some(write::OldCopies {
            uidvalidity: first.uidvalidity.unwrap(),
            uids: &old,
        }),
    )
    .await
    .unwrap();
    conn.logout().await;
    for folder in ["Sendt", "Utkast"] {
        test.empty(folder).await.unwrap();
    }
    test.logout().await;
}

/// The op runner as `ops`: read, archive, delete to the Trash and for good, and a folder marked
/// read, queued in the store and run by the engine's actor.
async fn runner(server: Server) {
    let folders = ["INBOX", "Arkiv", "Papirkurv"];
    let mut test = testbed_client::Client::connect(server.tls, &ca(), "ops")
        .await
        .unwrap();
    for folder in folders {
        test.create(folder).await.unwrap();
        test.empty(folder).await.unwrap();
    }
    let mut rng = snapshot::Rng(5);
    for n in 0..4 {
        let raw = testbed_client::message(&format!("runner.{n}@example.test"), &mut rng);
        test.append("INBOX", "", &raw).await.unwrap();
    }
    let mut engine = testbed_engine::TestEngine::start(SyncConfig {
        account_pass: Duration::from_secs(3),
        ..SyncConfig::default()
    });
    let account = engine.add(1, "ops", server.tls).await;
    engine.settled(account, Duration::from_secs(30)).await;
    let store = engine.store.clone();
    let inbox = store
        .account_folders(account)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .find(|folder| folder.role == FolderRole::Inbox)
        .unwrap()
        .folder;
    let ids: Vec<_> = store
        .folder_messages(inbox)
        .recv()
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    for (action, messages) in [
        (Triage::Read(true), vec![ids[0], ids[1]]),
        (Triage::Archive, vec![ids[2]]),
        (Triage::Delete, vec![ids[3]]),
        (Triage::Delete, vec![ids[3]]),
    ] {
        let queued = store
            .queue_triage(QueueTriage {
                action,
                messages,
                not_before: 0,
            })
            .recv()
            .await
            .unwrap();
        for account in queued.accounts {
            engine.send(Command::OpsQueued { account });
        }
        let drained = testbed_engine::until(Duration::from_secs(30), || async {
            store.pending_ops(account).recv().await.unwrap() == 0
        })
        .await;
        assert!(drained.is_some(), "{}: {action:?} ran", server.name);
    }
    let read = store.mark_folder_read(inbox, 0).recv().await.unwrap();
    for account in read.accounts {
        engine.send(Command::OpsQueued { account });
    }
    testbed_engine::until(Duration::from_secs(30), || async {
        store.pending_ops(account).recv().await.unwrap() == 0
    })
    .await;
    drop(engine);
    for folder in folders {
        test.empty(folder).await.unwrap();
    }
    test.logout().await;
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn trace_logs_hold_no_credentials_addresses_or_names() {
    let capture = Capture::default();
    let sink = capture.clone();
    logging::subscriber(EnvFilter::new("trace"), move || sink.clone(), false)
        .try_init()
        .unwrap();
    sealwax_engine::runtime().unwrap().block_on(flows());

    let log = capture.text();
    for event in [
        "imap connected",
        "folder list stored",
        "first sync done",
        "account pass done",
        "folder pass done",
        "older mail loaded",
        "imap write done",
        "op group ran",
        "smtp limit read",
        "smtp connected",
        "smtp envelope accepted",
        "smtp envelope not accepted",
        "smtp message accepted",
        "imap append done",
        "imap find done",
    ] {
        assert!(log.contains(event), "the capture holds {event}");
    }
    let plain = STANDARD.encode(format!("\0alice\0{PASSWORD}"));
    let outbox = STANDARD.encode(format!("\0outbox\0{PASSWORD}"));
    let forbidden = [
        PASSWORD,
        "wrong-password",
        plain.as_str(),
        outbox.as_str(),
        "bob@",
        "carol@",
        "nobody-here",
        "outbox@",
        "kari",
        "Nordmann",
        "example.com",
        "example.org",
        "example.net",
        "Plain alternative",
        "Hello **world**",
        "MAIL FROM",
        "RCPT TO",
        "AUTH PLAIN",
        "XOAUTH2",
        "APPEND",
        "APPENDUID",
        "alice",
        "rand",
        "nobody",
        "example.test",
        "logs.",
        "runner.",
        "$Junk",
        "\\Deleted",
        "UID MOVE",
        "UID STORE",
        "INBOX",
        "Søppelpost",
        "S&APg-ppelpost",
        "Papirkurv",
        "Sendt",
        "Utkast",
        "Arkiv",
        "Kvitteringer",
        "Prosjekter",
        "Midlertidig",
        "Hilsen fra Bærum",
        "Synthetic",
        "AUTHENTICATE",
        "LOGIN",
        "async_imap",
        "imap_proto",
        "lettre",
    ];
    for needle in forbidden {
        assert!(!log.contains(needle), "{needle} appears in the log");
    }
    println!(
        "captured {} lines, {} bytes at trace level:\n{log}",
        log.lines().count(),
        log.len()
    );

    // The control: the same subscriber without the caps logs async-imap's AUTHENTICATE line.
    let control = Capture::default();
    let sink = control.clone();
    let uncapped = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .with_filter(EnvFilter::new("trace")),
    );
    tracing::subscriber::with_default(uncapped, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                login(STALWART, Security::Tls, "alice").await.logout().await;
            });
    });
    assert!(
        control.text().contains(&plain),
        "without the caps async-imap's trace carries the AUTHENTICATE PLAIN response"
    );
}
