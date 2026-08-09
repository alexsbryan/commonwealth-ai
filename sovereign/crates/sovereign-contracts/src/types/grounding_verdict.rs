// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed grounding verdict — `NATIVE_GROUNDING.md §6`, verbatim.
//!
//! **What this replaces.** Abstention is decided in five places today and
//! rendered in three, coupled by a string namespace on `meta["action"]`
//! (§2). Eleven literals are in flight — `released`, `retry_released`,
//! `retry_released_specifics`, `retry_released_unverified`,
//! `judge_failed_open`, and six `abstained*` variants — and every consumer
//! reads them with `starts_with("abstained")` or a `matches!` arm. That is
//! smell-table row one (a `match` on string ids with more than 3 arms) at
//! the widest point in the runtime.
//!
//! [`GroundingVerdict`] is the one decider's output. On main it has
//! exactly one producer — H1's answerability admission stage
//! (`sovereign-core/src/runtime/grounding/native_grounding/admission.rs`),
//! and only when `SOVEREIGN_NATIVE_GROUNDING=1`. H2's agreement gate is
//! not built here, so `semantic_entropy` and `agreement` are `None` on
//! every verdict this workspace currently mints — the honest reading of
//! "not run" (ARCH §18.3, absence is reported, never defaulted).
//!
//! **The shim is the only writer of action strings on the native path.**
//! [`GroundingVerdict::to_gate_action`] emits the legacy literals so the
//! existing consumers keep working unchanged during the transition
//! (line numbers verified against main at 5bbceb82):
//!
//!   * `epistemic.rs:85` — `gate_action.starts_with("abstained")`, which
//!     decides whether the turn contributes ledger holdings at all;
//!   * `collaboration.rs:518` and `streaming.rs:1724,1928`, same predicate;
//!   * `handlers/knowledge_query.rs:1652,1822`, `handlers/simple.rs:221`,
//!     `handlers/complex_task.rs:369`, same predicate;
//!   * `collaboration.rs:671` — `unwrap_or("released")`, i.e. "released" is
//!     already this system's word for *a turn that released text*;
//!   * `epistemic.rs:71` `action_is_fail_open` — `judge_failed_open` and the
//!     two `*_released_unverified` literals. A native verdict is never
//!     fail-open (there is no judge to fail), so the shim never emits them,
//!     and that is asserted below rather than left to the next reader.
//!
//! Deleting the shim at graduation is the final cutover (§6).

use serde::{Deserialize, Serialize};
use std::ops::Range;

/// The three-way routing decision, decided ONCE — by H1's answerability
/// router, revisable only by H2's agreement gate (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingDecision {
    /// Proceed to generation, evidence-constrained (H3).
    Answer,
    /// Proceed, but the answer is born as `Parametric`-typed segments with
    /// the structural general-knowledge caveat prefix committed.
    Hedge,
    /// Emit the verdict *before* generation; the coverage probe and
    /// acquisition resolver run as they do today.
    Abstain,
}

/// Which mechanism decided. Glassbox: every decision names its decider
/// (§3 principle 5, ARCH §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeciderId {
    /// H1's answerability router, pre-generation.
    Router,
    /// H2's k-sample agreement / semantic-entropy gate.
    AgreementGate,
    /// The one surviving big-model judgment: a `forced_choice_ab` probe in
    /// the calibrated uncertainty band.
    Escalation,
    /// A code-enforced invariant fired (a span failed to resolve, a
    /// constraint refused) — no score was consulted.
    Structural,
}

/// The provenance of one stretch of released text (H4).
///
/// Closed set, so it is an enum, not a string tag (ARCH §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SegmentKind {
    /// Resolved verbatim (or by the ≥2-word phrase shortcut) inside the
    /// sealed evidence, at a real address.
    Grounded {
        /// The chunk the span was resolved in.
        chunk_id: String,
        /// Byte range within that chunk's text.
        span: Range<usize>,
    },
    /// The model's own parametric knowledge — the thing the caveat is a
    /// *rendering* of.
    Parametric,
    /// Derived from evidence rather than copied from it.
    Inference,
    /// Claimed grounded and failed to resolve. Demoted, never silently
    /// released as grounded (§5 H4, ARCH §18.3).
    Unverified,
}

