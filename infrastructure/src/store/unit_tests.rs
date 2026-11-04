//! The store's own threads and schema checks, which need its internals; the suites over its
//! public API are `tests/store/`.

use super::*;
use rusqlite::Connection;
use std::io::Write;

/// A scratch directory under the workspace's `target/`, on the real disk (`/tmp` is tmpfs here,
/// where fsync costs nothing).
pub(crate) fn scratch() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("store-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn a_store_thread_that_panics_says_so_in_the_log() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let sink = capture.clone();
    let quiet = Capture::default();
    let quiet_sink = quiet.clone();
    let subscriber = crate::logging::subscriber(
        tracing_subscriber::EnvFilter::new("info"),
        move || sink.clone(),
        false,
    );
    let calm = crate::logging::subscriber(
        tracing_subscriber::EnvFilter::new("info"),
        move || quiet_sink.clone(),
        false,
    );
    let panicked = std::thread::spawn(move || {
        tracing::subscriber::with_default(subscriber, || {
            let _watch = PanicWatch("writer");
            panic!("a job's bug");
        })
    })
    .join();
    assert!(panicked.is_err());
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("a store thread stopped after a panic"),
        "{log}"
    );
    assert!(log.contains("thread=\"writer\""), "{log}");
    // A thread that ends as it should logs nothing.
    std::thread::spawn(move || {
        tracing::subscriber::with_default(calm, || drop(PanicWatch("writer")));
    })
    .join()
    .unwrap();
    assert!(quiet.0.lock().unwrap().is_empty());
}

#[test]
fn a_reader_on_an_unmigrated_store_says_it_is_older_not_newer() {
    let dir = scratch();
    let path = dir.path().join("store.sqlite");
    drop(Store::open(&path, StoreOptions::default()).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version={}", SCHEMA_VERSION - 1))
        .unwrap();
    drop(conn);
    let error = schema::open_reader(&path).err().unwrap();
    assert!(
        matches!(
            error,
            StoreError::OlderSchema {
                found,
                expected: SCHEMA_VERSION
            } if found == SCHEMA_VERSION - 1
        ),
        "{error}"
    );
}
