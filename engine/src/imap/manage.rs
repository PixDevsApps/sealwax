//! Folder management on the server (ADR-020), on the account's command connection through the
//! raw layer: CREATE, then SUBSCRIBE (other programs often show subscribed folders only); RENAME,
//! then SUBSCRIBE of the new names and UNSUBSCRIBE of the old ones, the folder's and those under
//! it; DELETE of the folders under a folder, the deepest first, then of the folder, each followed
//! by UNSUBSCRIBE. A subscription the server refuses changes nothing else. After a CREATE or a
//! DELETE the folder list is read again as the account pass reads it; after a RENAME the store
//! renames the folders in place, keeping their messages.
//!
//! The server's refusals reach the app in its own words ([`FolderError::Refused`]), never the
//! log. A new folder at the top of the account goes under the Inbox where every other folder is
//! under it (`folders::plan`): NAMESPACE (RFC 2342) would name the personal prefix, but
//! imap-proto 0.17 can't parse its reply, and async-imap then can't read the connection again.

use super::connection::Connection;
use super::error::ImapError;
use super::folders::{list, list_under, spec};
use super::hidden;
use super::raw::{self, quote};
use super::sync::view;
use crate::api::{FolderChange, FolderError};
use crate::folders::{Plan, Wire, deepest_first, known, plan};
use sealwax_domain::folders::within;
use sealwax_domain::model::{AccountId, BlobKey};
use sealwax_infrastructure::store::Store;

/// The server's NO or BAD in its words; anything else broke the connection.
fn refused(error: ImapError) -> FolderError {
    match error {
        ImapError::No(text) | ImapError::Bad(text) => FolderError::Refused(text),
        _ => FolderError::Lost,
    }
}

/// A command whose refusal is the change's.
async fn command(conn: &mut Connection, line: &str) -> Result<(), FolderError> {
    raw::run(&mut conn.session, line, |_| {})
        .await
        .map_err(refused)
}

/// SUBSCRIBE or UNSUBSCRIBE each of `names`; the server's refusals are left alone.
async fn subscriptions(
    conn: &mut Connection,
    verb: &str,
    names: &[String],
) -> Result<(), FolderError> {
    for name in names {
        let line = format!("{verb} {}", quote(name).map_err(refused)?);
        match raw::run(&mut conn.session, &line, |_| {}).await {
            Ok(()) | Err(ImapError::No(_) | ImapError::Bad(_)) => {}
            Err(_) => return Err(FolderError::Lost),
        }
    }
    Ok(())
}

/// Leaves the selected mailbox when it is `top` or under it: UNSELECT where offered, else
/// EXAMINE of the Inbox (CLOSE would expunge).
async fn leave(
    conn: &mut Connection,
    top: &str,
    delimiter: Option<char>,
) -> Result<(), FolderError> {
    let Some(selected) = conn.selected.clone() else {
        return Ok(());
    };
    if !within(&selected, top, delimiter) {
        return Ok(());
    }
    if conn.caps().has("UNSELECT") {
        command(conn, "UNSELECT").await?;
        conn.selected = None;
    } else {
        view::examine(conn, "INBOX").await.map_err(refused)?;
    }
    Ok(())
}

/// Reads the folder list again and makes the store's the server's, as the account pass does;
/// returns the raw messages the folders that went took with them.
async fn relist(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
) -> Result<Vec<BlobKey>, FolderError> {
    let listed = list(conn).await.map_err(refused)?;
    if listed.is_empty() {
        return Ok(Vec::new());
    }
    let specs = listed.iter().map(|folder| spec(account, folder)).collect();
    Ok(store
        .replace_folders(account, specs)
        .recv()
        .await?
        .orphaned_blobs)
}

/// Makes `change` on the server and in the store. Returns the raw messages folders that went
/// took with them, for the blob store.
pub async fn change(
    conn: &mut Connection,
    store: &Store,
    account: AccountId,
    change: &FolderChange,
) -> Result<Vec<BlobKey>, FolderError> {
    let folders = known(store, account).await?;
    let microsoft = conn.microsoft();
    let planned = plan(change, &folders, Wire::Imap, |name, delimiter| {
        let entry = hidden::Entry {
            name,
            delimiter,
            selectable: true,
            messages: Some(0),
        };
        hidden::hidden(&entry, microsoft)
    })?;
    match planned {
        Plan::Create { name_raw, .. } => {
            let line = format!("CREATE {}", quote(&name_raw).map_err(refused)?);
            if let Err(error) = command(conn, &line).await {
                // A folder of that name the store didn't know may be why.
                if !matches!(error, FolderError::Lost) {
                    relist(conn, store, account).await?;
                }
                return Err(error);
            }
            subscriptions(conn, "SUBSCRIBE", &[name_raw]).await?;
            relist(conn, store, account).await
        }
        Plan::Rename {
            from_raw,
            to_raw,
            delimiter,
            renames,
            before,
        } => {
            if from_raw == to_raw {
                return Ok(Vec::new());
            }
            leave(conn, &from_raw, delimiter).await?;
            let line = format!(
                "RENAME {} {}",
                quote(&from_raw).map_err(refused)?,
                quote(&to_raw).map_err(refused)?
            );
            command(conn, &line).await?;
            let after: Vec<String> = renames.iter().map(|r| r.name_raw.clone()).collect();
            subscriptions(conn, "SUBSCRIBE", &after).await?;
            subscriptions(conn, "UNSUBSCRIBE", &before).await?;
            match store.rename_folders(account, renames).recv().await {
                Ok(_) => Ok(Vec::new()),
                Err(error) => {
                    tracing::warn!(
                        account = account.0,
                        %error,
                        "a renamed folder couldn't be renamed in the store; reading the list again"
                    );
                    relist(conn, store, account).await
                }
            }
        }
        Plan::Delete {
            top_raw,
            delimiter,
            folders,
        } => {
            let mut names: Vec<String> = folders.into_iter().map(|(_, raw)| raw).collect();
            if let Some(delimiter) = delimiter {
                for name in list_under(conn, &top_raw, delimiter)
                    .await
                    .map_err(refused)?
                {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            deepest_first(&mut names, String::as_str, delimiter);
            leave(conn, &top_raw, delimiter).await?;
            for name in &names {
                let line = format!("DELETE {}", quote(name).map_err(refused)?);
                if let Err(error) = command(conn, &line).await {
                    if !matches!(error, FolderError::Lost) {
                        relist(conn, store, account).await?;
                    }
                    return Err(error);
                }
                subscriptions(conn, "UNSUBSCRIBE", std::slice::from_ref(name)).await?;
            }
            relist(conn, store, account).await
        }
    }
}

#[cfg(test)]
mod tests {
    use async_imap::imap_proto::Response;

    /// Why NAMESPACE isn't sent: its reply doesn't parse, and async-imap's stream stops at a
    /// reply it can't parse (an error, not a wait for more bytes).
    #[test]
    fn namespaces_reply_cant_be_read() {
        for line in [
            &b"* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n"[..],
            b"* NAMESPACE ((\"INBOX.\" \".\")) NIL ((\"#shared.\" \".\"))\r\n",
        ] {
            let error = Response::parse(line).expect_err("NAMESPACE parsed");
            assert!(
                !format!("{error:?}").starts_with("Incomplete"),
                "{}",
                String::from_utf8_lossy(line)
            );
        }
        assert!(Response::parse(b"* LIST () \"/\" \"INBOX\"\r\n").is_ok());
    }
}
