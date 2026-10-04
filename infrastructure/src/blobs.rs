//! Raw messages, content-addressed (ADR-005): `<root>/ab/cd/<sha-256 in hex>`. Directories are
//! 0700 and files 0600. A write goes to a temporary file, is synced, renamed into place and the
//! directory synced, so after a crash a name holds the whole message or nothing. Identical
//! content is stored once.

use crate::xdg::ensure_private_dir;
use sealwax_domain::model::BlobKey;
use sha2::{Digest, Sha256};
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const TEMP_DIR: &str = "tmp";
/// Temporary files older than this are left over from a crash and removed when the store opens.
const STALE_TEMP: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct BlobStore {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    root: PathBuf,
    temp: PathBuf,
    counter: AtomicU64,
}

/// What the blob store holds on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub files: u64,
    /// The messages' lengths.
    pub bytes: u64,
    /// Space the file system allocated for them (after its own compression, if any).
    pub allocated: u64,
}

impl BlobStore {
    /// Opens or creates the store at `root` and removes stale temporary files.
    pub fn open(root: &Path) -> io::Result<BlobStore> {
        ensure_private_dir(root)?;
        let temp = root.join(TEMP_DIR);
        ensure_private_dir(&temp)?;
        let now = SystemTime::now();
        for entry in fs::read_dir(&temp)? {
            let entry = entry?;
            let stale = entry
                .metadata()?
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > STALE_TEMP);
            if stale {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(BlobStore {
            inner: Arc::new(Inner {
                root: root.to_owned(),
                temp,
                counter: AtomicU64::new(0),
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The key `bytes` are stored under.
    pub fn key(bytes: &[u8]) -> BlobKey {
        BlobKey(Sha256::digest(bytes).into())
    }

    pub fn path(&self, key: &BlobKey) -> PathBuf {
        let hex = key.to_hex();
        self.inner.root.join(&hex[..2]).join(&hex[2..4]).join(hex)
    }

    /// Stores `bytes` durably and returns their key; content already present is not rewritten.
    /// Safe to call from several threads at once.
    pub fn put(&self, bytes: &[u8]) -> io::Result<BlobKey> {
        let key = Self::key(bytes);
        let path = self.path(&key);
        if path.exists() {
            return Ok(key);
        }
        let dir = path.parent().expect("blob paths have a directory");
        self.create_dirs(dir)?;
        let temp = self.inner.temp.join(format!(
            "{}.{}.{}",
            &key.to_hex()[..16],
            std::process::id(),
            self.inner.counter.fetch_add(1, Ordering::Relaxed)
        ));
        let written = write_synced(&temp, bytes).and_then(|()| fs::rename(&temp, &path));
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        sync_dir(dir)?;
        Ok(key)
    }

    /// The message stored under `key`; `NotFound` if there is none.
    pub fn get(&self, key: &BlobKey) -> io::Result<Vec<u8>> {
        fs::read(self.path(key))
    }

    pub fn contains(&self, key: &BlobKey) -> bool {
        self.path(key).is_file()
    }

    /// Removes the message stored under `key`; returns whether there was one.
    pub fn remove(&self, key: &BlobKey) -> io::Result<bool> {
        match fs::remove_file(self.path(key)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Walks the store and adds up what it holds.
    pub fn disk_usage(&self) -> io::Result<DiskUsage> {
        let mut usage = DiskUsage::default();
        for first in fs::read_dir(&self.inner.root)? {
            let first = first?;
            if first.file_name() == TEMP_DIR || !first.file_type()?.is_dir() {
                continue;
            }
            for second in fs::read_dir(first.path())? {
                let second = second?;
                if !second.file_type()?.is_dir() {
                    continue;
                }
                for file in fs::read_dir(second.path())? {
                    let metadata = file?.metadata()?;
                    if metadata.is_file() {
                        usage.files += 1;
                        usage.bytes += metadata.len();
                        usage.allocated += metadata.blocks() * 512;
                    }
                }
            }
        }
        Ok(usage)
    }

    /// Creates the two fan-out levels above a blob, syncing each parent that gains an entry.
    fn create_dirs(&self, dir: &Path) -> io::Result<()> {
        if dir.is_dir() {
            return Ok(());
        }
        let parent = dir.parent().expect("fan-out directories have a parent");
        for (path, above) in [(parent, self.inner.root.as_path()), (dir, parent)] {
            match DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {
                    fs::set_permissions(path, Permissions::from_mode(0o700))?;
                    sync_dir(above)?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_data()
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn stores_once_and_reads_back() {
        let dir = crate::store::tests::scratch();
        let root = dir.path().join("blobs");
        let blobs = BlobStore::open(&root).unwrap();
        let first = blobs.put(b"From: a@example.test\r\n\r\nhei\r\n").unwrap();
        let again = blobs.put(b"From: a@example.test\r\n\r\nhei\r\n").unwrap();
        let other = blobs.put(b"From: b@example.test\r\n\r\nhallo\r\n").unwrap();
        assert_eq!(first, again);
        assert_ne!(first, other);
        assert_eq!(
            first,
            BlobStore::key(b"From: a@example.test\r\n\r\nhei\r\n")
        );
        assert_eq!(
            blobs.get(&first).unwrap(),
            b"From: a@example.test\r\n\r\nhei\r\n"
        );
        let hex = first.to_hex();
        assert_eq!(
            blobs.path(&first),
            root.join(&hex[..2]).join(&hex[2..4]).join(&hex)
        );
        let usage = blobs.disk_usage().unwrap();
        assert_eq!(usage.files, 2);
        assert_eq!(usage.bytes, 29 + 31);
        assert!(blobs.contains(&first));
        assert!(blobs.remove(&first).unwrap());
        assert!(!blobs.remove(&first).unwrap());
        assert!(!blobs.contains(&first));
        assert_eq!(
            blobs.get(&first).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs::read_dir(root.join(TEMP_DIR)).unwrap().count(), 0);
    }

    #[test]
    fn keys_are_sha256() {
        assert_eq!(
            BlobStore::key(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn directories_are_0700_and_files_0600() {
        let dir = crate::store::tests::scratch();
        let root = dir.path().join("data/blobs");
        let blobs = BlobStore::open(&root).unwrap();
        let key = blobs.put(b"private").unwrap();
        let path = blobs.path(&key);
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join(TEMP_DIR)), 0o700);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(path.parent().unwrap().parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn removes_stale_temporary_files_on_open() {
        let dir = crate::store::tests::scratch();
        let root = dir.path().join("blobs");
        BlobStore::open(&root).unwrap();
        let stale = root.join(TEMP_DIR).join("stale");
        let fresh = root.join(TEMP_DIR).join("fresh");
        fs::write(&stale, b"half a mess").unwrap();
        fs::write(&fresh, b"being written").unwrap();
        File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        BlobStore::open(&root).unwrap();
        assert!(!stale.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn concurrent_writers_agree() {
        let dir = crate::store::tests::scratch();
        let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
        let keys: Vec<Vec<BlobKey>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let blobs = blobs.clone();
                    scope.spawn(move || {
                        (0..50)
                            .map(|n| blobs.put(format!("message {n}").as_bytes()).unwrap())
                            .collect()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(blobs.disk_usage().unwrap().files, 50);
        for (n, key) in keys[0].iter().enumerate() {
            assert_eq!(blobs.get(key).unwrap(), format!("message {n}").as_bytes());
        }
    }
}
