//! "Cold-ish" starts without root: `posix_fadvise(POSIX_FADV_DONTNEED)` drops a file's clean
//! pages from the page cache, except pages some process has mapped (shared libraries that other
//! running programs use stay resident). `mincore` reports how much of a file is resident, so a
//! run can say how cold it really was.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default)]
pub struct Residency {
    pub bytes: u64,
    pub resident_before: u64,
    pub resident_after: u64,
}

impl Residency {
    pub fn add(&mut self, other: Residency) {
        self.bytes += other.bytes;
        self.resident_before += other.resident_before;
        self.resident_after += other.resident_after;
    }
}

fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(size).unwrap_or(4096)
}

/// Resident bytes of `file` according to `mincore` over a temporary read-only mapping.
pub fn resident(file: &File, len: u64) -> std::io::Result<u64> {
    if len == 0 {
        return Ok(0);
    }
    let page = page_size();
    let length = usize::try_from(len).map_err(std::io::Error::other)?;
    // SAFETY: a read-only mapping of a file we hold open, unmapped before returning.
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    let pages = len.div_ceil(page) as usize;
    let mut vector = vec![0u8; pages];
    // SAFETY: `vector` has one byte per page of the mapping, as mincore requires.
    let rc = unsafe { libc::mincore(address, length, vector.as_mut_ptr()) };
    let error = std::io::Error::last_os_error();
    // SAFETY: unmaps exactly the mapping created above.
    unsafe { libc::munmap(address, length) };
    if rc != 0 {
        return Err(error);
    }
    Ok(vector.iter().filter(|byte| **byte & 1 == 1).count() as u64 * page)
}

/// Drops the file's pages from the page cache where the kernel allows it.
pub fn evict(path: &Path) -> std::io::Result<Residency> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let resident_before = resident(&file, len)?;
    // SAFETY: plain advice on an open descriptor; offset 0 and length 0 mean the whole file.
    let rc = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc));
    }
    Ok(Residency {
        bytes: len,
        resident_before,
        resident_after: resident(&file, len)?,
    })
}

pub fn measure(path: &Path) -> std::io::Result<Residency> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let resident = resident(&file, len)?;
    Ok(Residency {
        bytes: len,
        resident_before: resident,
        resident_after: resident,
    })
}

/// Regular files mapped into a process, from `/proc/<pid>/maps` (shared libraries, fonts, icon
/// caches, the executable).
pub fn mapped_files(maps: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = maps
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .filter(|path| {
            path.starts_with('/') && !path.starts_with("/dev/") && !path.starts_with("/memfd:")
        })
        .map(PathBuf::from)
        .collect();
    files.sort();
    files.dedup();
    files.retain(|path| path.is_file());
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn evicts_a_file_we_wrote() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![7u8; 1 << 20]).unwrap();
        file.as_file().sync_all().unwrap();
        let residency = evict(file.path()).unwrap();
        assert_eq!(residency.bytes, 1 << 20);
        assert!(residency.resident_after <= residency.resident_before);
    }

    #[test]
    fn parses_maps() {
        let exe = std::env::current_exe().unwrap();
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let files = mapped_files(&maps);
        assert!(files.contains(&exe), "{files:?}");
        assert!(files.iter().any(|f| f.to_string_lossy().contains("libc")));
    }
}
