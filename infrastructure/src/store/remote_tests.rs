//! The senders whose remote images load without asking (ML-058, schema v8): kept in lower case
//! across restarts and account removals, added once, forgotten, with no change event, and a v7
//! store migrating to an empty list.

use super::tests::scratch;
use super::*;
use rusqlite::Connection;

fn senders(store: &Store) -> Vec<String> {
    store.remote_senders().recv_blocking().unwrap()
}

fn added(path: &Path, address: &str) -> Option<i64> {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT added FROM remote_senders WHERE address = ?1",
            [address],
            |row| row.get(0),
        )
        .ok()
}

#[test]
fn senders_are_kept_in_lower_case_and_added_once() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert!(senders(&store).is_empty());
    store
        .allow_remote_sender(" News@Northwind.Example.TEST ".into(), 1_790_000_000)
        .recv_blocking()
        .unwrap();
    store
        .allow_remote_sender("leah@studio-kim.example.test".into(), 1_790_000_100)
        .recv_blocking()
        .unwrap();
    store
        .allow_remote_sender("news@northwind.example.test".into(), 1_790_000_200)
        .recv_blocking()
        .unwrap();
    store
        .allow_remote_sender("   ".into(), 1_790_000_300)
        .recv_blocking()
        .unwrap();
    assert_eq!(
        senders(&store),
        [
            "leah@studio-kim.example.test",
            "news@northwind.example.test"
        ]
    );
    assert_eq!(
        added(&path, "news@northwind.example.test"),
        Some(1_790_000_000),
        "allowed again, it keeps the first time"
    );

    store
        .forget_remote_sender("NEWS@northwind.example.test".into())
        .recv_blocking()
        .unwrap();
    store
        .forget_remote_sender("nobody@example.test".into())
        .recv_blocking()
        .unwrap();
    assert_eq!(senders(&store), ["leah@studio-kim.example.test"]);
    store.close().unwrap();

    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(senders(&store), ["leah@studio-kim.example.test"]);
    store.close().unwrap();
}

#[test]
fn the_list_changes_no_version_and_sends_no_event() {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let events = store.subscribe();
    let before = store.accounts().recv_blocking().unwrap().version;
    store
        .allow_remote_sender("rides@fjordcycling.example.test".into(), 1)
        .recv_blocking()
        .unwrap();
    store
        .forget_remote_sender("rides@fjordcycling.example.test".into())
        .recv_blocking()
        .unwrap();
    store
        .allow_remote_sender("ase@example.test".into(), 2)
        .recv_blocking()
        .unwrap();
    assert_eq!(store.accounts().recv_blocking().unwrap().version, before);
    assert!(events.try_recv().is_err(), "no change event");
    store.close().unwrap();
}

#[test]
fn removing_an_account_keeps_the_list() {
    let dir = scratch();
    let store = Store::open(&dir.path().join("store.sqlite"), StoreOptions::default()).unwrap();
    let account = store
        .add_account(NewAccount {
            name: "Personal".into(),
            address: "alex.berg@example.test".into(),
        })
        .recv_blocking()
        .unwrap()
        .value;
    store
        .allow_remote_sender("noreply@akerlibrary.example.test".into(), 5)
        .recv_blocking()
        .unwrap();
    store.remove_account(account).recv_blocking().unwrap();
    assert_eq!(senders(&store), ["noreply@akerlibrary.example.test"]);
    store.close().unwrap();
}

#[test]
fn a_v7_store_migrates_to_an_empty_list() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    for (index, sql) in schema::MIGRATIONS[..7].iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            [index as i64 + 1],
        )
        .unwrap();
    }
    conn.execute_batch(
        "PRAGMA user_version = 7;
         INSERT INTO accounts(name, address) VALUES ('Fixture', 'fixture@example.test');",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    assert!(senders(&store).is_empty());
    assert_eq!(store.accounts().recv_blocking().unwrap().value.len(), 1);
    store
        .allow_remote_sender("fixture-sender@example.test".into(), 9)
        .recv_blocking()
        .unwrap();
    assert_eq!(senders(&store), ["fixture-sender@example.test"]);
    store.close().unwrap();
    let version: i64 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}
