//! The unread count for other programs (1.5.0, issue #17): while Sealwax runs it keeps
//! `unread.json` in its runtime folder ([`crate::xdg::SealwaxDirs::unread_file`]) up to date,
//! written atomically (0600 in a 0700 folder), and removes it when it quits, so a bar widget or a
//! script can show the count without reading the mail store and without starting Sealwax:
//!
//! ```json
//! {"version":1,"pid":4242,"unread":12,"accounts":[{"name":"Work","unread":9},{"name":"Home","unread":3}]}
//! ```
//!
//! `unread` is All Inboxes' number, the accounts' Inboxes as the sidebar counts them; an account
//! is named by its label, never by its address. `sealwax --unread` prints the number
//! ([`read_running`]). The folder is the user's own and gone at logout; a file left by a crash
//! names a process that is gone, which [`read_running`] checks outside a Flatpak.

use crate::accounts::replace;
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::Path;

pub const UNREAD_FILE: &str = "unread.json";

/// The file's format; a reader skips a file of a newer one.
pub const VERSION: u64 = 1;

/// The numbers the file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnreadCount {
    /// Unread messages in every account's Inbox.
    pub total: u32,
    /// Each account's, by its label, in the sidebar's order.
    pub accounts: Vec<(String, u32)>,
}

impl UnreadCount {
    /// The file's text for the process `pid`.
    pub fn to_json(&self, pid: u32) -> String {
        let accounts: Vec<Value> = self
            .accounts
            .iter()
            .map(|(name, unread)| json!({ "name": name, "unread": unread }))
            .collect();
        let mut text = json!({
            "version": VERSION,
            "pid": pid,
            "unread": self.total,
            "accounts": accounts,
        })
        .to_string();
        text.push('\n');
        text
    }
}

/// Writes `count` for the process `pid` to `path`, replacing what was there.
pub fn write(path: &Path, count: &UnreadCount, pid: u32) -> io::Result<()> {
    replace(path, &count.to_json(pid))
}

/// Removes the file; one already gone is fine.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The unread number in `text`, the file's contents, with the process that wrote it; `None` for
/// a file this version can't read.
pub fn parse(text: &str) -> Option<(u32, u32)> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("version")?.as_u64()? != VERSION {
        return None;
    }
    let unread = u32::try_from(value.get("unread")?.as_u64()?).ok()?;
    let pid = u32::try_from(value.get("pid")?.as_u64()?).ok()?;
    Some((unread, pid))
}

/// The unread number of a Sealwax running now, from `path`; `None` when none runs (no file, a
/// file this version can't read, or, with `check_process`, one whose process is gone).
pub fn read_running(path: &Path, check_process: bool) -> Option<u32> {
    let text = fs::read_to_string(path).ok()?;
    let (unread, pid) = parse(&text)?;
    if check_process && !Path::new("/proc").join(pid.to_string()).exists() {
        return None;
    }
    Some(unread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xdg::SealwaxDirs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writes_privately_reads_back_and_goes() {
        let root = crate::store::unit_tests::scratch();
        let path = SealwaxDirs::under(root.path()).unread_file().unwrap();
        assert_eq!(read_running(&path, true), None, "no file: not running");
        let count = UnreadCount {
            total: 12,
            accounts: vec![("Jobb \"Ø\"".to_owned(), 9), ("Home".to_owned(), 3)],
        };
        write(&path, &count, std::process::id()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["accounts"][0]["name"], "Jobb \"Ø\"");
        assert_eq!(value["accounts"][1]["unread"], 3);
        assert_eq!(read_running(&path, true), Some(12));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir = fs::metadata(path.parent().unwrap()).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert_eq!(read_running(&path, true), None);
    }

    #[test]
    fn a_file_from_a_gone_process_or_another_version_is_not_read() {
        let root = crate::store::unit_tests::scratch();
        let path = SealwaxDirs::under(root.path()).unread_file().unwrap();
        // Above the kernel's pid_max (4,194,304), so no process has it.
        write(&path, &UnreadCount::default(), 4_194_305).unwrap();
        assert_eq!(read_running(&path, true), None);
        assert_eq!(
            read_running(&path, false),
            Some(0),
            "in a Flatpak: not checked"
        );
        for text in [
            "",
            "{",
            r#"{"version":2,"pid":1,"unread":1}"#,
            r#"{"version":1,"pid":1,"unread":-1}"#,
            r#"{"version":1,"unread":1}"#,
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
        assert_eq!(parse(r#"{"version":1,"pid":7,"unread":3}"#), Some((3, 7)));
    }
}
