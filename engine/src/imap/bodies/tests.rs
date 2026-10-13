//! Body downloads against scripted servers: whole messages in one round trip, a large message's
//! text parts and its attachment on demand, a changed UIDVALIDITY, a vanished UID, a connection
//! cut mid-FETCH, and never anything but BODY.PEEK.

use super::*;
use crate::imap::{Credentials, connect};
use crate::net::testing::{Peer, bind, test_ca};
use rustls::ServerConfig;
use sealwax_domain::account::{Endpoint, Security};
use sealwax_domain::model::{AccountId, FolderId, FolderRole};
use sealwax_infrastructure::store::{
    FirstSyncStep, FolderSpec, NewAccount, NewMessage, StoreOptions,
};
use std::path::Path;
use std::sync::Arc;

const UIDVALIDITY: u32 = 4711;

struct Scratch {
    _dir: tempfile::TempDir,
    store: Store,
    blobs: BlobStore,
    writes: tokio::sync::Mutex<()>,
    folder: FolderId,
}

impl Scratch {
    fn new() -> Scratch {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("bodies-")
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
        let account: AccountId = store
            .add_account(NewAccount {
                name: "Test".into(),
                address: "test@example.test".into(),
            })
            .recv_blocking()
            .unwrap()
            .value;
        let folder = store
            .upsert_folder(FolderSpec::local(account, "INBOX", FolderRole::Inbox))
            .recv_blocking()
            .unwrap()
            .value;
        store
            .first_sync_step(
                folder,
                FirstSyncStep::Start {
                    uidvalidity: UIDVALIDITY,
                    top: 100,
                    modseq: 0,
                },
            )
            .recv_blocking()
            .unwrap();
        Scratch {
            _dir: dir,
            store,
            blobs,
            writes: tokio::sync::Mutex::new(()),
            folder,
        }
    }

    fn sink(&self) -> Sink<'_> {
        Sink {
            store: &self.store,
            blobs: &self.blobs,
            writes: &self.writes,
        }
    }

    /// Header-only rows, as the first sync leaves them.
    fn headers(&self, rows: &[(u32, u64)]) -> Vec<BodyTarget> {
        let ids = self
            .store
            .ingest(
                rows.iter()
                    .map(|(uid, size)| NewMessage {
                        folder: self.folder,
                        uid: Some(*uid),
                        date: 1_790_000_000 + i64::from(*uid),
                        size: *size,
                        snippet: format!("Preview {uid}"),
                        search_text: format!("preview {uid}"),
                        body_state: BodyState::Partial,
                        ..NewMessage::default()
                    })
                    .collect(),
            )
            .recv_blocking()
            .unwrap()
            .ids
            .into_iter()
            .map(Option::unwrap)
            .collect();
        self.store.body_targets(ids).recv_blocking().unwrap()
    }

    fn folder_sync(&self) -> FolderSync {
        self.store
            .folder_sync(self.folder)
            .recv_blocking()
            .unwrap()
            .unwrap()
    }

    fn target(&self, id: MessageId) -> BodyTarget {
        self.store
            .body_targets(vec![id])
            .recv_blocking()
            .unwrap()
            .remove(0)
    }
}

fn small(uid: u32) -> String {
    format!(
        "From: Avsender <avsender@example.test>\r\nTo: test@example.test\r\n\
         Subject: Liten {uid}\r\nMessage-ID: <small-{uid}@example.test>\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nHei {uid}, dette er hele meldingen.\r\n"
    )
}

/// The scripted server's half of a login, after which the script takes over.
async fn logged_in(listener: &tokio::net::TcpListener, config: Arc<ServerConfig>) -> Peer {
    let mut peer = Peer::accept_tls(listener, config).await;
    peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n")
        .await;
    let tag = peer.expect("AUTHENTICATE PLAIN").await;
    peer.send("+ \r\n").await;
    peer.line().await;
    peer.send(&format!("{tag} OK authenticated\r\n")).await;
    let tag = peer.expect("CAPABILITY").await;
    peer.send("* CAPABILITY IMAP4rev1\r\n").await;
    peer.send(&format!("{tag} OK done\r\n")).await;
    peer
}

