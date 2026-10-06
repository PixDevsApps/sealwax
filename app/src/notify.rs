//! New-mail notifications (ML-083, PRD D5, ADR-013 and its M7 amendment): the engine's
//! `Event::NewMail` becomes a `gio::Notification` showing the sender and the subject (or only a
//! count, `[notifications] show = "count"`), whose default action opens that message with or
//! without a window; Archive and Mark read where the server shows buttons.
//!
//! M7's contract: installed at startup and empty until phase A builds it.

use crate::application::AppState;
use std::rc::Rc;

/// Follows the engine's new mail and `[notifications]`, once at startup.
pub fn install(state: &Rc<AppState>) {
    let _ = state;
}
