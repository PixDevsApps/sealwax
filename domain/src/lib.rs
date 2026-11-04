//! Pure mail logic: no I/O, no GTK, no threads.

#![forbid(unsafe_code)]
#![warn(clippy::unwrap_used)]

pub mod account;
pub mod accounts_file;
pub mod address;
pub mod appearance;
pub mod attachment;
pub mod autoconfig;
pub mod bodies;
pub mod compose;
pub mod config;
pub mod flowed;
pub mod folders;
#[cfg(any(test, feature = "fuzz"))]
#[doc(hidden)]
pub mod fuzz;
pub mod html_depth;
pub mod html_scan;
pub mod links;
pub mod mailto;
pub mod message;
pub mod model;
pub mod percent;
pub mod quotes;
pub mod reading;
pub mod search;
pub mod senders;
pub mod settings_file;
pub mod sieve;
pub mod snippet;
pub mod support;
pub mod sync;
pub mod triage;
pub mod unsubscribe;
pub mod updates;
