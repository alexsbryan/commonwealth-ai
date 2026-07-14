// SPDX-License-Identifier: AGPL-3.0-or-later
//! Append-only ledger-event store backed by [`MeshStore`].
//!
//! Why piggy-back on `MeshStore` instead of a sibling SQLite table:
//! the existing store already has WAL persistence, gossip
//! replication via `all_entries_for_gossip`, and LWW merge
//! semantics. Writing events under a dedicated `app_id` reuses all
//! of that for free. The append-only invariant is enforced by the
//! key shape — every event's key contains a unique
//! origin+timestamp+nanos suffix so two events never collide and
//! LWW degenerates to "every event keeps".
//!
//! Glassbox: every emit produces a `contribution_emit:<kind>`
//! tracing event so an operator can `grep contribution_emit` to
//! see the live ledger inflow without inspecting SQLite.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use commonwealth_core::contributions::{
    aggregate, LedgerEvent, LedgerEventKind, NodeContributions, DEFAULT_WINDOW_DAYS,
};
use commonwealth_core::ids::NodeId;
use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::store::MeshStore;

/// `app_id` namespace used for ledger events inside `MeshStore`.
/// Distinct from peer-preference state (which is local-only and
/// excluded from gossip — see `peer_preferences` module in commit 3).
pub const CONTRIBUTIONS_APP_ID: &str = "contributions";

/// Per-process monotonic counter that nudges the unique suffix on
/// rapid-fire emits. Without it, two events emitted in the same
/// nanosecond would collide on key.
static EMIT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Emit ledger events into the gossip-replicated event log.
///
/// Cheap to clone — backed by `Arc` internally via `MeshStore`.
/// Holding it on `AppState` is the intended pattern.
#[derive(Clone)]
pub struct ContributionEmitter {
    store: MeshStore,
    self_node_id: NodeId,
}

impl ContributionEmitter {
    pub fn new(store: MeshStore, self_node_id: NodeId) -> Self {
        Self {
            store,
            self_node_id,
        }
    }

    /// The node id this emitter writes events as. Useful when a
    /// caller needs to populate fields like `ShardTransferred.from_node`
    /// with the emitter's identity (the legacy push path) without
    /// threading the id through separately.
    pub fn self_node_id(&self) -> NodeId {
        self.self_node_id
    }

    /// Emit a single event. Persists to the local store with the
    /// emitter's node id as origin; the next gossip round
    /// propagates it to peers automatically. Errors are
    /// non-fatal: a failure to record contribution must not block
    /// the underlying request, so on storage failure we log and
    /// continue.
    pub fn record(&self, kind: LedgerEventKind) {
        let now = now_secs();
        let event = LedgerEvent {
            node_id: self.self_node_id,
            timestamp: now,
            kind: kind.clone(),
        };

        // Glassbox: log the variant + the most useful field per
        // shape so operators can correlate emit-time decisions.
        emit_tracing_event(&kind);

        let payload = match serde_json::to_vec(&event) {
            Ok(b) => Bytes::from(b),
            Err(e) => {
                tracing::warn!(error = %e, "contribution_emit: serialize failed");
                return;
            }
        };
        let key = self.unique_key(now);
        if let Err(e) = self
            .store
            .set(CONTRIBUTIONS_APP_ID, &key, payload, self.self_node_id)
        {
            tracing::warn!(
                error = %e,
                key = %key,
                "contribution_emit: store write failed"
            );
        }
    }

    /// Read every persisted ledger event back as a `Vec`. Used by
    /// the aggregator and by `commonwealth balance` to render the
    /// dimensional summary.
    pub fn events(&self) -> Result<Vec<LedgerEvent>> {
        let entries = self.store.scan(CONTRIBUTIONS_APP_ID, "")?;
        let mut events = Vec::with_capacity(entries.len());
        for entry in entries {
            match serde_json::from_slice::<LedgerEvent>(entry.value.as_ref()) {
                Ok(ev) => events.push(ev),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        key = %entry.key,
                        "contribution_emit: stored event failed to deserialize — skipping"
                    );
                }
            }
        }
        Ok(events)
    }

    fn unique_key(&self, now_secs: u64) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let seq = EMIT_SEQ.fetch_add(1, Ordering::Relaxed);
        // Hex-encode the node id's big-endian u128 so keys sort
        // deterministically by node then time. The `seq` suffix
        // breaks ties at sub-nanosecond resolution.
        let id_bytes = self.self_node_id.as_bytes();
        let id_hex: String = id_bytes.iter().map(|b| format!("{b:02x}")).collect();
        format!("{id_hex}:{now_secs:020}:{nanos:010}:{seq:016}")
    }
}

