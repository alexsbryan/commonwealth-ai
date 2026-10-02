// SPDX-License-Identifier: AGPL-3.0-or-later
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

use oicp_types::JobKind;

use crate::capabilities::NodeCapabilities;
use crate::ids::{HandoffId, NodeId};

// The records live in `oicp_types::contributions` since pb-mesh-exit-core
// (gossip-replicated, served through cw-rails' ledger doors); the aggregation
// below is the decision and stays. Re-exported so every path resolves.
pub use oicp_types::contributions::*;

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
        let entry = by_node
            .entry(ev.node_id)
            .or_insert_with(|| NodeContributions {
                window_days,
                ..Default::default()
            });
        match &ev.kind {
            LedgerEventKind::InferenceServed {
                tokens_generated,
                wall_seconds,
                ..
            } => {
                entry.inference_served.requests += 1;
                entry.inference_served.total_tokens_generated += tokens_generated;
                entry.inference_served.wall_seconds += wall_seconds;
            }
            LedgerEventKind::InferenceReceived {
                tokens_generated, ..
            } => {
                entry.inference_consumed.requests += 1;
                entry.inference_consumed.total_tokens_generated += tokens_generated;
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
            LedgerEventKind::ShardTransferred {
                from_node,
                to_node,
                bytes,
                ..
            } => {
                // ShardTransferred is the one variant where `ev.node_id`
                // (the EMITTER) is not necessarily the actor on either
                // side: the merge-leader puller emits on behalf of the
                // peer that shipped the bytes, since the peer never
                // observes the transfer completing. Bucket bytes_served
                // onto `from_node` and bytes_received onto `to_node`,
                // ignoring the emitter for accounting purposes. The
                // emitter is preserved in the event itself for
                // provenance / debugging via tracing.
                //
                // The default `entry` we just opened above is keyed on
                // `ev.node_id`. Replace that bookkeeping with the two
                // explicit nodes from `kind`.
                let _ = entry; // see comment above — emitter bucket unused
                let sender_entry = by_node
                    .entry(*from_node)
                    .or_insert_with(|| NodeContributions {
                        window_days,
                        ..Default::default()
                    });
                sender_entry.bytes_served += bytes;
                let recipient_entry =
                    by_node
                        .entry(*to_node)
                        .or_insert_with(|| NodeContributions {
                            window_days,
                            ..Default::default()
                        });
                recipient_entry.bytes_received += bytes;
            }
            LedgerEventKind::JobUnitCompleted { wall_seconds, .. } => {
                // Credits the ORIGIN, which is `InferenceServed`'s rule and
                // not `ShardTransferred`'s: the donor is the one node that
                // ran the unit AND the one node that emits, so the emitter
                // and the actor are the same machine by construction. The
                // counter-party (the submitter) is reachable through
                // `handoff` on the work journal and is deliberately not
                // copied onto a second node's bucket here — nothing was
                // donated TO a submitter in a unit this ledger counts, and
                // inventing a `work_received` from one emission would be the
                // phantom counterpart `inference_served` refuses.
                entry.compute_donated.units += 1;
                entry.compute_donated.wall_seconds += wall_seconds;
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
                    let q = existing_queries.remove(corpus_id).unwrap_or(0);
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
            corpus.is_sole_host = hosting_count.get(&corpus.corpus_id).copied().unwrap_or(0) == 1;
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
            a,
            now - 10,
            LedgerEventKind::InferenceServed {
                for_node: b,
                model_id: "qwen-9b".into(),
                tokens_generated: 100,
                wall_seconds: 2.5,
            },
        )];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
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
            a,
            now - 10,
            LedgerEventKind::InferenceReceived {
                from_node: b,
                model_id: "qwen-9b".into(),
                tokens_generated: 100,
            },
        )];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&a].inference_consumed.requests, 1);
        assert_eq!(result[&a].inference_consumed.total_tokens_generated, 100);
        // wall_seconds is intentionally not on InferenceReceived —
        // the requester doesn't measure server-side wall clock.
        assert_eq!(result[&a].inference_consumed.wall_seconds, 0.0);
    }

    #[test]
    fn shard_transfer_lands_both_halves() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        // Emitter (`a`) is also the sender — the legacy stream_index
        // emission shape, where the sender's daemon records on its
        // own behalf. The merge-leader pull case is exercised by
        // `pull_emitted_shard_transfer_credits_actual_sender` below.
        let events = vec![ev(
            a,
            now - 10,
            LedgerEventKind::ShardTransferred {
                from_node: a,
                to_node: b,
                corpus_id: "wikipedia".into(),
                bytes: 5_000_000_000,
            },
        )];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&a].bytes_served, 5_000_000_000);
        assert_eq!(result[&b].bytes_received, 5_000_000_000);
    }

    #[test]
    fn pull_emitted_shard_transfer_credits_actual_sender() {
        // The merge-leader pull case: emitter is `puller` (the
        // recipient), the actual sender is `peer`. Aggregator must
        // bucket bytes_served onto `peer` (the sender), not onto the
        // emitter, and bytes_received onto `puller` (the recipient).
        let peer = nid(1);
        let puller = nid(2);
        let now = 1_000_000;
        let events = vec![ev(
            puller,
            now - 10,
            LedgerEventKind::ShardTransferred {
                from_node: peer,
                to_node: puller,
                corpus_id: "wikipedia".into(),
                bytes: 1_500_000_000,
            },
        )];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&peer].bytes_served, 1_500_000_000);
        assert_eq!(result[&puller].bytes_received, 1_500_000_000);
        // The emitter's bytes_served is NOT incremented just because
        // they wrote the event — the emitter's accounting is
        // determined entirely by `from_node`.
        assert_eq!(result[&puller].bytes_served, 0);
    }

    #[test]
    fn events_outside_window_are_dropped() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let window = 86_400; // one day
        let events = vec![
            ev(
                a,
                now - window - 1, // just past cutoff
                LedgerEventKind::InferenceServed {
                    for_node: b,
                    model_id: "qwen-9b".into(),
                    tokens_generated: 9_999,
                    wall_seconds: 999.0,
                },
            ),
            ev(
                a,
                now - 1,
                LedgerEventKind::InferenceServed {
                    for_node: b,
                    model_id: "qwen-9b".into(),
                    tokens_generated: 100,
                    wall_seconds: 1.0,
                },
            ),
        ];
        let result = aggregate(&events, now, window, &HashMap::new());
        // Only the recent event counts.
        assert_eq!(result[&a].inference_served.requests, 1);
        assert_eq!(result[&a].inference_served.total_tokens_generated, 100);
    }

    #[test]
    fn knowledge_query_attaches_to_corpus_bucket() {
        let a = nid(1);
        let b = nid(2);
        let now = 1_000_000;
        let events = vec![
            ev(
                a,
                now - 10,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b,
                    corpus_id: "sep".into(),
                    chunks_returned: 8,
                },
            ),
            ev(
                a,
                now - 5,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b,
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
                a,
                now - 100,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b,
                    corpus_id: "sep".into(),
                    chunks_returned: 5,
                },
            ),
            ev(
                a,
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

    fn kind(s: &str) -> JobKind {
        JobKind::parse(s).expect("a valid `id:vN` kind")
    }

    fn work_done(seconds: f64) -> LedgerEventKind {
        LedgerEventKind::JobUnitCompleted {
            handoff: HandoffId::from_u128(7),
            unit_hash: "a".repeat(64),
            donor_actor: "3".repeat(64),
            kind: kind("process:v1"),
            wall_seconds: seconds,
        }
    }

    /// **The credit itself.** The failing input is a ledger with a
    /// `JobUnitCompleted` in it and a `compute_donated` that stays zero — which
    /// is what the work plane did through all of 5d and 5e: it ran other
    /// people's compute and recorded nothing about who paid for it.
    #[test]
    fn a_completed_work_unit_credits_the_donor_that_emitted_it() {
        let donor = nid(1);
        let now = 1_000_000;
        let events = vec![ev(donor, now - 10, work_done(42.5))];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&donor].compute_donated.units, 1);
        assert!((result[&donor].compute_donated.wall_seconds - 42.5).abs() < 1e-6);
    }

    /// Principle 1 — plural and incommensurable. A donated CI shard is not
    /// an inference request, is not a byte and is not a hosted gigabyte, and
    /// the failing input is an `aggregate` that folded the new dimension into
    /// an old bucket to avoid adding a field.
    #[test]
    fn a_work_credit_lands_in_no_other_dimension() {
        let donor = nid(1);
        let now = 1_000_000;
        let result = aggregate(
            &[ev(donor, now - 10, work_done(9.0))],
            now,
            86_400,
            &HashMap::new(),
        );
        let c = &result[&donor];
        assert_eq!(c.inference_served, InferenceActivity::default());
        assert_eq!(c.inference_consumed, InferenceActivity::default());
        assert_eq!(c.bytes_served, 0);
        assert_eq!(c.bytes_received, 0);
        assert!(c.corpora_hosted.is_empty());
    }

    /// The counterpart `ShardTransferred` HAS and this variant deliberately
    /// does not. A submitter is not credited for work somebody else ran, and
    /// no third node appears in the map off one donor's emission — the same
    /// no-phantom-counterpart rule
    /// `inference_served_lands_on_origin_node_only` pins for inference.
    #[test]
    fn a_work_credit_opens_no_bucket_for_anybody_but_the_donor() {
        let donor = nid(1);
        let now = 1_000_000;
        let result = aggregate(
            &[ev(donor, now - 10, work_done(1.0))],
            now,
            86_400,
            &HashMap::new(),
        );
        assert_eq!(result.len(), 1, "one emission, one credited node");
        assert!(result.contains_key(&donor));
    }

    /// Two runs are two credits, and that is correct rather than a
    /// double-count: a unit whose report lapsed is re-leased and re-run, and
    /// two machines really did spend the time. The idempotence that matters
    /// is per-RUN and it lives at the emit site — the rail's at-least-once
    /// redelivery of one `Complete` never reaches this log, because nothing
    /// here is derived from the journal (see `work_donor::credit_for`).
    #[test]
    fn two_runs_of_one_unit_are_two_credits_on_two_donors() {
        let first = nid(1);
        let second = nid(2);
        let now = 1_000_000;
        let events = vec![
            ev(first, now - 100, work_done(3.0)),
            ev(second, now - 10, work_done(5.0)),
        ];
        let result = aggregate(&events, now, 86_400, &HashMap::new());
        assert_eq!(result[&first].compute_donated.units, 1);
        assert_eq!(result[&second].compute_donated.units, 1);
        assert!((result[&second].compute_donated.wall_seconds - 5.0).abs() < 1e-6);
    }

    #[test]
    fn a_work_credit_outside_the_window_is_dropped_like_every_other_event() {
        let donor = nid(1);
        let now = 1_000_000;
        let window = 86_400;
        let events = vec![
            ev(donor, now - window - 1, work_done(999.0)),
            ev(donor, now - 1, work_done(2.0)),
        ];
        let result = aggregate(&events, now, window, &HashMap::new());
        assert_eq!(result[&donor].compute_donated.units, 1);
        assert!((result[&donor].compute_donated.wall_seconds - 2.0).abs() < 1e-6);
    }

    /// The wire form carries the audit trail, so a reader holding a stored
    /// event can go find the signed `Complete` it claims. The failing input
    /// is a variant that serialized the donor key away.
    #[test]
    fn a_work_credit_round_trips_with_its_rail_pointers_intact() {
        let event = LedgerEvent {
            node_id: nid(1),
            timestamp: 1_000,
            kind: work_done(7.5),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains(&"3".repeat(64)), "the donor key must survive");
        assert!(json.contains("process:v1"), "the kind must survive");
        let back: LedgerEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, event);
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
                a,
                now - 30,
                LedgerEventKind::InferenceServed {
                    for_node: b,
                    model_id: "qwen-9b".into(),
                    tokens_generated: 100,
                    wall_seconds: 2.0,
                },
            ),
            ev(
                a,
                now - 20,
                LedgerEventKind::ShardTransferred {
                    from_node: a,
                    to_node: b,
                    corpus_id: "wikipedia".into(),
                    bytes: 1_000_000,
                },
            ),
            ev(
                a,
                now - 10,
                LedgerEventKind::KnowledgeQueryServed {
                    for_node: b,
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
