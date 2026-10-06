//! Headless measurements of search on a fixture store (M7, ML-081, ML-082): the first 50
//! results of the query language's shapes, cold and warm, in every folder and in the INBOX;
//! the index's size; the background rebuild (or schema v10's one-time pass, when the store
//! comes from an earlier build), with how long flag changes and searches wait meanwhile; and,
//! with `--date-keyed yes`, S3's date-keyed full-text table built beside the index for
//! comparison (run that on a copy: it adds a table).

use crate::bench::{evict, open};
use crate::import::{ACCOUNT_NAME, account_address};
use crate::stats::{ms, summary};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, params};
use sealwax_domain::model::{Flags, FolderId, FolderRole, MessageId};
use sealwax_domain::{message, search};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::fts::{self, Expression};
use sealwax_infrastructure::store::{Store, ViewSpec};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Options {
    pub bench: crate::bench::Options,
    pub date_keyed: bool,
    pub rebuild: bool,
}

/// The shapes measured: a name, the query, and whether it runs in the INBOX only.
const SHAPES: [(&str, &str, bool); 32] = [
    ("common Norwegian word", "møte", false),
    ("very common English word", "the", false),
    ("rare word with æøå", "blåbærsyltetøy", false),
    ("the same typed without æøå", "blabaersyltetoy", false),
    ("no match", "zzqxv", false),
    ("two words", "budsjett kontrakt", false),
    ("phrase", "\"project update\"", false),
    ("prefix", "prosjekt*", false),
    ("two-letter prefix", "pr*", false),
    ("from:", "from:hansen", false),
    ("to:, a few", "to:hansen", false),
    ("to:, nearly all", "to:alex", false),
    ("subject:", "subject:faktura", false),
    ("has:attachment and a word", "has:attachment faktura", false),
    ("is:unread alone", "is:unread", false),
    ("is:unread and a word", "is:unread møte", false),
    ("is:starred alone", "is:starred", false),
    ("has:attachment alone", "has:attachment", false),
    ("a range alone", "after:2024-01-01 before:2024-07-01", false),
    (
        "a range and a word",
        "møte after:2024-01-01 before:2024-07-01",
        false,
    ),
    (
        "an old range and a word",
        "faktura before:2022-01-01",
        false,
    ),
    (
        "combination",
        "from:hansen is:unread has:attachment after:2023-01-01 faktura*",
        false,
    ),
    ("an excluded word", "faktura -purring", false),
    ("Chinese word (bigrams)", "会议", false),
    ("common Norwegian word", "møte", true),
    ("very common English word", "the", true),
    ("no match", "zzqxv", true),
    ("is:unread alone", "is:unread", true),
    ("is:starred alone", "is:starred", true),
    ("has:attachment and a word", "has:attachment faktura", true),
    (
        "a range and a word",
        "møte after:2024-01-01 before:2024-07-01",
        true,
    ),
    ("prefix", "prosjekt*", true),
];

/// Shapes timed again after a rebuild, which leaves the index in more pieces until merged.
const AFTER: [&str; 5] = ["møte", "the", "budsjett kontrakt", "prosjekt*", "pr*"];

fn inbox(store: &Store) -> Result<FolderId> {
    let accounts = store.accounts().recv_blocking()?.value;
    let account = accounts
        .iter()
        .find(|account| account.address == account_address(ACCOUNT_NAME))
        .context("no fixture account; run `import` first")?;
    Ok(store
        .folders()
        .recv_blocking()?
        .value
        .iter()
        .find(|folder| folder.account == account.id && folder.role == FolderRole::Inbox)
        .context("no fixture INBOX")?
        .id)
}

