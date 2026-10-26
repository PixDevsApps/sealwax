//! Parts and originals on the Docker testbed (M5, ML-061, ML-063): a synthetic message of about
//! 12 MB APPENDed into `spike`'s own folder on Dovecot (full and IMAP4rev1-only) and Stalwart,
//! synced as headers and downloaded as its text parts (`BodyState::Text`); then one attachment
//! by its section (`fetch_part`, as `Command::FetchPart` runs it) and the message whole
//! (`fetch_original`, as `Command::FetchOriginal` runs it), byte for byte what was APPENDed.
//! Nothing is marked `\Seen`, and the trace-level log holds no secret, name, address, folder
//! name, subject or content. Needs the testbed running and seeded, and is skipped otherwise:
//!
//! SEALWAX_TESTBED_CA=tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test parts_testbed -- --ignored --nocapture

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::*;
use sealwax_domain::account::Security;
use sealwax_domain::model::FolderId;
use sealwax_engine::imap::{self, bodies};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{BodyState, NewAccount, Store, StoreOptions, ViewSpec};
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const USER: &str = "spike";
const FOLDER: &str = "Parts-ML061";

/// Deterministic filler bytes (SplitMix64).
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn base64_lines(data: &[u8]) -> String {
    STANDARD
        .encode(data)
        .as_bytes()
        .chunks(76)
        .map(|line| format!("{}\r\n", std::str::from_utf8(line).unwrap()))
        .collect()
}

fn attachment(name: &str, data: &[u8]) -> String {
    format!(
        "--p\r\nContent-Type: application/octet-stream; name=\"{name}\"\r\n\
         Content-Disposition: attachment; filename=\"{name}\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}",
        base64_lines(data)
    )
}

/// About 12 MB: an alternative (plain and HTML) and two attachments.
fn fixture(first: &[u8], second: &[u8]) -> Vec<u8> {
    format!(
        "From: Testbed <testbed@example.test>\r\nTo: spike@example.test\r\n\
         Subject: Parts fixture\r\nDate: Tue, 06 Oct 2026 12:00:00 +0000\r\n\
         Message-ID: <parts-fixture@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"p\"\r\n\r\n\
         --p\r\nContent-Type: multipart/alternative; boundary=\"a\"\r\n\r\n\
         --a\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe parts fixture, \
         partsfixtureword.\r\n--a\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
         <p>The parts fixture, <b>partsfixtureword</b>.</p>\r\n--a--\r\n\r\n{}{}--p--\r\n",
        attachment("first-part.bin", first),
        attachment("second-part.bin", second)
    )
    .into_bytes()
}

/// A plain IMAP client for the fixture: login, empty the folder, APPEND the message.
async fn append(port: u16, message: &[u8]) {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let config = trust().client_config().unwrap();
    let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, tcp)
        .await
        .unwrap();
    let mut io = BufReader::new(tls);
    let mut line = String::new();
    io.read_line(&mut line).await.unwrap();
    let mut tag = 0;
    async fn command<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        io: &mut BufReader<S>,
        tag: &mut u32,
        text: &str,
        literal: Option<&[u8]>,
    ) -> String {
        *tag += 1;
        let id = format!("p{tag}");
        let stream = io.get_mut();
        stream
            .write_all(format!("{id} {text}\r\n").as_bytes())
            .await
            .unwrap();
        stream.flush().await.unwrap();
        loop {
            let mut line = String::new();
            assert!(io.read_line(&mut line).await.unwrap() > 0, "closed");
            if line.starts_with('+')
                && let Some(literal) = literal
            {
                let stream = io.get_mut();
                stream.write_all(literal).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
                stream.flush().await.unwrap();
            } else if let Some(status) = line.strip_prefix(&format!("{id} ")) {
                return status.trim().to_owned();
            }
        }
    }
    let status = command(&mut io, &mut tag, &format!("LOGIN {USER} {PASSWORD}"), None).await;
    assert!(status.starts_with("OK"), "{status}");
    command(&mut io, &mut tag, &format!("CREATE {FOLDER}"), None).await;
    let status = command(&mut io, &mut tag, &format!("SELECT {FOLDER}"), None).await;
    assert!(status.starts_with("OK"), "{status}");
    command(
        &mut io,
        &mut tag,
        "UID STORE 1:* +FLAGS.SILENT (\\Deleted)",
        None,
    )
    .await;
    command(&mut io, &mut tag, "EXPUNGE", None).await;
    let status = command(
        &mut io,
        &mut tag,
        &format!("APPEND {FOLDER} () {{{}}}", message.len()),
        Some(message),
    )
    .await;
    assert!(status.starts_with("OK"), "APPEND: {status}");
    command(&mut io, &mut tag, "LOGOUT", None).await;
}

fn scratch() -> (tempfile::TempDir, Store, BlobStore) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("parts-testbed-")
        .tempdir_in(base)
        .unwrap();
    let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
    let store = Store::open(
        &dir.path().join("store.sqlite"),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .unwrap();
    (dir, store, blobs)
}