/// One typed stretch of the released answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerSegment {
    /// Byte range within the released answer text.
    pub text_range: Range<usize>,
    /// What this stretch rests on.
    pub kind: SegmentKind,
    /// Reranker sentence margin, when this segment was scored.
    pub margin: Option<f32>,
}

/// One decider's output. Everything downstream reads this (§6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroundingVerdict {
    /// Three-way, decided ONCE (H1 routing, revisable only by H2
    /// agreement).
    pub decision: GroundingDecision,
    /// Calibrated answerability from the containment scorer (H1). 0..1.
    pub answerability: f32,
    /// Semantic entropy over meaning-clusters from the k-sample gate (H2),
    /// when run. 0 = unanimous; log(k) = full divergence.
    pub semantic_entropy: Option<f32>,
    /// Largest-cluster fraction (the degenerate cheap statistic). 0..1.
    pub agreement: Option<f32>,
    /// Which mechanism decided.
    pub decided_by: DeciderId,
    /// Per-segment provenance of the released text (H4).
    #[serde(default)]
    pub segments: Vec<AnswerSegment>,
}

/// The legacy action string for a turn that released text.
///
/// Named rather than inlined so the shim has exactly one writer of it —
/// ARCH §10.6, one decider one name.
const ACTION_RELEASED: &str = "released";
/// The legacy action string for a turn that asserted nothing.
///
/// Deliberately the BARE literal, not one of the six `abstained_*`
/// variants: those encode *why the incumbent ladder gave up* (retry
/// exhausted, weak evidence, a decline detected in prose), and a native
/// verdict has no such history to report. Every consumer tests
/// `starts_with("abstained")`, so the bare form satisfies all of them
/// without inventing a reason that did not happen (§18.6, never silently
/// substitute).
const ACTION_ABSTAINED: &str = "abstained";