/// Aggregate stored events into the dimensional per-node view.
/// Convenience wrapper around `commonwealth_core::contributions::aggregate`
/// that pulls the event stream out of `MeshStore` first.
pub fn current_contributions(
    store: &MeshStore,
    peer_capabilities: &HashMap<NodeId, commonwealth_core::capabilities::NodeCapabilities>,
    window_days: u32,
) -> Result<HashMap<NodeId, NodeContributions>> {
    let entries = store.scan(CONTRIBUTIONS_APP_ID, "")?;
    let mut events = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Ok(ev) = serde_json::from_slice::<LedgerEvent>(entry.value.as_ref()) {
            events.push(ev);
        }
    }
    let now = now_secs();
    let window_secs = (window_days as u64) * 86_400;
    Ok(aggregate(&events, now, window_secs, peer_capabilities))
}

/// Default window (matches `DEFAULT_WINDOW_DAYS`). Re-exported so
/// the daemon and CLI don't need to import from two crates.
pub fn default_window_days() -> u32 {
    DEFAULT_WINDOW_DAYS
}

/// Default cadence for the hourly `StorageSnapshot` background
/// task. Aligned with `RetentionGc::DEFAULT_INTERVAL` so a single
/// daemon clock tick handles both rollups.
pub const STORAGE_SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3_600);

/// Long-running background task that emits one `StorageSnapshot`
/// event per [`STORAGE_SNAPSHOT_INTERVAL`] tick. Consumes a
/// `walker` closure that produces the per-corpus `(id, size_gb)`
/// pairs to record — keeps this module decoupled from
/// `corpus-engine` (the daemon supplies the walker; the state
/// crate doesn't pull in a knowledge dep).
///
/// Shuts down cleanly when `shutdown` flips to true.
pub async fn run_storage_snapshot_loop<F, Fut>(
    emitter: ContributionEmitter,
    mut walker: F,
    interval: std::time::Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Vec<(String, f64)>> + Send,
{
    let mut ticker = tokio::time::interval(interval);
    // The first tick fires immediately; we want a snapshot at boot
    // AND every interval after, so this is the desired behavior.
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let corpora = walker().await;
                if !corpora.is_empty() {
                    emitter.record(LedgerEventKind::StorageSnapshot {
                        corpora,
                    });
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!(
                        "storage_snapshot: shutdown requested — exiting"
                    );
                    return;
                }
            }
        }
    }
}

use commonwealth_core::clock::unix_now_secs as now_secs;

fn emit_tracing_event(kind: &LedgerEventKind) {
    match kind {
        LedgerEventKind::InferenceServed {
            for_node,
            tokens_generated,
            wall_seconds,
            ..
        } => {
            tracing::debug!(
                kind = "InferenceServed",
                for_node = %fmt_node(for_node),
                tokens = tokens_generated,
                wall_secs = wall_seconds,
                "contribution_emit: InferenceServed"
            );
        }
        LedgerEventKind::InferenceReceived {
            from_node,
            tokens_generated,
            ..
        } => {
            tracing::debug!(
                kind = "InferenceReceived",
                from_node = %fmt_node(from_node),
                tokens = tokens_generated,
                "contribution_emit: InferenceReceived"
            );
        }
        LedgerEventKind::KnowledgeQueryServed {
            for_node,
            corpus_id,
            chunks_returned,
        } => {
            tracing::debug!(
                kind = "KnowledgeQueryServed",
                for_node = %fmt_node(for_node),
                corpus_id = %corpus_id,
                chunks = chunks_returned,
                "contribution_emit: KnowledgeQueryServed"
            );
        }
        LedgerEventKind::ShardTransferred {
            from_node,
            to_node,
            corpus_id,
            bytes,
        } => {
            tracing::debug!(
                kind = "ShardTransferred",
                from_node = %fmt_node(from_node),
                to_node = %fmt_node(to_node),
                corpus_id = %corpus_id,
                bytes = bytes,
                "contribution_emit: ShardTransferred"
            );
        }
        LedgerEventKind::StorageSnapshot { corpora } => {
            tracing::debug!(
                kind = "StorageSnapshot",
                corpora_count = corpora.len(),
                total_gb = corpora.iter().map(|(_, gb)| gb).sum::<f64>(),
                "contribution_emit: StorageSnapshot"
            );
        }
    }
}

/// Render a node id as 12 hex chars (matches the redaction policy
/// from ARCH_PRINCIPLES §9.3 — enough to correlate events without
/// leaking the full id into logs).
fn fmt_node(id: &NodeId) -> String {
    let bytes = id.as_bytes();
    let prefix: String = bytes.iter().take(6).map(|b| format!("{b:02x}")).collect();
    prefix
}

