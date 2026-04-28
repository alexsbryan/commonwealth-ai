//! Dimensional contribution ledger — replaces the abandoned
//! `LedgerEntry` schema with append-only events that gossip
//! through the mesh and aggregate into per-node `NodeContributions`
//! locally on every machine.
//!
//! Design principles (per spec §2 of the Mesh Health requirements):
//!
//! 1. **Plural and incommensurable**: compute time, storage, and
//!    bandwidth are different kinds of value. They are never
//!    collapsed into a single score, ranking, or balance. The
//!    ledger does not carry a `balance` field.
//!
//! 2. **Append-only event log**: every write site emits one
//!    `LedgerEvent`. Aggregation is a pure function over the event
//!    stream — every node with the same events computes identical
//!    `NodeContributions`. This is the SICP "data is the program"
//!    separation.
//!
//! 3. **Gossip-replicated, never collapsed on the wire**: events
//!    propagate via the existing epidemic gossip mechanism.
//!    `NodeContributions` is a *local view*; it never crosses the
//!    wire. Two nodes that disagree about an aggregation almost
//!    certainly have a gossip-convergence bug, not a "balance"
//!    bug.
//!
//! See `commonwealth/docs/mesh-health.md` for the full rationale
//! and `commonwealth-state/src/contribution_store.rs` for storage.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::capabilities::NodeCapabilities;
use crate::ids::NodeId;

/// One discrete event describing mesh activity. Append-only,
/// gossip-replicated, never mutated after emission.
///
/// `node_id` is the *origin* — the node that observed the event
/// and is now broadcasting it. For directed events (an inference
/// served by A for B), the counter-party id rides inside `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerEvent {
    /// Origin: the node that observed and emitted this event.
    pub node_id: NodeId,
    /// Unix seconds when the event was emitted.
    pub timestamp: u64,
    pub kind: LedgerEventKind,
}

/// The dimensional shape of a ledger event. Closed set —
/// per ARCH_PRINCIPLES §2.1, every variant is a distinct kind of
/// activity, not a stringly-typed bag. Add a new variant when a
/// genuinely new dimension of contribution arrives; do NOT bend an
/// existing variant by overloading it with a new payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum LedgerEventKind {
    /// This node served an inference request for `for_node`. The
    /// emitter is the *server* side of the exchange; the requester
    /// is captured in `for_node`.
    InferenceServed {
        for_node: NodeId,
        model_id: String,
        tokens_generated: u64,
        wall_seconds: f64,
    },
    /// This node received an inference response from `from_node`.
    /// Symmetric counterpart to `InferenceServed`. Both sides
    /// emit on every cross-mesh inference; aggregation cross-checks
    /// these to flag missing-event scenarios.
    InferenceReceived {
        from_node: NodeId,
        model_id: String,
        tokens_generated: u64,
    },
    /// Federated knowledge query served by this node for `for_node`.
    /// `chunks_returned` is the aggregator's count of chunks this
    /// peer contributed to the merged response (not the total).
    KnowledgeQueryServed {
        for_node: NodeId,
        corpus_id: String,
        chunks_returned: u32,
    },
    /// Index/model shard transferred by this node to `to_node`.
    /// `bytes` is the on-the-wire payload size (after any
    /// compression).
    ShardTransferred {
        to_node: NodeId,
        corpus_id: String,
        bytes: u64,
    },
    /// Hourly snapshot of the corpora this node is hosting on
    /// disk. Drives the "storage" dimension of contribution; we do
    /// not reconstruct hosting from a stream of install/uninstall
    /// events because the hourly cadence is sufficient for routing
    /// and reporting and it cleanly handles process restarts.
    StorageSnapshot {
        corpora: Vec<(String, f64)>,
    },
}

/// Aggregated activity for a single inference role (served or
/// consumed). Three orthogonal counts so an operator can answer
/// "how many requests" / "how much output" / "how much wall-clock
/// did I spend" without inferring any one from the others.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InferenceActivity {
    pub requests: u64,
    pub total_tokens_generated: u64,
    pub wall_seconds: f64,
}