/// The folder list and the fixture folder's headers into `store`, as the first sync leaves them.
async fn headers(store: &Store, server: Server) -> FolderId {
    let account = store
        .add_account(NewAccount {
            name: "Spike".into(),
            address: "spike@example.test".into(),
        })
        .recv()
        .await
        .unwrap()
        .value;
    let mut conn = login(server, Security::Tls, USER).await;
    imap::sync_folders(&mut conn, store, account).await.unwrap();
    let folder = store
        .folders()
        .recv()
        .await
        .unwrap()
        .value
        .into_iter()
        .find(|folder| folder.name == FOLDER)
        .unwrap()
        .id;
    imap::first_sync(&mut conn, store, folder, |_| {})
        .await
        .unwrap();
    conn.logout().await;
    folder
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[derive(Clone, Default)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "needs the testbed: tools/testbed/up.sh && tools/testbed/seed.sh"]
fn parts_and_originals_on_every_server() {
    use tracing_subscriber::util::SubscriberInitExt;
    let capture = Capture::default();
    let sink = capture.clone();
    sealwax_infrastructure::logging::subscriber(
        tracing_subscriber::EnvFilter::new("trace"),
        move || sink.clone(),
        false,
    )
    .try_init()
    .unwrap();
    let runtime = sealwax_engine::runtime().unwrap();
    let first = noise(11, 4_400_000);
    let second = noise(12, 4_400_000);
    let message = fixture(&first, &second);
    println!("fixture: {} B", message.len());
    runtime.block_on(async {
        for server in SERVERS {
            append(server.tls, &message).await;
            let (_dir, store, blobs) = scratch();
            let folder = headers(&store, server).await;
            let sync = store.folder_sync(folder).recv().await.unwrap().unwrap();
            let rows = store
                .page(ViewSpec::folder(folder), 0, 10)
                .recv()
                .await
                .unwrap()
                .rows;
            assert_eq!(rows.len(), 1, "{}", server.name);
            let id = rows[0].id;
            let targets = store.body_targets(vec![id]).recv().await.unwrap();
            let writes = tokio::sync::Mutex::new(());
            let sink = bodies::Sink {
                store: &store,
                blobs: &blobs,
                writes: &writes,
            };
            let mut conn = login(server, Security::Tls, USER).await;
            let mut answers = Vec::new();
            bodies::fetch_bodies(
                &mut conn,
                sink,
                &sync,
                &targets,
                &HashSet::new(),
                0,
                |id, result| {
                    answers.push((id, result));
                },
            )
            .await
            .unwrap();
            assert_eq!(answers, [(id, Ok(()))], "{}", server.name);
            let stored = store.body_targets(vec![id]).recv().await.unwrap();
            assert_eq!(stored[0].body_state, BodyState::Text, "{}", server.name);

            let detail = store.message(id).recv().await.unwrap().value.unwrap();
            let wanted = detail
                .attachments
                .iter()
                .find(|attachment| attachment.name.as_deref() == Some("second-part.bin"))
                .unwrap();
            let source = store
                .part_source(id, wanted.part)
                .recv()
                .await
                .unwrap()
                .unwrap();
            let started = Instant::now();
            let part = bodies::fetch_part(&mut conn, &sync, &source)
                .await
                .unwrap()
                .unwrap();
            let part_time = started.elapsed();
            assert_eq!(part.data, second, "{}", server.name);

            let started = Instant::now();
            let original = bodies::fetch_original(&mut conn, &sync, stored[0].uid)
                .await
                .unwrap()
                .unwrap();
            let original_time = started.elapsed();
            assert_eq!(original, message, "{}: the server's bytes", server.name);
            let gone = bodies::fetch_original(&mut conn, &sync, stored[0].uid + 1000)
                .await
                .unwrap();
            assert_eq!(
                gone,
                Err(sealwax_engine::api::BodyError::Gone),
                "{}",
                server.name
            );
            conn.logout().await;
            let kept = store.raw_message(id).recv().await.unwrap().unwrap();
            assert!(kept.len() < 8_192, "{}: nothing stored", server.name);

            let (_again_dir, again, _again_blobs) = scratch();
            let folder_again = headers(&again, server).await;
            let rows = again
                .page(ViewSpec::folder(folder_again), 0, 10)
                .recv()
                .await
                .unwrap()
                .rows;
            assert!(
                rows.iter().all(|row| !row.flags.is_seen()),
                "{}: downloading set \\Seen",
                server.name
            );
            println!(
                "{}: section {} ({} B) in {:.1} ms; the original ({} B) in {:.1} ms",
                server.name,
                source.section,
                part.data.len(),
                ms(part_time),
                original.len(),
                ms(original_time)
            );
            store.close().unwrap();
            again.close().unwrap();
        }
    });
    let log = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    for needle in [
        PASSWORD,
        USER,
        "example.test",
        FOLDER,
        "Parts fixture",
        "partsfixtureword",
        "first-part.bin",
        "second-part.bin",
        "AUTHENTICATE",
        "LOGIN",
        "async_imap",
        "imap_proto",
    ] {
        assert!(!log.contains(needle), "{needle} appears in the log");
    }
    println!(
        "log grep: {} lines at trace level, none with a secret, an address, a folder name, a \
         subject or body text",
        log.lines().count()
    );
}