fn read_only(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

/// MiB of the full-text index (its tables) and of the whole database, from `dbstat`.
fn sizes(path: &Path) -> Result<(f64, f64)> {
    let conn = read_only(path)?;
    let index: i64 = conn.query_row(
        "SELECT coalesce(sum(pgsize), 0) FROM dbstat WHERE name LIKE 'message_fts%'",
        [],
        |row| row.get(0),
    )?;
    let all: i64 = conn.query_row("SELECT coalesce(sum(pgsize), 0) FROM dbstat", [], |row| {
        row.get(0)
    })?;
    Ok((index as f64 / 1_048_576.0, all as f64 / 1_048_576.0))
}

/// Flag changes and searches every `every`, timed, until `stop`.
fn probe(
    store: &Store,
    ids: &[MessageId],
    stop: &AtomicBool,
) -> Result<(Vec<Duration>, Vec<Duration>)> {
    let mut flags = Vec::new();
    let mut searches = Vec::new();
    let mut n = 0usize;
    while !stop.load(Ordering::SeqCst) {
        let id = ids[n % ids.len()];
        let (add, remove) = if n.is_multiple_of(2) {
            (Flags::FLAGGED, Flags::NONE)
        } else {
            (Flags::NONE, Flags::FLAGGED)
        };
        let started = Instant::now();
        store.set_flags(vec![id], add, remove).recv_blocking()?;
        flags.push(started.elapsed());
        if n % 2 == 1 {
            let started = Instant::now();
            store.search("møte", None, 50).recv_blocking()?;
            searches.push(started.elapsed());
        }
        n += 1;
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok((flags, searches))
}

/// Runs `work` while `probe` times flag changes and searches beside it.
fn probed<T>(
    store: &Store,
    ids: &[MessageId],
    work: impl FnOnce() -> Result<T>,
) -> Result<(T, Vec<Duration>, Vec<Duration>)> {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let prober = scope.spawn(|| probe(store, ids, &stop));
        let result = work();
        stop.store(true, Ordering::SeqCst);
        let (flags, searches) = prober.join().expect("the prober panicked")?;
        Ok((result?, flags, searches))
    })
}

/// Some message ids to flag, spread over the store.
fn sample(store: &Store, inbox: FolderId) -> Result<Vec<MessageId>> {
    let page = store
        .page(ViewSpec::folder(inbox), 0, 1_000)
        .recv_blocking()?;
    Ok(page.rows.iter().step_by(10).map(|row| row.id).collect())
}

fn wait_for_index(store: &Store) -> Duration {
    let started = Instant::now();
    while store.index_progress().is_some() {
        std::thread::sleep(Duration::from_millis(10));
    }
    started.elapsed()
}

