//! The reader's registry commands (M5): the window's actions that act on the message or the
//! conversation the reader shows. Each owner connects its own commands below; a command stays
//! unavailable, and so insensitive in the toolbar, the menu and the palette, until it does.

use super::Reader;
use crate::actions::ActionSet;
use sealwax_domain::actions::ActionId;

/// Connects the reader's commands to the window's actions, once the reader exists.
pub fn connect(actions: &ActionSet, reader: &Reader) {
    conversation(actions, reader);
    files(actions, reader);
    remote_content(actions, reader);
}

/// Expand All Messages and Collapse All Messages (ML-057), available while the shown
/// conversation has more than one message.
fn conversation(actions: &ActionSet, reader: &Reader) {
    for (id, run) in [
        (ActionId::ExpandAll, Reader::expand_all as fn(&Reader)),
        (ActionId::CollapseAll, Reader::collapse_all),
    ] {
        let target = reader.clone();
        actions.connect(id, move || run(&target));
        if let Some(action) = actions.action(id).cloned() {
            action.set_enabled(reader.state().messages > 1);
            reader.connect_size(move |size| action.set_enabled(size > 1));
        }
    }
}

/// Print, Show Original and Save as .eml (ML-063).
fn files(_actions: &ActionSet, _reader: &Reader) {}

/// Show Images and Always Show Images from This Sender (ML-058).
fn remote_content(_actions: &ActionSet, _reader: &Reader) {}
