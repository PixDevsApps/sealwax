//! The window's layout between runs (the user's UI tweaks, 2026-10-08): the widths the user
//! dragged the sidebar and the message list to, the accounts folded in the sidebar, the
//! accounts whose "More folders" are open, and the folders whose subfolders it folds away (M9,
//! folders; the sidebar comes back as the user left it, their word on 2026-10-09), in
//! `$XDG_STATE_HOME/sealwax/layout.toml`. Sealwax writes it; it isn't a setting, so it is read
//! leniently: anything it doesn't know, or a width out of range, is left out and the default
//! applies.

use crate::account::AccountKey;
use crate::accounts_file::is_valid_key;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;
use toml::Value;

pub const LAYOUT_FILE: &str = "layout.toml";
/// The widths the sidebar and the message list can be dragged to, in px.
pub const SIDEBAR_WIDTHS: RangeInclusive<u32> = 160..=360;
pub const LIST_WIDTHS: RangeInclusive<u32> = 280..=640;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The full sidebar's width; `None` for the default.
    pub sidebar_width: Option<u32>,
    /// The message list's width beside the reader; `None` for the default.
    pub list_width: Option<u32>,
    /// The accounts whose folders the sidebar folds away, by key.
    pub collapsed: Vec<AccountKey>,
    /// The accounts whose "More folders" are open, by key.
    pub more_open: Vec<AccountKey>,
    /// The folders whose subfolders the sidebar folds away: each account's by key, by the
    /// folder's whole name.
    pub folded: BTreeMap<AccountKey, BTreeSet<String>>,
}

impl Layout {
    pub fn parse(text: &str) -> Self {
        let Ok(table) = text.parse::<toml::Table>() else {
            return Self::default();
        };
        let width = |name: &str, range: &RangeInclusive<u32>| {
            table
                .get(name)
                .and_then(Value::as_integer)
                .and_then(|width| u32::try_from(width).ok())
                .filter(|width| range.contains(width))
        };
        let keys = |name: &str| {
            let mut keys: Vec<AccountKey> = Vec::new();
            for key in table
                .get(name)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|key| is_valid_key(key))
            {
                if !keys.iter().any(|known| known.0 == key) {
                    keys.push(AccountKey(key.to_owned()));
                }
            }
            keys
        };
        let mut folded: BTreeMap<AccountKey, BTreeSet<String>> = BTreeMap::new();
        for (key, names) in table
            .get("folded")
            .and_then(Value::as_table)
            .into_iter()
            .flatten()
            .filter(|(key, _)| is_valid_key(key))
        {
            let names: BTreeSet<String> = names
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect();
            if !names.is_empty() {
                folded.insert(AccountKey(key.clone()), names);
            }
        }
        Self {
            sidebar_width: width("sidebar_width", &SIDEBAR_WIDTHS),
            list_width: width("list_width", &LIST_WIDTHS),
            collapsed: keys("collapsed"),
            more_open: keys("more_open"),
            folded,
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from(
            "# Sealwax keeps the window's layout here: the widths the sidebar and the message\n\
             # list were dragged to, and the accounts and folders folded or open in the sidebar.\n\
             # It isn't a setting; delete the file to go back to the defaults.\n",
        );
        if let Some(width) = self.sidebar_width {
            text.push_str(&format!("sidebar_width = {width}\n"));
        }
        if let Some(width) = self.list_width {
            text.push_str(&format!("list_width = {width}\n"));
        }
        let keys: Vec<String> = self
            .collapsed
            .iter()
            .map(|key| format!("\"{}\"", key.0))
            .collect();
        text.push_str(&format!("collapsed = [{}]\n", keys.join(", ")));
        if !self.more_open.is_empty() {
            let keys: Vec<String> = self
                .more_open
                .iter()
                .map(|key| format!("\"{}\"", key.0))
                .collect();
            text.push_str(&format!("more_open = [{}]\n", keys.join(", ")));
        }
        if !self.folded.is_empty() {
            text.push_str("\n[folded]\n");
            for (key, names) in &self.folded {
                let names: Vec<String> = names
                    .iter()
                    .map(|name| Value::String(name.clone()).to_string())
                    .collect();
                text.push_str(&format!("{} = [{}]\n", key.0, names.join(", ")));
            }
        }
        text
    }

    /// Whether the folder `name` of `key`'s account has its subfolders folded away.
    pub fn is_folded(&self, key: &AccountKey, name: &str) -> bool {
        self.folded
            .get(key)
            .is_some_and(|names| names.contains(name))
    }

    /// Folds the subfolders of `key`'s folder `name` away or opens them again; true when that
    /// changed anything.
    pub fn set_folded(&mut self, key: &AccountKey, name: &str, folded: bool) -> bool {
        if folded {
            return self
                .folded
                .entry(key.clone())
                .or_default()
                .insert(name.to_owned());
        }
        let Some(names) = self.folded.get_mut(key) else {
            return false;
        };
        let changed = names.remove(name);
        if names.is_empty() {
            self.folded.remove(key);
        }
        changed
    }

    /// Opens `key`'s account's "More folders" or closes them; true when that changed anything.
    pub fn set_more_open(&mut self, key: &AccountKey, open: bool) -> bool {
        let known = self.more_open.iter().position(|known| known == key);
        match (known, open) {
            (None, true) => self.more_open.push(key.clone()),
            (Some(index), false) => {
                self.more_open.remove(index);
            }
            _ => return false,
        }
        true
    }

