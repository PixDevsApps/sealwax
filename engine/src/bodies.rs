//! What the service and phase B's account actor share about bodies (ML-044): the messages the
//! background prefetch downloads after a sync, and the disk cap's enforcement. The fetching
//! itself is `imap::bodies`; the rules are `sealwax_domain::bodies`.

use crate::imap::bodies::Sink;
use sealwax_domain::bodies::{CacheUse, Candidate, Prefetch, prefetch};
use sealwax_domain::config::Storage;
use sealwax_domain::model::MessageId;
use sealwax_infrastructure::store::{Store, StoreError};

/// Candidates read per prefetch; the policy picks from them.
const PREFETCH_CANDIDATES: u32 = 500;

/// The messages the background prefetch downloads at `now` under `storage`, newest first:
/// Inbox messages of server accounts within `prefetch_days`, each up to the whole-message limit,
/// while the bodies stay within half the cap. Empty when `prefetch_days` is 0.
pub async fn prefetch_ids(
    store: &Store,
    storage: &Storage,
    now: i64,
) -> Result<Vec<MessageId>, StoreError> {
    let policy = Prefetch::new(storage);
    let Some(since) = policy.since(now) else {
        return Ok(Vec::new());
    };
    let candidates = store
        .body_candidates(since, PREFETCH_CANDIDATES)
        .recv()
        .await?;
    let usage = store.body_usage().recv().await?;
    let candidates: Vec<Candidate> = candidates
        .iter()
        .map(|candidate| Candidate {
            message: candidate.message,
            role: candidate.role,
            date: candidate.date,
            size: candidate.size,
            downloaded: false,
        })
        .collect();
    let cache = CacheUse {
        used: usage.bytes,
        limit: storage.cache_size,
    };
    Ok(prefetch(&policy, now, cache, &candidates))
}

/// Sends the bodies used longest ago back to their headers and preview until the bodies take
/// at most `limit` bytes, sparing those of `keep`, and deletes their blobs after each commit.
/// Returns what the bodies take afterwards.
pub async fn enforce_cap(
    sink: Sink<'_>,
    limit: u64,
    keep: &[MessageId],
) -> Result<u64, StoreError> {
    loop {
        let _writes = sink.writes.lock().await;
        let evicted = sink.store.evict_bodies(limit, keep.to_vec()).recv().await?;
        if !evicted.blobs.is_empty() {
            tracing::info!(
                bodies = evicted.blobs.len(),
                messages = evicted.messages.len(),
                "bodies evicted under the disk cap"
            );
            let blobs = sink.blobs.clone();
            let keys = evicted.blobs.clone();
            let _ = tokio::task::spawn_blocking(move || {
                for key in &keys {
                    if let Err(error) = blobs.remove(key) {
                        tracing::warn!(%error, "an evicted raw message couldn't be deleted");
                    }
                }
            })
            .await;
        }
        if !evicted.more {
            return Ok(evicted.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealwax_domain::account::AccountKey;
    use sealwax_domain::model::BodyKind;
    use sealwax_domain::model::FolderRole;
    use sealwax_infrastructure::blobs::BlobStore;
    use sealwax_infrastructure::store::{
        BodyState, BodyUpgrade, FolderSpec, NewAccount, NewMessage, StoreOptions,
    };
    use std::path::Path;

    const NOW: i64 = 1_790_000_000;
    const MIB: u64 = 1024 * 1024;

    fn scratch() -> (tempfile::TempDir, Store, BlobStore) {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/scratch");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("engine-bodies-")
            .tempdir_in(base)
            .unwrap();
        let blobs = BlobStore::open(&dir.path().join("blobs")).unwrap();
        let store = Store::open(
            &dir.path().join("store.sqlite"),
            StoreOptions {
                blobs: Some(blobs.clone()),
                ..StoreOptions::default()
            },
        )
        .unwrap();
        (dir, store, blobs)
    }

    /// A server account's Inbox with header-only messages of `sizes`, one a day back from now.
    async fn inbox(store: &Store, sizes: &[u64]) -> Vec<MessageId> {
        let account = store
            .ensure_account(
                AccountKey("work".into()),
                NewAccount {
                    name: "Work".into(),
                    address: "work@example.test".into(),
                },
            )
            .recv()
            .await
            .unwrap()
            .value;
        let folder = store
            .upsert_folder(FolderSpec::local(account, "INBOX", FolderRole::Inbox))
            .recv()
            .await
            .unwrap()
            .value;
        let messages = sizes
            .iter()
            .enumerate()
            .map(|(n, size)| NewMessage {
                folder,
                uid: Some(n as u32 + 1),
                date: NOW - n as i64 * 86_400 - 60,
                size: *size,
                body_state: BodyState::Partial,
                ..NewMessage::default()
            })
            .collect();
        store
            .ingest(messages)
            .recv()
            .await
            .unwrap()
            .ids
            .into_iter()
            .map(Option::unwrap)
            .collect()
    }

    #[tokio::test]
    async fn the_prefetch_takes_recent_inbox_mail_within_half_the_cap() {
        let (_dir, store, _blobs) = scratch();
        let ids = inbox(&store, &[MIB, 20 * MIB, MIB, MIB, 300 * MIB, MIB]).await;
        let storage = Storage {
            cache_size: Some(1000 * MIB),
            prefetch_days: 4,
        };
        let picked = prefetch_ids(&store, &storage, NOW).await.unwrap();
        assert_eq!(picked, [ids[0], ids[2], ids[3]], "{picked:?}");
        let tight = Storage {
            cache_size: Some(5 * MIB),
            ..storage
        };
        assert_eq!(
            prefetch_ids(&store, &tight, NOW).await.unwrap(),
            [ids[0], ids[2]]
        );
        let off = Storage {
            prefetch_days: 0,
            ..storage
        };
        assert!(prefetch_ids(&store, &off, NOW).await.unwrap().is_empty());
        store.close().unwrap();
    }

    #[tokio::test]
    async fn the_cap_deletes_the_evicted_blobs() {
        let (_dir, store, blobs) = scratch();
        let ids = inbox(&store, &[100, 100, 100]).await;
        let mut keys = Vec::new();
        for (n, id) in ids.iter().enumerate() {
            let raw = format!("Subject: {n}\r\n\r\n{}\r\n", "x".repeat(1000)).into_bytes();
            let key = blobs.put(&raw).unwrap();
            keys.push(key);
            let upgraded = store
                .upgrade_body(BodyUpgrade {
                    message: *id,
                    uid: n as u32 + 1,
                    blob: key,
                    blob_size: raw.len() as u64,
                    body_state: BodyState::Full,
                    body_kind: BodyKind::Plain,
                    has_attachments: false,
                    snippet: String::new(),
                    text_parts: vec![0],
                    html_parts: Vec::new(),
                    attachments: Vec::new(),
                    sections: Vec::new(),
                    search_text: String::new(),
                    used: NOW + n as i64 * 100,
                })
                .recv()
                .await
                .unwrap();
            assert!(upgraded.found);
        }
        let writes = tokio::sync::Mutex::new(());
        let sink = Sink {
            store: &store,
            blobs: &blobs,
            writes: &writes,
        };
        let left = enforce_cap(sink, 2_100, &[ids[0]]).await.unwrap();
        assert!(left <= 2_100, "{left}");
        assert!(blobs.contains(&keys[0]), "kept, though used longest ago");
        assert!(!blobs.contains(&keys[1]), "the oldest of the others went");
        assert!(blobs.contains(&keys[2]));
        assert_eq!(store.body_usage().recv().await.unwrap().bodies, 2);
        store.close().unwrap();
    }
}
