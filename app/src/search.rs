//! Search beyond the list's field (ML-081, ML-082): Rebuild Search Index and its progress.
//! The search field itself lives in the list (`list::MessageList`).
//!
//! M7's contract: installed at startup and empty until phase A builds it.

use crate::application::AppState;
use std::rc::Rc;

/// Installs the search commands, once at startup.
pub fn install(state: &Rc<AppState>) {
    let _ = state;
}