async fn examined(peer: &mut Peer, uidvalidity: u32) {
    let tag = peer.expect("EXAMINE \"INBOX\"").await;
    peer.send(&format!(
        "* 3 EXISTS\r\n* OK [UIDVALIDITY {uidvalidity}] ok\r\n* OK [UIDNEXT 100] ok\r\n\
         {tag} OK [READ-ONLY] examined\r\n"
    ))
    .await;
}

/// `NAME {length}\r\nDATA`, an IMAP literal.
fn literal(name: &str, data: &str) -> String {
    format!("{name} {{{}}}\r\n{data}", data.len())
}

async fn client(port: u16, trust: &crate::net::Trust) -> Connection {
    connect(
        &Endpoint {
            host: "127.0.0.1".into(),
            port,
            security: Security::Tls,
            certificate: None,
        },
        Credentials::Password {
            username: "user",
            password: "secret-password",
        },
        trust.client_config().unwrap(),
    )
    .await
    .unwrap()
}

/// Never a FETCH that sets `\Seen`, and no STORE at all.
fn assert_read_only(received: &str) {
    for line in received.lines() {
        let upper = line.to_ascii_uppercase();
        assert!(!upper.contains(" STORE "), "{line}");
        assert!(
            !upper.contains("BODY[") && !upper.contains("RFC822)") && !upper.contains("RFC822 "),
            "{line}"
        );
    }
}

type Answers = Vec<(MessageId, Result<(), BodyError>)>;

#[tokio::test]
async fn small_messages_come_whole_in_one_round_trip_with_peek_only() {
    let scratch = Scratch::new();
    let targets = scratch.headers(&[(5, 300), (7, 300)]);
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY).await;
        let tag = peer.expect("UID FETCH 5,7 (UID BODY.PEEK[])").await;
        peer.send(&format!(
            "* 1 FETCH (UID 5 {})\r\n* 3 FETCH (FLAGS (\\Seen) UID 9)\r\n* 2 FETCH (UID 7 {})\r\n\
             {tag} OK fetched\r\n",
            literal("BODY[]", &small(5)),
            literal("BODY[]", &small(7))
        ))
        .await;
        peer.drain().await
    });
    let mut conn = client(port, &ca.trust).await;
    let mut answers: Answers = Vec::new();
    fetch_bodies(
        &mut conn,
        scratch.sink(),
        &scratch.folder_sync(),
        &targets,
        1_790_000_500,
        |id, result| answers.push((id, result)),
    )
    .await
    .unwrap();
    drop(conn);
    assert_eq!(
        answers,
        [(targets[0].message, Ok(())), (targets[1].message, Ok(()))]
    );
    for (target, uid) in targets.iter().zip([5, 7]) {
        let stored = scratch.target(target.message);
        assert_eq!(stored.body_state, BodyState::Full);
        assert_eq!(
            scratch
                .store
                .raw_message(target.message)
                .recv()
                .await
                .unwrap(),
            Some(small(uid).into_bytes())
        );
    }
    let found = scratch
        .store
        .search("meldingen", None, 10)
        .recv()
        .await
        .unwrap();
    assert_eq!(found.value.len(), 2);
    assert_read_only(&server.await.unwrap());
}

/// multipart/mixed: an alternative (plain and HTML) and a 12 MB PDF.
const STRUCTURE: &str = "((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 22 1 NIL NIL \
    NIL NIL)(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 38 1 NIL NIL NIL NIL) \
    \"ALTERNATIVE\" (\"BOUNDARY\" \"alt\") NIL NIL NIL)(\"APPLICATION\" \"PDF\" (\"NAME\" \
    \"stor.pdf\") NIL NIL \"BASE64\" 12000000 NIL (\"ATTACHMENT\" (\"FILENAME\" \"stor.pdf\")) \
    NIL NIL) \"MIXED\" (\"BOUNDARY\" \"mix\") NIL NIL NIL";
