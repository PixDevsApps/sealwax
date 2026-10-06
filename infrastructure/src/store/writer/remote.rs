//! The senders whose remote images load without asking (ML-058). Nothing a list shows
//! changes, so these writes bump no version and send no change event.

use super::Writer;
use crate::store::{StoreError, remote_sender_key};
use rusqlite::params;

impl Writer {
    /// Adds `address` to the list; one already there keeps the time it was first added.
    pub(super) fn allow_remote_sender(&mut self, address: &str, at: i64) -> Result<(), StoreError> {
        let Some(address) = remote_sender_key(address) else {
            return Ok(());
        };
        let added = self
            .conn
            .prepare_cached(
                "INSERT INTO remote_senders(address, added) VALUES (?1, ?2) \
                 ON CONFLICT(address) DO NOTHING",
            )?
            .execute(params![address, at])?;
        if added > 0 {
            self.state.after_commit();
        }
        Ok(())
    }

    /// Takes `address` off the list; one that isn't there changes nothing.
    pub(super) fn forget_remote_sender(&mut self, address: &str) -> Result<(), StoreError> {
        let Some(address) = remote_sender_key(address) else {
            return Ok(());
        };
        let removed = self
            .conn
            .prepare_cached("DELETE FROM remote_senders WHERE address = ?1")?
            .execute([address])?;
        if removed > 0 {
            self.state.after_commit();
        }
        Ok(())
    }
}
