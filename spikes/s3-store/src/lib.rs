//! Spike S3: store and message list at 100k (M0, ML-011). Measurement code only; production
//! crates never depend on it.

pub mod cjk;
pub mod rng;
pub mod schema;
pub mod sections;
pub mod stats;
pub mod store;
pub mod synth;

use std::path::PathBuf;

/// Generated databases live in the git-ignored workspace `target/` directory.
pub fn default_db_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/s3/data/s3.sqlite")
}
