//! Imports `.eml` files, or a corpus generated on the fly, into a store through the production
//! ingest path (`ingest::prepare` + `ingest::submit`, the two halves of `ingest_raw`). Preparing
//! the next chunk (blob store and parsing, on several threads) overlaps with the writer storing
//! the previous one.

use crate::corpus::{Corpus, Folder};
use anyhow::{Context, Result, bail};
use sealwax_domain::model::{Flags, FolderId};
use sealwax_infrastructure::blobs::{BlobStore, DiskUsage};
use sealwax_infrastructure::ingest::{self, RawMessage};
use sealwax_infrastructure::store::{FolderSpec, Ingested, NewAccount, Reply, Store, StoreOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The account fixtures go into unless `--account` names another.
pub const ACCOUNT_NAME: &str = "Fixtures";

/// The fictional address of a fixture account.
pub fn account_address(name: &str) -> String {
    let local: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("{local}@example.test")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderMode {
    /// Everything in INBOX: one big folder.
    Single,
    /// INBOX, Sent, Archive, Lister, Junk, Trash and Drafts, as the corpus files them.
    Spread,
}

pub enum Source {
    Dir(PathBuf),
    Corpus(Corpus),
}

pub struct Options {
    pub account: String,
    pub store: PathBuf,
    pub blobs: PathBuf,
    pub folders: FolderMode,
    pub chunk: usize,
    pub workers: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    pub messages: usize,
    pub stored: usize,
    pub skipped: usize,
    pub elapsed: Duration,
    pub read: Duration,
    pub prepare: Duration,
    pub blob_time: Duration,
    pub parse_time: Duration,
    pub header_time: Duration,
    pub index_time: Duration,
    pub batches: u32,
    pub raw_bytes: u64,
    pub store_bytes: u64,
    pub blobs: DiskUsage,
}

/// Flags a server would report, derived from the raw bytes so any directory imports the same
/// way every time: about 30 % of INBOX unread, 2 % starred.
pub fn flags_for(raw: &[u8], folder: Folder) -> Flags {
    let hash = BlobStore::key(raw).0;
    let below = |byte: u8, percent: u32| u32::from(byte) * 100 < percent * 256;
    let unread_percent = match folder {
        Folder::Inbox => 30,
        Folder::Lists => 80,
        Folder::Junk => 85,
        Folder::Trash => 15,
        Folder::Archive => 8,
        Folder::Sent | Folder::Drafts => 0,
    };
    let mut flags = if below(hash[0], unread_percent) {
        Flags::NONE
    } else {
        Flags::SEEN
    };
    if matches!(folder, Folder::Inbox | Folder::Archive) && below(hash[1], 2) {
        flags = flags | Flags::FLAGGED;
    }
    if folder == Folder::Inbox && below(hash[2], 8) {
        flags = flags | Flags::ANSWERED;
    }
    if folder == Folder::Drafts {
        flags = flags | Flags::DRAFT;
    }
    flags
}

fn collect(reply: Reply<Ingested>, report: &mut Report) -> Result<()> {
    let ingested = reply.recv_blocking()?;
    report.stored += ingested.ids.iter().filter(|id| id.is_some()).count();
    report.skipped += ingested.ids.iter().filter(|id| id.is_none()).count();
    report.header_time += ingested.header_time;
    report.index_time += ingested.index_time;
    report.batches += ingested.batches;
    Ok(())
}

pub fn store_files(path: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect()
}

/// A fixture account and its folders, created when missing.
pub fn folders(store: &Store, name: &str, mode: FolderMode) -> Result<HashMap<Folder, FolderId>> {
    let address = account_address(name);
    let accounts = store.accounts().recv_blocking()?.value;
    let account = match accounts.iter().find(|account| account.address == address) {
        Some(account) => account.id,
        None => {
            store
                .add_account(NewAccount {
                    name: name.into(),
                    address,
                })
                .recv_blocking()?
                .value
        }
    };
    let wanted: &[Folder] = match mode {
        FolderMode::Single => &[Folder::Inbox],
        FolderMode::Spread => &Folder::ALL,
    };
    let mut ids = HashMap::new();
    for folder in wanted {
        let id = store
            .upsert_folder(FolderSpec::local(account, folder.name(), folder.role()))
            .recv_blocking()?
            .value;
        ids.insert(*folder, id);
    }
    Ok(ids)
}

fn eml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "eml"))
        .collect();
    files.sort();
    Ok(files)
}

/// The folder a generated file names (`000123-archive.eml`), else INBOX.
fn folder_of(path: &Path) -> Folder {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.split_once('-'))
        .and_then(|(_, slug)| Folder::from_slug(slug))
        .unwrap_or(Folder::Inbox)
}

