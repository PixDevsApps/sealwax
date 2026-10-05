//! Body downloads on the Docker testbed (ML-044): three synthetic messages APPENDed into
//! `spike`'s own folder on Dovecot (full and IMAP4rev1-only) and Stalwart, of about 1 KB, 5 MB
//! with an attachment and 12 MB with two, then synced as headers and downloaded: the first two
//! whole, the third as its text parts, one of its attachments then on demand. Nothing is marked
//! `\Seen`. Needs the testbed running and seeded, and is skipped otherwise:
//!
//! SEALWAX_TESTBED_CA=tools/testbed/certs/ca.pem \
//!   cargo test -p sealwax-engine --release --test bodies_testbed -- --ignored --nocapture

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::*;
use sealwax_domain::account::Security;
use sealwax_domain::model::{FolderId, MessageId};
use sealwax_engine::api::BodyError;
use sealwax_engine::imap::{self, bodies};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{BodyState, NewAccount, Store, StoreOptions};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const USER: &str = "spike";
const FOLDER: &str = "Bodies-ML044";

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

fn header(n: u32, content_type: &str) -> String {
    format!(
        "From: Testbed <testbed@example.test>\r\nTo: spike@example.test\r\n\
         Subject: Body fixture {n}\r\nDate: Mon, 05 Oct 2026 12:0{n}:00 +0000\r\n\
         Message-ID: <body-fixture-{n}@example.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: {content_type}\r\n\r\n"
    )
}

fn attachment(boundary: &str, name: &str, data: &[u8]) -> String {
    format!(
        "--{boundary}\r\nContent-Type: application/octet-stream; name=\"{name}\"\r\n\
         Content-Disposition: attachment; filename=\"{name}\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}",
        base64_lines(data)
    )
}

/// About 1 KB of plain text.
fn small() -> Vec<u8> {
    let text = "Synthetic body text for the small fixture. ".repeat(20);
    format!(
        "{}{text}\r\nsmallfixtureword\r\n",
        header(1, "text/plain; charset=utf-8")
    )
    .into_bytes()
}

/// About 5 MB: plain and HTML text and one attachment.
fn medium(data: &[u8]) -> Vec<u8> {
    format!(
        "{}--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe medium fixture.\r\n\
         --m\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>The medium fixture.</p>\r\n\
         {}--m--\r\n",
        header(2, "multipart/mixed; boundary=\"m\""),
        attachment("m", "medium.bin", data)
    )
    .into_bytes()
}

/// About 12 MB: an alternative (plain and HTML) and two attachments.
fn large(first: &[u8], second: &[u8]) -> Vec<u8> {
    format!(
        "{}--l\r\nContent-Type: multipart/alternative; boundary=\"a\"\r\n\r\n\
         --a\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe large fixture, \
         largefixtureword.\r\n--a\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
         <p>The large fixture, <b>largefixtureword</b>.</p>\r\n--a--\r\n\r\n{}{}--l--\r\n",
        header(3, "multipart/mixed; boundary=\"l\""),
        attachment("l", "first.bin", first),
        attachment("l", "second.bin", second)
    )
    .into_bytes()
}

/// A plain IMAP client for the fixtures: login, empty the folder, APPEND each message.
async fn append(port: u16, messages: &[Vec<u8>]) {
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
        let id = format!("f{tag}");
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
    for message in messages {
        let status = command(
            &mut io,
            &mut tag,
            &format!("APPEND {FOLDER} () {{{}}}", message.len()),
            Some(message),
        )
        .await;
        assert!(status.starts_with("OK"), "APPEND: {status}");
    }
    command(&mut io, &mut tag, "LOGOUT", None).await;
}