// Surface the unused-Error-import compiler hint as a real type
// alias so callers can match on it.
#[doc(hidden)]
pub type _ContributionStoreError = Error;

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    #[test]
    fn emit_then_read_round_trips_events() {
        let store = MeshStore::in_memory().unwrap();
        let emitter = ContributionEmitter::new(store, nid(7));
        emitter.record(LedgerEventKind::InferenceServed {
            for_node: nid(8),
            model_id: "qwen-9b".into(),
            tokens_generated: 100,
            wall_seconds: 2.5,
        });
        emitter.record(LedgerEventKind::ShardTransferred {
            from_node: nid(7),
            to_node: nid(9),
            corpus_id: "wikipedia".into(),
            bytes: 5_000_000_000,
        });
        let events = emitter.events().unwrap();
        assert_eq!(events.len(), 2, "two emits must produce two stored events");
        // Origin is the emitter's node id on every event.
        for ev in &events {
            assert_eq!(ev.node_id, nid(7));
        }
    }

    #[test]
    fn keys_are_unique_under_rapid_fire_emits() {
        // Hammer the emitter with many same-millisecond emits and
        // verify no events were lost to LWW collision.
        let store = MeshStore::in_memory().unwrap();
        let emitter = ContributionEmitter::new(store, nid(7));
        for i in 0..1000 {
            emitter.record(LedgerEventKind::InferenceReceived {
                from_node: nid(((i % 250) + 1) as u8),
                model_id: format!("model-{i}"),
                tokens_generated: i as u64,
            });
        }
        let events = emitter.events().unwrap();
        assert_eq!(events.len(), 1000, "no events may collide on key");
    }

    #[tokio::test]
    async fn storage_snapshot_loop_emits_on_first_tick() {
        // Pinned cadence — first tick fires immediately when the
        // tokio interval is created, so a snapshot lands at boot
        // without waiting an hour. Pin this so a future tokio
        // change doesn't silently shift the boot-time snapshot
        // off the ledger.
        let store = MeshStore::in_memory().unwrap();
        let emitter = ContributionEmitter::new(store.clone(), nid(1));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let walker = || async { vec![("wikipedia".to_string(), 12.5_f64)] };
        let handle = tokio::spawn(run_storage_snapshot_loop(
            emitter.clone(),
            walker,
            std::time::Duration::from_secs(3_600),
            shutdown_rx,
        ));
        // Real-time wait — first tick is immediate, so 50ms is
        // generous. Pinned at 3600s interval so the second tick is
        // an hour out (well after this test ends).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = shutdown_tx.send(true);
        let _ = handle.await;

        let events = emitter.events().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                LedgerEventKind::StorageSnapshot { corpora }
                    if corpora == &vec![("wikipedia".to_string(), 12.5)]
            )),
            "first tick must produce a StorageSnapshot, got {events:?}"
        );
    }

    #[tokio::test]
    async fn storage_snapshot_loop_skips_emission_when_walker_returns_empty() {
        // Empty walker → no event. Lets a daemon without a corpus
        // engine wired up (or with no mesh-shared corpora) start
        // the loop unconditionally without polluting the ledger.
        let store = MeshStore::in_memory().unwrap();
        let emitter = ContributionEmitter::new(store.clone(), nid(1));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let walker = || async { Vec::<(String, f64)>::new() };
        let handle = tokio::spawn(run_storage_snapshot_loop(
            emitter.clone(),
            walker,
            std::time::Duration::from_secs(3_600),
            shutdown_rx,
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = shutdown_tx.send(true);
        let _ = handle.await;

        let events = emitter.events().unwrap();
        assert!(
            events.is_empty(),
            "no events expected when walker returns empty, got {events:?}"
        );
    }

    #[test]
    fn current_contributions_aggregates_emitted_events() {
        let store = MeshStore::in_memory().unwrap();
        let emitter = ContributionEmitter::new(store.clone(), nid(7));
        emitter.record(LedgerEventKind::InferenceServed {
            for_node: nid(8),
            model_id: "qwen-9b".into(),
            tokens_generated: 100,
            wall_seconds: 2.5,
        });
        emitter.record(LedgerEventKind::InferenceServed {
            for_node: nid(9),
            model_id: "qwen-9b".into(),
            tokens_generated: 200,
            wall_seconds: 5.0,
        });
        let result = current_contributions(&store, &HashMap::new(), DEFAULT_WINDOW_DAYS).unwrap();
        assert_eq!(result[&nid(7)].inference_served.requests, 2);
        assert_eq!(result[&nid(7)].inference_served.total_tokens_generated, 300);
        assert!((result[&nid(7)].inference_served.wall_seconds - 7.5).abs() < 1e-6);
    }
}