pub fn run(source: Source, options: &Options) -> Result<Report> {
    let started = Instant::now();
    let blobs = BlobStore::open(&options.blobs)
        .with_context(|| format!("open the blob store {}", options.blobs.display()))?;
    let store = Store::open(
        &options.store,
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .with_context(|| format!("open the store {}", options.store.display()))?;
    let ids = folders(&store, &options.account, options.folders)?;
    let target = |folder: Folder| match options.folders {
        FolderMode::Single => ids[&Folder::Inbox],
        FolderMode::Spread => ids[&folder],
    };
    let (total, files) = match &source {
        Source::Dir(dir) => {
            let files = eml_files(dir)?;
            if files.is_empty() {
                bail!("no .eml files in {}", dir.display());
            }
            (files.len(), files)
        }
        Source::Corpus(corpus) => (corpus.len(), Vec::new()),
    };
    let mut report = Report {
        messages: total,
        ..Report::default()
    };
    let mut pending: Option<Reply<Ingested>> = None;
    for start in (0..total).step_by(options.chunk.max(1)) {
        let end = (start + options.chunk.max(1)).min(total);
        let read = Instant::now();
        let messages: Vec<RawMessage> = match &source {
            Source::Dir(_) => files[start..end]
                .iter()
                .map(|path| {
                    let raw =
                        std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
                    let folder = folder_of(path);
                    Ok(RawMessage {
                        folder: target(folder),
                        flags: flags_for(&raw, folder),
                        raw,
                        ..RawMessage::default()
                    })
                })
                .collect::<Result<_>>()?,
            Source::Corpus(corpus) => render_parallel(corpus, start..end, options.workers)
                .into_iter()
                .zip(start..end)
                .map(|(raw, index)| {
                    let folder = corpus.specs[index].folder;
                    RawMessage {
                        folder: target(folder),
                        flags: flags_for(&raw, folder),
                        raw,
                        ..RawMessage::default()
                    }
                })
                .collect(),
        };
        report.read += read.elapsed();
        report.raw_bytes += messages.iter().map(|m| m.raw.len() as u64).sum::<u64>();
        let prepared = ingest::prepare(&blobs, messages, options.workers)?;
        report.prepare += prepared.elapsed;
        report.blob_time += prepared.blob_time;
        report.parse_time += prepared.parse_time;
        if let Some(reply) = pending.take() {
            collect(reply, &mut report)?;
        }
        pending = Some(ingest::submit(&store, prepared));
    }
    if let Some(reply) = pending.take() {
        collect(reply, &mut report)?;
    }
    report.elapsed = started.elapsed();
    store.close()?;
    report.store_bytes = store_files(&options.store)
        .iter()
        .filter_map(|file| std::fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum();
    report.blobs = blobs.disk_usage()?;
    Ok(report)
}

/// Renders messages `range` of the corpus on up to `workers` threads, in order.
pub fn render_parallel(
    corpus: &Corpus,
    range: std::ops::Range<usize>,
    workers: usize,
) -> Vec<Vec<u8>> {
    let indexes: Vec<usize> = range.collect();
    let per = indexes.len().div_ceil(workers.max(1)).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = indexes
            .chunks(per)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|index| corpus.render(*index))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("render threads don't panic"))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{Config, DEFAULT_NOW};
    use sealwax_infrastructure::store::{OpenView, ViewSpec};

    fn scratch() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("fixtures-")
            .tempdir_in(base)
            .unwrap()
    }

    fn corpus(count: usize) -> Corpus {
        Corpus::generate(Config {
            count,
            seed: 9,
            now: DEFAULT_NOW,
            years: 2.0,
        })
    }

    #[test]
    fn imports_a_generated_directory_and_a_stream_alike() {
        let dir = scratch();
        let corpus = corpus(300);
        let files = dir.path().join("eml");
        std::fs::create_dir_all(&files).unwrap();
        for index in 0..corpus.len() {
            std::fs::write(files.join(corpus.file_name(index)), corpus.render(index)).unwrap();
        }
        let options = |name: &str| Options {
            account: ACCOUNT_NAME.into(),
            store: dir.path().join(name).join("store.sqlite"),
            blobs: dir.path().join(name).join("blobs"),
            folders: FolderMode::Spread,
            chunk: 64,
            workers: 4,
        };
        let from_files = run(Source::Dir(files.clone()), &options("a")).unwrap();
        let streamed = run(Source::Corpus(corpus), &options("b")).unwrap();
        assert_eq!(from_files.stored, 300);
        assert_eq!(streamed.stored, 300);
        assert_eq!(
            (from_files.blobs.files, from_files.blobs.bytes),
            (streamed.blobs.files, streamed.blobs.bytes)
        );
        let again = run(Source::Dir(files), &options("a")).unwrap();
        assert_eq!(
            (again.stored, again.skipped),
            (0, 300),
            "re-imports are skipped"
        );

        for name in ["a", "b"] {
            let store = Store::open(&options(name).store, StoreOptions::default()).unwrap();
            let folders = store.folders().recv_blocking().unwrap().value;
            assert_eq!(folders.len(), 7);
            assert_eq!(folders.iter().map(|f| f.total).sum::<u32>(), 300);
            assert_eq!(store.check().recv_blocking().unwrap(), Vec::<String>::new());
            let inbox = folders.iter().find(|f| f.name == "INBOX").unwrap();
            let opened = store
                .open_view(OpenView {
                    spec: ViewSpec::folder(inbox.id),
                    first_page: 50,
                    section_starts: Vec::new(),
                })
                .recv_blocking()
                .unwrap();
            assert_eq!(opened.total, inbox.total);
            assert!(opened.rows.iter().any(|row| row.thread_count > 1));
            store.close().unwrap();
        }
    }

    #[test]
    fn flags_follow_the_folder() {
        let corpus = corpus(400);
        let mut unread = 0;
        for index in 0..corpus.len() {
            let raw = corpus.render(index);
            assert_eq!(
                flags_for(&raw, Folder::Inbox),
                flags_for(&raw, Folder::Inbox)
            );
            assert!(flags_for(&raw, Folder::Sent).is_seen());
            unread += usize::from(!flags_for(&raw, Folder::Inbox).is_seen());
        }
        assert!((80..160).contains(&unread), "{unread}");
    }
}
