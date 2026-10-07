//! A POP3 account's downloaded UIDLs (M8, ML-088).

use super::Reader;
use crate::store::{Pop3Uidl, StoreError};
use sealwax_domain::model::{AccountId, MessageId};

impl Reader {
    /// Every UIDL `account` downloaded, in UIDL order.
    pub(super) fn pop3_uidls(&self, account: AccountId) -> Result<Vec<Pop3Uidl>, StoreError> {
        let uidls = self
            .conn
            .prepare_cached(
                "SELECT uidl, message, downloaded, deleted FROM pop3_uidls \
                 WHERE account_id = ?1 ORDER BY uidl",
            )?
            .query_map([account.0], |row| {
                Ok(Pop3Uidl {
                    uidl: row.get(0)?,
                    message: row.get::<_, Option<i64>>(1)?.map(MessageId),
                    downloaded: row.get(2)?,
                    deleted: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(uidls)
    }
}
