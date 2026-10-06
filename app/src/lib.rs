//! The GTK application: windows, widgets, theme, actions and the self-test runner.
//! `main.rs` only starts it, so every module here can be built and tested on its own.

pub mod actions;
pub mod activation;
pub mod application;
pub mod background;
pub mod cli;
pub mod compose_actions;
pub mod composer;
pub mod config;
pub mod default_handler;
pub mod desktop;
pub mod engine;
pub mod engine_link;
pub mod list;
pub mod mailbox;
pub mod menu;
pub mod monitor;
pub mod move_to;
pub mod notify;
pub mod oauth;
pub mod outbox;
pub mod palette;
pub mod preferences;
pub mod reader;
pub mod search;
pub mod selftest;
pub mod setup;
pub mod shortcuts;
pub mod status;
pub mod theme;
pub mod triage;
pub mod window;