pub fn run(options: &Options) -> Result<()> {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = sealwax_infrastructure::logging::init();
    }
    let bench = &options.bench;
    let blobs = BlobStore::open(&bench.blobs)?;
    let version: i64 =
        read_only(&bench.store)?.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let (index_mib, all_mib) = sizes(&bench.store)?;
    println!("## Store");
    println!(
        "- SQLite {}; schema v{version} before opening",
        rusqlite::version()
    );
    println!("- full-text index {index_mib:.1} MiB of {all_mib:.1} MiB (dbstat)");

    let opened = Instant::now();
    let mut store = open(bench, &blobs)?;
    println!("- opened (and migrated) in {:.0} ms", ms(opened.elapsed()));
    let inbox = inbox(&store)?;
    let ids = sample(&store, inbox)?;
    // An earlier build's store gets schema v10's pass: every message indexed again once.
    store.touch_body(MessageId(0), 0).recv_blocking()?;
    if let Some(progress) = store.index_progress() {
        println!("\n## Schema v10's pass ({} messages)", progress.total);
        let (waited, flags, searches) = probed(&store, &ids, || Ok(wait_for_index(&store)))?;
        println!("- done in {:.1} s", waited.as_secs_f64());
        println!("- a flag change meanwhile: {}", summary(&flags));
        println!(
            "- a search (`møte`, first 50) meanwhile: {}",
            summary(&searches)
        );
        let (index_mib, all_mib) = sizes(&bench.store)?;
        println!("- full-text index {index_mib:.1} MiB of {all_mib:.1} MiB after it");
    }
    let messages: u32 = store
        .folders()
        .recv_blocking()?
        .value
        .iter()
        .map(|folder| folder.total)
        .sum();
    println!("- {messages} messages");

    println!(
        "\n## Search: the first 50 results with their rows ({} cold, {} warm)",
        bench.cold, bench.warm
    );
    println!(
        "| shape | query | where | rows | cold p50 / p95 / max ms | warm p50 / p95 / max ms |"
    );
    println!("| --- | --- | --- | --- | --- | --- |");
    let mut worst_cold = 0.0f64;
    let mut worst_warm = 0.0f64;
    for (name, query, inbox_only) in SHAPES {
        let folder = inbox_only.then_some(inbox);
        let mut cold = Vec::new();
        let mut rows = 0;
        for _ in 0..bench.cold {
            store.close()?;
            evict(&bench.store)?;
            store = open(bench, &blobs)?;
            let started = Instant::now();
            rows = store.search(query, folder, 50).recv_blocking()?.value.len();
            cold.push(started.elapsed());
        }
        let mut warm = Vec::new();
        for _ in 0..bench.warm {
            let started = Instant::now();
            rows = store.search(query, folder, 50).recv_blocking()?.value.len();
            warm.push(started.elapsed());
        }
        let (cold, warm) = (summary(&cold), summary(&warm));
        if bench.cold > 0 {
            worst_cold = worst_cold.max(cold.p95);
        }
        worst_warm = worst_warm.max(warm.p95);
        println!(
            "| {name} | `{query}` | {} | {rows} | {:.2} / {:.2} / {:.2} | {:.2} / {:.2} / {:.2} |",
            if inbox_only { "INBOX" } else { "all" },
            cold.p50,
            cold.p95,
            cold.max,
            warm.p50,
            warm.p95,
            warm.max
        );
    }
    println!("- worst p95: cold {worst_cold:.2} ms, warm {worst_warm:.2} ms");

    let ((), flags, searches) = probed(&store, &ids, || {
        std::thread::sleep(Duration::from_secs(3));
        Ok(())
    })?;
    println!("\n## Flag changes and searches with no rebuild (3 s)");
    println!("- a flag change: {}", summary(&flags));
    println!("- a search (`møte`, first 50): {}", summary(&searches));

    if options.rebuild {
        println!("\n## Rebuild Search Index");
        let (rebuilt, flags, searches) = probed(&store, &ids, || {
            Ok(store.rebuild_index().recv_blocking()?)
        })?;
        println!(
            "- {} messages indexed again, {} kept, {} entries removed, in {:.1} s ({:.0} messages/s)",
            rebuilt.messages,
            rebuilt.kept,
            rebuilt.removed,
            rebuilt.elapsed.as_secs_f64(),
            rebuilt.messages as f64 / rebuilt.elapsed.as_secs_f64()
        );
        println!("- a flag change meanwhile: {}", summary(&flags));
        println!(
            "- a search (`møte`, first 50) meanwhile: {}",
            summary(&searches)
        );
        store.close()?;
        let (index_mib, all_mib) = sizes(&bench.store)?;
        println!("- full-text index {index_mib:.1} MiB of {all_mib:.1} MiB after it");
        store = open(bench, &blobs)?;
        for query in AFTER {
            let mut warm = Vec::new();
            for _ in 0..bench.warm {
                let started = Instant::now();
                store.search(query, None, 50).recv_blocking()?;
                warm.push(started.elapsed());
            }
            println!("- `{query}` after it, warm: {}", summary(&warm));
        }
    }
    let problems = store.check().recv_blocking()?;
    println!(
        "\n- consistency check: {}",
        if problems.is_empty() {
            "ok".to_owned()
        } else {
            problems.join("; ")
        }
    );
    store.close()?;
    if options.date_keyed {
        date_keyed(bench, &blobs)?;
    }
    Ok(())
}

/// Bits of a date-keyed rowid that hold the message id (S3: `date << 21 | id`).
const ID_BITS: u32 = 21;

