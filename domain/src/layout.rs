//! The window's layout between runs (the user's UI tweaks, 2026-10-08): the widths the user
//! dragged the sidebar and the message list to, and the accounts folded in the sidebar, in
//! `$XDG_STATE_HOME/sealwax/layout.toml`. Sealwax writes it; it isn't a setting, so it is read
//! leniently: anything it doesn't know, or a width out of range, is left out and the default
//! applies.

use crate::account::AccountKey;
use crate::accounts_file::is_valid_key;
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
        let mut collapsed: Vec<AccountKey> = Vec::new();
        for key in table
            .get("collapsed")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|key| is_valid_key(key))
        {
            if !collapsed.iter().any(|known| known.0 == key) {
                collapsed.push(AccountKey(key.to_owned()));
            }
        }
        Self {
            sidebar_width: width("sidebar_width", &SIDEBAR_WIDTHS),
            list_width: width("list_width", &LIST_WIDTHS),
            collapsed,
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from(
            "# Sealwax keeps the window's layout here: the widths the sidebar and the message\n\
             # list were dragged to, and the accounts folded in the sidebar. It isn't a setting;\n\
             # delete the file to go back to the defaults.\n",
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
        text
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
            }
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
    }
}
