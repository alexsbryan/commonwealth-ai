//! Storage for the local [Activity ledger](commonwealth_core::activity),
//! backed by [`MeshStore`] under the `activity-private` namespace.
//!
//! This mirrors [`crate::contributions`] exactly — same append-only
//! key shape, same WAL persistence — but writes under a namespace
//! that `peer_preferences::GOSSIP_EXCLUDED_APP_IDS` structurally keeps
//! off the wire. The append-only invariant holds via the unique
//! origin+timestamp+nanos+seq key suffix, so two events never collide
//! under LWW.
//!
//! Glassbox: every emit produces an `activity_emit:<kind>` tracing
//! event so `grep activity_emit` shows the live local-activity inflow
//! without opening SQLite.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;

use commonwealth_core::activity::{
    aggregate_activity, ActivityEvent, ActivityEventKind, ActivitySummary,
    ServedFor,
};
use commonwealth_core::ids::NodeId;

use crate::error::Result;
use crate::store::MeshStore;

/// `app_id` namespace for local activity events inside `MeshStore`.
/// Pinned gossip-excluded by
/// `peer_preferences::gossip_excludes_activity_private_app_id`.
pub const ACTIVITY_APP_ID: &str = "activity-private";

/// Per-process monotonic counter that breaks key ties on rapid-fire
/// emits (same as the contribution emitter's `EMIT_SEQ`).
static ACTIVITY_EMIT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Emit local-activity events into the gossip-*excluded* event log.
///
/// Cheap to clone (backed by `Arc` inside `MeshStore`). Held on
/// `AppState` alongside `ContributionEmitter`; they share the same
/// underlying store but write to different namespaces.
#[derive(Clone)]
pub struct ActivityEmitter {
    store: MeshStore,
    self_node_id: NodeId,
}

impl ActivityEmitter {
    pub fn new(store: MeshStore, self_node_id: NodeId) -> Self {
        Self {
            store,
            self_node_id,
        }
    }

    /// Emit a single activity event. Persists locally; never gossips.
    /// Errors are non-fatal — failing to record activity must never
    /// block the underlying work, so on failure we log and continue.
    pub fn record(&self, kind: ActivityEventKind) {
        let now = now_secs();
        let event = ActivityEvent {
            node_id: self.self_node_id,
            timestamp: now,
            kind: kind.clone(),
        };

        emit_tracing_event(&kind);

        let payload = match serde_json::to_vec(&event) {
            Ok(b) => Bytes::from(b),
            Err(e) => {
                tracing::warn!(error = %e, "activity_emit: serialize failed");
                return;
            }
        };
        let key = self.unique_key(now);
        if let Err(e) =
            self.store
                .set(ACTIVITY_APP_ID, &key, payload, self.self_node_id)
        {
            tracing::warn!(
                error = %e,
                key = %key,
                "activity_emit: store write failed"
            );
        }
    }

    /// Read every persisted activity event back as a `Vec`.
    pub fn events(&self) -> Result<Vec<ActivityEvent>> {
        read_activity_events(&self.store)
    }

    fn unique_key(&self, now_secs: u64) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let seq = ACTIVITY_EMIT_SEQ.fetch_add(1, Ordering::Relaxed);
        let id_hex: String = self
            .self_node_id
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("{id_hex}:{now_secs:020}:{nanos:010}:{seq:016}")
    }
}

/// Read + deserialize the activity event stream from a store handle.
fn read_activity_events(store: &MeshStore) -> Result<Vec<ActivityEvent>> {
    let entries = store.scan(ACTIVITY_APP_ID, "")?;
    let mut events = Vec::with_capacity(entries.len());
    for entry in entries {
        match serde_json::from_slice::<ActivityEvent>(entry.value.as_ref()) {
            Ok(ev) => events.push(ev),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    key = %entry.key,
                    "activity_emit: stored event failed to deserialize — skipping"
                );
            }
        }
    }
    Ok(events)
}

