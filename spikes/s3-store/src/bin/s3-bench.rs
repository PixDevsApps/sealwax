//! S3 store measurements: ingest, FTS indexing, folder open, paging, search and the one-insert
//! diff over a synthetic 100k-message database. Prints a report to stdout.

use anyhow::{Context, Result, bail};
use email_spike_s3::cjk;
use email_spike_s3::rng::Rng;
use email_spike_s3::schema;
use email_spike_s3::sections::Thresholds;
use email_spike_s3::stats::{Summary, ms, summarize};
use email_spike_s3::store::{
    self, CheckpointRun, Checkpointer, FtsDoc, NewMessage, Reader, SearchFilter, SearchPlan, View,
    Writer, flags,
};
use email_spike_s3::synth::{self, Config, Lang, MAILBOXES};
use rusqlite::Connection;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const DATE_KEYED_FTS: &str = "CREATE VIRTUAL TABLE message_fts_by_date USING fts5(\
     subject, sender, recipients, body, content='', contentless_delete=1, \
     tokenize='unicode61 remove_diacritics 2')";

const ID_BITS: i64 = 21;

/// A date-ordered FTS rowid; the spike's ids stay below 2^21.
fn date_key(date: i64, id: i64) -> Result<i64> {
    if !(0..1 << ID_BITS).contains(&id) || date < 0 {
        bail!("id {id} or date {date} out of range for the date key");
    }
    Ok((date << ID_BITS) | id)
}

fn date_keyed_sql(filter: SearchFilter) -> String {
    let mut conditions = String::new();
    if filter.mailbox.is_some() {
        conditions.push_str(" AND m.mailbox_id = ?3");
    }
    if filter.unread_only {
        conditions.push_str(" AND m.flags & 1 = 0");
    }
    format!(
        "SELECT m.id, m.thread_id, m.mailbox_id, m.date, m.from_name, m.from_addr, m.subject, \
                m.snippet, m.flags, m.has_attachments, t.message_count \
         FROM (SELECT m.id AS id FROM message_fts_by_date f \
               CROSS JOIN messages m ON m.id = (f.rowid & {mask}) \
               WHERE message_fts_by_date MATCH ?1{conditions} ORDER BY f.rowid DESC LIMIT ?2) AS p \
         JOIN messages m ON m.id = p.id JOIN threads t ON t.id = m.thread_id \
         ORDER BY m.date DESC, m.id DESC",
        mask = (1i64 << ID_BITS) - 1
    )
}

