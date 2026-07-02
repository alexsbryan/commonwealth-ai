// SPDX-License-Identifier: AGPL-3.0-or-later
//! OICP-driven selection primitives shared across the mesh crate.
//!
//! Both sides of a mesh chat completion need the same scoring +
//! tie-break policy:
//!
//!   - **Joiner side** (`peer_inference`): score our own manifest
//!     and every reachable peer's manifest; pick the best model.
//!     Only cross the wire when a peer is strictly better than
//!     local under the (score, size_gb) policy.
//!
//!   - **Peer side** (`inference_adapter`): a chat completion
//!     request has arrived carrying an OICP envelope. Our own
//!     daemon has multiple slots loaded (Fast = 9B, Slow = 27B,
//!     say). We must pick the slot whose capabilities best match
//!     the request — otherwise the OICP work the Joiner did is
//!     wasted the moment we default to `Speed::Slow`.
//!
//! Keeping the primitives in one place means the two sides can't
//! drift out of agreement about what "best" means.
use sovereign_core::oicp::{
    self as oicp, score_with_adjustments, BenchmarkResult, CapabilityHint, InferenceRequirements,
    LatencyClass, NodeLocality, NodeObservations, ScoreBreakdown,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Speed;

// The scoring primitives are the oicp-types SSOT (2026-06-10
// rationalization — this module used to carry its own copies, one of
// three divergent implementations). Re-exported under the historical
// local names so call sites and tests read unchanged.
pub(crate) use sovereign_core::oicp::{
    best_claim_for_request as score_manifest_for_request, pick_better,
    ScoredClaim as ModelCandidate, SCORING_EPSILON as SCORE_TIE_EPSILON,
};

/// Used by the Joiner-side selector to detect "peer pick is
/// identical to local pick" so a zero-delta routing decision
/// doesn't trip a network hop (e.g. both sides advertise the
/// same Qwen3.5-9B).
pub(crate) fn candidates_equal(a: &ModelCandidate, b: &ModelCandidate) -> bool {
    (a.score - b.score).abs() <= SCORE_TIE_EPSILON
        && a.size_gb == b.size_gb
        && a.model_id == b.model_id
}

/// RTT threshold below which a peer is classified as
/// `NodeLocality::Local` — sub-5ms is same-host (loopback, Unix
/// socket equivalents). Rare in the mesh (peers are normally
/// separate machines) but handled for completeness.
pub(crate) const LOCAL_RTT_MS_THRESHOLD: u32 = 5;

/// RTT threshold below which a peer is classified as
/// `NodeLocality::Near` — typical for same-LAN (ethernet/WiFi with
/// a shared subnet) and direct Tailscale/WireGuard WAN links
/// between nearby endpoints. 25ms comfortably covers reasonable
/// LAN deployments without grabbing every lucky cross-internet
/// peer.
pub(crate) const NEAR_RTT_MS_THRESHOLD: u32 = 25;

/// Classify a measured round-trip time into a
/// [`NodeLocality`] bucket. Pure function; the async HTTP probe
/// that produces the `rtt_ms` value lives in
/// [`crate::peer_inference::MeshInferenceProvider::get_peer_manifest`].
pub(crate) fn classify_rtt_ms(rtt_ms: u32) -> NodeLocality {
    if rtt_ms < LOCAL_RTT_MS_THRESHOLD {
        NodeLocality::Local
    } else if rtt_ms < NEAR_RTT_MS_THRESHOLD {
        NodeLocality::Near
    } else {
        NodeLocality::Far
    }
}

/// Fold v0.3 §7 operational adjustments (observation, load,
/// locality, cold-start, throughput, availability) into a
/// claim-scored candidate via the oicp-types SSOT scorer. The
/// returned candidate has `score` rescaled; all other fields are
/// preserved so downstream tie-breaks (`size_gb`, `model_id`) still
/// work. The full [`ScoreBreakdown`] rides along for the caller's
/// glassbox event — emit it, don't drop it.
///
/// `baseline_benchmark` is the peer's gossiped [`BenchmarkResult`]
/// (or `None` for older peers). `availability` is the peer's
/// gossiped `inference_availability` — pass `None` when scoring the
/// local node (its business is already captured by
/// `obs.in_flight`), `Some(...)` for peers. Adopting the gossiped
/// signal on the Joiner side is the one disclosed behavior change
/// of the 2026-06-10 rationalization: a peer advertising 0.2
/// availability used to be scored as if idle.
pub(crate) fn adjust_for_observations(
    cand: ModelCandidate,
    obs: &NodeObservations,
    locality: NodeLocality,
    baseline_benchmark: Option<&BenchmarkResult>,
    availability: Option<f32>,
) -> (ModelCandidate, ScoreBreakdown) {
    let breakdown = score_with_adjustments(
        cand.score,
        cand.claim_affinity,
        obs,
        locality,
        cand.size_gb.unwrap_or(0.0),
        baseline_benchmark,
        availability,
    );
    (
        ModelCandidate {
            score: breakdown.final_score,
            ..cand
        },
        breakdown,
    )
}

/// Decide which `Speed` slot on the local provider should serve a
/// chat request, given its OICP capability envelope.
///
/// Why this exists: the Founder is running two loaded slots — a
/// 9B in Fast and a 27B in Slow. When the Joiner's selector
/// routes a DeepQuery to us (having already picked our 9B via
/// OICP scoring), our adapter must load the 9B slot, not default
/// to `Speed::Slow` and fire up the 27B. Otherwise every federated
/// DeepQuery pays the 27B-scale latency regardless of whether the
/// 9B would have served identically.
///
/// The logic mirrors `peer_inference::select_peer` in miniature:
/// build candidates for each loaded chat slot (Fast, Slow), score
/// them with `score_manifest`-style reasoning, pick the winner
/// under `pick_better`. When no OICP envelope is present (or the
/// capabilities section is empty), fall back to `Speed::Slow` —
/// that's the legacy non-mesh behaviour for direct local requests
/// and preserves every pre-OICP call path.
///
/// Does NOT consult `ShardingPrivacy::LocalOnly`: by the time a
/// request reaches the adapter, the peer trust boundary has
/// already been crossed (or the caller is local). Privacy is a
/// *routing* constraint, not a serving constraint.
pub(crate) fn pick_slot_for_oicp(
    provider: &dyn InferenceProvider,
    request: &sovereign_core::types::CompletionRequest,
) -> Speed {
    // No OICP envelope → Slow default (pre-mesh path). External
    // OpenAI-compatible clients that don't speak OICP get the
    // conservative fall-through. Internal callers carry intent via
    // OICP (`SplitInferenceProvider::build_request` auto-derives a
    // `latency_class` from the runtime's `Speed`), so this branch is
    // not on the internal hot path.
    let Some(oicp) = &request.oicp else {
        return Speed::Slow;
    };
    pick_slot_v03(provider, oicp)
}

/// v0.3 slot selection: latency class picks the primary slot; hint
/// acts as a veto when the primary slot's model cannot serve the
/// requested specialization. Falls back to the latency-class default
/// when slots have no manifest entries (BYOM case) — trusting the
/// operator's slot configuration rather than punishing them with a
/// blanket Slow.
fn pick_slot_v03(provider: &dyn InferenceProvider, req: &InferenceRequirements) -> Speed {
    let hint = req.effective_hint();
    let class = req.effective_latency_class();
    let manifest = &sovereign_core::models_manifest::DEFAULT_MANIFEST;

    let slot_matches_hint = |speed: Speed| -> Option<(String, CapabilityHint)> {
        let id = provider.model_id_for(speed);
        if id.is_empty() || id == "unknown" {
            return None;
        }
        let info = manifest.info_for_file(&id)?;
        let slot_hint = oicp::infer_hint_from_profile(&info.capabilities);
        if oicp::hint_match_score(&slot_hint, &hint) > 0.0 {
            Some((id, slot_hint))
        } else {
            None
        }
    };

    let fast = slot_matches_hint(Speed::Fast);
    let slow = slot_matches_hint(Speed::Slow);

    let primary = match class {
        LatencyClass::Fast => Speed::Fast,
        LatencyClass::Normal | LatencyClass::Extended => Speed::Slow,
    };
    let primary_available = matches!(
        (primary, &fast, &slow),
        (Speed::Fast, Some(_), _) | (Speed::Slow, _, Some(_))
    );

    if primary_available {
        tracing::debug!(
            hint = %hint,
            latency_class = ?class,
            picked = ?primary,
            "pick_slot_for_oicp (v0.3): latency class guided slot pick"
        );
        return primary;
    }

    // Primary slot doesn't satisfy the hint. Try the other slot.
    let fallback = match primary {
        Speed::Fast => Speed::Slow,
        _ => Speed::Fast,
    };
    let fallback_available = matches!(
        (fallback, &fast, &slow),
        (Speed::Fast, Some(_), _) | (Speed::Slow, _, Some(_))
    );
    if fallback_available {
        tracing::info!(
            hint = %hint,
            latency_class = ?class,
            picked = ?fallback,
            "pick_slot_for_oicp (v0.3): primary slot did not match hint — fell back"
        );
        return fallback;
    }

    // Neither slot has a manifest entry — BYOM case. Trust the
    // operator's slot configuration: route on latency_class alone,
    // verifying only that the chosen slot is actually loaded.
    let fast_loaded = slot_loaded(provider, Speed::Fast);
    let slow_loaded = slot_loaded(provider, Speed::Slow);
    let resolved = match (primary, fast_loaded, slow_loaded) {
        (Speed::Fast, true, _) => Some(Speed::Fast),
        (Speed::Fast, false, true) => Some(Speed::Slow),
        (Speed::Slow, _, true) => Some(Speed::Slow),
        (Speed::Slow, true, false) => Some(Speed::Fast),
        _ => None,
    };
    if let Some(s) = resolved {
        tracing::info!(
            hint = %hint,
            latency_class = ?class,
            picked = ?s,
            "pick_slot_for_oicp (v0.3): BYOM slots — routing on latency_class"
        );
        return s;
    }

    tracing::warn!(
        hint = %hint,
        latency_class = ?class,
        "pick_slot_for_oicp (v0.3): no slot loaded — serving from Slow"
    );
    Speed::Slow
}

/// True when the provider has a model loaded into `speed`'s slot.
/// Mirrors `slot_matches_hint`'s id check without the manifest lookup
/// — distinguishes empty/unknown (no slot) from a loaded BYOM model.
fn slot_loaded(provider: &dyn InferenceProvider, speed: Speed) -> bool {
    let id = provider.model_id_for(speed);
    !id.is_empty() && id != "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::oicp::ProviderManifest;

    fn cand(score: f32, size_gb: Option<f32>, id: &str) -> ModelCandidate {
        ModelCandidate {
            score,
            size_gb,
            model_id: id.into(),
            claim_affinity: score,
        }
    }

    /// GOLDEN VECTOR — pins the operational-adjustment product
    /// bit-for-bit across the SSOT move to oicp-types (Phase B of
    /// the 2026-06-10 rationalization). Every factor ≠ 1.0:
    ///   observation_mult = eff(0.95, obs)/0.95
    ///                    = (0.95·(1 − (10/50)·0.1))/0.95 = 0.98
    ///   load   = 1/(1 + 0.05·10)   = 2/3
    ///   loc    = Near              = 1.05
    ///   cold   = 0.7 + 0.3·(10/20) = 0.85
    ///   thru   = 10/20 (observed)  = 0.5
    ///   final  = 0.5 · 0.98 · (2/3) · 1.05 · 0.85 · 0.5
    /// If this fails after a refactor, the refactor changed routing
    /// behavior — that is a disclosure, not a test update.
    #[test]
    fn golden_adjustment_product_all_factors_active() {
        let obs = NodeObservations {
            in_flight: 10,
            samples: 10,
            recent_failure_rate: 0.1,
            tg_tok_s_ewma: 10.0,
            ..Default::default()
        };
        let raw = ModelCandidate {
            score: 0.5,
            size_gb: Some(8.0),
            model_id: "golden".into(),
            claim_affinity: 0.95,
        };
        let (adjusted, breakdown) =
            adjust_for_observations(raw, &obs, NodeLocality::Near, None, None);
        let expected = 0.5_f32 * 0.98 * (2.0 / 3.0) * 1.05 * 0.85 * 0.5;
        assert!(
            (adjusted.score - expected).abs() < 1e-6,
            "golden product drifted: got {}, want {expected}",
            adjusted.score
        );
        // Equivalence: the wrapper's candidate score IS the SSOT
        // breakdown's final score, forever.
        assert_eq!(adjusted.score.to_bits(), breakdown.final_score.to_bits());
        assert!(
            (breakdown.availability - 1.0).abs() < 1e-6,
            "None ⇒ neutral"
        );
        // Tie-break inputs must survive adjustment untouched.
        assert_eq!(adjusted.model_id, "golden");
        assert_eq!(adjusted.size_gb, Some(8.0));
    }

    /// The decided behavior change (2026-06-10): the Joiner honors
    /// gossiped `inference_availability`. Two otherwise-identical
    /// peers — the one advertising 0.2 loses to the idle one 5:1.
    #[test]
    fn gossiped_availability_demotes_busy_peer() {
        let obs = NodeObservations {
            samples: 100, // fully ramped — isolate the availability term
            ..Default::default()
        };
        let raw = |id: &str| ModelCandidate {
            score: 0.8,
            size_gb: Some(8.0),
            model_id: id.into(),
            claim_affinity: 0.9,
        };
        let (busy, _) =
            adjust_for_observations(raw("busy"), &obs, NodeLocality::Far, None, Some(0.2));
        let (idle, _) =
            adjust_for_observations(raw("idle"), &obs, NodeLocality::Far, None, Some(1.0));
        assert!(idle.score > busy.score * 4.9);
        assert_eq!(pick_better(busy, idle).model_id, "idle");
    }

    #[test]
    fn pick_better_higher_score_wins() {
        let a = cand(0.5, Some(5.5), "small");
        let b = cand(1.0, Some(16.5), "big");
        assert_eq!(pick_better(a, b).model_id, "big");
    }

    // ── v0.3 §7 — RTT-based locality classification ───────────

    #[test]
    fn classify_rtt_sub_5ms_is_local() {
        assert_eq!(classify_rtt_ms(0), NodeLocality::Local);
        assert_eq!(classify_rtt_ms(1), NodeLocality::Local);
        assert_eq!(classify_rtt_ms(4), NodeLocality::Local);
    }

    #[test]
    fn classify_rtt_lan_range_is_near() {
        // 5ms is the Local threshold (exclusive), so it tips into
        // Near — the "local" bucket is reserved for same-host loop.
        assert_eq!(classify_rtt_ms(5), NodeLocality::Near);
        assert_eq!(classify_rtt_ms(12), NodeLocality::Near);
        assert_eq!(classify_rtt_ms(24), NodeLocality::Near);
    }

    #[test]
    fn classify_rtt_wan_range_is_far() {
        assert_eq!(classify_rtt_ms(25), NodeLocality::Far);
        assert_eq!(classify_rtt_ms(50), NodeLocality::Far);
        assert_eq!(classify_rtt_ms(250), NodeLocality::Far);
        assert_eq!(classify_rtt_ms(u32::MAX), NodeLocality::Far);
    }

    #[test]
    fn classify_rtt_thresholds_are_exclusive_upper() {
        // Exact-boundary behaviour: the `< LOCAL`, `< NEAR` rule
        // means LOCAL_RTT_MS_THRESHOLD itself falls into Near, and
        // NEAR_RTT_MS_THRESHOLD itself falls into Far. Document
        // this so future tweaks to the constants can't silently
        // shift which bucket the boundary lands in.
        assert_eq!(classify_rtt_ms(LOCAL_RTT_MS_THRESHOLD), NodeLocality::Near);
        assert_eq!(classify_rtt_ms(NEAR_RTT_MS_THRESHOLD), NodeLocality::Far);
    }

    #[test]
    fn pick_better_score_tied_smaller_size_wins() {
        let nine = cand(1.0, Some(5.5), "qwen-9b");
        let twenty_seven = cand(1.0, Some(16.5), "qwen-27b");
        assert_eq!(
            pick_better(twenty_seven.clone(), nine.clone()).model_id,
            "qwen-9b"
        );
        assert_eq!(pick_better(nine, twenty_seven).model_id, "qwen-9b");
    }

    #[test]
    fn pick_better_known_size_beats_unknown_on_tie() {
        let annotated = cand(1.0, Some(5.5), "annotated");
        let unannotated = cand(1.0, None, "byom");
        assert_eq!(
            pick_better(unannotated.clone(), annotated.clone()).model_id,
            "annotated"
        );
        assert_eq!(pick_better(annotated, unannotated).model_id, "annotated");
    }

    #[test]
    fn pick_better_full_tie_keeps_incumbent() {
        let a = cand(1.0, Some(5.5), "incumbent");
        let b = cand(1.0, Some(5.5), "challenger");
        assert_eq!(pick_better(a, b).model_id, "incumbent");
    }

    #[test]
    fn pick_better_epsilon_ignores_floating_point_noise() {
        let nine = cand(1.0, Some(5.5), "qwen-9b");
        let twenty_seven = cand(1.0 - 1e-6, Some(16.5), "qwen-27b");
        assert_eq!(pick_better(twenty_seven, nine).model_id, "qwen-9b");
    }

    // ── pick_slot_for_oicp tests (v0.3 only) ──────────────────
    //
    // These guard the peer-side adapter's slot selection: a Founder
    // running Qwen3.5-9B in Fast and Qwen3.5-27B in Slow must pick
    // the right slot for each latency class per the request's v0.3
    // envelope.

    use async_trait::async_trait;
    use sovereign_core::error::Result;
    use sovereign_core::oicp::InferenceRequirements;
    use sovereign_core::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};

    /// Stub provider: only `model_id_for` is exercised by
    /// `pick_slot_for_oicp`. The rest is `unimplemented!()` so
    /// drift in the helper (e.g. accidentally calling `complete`)
    /// blows the test up loudly rather than serving silent garbage.
    struct StubProvider {
        fast_model: String,
        slow_model: String,
    }

    #[async_trait]
    impl InferenceProvider for StubProvider {
        async fn complete(&self, _: &CompletionRequest) -> Result<CompletionResponse> {
            unimplemented!("pick_slot_for_oicp must not call complete()")
        }
        async fn complete_stream(
            &self,
            _: &CompletionRequest,
        ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
            unimplemented!("pick_slot_for_oicp must not call complete_stream()")
        }
        async fn embed(&self, _: &str) -> Result<Vec<f32>> {
            unimplemented!()
        }
        async fn embed_batch(&self, _: &[String]) -> Result<Vec<Vec<f32>>> {
            unimplemented!()
        }
        async fn embed_query(&self, _: &str) -> Result<Vec<f32>> {
            unimplemented!()
        }
        fn model_id_for(&self, speed: Speed) -> String {
            match speed {
                Speed::Fast => self.fast_model.clone(),
                Speed::Slow | Speed::Medium => self.slow_model.clone(),
            }
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 32_768,
                supports_structured_output: false,
                relative_speed: Speed::Slow,
                relative_reasoning: sovereign_core::types::Depth::Moderate,
            }
        }
    }

    #[test]
    fn pick_slot_placeholder_removed_in_pr_c() {
        // The original v0.2 "both satisfy required, smaller wins"
        // test used CapabilityRequirements / CapabilityProfile; in
        // v0.3 the same "9B over 27B for a normal request" outcome
        // falls out of pick_slot_v03 mapping latency_class:Normal to
        // Speed::Slow, which the v0.3 tests below cover.
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0.1".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let _ = provider.model_id_for(Speed::Fast);
    }

    #[test]
    fn pick_slot_defaults_to_slow_when_no_oicp_envelope() {
        // Non-mesh callers: no OICP envelope → Slow default.
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let req = CompletionRequest::new("x");
        assert_eq!(pick_slot_for_oicp(&provider, &req), Speed::Slow);
    }

    // -----------------------------------------------------------
    // v0.3 — latency_class guided slot selection
    // -----------------------------------------------------------

    #[test]
    fn pick_slot_v03_latency_fast_picks_fast_slot() {
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0.1".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let envelope = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Fast);
        let req = CompletionRequest::new("quick").with_oicp(envelope);
        assert_eq!(pick_slot_for_oicp(&provider, &req), Speed::Fast);
    }

    #[test]
    fn pick_slot_v03_latency_normal_picks_slow_slot() {
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0.1".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let envelope = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal);
        let req = CompletionRequest::new("substantive").with_oicp(envelope);
        assert_eq!(pick_slot_for_oicp(&provider, &req), Speed::Slow);
    }

    #[test]
    fn pick_slot_v03_latency_extended_picks_slow_slot() {
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0.1".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let envelope = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Extended);
        let req = CompletionRequest::new("deep").with_oicp(envelope);
        assert_eq!(pick_slot_for_oicp(&provider, &req), Speed::Slow);
    }

    #[test]
    fn pick_slot_v03_hint_only_defaults_to_slow_for_normal_latency() {
        // When only a hint is present, effective_latency_class()
        // returns Normal → Slow slot.
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0.1".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let envelope = InferenceRequirements::new().with_hint(CapabilityHint::general());
        let req = CompletionRequest::new("hint-only").with_oicp(envelope);
        assert_eq!(pick_slot_for_oicp(&provider, &req), Speed::Slow);
    }

    // -----------------------------------------------------------
    // v0.3 — score_manifest_for_request claim path
    // -----------------------------------------------------------

    fn manifest_with_claim(
        id: &str,
        size_gb: Option<f32>,
        claim: sovereign_core::oicp::CapabilityClaim,
    ) -> ProviderManifest {
        ProviderManifest::new(vec![sovereign_core::oicp::ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            context_tokens: claim.max_context,
            status: sovereign_core::oicp::ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb,
            claims: vec![claim],
        }])
    }

    #[test]
    fn score_manifest_for_request_prefers_claim_path_when_claims_present() {
        use sovereign_core::oicp::CapabilityClaim;
        let qwen_coder = manifest_with_claim(
            "qwen-coder-32b",
            Some(16.1),
            CapabilityClaim::new(
                CapabilityHint::code(),
                LatencyClass::Normal,
                32_000,
                4_000,
                0.95,
            ),
        );
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::code())
            .with_latency_class(LatencyClass::Normal)
            .with_context_tokens(16_000)
            .with_max_output_tokens(2_000);
        let cand =
            score_manifest_for_request(&qwen_coder, &req).expect("v0.3 claim scores non-None");
        assert_eq!(cand.model_id, "qwen-coder-32b");
        // Exact hint + latency match → score equals affinity.
        assert!((cand.score - 0.95).abs() < 1e-4);
    }

    #[test]
    fn score_manifest_for_request_returns_none_when_no_claims_match() {
        // Claim with zero-output gate against a request needing any
        // output → hard gate fails, no candidate.
        let m = manifest_with_claim(
            "undersized",
            Some(1.0),
            sovereign_core::oicp::CapabilityClaim::new(
                CapabilityHint::general(),
                LatencyClass::Normal,
                100,
                50,
                0.5,
            ),
        );
        let req = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal)
            .with_context_tokens(8_000)
            .with_max_output_tokens(1_000);
        assert!(score_manifest_for_request(&m, &req).is_none());
    }
}
