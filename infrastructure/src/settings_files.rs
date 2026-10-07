//! Reading `config.toml` and `keys.toml` (ADR-009), and creating them with commented defaults
//! when the palette opens one that does not exist yet.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::xdg::ensure_private_dir;

/// Settings files larger than this are refused rather than read.
pub const MAX_BYTES: u64 = 1024 * 1024;

/// The text of a settings file, or `None` when there is none.
pub fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(meta) if meta.len() > MAX_BYTES => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("it is larger than {MAX_BYTES} bytes"),
            ));
        }
        Ok(_) => {}
    }
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "it is not UTF-8 text")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Writes `text` to `path` (mode 0600) unless the file exists, creating its directory (mode
/// 0700). Returns whether it wrote the file; an existing file is never touched.
pub fn create_if_missing(path: &Path, text: &str) -> io::Result<bool> {
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir)?;
    }
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xdg::SealwaxDirs;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch root inside the repository's `.local/`, never the user's folders.
    fn scratch() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.local/test-scratch");
        fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    #[test]
    fn missing_files_read_as_none() {
        let root = scratch();
        let dirs = SealwaxDirs::under(root.path());
        assert_eq!(read_optional(&dirs.config_file()).unwrap(), None);
        fs::create_dir_all(&dirs.config).unwrap();
        fs::write(dirs.config_file(), "[appearance]\n").unwrap();
        assert_eq!(
            read_optional(&dirs.config_file()).unwrap(),
            Some("[appearance]\n".to_owned())
        );
        fs::write(dirs.keys_file(), [0xff, 0xfe]).unwrap();
        assert!(read_optional(&dirs.keys_file()).is_err());
        let big = vec![b'#'; MAX_BYTES as usize + 1];
        fs::write(dirs.keys_file(), big).unwrap();
        assert_eq!(
            read_optional(&dirs.keys_file()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn creates_once_and_never_overwrites() {
        let root = scratch();
        let dirs = SealwaxDirs::under(root.path());
        let path = dirs.keys_file();
        assert!(create_if_missing(&path, "# defaults\n").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "# defaults\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dirs.config).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::write(&path, "quit = []\n").unwrap();
        assert!(!create_if_missing(&path, "# defaults\n").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "quit = []\n");
    }
}
