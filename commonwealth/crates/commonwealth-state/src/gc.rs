// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retention garbage collector for the mesh store.
//!
//! Runs on a periodic interval and deletes entries older than `ttl_seconds`.
//! Shuts down when the watch channel fires.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::store::MeshStore;

pub struct RetentionGc {
    store: Arc<MeshStore>,
    ttl_seconds: u64,
    interval: Duration,
    /// `None` = sweep every app in the store (the original,
    /// whole-store behaviour). `Some(app_id)` = bound exactly one
    /// namespace and leave every other app's entries alone.
    ///
    /// WHY THE SCOPE EXISTS. A `MeshStore` is shared by apps with
    /// opposite retention semantics. The contributions ledger is an
    /// append-only event log that grows without bound and is read only
    /// over a trailing window, so old rows are provably dead. But the
    /// SAME store holds processed-shards dedup markers
    /// (`PROCESSED_SHARDS_APP_ID`) and ingestion-handoff records
    /// (`corpus-engine/handoff:*`) that are written once and
    /// deliberately never rewritten — deleting those on age re-opens
    /// ingest work the mesh already completed. A daemon that needs the
    /// ledger bounded must therefore be able to say so without
    /// inheriting a whole-store delete.
    app_scope: Option<String>,
}

impl RetentionGc {
    pub fn new(store: Arc<MeshStore>, ttl_seconds: u64, interval: Duration) -> Self {
        Self {
            store,
            ttl_seconds,
            interval,
            app_scope: None,
        }
    }

    /// Restrict this GC to a single `app_id`. See [`RetentionGc::app_scope`].
    pub fn scoped_to_app(mut self, app_id: impl Into<String>) -> Self {
        self.app_scope = Some(app_id.into());
        self
    }

    /// One GC pass. The single place the scope decision is made, so
    /// the loop above and any test drive the same decider.
    pub fn sweep(&self) -> crate::error::Result<usize> {
        match &self.app_scope {
            Some(app_id) => self.store.gc_app(app_id, self.ttl_seconds),
            None => self.store.gc(self.ttl_seconds),
        }
    }

    /// Run until the shutdown watch fires `true`.
    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match self.sweep() {
                        Ok(n) if n > 0 => {
                            debug!(
                                deleted = n,
                                ttl_secs = self.ttl_seconds,
                                app_scope = self.app_scope.as_deref().unwrap_or("<all apps>"),
                                "RetentionGc: deleted expired entries"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("RetentionGc error: {e}");
                        }
                    }
                }
                Ok(()) = shutdown.changed() => {
                    if *shutdown.borrow() {
                        debug!("RetentionGc: shutdown signal received");
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MeshStore, StoreEntry};
    use crate::{CONTRIBUTIONS_APP_ID, PROCESSED_SHARDS_APP_ID};
    use commonwealth_core::ids::NodeId;

    fn seed(store: &MeshStore, app_id: &str, key: &str, age_secs: u64) {
        let now = commonwealth_core::clock::unix_now_secs();
        store
            .merge_entry(StoreEntry {
                app_id: app_id.to_string(),
                key: key.to_string(),
                value: bytes::Bytes::from_static(b"{}"),
                timestamp: now.saturating_sub(age_secs),
                origin: NodeId::from_u128(7),
            })
            .expect("seed write");
    }

    const DAY: u64 = 86_400;

    /// RED-FIRST (order mesh-scale-t0, item 6). The sovereign daemon
    /// needs the contributions ledger bounded, but its `MeshStore` also
    /// holds processed-shards dedup markers that are written once and
    /// never rewritten. Spawning the pre-fix, whole-store `RetentionGc`
    /// there would have deleted those markers on age and re-opened
    /// ingest work the mesh already did.
    ///
    /// On pre-fix code (`RetentionGc` with no `scoped_to_app`, sweeping
    /// via `store.gc`) the second assertion fails: the shard marker is
    /// gone along with the ledger event.
    #[test]
    fn scoped_gc_bounds_the_ledger_without_touching_other_apps() {
        let store = Arc::new(MeshStore::in_memory().unwrap());
        seed(&store, CONTRIBUTIONS_APP_ID, "old-event", 40 * DAY);
        seed(&store, CONTRIBUTIONS_APP_ID, "fresh-event", DAY);
        seed(&store, PROCESSED_SHARDS_APP_ID, "corpus:shard-0", 400 * DAY);

        let gc = RetentionGc::new(Arc::clone(&store), 30 * DAY, Duration::from_secs(3_600))
            .scoped_to_app(CONTRIBUTIONS_APP_ID);
        let deleted = gc.sweep().expect("sweep");

        assert_eq!(deleted, 1, "only the out-of-window ledger event is dead");
        assert!(
            store
                .get(CONTRIBUTIONS_APP_ID, "old-event")
                .unwrap()
                .is_none(),
            "an event older than the aggregation window must be collected"
        );
        assert!(
            store
                .get(CONTRIBUTIONS_APP_ID, "fresh-event")
                .unwrap()
                .is_some(),
            "an in-window event must survive"
        );
        assert!(
            store
                .get(PROCESSED_SHARDS_APP_ID, "corpus:shard-0")
                .unwrap()
                .is_some(),
            "a processed-shards marker is write-once by design — an unscoped \
             GC deleting it re-opens completed ingest work"
        );
    }

    /// The unscoped form still behaves as it always did — this is what
    /// `commonwealth-daemon` runs, and the test above only makes sense
    /// against it.
    #[test]
    fn unscoped_gc_sweeps_every_app() {
        let store = Arc::new(MeshStore::in_memory().unwrap());
        seed(&store, CONTRIBUTIONS_APP_ID, "old-event", 40 * DAY);
        seed(&store, PROCESSED_SHARDS_APP_ID, "corpus:shard-0", 400 * DAY);

        let gc = RetentionGc::new(Arc::clone(&store), 30 * DAY, Duration::from_secs(3_600));
        assert_eq!(gc.sweep().expect("sweep"), 2);
    }
}
