// SPDX-License-Identifier: AGPL-3.0-or-later
//! Slot policy — the executable core of `sovereign/docs/SLOT_POLICY.md`.
//!
//! Call sites declare WHAT a call needs (a [`Workload`] class → an OICP
//! requirement bundle); the scheduler resolves WHERE it runs. No call
//! site picks a slot, writes `preferred_speed` directly, or pins a
//! model to smuggle a quality requirement (SLOT_POLICY §2).
//!
//! This module is also the ONE canonical home of the
//! `Speed ↔ LatencyClass` derivation (§8). The five historical inline
//! maps (executor, oicp-client, inference_adapter, oicp_select,
//! sovereign-workflow) are absorbed here; any new mapping site is a
//! policy violation.

use crate::oicp::{CapabilityHint, InferenceRequirements, LatencyClass, ShardingPrivacy};
use crate::types::{CompletionRequest, Speed};

/// SLOT_POLICY §3 workload classes. Fieldless — the class, not the
/// call site, owns the requirement bundle; dynamic knobs (honest
/// budgets, privacy posture) are supplied at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    /// Schema-constrained classification consumed by control flow,
    /// never shown to the user (router passes, intent/tool select,
    /// doc-type detect, difficulty estimate, branch conditions).
    Route,
    /// Turn-loop hygiene whose output is advisory context, not durable
    /// truth (titles, preambles, topic extraction, working-memory
    /// compression, note digests, gap checks, re-query generation).
    Housekeep,
    /// Extraction written to a durable store or protecting one
    /// (memory facts, contradiction checks, skeletons, typed
    /// extension, RAPTOR summaries). Corruption outlives the session.
    ExtractDurable,
    /// High-volume corpus enrichment where fast-class throughput is
    /// existential and quality is bench-validated per recipe. Grammar
    /// constraint mandatory (§6.2).
    EnrichBulk,
    /// Anything that scores, ranks, verifies, or gates another output
    /// — including forced-choice logprob elicitation (grounding
    /// critics, sufficiency judges, eval judges, best-of-N selection).
    Judge,
    /// Prose composed for the user; final reduces of document ops.
    Synthesize,
    /// Naked chat and agentic reasoning loops — the model the user
    /// chose, doing what they ran it for.
    Passthrough,
}

/// One row of the SLOT_POLICY §3 table. `max_output_cap` and
/// `think_budget` are guidance — sites own honest budgets (§2.3); the
/// [`CompletionRequest::for_workload`] constructor applies
/// `think_budget` only. `constraint` documents the class's structured-
/// output expectation for review and the table test; it is not (yet)
/// mechanically enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequirementBundle {
    /// OICP latency class the workload declares — what `latency_to_speed` shadows into `preferred_speed`.
    pub latency: LatencyClass,
    /// §3 guidance output cap; call sites still own their honest budgets (§2.3).
    pub max_output_cap: Option<u32>,
    /// Think-block budget `for_workload` applies; `None` = the class doesn't constrain it.
    pub think_budget: Option<usize>,
    /// The class's structured-output expectation — documented for review, not mechanically enforced.
    pub constraint: ConstraintExpectation,
}

/// The §3 structured-output expectation per class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintExpectation {
    /// No structured output expected.
    None,
    /// Schema-constrained where the call produces structured output; free text otherwise.
    SchemaWhereStructured,
    /// Every call in this class must carry a schema.
    SchemaRequired,
    /// Calls must be grammar-constrained (lark).
    GrammarRequired,
}

impl Workload {
    /// Every workload class — for table tests and exhaustive iteration.
    pub const ALL: [Workload; 7] = [
        Workload::Route,
        Workload::Housekeep,
        Workload::ExtractDurable,
        Workload::EnrichBulk,
        Workload::Judge,
        Workload::Synthesize,
        Workload::Passthrough,
    ];