const HEADER: &str = "From: Avsender <avsender@example.test>\r\nTo: test@example.test\r\n\
    Subject: Stor melding\r\nMessage-ID: <large@example.test>\r\nMIME-Version: 1.0\r\n\
    Content-Type: multipart/mixed; boundary=\"mix\"\r\n\r\n";
const PLAIN: &str = "Vedlagt er rapporten.";
const HTML: &str = "<p>Vedlagt er <b>rapporten</b>.</p>";

/// The server's replies to the large message's two FETCHes.
async fn serve_large(peer: &mut Peer) {
    let tag = peer.expect("UID FETCH 9 (UID BODYSTRUCTURE)").await;
    peer.send(&format!(
        "* 3 FETCH (UID 9 BODYSTRUCTURE ({STRUCTURE}))\r\n{tag} OK fetched\r\n"
    ))
    .await;
    let tag = peer
        .expect(
            "UID FETCH 9 (UID BODY.PEEK[HEADER] BODY.PEEK[1.MIME] BODY.PEEK[1.1.MIME] \
             BODY.PEEK[1.2.MIME] BODY.PEEK[2.MIME] BODY.PEEK[1.1] BODY.PEEK[1.2])",
        )
        .await;
    let sections = [
        literal("BODY[HEADER]", HEADER),
        literal(
            "BODY[1.MIME]",
            "Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n",
        ),
        literal(
            "BODY[1.1.MIME]",
            "Content-Type: text/plain; charset=utf-8\r\n\r\n",
        ),
        literal(
            "BODY[1.2.MIME]",
            "Content-Type: text/html; charset=utf-8\r\n\r\n",
        ),
        literal(
            "BODY[2.MIME]",
            "Content-Type: application/pdf; name=\"stor.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"stor.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n",
        ),
        literal("BODY[1.1]", PLAIN),
        literal("BODY[1.2]", HTML),
    ];
    peer.send(&format!(
        "* 3 FETCH (UID 9 {})\r\n{tag} OK fetched\r\n",
        sections.join(" ")
    ))
    .await;
}

#[tokio::test]
async fn a_large_message_comes_as_its_text_parts_and_its_attachment_on_demand() {
    let scratch = Scratch::new();
    let targets = scratch.headers(&[(9, 12_400_000)]);
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = logged_in(&listener, config.clone()).await;
        examined(&mut peer, UIDVALIDITY).await;
        serve_large(&mut peer).await;
        let first = peer.drain().await;
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY).await;
        let tag = peer
            .expect("UID FETCH 9 (UID BODY.PEEK[2.MIME] BODY.PEEK[2])")
            .await;
        peer.send(&format!(
            "* 3 FETCH (UID 9 {} {})\r\n{tag} OK fetched\r\n",
            literal(
                "BODY[2.MIME]",
                "Content-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\n\r\n"
            ),
            literal("BODY[2]", "JVBERi0xLjQKJSBzdG9y\r\n")
        ))
        .await;
        (first, peer.drain().await)
    });
    let mut conn = client(port, &ca.trust).await;
    let mut answers: Answers = Vec::new();
    fetch_bodies(
        &mut conn,
        scratch.sink(),
        &scratch.folder_sync(),
        &targets,
        1_790_000_500,
        |id, result| answers.push((id, result)),
    )
    .await
    .unwrap();
    drop(conn);
    let id = targets[0].message;
    assert_eq!(answers, [(id, Ok(()))]);
    assert_eq!(scratch.target(id).body_state, BodyState::Text);
    let detail = scratch
        .store
        .message(id)
        .recv()
        .await
        .unwrap()
        .value
        .unwrap();
    let raw = scratch.store.raw_message(id).recv().await.unwrap().unwrap();
    assert!(raw.len() < 2_000, "the attachment stayed on the server");
    let html = message::part(&raw, detail.html_parts[0]).unwrap();
    assert_eq!(html.text.as_deref(), Some(HTML));
    let plain = message::part(&raw, detail.text_parts[0]).unwrap();
    assert_eq!(plain.text.as_deref(), Some(PLAIN));
    assert_eq!(detail.attachments.len(), 1);
    let attachment = &detail.attachments[0];
    assert_eq!(attachment.name.as_deref(), Some("stor.pdf"));
    assert_eq!(attachment.mime_type, "application/pdf");
    assert_eq!(
        attachment.size, 8_769_230,
        "12,000,000 base64 octets in 78-byte lines, decoded"
    );
    assert!(detail.summary.has_attachments);
    let source = scratch
        .store
        .part_source(id, attachment.part)
        .recv()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source.section, "2");

    let mut conn = client(port, &ca.trust).await;
    let part = fetch_part(&mut conn, &scratch.folder_sync(), &source)
        .await
        .unwrap()
        .unwrap();
    drop(conn);
    assert_eq!(part.mime_type, "application/pdf");
    assert_eq!(part.data, b"%PDF-1.4\n% stor");
    let (first, second) = server.await.unwrap();
    assert_read_only(&first);
    assert_read_only(&second);
}