impl GroundingVerdict {
    /// The compatibility shim (§6): emit the legacy `meta["action"]` string
    /// this verdict corresponds to.
    ///
    /// `Hedge` maps to `released` on purpose. The legacy namespace has no
    /// hedge literal — the incumbent expresses a caveated answer as a
    /// released one plus caveat prose, which `classify_caveat` then reads
    /// back out. Under this design the caveat is a *segment type*
    /// ([`SegmentKind::Parametric`]) and measuring it is a field read (§3
    /// principle 4), so the distinction lives on the verdict and the legacy
    /// channel carries only what it always carried: did the turn release
    /// text, or abstain.
    pub fn to_gate_action(&self) -> &'static str {
        match self.decision {
            GroundingDecision::Answer | GroundingDecision::Hedge => ACTION_RELEASED,
            GroundingDecision::Abstain => ACTION_ABSTAINED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(decision: GroundingDecision) -> GroundingVerdict {
        GroundingVerdict {
            decision,
            answerability: 0.5,
            semantic_entropy: None,
            agreement: None,
            decided_by: DeciderId::Router,
            segments: Vec::new(),
        }
    }

    /// The predicate at `sovereign-core/src/runtime/epistemic.rs:85`:
    ///
    /// ```text
    /// let abstained = gate_action.starts_with("abstained");
    /// ```
    ///
    /// Reproduced here rather than imported: `sovereign-contracts` is below
    /// `sovereign-core` in the layer map, so the test states the predicate
    /// it must satisfy and cites where the real one lives.
    fn epistemic_rs_85_abstained(gate_action: &str) -> bool {
        gate_action.starts_with("abstained")
    }

    /// `sovereign-core/src/runtime/epistemic.rs:71` `action_is_fail_open`.
    fn epistemic_rs_71_fail_open(action: &str) -> bool {
        matches!(
            action,
            "judge_failed_open" | "retry_released_unverified" | "rewrite_released_unverified"
        )
    }

    #[test]
    fn abstain_is_the_only_decision_the_ledger_reads_as_abstained() {
        assert!(epistemic_rs_85_abstained(
            verdict(GroundingDecision::Abstain).to_gate_action()
        ));
        for d in [GroundingDecision::Answer, GroundingDecision::Hedge] {
            assert!(
                !epistemic_rs_85_abstained(verdict(d).to_gate_action()),
                "{d:?} released text; epistemic.rs:85 must collect its holdings"
            );
        }
    }

    #[test]
    fn the_shim_emits_exactly_the_two_strings_it_documents() {
        assert_eq!(
            verdict(GroundingDecision::Answer).to_gate_action(),
            "released"
        );
        assert_eq!(
            verdict(GroundingDecision::Hedge).to_gate_action(),
            "released"
        );
        assert_eq!(
            verdict(GroundingDecision::Abstain).to_gate_action(),
            "abstained"
        );
    }

    #[test]
    fn a_native_verdict_is_never_fail_open() {
        // There is no judge on the native path, so nothing can fail open.
        // `epistemic.rs:71` must never classify a shim output as FailOpen —
        // that would stamp `Verification::FailOpen` on real holdings.
        for d in [
            GroundingDecision::Answer,
            GroundingDecision::Hedge,
            GroundingDecision::Abstain,
        ] {
            assert!(!epistemic_rs_71_fail_open(verdict(d).to_gate_action()));
        }
    }

    /// `grounding/mod.rs:1263-1265` scores the action into a (released,
    /// abstained) counter pair. `"released"` hits the `(1, 0)` arm exactly;
    /// `"abstained"` hits the `a if a.starts_with("abstained")` arm. Any
    /// other string would fall through to the catch-all and be counted as
    /// neither.
    #[test]
    fn the_shim_hits_a_real_arm_of_the_gate_counter() {
        let count = |a: &str| -> (u32, u32) {
            match a {
                "released" => (1, 0),
                a if a.starts_with("abstained") => (0, 1),
                _ => (0, 0),
            }
        };
        assert_eq!(
            count(verdict(GroundingDecision::Answer).to_gate_action()),
            (1, 0)
        );
        assert_eq!(
            count(verdict(GroundingDecision::Hedge).to_gate_action()),
            (1, 0)
        );
        assert_eq!(
            count(verdict(GroundingDecision::Abstain).to_gate_action()),
            (0, 1)
        );
    }

    #[test]
    fn the_verdict_round_trips_on_the_wire() {
        let v = GroundingVerdict {
            decision: GroundingDecision::Hedge,
            answerability: 0.62,
            semantic_entropy: Some(0.95),
            agreement: Some(0.6),
            decided_by: DeciderId::AgreementGate,
            segments: vec![
                AnswerSegment {
                    text_range: 0..12,
                    kind: SegmentKind::Grounded {
                        chunk_id: "sec_0002".into(),
                        span: 40..52,
                    },
                    margin: Some(1.4),
                },
                AnswerSegment {
                    text_range: 12..30,
                    kind: SegmentKind::Parametric,
                    margin: None,
                },
                AnswerSegment {
                    text_range: 30..44,
                    kind: SegmentKind::Unverified,
                    margin: Some(-0.8),
                },
            ],
        };
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<GroundingVerdict>(&json).unwrap(), v);
        // The wire form is snake_case, so a JS/Python reader of the
        // transcript sidecar sees the same vocabulary the docs use.
        assert!(json.contains(r#""decision":"hedge""#), "{json}");
        assert!(json.contains(r#""decided_by":"agreement_gate""#), "{json}");
        assert!(json.contains(r#""kind":"grounded""#), "{json}");
        assert!(json.contains(r#""kind":"unverified""#), "{json}");
    }

    #[test]
    fn segments_default_to_empty_for_a_pre_generation_verdict() {
        // An `Abstain` decided by the router is emitted BEFORE any token is
        // generated (§5 H1), so it has no segments and must still decode.
        let json = r#"{"decision":"abstain","answerability":0.11,
            "semantic_entropy":null,"agreement":null,"decided_by":"router"}"#;
        let v: GroundingVerdict = serde_json::from_str(json).unwrap();
        assert!(v.segments.is_empty());
        assert_eq!(v.to_gate_action(), "abstained");
    }
}
