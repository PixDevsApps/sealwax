use email_spike_s3::cjk;
use email_spike_s3::rng::Rng;
use email_spike_s3::sections::Thresholds;
use email_spike_s3::store::{
    FtsDoc, NewMessage, Reader, SearchFilter, SearchPlan, View, Writer, flags,
};
use email_spike_s3::synth::{self, Config};
use std::path::Path;

const NOW: i64 = 1_791_100_000;

fn message(mailbox: i64, uid: i64, id: &str, date: i64) -> NewMessage {
    NewMessage {
        mailbox_id: mailbox,
        uid,
        message_id: Some(id.to_owned()),
        date,
        from_name: format!("Sender {uid}"),
        from_addr: format!("sender{uid}@example.org"),
        subject: format!("Subject {id}"),
        ..NewMessage::default()
    }
}

fn thread_of(reader: &Reader, id: &str) -> i64 {
    reader
        .connection()
        .query_row(
            "SELECT thread_id FROM messages WHERE message_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

fn setup(path: &Path) -> (Writer, i64, i64) {
    let mut writer = Writer::open(path).unwrap();
    let account = writer.add_account("Test", "me@home.example").unwrap();
    let inbox = writer
        .add_mailbox(account, "INBOX", Some("inbox"), 1)
        .unwrap();
    let archive = writer
        .add_mailbox(account, "Archive", Some("archive"), 2)
        .unwrap();
    (writer, inbox, archive)
}

#[test]
fn reply_before_parent_and_branch_merges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.sqlite");
    let (mut writer, inbox, archive) = setup(&path);

    let mut reply = message(inbox, 1, "reply@x.example", 200);
    reply.references = vec!["root@x.example".to_owned()];
    reply.in_reply_to = Some("root@x.example".to_owned());
    writer.ingest(&[reply]).unwrap();
    writer
        .ingest(&[message(inbox, 2, "root@x.example", 100)])
        .unwrap();

    let mut stray = message(inbox, 3, "stray@x.example", 300);
    stray.in_reply_to = Some("lost@x.example".to_owned());
    writer.ingest(&[stray]).unwrap();

    let reader = Reader::open(&path).unwrap();
    assert_eq!(
        thread_of(&reader, "reply@x.example"),
        thread_of(&reader, "root@x.example")
    );
    assert_ne!(
        thread_of(&reader, "stray@x.example"),
        thread_of(&reader, "root@x.example")
    );

    let mut bridge = message(inbox, 4, "bridge@x.example", 400);
    bridge.references = vec!["root@x.example".to_owned(), "lost@x.example".to_owned()];
    let diff = writer.insert_one(&bridge, None, &[View::All]).unwrap();
    assert_eq!(diff.merged_threads.len(), 1);
    assert!(diff.thread_move.is_none());
    let thread = thread_of(&reader, "root@x.example");
    for id in ["reply@x.example", "stray@x.example", "bridge@x.example"] {
        assert_eq!(thread_of(&reader, id), thread);
    }

    writer
        .ingest(&[message(archive, 1, "root@x.example", 100)])
        .unwrap();
    assert_eq!(reader.thread_messages(thread).unwrap().len(), 5);
    let subject: String = reader
        .connection()
        .query_row(
            "SELECT subject FROM threads WHERE id = ?1",
            [thread],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(subject, "Subject root@x.example");
    assert_eq!(writer.check_aggregates().unwrap(), Vec::<String>::new());
}

#[test]
fn aggregates_survive_inserts_flag_changes_and_expunges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.sqlite");
    let mut writer = Writer::open(&path).unwrap();
    let ids = synth::create_mailboxes(&mut writer).unwrap();
    let mut corpus = synth::generate(Config {
        messages: 3_000,
        seed: 5,
        now: NOW,
        years: 2.0,
    });
    corpus.assign_mailboxes(&ids);
    for chunk in corpus.messages.chunks(250) {
        let headers: Vec<NewMessage> = chunk.iter().map(|m| m.header.clone()).collect();
        writer.ingest(&headers).unwrap();
    }
    assert_eq!(writer.check_aggregates().unwrap(), Vec::<String>::new());

    let inbox = ids[0];
    let mut rng = Rng::new(9);
    for step in 0..40 {
        let existing = &corpus.messages[rng.below(corpus.messages.len() as u64) as usize].header;
        let reply_to = (step % 2 == 0).then(|| {
            (
                existing.message_id.as_deref().unwrap(),
                existing.subject.as_str(),
            )
        });
        let date = NOW - rng.range(0, 400 * 86_400);
        let (mut header, _) = synth::incoming(1_000 + step, date, reply_to);
        header.mailbox_id = inbox;
        header.uid = 100_000 + step as i64;
        writer
            .insert_one(&header, None, &[View::All, View::Mailbox(inbox)])
            .unwrap();
    }
    let count: i64 = writer
        .connection()
        .query_row("SELECT max(id) FROM messages", [], |row| row.get(0))
        .unwrap();
    for _ in 0..60 {
        let id = rng.range(1, count + 1);
        let new_flags = rng.range(0, 8);
        let _ = writer.set_flags(id, new_flags);
    }
    for _ in 0..40 {
        let id = rng.range(1, count + 1);
        let _ = writer.expunge(id);
    }
    assert_eq!(writer.check_aggregates().unwrap(), Vec::<String>::new());
}

#[test]
fn insert_diff_positions_match_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.sqlite");
    let mut writer = Writer::open(&path).unwrap();
    let ids = synth::create_mailboxes(&mut writer).unwrap();
    let mut corpus = synth::generate(Config {
        messages: 4_000,
        seed: 21,
        now: NOW,
        years: 3.0,
    });
    corpus.assign_mailboxes(&ids);
    let headers: Vec<NewMessage> = corpus.messages.iter().map(|m| m.header.clone()).collect();
    writer.ingest(&headers).unwrap();
    let reader = Reader::open(&path).unwrap();
    let inbox = ids[0];
    let views = [View::All, View::Mailbox(inbox)];
    let mut rng = Rng::new(3);
    for step in 0..30 {
        let date = match step % 3 {
            0 => NOW + step,
            1 => NOW - rng.range(0, 3 * 365 * 86_400),
            _ => NOW - 5 * 365 * 86_400,
        };
        let existing = &corpus.messages[rng.below(corpus.messages.len() as u64) as usize].header;
        let reply_to = (step % 2 == 1).then(|| {
            (
                existing.message_id.as_deref().unwrap(),
                existing.subject.as_str(),
            )
        });
        let (mut header, _) = synth::incoming(50 + step as u64, date, reply_to);
        header.mailbox_id = inbox;
        header.uid = 200_000 + step;
        let diff = writer.insert_one(&header, None, &views).unwrap();
        assert_eq!(diff.positions.len(), 2);
        for (view, position) in &diff.positions {
            let page = reader.page(*view, *position, 1).unwrap();
            assert_eq!(page.version, diff.version);
            assert_eq!(page.rows[0].id, diff.row.id, "{view:?} at {position}");
        }
        if let Some(moved) = diff.thread_move {
            let rows = reader.conversations(inbox, moved.to, 1).unwrap();
            assert_eq!(rows[0].thread_id, diff.row.thread_id);
        }
    }
    let snapshot = reader
        .open_view(
            View::Mailbox(inbox),
            &Thresholds::new(NOW - 3600, NOW - 90_000, NOW - 400_000, NOW - 2_000_000),
            100,
        )
        .unwrap();
    assert_eq!(snapshot.rows.len(), 100);
    assert!(
        snapshot
            .rows
            .windows(2)
            .all(|pair| (pair[0].date, pair[0].id) > (pair[1].date, pair[1].id))
    );
    let deep = reader
        .page(View::Mailbox(inbox), snapshot.total - 50, 100)
        .unwrap();
    let forward = reader
        .page_forward(View::Mailbox(inbox), snapshot.total - 50, 100)
        .unwrap();
    assert_eq!(deep.rows, forward);
    assert_eq!(deep.rows.len(), 50);
}