#[tokio::test]
async fn a_changed_uidvalidity_answers_gone_without_fetching() {
    let scratch = Scratch::new();
    let targets = scratch.headers(&[(5, 300), (9, 12_000_000)]);
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY + 1).await;
        peer.drain().await
    });
    let mut conn = client(port, &ca.trust).await;
    let mut answers: Answers = Vec::new();
    fetch_bodies(
        &mut conn,
        scratch.sink(),
        &scratch.folder_sync(),
        &targets,
        0,
        |id, result| answers.push((id, result)),
    )
    .await
    .unwrap();
    drop(conn);
    assert_eq!(
        answers,
        [
            (targets[0].message, Err(BodyError::Gone)),
            (targets[1].message, Err(BodyError::Gone))
        ]
    );
    assert!(!server.await.unwrap().contains("FETCH"));
    assert_eq!(scratch.target(targets[0].message).blob, None);
}

#[tokio::test]
async fn a_vanished_uid_is_gone_and_the_rest_is_stored() {
    let scratch = Scratch::new();
    let targets = scratch.headers(&[(5, 300), (6, 300), (9, 12_000_000)]);
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY).await;
        let tag = peer.expect("UID FETCH 5:6 (UID BODY.PEEK[])").await;
        peer.send(&format!(
            "* 1 FETCH (UID 6 {})\r\n{tag} OK fetched\r\n",
            literal("BODY[]", &small(6))
        ))
        .await;
        let tag = peer.expect("UID FETCH 9 (UID BODYSTRUCTURE)").await;
        peer.send(&format!("{tag} NO Some messages no longer exist\r\n"))
            .await;
        peer.drain().await
    });
    let mut conn = client(port, &ca.trust).await;
    let mut answers: Answers = Vec::new();
    fetch_bodies(
        &mut conn,
        scratch.sink(),
        &scratch.folder_sync(),
        &targets,
        0,
        |id, result| answers.push((id, result)),
    )
    .await
    .unwrap();
    drop(conn);
    assert_eq!(
        answers,
        [
            (targets[0].message, Err(BodyError::Gone)),
            (targets[1].message, Ok(())),
            (targets[2].message, Err(BodyError::Gone))
        ]
    );
    assert_eq!(scratch.target(targets[0].message).blob, None);
    assert_eq!(
        scratch.target(targets[1].message).body_state,
        BodyState::Full
    );
    assert_read_only(&server.await.unwrap());
}

