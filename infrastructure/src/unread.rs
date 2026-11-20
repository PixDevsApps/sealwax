//! The unread count for other programs (1.5.0, issue #17): while Sealwax runs it keeps
//! `unread.json` in its runtime folder ([`crate::xdg::SealwaxDirs::unread_file`]) up to date,
//! written atomically (0600 in a 0700 folder), and removes it when it quits, so a bar widget or a
//! script can show the count without reading the mail store and without starting Sealwax:
//!
//! ```json
//! {"version":1,"pid":4242,"unread":12,"accounts":[{"name":"Work","unread":9,"folders":[
//!   {"key":"folder-3","name":"Inbox","parent":"","unread":7},
//!   {"key":"folder-9","name":"Bank","parent":"Inbox","unread":2}]},
//!  {"name":"Home","unread":3,"folders":[{"key":"folder-21","name":"Inbox","parent":"","unread":3}]}]}
//! ```
//!
//! `unread` counts each account's Inbox, the folders under it and the user's own folders, not
//! Sent, Drafts, Archive, Junk or Trash (1.6.3, ADR-029 amendment). An account is named by its
//! label, never by its address; `folders` lists its counted folders that have unread mail, in
//! the sidebar's order, each by its sidebar key (what `sealwax --folder` takes), its name and its
//! parents' names. Fields were only added, so the `version` stays 1. `sealwax --unread` prints
//! the number ([`read_running`]). The folder is the user's own and gone at logout; a file left by
//! a crash names a process that is gone, which [`read_running`] checks outside a Flatpak.

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
    /// Unread messages in every account's counted folders.
    pub total: u32,
    /// Each account's, in the sidebar's order.
    pub accounts: Vec<AccountUnread>,
}

/// One account's unread mail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountUnread {
    /// Its label, or "Account N"; never its address.
    pub name: String,
    pub unread: u32,
    /// Its counted folders with unread mail, in the sidebar's order.
    pub folders: Vec<FolderUnread>,
}

/// A folder with unread mail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderUnread {
    /// Its sidebar key (`folder-42`), which `sealwax --folder` opens.
    pub key: String,
    /// The name the sidebar shows.
    pub name: String,
    /// The names of the folders it is under, outermost first, apart by `/`; empty at the top.
    pub parent: String,
    pub unread: u32,
}

impl UnreadCount {
    /// The file's text for the process `pid`.
    pub fn to_json(&self, pid: u32) -> String {
        let accounts: Vec<Value> = self
            .accounts
            .iter()
            .map(|account| {
                let folders: Vec<Value> = account
                    .folders
                    .iter()
                    .map(|folder| {
                        json!({
                            "key": folder.key,
                            "name": folder.name,
                            "parent": folder.parent,
                            "unread": folder.unread,
                        })
                    })
                    .collect();
                json!({ "name": account.name, "unread": account.unread, "folders": folders })
            })
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
            accounts: vec![
                AccountUnread {
                    name: "Jobb \"Ø\"".to_owned(),
                    unread: 9,
                    folders: vec![
                        FolderUnread {
                            key: "folder-3".to_owned(),
                            name: "Inbox".to_owned(),
                            parent: String::new(),
                            unread: 7,
                        },
                        FolderUnread {
                            key: "folder-9".to_owned(),
                            name: "Bank".to_owned(),
                            parent: "Inbox".to_owned(),
                            unread: 2,
                        },
                    ],
                },
                AccountUnread {
                    name: "Home".to_owned(),
                    unread: 3,
                    folders: Vec::new(),
                },
            ],
        };
        write(&path, &count, std::process::id()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 1, "fields were only added");
        assert_eq!(value["accounts"][0]["name"], "Jobb \"Ø\"");
        assert_eq!(value["accounts"][0]["folders"][1]["key"], "folder-9");
        assert_eq!(value["accounts"][0]["folders"][1]["parent"], "Inbox");
        assert_eq!(value["accounts"][0]["folders"][1]["unread"], 2);
        assert_eq!(value["accounts"][1]["unread"], 3);
        assert_eq!(value["accounts"][1]["folders"], json!([]));
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