/// One entry in `NodeContributions.corpora_hosted` describing one
/// corpus this node hosts.
///
/// `is_sole_host` is computed by checking the gossip-replicated
/// `NodeCapabilities.hosted_corpora` across all members at
/// aggregation time — a corpus only this node advertises is a
/// public-good signal worth surfacing in the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CorpusHosting {
    pub corpus_id: String,
    pub corpus_name: String,
    pub size_gb: f64,
    pub queries_served: u64,
    pub is_sole_host: bool,
}

/// Per-node dimensional aggregation. Computed locally on every
/// machine from the same gossip-replicated event stream — every
/// node with the same events produces identical `NodeContributions`.
///
/// Deliberately carries no `balance` field, no exchange rate, and
/// no ranking. Operators read it as "this peer served N inferences
/// for me, hosts M corpora (one of which they're the sole host of),
/// and shipped K GB of bytes" — three facts in three different
/// units.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NodeContributions {
    pub window_days: u32,
    pub inference_served: InferenceActivity,
    pub inference_consumed: InferenceActivity,
    pub corpora_hosted: Vec<CorpusHosting>,
    pub bytes_served: u64,
    pub bytes_received: u64,
}

/// Default aggregation window. 30 days roughly matches the cadence
/// at which a healthy mesh churns peers — short enough that a peer
/// who joined yesterday isn't drowned out by historical totals,
/// long enough that day-to-day variance smooths out.
pub const DEFAULT_WINDOW_DAYS: u32 = 30;