#[test]
fn full_text_handles_norwegian_cjk_and_prefixes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.sqlite");
    let (mut writer, inbox, _) = setup(&path);
    let headers: Vec<NewMessage> = (1..=4)
        .map(|uid| message(inbox, uid, &format!("m{uid}@x.example"), 1000 + uid))
        .collect();
    let ids = writer.ingest(&headers).unwrap();
    let bodies = [
        "Vi tar med blåbærsyltetøy til hytta på fredag.",
        "关于下周的会议安排，请查看附件。",
        "プロジェクトの進捗について報告します。",
        "Prosjektplanen er oppdatert.",
    ];
    let docs: Vec<FtsDoc> = ids
        .iter()
        .zip(bodies)
        .map(|(id, body)| FtsDoc {
            id: *id,
            body: body.to_owned(),
            ..FtsDoc::default()
        })
        .collect();
    writer.index(&docs).unwrap();
    let reader = Reader::open(&path).unwrap();
    let find = |query: &str| -> Vec<i64> {
        let mut found = Vec::new();
        for plan in [SearchPlan::FtsFirst, SearchPlan::DateFirst] {
            let rows = reader
                .search(query, SearchFilter::default(), 50, plan)
                .unwrap();
            let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
            if plan == SearchPlan::FtsFirst {
                found = ids;
            } else {
                assert_eq!(found, ids, "{query}");
            }
        }
        found
    };
    assert_eq!(find("blåbærsyltetøy"), [ids[0]]);
    assert_eq!(find("blabaersyltetoy"), Vec::<i64>::new());
    assert_eq!(find(&cjk::query_term("会议")), [ids[1]]);
    assert_eq!(find(&cjk::query_term("進捗")), [ids[2]]);
    assert_eq!(find("prosjekt*"), [ids[3]]);
    let unread = reader
        .search(
            "prosjekt*",
            SearchFilter {
                mailbox: Some(inbox),
                unread_only: true,
            },
            50,
            SearchPlan::FtsFirst,
        )
        .unwrap();
    assert_eq!(unread.len(), 1);
    writer.set_flags(ids[3], flags::SEEN).unwrap();
    writer.expunge(ids[0]).unwrap();
    assert_eq!(find("blåbærsyltetøy"), Vec::<i64>::new());
}
