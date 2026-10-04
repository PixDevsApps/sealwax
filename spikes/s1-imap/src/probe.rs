//! Behaviour probes: what the servers advertise and do, and what async-imap's typed API reports
//! for the same commands.

use std::time::Duration;

use anyhow::Result;
use futures_util::TryStreamExt;

use crate::net::{ConnectOptions, Security};
use crate::ops;
use crate::random::{append, empty_mailbox};
use crate::raw::{self, ImapError};
use crate::rng::Rng;
use crate::sync;
use crate::synth;
use crate::testbed::{self, Server};

pub type Lines = Vec<(String, String)>;

fn line(out: &mut Lines, check: &str, result: impl std::fmt::Display) {
    out.push((check.to_owned(), result.to_string()));
}

pub async fn probe(server: Server) -> Result<Lines> {
    let mut out = Lines::new();
    let tls = testbed::tls()?;
    let login = testbed::login("spike");

    let mut conn = crate::net::connect(
        &testbed::endpoint(server, Security::Tls),
        tls.clone(),
        &login,
        ConnectOptions::default(),
    )
    .await?;
    line(
        &mut out,
        "implicit TLS: authentication",
        format!("{:?}", conn.auth),
    );
    line(
        &mut out,
        "capabilities after login",
        conn.caps.sorted().join(" "),
    );
    line(&mut out, "QRESYNC enabled", conn.qresync);
    let starttls = crate::net::connect(
        &testbed::endpoint(server, Security::StartTls),
        tls.clone(),
        &login,
        ConnectOptions::default(),
    )
    .await;
    line(
        &mut out,
        "STARTTLS: authentication",
        match starttls {
            Ok(c) => {
                let auth = format!("{:?}", c.auth);
                c.logout().await;
                auth
            }
            Err(e) => format!("failed: {e}"),
        },
    );
    let platform = crate::net::connect(
        &testbed::endpoint(server, Security::Tls),
        crate::tls::platform()?,
        &login,
        ConnectOptions::default(),
    )
    .await;
    line(
        &mut out,
        "platform verifier rejects the testbed certificate",
        match platform {
            Ok(_) => "NO (accepted)".to_owned(),
            Err(e) => format!("yes ({e})"),
        },
    );

    for listed in sync::listed_mailboxes(&sync::list(&mut conn).await?) {
        line(
            &mut out,
            &format!("LIST {:?}", listed.name_raw),
            format!(
                "{:?} role={:?} attrs={:?}",
                listed.name, listed.role, listed.attributes
            ),
        );
    }

    let mut rng = Rng::new(7);
    empty_mailbox(&mut conn, "INBOX").await?;
    for i in 0..150 {
        append(
            &mut conn,
            "INBOX",
            &format!("spike.probe.{i}@example.test"),
            &mut rng,
        )
        .await?;
    }

    {
        conn.session.examine("INBOX").await?;
        let items: Result<Vec<_>, _> = conn
            .session
            .uid_fetch("1:*", "(FLAGS BOGUS)")
            .await?
            .try_collect()
            .await;
        line(
            &mut out,
            "async-imap uid_fetch with BAD items",
            format!("{:?}", items.map(|v| v.len())),
        );
        let raw_result = raw::run(&mut conn.session, "UID FETCH 1:* (FLAGS BOGUS)", |_| {}).await;
        line(
            &mut out,
            "raw layer, same command",
            format!("{:?}", raw_result.map(|_| "OK")),
        );

        let found = conn.session.uid_search("BOGUSKEY 1").await;
        line(
            &mut out,
            "async-imap uid_search with BAD criteria",
            format!("{:?}", found.map(|s| s.len())),
        );

        let stored: Result<Vec<_>, _> = conn
            .session
            .uid_store("1", "+FLAGS (\\Seen)")
            .await?
            .try_collect()
            .await;
        line(
            &mut out,
            "async-imap uid_store on an EXAMINEd mailbox",
            format!("{:?}", stored.map(|v| v.len())),
        );
        let raw_store = raw::run(&mut conn.session, "UID STORE 1 +FLAGS (\\Seen)", |_| {}).await;
        line(
            &mut out,
            "raw layer, same command",
            format!("{:?}", raw_store.map(|_| "OK")),
        );
    }

    {
        conn.session.select("INBOX").await?;
        while conn.session.unsolicited_responses.try_recv().is_ok() {}
        let mut other = testbed::account(server, "spike")?;
        other.connect = ConnectOptions {
            enable_qresync: false,
            send_id: false,
        };
        let mut other = other.connect().await?;
        ops::select(&mut other, "INBOX").await?;
        raw::run(&mut other.session, "UID STORE 1:* +FLAGS (Storm)", |_| {}).await?;
        other.logout().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        conn.session.noop().await?;
        let mut received = 0;
        while conn.session.unsolicited_responses.try_recv().is_ok() {
            received += 1;
        }
        line(
            &mut out,
            "async-imap unsolicited channel after 150 flag changes by another client",
            format!("{received} responses delivered (capacity 100; the rest dropped silently)"),
        );
    }

    let message_id = format!("spike.appenduid.{}@example.test", std::process::id());
    let message = synth::message(&message_id, &mut rng);
    let appenduid = ops::append_raw(&mut conn, "INBOX", "", &message).await?;
    line(&mut out, "raw APPEND: APPENDUID", format!("{appenduid:?}"));
    let library_id = format!("spike.library.{}@example.test", std::process::id());
    let library_message = synth::message(&library_id, &mut rng);
    let by_search =
        ops::append_library(&mut conn, "INBOX", "", &library_message, &library_id).await?;
    line(
        &mut out,
        "async-imap append (returns ()), UID by HEADER Message-ID search",
        format!("{by_search:?}"),
    );

    let mut misses = 0;
    for i in 0..20 {
        let id = format!("spike.search.{}.{i}@example.test", std::process::id());
        let message = synth::message(&id, &mut rng);
        ops::append_raw(&mut conn, "INBOX", "", &message).await?;
        if ops::find_by_message_id(&mut conn, "INBOX", &id)
            .await?
            .is_empty()
        {
            misses += 1;
        }
    }
    line(
        &mut out,
        "HEADER Message-ID search right after APPEND: misses out of 20",
        misses,
    );

    if conn.caps.has("MOVE") {
        crate::random::ensure_mailbox(&mut conn, "Arkiv").await?;
        let copyuid = ops::move_raw(&mut conn, "INBOX", 1, "Arkiv").await?;
        line(&mut out, "raw UID MOVE: COPYUID", format!("{copyuid:?}"));
        let gone = ops::move_raw(&mut conn, "INBOX", 1, "Arkiv").await;
        line(
            &mut out,
            "UID MOVE of a UID that no longer exists",
            format!("{:?}", gone.map(|c| c.is_some())),
        );
    }

    if conn.caps.has("ESEARCH") {
        let esearch = raw::run(&mut conn.session, "UID SEARCH RETURN (COUNT) ALL", |_| {}).await;
        line(
            &mut out,
            "ESEARCH response through async-imap's parser",
            format!("{:?}", esearch.map(|_| "OK")),
        );
        let after = raw::run(&mut conn.session, "NOOP", |_| {}).await;
        line(
            &mut out,
            "connection usable after that",
            format!(
                "{:?}",
                after.map(|_| "yes").map_err(|e: ImapError| e.to_string())
            ),
        );
    }

    conn.logout().await;
    Ok(out)
}