    /// `key`'s folder `old` is now `new`: its fold and its subfolders' follow it. True when that
    /// changed anything.
    pub fn rename_folded(
        &mut self,
        key: &AccountKey,
        old: &str,
        new: &str,
        delimiter: Option<char>,
    ) -> bool {
        let Some(names) = self.folded.get_mut(key) else {
            return false;
        };
        let moved: Vec<(String, String)> = names
            .iter()
            .filter_map(|name| {
                crate::folders::renamed(name, old, new, delimiter).map(|to| (name.clone(), to))
            })
            .collect();
        for (from, to) in &moved {
            names.remove(from);
            names.insert(to.clone());
        }
        !moved.is_empty()
    }

    /// Folds `key`'s account away or opens it again; true when that changed anything.
    pub fn set_collapsed(&mut self, key: &AccountKey, collapsed: bool) -> bool {
        let known = self.collapsed.iter().position(|known| known == key);
        match (known, collapsed) {
            (None, true) => self.collapsed.push(key.clone()),
            (Some(index), false) => {
                self.collapsed.remove(index);
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_broken_file_is_the_default() {
        assert_eq!(Layout::parse(""), Layout::default());
        assert_eq!(Layout::parse("not toml ["), Layout::default());
    }

    #[test]
    fn round_trips() {
        let layout = Layout {
            sidebar_width: Some(240),
            list_width: Some(420),
            collapsed: vec![AccountKey("work".into()), AccountKey("k3j5x8m2q9wa".into())],
            more_open: vec![AccountKey("home".into())],
            folded: BTreeMap::from([(
                AccountKey("work".into()),
                BTreeSet::from([
                    "Kunder".to_owned(),
                    "INBOX/Prosjekter \"Høst\" 📬".to_owned(),
                    "a\\b".to_owned(),
                ]),
            )]),
        };
        assert_eq!(Layout::parse(&layout.to_text()), layout);
        assert_eq!(
            Layout::parse(&Layout::default().to_text()),
            Layout::default()
        );
    }

    #[test]
    fn what_doesnt_fit_is_left_out() {
        let layout = Layout::parse(
            "sidebar_width = 9999\nlist_width = \"wide\"\nother = 1\n\
             collapsed = [\"work\", 3, \"Not a key\", \"work\", \"home\"]\n",
        );
        assert_eq!(
            layout,
            Layout {
                sidebar_width: None,
                list_width: None,
                collapsed: vec![AccountKey("work".into()), AccountKey("home".into())],
                more_open: Vec::new(),
                folded: BTreeMap::new(),
            }
        );
        assert_eq!(
            Layout::parse("more_open = [\"work\", \"Not a key\", 4, \"work\"]").more_open,
            [AccountKey("work".into())]
        );
        let folded = Layout::parse(
            "[folded]\nwork = [\"Kunder\", 3, \"\"]\n\"Not a key\" = [\"Kunder\"]\nhome = []\n\
             club = \"Kunder\"\n",
        );
        assert_eq!(
            folded.folded,
            BTreeMap::from([(
                AccountKey("work".into()),
                BTreeSet::from(["Kunder".to_owned()])
            )])
        );
        assert_eq!(Layout::parse("list_width = 280").list_width, Some(280));
        assert_eq!(Layout::parse("list_width = 279").list_width, None);
    }

    #[test]
    fn folding_changes_the_list_once() {
        let mut layout = Layout::default();
        let work = AccountKey("work".into());
        assert!(layout.set_collapsed(&work, true));
        assert!(!layout.set_collapsed(&work, true));
        assert_eq!(layout.collapsed, std::slice::from_ref(&work));
        assert!(layout.set_collapsed(&work, false));
        assert!(!layout.set_collapsed(&work, false));
        assert!(layout.collapsed.is_empty());
        assert!(layout.set_folded(&work, "Kunder", true));
        assert!(!layout.set_folded(&work, "Kunder", true));
        assert!(layout.is_folded(&work, "Kunder"));
        assert!(!layout.is_folded(&work, "Kunder/Acme"));
        assert!(layout.set_folded(&work, "Kunder", false));
        assert!(!layout.set_folded(&work, "Kunder", false));
        assert!(
            layout.folded.is_empty(),
            "an account without folds leaves the table"
        );
    }

    #[test]
    fn more_folders_open_and_closed() {
        let mut layout = Layout::default();
        let work = AccountKey("work".into());
        assert!(layout.set_more_open(&work, true));
        assert!(!layout.set_more_open(&work, true));
        assert!(layout.to_text().contains("more_open = [\"work\"]\n"));
        assert!(layout.set_more_open(&work, false));
        assert!(!layout.to_text().contains("more_open"));
    }

    #[test]
    fn a_renamed_folder_keeps_its_fold_and_its_subfolders() {
        let work = AccountKey("work".into());
        let mut layout = Layout::default();
        layout.set_folded(&work, "INBOX.Kunder", true);
        layout.set_folded(&work, "INBOX.Kunder.Acme", true);
        layout.set_folded(&work, "INBOX.Kunder2", true);
        assert!(layout.rename_folded(&work, "INBOX.Kunder", "INBOX.Klienter", Some('.')));
        assert_eq!(
            layout.folded[&work],
            BTreeSet::from([
                "INBOX.Klienter".to_owned(),
                "INBOX.Klienter.Acme".to_owned(),
                "INBOX.Kunder2".to_owned(),
            ])
        );
        assert!(!layout.rename_folded(&work, "Nowhere", "Else", Some('.')));
        assert!(!layout.rename_folded(
            &AccountKey("home".into()),
            "INBOX.Klienter",
            "X",
            Some('.')
        ));
    }
}
