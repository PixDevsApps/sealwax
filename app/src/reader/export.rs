//! Print, Show Original and Save as .eml (ML-063) for the message the reader shows: printed
//! through WebKit's print dialog, the raw source in a read-only view, the `.eml` saved through
//! the file chooser portal. A message stored as its text parts (`BodyState::Text`) is fetched
//! whole first (`Command::FetchOriginal`). Built in M5 phase A.

use super::Reader;

pub(super) fn print(_reader: &Reader) {}

pub(super) fn show_original(_reader: &Reader) {}

pub(super) fn save_message(_reader: &Reader) {}
