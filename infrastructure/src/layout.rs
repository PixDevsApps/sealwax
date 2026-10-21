//! `layout.toml` on disk (`domain::layout`): read leniently, written atomically (0600 in a 0700
//! folder) by the same writer as `accounts.toml`. A file that can't be read is the default
//! layout; nothing in it is ever logged.

use crate::accounts::replace;
use crate::settings_files::read_optional;
use sealwax_domain::layout::Layout;
use std::io;
use std::path::Path;

/// The layout in `path`; the default when it is missing or can't be read.
pub fn read(path: &Path) -> Layout {
    match read_optional(path) {
        Ok(Some(text)) => Layout::parse(&text),
        Ok(None) => Layout::default(),
        Err(error) => {
            tracing::warn!(%error, "the window's layout can't be read; using the defaults");
            Layout::default()
        }
    }
}

pub fn write(path: &Path, layout: &Layout) -> io::Result<()> {
    replace(path, &layout.to_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xdg::SealwaxDirs;
    use sealwax_domain::account::AccountKey;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writes_privately_and_reads_back() {
        let root = crate::store::tests::scratch();
        let path = SealwaxDirs::under(root.path()).layout_file();
        assert_eq!(read(&path), Layout::default());
        let layout = Layout {
            sidebar_width: Some(250),
            list_width: None,
            sidebar_hidden: false,
            collapsed: vec![AccountKey("work".into())],
            more_open: vec![AccountKey("work".into())],
            folded: Default::default(),
            view: Some(sealwax_domain::layout::View::Folder {
                account: AccountKey("work".into()),
                name: "INBOX".into(),
            }),
        };
        write(&path, &layout).unwrap();
        assert_eq!(read(&path), layout);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(read(&path), Layout::default(), "not UTF-8: the defaults");
    }
}
