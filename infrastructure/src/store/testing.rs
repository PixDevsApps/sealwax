//! What the store's suites (`tests/store/`) check beyond the public API (`test-support` only):
//! the schema's steps, to build stores of older versions, and the codes and rules the suites
//! compare with what SQLite holds.

use super::api::{BodyState, ViewFilter};
use super::{schema, views, writer};
use sealwax_domain::search::Date;

/// Schema version 1, as the first store held it.
pub const V1: &str = schema::V1;
/// Every schema step in order: `MIGRATIONS[n]` makes version `n + 1`.
pub const MIGRATIONS: &[&str] = schema::MIGRATIONS;
/// Takes the current schema back to version 13.
pub const UNDO_V14: &str = schema::UNDO_V14;
/// Takes the current schema back to version 12.
pub const UNDO_V13: &str = schema::UNDO_V13;
/// The prefix of a conversation key made from a `Thread-Index` header.
pub const THREAD_INDEX_ALIAS: &str = writer::THREAD_INDEX_ALIAS;

/// The column value the store keeps for `state`.
pub fn body_state_code(state: BodyState) -> i64 {
    state.code()
}

/// Whether a row with these values passes `filter`, as the views' SQL decides.
pub fn passes(filter: ViewFilter, flags: u32, has_attachments: bool) -> bool {
    views::passes(filter, flags, has_attachments)
}

/// Unix seconds at the start of `date` in the local time zone, as search's dates mean it.
pub fn local_midnight(date: Date) -> i64 {
    views::local_midnight(date)
}