    /// Canonical kebab-case name, used in tracing events and request-id tags.
    pub const fn as_str(self) -> &'static str {
        match self {
            Workload::Route => "route",
            Workload::Housekeep => "housekeep",
            Workload::ExtractDurable => "extract-durable",
            Workload::EnrichBulk => "enrich-bulk",
            Workload::Judge => "judge",
            Workload::Synthesize => "synthesize",
            Workload::Passthrough => "passthrough",
        }
    }

    /// The SLOT_POLICY §3 table, verbatim. Capability hint is
    /// `general` for every class (Synthesize/Passthrough refine hints
    /// via the runtime's intent table, `build_oicp` — not here).
    /// `Extended` never appears: rule 4.4 — only intent/skill
    /// declarations produce it.
    pub const fn bundle(self) -> RequirementBundle {
        match self {
            Workload::Route => RequirementBundle {
                latency: LatencyClass::Fast,
                max_output_cap: Some(256),
                think_budget: Some(0),
                constraint: ConstraintExpectation::SchemaRequired,
            },
            Workload::Housekeep => RequirementBundle {
                latency: LatencyClass::Fast,
                max_output_cap: Some(512),
                think_budget: Some(0),
                constraint: ConstraintExpectation::SchemaWhereStructured,
            },
            Workload::ExtractDurable => RequirementBundle {
                latency: LatencyClass::Normal,
                max_output_cap: None,
                think_budget: None,
                constraint: ConstraintExpectation::SchemaRequired,
            },
            Workload::EnrichBulk => RequirementBundle {
                latency: LatencyClass::Fast,
                max_output_cap: Some(512),
                think_budget: None,
                constraint: ConstraintExpectation::GrammarRequired,
            },
            Workload::Judge => RequirementBundle {
                latency: LatencyClass::Normal,
                max_output_cap: Some(512),
                think_budget: None,
                constraint: ConstraintExpectation::SchemaRequired,
            },
            Workload::Synthesize => RequirementBundle {
                latency: LatencyClass::Normal,
                max_output_cap: None,
                think_budget: None,
                constraint: ConstraintExpectation::None,
            },
            Workload::Passthrough => RequirementBundle {
                latency: LatencyClass::Normal,
                max_output_cap: None,
                think_budget: None,
                constraint: ConstraintExpectation::None,
            },
        }
    }

    /// The OICP envelope for this class. Always carries `oicp_version`
    /// (via [`InferenceRequirements::new`] — absence is a structural
    /// 422 at the daemon), the class latency, a `general` hint, the
    /// caller's privacy posture, and the glassbox request tag
    /// `wl-<class>-<uuid8>` (§7) — joinable against the serving node's
    /// `slot_selected` telemetry.
    pub fn requirements(self, posture: ShardingPrivacy) -> InferenceRequirements {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(self.bundle().latency)
            .with_sharding(posture)
            .with_request_id(format!("wl-{}-{}", self.as_str(), &tag[..8]))
    }
}

/// Effective sharding posture of a request (§3.1: envelope-absent =
/// LocalOnly).
pub fn posture_of(req: &CompletionRequest) -> ShardingPrivacy {
    req.oicp
        .as_ref()
        .map(|o| o.sharding())
        .unwrap_or(ShardingPrivacy::LocalOnly)
}

/// §8 request-side derive. `Medium` is a deprecated alias of `Slow`
/// (both mean "primary work"). NEVER produces `Extended` (rule 4.4 —
/// only intent/skill declarations emit it).
pub const fn speed_to_latency(speed: Speed) -> LatencyClass {
    match speed {
        Speed::Fast => LatencyClass::Fast,
        Speed::Medium | Speed::Slow => LatencyClass::Normal,
    }
}

