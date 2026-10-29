//! The one-time backfill of List-Unsubscribe (schema v13, ADR-023): a folder whose mail was
//! stored before the header FETCH asked for it gets the two header fields for every stored
//! message, a batch of UIDs per FETCH, after its pass. A reply that can't be read ends the
//! backfill for the folder rather than failing every later pass.

use super::view::{self, View};
use crate::imap::connection::Connection;
use crate::imap::error::{ImapError, SyncError};
use crate::imap::first_sync::BATCH;
use crate::imap::raw;
use sealwax_domain::message;
use sealwax_domain::model::{FolderId, MessageId};
use sealwax_infrastructure::store::{Store, UnsubscribeHeaders};
use std::collections::HashMap;

const ITEMS: &str = "(UID BODY.PEEK[HEADER.FIELDS (LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST)])";

pub(super) async fn unsubscribe(
    conn: &mut Connection,
    store: &Store,
    folder: FolderId,
    view: &mut View,
) -> Result<(), SyncError> {
    let stored = store.folder_messages(folder).recv().await?;
    let mut found = 0;
    for batch in stored.chunks(BATCH) {
        let ids: HashMap<u32, MessageId> = batch
            .iter()
            .map(|message| (message.uid, message.id))
            .collect();
        let uids: Vec<u32> = batch.iter().map(|message| message.uid).collect();
        let mut fetched = Vec::new();
        let set = raw::uid_set(&uids);
        match view::fetch_into(conn, &set, ITEMS, &|_| true, view, &mut fetched).await {
            Ok(()) => {}
            Err(ImapError::Unreadable(_)) => {
                tracing::warn!(%folder, "List-Unsubscribe couldn't be read; the folder's backfill ends");
                break;
            }
            Err(error) => return Err(error.into()),
        }
        let headers: Vec<UnsubscribeHeaders> = fetched
            .into_iter()
            .filter_map(|message| {
                let (list_unsubscribe, one_click) = message::unsubscribe_headers(&message.header);
                Some(UnsubscribeHeaders {
                    id: *ids.get(&message.uid)?,
                    list_unsubscribe: Some(list_unsubscribe?),
                    one_click,
                })
            })
            .collect();
        found += headers.len();
        if !headers.is_empty() {
            store
                .backfill_unsubscribe(folder, headers, false)
                .recv()
                .await?;
        }
    }
    store
        .backfill_unsubscribe(folder, Vec::new(), true)
        .recv()
        .await?;
    tracing::debug!(%folder, messages = stored.len(), found, "List-Unsubscribe backfilled");
    Ok(())
}