#[tokio::test]
async fn a_connection_cut_mid_fetch_leaves_the_row_unchanged() {
    let scratch = Scratch::new();
    let targets = scratch.headers(&[(5, 300)]);
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY).await;
        peer.expect("UID FETCH 5 (UID BODY.PEEK[])").await;
        let message = small(5);
        peer.send(&format!(
            "* 1 FETCH (UID 5 BODY[] {{{}}}\r\n{}",
            message.len(),
            &message[..40]
        ))
        .await;
        drop(peer);
    });
    let mut conn = client(port, &ca.trust).await;
    let mut answers: Answers = Vec::new();
    let error = fetch_bodies(
        &mut conn,
        scratch.sink(),
        &scratch.folder_sync(),
        &targets,
        0,
        |id, result| answers.push((id, result)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.body_error(), BodyError::Offline, "{error}");
    assert!(answers.is_empty(), "the caller answers what is left");
    let stored = scratch.target(targets[0].message);
    assert_eq!((stored.body_state, stored.blob), (BodyState::Partial, None));
    let detail = scratch
        .store
        .message(targets[0].message)
        .recv()
        .await
        .unwrap()
        .value
        .unwrap();
    assert_eq!(detail.summary.snippet, "Preview 5");
    assert_eq!(scratch.blobs.disk_usage().unwrap().files, 0);
}

#[test]
fn a_structure_without_boundaries_or_all_text_comes_whole() {
    let text = Node::Leaf {
        text: true,
        octets: 20_000_000,
        base64: false,
    };
    assert_eq!(plan(&text), Plan::Whole);
    let unbounded = Node::Multipart {
        boundary: None,
        parts: vec![text.clone()],
    };
    assert_eq!(plan(&unbounded), Plan::Whole);
    let pdf = Node::Leaf {
        text: false,
        octets: 20_000_000,
        base64: true,
    };
    assert_eq!(
        plan(&pdf),
        Plan::Parts(vec!["BODY.PEEK[HEADER]".to_owned()])
    );
    let sections = HashMap::from([(
        "HEADER".to_owned(),
        b"Subject: x\r\nContent-Type: application/pdf; name=a.pdf\r\n\
          Content-Disposition: attachment\r\n"
            .to_vec(),
    )]);
    let raw = rebuild(&pdf, &sections);
    assert!(raw.ends_with(b"\r\n\r\n"));
    assert_eq!(part_sections(&raw), HashMap::from([(0, "TEXT".to_owned())]));
}

#[test]
fn sections_follow_the_mime_tree() {
    let raw = "Subject: x\r\nContent-Type: multipart/mixed; boundary=\"a\"\r\n\r\n\
               --a\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n\
               --b\r\nContent-Type: text/plain\r\n\r\nhei\r\n\
               --b\r\nContent-Type: text/html\r\n\r\n<p>hei</p>\r\n--b--\r\n\r\n\
               --a\r\nContent-Type: image/png\r\nContent-Disposition: attachment\r\n\r\n\r\n\
               --a\r\nContent-Type: message/rfc822\r\n\r\nSubject: inner\r\n\r\nindre\r\n\
               --a--\r\n";
    let sections = part_sections(raw.as_bytes());
    let mut found: Vec<_> = sections.values().cloned().collect();
    found.sort();
    assert_eq!(found, ["1", "1.1", "1.2", "2", "3"]);
    let parsed = message::parse(raw.as_bytes());
    let named: Vec<_> = parsed
        .attachments
        .iter()
        .map(|attachment| sections[&attachment.part].as_str())
        .collect();
    assert_eq!(named, ["2", "3"]);
    assert_eq!(sections[&parsed.html_parts[0]], "1.2");
}

