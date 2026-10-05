//! `sealwax-fixtures`: deterministic synthetic mail for Sealwax's development and benchmarks.
//! Every name and address is fictional, at example.test. Write stores only to scratch paths;
//! the app's own store is at `SealwaxDirs::from_env().store_file()`.

mod bench;
mod corpus;
mod import;
mod mime;
mod remove;
mod rng;
mod stats;
mod unified;
mod words;

use anyhow::{Context, Result, bail};
use corpus::{Config, Corpus, DEFAULT_NOW};
use import::{FolderMode, Options, Source};
use stats::ms;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const USAGE: &str = "usage:
  sealwax-fixtures generate --count N [--seed S] [--now UNIX|now] [--years Y] --out DIR
  sealwax-fixtures import --store PATH [--blobs DIR] (--dir DIR | --count N [--seed S] [--now UNIX|now] [--years Y])
                          [--folders single|spread] [--account NAME] [--chunk N] [--workers N]
  sealwax-fixtures bench --store PATH [--blobs DIR] [--cold N] [--warm N] [--inserts N] [--now UNIX]
  sealwax-fixtures bench-unified --store PATH [--blobs DIR] [--cold N] [--warm N] [--inserts N] [--now UNIX]
  sealwax-fixtures check --store PATH
  sealwax-fixtures remove-account --store PATH [--blobs DIR] [--account NAME]

The blob store defaults to `blobs` next to the store file. Messages are numbered newest first,
the order of a first sync. `bench-unified` measures All Inboxes over several accounts, each
imported with its own --account.";

struct Args {
    command: String,
    values: HashMap<String, String>,
}

impl Args {
    fn parse() -> Result<Args> {
        let mut args = std::env::args().skip(1);
        let command = args.next().context(USAGE)?;
        let mut values = HashMap::new();
        while let Some(flag) = args.next() {
            let Some(name) = flag.strip_prefix("--") else {
                bail!("unexpected argument {flag}\n\n{USAGE}");
            };
            let value = args
                .next()
                .with_context(|| format!("--{name} needs a value"))?;
            values.insert(name.to_owned(), value);
        }
        Ok(Args { command, values })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        self.get(name)
            .map(PathBuf::from)
            .with_context(|| format!("--{name} is required\n\n{USAGE}"))
    }

    fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T> {
        match self.get(name) {
            Some(value) => value
                .parse()
                .map_err(|_| anyhow::anyhow!("--{name}: not a number: {value}")),
            None => Ok(default),
        }
    }

    fn now(&self) -> Result<i64> {
        match self.get("now") {
            Some("now") => Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64),
            _ => self.number("now", DEFAULT_NOW),
        }
    }

    fn corpus(&self) -> Result<Corpus> {
        let count: usize = self.number("count", 0)?;
        if count == 0 {
            bail!("--count must be at least 1\n\n{USAGE}");
        }
        Ok(Corpus::generate(Config {
            count,
            seed: self.number("seed", 1)?,
            now: self.now()?,
            years: self.number("years", 5.0)?,
        }))
    }

    fn bench(&self) -> Result<bench::Options> {
        let store = self.path("store")?;
        Ok(bench::Options {
            blobs: self.blobs(&store)?,
            store,
            cold: self.number("cold", 10)?,
            warm: self.number("warm", 30)?,
            inserts: self.number("inserts", 400)?,
            now: self.now()?,
        })
    }

    fn blobs(&self, store: &Path) -> Result<PathBuf> {
        match self.get("blobs") {
            Some(blobs) => Ok(PathBuf::from(blobs)),
            None => Ok(store
                .parent()
                .context("--store needs a directory")?
                .join("blobs")),
        }
    }
}

