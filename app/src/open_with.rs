//! Open With (issue #39, ADR-031): `config.toml`'s `[[open_with]]` entries as commands on the
//! message the reader shows. Each entry makes its `open-with-N` action available, titled "Open
//! With" and its name in the palette, and puts an item in the reader's More menu; with none,
//! nothing is shown. They follow `config.toml` whenever it is saved (`preferences`). In a Flatpak
//! the sandbox can't start the user's programs, so none is offered there. The reader writes the
//! file and starts the command (`reader::open_with`).

use crate::application::AppState;
use sealwax_domain::open_with::{MAX_ENTRIES, OpenWith};
use sealwax_infrastructure::sandbox;
use sealwax_ui_model::actions::ActionId;

/// The entries offered: the configured ones, none in a Flatpak.
fn offered(configured: Vec<OpenWith>, flatpak: bool) -> Vec<OpenWith> {
    if flatpak { Vec::new() } else { configured }
}

fn entries(state: &AppState) -> Vec<OpenWith> {
    let configured = state
        .preferences()
        .map(|prefs| prefs.config().open_with)
        .unwrap_or_default();
    offered(configured, sandbox::is_flatpak())
}

/// Hands the entries offered to the reader and makes one command available per entry.
pub(crate) fn apply(state: &AppState) {
    let entries = entries(state);
    for number in 1..=MAX_ENTRIES {
        if let Some(id) = ActionId::open_with(number) {
            state.set_window_available(id, number <= entries.len());
        }
    }
    if let Some(reader) = state.reader() {
        reader.set_open_with(entries);
    }
}

/// The commands' titles in the palette: "Open With" and each entry's name.
pub(crate) fn titles(state: &AppState) -> Vec<(ActionId, String)> {
    entries(state)
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| Some((ActionId::open_with(index + 1)?, entry.title())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flatpak_offers_none() {
        let configured = vec![OpenWith {
            name: "Archive".to_owned(),
            command: vec!["my-archiver".to_owned(), "{file}".to_owned()],
        }];
        assert_eq!(offered(configured.clone(), false), configured);
        assert!(offered(configured, true).is_empty());
    }
}