fn date_keyed_search(
    conn: &Connection,
    query: &str,
    filter: SearchFilter,
    limit: u32,
) -> Result<Vec<i64>> {
    let mut statement = conn.prepare_cached(&date_keyed_sql(filter))?;
    let ids = match filter.mailbox {
        Some(mailbox) => statement
            .query_map(rusqlite::params![query, limit, mailbox], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
        None => statement
            .query_map(rusqlite::params![query, limit], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?,
    };
    Ok(ids)
}

struct Args {
    db: PathBuf,
    messages: usize,
    batch: usize,
    seed: u64,
    now: i64,
    reuse: bool,
    combined: bool,
    corpus_only: bool,
    contention: bool,
    runs: usize,
}

fn parse_args() -> Result<Args> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let mut args = Args {
        db: email_spike_s3::default_db_path(),
        messages: 100_000,
        batch: 500,
        seed: 20_261_004,
        now,
        reuse: false,
        combined: true,
        corpus_only: false,
        contention: false,
        runs: 30,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--db" => args.db = PathBuf::from(value()?),
            "--messages" => args.messages = value()?.parse()?,
            "--batch" => args.batch = value()?.parse()?,
            "--seed" => args.seed = value()?.parse()?,
            "--now" => args.now = value()?.parse()?,
            "--runs" => args.runs = value()?.parse()?,
            "--reuse" => args.reuse = true,
            "--no-combined" => args.combined = false,
            "--corpus-only" => args.corpus_only = true,
            "--contention" => args.contention = true,
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(args)
}

fn remove_db(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let file = PathBuf::from(format!("{}{suffix}", path.display()));
        if file.exists() {
            std::fs::remove_file(&file)?;
        }
    }
    Ok(())
}

/// Drops the database's pages from the OS page cache (POSIX_FADV_DONTNEED), so the next read
/// comes from the SSD. Works without root on files we own.
fn evict(path: &Path) -> Result<()> {
    for suffix in ["", "-wal"] {
        let name = PathBuf::from(format!("{}{suffix}", path.display()));
        if !name.exists() {
            continue;
        }
        let file = File::open(&name)?;
        file.sync_all()?;
        let status =
            unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        if status != 0 {
            bail!("posix_fadvise failed with {status}");
        }
    }
    Ok(())
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn line(title: &str) {
    println!("\n## {title}\n");
}

fn row(label: &str, summary: Summary) {
    println!(
        "| {label} | {} | {:.2} | {:.2} | {:.2} | {:.2} |",
        summary.n, summary.p50, summary.p95, summary.p99, summary.max
    );
}

fn table_head(first: &str) {
    println!("| {first} | n | p50 ms | p95 ms | p99 ms | max ms |");
    println!("| --- | --- | --- | --- | --- | --- |");
}

fn main() -> Result<()> {
    let args = parse_args()?;
    println!("# s3-bench");
    println!();
    println!(
        "db `{}`, messages {}, batch {}, seed {}, now {}, release build: {}",
        args.db.display(),
        args.messages,
        args.batch,
        args.seed,
        args.now,
        !cfg!(debug_assertions)
    );
    environment()?;
    if args.contention {
        return contention(&args);
    }
    if !args.reuse {
        build(&args)?;
        if args.combined {
            combined(&args)?;
        }
    }
    database_report(&args.db)?;
    folder_open(&args)?;
    paging(&args)?;
    search(&args)?;
    writes(&args)?;
    Ok(())
}

fn environment() -> Result<()> {
    line("SQLite");
    let caps = schema::capabilities()?;
    println!("- version {} ({})", caps.version, caps.source);
    println!(
        "- ENABLE_FTS5 compiled: {}; FTS5 works: {}; contentless_delete: {}; trigram: {}; dbstat: {}",
        caps.fts5_compiled, caps.fts5_works, caps.contentless_delete, caps.trigram, caps.dbstat
    );
    println!("- compile options: {}", caps.compile_options.join(", "));
    Ok(())
}

fn generate(args: &Args) -> Result<(synth::Corpus, f64)> {
    let started = Instant::now();
    let corpus = synth::generate(Config {
        messages: args.messages,
        seed: args.seed,
        now: args.now,
        years: 5.0,
    });
    Ok((corpus, ms(started.elapsed())))
}

fn build(args: &Args) -> Result<()> {
    if let Some(parent) = args.db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_db(&args.db)?;
    let (mut corpus, generation_ms) = generate(args)?;
    line("Corpus");
    println!(
        "- generated {} messages in {:.0} ms (not counted below)",
        corpus.messages.len(),
        generation_ms
    );
    let mut per_mailbox = vec![0usize; MAILBOXES.len()];
    let mut langs = [0usize; 4];
    let (mut unread, mut flagged, mut attachments) = (0usize, 0usize, 0usize);
    for message in &corpus.messages {
        per_mailbox[message.mailbox] += 1;
        langs[match message.body.lang {
            Lang::No => 0,
            Lang::En => 1,
            Lang::Zh => 2,
            Lang::Ja => 3,
        }] += 1;
        unread += usize::from(message.header.flags & flags::SEEN == 0);
        flagged += usize::from(message.header.flags & flags::FLAGGED != 0);
        attachments += usize::from(message.header.has_attachments);
    }
    let n = corpus.messages.len() as f64;
    let mailboxes: Vec<String> = MAILBOXES
        .iter()
        .zip(&per_mailbox)
        .map(|(spec, count)| format!("{} {count}", spec.name))
        .collect();
    println!("- mailboxes: {}", mailboxes.join(", "));
    println!(
        "- language: Norwegian {:.1} %, English {:.1} %, Chinese {:.1} %, Japanese {:.1} %",
        100.0 * langs[0] as f64 / n,
        100.0 * langs[1] as f64 / n,
        100.0 * langs[2] as f64 / n,
        100.0 * langs[3] as f64 / n
    );
    println!(
        "- unread {:.1} %, starred {:.1} %, with attachments {:.1} %",
        100.0 * unread as f64 / n,
        100.0 * flagged as f64 / n,
        100.0 * attachments as f64 / n
    );
    let first = corpus
        .messages
        .iter()
        .map(|m| m.header.date)
        .min()
        .unwrap_or(0);
    let last = corpus
        .messages
        .iter()
        .map(|m| m.header.date)
        .max()
        .unwrap_or(0);
    println!(
        "- dates span {:.2} years; subjects {:.0} chars on average; snippets {:.0} chars on average",
        (last - first) as f64 / (365.25 * 86_400.0),
        corpus
            .messages
            .iter()
            .map(|m| m.header.subject.chars().count())
            .sum::<usize>() as f64
            / n,
        corpus
            .messages
            .iter()
            .map(|m| m.header.snippet.chars().count())
            .sum::<usize>() as f64
            / n
    );

    if args.corpus_only {
        std::process::exit(0);
    }
    let mut writer = Writer::open(&args.db)?;
    let ids = synth::create_mailboxes(&mut writer)?;
    corpus.assign_mailboxes(&ids);

    line("Header ingest");
    let mut batch_ms = Vec::new();
    let mut message_ids = Vec::with_capacity(corpus.messages.len());
    for chunk in corpus.messages.chunks(args.batch) {
        let headers: Vec<NewMessage> = chunk.iter().map(|m| m.header.clone()).collect();
        let started = Instant::now();
        message_ids.extend(writer.ingest(&headers)?);
        batch_ms.push(ms(started.elapsed()));
    }
    let total: f64 = batch_ms.iter().sum();
    println!(
        "- {} headers in {:.0} ms = **{:.0} headers/s** (batches of {} in one transaction each; \
         threading, aggregates and 7 indexes included)",
        message_ids.len(),
        total,
        message_ids.len() as f64 / (total / 1000.0),
        args.batch
    );
    table_head("batch commit");
    row(&format!("{} rows", args.batch), summarize(&batch_ms));
    for (index, spec) in MAILBOXES.iter().enumerate() {
        let (uidnext, modseq) = corpus
            .messages
            .iter()
            .filter(|m| m.mailbox == index)
            .map(|m| (m.header.uid + 1, m.header.modseq))
            .max()
            .unwrap_or((1, 1));
        writer.set_mailbox_state(ids[index], uidnext, modseq)?;
        let _ = spec;
    }

    line("Full-text indexing");
    let mut fts_ms = Vec::new();
    let mut lengths = Vec::with_capacity(corpus.messages.len());
    let mut text_bytes = 0u64;
    let mut generate_ms = 0.0;
    for (chunk, chunk_ids) in corpus
        .messages
        .chunks(args.batch)
        .zip(message_ids.chunks(args.batch))
    {
        let started = Instant::now();
        let docs: Vec<FtsDoc> = chunk
            .iter()
            .zip(chunk_ids)
            .map(|(m, id)| m.doc(*id))
            .collect();
        generate_ms += ms(started.elapsed());
        for doc in &docs {
            lengths.push(doc.body.len() as f64);
            text_bytes +=
                (doc.subject.len() + doc.sender.len() + doc.recipients.len() + doc.body.len())
                    as u64;
        }
        let started = Instant::now();
        writer.index(&docs)?;
        fts_ms.push(ms(started.elapsed()));
    }
    let total: f64 = fts_ms.iter().sum();
    println!(
        "- {} documents ({:.1} MiB of text) in {:.0} ms = **{:.0} messages/s** ({:.1} MiB/s); \
         body generation ({:.0} ms) not counted",
        message_ids.len(),
        mib(text_bytes),
        total,
        message_ids.len() as f64 / (total / 1000.0),
        mib(text_bytes) / (total / 1000.0),
        generate_ms
    );
    table_head("batch commit");
    row(&format!("{} docs", args.batch), summarize(&fts_ms));
    let body = summarize(&lengths);
    println!(
        "\nPlain-text body bytes: mean {:.0}, p50 {:.0}, p95 {:.0}, p99 {:.0}, max {:.0}",
        body.mean, body.p50, body.p95, body.p99, body.max
    );

    line("Experiment: date-keyed FTS table");
    writer.connection().execute_batch(DATE_KEYED_FTS)?;
    let dates: Vec<i64> = corpus.messages.iter().map(|m| m.header.date).collect();
    let mut keyed_ms = Vec::new();
    for ((chunk, chunk_ids), chunk_dates) in corpus
        .messages
        .chunks(args.batch)
        .zip(message_ids.chunks(args.batch))
        .zip(dates.chunks(args.batch))
    {
        let docs: Vec<FtsDoc> = chunk
            .iter()
            .zip(chunk_ids)
            .map(|(m, id)| m.doc(*id))
            .collect();
        let started = Instant::now();
        let tx = writer.connection().unchecked_transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO message_fts_by_date(rowid, subject, sender, recipients, body) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (doc, date) in docs.iter().zip(chunk_dates) {
                insert.execute(rusqlite::params![
                    date_key(*date, doc.id)?,
                    cjk::index_text(&doc.subject).as_ref(),
                    cjk::index_text(&doc.sender).as_ref(),
                    cjk::index_text(&doc.recipients).as_ref(),
                    cjk::index_text(&doc.body).as_ref(),
                ])?;
            }
        }
        tx.commit()?;
        keyed_ms.push(ms(started.elapsed()));
    }
    let total: f64 = keyed_ms.iter().sum();
    println!(
        "- the same documents with rowid = `date << 21 | id` (a date-ordered key) in {:.0} ms = \
         {:.0} messages/s; searched below as \"date-keyed\"",
        total,
        message_ids.len() as f64 / (total / 1000.0)
    );

    line("After ingest");
    let started = Instant::now();
    writer
        .connection()
        .execute_batch("PRAGMA optimize=0x10002;")?;
    println!(
        "- `PRAGMA optimize=0x10002` (ANALYZE where useful): {:.0} ms",
        ms(started.elapsed())
    );
    let started = Instant::now();
    writer
        .connection()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    println!(
        "- `PRAGMA wal_checkpoint(TRUNCATE)`: {:.0} ms",
        ms(started.elapsed())
    );
    let started = Instant::now();
    let problems = writer.check_aggregates()?;
    println!(
        "- aggregate check (recomputes every count from the messages): {:.0} ms, {}",
        ms(started.elapsed()),
        if problems.is_empty() {
            "all consistent".to_owned()
        } else {
            problems.join("; ")
        }
    );
    Ok(())
}

/// Headers and text in the same transaction, on a separate database.
fn combined(args: &Args) -> Result<()> {
    let path = args.db.with_file_name("s3-combined.sqlite");
    remove_db(&path)?;
    let (mut corpus, _) = generate(args)?;
    let mut writer = Writer::open(&path)?;
    let ids = synth::create_mailboxes(&mut writer)?;
    corpus.assign_mailboxes(&ids);
    let mut batch_ms = Vec::new();
    for chunk in corpus.messages.chunks(args.batch) {
        let batch: Vec<(NewMessage, FtsDoc)> =
            chunk.iter().map(|m| (m.header.clone(), m.doc(0))).collect();
        let started = Instant::now();
        writer.ingest_with_text(&batch)?;
        batch_ms.push(ms(started.elapsed()));
    }
    let total: f64 = batch_ms.iter().sum();
    line("Headers and text in one transaction (separate database)");
    println!(
        "- {} messages in {:.0} ms = **{:.0} messages/s**",
        corpus.messages.len(),
        total,
        corpus.messages.len() as f64 / (total / 1000.0)
    );
    table_head("batch commit");
    row(&format!("{} messages", args.batch), summarize(&batch_ms));
    drop(writer);
    remove_db(&path)?;
    Ok(())
}

fn database_report(path: &Path) -> Result<()> {
    let conn = Connection::open(path)?;
    line("Database");
    println!(
        "- file {:.1} MiB (WAL {:.1} MiB), page size {}",
        mib(file_size(path)),
        mib(file_size(&PathBuf::from(format!("{}-wal", path.display())))),
        conn.query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))?
    );
    if conn.execute_batch("SELECT count(*) FROM dbstat").is_ok() {
        let mut statement = conn.prepare(
            "SELECT name, sum(pgsize) FROM dbstat GROUP BY name ORDER BY sum(pgsize) DESC LIMIT 16",
        )?;
        let sizes: Vec<String> = statement
            .query_map([], |row| {
                Ok(format!(
                    "{} {:.1}",
                    row.get::<_, String>(0)?,
                    mib(row.get::<_, i64>(1)? as u64)
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        println!("- largest objects (MiB): {}", sizes.join(", "));
    }
    let threads: i64 = conn.query_row("SELECT count(*) FROM threads", [], |row| row.get(0))?;
    let messages: i64 = conn.query_row("SELECT count(*) FROM messages", [], |row| row.get(0))?;
    let buckets: Vec<String> = conn
        .prepare(
            "SELECT CASE WHEN message_count = 1 THEN '1' WHEN message_count <= 3 THEN '2-3' \
               WHEN message_count <= 10 THEN '4-10' WHEN message_count <= 25 THEN '11-25' ELSE '26+' END AS b, \
             count(*) FROM threads GROUP BY b ORDER BY min(message_count)",
        )?
        .query_map([], |row| Ok(format!("{} {}", row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let biggest: i64 = conn.query_row("SELECT max(message_count) FROM threads", [], |row| {
        row.get(0)
    })?;
    let duplicates: i64 = conn.query_row(
        "SELECT count(*) FROM (SELECT message_id FROM messages GROUP BY message_id HAVING count(*) > 1)",
        [],
        |row| row.get(0),
    )?;
    println!(
        "- {messages} messages in {threads} conversations (sizes: {}; largest {biggest}); \
         {duplicates} Message-IDs stored in two mailboxes",
        buckets.join(", ")
    );
    let mailboxes: Vec<String> = conn
        .prepare("SELECT name, total_count, unread_count FROM mailboxes ORDER BY id")?
        .query_map([], |row| {
            Ok(format!(
                "{} {}/{}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    println!("- mailboxes (total/unread): {}", mailboxes.join(", "));

    line("Query plans");
    let inbox: i64 =
        conn.query_row("SELECT id FROM mailboxes WHERE name = 'INBOX'", [], |row| {
            row.get(0)
        })?;
    let plans: Vec<(&str, String, Vec<rusqlite::types::Value>)> = vec![
        (
            "page, all mail",
            store::page_sql(View::All, false),
            vec![100.into(), 0.into()],
        ),
        (
            "page, all mail, oldest end",
            store::page_sql(View::All, true),
            vec![100.into(), 0.into()],
        ),
        (
            "page, mailbox",
            store::page_sql(View::Mailbox(inbox), false),
            vec![100.into(), 0.into(), inbox.into()],
        ),
        (
            "conversations",
            store::CONVERSATIONS_SQL.to_owned(),
            vec![inbox.into(), 100.into(), 0.into()],
        ),
        (
            "search, FTS first",
            store::search_sql(SearchFilter::default(), SearchPlan::FtsFirst),
            vec!["møte".to_owned().into(), 50.into()],
        ),
        (
            "search, date first",
            store::search_sql(SearchFilter::default(), SearchPlan::DateFirst),
            vec!["møte".to_owned().into(), 50.into()],
        ),
        (
            "search, mailbox + unread",
            store::search_sql(
                SearchFilter {
                    mailbox: Some(inbox),
                    unread_only: true,
                },
                SearchPlan::FtsFirst,
            ),
            vec!["møte".to_owned().into(), 50.into(), inbox.into()],
        ),
        (
            "search, date-keyed FTS",
            date_keyed_sql(SearchFilter::default()),
            vec!["møte".to_owned().into(), 50.into()],
        ),
        (
            "search, date-keyed FTS, mailbox + unread",
            date_keyed_sql(SearchFilter {
                mailbox: Some(inbox),
                unread_only: true,
            }),
            vec!["møte".to_owned().into(), 50.into(), inbox.into()],
        ),
        (
            "position (diff)",
            "SELECT count(*) FROM messages WHERE mailbox_id = ?1 AND date > ?2".to_owned(),
            vec![inbox.into(), 0.into()],
        ),
    ];
    for (name, sql, params) in plans {
        let steps: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
            .query_map(rusqlite::params_from_iter(params), |row| {
                row.get::<_, String>(3)
            })?
            .collect::<rusqlite::Result<_>>()?;
        println!("- {name}: `{}`", steps.join(" → "));
    }
    Ok(())
}

fn views(conn: &Connection) -> Result<Vec<(String, View)>> {
    let mut views = vec![("All mail".to_owned(), View::All)];
    let mut statement = conn.prepare("SELECT id, name, total_count FROM mailboxes ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for item in rows {
        let (id, name, total) = item?;
        views.push((format!("{name} ({total})"), View::Mailbox(id)));
    }
    Ok(views)
}

fn folder_open(args: &Args) -> Result<()> {
    let thresholds = Thresholds::local_now()?;
    let probe = Reader::open(&args.db)?;
    let views = views(probe.connection())?;
    drop(probe);
    line("Folder open (count + section bounds + first 100 rows, one read transaction)");
    println!(
        "Cold = database evicted from the page cache (POSIX_FADV_DONTNEED) and a new read-only \
         connection; warm = the same connection again. Section bounds come from four range counts.\n"
    );
    table_head("view");
    let mut cold_connect = Vec::new();
    for (name, view) in &views {
        let mut cold = Vec::new();
        for _ in 0..10 {
            evict(&args.db)?;
            let started = Instant::now();
            let reader = Reader::open(&args.db)?;
            cold_connect.push(ms(started.elapsed()));
            let started = Instant::now();
            let snapshot = reader.open_view(*view, &thresholds, 100)?;
            cold.push(ms(started.elapsed()));
            assert!(snapshot.rows.len() as u32 == snapshot.total.min(100));
        }
        let reader = Reader::open(&args.db)?;
        let mut warm = Vec::new();
        for _ in 0..50 {
            let started = Instant::now();
            reader.open_view(*view, &thresholds, 100)?;
            warm.push(ms(started.elapsed()));
        }
        row(&format!("{name}, cold"), summarize(&cold));
        row(&format!("{name}, warm"), summarize(&warm));
    }
    row(
        "open a read-only connection (cold)",
        summarize(&cold_connect),
    );

    let reader = Reader::open(&args.db)?;
    let inbox = views
        .iter()
        .find_map(|(name, view)| name.starts_with("INBOX").then_some(*view))
        .context("no INBOX")?;
    let View::Mailbox(inbox) = inbox else {
        unreachable!()
    };
    let threads: i64 = reader.connection().query_row(
        "SELECT count(*) FROM thread_mailboxes WHERE mailbox_id = ?1",
        [inbox],
        |row| row.get(0),
    )?;
    let mut cold = Vec::new();
    for _ in 0..10 {
        evict(&args.db)?;
        let reader = Reader::open(&args.db)?;
        let started = Instant::now();
        reader.conversations(inbox, 0, 100)?;
        cold.push(ms(started.elapsed()));
    }
    let mut warm = Vec::new();
    let mut deep = Vec::new();
    for _ in 0..50 {
        let started = Instant::now();
        reader.conversations(inbox, 0, 100)?;
        warm.push(ms(started.elapsed()));
        let started = Instant::now();
        reader.conversations(inbox, threads as u32 - 100, 100)?;
        deep.push(ms(started.elapsed()));
    }
    println!();
    table_head(&format!(
        "INBOX conversation list ({threads} conversations)"
    ));
    row("first 100, cold", summarize(&cold));
    row("first 100, warm", summarize(&warm));
    row("last 100 (OFFSET), warm", summarize(&deep));
    Ok(())
}

fn paging(args: &Args) -> Result<()> {
    let reader = Reader::open(&args.db)?;
    let total: i64 =
        reader
            .connection()
            .query_row("SELECT sum(total_count) FROM mailboxes", [], |row| {
                row.get(0)
            })?;
    let total = total as u32;
    line(&format!(
        "Page of 100 rows by position, All mail ({total} rows)"
    ));
    println!(
        "`page` walks from whichever end of the date index is nearer (OFFSET ≤ n/2); \
         `forward` always walks from the newest row.\n"
    );
    table_head("offset");
    for offset in [0, total / 4, total / 2, 3 * total / 4, total - 100] {
        let mut near = Vec::new();
        let mut forward = Vec::new();
        for _ in 0..args.runs {
            let started = Instant::now();
            reader.page(View::All, offset, 100)?;
            near.push(ms(started.elapsed()));
            let started = Instant::now();
            reader.page_forward(View::All, offset, 100)?;
            forward.push(ms(started.elapsed()));
        }
        row(&format!("{offset}, page"), summarize(&near));
        row(&format!("{offset}, forward"), summarize(&forward));
    }
    let mut cold = Vec::new();
    for _ in 0..10 {
        evict(&args.db)?;
        let reader = Reader::open(&args.db)?;
        let started = Instant::now();
        reader.page(View::All, total - 100, 100)?;
        cold.push(ms(started.elapsed()));
    }
    row("last page, cold", summarize(&cold));
    Ok(())
}

fn search(args: &Args) -> Result<()> {
    let reader = Reader::open(&args.db)?;
    let inbox: i64 = reader.connection().query_row(
        "SELECT id FROM mailboxes WHERE name = 'INBOX'",
        [],
        |row| row.get(0),
    )?;
    let shapes: Vec<(&str, String, SearchFilter)> = vec![
        (
            "common Norwegian word",
            "møte".to_owned(),
            SearchFilter::default(),
        ),
        (
            "very common English word",
            "the".to_owned(),
            SearchFilter::default(),
        ),
        (
            "two words (AND)",
            "budsjett kontrakt".to_owned(),
            SearchFilter::default(),
        ),
        (
            "phrase",
            "\"project update\"".to_owned(),
            SearchFilter::default(),
        ),
        ("prefix", "prosjekt*".to_owned(), SearchFilter::default()),
        (
            "sender column",
            "sender:kari".to_owned(),
            SearchFilter::default(),
        ),
        (
            "rare word with æøå",
            "blåbærsyltetøy".to_owned(),
            SearchFilter::default(),
        ),
        (
            "Chinese word (bigrams)",
            cjk::query_term("会议"),
            SearchFilter::default(),
        ),
        (
            "Japanese word (bigrams)",
            cjk::query_term("進捗"),
            SearchFilter::default(),
        ),
        ("no match", "zzqxv".to_owned(), SearchFilter::default()),
        (
            "word + INBOX + unread",
            "faktura".to_owned(),
            SearchFilter {
                mailbox: Some(inbox),
                unread_only: true,
            },
        ),
    ];
    line("Full-text search: newest 50 matches with their list rows");
    println!(
        "Each shape: one cold run (page cache evicted, new connection) and {} warm runs per plan. \
         FTS first = every match joined to its date, newest 50 kept; date first = walk the date \
         index and test membership in the match set.\n",
        args.runs
    );
    println!(
        "| shape | query | matches | plan | cold ms | warm p50 ms | warm p95 ms | warm max ms |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
    let keyed = reader.connection().query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name = 'message_fts_by_date'",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1;
    for (name, query, filter) in &shapes {
        let matches = reader.search_count(query)?;
        if keyed {
            evict(&args.db)?;
            let cold_reader = Reader::open(&args.db)?;
            let started = Instant::now();
            let cold_ids = date_keyed_search(cold_reader.connection(), query, *filter, 50)?;
            let cold = ms(started.elapsed());
            let expected: Vec<i64> = reader
                .search(query, *filter, 50, SearchPlan::FtsFirst)?
                .iter()
                .map(|row| row.id)
                .collect();
            assert_eq!(cold_ids, expected, "date-keyed results differ for {query}");
            let mut warm = Vec::new();
            for _ in 0..args.runs {
                let started = Instant::now();
                date_keyed_search(reader.connection(), query, *filter, 50)?;
                warm.push(ms(started.elapsed()));
            }
            let summary = summarize(&warm);
            println!(
                "| {name} | `{}` | {matches} | date-keyed | {cold:.2} | {:.2} | {:.2} | {:.2} |",
                query.replace('|', "\\|"),
                summary.p50,
                summary.p95,
                summary.max
            );
        }
        for plan in [SearchPlan::FtsFirst, SearchPlan::DateFirst] {
            evict(&args.db)?;
            let cold_reader = Reader::open(&args.db)?;
            let started = Instant::now();
            let cold_rows = cold_reader.search(query, *filter, 50, plan)?;
            let cold = ms(started.elapsed());
            let mut warm = Vec::new();
            for _ in 0..args.runs {
                let started = Instant::now();
                let rows = reader.search(query, *filter, 50, plan)?;
                warm.push(ms(started.elapsed()));
                assert_eq!(rows.len(), cold_rows.len());
            }
            let summary = summarize(&warm);
            println!(
                "| {name} | `{}` | {matches} | {} | {cold:.2} | {:.2} | {:.2} | {:.2} |",
                query.replace('|', "\\|"),
                if plan == SearchPlan::FtsFirst {
                    "FTS first"
                } else {
                    "date first"
                },
                summary.p50,
                summary.p95,
                summary.max
            );
        }
    }
    Ok(())
}

struct Planned {
    kind: usize,
    header: NewMessage,
    doc: Option<FtsDoc>,
}

const KINDS: [&str; 4] = [
    "new message, newest (top of list), with text",
    "reply to an existing conversation, newest, with text",
    "old message (deep position), with text",
    "new message, newest, headers only",
];

fn plan_inserts(args: &Args, writer: &Writer, seed: u64, uid: &mut i64) -> Result<Vec<Planned>> {
    let conn = writer.connection();
    let inbox: i64 =
        conn.query_row("SELECT id FROM mailboxes WHERE name = 'INBOX'", [], |row| {
            row.get(0)
        })?;
    let existing: Vec<(String, String)> = conn
        .prepare("SELECT message_id, subject FROM messages WHERE mailbox_id = ?1 ORDER BY random() LIMIT 200")?
        .query_map([inbox], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut rng = Rng::new(seed);
    let mut planned = Vec::new();
    for round in 0..100i64 {
        for kind in 0..KINDS.len() {
            let date = match kind {
                2 => args.now - rng.range(30 * 86_400, 5 * 365 * 86_400),
                _ => args.now + 60 + round,
            };
            let reply = (kind == 1).then(|| {
                let (id, subject) = &existing[rng.below(existing.len() as u64) as usize];
                (id.as_str(), subject.as_str())
            });
            let (mut header, body) =
                synth::incoming(seed + 7 * round as u64 + kind as u64, date, reply);
            header.mailbox_id = inbox;
            header.uid = *uid;
            *uid += 1;
            let doc = (kind != 3).then(|| FtsDoc {
                id: 0,
                subject: header.subject.clone(),
                sender: format!("{} {}", header.from_name, header.from_addr),
                recipients: header.to_summary.clone(),
                body: synth::body(&body, usize::MAX),
            });
            planned.push(Planned { kind, header, doc });
        }
    }
    Ok(planned)
}

/// A PASSIVE checkpoint every 200 ms on its own connection until stopped (the naive policy).
fn timed_checkpoints(
    path: PathBuf,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<Vec<CheckpointRun>>> {
    std::thread::Builder::new()
        .name("s3-checkpoint".into())
        .spawn(move || {
            let conn = Connection::open(&path)?;
            let mut runs = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let (_, log, _) = store::checkpoint(&conn)?;
                runs.push(CheckpointRun {
                    frames: log,
                    checkpoint_ms: ms(started.elapsed()),
                    restart_ms: None,
                });
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok(runs)
        })
        .expect("spawn checkpoint thread")
}

/// The proposed policy: the writer's WAL hook counts frames; this thread checkpoints at
/// `threshold` frames and restarts the emptied WAL itself.
fn hooked_checkpoints(
    path: PathBuf,
    threshold: i64,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<Vec<CheckpointRun>>> {
    std::thread::Builder::new()
        .name("s3-checkpoint".into())
        .spawn(move || {
            let checkpointer = Checkpointer::open(&path, threshold)?;
            let mut runs = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Some(run) = checkpointer.tick()? {
                    runs.push(run);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(runs)
        })
        .expect("spawn checkpoint thread")
}

fn describe_runs(runs: &[CheckpointRun]) -> String {
    let checkpoints: Vec<f64> = runs
        .iter()
        .filter(|run| run.frames > 0)
        .map(|run| run.checkpoint_ms)
        .collect();
    let restarts: Vec<f64> = runs.iter().filter_map(|run| run.restart_ms).collect();
    let checkpoint = summarize(&checkpoints);
    let restart = summarize(&restarts);
    format!(
        "{} checkpoints with frames (p50 {:.1} ms, max {:.1} ms), {} background WAL restarts (p50 {:.1} ms, max {:.1} ms)",
        checkpoint.n, checkpoint.p50, checkpoint.max, restart.n, restart.p50, restart.max
    )
}

fn writes(args: &Args) -> Result<()> {
    let mut writer = Writer::open(&args.db)?;
    let inbox: i64 = writer.connection().query_row(
        "SELECT id FROM mailboxes WHERE name = 'INBOX'",
        [],
        |row| row.get(0),
    )?;
    let mut uid: i64 = writer.connection().query_row(
        "SELECT max(uid) + 1 FROM messages WHERE mailbox_id = ?1",
        [inbox],
        |row| row.get(0),
    )?;
    let views = [View::All, View::Mailbox(inbox)];

    line("One-message insert + list diff (writer connection)");
    println!(
        "Each insert: thread resolution, aggregates, `store_version` bump, and the positions in \
         All mail and INBOX plus the conversation move, read inside the transaction; timed until \
         COMMIT returns (WAL, synchronous=NORMAL). 100 inserts of each kind, interleaved.\n"
    );
    println!(
        "**A: defaults** (wal_autocheckpoint=1000 on the writer; FTS text in the same transaction)\n"
    );
    let planned = plan_inserts(args, &writer, args.seed ^ 0xA11, &mut uid)?;
    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); KINDS.len()];
    let mut inserted = Vec::new();
    let mut deepest = 0;
    for item in &planned {
        let started = Instant::now();
        let diff = writer.insert_one(&item.header, item.doc.as_ref(), &views)?;
        samples[item.kind].push(ms(started.elapsed()));
        deepest = deepest.max(diff.positions.iter().map(|(_, p)| *p).max().unwrap_or(0));
        inserted.push(diff.row.id);
    }
    table_head("kind");
    for (name, values) in KINDS.iter().zip(&samples) {
        row(name, summarize(values));
    }
    row("all", summarize(&samples.concat()));
    let mut expunge_a = Vec::new();
    for id in &inserted {
        let started = Instant::now();
        writer.expunge(*id)?;
        expunge_a.push(ms(started.elapsed()));
    }
    row("expunge (aggregates recomputed)", summarize(&expunge_a));
    println!("\nDeepest position reached: {deepest}.\n");

    println!(
        "**B: checkpoints handed off, text afterwards** (the writer's WAL hook replaces \
         autocheckpoint and only counts frames; a checkpoint thread runs PASSIVE checkpoints at \
         1000 frames and restarts the emptied WAL itself; the diff is ready when the header \
         transaction commits, and the FTS row follows in its own transaction)\n"
    );
    writer
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    writer.hand_off_checkpoints();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = hooked_checkpoints(args.db.clone(), 1000, stop.clone());
    let planned = plan_inserts(args, &writer, args.seed ^ 0xB22, &mut uid)?;
    let mut diff_ms: Vec<Vec<f64>> = vec![Vec::new(); KINDS.len()];
    let mut text_ms = Vec::new();
    let mut inserted = Vec::new();
    for item in &planned {
        let started = Instant::now();
        let diff = writer.insert_one(&item.header, None, &views)?;
        diff_ms[item.kind].push(ms(started.elapsed()));
        if let Some(doc) = &item.doc {
            let started = Instant::now();
            writer.index(&[FtsDoc {
                id: diff.row.id,
                ..doc.clone()
            }])?;
            text_ms.push(ms(started.elapsed()));
        }
        inserted.push(diff.row.id);
    }
    table_head("kind (diff ready)");
    for (name, values) in KINDS.iter().zip(&diff_ms) {
        row(name, summarize(values));
    }
    row("all (diff ready)", summarize(&diff_ms.concat()));
    row("FTS row, own transaction", summarize(&text_ms));
    let mut flag_ms = Vec::new();
    for id in inserted.iter().take(200) {
        let started = Instant::now();
        writer.set_flags(*id, flags::SEEN | flags::FLAGGED)?;
        flag_ms.push(ms(started.elapsed()));
    }
    let mut expunge_b = Vec::new();
    for id in &inserted {
        let started = Instant::now();
        writer.expunge(*id)?;
        expunge_b.push(ms(started.elapsed()));
    }
    row("set flags (read + starred)", summarize(&flag_ms));
    row("expunge (aggregates recomputed)", summarize(&expunge_b));
    stop.store(true, Ordering::Relaxed);
    let runs = thread.join().expect("checkpoint thread")?;
    writer
        .connection()
        .execute_batch("PRAGMA wal_autocheckpoint=1000;")?;
    let started = Instant::now();
    writer
        .connection()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    println!(
        "\nCheckpoint thread: {}; the final TRUNCATE checkpoint took {:.1} ms.",
        describe_runs(&runs),
        ms(started.elapsed())
    );
    let problems = writer.check_aggregates()?;
    println!(
        "After the inserted messages were expunged again: {}.",
        if problems.is_empty() {
            "aggregates consistent".to_owned()
        } else {
            problems.join("; ")
        }
    );
    let total: i64 = writer
        .connection()
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))?;
    println!("Messages in the database: {total}.");
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    Autocheckpoint,
    Timed,
    Hooked,
}

/// The list app's setup without GTK: readers paging on two threads, a checkpoint policy, and
/// single inserts 250 ms apart.
fn contention(args: &Args) -> Result<()> {
    line("fdatasync of a 4 KiB append next to the database");
    let probe_path = args.db.with_file_name("fsync-probe.bin");
    let mut fsyncs = Vec::new();
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&probe_path)?;
        for _ in 0..30 {
            file.write_all(&[7u8; 4096])?;
            let started = Instant::now();
            file.sync_data()?;
            fsyncs.push(ms(started.elapsed()));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    std::fs::remove_file(&probe_path)?;
    table_head("operation");
    row("fdatasync after a 4 KiB append", summarize(&fsyncs));

    line("Insert latency next to readers and a checkpoint policy (inserts 250 ms apart)");
    println!(
        "autocheckpoint = SQLite's default on the writer (checkpoint inside the commit that \
         crosses 1000 frames); timed = writer autocheckpoint off, PASSIVE checkpoint every 200 ms \
         on another thread; hooked = the writer's WAL hook counts frames, another thread \
         checkpoints at 50 frames (low, to force several cycles) and restarts the WAL itself.\n"
    );
    table_head("setup");
    let setups = [
        (Policy::Autocheckpoint, false, false, false),
        (Policy::Autocheckpoint, true, false, false),
        (Policy::Timed, true, false, false),
        (Policy::Timed, true, false, true),
        (Policy::Hooked, true, false, false),
        (Policy::Hooked, true, true, false),
    ];
    for (policy, readers, text, unsynced) in setups {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        if readers {
            for _ in 0..2 {
                let stop = stop.clone();
                let path = args.db.clone();
                threads.push(std::thread::spawn(move || -> Result<()> {
                    let reader = Reader::open(&path)?;
                    let mut rng = Rng::new(1);
                    while !stop.load(Ordering::Relaxed) {
                        reader.page(View::All, rng.below(99_900) as u32, 100)?;
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Ok(())
                }));
            }
        }
        let mut writer = Writer::open(&args.db)?;
        let checkpoints = match policy {
            Policy::Autocheckpoint => None,
            Policy::Timed => {
                writer.disable_autocheckpoint()?;
                Some(timed_checkpoints(args.db.clone(), stop.clone()))
            }
            Policy::Hooked => {
                writer.hand_off_checkpoints();
                Some(hooked_checkpoints(args.db.clone(), 50, stop.clone()))
            }
        };
        if unsynced {
            writer
                .connection()
                .execute_batch("PRAGMA synchronous=OFF;")?;
        }
        let inbox: i64 = writer.connection().query_row(
            "SELECT id FROM mailboxes WHERE name = 'INBOX'",
            [],
            |row| row.get(0),
        )?;
        let first_uid: i64 = writer.connection().query_row(
            "SELECT max(uid) + 1 FROM messages WHERE mailbox_id = ?1",
            [inbox],
            |row| row.get(0),
        )?;
        let mut samples = Vec::new();
        let mut ids = Vec::new();
        for round in 0..30u64 {
            let (mut header, body) =
                synth::incoming(77 + round, args.now + 100 + round as i64, None);
            header.mailbox_id = inbox;
            header.uid = first_uid + round as i64;
            std::thread::sleep(std::time::Duration::from_millis(250));
            let started = Instant::now();
            let diff = writer.insert_one(&header, None, &[View::All])?;
            samples.push(ms(started.elapsed()));
            ids.push(diff.row.id);
            if text {
                writer.index(&[FtsDoc {
                    id: diff.row.id,
                    subject: header.subject.clone(),
                    sender: header.from_name.clone(),
                    recipients: header.to_summary.clone(),
                    body: synth::body(&body, usize::MAX),
                }])?;
            }
        }
        stop.store(true, Ordering::Relaxed);
        for thread in threads {
            thread.join().expect("reader thread")?;
        }
        let runs = match checkpoints {
            Some(thread) => thread.join().expect("checkpoint thread")?,
            None => Vec::new(),
        };
        writer.connection().wal_hook(None);
        writer
            .connection()
            .execute_batch("PRAGMA wal_autocheckpoint=1000; PRAGMA synchronous=NORMAL;")?;
        for id in ids {
            writer.expunge(id)?;
        }
        writer
            .connection()
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        let name = format!(
            "{}{}{}{}",
            match policy {
                Policy::Autocheckpoint => "autocheckpoint",
                Policy::Timed => "timed",
                Policy::Hooked => "hooked",
            },
            if readers { ", 2 readers" } else { "" },
            if text {
                ", FTS row after each insert"
            } else {
                ""
            },
            if unsynced {
                ", writer synchronous=OFF (diagnosis only)"
            } else {
                ""
            }
        );
        row(&name, summarize(&samples));
        if !runs.is_empty() {
            println!("|   ↳ {} | | | | | |", describe_runs(&runs));
        }
    }
    Ok(())
}