/// §8 resolve — serve side and shadow side. `Extended` collapses to
/// the primary slot (there is no third chat slot). NEVER produces
/// `Medium`.
pub const fn latency_to_speed(class: LatencyClass) -> Speed {
    match class {
        LatencyClass::Fast => Speed::Fast,
        LatencyClass::Normal | LatencyClass::Extended => Speed::Slow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oicp::OICP_VERSION;

    /// Executable transcription of SLOT_POLICY §3 — a policy edit must
    /// change this test and the doc in the same PR (§9.5).
    #[test]
    fn bundle_table_matches_slot_policy_section_3() {
        use ConstraintExpectation as C;
        use LatencyClass as L;
        let expect: [(Workload, L, Option<u32>, Option<usize>, C); 7] = [
            (
                Workload::Route,
                L::Fast,
                Some(256),
                Some(0),
                C::SchemaRequired,
            ),
            (
                Workload::Housekeep,
                L::Fast,
                Some(512),
                Some(0),
                C::SchemaWhereStructured,
            ),
            (
                Workload::ExtractDurable,
                L::Normal,
                None,
                None,
                C::SchemaRequired,
            ),
            (
                Workload::EnrichBulk,
                L::Fast,
                Some(512),
                None,
                C::GrammarRequired,
            ),
            (
                Workload::Judge,
                L::Normal,
                Some(512),
                None,
                C::SchemaRequired,
            ),
            (Workload::Synthesize, L::Normal, None, None, C::None),
            (Workload::Passthrough, L::Normal, None, None, C::None),
        ];
        for (w, latency, cap, think, constraint) in expect {
            let b = w.bundle();
            assert_eq!(b.latency, latency, "{}", w.as_str());
            assert_eq!(b.max_output_cap, cap, "{}", w.as_str());
            assert_eq!(b.think_budget, think, "{}", w.as_str());
            assert_eq!(b.constraint, constraint, "{}", w.as_str());
        }
        assert_eq!(Workload::ALL.len(), expect.len());
    }

    #[test]
    fn derivation_never_produces_extended_or_medium() {
        for s in [Speed::Fast, Speed::Medium, Speed::Slow] {
            assert_ne!(speed_to_latency(s), LatencyClass::Extended);
        }
        for c in [
            LatencyClass::Fast,
            LatencyClass::Normal,
            LatencyClass::Extended,
        ] {
            assert_ne!(latency_to_speed(c), Speed::Medium);
        }
    }

    #[test]
    fn derivation_round_trips_with_documented_collapses() {
        // Identity on the physical pairs.
        assert_eq!(latency_to_speed(speed_to_latency(Speed::Fast)), Speed::Fast);
        assert_eq!(latency_to_speed(speed_to_latency(Speed::Slow)), Speed::Slow);
        // Medium collapses to Slow — that IS the deprecation semantics.
        assert_eq!(
            latency_to_speed(speed_to_latency(Speed::Medium)),
            Speed::Slow
        );
        // Extended is declaration-only and collapses on round-trip.
        assert_eq!(
            speed_to_latency(latency_to_speed(LatencyClass::Extended)),
            LatencyClass::Normal
        );
        assert_eq!(
            speed_to_latency(latency_to_speed(LatencyClass::Fast)),
            LatencyClass::Fast
        );
        assert_eq!(
            speed_to_latency(latency_to_speed(LatencyClass::Normal)),
            LatencyClass::Normal
        );
    }

    #[test]
    fn requirements_always_carry_version_posture_and_tag() {
        for w in Workload::ALL {
            let local = w.requirements(ShardingPrivacy::LocalOnly);
            assert_eq!(local.oicp_version, OICP_VERSION, "{}", w.as_str());
            assert_eq!(local.sharding(), ShardingPrivacy::LocalOnly);
            assert_eq!(local.effective_latency_class(), w.bundle().latency);
            let id = local.request_id.expect("workload tag");
            assert!(id.starts_with(&format!("wl-{}-", w.as_str())), "{id}");

            let shared = w.requirements(ShardingPrivacy::MeshAllowed);
            assert_eq!(shared.sharding(), ShardingPrivacy::MeshAllowed);
        }
    }

    #[test]
    fn shadow_speed_agrees_with_bundle_latency() {
        for w in Workload::ALL {
            let req = CompletionRequest::for_workload(w, "probe");
            assert_eq!(
                speed_to_latency(req.preferred_speed),
                w.bundle().latency,
                "{}",
                w.as_str()
            );
        }
    }
}
