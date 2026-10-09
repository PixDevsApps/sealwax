//! M9's testbed suite for folder management (ADR-020) on Dovecot (full and IMAP4rev1-only) and
//! Stalwart, through `imap::manage` on a real connection and a scratch store: CREATE and
//! SUBSCRIBE, at the top and under a folder, with a non-ASCII name; the engine's own refusal of
//! a sibling's name (nothing sent) and the server's of a name only it knew; RENAME of a folder
//! with a subfolder, the subscriptions moved and the store's ids kept; DELETE deepest first, a
//! subfolder the store never saw included. Each run uses folder names of its own under the user
//! `spike` and deletes them at its end (and those an earlier run left). It needs the Docker
//! testbed running and seeded:
//!
//! SEALWAX_TESTBED_CA=…/tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test folders_testbed -- --ignored --test-threads=1 --nocapture
//!
//! NAMESPACE isn't sent (imap-proto can't read its reply; `imap::manage`), so the run prints
//! where a new folder at the top of each server went.

mod common;
mod snapshot;
mod testbed_client;

use async_imap::imap_proto::{MailboxDatum, Response};
use common::{SERVERS, Server};
use sealwax_domain::account::Security;
use sealwax_domain::folders::NameProblem;
use sealwax_domain::model::{AccountId, Folder};
use sealwax_engine::api::{FolderChange, FolderError};
use sealwax_engine::imap::{self, Connection, manage};
use sealwax_infrastructure::store::Store;
use std::time::{Duration, Instant};
use testbed_client::Client;

const USER: &str = "spike";
/// Every folder this suite makes starts so.
const PREFIX: &str = "Sealwax mappe ";

fn runtime() -> tokio::runtime::Runtime {
    sealwax_engine::runtime().unwrap()
}

async fn client(server: Server) -> Client {
    Client::connect(server.tls, &common::ca(), USER)
        .await
        .unwrap_or_else(|error| panic!("test client on {}: {error:?}", server.name))
}

/// The server's mailboxes (wire names), by LIST or LSUB, starting with `start`.
async fn listed(client: &mut Client, verb: &str, start: &str) -> Vec<String> {
    let mut names = Vec::new();
    client
        .run(&format!("{verb} \"\" \"*\""), |response| {
            if let Response::MailboxData(MailboxDatum::List(list)) = response
                && list.name.starts_with(start)
            {
                names.push(list.name.to_string());
            }
        })
        .await
        .unwrap();
    names.sort();
    names
}

/// Deletes the suite's folders the server holds (at the top or under the Inbox), the deepest
/// first, and their subscriptions.
async fn clean(client: &mut Client) {
    let mut names = listed(client, "LIST", "").await;
    names.extend(listed(client, "LSUB", "").await);
    names.retain(|name| name.contains(PREFIX));
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    names.dedup();
    for name in names {
        let quoted = format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""));
        let _ = client.run(&format!("DELETE {quoted}"), |_| {}).await;
        let _ = client.run(&format!("UNSUBSCRIBE {quoted}"), |_| {}).await;
    }
}

async fn folders(store: &Store, account: AccountId) -> Vec<Folder> {
    store
        .folders()
        .recv()
        .await
        .unwrap()
        .value
        .into_iter()
        .filter(|folder| folder.account == account)
        .collect()
}

async fn named(store: &Store, account: AccountId, name: &str) -> Option<Folder> {
    folders(store, account)
        .await
        .into_iter()
        .find(|folder| folder.name == name)
}

/// One change, timed.
async fn change(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
    change: FolderChange,
) -> (Result<(), FolderError>, Duration) {
    let started = Instant::now();
    let result = manage::change(conn, store, account, &change)
        .await
        .map(|_| ());
    (result, started.elapsed())
}

