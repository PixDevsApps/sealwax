//! Spike S1: IMAP library and sync model (M0). Measurement code only; production crates never
//! depend on it. See docs/spikes/S1-imap.md.

pub mod faults;
pub mod fetch;
pub mod idle;
pub mod mutf7;
pub mod net;
pub mod ops;
pub mod perf;
pub mod probe;
pub mod proxy;
pub mod random;
pub mod raw;
pub mod rng;
pub mod roles;
pub mod store;
pub mod sync;
pub mod synth;
pub mod testbed;
pub mod tls;