/// Aggregate stored activity into the single self-view summary.
/// Convenience wrapper around
/// [`commonwealth_core::activity::aggregate_activity`].
pub fn current_activity(
    store: &MeshStore,
    window_days: u32,
) -> Result<ActivitySummary> {
    let events = read_activity_events(store)?;
    let now = now_secs();
    let window_secs = (window_days as u64) * 86_400;
    Ok(aggregate_activity(&events, now, window_secs))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn emit_tracing_event(kind: &ActivityEventKind) {
    match kind {
        ActivityEventKind::LocalInferenceServed {
            model_id,
            completion_tokens,
            wall_seconds,
            ..
        } => tracing::debug!(
            kind = "LocalInferenceServed",
            model_id = %model_id,
            tokens = completion_tokens,
            wall_secs = wall_seconds,
            "activity_emit: LocalInferenceServed"
        ),
        ActivityEventKind::EmbeddingsServed {
            served_for,
            n_texts,
            tokens,
        } => tracing::debug!(
            kind = "EmbeddingsServed",
            for_peer = served_for.is_peer(),
            n_texts = n_texts,
            tokens = tokens,
            "activity_emit: EmbeddingsServed"
        ),
        ActivityEventKind::LocalKnowledgeServed {
            corpus_id,
            chunks_returned,
        } => tracing::debug!(
            kind = "LocalKnowledgeServed",
            corpus_id = %corpus_id,
            chunks = chunks_returned,
            "activity_emit: LocalKnowledgeServed"
        ),
        ActivityEventKind::ChunksIngested {
            corpus_id,
            chunks,
            duration_secs,
        } => tracing::debug!(
            kind = "ChunksIngested",
            corpus_id = %corpus_id,
            chunks = chunks,
            duration_secs = duration_secs,
            "activity_emit: ChunksIngested"
        ),
        ActivityEventKind::CorpusEnriched {
            corpus_id,
            atoms,
            duration_secs,
        } => tracing::debug!(
            kind = "CorpusEnriched",
            corpus_id = %corpus_id,
            atoms = atoms,
            duration_secs = duration_secs,
            "activity_emit: CorpusEnriched"
        ),
        ActivityEventKind::NewsworthyFetched {
            articles,
            portal_ingested,
        } => tracing::debug!(
            kind = "NewsworthyFetched",
            articles = articles,
            portal_ingested = portal_ingested,
            "activity_emit: NewsworthyFetched"
        ),
    }
}

/// Convenience: build a [`ServedFor`] from an optional requester node
/// id (the shape every HTTP handler already has —
/// `parse_x_node_id(&headers)` returns `Option<NodeId>`).
pub fn served_for(requester: Option<NodeId>) -> ServedFor {
    match requester {
        Some(node_id) => ServedFor::Peer { node_id },
        None => ServedFor::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    #[test]
    fn emit_then_read_round_trips() {
        let store = MeshStore::in_memory().unwrap();
        let emitter = ActivityEmitter::new(store, nid(7));
        emitter.record(ActivityEventKind::ChunksIngested {
            corpus_id: "obsidian".into(),
            chunks: 3000,
            duration_secs: 120,
        });
        emitter.record(ActivityEventKind::EmbeddingsServed {
            served_for: ServedFor::Peer { node_id: nid(2) },
            n_texts: 64,
            tokens: 4096,
        });
        let events = emitter.events().unwrap();
        assert_eq!(events.len(), 2);
        for ev in &events {
            assert_eq!(ev.node_id, nid(7));
        }
    }

    #[test]
    fn rapid_fire_emits_do_not_collide() {
        let store = MeshStore::in_memory().unwrap();
        let emitter = ActivityEmitter::new(store, nid(7));
        for i in 0..500 {
            emitter.record(ActivityEventKind::ChunksIngested {
                corpus_id: format!("c{i}"),
                chunks: i as u64,
                duration_secs: 1,
            });
        }
        assert_eq!(emitter.events().unwrap().len(), 500);
    }

    #[test]
    fn current_activity_aggregates_stored_events() {
        let store = MeshStore::in_memory().unwrap();
        let emitter = ActivityEmitter::new(store.clone(), nid(7));
        emitter.record(ActivityEventKind::ChunksIngested {
            corpus_id: "obsidian".into(),
            chunks: 3000,
            duration_secs: 120,
        });
        emitter.record(ActivityEventKind::ChunksIngested {
            corpus_id: "obsidian".into(),
            chunks: 21,
            duration_secs: 5,
        });
        let summary = current_activity(&store, 7).unwrap();
        assert_eq!(summary.total_chunks_ingested, 3021);
        assert_eq!(summary.corpora.len(), 1);
        assert_eq!(summary.corpora[0].chunks_ingested, 3021);
        assert_eq!(summary.corpora[0].ingest_runs, 2);
    }

    #[test]
    fn served_for_maps_requester_option() {
        assert!(matches!(served_for(None), ServedFor::Local));
        assert!(matches!(
            served_for(Some(nid(3))),
            ServedFor::Peer { .. }
        ));
    }
}