async fn run_on(server: Server) {
    let name = server.name;
    let mut client = client(server).await;
    clean(&mut client).await;
    let (_dir, store, account) = common::store();
    let mut conn = common::login(server, Security::Tls, USER).await;
    imap::sync_folders(&mut conn, &store, account)
        .await
        .unwrap();
    let inbox = named(&store, account, "INBOX").await.expect("INBOX");
    let delimiter = inbox.delimiter.expect("a hierarchy delimiter");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let top = format!("{PREFIX}{stamp} æ");

    // CREATE at the top, then under it.
    let (result, took) = change(
        &mut conn,
        &store,
        account,
        FolderChange::Create {
            parent: None,
            name: top.clone(),
        },
    )
    .await;
    result.unwrap_or_else(|error| panic!("{name}: create at the top: {error:?}"));
    let made = folders(&store, account)
        .await
        .into_iter()
        .find(|folder| folder.name.ends_with(&top))
        .unwrap_or_else(|| panic!("{name}: the new folder isn't in the store"));
    println!(
        "{name}: create at the top {took:?}; delimiter {delimiter:?}; it went to {:?}",
        if made.name == top {
            "the top".to_owned()
        } else {
            made.name.replace(&top, "<new>")
        }
    );
    let parent = made.name.clone();
    let wire_parent = imap::mutf7::encode(&parent);
    assert!(
        listed(&mut client, "LIST", "").await.contains(&wire_parent),
        "{name}: the server lists no new folder"
    );
    let (result, took) = change(
        &mut conn,
        &store,
        account,
        FolderChange::Create {
            parent: Some(made.id),
            name: "Acme".into(),
        },
    )
    .await;
    result.unwrap_or_else(|error| panic!("{name}: create under it: {error:?}"));
    println!("{name}: create under it {took:?}");
    let child = named(&store, account, &format!("{parent}{delimiter}Acme"))
        .await
        .unwrap_or_else(|| panic!("{name}: the subfolder isn't in the store"));
    let wire_child = format!("{wire_parent}{delimiter}Acme");
    let lsub = listed(&mut client, "LSUB", "").await;
    assert!(
        lsub.contains(&wire_parent) && lsub.contains(&wire_child),
        "{name}: not subscribed: {lsub:?}"
    );

    // The engine refuses a sibling's name itself; the server refuses one only it knew.
    let before = Instant::now();
    let (result, _) = change(
        &mut conn,
        &store,
        account,
        FolderChange::Create {
            parent: Some(made.id),
            name: "ACME".into(),
        },
    )
    .await;
    assert!(
        matches!(result, Err(FolderError::Name(NameProblem::Taken))),
        "{name}: {result:?}"
    );
    println!(
        "{name}: a sibling's name refused before sending, {:?}",
        before.elapsed()
    );
    let other = format!("{wire_parent}{delimiter}Fjord");
    client.create(&other).await.unwrap();
    let (result, _) = change(
        &mut conn,
        &store,
        account,
        FolderChange::Create {
            parent: Some(made.id),
            name: "Fjord".into(),
        },
    )
    .await;
    match result {
        Err(FolderError::Refused(text)) => {
            println!("{name}: the server's refusal: code {:?}", text.code());
        }
        other => panic!("{name}: a name only the server knew: {other:?}"),
    }
    assert!(
        named(&store, account, &format!("{parent}{delimiter}Fjord"))
            .await
            .is_some(),
        "{name}: the refusal didn't list the folders again"
    );

    // RENAME of the folder with its subfolders.
    let renamed_top = format!("{PREFIX}{stamp} ø");
    let (result, took) = change(
        &mut conn,
        &store,
        account,
        FolderChange::Rename {
            folder: made.id,
            name: renamed_top.clone(),
        },
    )
    .await;
    result.unwrap_or_else(|error| panic!("{name}: rename: {error:?}"));
    println!("{name}: rename with two subfolders {took:?}");
    let renamed_parent = parent.replace(&top, &renamed_top);
    let wire_renamed = imap::mutf7::encode(&renamed_parent);
    let after = listed(&mut client, "LIST", "").await;
    assert!(
        after.contains(&wire_renamed)
            && after.contains(&format!("{wire_renamed}{delimiter}Acme"))
            && !after.contains(&wire_parent)
            && !after.contains(&wire_child),
        "{name}: {after:?}"
    );
    let lsub = listed(&mut client, "LSUB", "").await;
    assert!(
        lsub.contains(&wire_renamed)
            && lsub.contains(&format!("{wire_renamed}{delimiter}Acme"))
            && !lsub.contains(&wire_parent)
            && !lsub.contains(&wire_child),
        "{name}: subscriptions {lsub:?}"
    );
    let kept = named(&store, account, &format!("{renamed_parent}{delimiter}Acme"))
        .await
        .unwrap_or_else(|| panic!("{name}: the renamed subfolder isn't in the store"));
    assert_eq!(kept.id, child.id, "{name}: the subfolder kept its id");

    // DELETE deepest first, with a subfolder the store never saw.
    client
        .create(&format!("{wire_renamed}{delimiter}Acme{delimiter}2026"))
        .await
        .unwrap();
    let (result, took) = change(
        &mut conn,
        &store,
        account,
        FolderChange::Delete { folder: made.id },
    )
    .await;
    result.unwrap_or_else(|error| panic!("{name}: delete: {error:?}"));
    println!("{name}: delete of four folders {took:?}");
    let left: Vec<String> = listed(&mut client, "LIST", "")
        .await
        .into_iter()
        .filter(|wire| wire.starts_with(&wire_renamed))
        .collect();
    assert!(left.is_empty(), "{name}: left on the server: {left:?}");
    let subscribed: Vec<String> = listed(&mut client, "LSUB", "")
        .await
        .into_iter()
        .filter(|wire| wire.starts_with(&wire_renamed))
        .collect();
    assert!(
        subscribed.is_empty(),
        "{name}: still subscribed: {subscribed:?}"
    );
    assert!(
        folders(&store, account)
            .await
            .iter()
            .all(|folder| !folder.name.starts_with(&renamed_parent)),
        "{name}: the store kept deleted folders"
    );
    clean(&mut client).await;
    conn.logout_within(Duration::from_secs(2)).await;
    client.logout().await;
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn folders_are_created_renamed_and_deleted_on_every_server() {
    runtime().block_on(async {
        for server in SERVERS {
            run_on(server).await;
        }
    });
}
