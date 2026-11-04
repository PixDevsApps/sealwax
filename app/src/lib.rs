//! The GTK application: windows, widgets, theme, actions and the self-test runner.
//! `main.rs` only calls [`main`], as `sealwax-selftest` (the `uitest` crate) does.
//!
//! The modules are the crate's own. With the `test-hooks` feature, which only `uitest` turns on,
//! the self-test runner, the sample mail and the stand-in composer backend are built in, and the
//! modules the harnesses in `uitest/examples` drive are public; the released `sealwax` has none
//! of them (ADR-025 amendment).

#![warn(clippy::unwrap_used)]
// Items outside the harness modules are the crate's own: `pub(crate)`, so rustc reports the dead
// ones. Checked in the `test-hooks` build, where the harness modules are public.
#![cfg_attr(feature = "test-hooks", warn(unreachable_pub))]
#![allow(
    rustdoc::private_intra_doc_links,
    reason = "the app's docs are for its contributors and name its own items; with test-hooks the \
              harness modules are public and their docs still link the private ones"
)]

/// A module the harnesses use: public with `test-hooks`, the crate's own without.
macro_rules! harness_mod {
    ($($name:ident),* $(,)?) => {
        $(
            #[cfg(feature = "test-hooks")]
            pub mod $name;
            #[cfg(not(feature = "test-hooks"))]
            mod $name;
        )*
    };
}

harness_mod!(
    actions,
    application,
    background,
    composer,
    mailbox,
    menu,
    notify,
    palette,
    preferences,
    reader,
    shortcuts,
    test_hooks,
    theme,
    window,
);

mod accessibility;
mod activation;
mod autostart;
mod cli;
mod compose_actions;
mod config;
mod default_handler;
mod desktop;
mod engine;
mod engine_link;
mod folders;
mod go_to;
mod latest_write;
mod list;
mod message_menu;
mod monitor;
mod move_to;
mod oauth;
mod outbox;
mod part_cache;
mod pins;
mod pop3;
mod rules;
mod search;
#[cfg(feature = "test-hooks")]
mod selftest;
mod setup;
mod status;
mod triage;
mod unsubscribe;
mod updates;
mod webview;

mod entry;
pub use entry::main;
