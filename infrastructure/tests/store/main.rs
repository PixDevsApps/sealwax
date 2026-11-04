//! The store's suites, over its public API and `store::testing` (the `test-support` feature):
//! one test binary, run by `cargo test -p sealwax-infrastructure`.

#![warn(clippy::unwrap_used)]
#![allow(
    clippy::unwrap_used,
    reason = "tests unwrap: a failure is the test failing"
)]

/// What each suite had from the store module's scope before they moved out of it.
mod prelude {
    pub use sealwax_domain::account::AccountKey;
    pub use sealwax_domain::model::{
        AccountId, BlobKey, Flags, Folder, FolderId, MessageDetail, MessageId, MessageSummary,
    };
    pub use sealwax_infrastructure::blobs::BlobStore;
    pub use sealwax_infrastructure::store::testing;
    pub use sealwax_infrastructure::store::*;
    pub use std::fs;
    pub use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    pub use std::path::{Path, PathBuf};
    pub use std::sync::Arc;
}

mod account_tests;
mod body_tests;
mod common;
mod compose_prop_tests;
mod compose_tests;
mod conversation_tests;
mod count_tests;
mod folders_tests;
mod ops_prop_tests;
mod ops_tests;
mod pin_tests;
mod pop3_tests;
mod remote_tests;
mod resync_tests;
mod rules_tests;
mod search_tests;
mod sync_tests;
mod thread_tests;
mod unified_tests;
