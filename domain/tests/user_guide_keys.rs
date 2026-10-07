//! The user guide's keyboard shortcut table (`docs/USER_GUIDE.md`, between the keymap markers)
//! is the registry's default keymap, so the guide can't drift from the keys Sealwax installs: a
//! command, title or key changed in `domain::actions` fails here until the guide's table is
//! replaced with `keymap::markdown_table()`.

use sealwax_domain::actions::ActionId;
use sealwax_domain::keymap::{Keymap, TABLE_END, TABLE_START, markdown_table};

const GUIDE: &str = include_str!("../../docs/USER_GUIDE.md");

/// The table between the markers, without them.
fn table() -> &'static str {
    let start = GUIDE
        .find(TABLE_START)
        .expect("docs/USER_GUIDE.md has the keymap table's start marker")
        + TABLE_START.len();
    let end = GUIDE[start..]
        .find(TABLE_END)
        .expect("docs/USER_GUIDE.md has the keymap table's end marker")
        + start;
    GUIDE[start..end].trim()
}

/// The table's rows below its header, as trimmed cells.
fn rows() -> Vec<Vec<&'static str>> {
    table()
        .lines()
        .skip(2)
        .map(|line| {
            line.trim()
                .trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(str::trim)
                .collect()
        })
        .collect()
}

#[test]
fn the_guide_shows_the_registry_keymap() {
    assert_eq!(
        table(),
        markdown_table().trim(),
        "docs/USER_GUIDE.md's keymap table differs from the registry; replace it with \
         sealwax_domain::keymap::markdown_table()"
    );
}

#[test]
fn every_command_has_one_row_with_its_default_keys() {
    let keymap = Keymap::default();
    let rows = rows();
    assert_eq!(rows.len(), ActionId::ALL.len(), "one row per command");
    for (row, id) in rows.iter().zip(ActionId::ALL) {
        assert_eq!(row.len(), 5, "{id:?}: {row:?}");
        assert_eq!(row[0], id.category().label(), "{id:?}");
        assert_eq!(row[1], id.title(), "{id:?}");
        let keys = keymap.display(id);
        let keys = if keys.is_empty() {
            "—"
        } else {
            keys.as_str()
        };
        assert_eq!(row[2], keys, "{id:?}");
        assert_eq!(row[4], format!("`{}`", id.name()), "{id:?}");
    }
}
