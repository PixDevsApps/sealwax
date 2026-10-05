//! M2's testbed suites: the login check, the folder list and the first INBOX header sync on
//! Dovecot (full and IMAP4rev1-only) and Stalwart, plus the 1,000-headers speed bar. The perf
//! account's suites sync the whole folder (`Window::ALL`); M3's first-sync window has its own
//! suites (`sync_testbed.rs`). They need the Docker testbed running and seeded, and are skipped
//! otherwise:
//!
//! bash tools/testbed/up.sh && bash tools/testbed/seed.sh
//! cargo test -p sealwax-engine --release --test testbed -- --ignored --test-threads=1 --nocapture

mod common;

use common::*;
use sealwax_domain::account::{AccountSettings, LoginError, LoginFailure, Protocol, Security};
use sealwax_domain::model::{FolderId, FolderRole};
use sealwax_domain::sync::Window;
use sealwax_engine::api::Password;
use sealwax_engine::imap::{self, Progress, SyncOptions};
use sealwax_engine::net::Trust;
use sealwax_engine::verify::verify;
use sealwax_infrastructure::store::{OpenView, ServerCounts, Store, SyncState, ViewSpec};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn password(text: &str) -> Password {
    Password::new(text.to_owned())
}

fn whole_folder() -> SyncOptions {
    SyncOptions {
        window: Window::ALL,
        ..SyncOptions::default()
    }
}