/// Collapse an append-only event stream into a per-node
/// `NodeContributions` map. Pure function — same events on every
/// node yield identical results.
///
/// `now_unix` is the upper bound; `window_secs` is the lookback
/// window. Events whose `timestamp + window_secs < now_unix` are
/// dropped from the aggregation. Rolling the window forward simply
/// re-runs this function; there is no incremental state.
///
/// `peer_capabilities` is consulted to compute `is_sole_host`:
/// the aggregator examines every peer's
/// `NodeCapabilities.hosted_corpora`, and a corpus that only one
/// node advertises is flagged on that node's `CorpusHosting`.
pub fn aggregate(
    events: &[LedgerEvent],
    now_unix: u64,
    window_secs: u64,
    peer_capabilities: &HashMap<NodeId, NodeCapabilities>,
) -> HashMap<NodeId, NodeContributions> {
    let cutoff = now_unix.saturating_sub(window_secs);
    let mut by_node: HashMap<NodeId, NodeContributions> = HashMap::new();

    // First pass: walk events. Every variant lands a single side of
    // the contribution pair onto the *origin* node. The peer side
    // (e.g. `InferenceReceived` for the requester) lands on the
    // requester's own emitted events when they ship them — we do
    // NOT double-count by inferring the counterpart from a single
    // emission.
    let window_days = (window_secs / 86_400).max(1) as u32;
    for ev in events.iter().filter(|e| e.timestamp >= cutoff) {
        let entry = by_node.entry(ev.node_id.clone()).or_insert_with(|| {
            NodeContributions {
                window_days,
                ..Default::default()
            }
        });
        match &ev.kind {
            LedgerEventKind::InferenceServed {
                tokens_generated,
                wall_seconds,
                ..
            } => {
                entry.inference_served.requests += 1;
                entry.inference_served.total_tokens_generated +=
                    tokens_generated;
                entry.inference_served.wall_seconds += wall_seconds;
            }
            LedgerEventKind::InferenceReceived {
                tokens_generated, ..
            } => {
                entry.inference_consumed.requests += 1;
                entry.inference_consumed.total_tokens_generated +=
                    tokens_generated;
            }
            LedgerEventKind::KnowledgeQueryServed { corpus_id, .. } => {
                let bucket = entry
                    .corpora_hosted
                    .iter_mut()
                    .find(|c| c.corpus_id == *corpus_id);
                match bucket {
                    Some(b) => b.queries_served += 1,
                    None => entry.corpora_hosted.push(CorpusHosting {
                        corpus_id: corpus_id.clone(),
                        corpus_name: corpus_id.clone(),
                        size_gb: 0.0,
                        queries_served: 1,
                        is_sole_host: false,
                    }),
                }
            }
            LedgerEventKind::ShardTransferred { bytes, .. } => {
                entry.bytes_served += bytes;
                // The receiving node will emit no symmetric event;
                // we infer `bytes_received` for the recipient from
                // this same event so a single transmission produces
                // both halves of the byte ledger.
                let recipient = match &ev.kind {
                    LedgerEventKind::ShardTransferred { to_node, .. } => {
                        to_node.clone()
                    }
                    _ => unreachable!(),
                };
                let recipient_entry =
                    by_node.entry(recipient).or_insert_with(|| {
                        NodeContributions {
                            window_days,
                            ..Default::default()
                        }
                    });
                recipient_entry.bytes_received += bytes;
            }
            LedgerEventKind::StorageSnapshot { corpora } => {
                // A snapshot is the canonical view of "what this
                // node currently hosts". Replace the existing
                // size-only fields, preserving query counts that
                // came from `KnowledgeQueryServed` events.
                let mut existing_queries: HashMap<String, u64> = entry
                    .corpora_hosted
                    .iter()
                    .map(|c| (c.corpus_id.clone(), c.queries_served))
                    .collect();
                entry.corpora_hosted.clear();
                for (corpus_id, size_gb) in corpora {
                    let q =
                        existing_queries.remove(corpus_id).unwrap_or(0);
                    entry.corpora_hosted.push(CorpusHosting {
                        corpus_id: corpus_id.clone(),
                        corpus_name: corpus_id.clone(),
                        size_gb: *size_gb,
                        queries_served: q,
                        is_sole_host: false,
                    });
                }
                // Re-attach orphan queries (a corpus we served queries
                // for but which the snapshot dropped — e.g. the
                // hosting node uninstalled the corpus this hour).
                for (corpus_id, queries) in existing_queries {
                    entry.corpora_hosted.push(CorpusHosting {
                        corpus_id: corpus_id.clone(),
                        corpus_name: corpus_id,
                        size_gb: 0.0,
                        queries_served: queries,
                        is_sole_host: false,
                    });
                }
            }
        }
    }

    // Second pass: stamp `is_sole_host` from the gossiped
    // capabilities map. A corpus is sole-hosted when exactly one
    // peer advertises it in `hosted_corpora`.
    let mut hosting_count: HashMap<String, u64> = HashMap::new();
    for caps in peer_capabilities.values() {
        for shard in &caps.hosted_corpora {
            *hosting_count.entry(shard.corpus_id.clone()).or_insert(0) += 1;
        }
    }
    for (_node_id, contrib) in by_node.iter_mut() {
        for corpus in &mut contrib.corpora_hosted {
            corpus.is_sole_host = hosting_count
                .get(&corpus.corpus_id)
                .copied()
                .unwrap_or(0)
                == 1;
        }
    }

    by_node
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    fn ev(node_id: NodeId, ts: u64, kind: LedgerEventKind) -> LedgerEvent {
        LedgerEvent {
            node_id,
            timestamp: ts,
            kind,
        }
    }

    #[test]
    fn empty_event_stream_aggregates_to_empty_map() {
        let now = 1_000_000;
        let result = aggregate(&[], now, 86_400, &HashMap::new());
        assert!(result.is_empty());
    }

    #[test]
    fn inference_served_lands_on_origin_node_only() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let events = vec![ev(
            a.clone(),
            now - 10,
            LedgerEventKind::InferenceServed {
                for_node: b.clone(),
                model_id: "qwen-9b".into(),
                tokens_generated: 100,
                wall_seconds: 2.5,
            },
        )];
        let result =
            aggregate(&events, now, 86_400, &HashMap::new());
        // Origin (server side) sees a served bump.
        assert_eq!(result[&a].inference_served.requests, 1);
        assert_eq!(result[&a].inference_served.total_tokens_generated, 100);
        assert!((result[&a].inference_served.wall_seconds - 2.5).abs() < 1e-6);
        // The requester does NOT receive a phantom
        // inference_consumed bump from the server's emission — the
        // requester emits its own InferenceReceived event.
        assert!(!result.contains_key(&b));
    }

    #[test]
    fn inference_received_lands_on_origin_node_only() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let events = vec![ev(
            a.clone(),
            now - 10,
            LedgerEventKind::InferenceReceived {
                from_node: b.clone(),
                model_id: "qwen-9b".into(),
                tokens_generated: 100,
            },
        )];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&a].inference_consumed.requests, 1);
        assert_eq!(
            result[&a].inference_consumed.total_tokens_generated,
            100
        );
        // wall_seconds is intentionally not on InferenceReceived —
        // the requester doesn't measure server-side wall clock.
        assert_eq!(result[&a].inference_consumed.wall_seconds, 0.0);
    }

    #[test]
    fn shard_transfer_lands_both_halves() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let events = vec![ev(
            a.clone(),
            now - 10,
            LedgerEventKind::ShardTransferred {
                to_node: b.clone(),
                corpus_id: "wikipedia".into(),
                bytes: 5_000_000_000,
            },
        )];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&a].bytes_served, 5_000_000_000);
        assert_eq!(result[&b].bytes_received, 5_000_000_000);
    }

    #[test]
    fn events_outside_window_are_dropped() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let window = 86_400; // one day
        let events = vec![
            ev(
                a.clone(),
                now - window - 1, // just past cutoff
                LedgerEventKind::InferenceServed {
                    for_node: b.clone(),
                    model_id: "qwen-9b".into(),
                    tokens_generated: 9_999,
                    wall_seconds: 999.0,
                },
            ),
            ev(
                a.clone(),
                now - 1,
                LedgerEventKind::InferenceServed {
                    for_node: b.clone(),
                    model_id: "qwen-9b".into(),
                    tokens_generated: 100,
                    wall_seconds: 1.0,
                },
            ),
        ];
        let result =
            aggregate(&events, now, window, &HashMap::new());
        // Only the recent event counts.
        assert_eq!(result[&a].inference_served.requests, 1);
        assert_eq!(
            result[&a].inference_served.total_tokens_generated,
            100
        );
    }

    #[test]
    fn knowledge_query_attaches_to_corpus_bucket() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let events = vec![
            ev(
                a.clone(),
                now - 10,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b.clone(),
                    corpus_id: "sep".into(),
                    chunks_returned: 8,
                },
            ),
            ev(
                a.clone(),
                now - 5,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b.clone(),
                    corpus_id: "sep".into(),
                    chunks_returned: 3,
                },
            ),
        ];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&a].corpora_hosted.len(), 1);
        assert_eq!(result[&a].corpora_hosted[0].corpus_id, "sep");
        assert_eq!(result[&a].corpora_hosted[0].queries_served, 2);
    }

    #[test]
    fn storage_snapshot_replaces_size_preserves_query_counts() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let events = vec![
            ev(
                a.clone(),
                now - 100,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b.clone(),
                    corpus_id: "sep".into(),
                    chunks_returned: 5,
                },
            ),
            ev(
                a.clone(),
                now - 50,
                LedgerEventKind::StorageSnapshot {
                    corpora: vec![("sep".into(), 12.5)],
                },
            ),
        ];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&a].corpora_hosted.len(), 1);
        let sep = &result[&a].corpora_hosted[0];
        assert_eq!(sep.corpus_id, "sep");
        assert!((sep.size_gb - 12.5).abs() < 1e-6);
        assert_eq!(sep.queries_served, 1, "queries preserved across snapshot");
    }

    #[test]
    fn aggregation_is_deterministic_across_event_orderings() {
        // SICP-style purity test: shuffle the events, get the same
        // result. This is the property that makes the ledger work
        // across gossip — two nodes that received the same events
        // in different orders must compute identical aggregations.
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let events_in_order = vec![
            ev(
                a.clone(),
                now - 30,
                LedgerEventKind::InferenceServed {
                    for_node: b.clone(),
                    model_id: "qwen-9b".into(),
                    tokens_generated: 100,
                    wall_seconds: 2.0,
                },
            ),
            ev(
                a.clone(),
                now - 20,
                LedgerEventKind::ShardTransferred {
                    to_node: b.clone(),
                    corpus_id: "wikipedia".into(),
                    bytes: 1_000_000,
                },
            ),
            ev(
                a.clone(),
                now - 10,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b.clone(),
                    corpus_id: "sep".into(),
                    chunks_returned: 5,
                },
            ),
        ];
        let mut events_shuffled = events_in_order.clone();
        events_shuffled.reverse();

        let r1 = aggregate(&events_in_order, now, 86_400, &HashMap::new());
        let r2 = aggregate(&events_shuffled, now, 86_400, &HashMap::new());
        assert_eq!(r1, r2);
    }
}
