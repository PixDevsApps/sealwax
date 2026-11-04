//! The UI model: what the window shows and how it is driven, as pure values. The action
//! registry, the keymap and `keys.toml`, the command palette's matcher, the theme and its
//! stylesheets, the window layout and the search field's suggestions. No GTK and no I/O: the
//! app builds widgets from these, and infrastructure reads the files they describe.

#![forbid(unsafe_code)]
#![warn(clippy::unwrap_used)]

pub mod actions;
#[cfg(any(test, feature = "fuzz"))]
#[doc(hidden)]
pub mod fuzz;
pub mod fuzzy;
pub mod keymap;
pub mod layout;
pub mod search_hints;
pub mod theme;