/// `verify`, paced: Stalwart's submission listener drops the sixth connection that arrives
/// within about a second from one address (seen on the testbed, 2026-10-05).
async fn check(
    account: &AccountSettings,
    second: Option<&str>,
    password: &Password,
    trust: &Trust,
) -> Result<String, LoginError> {
    tokio::time::sleep(Duration::from_millis(300)).await;
    verify(account, second, password, trust).await
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn the_login_check_on_the_testbed() {
    runtime().block_on(async {
        let trust = trust();
        let right = password(PASSWORD);
        for server in SERVERS {
            for security in [Security::Tls, Security::StartTls] {
                let account = settings(imap(server, security), submission(security), "alice");
                assert_eq!(
                    check(&account, None, &right, &trust).await,
                    Ok("alice".to_owned()),
                    "{} {security:?}",
                    server.name
                );
            }
        }
        let account = settings(imap(STALWART, Security::Tls), mailpit(), "alice");
        assert_eq!(
            check(&account, None, &right, &trust).await,
            Ok("alice".to_owned()),
            "Mailpit over STARTTLS"
        );

        let refused = |protocol, failure| Err(LoginError { protocol, failure });
        for server in SERVERS {
            let account = settings(
                imap(server, Security::Tls),
                submission(Security::Tls),
                "alice",
            );
            assert_eq!(
                check(&account, None, &password("wrong-password"), &trust).await,
                refused(Protocol::Imap, LoginFailure::WrongPassword),
                "{}",
                server.name
            );
        }
        let mut account = settings(
            imap(STALWART, Security::Tls),
            submission(Security::Tls),
            "alice",
        );
        account.username = "nobody".into();
        assert_eq!(
            check(&account, Some("alice"), &right, &trust).await,
            Ok("alice".to_owned()),
            "the second username is tried once"
        );
        // Both Dovecots accept any user name with the testbed password (static passdb), so the
        // IMAP check passes and Stalwart's SMTP refuses the name.
        let account = settings(
            imap(DOVECOT, Security::Tls),
            submission(Security::StartTls),
            "nobody",
        );
        assert_eq!(
            check(&account, None, &right, &trust).await,
            refused(Protocol::Smtp, LoginFailure::WrongPassword)
        );
        let account = settings(
            imap(DOVECOT, Security::Tls),
            submission(Security::Tls),
            "alice",
        );
        assert_eq!(
            check(&account, None, &right, &Trust::Platform).await,
            refused(Protocol::Imap, LoginFailure::Certificate),
            "the platform verifier doesn't know the testbed CA"
        );
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let account = settings(
            endpoint(port, Security::Tls),
            submission(Security::Tls),
            "alice",
        );
        assert_eq!(
            check(&account, None, &right, &trust).await,
            refused(Protocol::Imap, LoginFailure::Unreachable)
        );
        // Ports that speak another protocol's plaintext: no TLS.
        let account = settings(
            endpoint(16025, Security::Tls),
            submission(Security::Tls),
            "alice",
        );
        assert_eq!(
            check(&account, None, &right, &trust).await,
            refused(Protocol::Imap, LoginFailure::NoTls)
        );
    });
}

fn folder_list(server: imap::SyncedFolders) -> Vec<(String, FolderRole, bool)> {
    server
        .folders
        .into_iter()
        .map(|(_, folder)| (folder.name, folder.role, folder.selectable))
        .collect()
}

fn role_of(folders: &[(String, FolderRole, bool)], name: &str) -> FolderRole {
    folders
        .iter()
        .find(|(found, ..)| found == name)
        .unwrap_or_else(|| panic!("{name} not listed"))
        .1
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn the_folder_list_on_every_server() {
    runtime().block_on(async {
        for server in SERVERS {
            for security in [Security::Tls, Security::StartTls] {
                let (_dir, store, account) = store();
                let mut conn = login(server, security, "alice").await;
                let caps = conn.caps();
                println!(
                    "{} {security:?}: {:?} LIST-STATUS {} LIST-EXTENDED {} SPECIAL-USE {} \
                     QRESYNC enabled {} ID {}",
                    server.name,
                    conn.mechanism(),
                    caps.has("LIST-STATUS"),
                    caps.has("LIST-EXTENDED"),
                    caps.has("SPECIAL-USE"),
                    conn.qresync(),
                    caps.has("ID"),
                );
                let synced = imap::sync_folders(&mut conn, &store, account)
                    .await
                    .unwrap();
                let inbox = synced.inbox.expect("INBOX found");
                let folders = folder_list(synced);
                let what = format!("{} {security:?}", server.name);
                for name in [
                    "Søppelpost",
                    "Papirkurv",
                    "Sendt",
                    "Utkast",
                    "Arkiv",
                    "Økonomi & regnskap/Kvitteringer",
                    "Prosjekter/Høst 📬 2026",
                ] {
                    assert!(folders.iter().any(|(n, ..)| n == name), "{what}: {name}");
                }
                assert_eq!(role_of(&folders, "INBOX"), FolderRole::Inbox, "{what}");
                if server == STALWART {
                    assert_eq!(role_of(&folders, "Sent Items"), FolderRole::Sent, "{what}");
                    assert_eq!(
                        role_of(&folders, "Deleted Items"),
                        FolderRole::Trash,
                        "{what}"
                    );
                    assert_eq!(role_of(&folders, "Junk Mail"), FolderRole::Junk, "{what}");
                    assert_eq!(role_of(&folders, "Drafts"), FolderRole::Drafts, "{what}");
                    assert_eq!(role_of(&folders, "Sendt"), FolderRole::Other, "{what}");
                } else {
                    assert_eq!(role_of(&folders, "Sendt"), FolderRole::Sent, "{what}");
                    assert_eq!(role_of(&folders, "Søppelpost"), FolderRole::Junk, "{what}");
                    assert_eq!(role_of(&folders, "Papirkurv"), FolderRole::Trash, "{what}");
                    assert_eq!(role_of(&folders, "Utkast"), FolderRole::Drafts, "{what}");
                }
                assert_eq!(role_of(&folders, "Arkiv"), FolderRole::Archive, "{what}");
                let counts = store.folders().recv().await.unwrap().value;
                let inbox_counts = counts.iter().find(|f| f.id == inbox).unwrap();
                assert_eq!(
                    (inbox_counts.total, inbox_counts.unread),
                    (30, 10),
                    "{what}: STATUS counts before the first sync"
                );
                let sent = counts.iter().find(|f| f.name == "Sendt").unwrap();
                assert_eq!((sent.total, sent.unread), (5, 1), "{what}");
                assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
                conn.logout().await;
                store.close().unwrap();
            }
        }
    });
}

/// Rows of a folder's view, newest first.
async fn rows(store: &Store, folder: FolderId) -> Vec<sealwax_domain::model::MessageSummary> {
    store
        .open_view(OpenView {
            spec: ViewSpec::folder(folder),
            first_page: 20_000,
            section_starts: Vec::new(),
        })
        .recv()
        .await
        .unwrap()
        .rows
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn the_first_inbox_sync_on_every_server() {
    runtime().block_on(async {
        for server in SERVERS {
            let (_dir, store, account) = store();
            let mut conn = login(server, Security::Tls, "alice").await;
            let listed = imap::sync_folders(&mut conn, &store, account)
                .await
                .unwrap();
            let inbox = listed.inbox.unwrap();
            // 31 after the seed; higher once a message was delivered and removed again, as on
            // Stalwart since 2026-10-05 (UIDs are never reused).
            let uidnext = listed
                .folders
                .iter()
                .find(|(id, _)| *id == inbox)
                .and_then(|(_, folder)| folder.status.as_ref()?.uidnext);
            let mut progress = Vec::new();
            let report = imap::first_sync(&mut conn, &store, inbox, |p| progress.push(p))
                .await
                .unwrap();
            let what = server.name;
            assert_eq!((report.fetched, report.total), (30, 30), "{what}");
            assert_eq!(
                progress,
                vec![
                    Progress {
                        fetched: 0,
                        total: 30
                    },
                    Progress {
                        fetched: 30,
                        total: 30
                    }
                ],
                "{what}"
            );
            let rows = rows(&store, inbox).await;
            assert_eq!(rows.len(), 30, "{what}");
            assert!(rows.windows(2).all(|pair| pair[0].date >= pair[1].date));
            assert_eq!(
                rows.iter().filter(|r| !r.flags.is_seen()).count(),
                10,
                "{what}"
            );
            assert_eq!(
                rows.iter().filter(|r| r.flags.is_flagged()).count(),
                2,
                "{what}"
            );
            assert_eq!(
                rows.iter().filter(|r| r.has_attachments).count(),
                4,
                "{what}"
            );
            assert_eq!(
                rows.iter()
                    .filter(|r| r.subject.starts_with("Hilsen fra Bærum"))
                    .count(),
                6,
                "{what}: RFC 2047 subjects decoded"
            );
            let empty: Vec<_> = rows
                .iter()
                .filter(|r| r.snippet.is_empty())
                .map(|r| r.subject.clone())
                .collect();
            assert!(empty.is_empty(), "{what}: no snippet for {empty:?}");
            assert!(rows.iter().any(|r| r.thread_count > 1), "{what}: threads");
            assert!(
                rows.iter()
                    .all(|r| r.sender_name.starts_with("Synthetic Sender"))
            );
            let state = store.folder_sync(inbox).recv().await.unwrap().unwrap();
            assert_eq!(state.state, SyncState::Ready, "{what}");
            assert_eq!(Some(state.uidnext), uidnext, "{what}: the server's UIDNEXT");
            assert!(state.uidnext >= 31, "{what}");
            let counts = store.folders().recv().await.unwrap().value;
            let inbox_counts = counts.iter().find(|f| f.id == inbox).unwrap();
            assert_eq!(
                (inbox_counts.total, inbox_counts.unread),
                (30, 10),
                "{what}"
            );
            let again = imap::first_sync(&mut conn, &store, inbox, |_| {})
                .await
                .unwrap();
            assert_eq!(
                again.fetched, 0,
                "{what}: a finished first sync is M3's to re-sync"
            );
            assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
            conn.logout().await;
            store.close().unwrap();
        }
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn an_interrupted_first_sync_resumes_and_a_new_uidvalidity_starts_over() {
    runtime().block_on(async {
        for server in SERVERS {
            let what = server.name;
            let (_dir, store, account) = store();
            let mut conn = login(server, Security::Tls, "perf").await;
            let inbox = imap::sync_folders(&mut conn, &store, account)
                .await
                .unwrap()
                .inbox
                .unwrap();
            let cut = Arc::new(tokio::sync::Notify::new());
            let signal = cut.clone();
            let whole = whole_folder();
            tokio::select! {
                _ = imap::first_sync_with(&mut conn, &store, inbox, &whole, |p| {
                    if p.fetched >= 1_000 {
                        signal.notify_one();
                    }
                }) => panic!("{what}: finished before the cut"),
                _ = cut.notified() => {}
            }
            drop(conn);
            tokio::time::sleep(Duration::from_millis(200)).await;
            let state = store.folder_sync(inbox).recv().await.unwrap().unwrap();
            let SyncState::Initial { floor, top, .. } = state.state else {
                panic!("{what}: {:?}", state.state);
            };
            assert!(floor > 1 && floor < top, "{what}: floor {floor}");
            assert!(state.messages >= 1_000, "{what}");

            let mut conn = login(server, Security::Tls, "perf").await;
            let report = imap::first_sync_with(&mut conn, &store, inbox, &whole_folder(), |_| {})
                .await
                .unwrap();
            assert!(report.resumed, "{what}");
            let state = store.folder_sync(inbox).recv().await.unwrap().unwrap();
            assert_eq!(
                (state.state, state.messages),
                (SyncState::Ready, 10_000),
                "{what}"
            );
            assert_eq!(report.total, 10_000, "{what}");

            store.reset_folder(inbox, 1).recv().await.unwrap();
            let report = imap::first_sync_with(&mut conn, &store, inbox, &whole_folder(), |_| {})
                .await
                .unwrap();
            assert!(report.reset && !report.resumed, "{what}");
            assert_eq!(report.fetched, 10_000, "{what}");
            assert_eq!(store.check().recv().await.unwrap(), Vec::<String>::new());
            conn.logout().await;
            store.close().unwrap();
        }
    });
}

/// ML-037's bar: 1,000 headers in the store within 5 s, from connecting on. Release build.
#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn first_sync_speed() {
    runtime().block_on(async {
        for server in SERVERS {
            for run in 1..=3 {
                let (_dir, store, account) = store();
                let started = Instant::now();
                let mut conn = login(server, Security::Tls, "perf").await;
                let connected = started.elapsed();
                let inbox = imap::sync_folders(&mut conn, &store, account)
                    .await
                    .unwrap()
                    .inbox
                    .unwrap();
                let listed = started.elapsed();
                let mut first_thousand = None;
                let report =
                    imap::first_sync_with(&mut conn, &store, inbox, &whole_folder(), |p| {
                        if p.fetched >= 1_000 && first_thousand.is_none() {
                            first_thousand = Some((started.elapsed(), p.fetched));
                        }
                    })
                    .await
                    .unwrap();
                let total = started.elapsed();
                let (thousand, at) = first_thousand.unwrap();
                println!(
                    "{} run {run}: connect {:.1} ms, + LIST {:.1} ms, {at} headers at {:.1} ms, \
                     {} headers at {:.1} ms in {} batches",
                    server.name,
                    ms(connected),
                    ms(listed),
                    ms(thousand),
                    report.fetched,
                    ms(total),
                    report.batches
                );
                assert_eq!(report.fetched, 10_000);
                assert!(
                    thousand <= Duration::from_secs(5),
                    "{}: {thousand:?}",
                    server.name
                );
                let counts = store.folders().recv().await.unwrap().value;
                let inbox_counts = counts.iter().find(|f| f.id == inbox).unwrap();
                assert_eq!(
                    ServerCounts {
                        total: inbox_counts.total,
                        unread: inbox_counts.unread
                    },
                    ServerCounts {
                        total: 10_000,
                        unread: 3_333
                    }
                );
                conn.logout().await;
                store.close().unwrap();
            }
        }
    });
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
