//! The senders whose remote images load without asking (ML-058).

use super::Reader;
use crate::store::StoreError;

impl Reader {
    /// Every address on the list, lower case, in order.
    pub(super) fn remote_senders(&self) -> Result<Vec<String>, StoreError> {
        let addresses = self
            .conn
            .prepare_cached("SELECT address FROM remote_senders ORDER BY address")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(addresses)
    }
}
