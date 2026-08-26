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

    /// Build the request this workload class declares (SLOT_POLICY §9.4).
    /// The call site says WHAT the call is; the scheduler resolves WHERE it
    /// runs. Attaches the OICP requirement bundle, the derived
    /// `preferred_speed` shadow (§8 — via `latency_to_speed`, never a
    /// literal), the class think budget, and emits the glassbox `workload=`
    /// tracing event.
    ///
    /// Privacy: LocalOnly. Internal machinery uses this; it is provably
    /// routing-neutral at the mesh privacy gate. Session-posture-aware callers
    /// (grounding judges, EnrichBulk fan-out) use [`Self::request_shared`].
    ///
    /// Was `CompletionRequest::for_workload` until 2026-08-20. It could not
    /// stay an inherent constructor once `CompletionRequest` moved down to
    /// `oicp-types`: the protocol crate would have had to name sovereign's
    /// workload table to keep it. Living on `Workload` puts the decision on
    /// the noun that owns it (noun-convergence rung 2b).
    pub fn request(self, prompt: impl Into<String>) -> CompletionRequest {
        self.request_shared(prompt, ShardingPrivacy::LocalOnly)
    }

    /// [`Self::request`] with an explicit privacy posture — the only path by
    /// which internal work becomes mesh-offloadable. Threading the
    /// session/operator posture (never hardcoding it) is SLOT_POLICY §2.4.
    pub fn request_shared(
        self,
        prompt: impl Into<String>,
        posture: ShardingPrivacy,
    ) -> CompletionRequest {
        let bundle = self.bundle();
        let oicp = self.requirements(posture);
        tracing::debug!(
            target: "slot_policy",
            workload = self.as_str(),
            latency_class = ?bundle.latency,
            privacy = ?posture,
            request_id = oicp.request_id.as_deref().unwrap_or(""),
            "workload request constructed"
        );
        let mut req = CompletionRequest::new(&prompt.into());
        // The ONE canonical shadow derivation (SLOT_POLICY §8).
        req.preferred_speed = latency_to_speed(bundle.latency);
        req.think_budget = bundle.think_budget;
        req.oicp = Some(oicp);
        req
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

// §8's Speed <-> LatencyClass derivation. The DEFINITIONS moved to
// `oicp-types` with `Speed` itself — both types are protocol vocabulary, and
// leaving the map up here would have kept `oicp-client` depending on sovereign
// for it. This module remains the canonical PATH every call site spells, so
// the "one home" claim in the header still holds (noun-convergence rung 2b).
pub use crate::oicp::{latency_to_speed, speed_to_latency};

/// Forced yes/no check on the fast slot: 5-token budget, temperature 0, no
/// thinking. Read the verdict with `CompletionResponse::as_bool`. Used by
/// `Branch` steps and other binary gates.
///
/// SLOT_POLICY §3 Route: a branch-condition check. The envelope makes every
/// call site scheduler-visible and the honest 5-token budget rides along as
/// the FastShort hard gate; speed stays Fast (the shadow Route would derive
/// anyway). Was `CompletionRequest::yes_no` — it reads the workload table, so
/// it is policy and stayed behind when the request type moved down.
pub fn yes_no(condition: &str, context: &str) -> CompletionRequest {
    CompletionRequest {
        prompt: format!(
            "Given the following context:\n{context}\n\n\
             Answer this yes/no question with only \"yes\" or \"no\":\n{condition}"
        ),
        preferred_speed: Speed::Fast,
        max_tokens: Some(5),
        temperature: Some(0.0),
        think_budget: Some(0), // No thinking needed for yes/no
        oicp: Some(
            Workload::Route
                .requirements(ShardingPrivacy::LocalOnly)
                .with_max_output_tokens(5),
        ),
        ..Default::default()
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
            let req = w.request("probe");
            assert_eq!(
                speed_to_latency(req.preferred_speed),
                w.bundle().latency,
                "{}",
                w.as_str()
            );
        }
    }

    // ── The workload constructors, moved here with the methods ─────────
    //
    // These tests came from `types/completion.rs` when `for_workload*` and
    // `yes_no` stayed behind on the policy side of the 2b move. They assert
    // policy, not vocabulary: which envelope a class attaches, and that the
    // §8 shadow is derived rather than typed.

    #[test]
    fn every_workload_request_sets_oicp_version() {
        // Pins the structural-422 invariant: an envelope missing
        // `oicp_version` is rejected at the daemon's Json extractor.
        for w in Workload::ALL {
            let req = w.request("p");
            let oicp = req.oicp.expect("workload envelope");
            assert_eq!(oicp.oicp_version, OICP_VERSION, "{}", w.as_str());
        }
    }

    #[test]
    fn request_defaults_to_local_only() {
        let req = Workload::Route.request("p");
        assert_eq!(req.oicp.unwrap().sharding(), ShardingPrivacy::LocalOnly);
    }

    #[test]
    fn request_shared_threads_posture() {
        let req = Workload::Judge.request_shared("p", ShardingPrivacy::MeshAllowed);
        assert_eq!(req.oicp.unwrap().sharding(), ShardingPrivacy::MeshAllowed);
    }

    #[test]
    fn request_tags_request_id() {
        let req = Workload::Housekeep.request("p");
        let id = req.oicp.unwrap().request_id.expect("tag");
        assert!(id.starts_with("wl-housekeep-"), "{id}");
    }

    #[test]
    fn with_output_budget_sets_both_max_tokens_and_envelope() {
        let req = Workload::Route.request("p").with_output_budget(5);
        assert_eq!(req.max_tokens, Some(5));
        assert_eq!(req.oicp.unwrap().max_output_tokens, Some(5));
    }

    #[test]
    fn yes_no_carries_route_envelope_and_stays_fast() {
        let yn = yes_no("is it?", "ctx");
        assert_eq!(yn.preferred_speed, Speed::Fast);
        let oicp = yn.oicp.expect("route envelope");
        assert_eq!(oicp.effective_latency_class(), LatencyClass::Fast);
        assert_eq!(oicp.max_output_tokens, Some(5));
        assert_eq!(oicp.sharding(), ShardingPrivacy::LocalOnly);
    }
}
