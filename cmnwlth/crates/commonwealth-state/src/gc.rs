// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retention garbage collector for the mesh store.
//!
//! Runs on a periodic interval and deletes entries older than `ttl_seconds`.
//! Shuts down when the watch channel fires.
//!
//! **The cutoff comes from [`crate::retention`], not from the caller.** The
//! store is a projection of the ring journal, so a sweep at a cutoff the fold
//! does not share is undone by the next round — see
//! [`RetentionGc::for_namespace`], which is the constructor the daemon uses.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::store::MeshStore;

/// The periodic sweep. The sovereign daemon spawns exactly one — the
/// contributions ledger, through [`RetentionGc::for_namespace`].
///
/// **It is not the only thing that bounds a rail-backed namespace, and it
/// is not redundant with the thing that is.** `apply_projection` applies
/// the same floor on every fold, so on a node with an online peer this
/// sweep removes what the round would have removed anyway. On a node with
/// NO online peer nothing projects at all — `ring_sync::run_one_round`
/// returns before the fold when the peer list is empty — and this task is
/// then the only bound on a store that is `in_memory()` in the shipped
/// daemon. One window, read by both (ARCH §10.6).
pub struct RetentionGc {
    store: Arc<MeshStore>,
    ttl_seconds: u64,
    interval: Duration,
    /// `None` = sweep every app in the store (the original,
    /// whole-store behaviour). `Some(app_id)` = bound exactly one
    /// namespace and leave every other app's entries alone. Set by
    /// [`RetentionGc::scoped_to_app`], whose docs carry the why.
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

    /// A GC for a namespace that DECLARES a retention window
    /// ([`crate::retention`]), with the TTL taken from that declaration
    /// rather than from the caller.
    ///
    /// This is the constructor to use. The window is the projection's floor
    /// too, and a sweep whose cutoff differs from the fold's is undone by the
    /// next round — so the number cannot be a parameter here without being two
    /// numbers (ARCH §10.6, §7.1).
    ///
    /// `None` when the namespace declares no window: absence is reported, never
    /// defaulted to some cutoff this call invented (ARCH §18.3). A caller that
    /// gets `None` has asked for retention on a namespace nobody has said how
    /// long to keep, and spawning a sweep on a guess would delete live rows.
    pub fn for_namespace(store: Arc<MeshStore>, app_id: &str, interval: Duration) -> Option<Self> {
        let ttl_seconds = crate::retention::window_secs(app_id)?;
        Some(Self {
            store,
            ttl_seconds,
            interval,
            app_scope: Some(app_id.to_string()),
        })
    }

    /// Restrict this GC to a single `app_id`, leaving every other app's
    /// entries alone. Unset, it sweeps the whole store.
    ///
    /// Prefer [`RetentionGc::for_namespace`] for a namespace with a declared
    /// window — this form lets the caller pick a cutoff, and on a rail-backed
    /// namespace a cutoff the fold does not share is undone every round.
    ///
    /// Scope it unless you have checked every app sharing the store. One
    /// [`MeshStore`] holds apps with opposite retention semantics: the
    /// contributions ledger is an append-only event log read only over a
    /// trailing window, so old rows there are provably dead — but the same
    /// store holds processed-shard dedup markers
    /// (`crate::processed_shards::PROCESSED_SHARDS_APP_ID`) and
    /// ingestion-handoff records (`corpus-engine/handoff:*`) that are written
    /// once and deliberately never rewritten. Deleting those on age re-opens
    /// ingest work the mesh already finished.
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