fn scratch() -> (tempfile::TempDir, Store, BlobStore) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("bodies-testbed-")
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
fn bodies_on_every_server() {
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
    let medium_data = noise(2, 3_700_000);
    let first = noise(3, 4_400_000);
    let second = noise(4, 4_400_000);
    let fixtures = [small(), medium(&medium_data), large(&first, &second)];
    println!(
        "fixtures: {} B, {} B, {} B",
        fixtures[0].len(),
        fixtures[1].len(),
        fixtures[2].len()
    );
    runtime.block_on(async {
        for server in SERVERS {
            append(server.tls, &fixtures).await;
            let (_dir, store, blobs) = scratch();
            let folder = headers(&store, server).await;
            let sync = store.folder_sync(folder).recv().await.unwrap().unwrap();
            let summaries = store
                .page(
                    sealwax_infrastructure::store::ViewSpec::folder(folder),
                    0,
                    10,
                )
                .recv()
                .await
                .unwrap()
                .rows;
            assert_eq!(summaries.len(), 3, "{}", server.name);
            assert!(summaries.iter().all(|row| !row.flags.is_seen()));
            let mut targets = store
                .body_targets(summaries.iter().map(|row| row.id).collect())
                .recv()
                .await
                .unwrap();
            targets.sort_by_key(|target| target.uid);
            let ids: Vec<MessageId> = targets.iter().map(|target| target.message).collect();
            let sizes: Vec<u64> = targets.iter().map(|target| target.size).collect();

            let writes = tokio::sync::Mutex::new(());
            let sink = bodies::Sink {
                store: &store,
                blobs: &blobs,
                writes: &writes,
            };
            let started = Instant::now();
            let mut conn = login(server, Security::Tls, USER).await;
            let connected = started.elapsed();
            let mut answers: Vec<(MessageId, Result<(), BodyError>, Duration)> = Vec::new();
            bodies::fetch_bodies(&mut conn, sink, &sync, &targets, 0, |id, result| {
                answers.push((id, result, started.elapsed()));
            })
            .await
            .unwrap();
            let fetched = started.elapsed();
            assert!(
                answers.iter().all(|(_, result, _)| result.is_ok()),
                "{}: {answers:?}",
                server.name
            );

            let states: Vec<BodyState> = store
                .body_targets(ids.clone())
                .recv()
                .await
                .unwrap()
                .iter()
                .map(|target| target.body_state)
                .collect();
            assert_eq!(
                states,
                [BodyState::Full, BodyState::Full, BodyState::Text],
                "{}",
                server.name
            );
            let raw: Vec<Vec<u8>> =
                futures_util::future::join_all(ids.iter().map(|id| store.raw_message(*id).recv()))
                    .await
                    .into_iter()
                    .map(|raw| raw.unwrap().unwrap())
                    .collect();
            assert_eq!(raw[0], fixtures[0], "{}", server.name);
            assert_eq!(raw[1], fixtures[1], "{}", server.name);
            assert!(raw[2].len() < 8_192, "{}: {} B", server.name, raw[2].len());
            for word in ["smallfixtureword", "largefixtureword"] {
                let found = store.search(word, None, 10).recv().await.unwrap();
                assert_eq!(found.value.len(), 1, "{} {word}", server.name);
            }
            let detail = store.message(ids[2]).recv().await.unwrap().value.unwrap();
            assert_eq!(detail.attachments.len(), 2, "{}", server.name);
            let wanted = detail
                .attachments
                .iter()
                .find(|attachment| attachment.name.as_deref() == Some("second.bin"))
                .unwrap();
            let source = store
                .part_source(ids[2], wanted.part)
                .recv()
                .await
                .unwrap()
                .unwrap();
            let part_started = Instant::now();
            let part = bodies::fetch_part(&mut conn, &sync, &source)
                .await
                .unwrap()
                .unwrap();
            let part_time = part_started.elapsed();
            assert_eq!(part.data, second, "{}", server.name);
            conn.logout().await;

            let (_again_dir, again, _again_blobs) = scratch();
            let folder_again = headers(&again, server).await;
            let rows = again
                .page(
                    sealwax_infrastructure::store::ViewSpec::folder(folder_again),
                    0,
                    10,
                )
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
                "{}: RFC822.SIZE {sizes:?}; blobs {} B, {} B, {} B; connect {:.1} ms; \
                 answered at {:.1}, {:.1}, {:.1} ms; all three {:.1} ms; attachment \
                 {} B on demand in {:.1} ms (section {}, estimated {} B)",
                server.name,
                raw[0].len(),
                raw[1].len(),
                raw[2].len(),
                ms(connected),
                ms(answers[0].2),
                ms(answers[1].2),
                ms(answers[2].2),
                ms(fetched),
                part.data.len(),
                ms(part_time),
                source.section,
                wanted.size
            );
            store.close().unwrap();
            again.close().unwrap();
        }
    });
    let log = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(
        log.contains("bodies fetched"),
        "the capture holds the downloads"
    );
    for needle in [
        PASSWORD,
        USER,
        "example.test",
        FOLDER,
        "Body fixture",
        "smallfixtureword",
        "largefixtureword",
        "medium.bin",
        "second.bin",
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
