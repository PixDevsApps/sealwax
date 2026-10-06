//! ADR-018's log-grep test for M2's, M3's and M4's code: the login check, the folder list, the
//! first sync, account passes over the windowed folders of the rand account, older mail, and
//! the op queue's writes (flags, a move retried, deleting for good, a folder marked read) as
//! the ops account run on the testbed at trace level under the app's own subscriber, and the
//! captured log holds no password, AUTH line, address, user name, folder name, subject or
//! Message-ID. A control run without the protocol-crate caps shows the grep would catch
//! async-imap's trace of the AUTHENTICATE line. Its own test binary, because it installs the
//! global subscriber.
//!
//! bash tools/testbed/up.sh && bash tools/testbed/seed.sh
//! cargo test -p sealwax-engine --test log_grep -- --ignored

mod common;
mod snapshot;
mod testbed_client;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::*;
use sealwax_domain::account::Security;
use sealwax_domain::model::Flags;
use sealwax_domain::sync::Window;
use sealwax_engine::api::Password;
use sealwax_engine::imap::write::{self, Move, Moving, OpFolder};
use sealwax_engine::imap::{self, FolderLocks, SyncOptions};
use sealwax_engine::verify::verify;
use sealwax_infrastructure::logging;
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
    }
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
    ] {
        assert!(log.contains(event), "the capture holds {event}");
    }
    let plain = STANDARD.encode(format!("\0alice\0{PASSWORD}"));
    let forbidden = [
        PASSWORD,
        "wrong-password",
        plain.as_str(),
        "alice",
        "rand",
        "nobody",
        "example.test",
        "logs.",
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