/// S3's date-keyed table: the same text with a rowid that sorts by date, so FTS5 returns
/// matches newest first and stops at 50. Built from the blobs; measured for the text shapes.
fn date_keyed(bench: &crate::bench::Options, blobs: &BlobStore) -> Result<()> {
    println!("\n## S3's date-keyed full-text table, for comparison");
    let conn = Connection::open(&bench.store)?;
    conn.execute_batch(
        "DROP TABLE IF EXISTS exp_dated;
         CREATE VIRTUAL TABLE exp_dated USING fts5(
             subject, sender, recipients, body,
             content='', contentless_delete=1,
             tokenize='unicode61 remove_diacritics 2'
         );",
    )?;
    let rows: Vec<(i64, i64, Option<Vec<u8>>)> = conn
        .prepare("SELECT id, date, blob_key FROM messages ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows
        .iter()
        .any(|(id, date, _)| *id >= 1 << ID_BITS || *date < 0)
    {
        bail!("ids or dates out of the date-keyed table's range");
    }
    let people = |list: &[sealwax_domain::model::Address]| -> String {
        list.iter()
            .map(|address| format!("{} {}", address.name, address.email))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parse_time = Duration::ZERO;
    let mut index_time = Duration::ZERO;
    for chunk in rows.chunks(500) {
        let started = Instant::now();
        let docs: Vec<(i64, [String; 4])> = chunk
            .iter()
            .filter_map(|(id, date, key)| {
                let key = key.as_deref()?;
                let key = sealwax_domain::model::BlobKey::from_slice(key)?;
                let raw = blobs.get(&key).ok()?;
                let parsed = message::parse(&raw);
                let headers = &parsed.headers;
                let recipients = [headers.to.as_slice(), headers.cc.as_slice()].concat();
                Some((
                    (date << ID_BITS) | id,
                    [
                        fts::index_text(&headers.subject).into_owned(),
                        fts::index_text(&people(&headers.from)).into_owned(),
                        fts::index_text(&people(&recipients)).into_owned(),
                        fts::index_text(&fts::body_text(&parsed.body_text, &parsed.attachments))
                            .into_owned(),
                    ],
                ))
            })
            .collect();
        parse_time += started.elapsed();
        let started = Instant::now();
        let tx = conn.unchecked_transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO exp_dated(rowid, subject, sender, recipients, body) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (rowid, [subject, sender, recipients, body]) in &docs {
                insert.execute(params![rowid, subject, sender, recipients, body])?;
            }
        }
        tx.commit()?;
        index_time += started.elapsed();
    }
    println!(
        "- built from the blobs: parsing {:.1} s; indexing {:.1} s = {:.0} messages/s",
        parse_time.as_secs_f64(),
        index_time.as_secs_f64(),
        rows.len() as f64 / index_time.as_secs_f64()
    );
    drop(conn);
    let size = |name: &str| -> Result<f64> {
        let pages: i64 = read_only(&bench.store)?.query_row(
            "SELECT coalesce(sum(pgsize), 0) FROM dbstat WHERE name LIKE ?1",
            [format!("{name}%")],
            |row| row.get(0),
        )?;
        Ok(pages as f64 / 1_048_576.0)
    };
    println!(
        "- size: {:.1} MiB, against {:.1} MiB for the index",
        size("exp_dated")?,
        size("message_fts")?
    );
    let sql = format!(
        "SELECT m.id, m.folder_id, m.thread_id, m.from_name, m.from_addr, m.subject, \
           m.snippet, m.date, m.flags, m.has_attachments, t.message_count \
         FROM exp_dated f JOIN messages m ON m.id = f.rowid & {mask} \
           JOIN threads t ON t.id = m.thread_id \
         WHERE exp_dated MATCH ?1 AND m.flags & 264 = 0 AND m.folder_id IN ({{folders}}) \
         ORDER BY f.rowid DESC LIMIT 50",
        mask = (1i64 << ID_BITS) - 1
    );
    let folders: Vec<String> = read_only(&bench.store)?
        .prepare(
            "SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id \
             WHERE a.detached = 0 AND f.role NOT IN ('trash', 'junk') ORDER BY f.id",
        )?
        .query_map([], |row| row.get::<_, i64>(0))?
        .map(|id| id.map(|id| id.to_string()))
        .collect::<rusqlite::Result<_>>()?;
    let sql = sql.replace("{folders}", &folders.join(", "));
    println!("| shape | query | rows | cold p50 / p95 / max ms | warm p50 / p95 / max ms |");
    println!("| --- | --- | --- | --- | --- |");
    for (name, query, inbox_only) in SHAPES {
        let expression = Expression::of(&search::parse(query));
        let Some(matching) = expression.matching.filter(|_| !inbox_only) else {
            continue;
        };
        if expression.excluded.is_some() || query.contains(':') {
            continue;
        }
        let run = |conn: &Connection| -> Result<usize> {
            Ok(conn
                .prepare_cached(&sql)?
                .query_map([&matching], |row| row.get::<_, i64>(0))?
                .count())
        };
        let mut cold = Vec::new();
        let mut rows = 0;
        for _ in 0..bench.cold {
            evict(&bench.store)?;
            let conn = read_only(&bench.store)?;
            let started = Instant::now();
            rows = run(&conn)?;
            cold.push(started.elapsed());
        }
        let conn = read_only(&bench.store)?;
        let mut warm = Vec::new();
        for _ in 0..bench.warm {
            let started = Instant::now();
            rows = run(&conn)?;
            warm.push(started.elapsed());
        }
        let (cold, warm) = (summary(&cold), summary(&warm));
        println!(
            "| {name} | `{query}` | {rows} | {:.2} / {:.2} / {:.2} | {:.2} / {:.2} / {:.2} |",
            cold.p50, cold.p95, cold.max, warm.p50, warm.p95, warm.max
        );
    }
    Ok(())
}
