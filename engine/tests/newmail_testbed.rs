//! M7's testbed suite for new mail (ML-083), through the engine service: another connection
//! APPENDs to INBOX while the account's actor idles, and `Event::NewMail` names exactly the new
//! messages, timed from the APPEND's OK to the event; nothing for the first sync, for older
//! mail, or after a restart for mail stored before it. They need the Docker testbed running and
//! seeded and are skipped otherwise. They reset and change the user newmail:
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test newmail_testbed -- --ignored --test-threads=1 --nocapture

mod common;
mod mutator;
mod snapshot;
mod testbed_client;
mod testbed_engine;

use common::{DOVECOT, DOVECOT_REV1, PASSWORD, STALWART, Server};
use mutator::{client, reset_account};
use sealwax_domain::model::{FolderId, MessageId};
use sealwax_domain::sync::Window;
use sealwax_engine::api::{Command, Event, Password, SyncStatus};
use sealwax_engine::imap::SyncOptions;
use sealwax_engine::service::SyncConfig;
use snapshot::Rng;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use testbed_client::Client;
use testbed_engine::TestEngine;

const USER: &str = "newmail";
const WAIT: Duration = Duration::from_secs(60);

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// A synthetic message with `message_id`, unread.
async fn deliver(appender: &mut Client, message_id: &str, rng: &mut Rng) {
    let message = testbed_client::message(message_id, rng);
    appender.append("INBOX", "", &message).await.unwrap();
}

/// The Message-IDs of stored messages.
async fn message_ids(engine: &TestEngine, ids: &[MessageId]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for id in ids {
        let detail = engine.store.message(*id).recv().await.unwrap().value;
        found.insert(
            detail
                .and_then(|detail| detail.headers.message_id)
                .unwrap_or_default(),
        );
    }
    found
}

fn new_mail_events(engine: &TestEngine) -> usize {
    engine
        .seen
        .iter()
        .filter(|event| matches!(event, Event::NewMail { .. }))
        .count()
}

/// Reads events for `quiet`.
async fn drain(engine: &mut TestEngine, quiet: Duration) {
    let _ = engine.event(quiet, |_| None::<()>).await;
}

/// The next `NewMail` for `folder`.
async fn next_new_mail(engine: &mut TestEngine, folder: FolderId) -> Option<Vec<MessageId>> {
    engine
        .event(Duration::from_secs(10), |event| match event {
            Event::NewMail {
                folder: found,
                messages,
                ..
            } if *found == folder => Some(messages.clone()),
            _ => None,
        })
        .await
}

