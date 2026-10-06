//! Background mode (ML-084, PRD D6, ADR-013 and its M7 amendment): on by default, closing the
//! window keeps Sealwax running so sync and notifications go on, with a one-time notice the
//! first time; Quit stops it. Also Start Sealwax at Login (opt-in, asks first) and the engine
//! started without a window.
//!
//! M7's contract: installed at startup and empty until phase A builds it.

use crate::application::{AppState, SealwaxApplication};
use std::rc::Rc;

/// Follows `[background]` and installs Start Sealwax at Login, once at startup.
pub fn install(app: &SealwaxApplication, state: &Rc<AppState>) {
    let _ = (app, state);
}
