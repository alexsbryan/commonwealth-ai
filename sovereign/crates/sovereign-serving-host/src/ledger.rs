// SPDX-License-Identifier: AGPL-3.0-or-later
//! The contribution-ledger port.
//!
//! `quality/DAEMON_CORE.md` §4.2 "The facts rule": no context outside Fabric
//! names `ContributionEmitter`. Serving emits FACTS; Fabric prices them. The
//! host MINTS the fact a completed [`RoutingOutcome`] describes — a peer
//! served a stream of N tokens from a node under a model id — and hands it to
//! this port, which the daemon implements over its `ContributionEmitter`.
//!
//! `sovereign/SERVING_BOUNDARY.md` (a): `ledger_emission_for` came off the
//! roster port and the host mints from `RoutingOutcome` instead. The record
//! already carries who served and how many tokens, so nothing needs to be
//! threaded from the routing decision to the stream wrapper — the facts and
//! the emitter arrive on different sides of this seam, which is the whole
//! split.
//!
//! Absence is reported, never defaulted: a host with no ledger answers `None`
//! and nothing is emitted.

use kernel_types::NodeId;
use sovereign_scheduler::decision_log::{RoutingOutcome, ServedBy};

/// The port the daemon implements over its `ContributionEmitter`.
pub trait LedgerEmitter: Send + Sync {
    /// Record that a peer-routed inference completed: `tokens_generated`
    /// tokens of `model_id` were received from `from_node`.
    fn record_inference_received(&self, from_node: &NodeId, model_id: &str, tokens_generated: u64);
}

/// Mint the ledger fact a completed [`RoutingOutcome`] describes and hand it
/// to `emitter`.
///
/// Only a peer-served request with at least one token is a ledger fact: a
/// local serve is intra-mesh-only (spec §10 — a "received from self" event is
/// meaningless), a failure served nothing, and a zero-token dispatch
/// generated nothing. A `ServedBy::Peer` whose node id is absent or
/// unparsable is reported as no fact rather than guessed.
pub fn emit_from_outcome(emitter: &dyn LedgerEmitter, outcome: &RoutingOutcome) {
    let ServedBy::Peer {
        node_id, model_id, ..
    } = &outcome.served_by
    else {
        return;
    };
    let Some(tokens) = outcome.output_tokens.filter(|n| *n > 0) else {
        return;
    };
    let Some(from_node) = node_id.as_deref().and_then(NodeId::from_hex) else {
        tracing::debug!(
            target: "throughput_ledger",
            %model_id,
            "ledger: peer outcome carries no parsable node id — no emission"
        );
        return;
    };
    tracing::debug!(
        target: "throughput_ledger",
        %from_node,
        %model_id,
        tokens,
        "ledger: minting InferenceReceived from the routing outcome"
    );
    emitter.record_inference_received(&from_node, model_id, tokens);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_scheduler::decision_log::DECISION_LOG_SCHEMA;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingLedger(Mutex<Vec<(NodeId, String, u64)>>);

    impl LedgerEmitter for RecordingLedger {
        fn record_inference_received(&self, from_node: &NodeId, model_id: &str, tokens: u64) {
            self.0
                .lock()
                .unwrap()
                .push((*from_node, model_id.to_string(), tokens));
        }
    }

    fn outcome(served_by: ServedBy, output_tokens: Option<u64>) -> RoutingOutcome {
        RoutingOutcome {
            schema: DECISION_LOG_SCHEMA.to_string(),
            decision_id: "d".into(),
            oicp_request_id: "r".into(),
            ts_unix_ms: 1,
            served_by,
            attempt_index: 0,
            ttft_ms: Some(1.0),
            total_ms: Some(2.0),
            output_tokens,
            shed: false,
            error: None,
            failovers: Vec::new(),
        }
    }

    fn peer(node_id: NodeId) -> ServedBy {
        ServedBy::Peer {
            name: "hub".into(),
            node_id: Some(node_id.to_hex()),
            model_id: "m".into(),
        }
    }

    /// Positive control: a peer-served outcome with tokens mints exactly one
    /// fact, carrying the peer's node id, the model id and the token count.
    #[test]
    fn a_peer_outcome_with_tokens_mints_one_fact() {
        let id = NodeId::from_u128(0x1234);
        let ledger = RecordingLedger::default();
        emit_from_outcome(&ledger, &outcome(peer(id), Some(7)));
        assert_eq!(
            ledger.0.lock().unwrap().as_slice(),
            &[(id, "m".to_string(), 7)]
        );
    }

    /// Negative control: a local serve is not a ledger fact (spec §10).
    #[test]
    fn a_local_outcome_mints_nothing() {
        let ledger = RecordingLedger::default();
        emit_from_outcome(
            &ledger,
            &outcome(
                ServedBy::Local {
                    model_id: "m".into(),
                },
                Some(7),
            ),
        );
        assert!(ledger.0.lock().unwrap().is_empty());
    }

    /// Negative control: a peer outcome with no tokens (or none recorded)
    /// mints nothing — the same `count > 0` gate the stream wrapper had.
    #[test]
    fn a_zero_token_peer_outcome_mints_nothing() {
        let ledger = RecordingLedger::default();
        emit_from_outcome(&ledger, &outcome(peer(NodeId::from_u128(1)), Some(0)));
        emit_from_outcome(&ledger, &outcome(peer(NodeId::from_u128(1)), None));
        assert!(ledger.0.lock().unwrap().is_empty());
    }

    /// Negative control: a peer outcome whose node id is absent or
    /// unparsable is reported as no fact, never guessed.
    #[test]
    fn a_peer_outcome_without_a_parsable_node_id_mints_nothing() {
        let ledger = RecordingLedger::default();
        for node_id in [None, Some("not-hex".to_string())] {
            emit_from_outcome(
                &ledger,
                &outcome(
                    ServedBy::Peer {
                        name: "hub".into(),
                        node_id,
                        model_id: "m".into(),
                    },
                    Some(7),
                ),
            );
        }
        assert!(ledger.0.lock().unwrap().is_empty());
    }
}