/// With the production intervals: 20 messages APPENDed to INBOX one at a time by another
/// connection while the actor idles, each `NewMail` timed from the APPEND's OK and checked to
/// name exactly that message; none for the first sync.
fn latency(server: Server) {
    runtime().block_on(async {
        let mut appender = client(server.tls, USER).await;
        reset_account(&mut appender, &["INBOX"]).await.unwrap();
        let mut rng = Rng(83);
        for i in 0..5 {
            deliver(
                &mut appender,
                &format!("newmail.before.{i}@example.test"),
                &mut rng,
            )
            .await;
        }
        let mut engine = TestEngine::start(SyncConfig::default());
        let account = engine.add(1, USER, server.tls).await;
        assert_eq!(engine.status(account, WAIT).await, Some(SyncStatus::Synced));
        let inbox = engine.folder(account, "INBOX").await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        drain(&mut engine, Duration::from_millis(100)).await;
        assert_eq!(
            new_mail_events(&engine),
            0,
            "{}: the first sync",
            server.name
        );
        let mut samples = Vec::new();
        for i in 0..20u32 {
            let message_id = format!("newmail.latency.{i}@example.test");
            deliver(&mut appender, &message_id, &mut rng).await;
            let appended = Instant::now();
            let messages = next_new_mail(&mut engine, inbox).await;
            let elapsed = appended.elapsed();
            let messages =
                messages.unwrap_or_else(|| panic!("{}: no NewMail for {i}", server.name));
            assert_eq!(
                message_ids(&engine, &messages).await,
                BTreeSet::from([message_id]),
                "{}: exactly the new message",
                server.name
            );
            samples.push(ms(elapsed));
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        appender.logout().await;
        drain(&mut engine, Duration::from_millis(500)).await;
        assert_eq!(new_mail_events(&engine), 20, "{}: one each", server.name);
        let stop = engine.stop();
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        println!(
            "{} new mail through IDLE, APPEND OK to NewMail, 20 samples: median {:.1} ms, p95 \
             {:.1} ms, max {:.1} ms; all {samples:.1?}; stop {:.0} ms",
            server.name,
            sorted[10],
            sorted[18],
            sorted[19],
            ms(stop)
        );
        assert!(sorted[19] <= 5000.0, "{}: over 5 s", server.name);
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn new_mail_latency_dovecot() {
    latency(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn new_mail_latency_stalwart() {
    latency(STALWART);
}

/// A narrow first-sync window, so older mail is left on the server, and INBOX polled every
/// second where the server has no IDLE.
fn windowed() -> SyncConfig {
    SyncConfig {
        options: SyncOptions {
            window: Window {
                days: 90,
                max: 30,
                min: 10,
            },
            older: 15,
            ..SyncOptions::default()
        },
        inbox_poll: Duration::from_secs(1),
        ..SyncConfig::default()
    }
}

/// 60 messages, of which the first sync stores 30: no `NewMail` for the first sync, for two
/// loads of older mail, or after a restart for what is stored; one for a message after it.
fn not_new(server: Server) {
    runtime().block_on(async {
        let mut appender = client(server.tls, USER).await;
        reset_account(&mut appender, &["INBOX"]).await.unwrap();
        let mut rng = Rng(84);
        for i in 0..60 {
            deliver(
                &mut appender,
                &format!("newmail.old.{i}@example.test"),
                &mut rng,
            )
            .await;
        }
        let mut engine = TestEngine::start(windowed());
        let account = engine.add(1, USER, server.tls).await;
        assert_eq!(engine.status(account, WAIT).await, Some(SyncStatus::Synced));
        let inbox = engine.folder(account, "INBOX").await.unwrap();
        assert_eq!(engine.count(inbox).await, 30, "{}", server.name);
        for _ in 0..2 {
            engine.send(Command::LoadOlder { folder: inbox });
            let fetched = engine
                .event(WAIT, |event| match event {
                    Event::OlderLoaded {
                        folder, fetched, ..
                    } if *folder == inbox => Some(*fetched),
                    _ => None,
                })
                .await;
            assert_eq!(fetched, Some(15), "{}", server.name);
        }
        drain(&mut engine, Duration::from_secs(2)).await;
        assert_eq!(engine.count(inbox).await, 60, "{}", server.name);
        assert_eq!(
            new_mail_events(&engine),
            0,
            "{}: the first sync and older mail",
            server.name
        );
        let mut engine = engine.restart(windowed());
        let key = engine.accounts()[0].key.clone();
        engine.send(Command::UpdatePassword {
            request: 2,
            key,
            password: Password::new(PASSWORD.into()),
        });
        let updated = engine
            .event(WAIT, |event| match event {
                Event::PasswordUpdated { result, .. } => Some(result.is_ok()),
                _ => None,
            })
            .await;
        assert_eq!(updated, Some(true), "{}", server.name);
        assert_eq!(
            engine.settled(account, WAIT).await,
            Some(SyncStatus::Synced),
            "{}",
            server.name
        );
        drain(&mut engine, Duration::from_secs(3)).await;
        assert_eq!(
            new_mail_events(&engine),
            0,
            "{}: stored before the restart",
            server.name
        );
        let message_id = "newmail.after-restart@example.test".to_owned();
        deliver(&mut appender, &message_id, &mut rng).await;
        let messages = next_new_mail(&mut engine, inbox)
            .await
            .unwrap_or_else(|| panic!("{}: no NewMail after the restart", server.name));
        assert_eq!(
            message_ids(&engine, &messages).await,
            BTreeSet::from([message_id]),
            "{}",
            server.name
        );
        appender.logout().await;
        println!(
            "{}: no NewMail for the first sync, 2 loads of older mail and a restart; 1 for the \
             message after it",
            server.name
        );
    });
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn not_new_mail_dovecot() {
    not_new(DOVECOT);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn not_new_mail_dovecot_rev1() {
    not_new(DOVECOT_REV1);
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn not_new_mail_stalwart() {
    not_new(STALWART);
}
