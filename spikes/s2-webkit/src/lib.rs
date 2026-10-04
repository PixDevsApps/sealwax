//! Spike S2: WebKit rendering and security for the reading pane, approach A (ADR-011).
//! Measurement code only; production crates never depend on it. The GTK and WebKit parts live
//! in the binary; this library holds the pieces that run without a display.

pub mod canary;
pub mod corpus;
pub mod heuristic;
pub mod hostile;
pub mod png;
pub mod procmem;
pub mod stats;
pub mod theme;
