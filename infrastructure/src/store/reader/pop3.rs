//! A POP3 account's downloaded UIDLs, and whether an account is one (M8, ML-088).

use super::Reader;
use crate::store::{Pop3Uidl, StoreError};
use sealwax_domain::model::{AccountId, MessageId};

impl Reader {
    /// Every UIDL `account` downloaded, in UIDL order.
    pub(super) fn pop3_uidls(&self, account: AccountId) -> Result<Vec<Pop3Uidl>, StoreError> {
        let uidls = self
            .conn
            .prepare_cached(
                "SELECT uidl, message, downloaded, whole, deleted FROM pop3_uidls \
                 WHERE account_id = ?1 ORDER BY uidl",
            )?
            .query_map([account.0], |row| {
                Ok(Pop3Uidl {
                    uidl: row.get(0)?,
                    message: row.get::<_, Option<i64>>(1)?.map(MessageId),
                    downloaded: row.get(2)?,
                    whole: row.get(3)?,
                    deleted: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(uidls)
    }

    pub(super) fn is_pop3(&self, account: AccountId) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM pop3_accounts WHERE account_id = ?1)")?
            .query_row([account.0], |row| row.get(0))?)
    }
}
