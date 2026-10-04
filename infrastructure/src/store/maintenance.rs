//! WAL maintenance off the writer (ADR-005 amendment). The writer's WAL hook only counts
//! frames and asks for a checkpoint past a threshold; this thread runs a PASSIVE checkpoint and
//! then one tiny write, so the WAL restart and its header fsync happen here instead of in the
//! next commit the UI waits for. It sleeps until asked: no polling.

use rusqlite::Connection;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Maintenance {
    Checkpoint,
    Optimize,
}

const RETRIES: u32 = 20;

pub(crate) fn run(conn: Connection, queue: async_channel::Receiver<Maintenance>) {
    while let Ok(first) = queue.recv_blocking() {
        let mut requests = vec![first];
        while let Ok(more) = queue.try_recv() {
            requests.push(more);
        }
        if requests.contains(&Maintenance::Checkpoint) {
            checkpoint(&conn, &queue);
        }
        if requests.contains(&Maintenance::Optimize) {
            let started = Instant::now();
            match conn.execute_batch("PRAGMA optimize;") {
                Ok(()) => tracing::debug!(ms = started.elapsed().as_millis(), "store optimized"),
                Err(error) => tracing::warn!(%error, "store optimize failed"),
            }
        }
    }
}

/// Checkpoints until the WAL is fully copied back (readers holding old snapshots can delay
/// that), then restarts it.
fn checkpoint(conn: &Connection, queue: &async_channel::Receiver<Maintenance>) {
    for attempt in 0..RETRIES {
        let started = Instant::now();
        let result = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        });
        let (busy, log, done) = match result {
            Ok(values) => values,
            Err(error) => {
                tracing::warn!(%error, "checkpoint failed");
                return;
            }
        };
        if busy == 0 && done == log {
            if log > 0
                && let Err(error) = conn.execute(
                    "INSERT INTO meta(key, value) VALUES ('wal_restarts', 1) \
                     ON CONFLICT(key) DO UPDATE SET value = value + 1",
                    [],
                )
            {
                tracing::warn!(%error, "WAL restart failed");
            }
            tracing::debug!(
                frames = log,
                ms = started.elapsed().as_millis(),
                attempt,
                "checkpoint"
            );
            return;
        }
        if queue.is_closed() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
