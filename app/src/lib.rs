//! The GTK application: windows, widgets, theme, actions and the self-test runner.
//! `main.rs` only starts it, so every module here can be built and tested on its own.

pub mod actions;
pub mod activation;
pub mod application;
pub mod cli;
pub mod config;
pub mod menu;
pub mod monitor;
pub mod palette;
pub mod preferences;
pub mod reader;
pub mod selftest;
pub mod shortcuts;
pub mod theme;
pub mod window;
