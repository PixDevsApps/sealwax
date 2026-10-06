//! The outbox in the main window (M6 phase B, ML-076): the toast with Undo that follows a draft
//! queued to send, from a composer window or the quick reply, and what the window says about a
//! send that failed or may have gone out.

use crate::application::AppState;
use sealwax_domain::model::{AccountId, DraftId};

/// `draft` was queued to send with the undo window: the main window says so, with Undo, which
/// takes it back to the composer while it hasn't started. Built in phase B; until then nothing
/// is shown.
pub fn queued(_state: &AppState, _draft: DraftId, _account: AccountId) {}
