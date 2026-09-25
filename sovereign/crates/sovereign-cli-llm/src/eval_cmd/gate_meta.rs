// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gate's typed decision, off a persisted turn's message metadata.
//!
//! `eval run --synth` reads everything it scores out of the PERSISTED
//! assistant message (`runner.rs`), and the runtime writes the gate's
//! decision there under `metadata["grounding_gate"]` (knowledge_query.rs,
//! streaming.rs — `GateOutcome.meta`, whose `action` is the typed
//! `GateAction` id, byte-stable since 2026-08-26). Until 2026-09-22 the
//! eval row dropped it, so a board could not tell a gate abstention from
//! a retrieval miss after the fact — the ei7-ans board's `full` arm lost
//! k0 0.50 vs bare 0.83 and the run JSON carried no per-question reason.
//! This module is the read; same contract as `atlas_walk_meta`.

use serde::{Deserialize, Serialize};

/// The gate decision fields a study needs to attribute a row. All fields
/// optional: the metadata block is best-effort and a surface that never
/// gated writes no key, which is a different fact from a gate that ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateDecisionEcho {
    /// The typed action id (`GateAction::id`), e.g. `citation_grounded`,
    /// `abstained`, `annotated_marked`.
    pub action: Option<String>,
    /// Whether the gate ran a retry synthesis.
    pub retried: Option<bool>,
    /// The final violation probability, when measured.
    pub violation_prob: Option<f64>,
    /// Members of a multi-member value span the veto refused by name
    /// (member-split, 2026-09-22). Empty on every other verdict.
    #[serde(default)]
    pub unsupported_values: Vec<String>,
}

/// Read the turn's gate decision out of its persisted message metadata.
///
/// `None` means no gate decision reached this row: the key is absent (the
/// turn took a route that never gates — naked/closed-book, or a surface
/// without the gate) or `null`. A key that is present but unreadable is a
/// LOSS, not an absence (ARCH §6): it still yields `None`, because the
/// echo struct has nowhere else to go, but says so on stderr tagged with
/// the question id so the run's own log shows the loss.
pub fn gate_decision_from_metadata(
    metadata: Option<&serde_json::Value>,
    question_id: &str,
) -> Option<GateDecisionEcho> {
    let raw = metadata?.get("grounding_gate")?;
    if raw.is_null() {
        return None;
    }
    let obj = raw.as_object()?;
    let field = |name: &str| obj.get(name).cloned().filter(|v| !v.is_null());
    if field("action").is_none() && field("retried").is_none() {
        // Present but carries none of the decision fields — treat as
        // unreadable rather than as "ran and decided nothing", which is
        // not a state the gate produces.
        eprintln!(
            "  [{question_id}] grounding_gate metadata present but carries no \
             decision fields; this row reports NO gate decision and the gate \
             may have run"
        );
        return None;
    }
    Some(GateDecisionEcho {
        action: field("action").and_then(|v| v.as_str().map(str::to_string)),
        retried: field("retried").and_then(|v| v.as_bool()),
        violation_prob: field("violation_prob").and_then(|v| v.as_f64()),
        unsupported_values: field("unsupported_values")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_a_decision() {
        let metadata = json!({
            "model": "x",
            "grounding_gate": {
                "surface": "knowledge_query",
                "action": "annotated_marked",
                "retried": true,
                "violation_prob": 1.0,
                "threshold": 0.9,
                "mode": "single_claim",
                "unsupported_values": ["Lampsacus, Abydus"],
            }
        });
        let echo = gate_decision_from_metadata(Some(&metadata), "q1")
            .expect("a written decision must read back");
        assert_eq!(echo.action.as_deref(), Some("annotated_marked"));
        assert_eq!(echo.retried, Some(true));
        assert_eq!(echo.violation_prob, Some(1.0));
        assert_eq!(echo.unsupported_values, ["Lampsacus, Abydus"]);
    }

    #[test]
    fn absent_and_null_stay_distinct_from_unreadable() {
        assert!(
            gate_decision_from_metadata(None, "q1").is_none(),
            "no metadata: the turn never reached a handler"
        );
        assert!(
            gate_decision_from_metadata(Some(&json!({ "model": "x" })), "q1").is_none(),
            "key absent: the route never gated"
        );
        assert!(
            gate_decision_from_metadata(Some(&json!({ "grounding_gate": null })), "q1").is_none(),
            "key null: the plan carried no gate outcome"
        );
        // Present but decision-less: a loss, and still None — with the
        // stderr line this test cannot hear.
        assert!(gate_decision_from_metadata(
            Some(&json!({ "grounding_gate": { "surface": "knowledge_query" } })),
            "q1"
        )
        .is_none());
    }
}