fn generate(args: &Args) -> Result<()> {
    let out = args.path("out")?;
    let started = Instant::now();
    let corpus = args.corpus()?;
    std::fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;
    let mut bytes = 0;
    for index in 0..corpus.len() {
        let raw = corpus.render(index);
        bytes += raw.len();
        std::fs::write(out.join(corpus.file_name(index)), raw)?;
    }
    println!(
        "wrote {} messages ({:.1} MiB) to {} in {:.0} ms",
        corpus.len(),
        bytes as f64 / 1_048_576.0,
        out.display(),
        ms(started.elapsed())
    );
    Ok(())
}

fn import(args: &Args) -> Result<()> {
    let store = args.path("store")?;
    let options = Options {
        account: args
            .get("account")
            .unwrap_or(import::ACCOUNT_NAME)
            .to_owned(),
        blobs: args.blobs(&store)?,
        store,
        folders: match args.get("folders").unwrap_or("single") {
            "single" => FolderMode::Single,
            "spread" => FolderMode::Spread,
            other => bail!("--folders: single or spread, not {other}"),
        },
        chunk: args.number("chunk", 1_000)?,
        workers: args.number("workers", sealwax_infrastructure::ingest::DEFAULT_WORKERS)?,
    };
    let generated = Instant::now();
    let source = match args.get("dir") {
        Some(dir) => Source::Dir(PathBuf::from(dir)),
        None => Source::Corpus(args.corpus()?),
    };
    let generate_ms = ms(generated.elapsed());
    let report = import::run(source, &options)?;
    let rate = |count: usize, ms: f64| count as f64 / (ms / 1000.0);
    let elapsed = ms(report.elapsed);
    let header = ms(report.header_time);
    let index = ms(report.index_time);
    println!("## Import");
    println!(
        "- {} messages ({:.1} MiB raw): {} stored, {} skipped",
        report.messages,
        report.raw_bytes as f64 / 1_048_576.0,
        report.stored,
        report.skipped
    );
    println!("- corpus specs generated in {generate_ms:.0} ms");
    println!(
        "- end to end: {elapsed:.0} ms = {:.0} messages/s (render or read, blob store, parse, rows, full text)",
        rate(report.stored, elapsed)
    );
    println!(
        "- writer, header transactions: {header:.0} ms in {} batches = {:.0} headers/s",
        report.batches,
        rate(report.stored, header)
    );
    println!(
        "- writer, full-text transactions: {index:.0} ms = {:.0} messages/s",
        rate(report.stored, index)
    );
    println!(
        "- preparation wall time {:.0} ms (blob store {:.0} ms, parsing {:.0} ms, summed over {} workers); rendering or reading {:.0} ms",
        ms(report.prepare),
        ms(report.blob_time),
        ms(report.parse_time),
        options.workers,
        ms(report.read)
    );
    println!(
        "- store files {:.1} MiB; blob store {} files, {:.1} MiB ({:.1} MiB allocated)",
        report.store_bytes as f64 / 1_048_576.0,
        report.blobs.files,
        report.blobs.bytes as f64 / 1_048_576.0,
        report.blobs.allocated as f64 / 1_048_576.0
    );
    Ok(())
}

fn check(args: &Args) -> Result<()> {
    let store = sealwax_infrastructure::store::Store::open(
        &args.path("store")?,
        sealwax_infrastructure::store::StoreOptions::default(),
    )?;
    let problems = store.check().recv_blocking()?;
    store.close()?;
    if problems.is_empty() {
        println!("consistent");
        Ok(())
    } else {
        bail!("{}", problems.join("\n"))
    }
}

fn main() -> Result<()> {
    let args = Args::parse()?;
    match args.command.as_str() {
        "generate" => generate(&args),
        "import" => import(&args),
        "bench" => bench::run(&args.bench()?),
        "bench-unified" => unified::run(&args.bench()?),
        "check" => check(&args),
        "remove-account" => {
            let store = args.path("store")?;
            remove::run(&remove::Options {
                blobs: args.blobs(&store)?,
                store,
                account: args
                    .get("account")
                    .unwrap_or(import::ACCOUNT_NAME)
                    .to_owned(),
            })
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown command {other}\n\n{USAGE}"),
    }
}
