//! The reader's registry commands (M5): the window's actions that act on the message or the
//! conversation the reader shows. Each owner connects its own commands below; a command stays
//! unavailable, and so insensitive in the toolbar, the menu and the palette, until it does.

use super::Reader;
use crate::actions::ActionSet;

/// Connects the reader's commands to the window's actions, once the reader exists.
pub fn connect(actions: &ActionSet, reader: &Reader) {
    conversation(actions, reader);
    files(actions, reader);
    remote_content(actions, reader);
}

/// Expand All Messages and Collapse All Messages (ML-057).
fn conversation(_actions: &ActionSet, _reader: &Reader) {}

/// Print, Show Original and Save as .eml (ML-063).
fn files(_actions: &ActionSet, _reader: &Reader) {}

/// Show Images and Always Show Images from This Sender (ML-058).
fn remote_content(_actions: &ActionSet, _reader: &Reader) {}