#[tokio::test]
async fn an_original_comes_whole_with_peek_only_and_nothing_is_stored() {
    let scratch = Scratch::new();
    let targets = scratch.headers(&[(9, 12_400_000)]);
    let original =
        format!("{HEADER}--mix\r\nContent-Type: text/plain\r\n\r\n{PLAIN}\r\n--mix--\r\n");
    let sent = original.clone();
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY).await;
        let tag = peer.expect("UID FETCH 9 (UID BODY.PEEK[])").await;
        peer.send(&format!(
            "* 3 FETCH (UID 9 {})\r\n{tag} OK fetched\r\n",
            literal("BODY[]", &sent)
        ))
        .await;
        peer.drain().await
    });
    let mut conn = client(port, &ca.trust).await;
    let fetched = fetch_original(&mut conn, &scratch.folder_sync(), 9)
        .await
        .unwrap();
    drop(conn);
    assert_eq!(fetched, Ok(original.into_bytes()));
    let stored = scratch.target(targets[0].message);
    assert_eq!((stored.body_state, stored.blob), (BodyState::Partial, None));
    assert_eq!(scratch.blobs.disk_usage().unwrap().files, 0);
    assert_read_only(&server.await.unwrap());
}

#[tokio::test]
async fn an_original_or_a_part_gone_from_the_server_is_gone() {
    let scratch = Scratch::new();
    scratch.headers(&[(9, 12_400_000)]);
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    let server = tokio::spawn(async move {
        let mut peer = logged_in(&listener, config.clone()).await;
        examined(&mut peer, UIDVALIDITY).await;
        let tag = peer.expect("UID FETCH 9 (UID BODY.PEEK[])").await;
        peer.send(&format!("{tag} OK nothing\r\n")).await;
        examined(&mut peer, UIDVALIDITY).await;
        let tag = peer
            .expect("UID FETCH 9 (UID BODY.PEEK[2.MIME] BODY.PEEK[2])")
            .await;
        peer.send(&format!("{tag} NO no such message\r\n")).await;
        let first = peer.drain().await;
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY + 1).await;
        (first, peer.drain().await)
    });
    let folder = scratch.folder_sync();
    let source = PartSource {
        message: MessageId(1),
        folder: folder.folder,
        uid: 9,
        section: "2".into(),
        mime_type: "application/pdf".into(),
    };
    let mut conn = client(port, &ca.trust).await;
    assert_eq!(
        fetch_original(&mut conn, &folder, 9).await.unwrap(),
        Err(BodyError::Gone)
    );
    assert_eq!(
        fetch_part(&mut conn, &folder, &source).await.unwrap(),
        Err(BodyError::Gone)
    );
    drop(conn);
    let mut conn = client(port, &ca.trust).await;
    assert_eq!(
        fetch_original(&mut conn, &folder, 9).await.unwrap(),
        Err(BodyError::Gone),
        "a new UIDVALIDITY"
    );
    drop(conn);
    let (first, second) = server.await.unwrap();
    assert_read_only(&first);
    assert!(!second.contains("FETCH"), "{second}");
}

#[tokio::test]
async fn an_original_cut_mid_literal_is_a_lost_connection() {
    let scratch = Scratch::new();
    scratch.headers(&[(9, 12_400_000)]);
    let ca = test_ca();
    let config = ca.server.clone();
    let (listener, port) = bind().await;
    tokio::spawn(async move {
        let mut peer = logged_in(&listener, config).await;
        examined(&mut peer, UIDVALIDITY).await;
        peer.expect("UID FETCH 9 (UID BODY.PEEK[])").await;
        peer.send(&format!(
            "* 3 FETCH (UID 9 BODY[] {{5000}}\r\n{}",
            &HEADER[..40]
        ))
        .await;
        drop(peer);
    });
    let mut conn = client(port, &ca.trust).await;
    let error = fetch_original(&mut conn, &scratch.folder_sync(), 9)
        .await
        .unwrap_err();
    assert!(error.is_connection(), "{error}");
}
