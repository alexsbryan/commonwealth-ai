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
    self, CapabilityHint, CapabilityProfile, InferenceRequirements,
    LatencyClass, ProviderManifest,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Speed;

/// A scored model pick from a single manifest. Carried through
/// selection so tie-breaks can see both the OICP score and the
/// model's declared size — and so logs can attribute decisions to
/// a specific model id, not just a numeric score.
#[derive(Debug, Clone)]
pub(crate) struct ModelCandidate {
    pub(crate) score: f32,
    pub(crate) size_gb: Option<f32>,
    pub(crate) model_id: String,
}

/// Score-floor below which score-ties are considered "the same".
/// Floating-point noise in the OICP scorer (division-by-max-level
/// produces 1/3, 2/3, 1.0 type values) shouldn't cause spurious
/// decisions where a 5.5 GB model beats a 16.5 GB model by a
/// rounding blip.
pub(crate) const SCORE_TIE_EPSILON: f32 = 1e-3;

/// Compare two `ModelCandidate`s under the OICP selection policy
/// and return the winner:
///
/// 1. Strictly higher `score` wins.
/// 2. Scores tied (within `SCORE_TIE_EPSILON`): smaller known
///    `size_gb` wins.
/// 3. Known size always beats unknown size on a score tie — an
///    annotated manifest entry represents curated data we trust
///    over a silent BYOM default.
/// 4. Full tie (same score bucket, both sizes unknown or equal):
///    incumbent (`cur`) wins for stability. Caller uses this to
///    encode "local wins ties" and "earlier peer wins duplicate-
///    score ties".
pub(crate) fn pick_better(cur: ModelCandidate, new: ModelCandidate) -> ModelCandidate {
    if new.score > cur.score + SCORE_TIE_EPSILON {
        return new;
    }
    if cur.score > new.score + SCORE_TIE_EPSILON {
        return cur;
    }
    match (cur.size_gb, new.size_gb) {
        (Some(c), Some(n)) if n < c => new,
        (None, Some(_)) => new,
        _ => cur,
    }
}

/// Used by the Joiner-side selector to detect "peer pick is
/// identical to local pick" so a zero-delta routing decision
/// doesn't trip a network hop (e.g. both sides advertise the
/// same Qwen3.5-9B).
pub(crate) fn candidates_equal(a: &ModelCandidate, b: &ModelCandidate) -> bool {
    (a.score - b.score).abs() <= SCORE_TIE_EPSILON
        && a.size_gb == b.size_gb
        && a.model_id == b.model_id
}

/// Score a manifest's best-fitting model against the request.
/// `Some(candidate)` when at least one model satisfies
/// `required`; `None` when no model in the manifest can serve
/// this request.
///
/// "Best" has a tiebreaker: among models with the same score,
/// prefer the one with the smallest declared `size_gb`. This is
/// the closest proxy we have to "fastest at this capability
/// level" without a live latency measurement — a 9B satisfying
/// `{Analysis:3, General:3}` is the right pick over a 27B that
/// scores identically for the same request. Unknown sizes sort
/// after any known size so an unannotated BYOM entry can't sneak
/// past an annotated one on a score tie.
pub(crate) fn score_manifest(
    manifest: &ProviderManifest,
    required: &CapabilityProfile,
    preferred: &CapabilityProfile,
) -> Option<ModelCandidate> {
    let mut best: Option<ModelCandidate> = None;
    for model in &manifest.models {
        if !oicp::satisfies_required(&model.capabilities, required) {
            continue;
        }
        let score = oicp::score_preferred(&model.capabilities, preferred);
        let cand = ModelCandidate {
            score,
            size_gb: model.size_gb,
            model_id: model.id.clone(),
        };
        best = Some(match best {
            None => cand,
            Some(cur) => pick_better(cur, cand),
        });
    }
    best
}

/// v0.3-aware wrapper around [`score_manifest`]. Prefers claim-based
/// scoring (every manifest model's `claims` vector) when any model
/// publishes claims and falls back to the legacy capability-profile
/// path otherwise.
///
/// The result uses [`ModelCandidate`] — so the caller still sees a
/// single `(model_id, size_gb, score)` pick even though the scoring
/// unit may have been a `(model, claim)` pair internally. When a
/// model publishes multiple claims, the highest-scoring claim wins
/// and its parent model becomes the candidate.
pub(crate) fn score_manifest_for_request(
    manifest: &ProviderManifest,
    req: &InferenceRequirements,
) -> Option<ModelCandidate> {
    let any_claims = manifest.models.iter().any(|m| !m.claims.is_empty());

    if any_claims {
        let mut best: Option<ModelCandidate> = None;
        for model in &manifest.models {
            // A model that hasn't published claims is skipped here;
            // the claim path is authoritative for nodes that opted
            // in to v0.3. Back-fall of a claim-less model via the
            // v0.2 path within the same manifest would produce
            // asymmetric, hard-to-reason-about scoring.
            for claim in &model.claims {
                let Some(score) = oicp::score_claim_for_request(claim, req)
                else {
                    continue;
                };
                let cand = ModelCandidate {
                    score,
                    size_gb: model.size_gb,
                    model_id: model.id.clone(),
                };
                best = Some(match best {
                    None => cand,
                    Some(cur) => pick_better(cur, cand),
                });
            }
        }
        return best;
    }

    // v0.2 fallback. Removed in PR-C.
    score_manifest(manifest, req.required(), req.preferred())
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
    // Canonical "no OICP contract" → Slow (primary). This is the
    // same conservative default that existed before the multi-
    // slot refactor, so non-mesh callers see no behavioural change.
    let Some(oicp) = &request.oicp else {
        return Speed::Slow;
    };

    // v0.3 fast path: when the request carries `capability_hint`
    // and/or `latency_class`, select the slot whose latency matches
    // the requested class, provided the slot's model can serve the
    // hint. This keeps the ground-truth dispatch key (Speed) aligned
    // with the protocol-level latency class without going through a
    // synthetic-manifest intermediate step.
    if oicp.capability_hint.is_some() || oicp.latency_class.is_some() {
        return pick_slot_v03(provider, oicp);
    }

    let Some(caps) = &oicp.capabilities else {
        return Speed::Slow;
    };
    if caps.required.is_empty() && caps.preferred.is_empty() {
        return Speed::Slow;
    }
    let required = &caps.required;
    let preferred = &caps.preferred;

    // Score each chat-capable slot independently. We can't just
    // call `score_manifest` on `build_self_manifest(provider)` and
    // map-model-id-back-to-speed, because two slots can share a
    // model name in degenerate configurations; Speed is the
    // ground-truth dispatch key. Walk the speeds directly.
    let manifest = &sovereign_core::models_manifest::DEFAULT_MANIFEST;
    let score_slot = |speed: Speed| -> Option<ModelCandidate> {
        let model_id = provider.model_id_for(speed);
        if model_id.is_empty() || model_id == "unknown" {
            return None;
        }
        let info = manifest.info_for_file(&model_id)?;
        if !oicp::satisfies_required(&info.capabilities, required) {
            return None;
        }
        let score = oicp::score_preferred(&info.capabilities, preferred);
        Some(ModelCandidate {
            score,
            size_gb: info.size_gb,
            model_id,
        })
    };

    let fast = score_slot(Speed::Fast);
    let slow = score_slot(Speed::Slow);

    match (fast, slow) {
        (Some(f), Some(s)) => {
            // Ties go to Fast by our `pick_better` policy
            // (smaller size wins ties). If Fast strictly loses
            // on score, Slow wins. Log both so the decision is
            // auditable from server logs — mirrors the Joiner-
            // side `peer_inference` logging.
            let winner = pick_better(s.clone(), f.clone());
            tracing::info!(
                fast_model = %f.model_id,
                fast_score = f.score,
                fast_size_gb = ?f.size_gb,
                slow_model = %s.model_id,
                slow_score = s.score,
                slow_size_gb = ?s.size_gb,
                picked = %winner.model_id,
                "inference_adapter: slot picked by OICP (score, then size_gb)"
            );
            if winner.model_id == f.model_id
                && winner.score == f.score
                && winner.size_gb == f.size_gb
            {
                Speed::Fast
            } else {
                Speed::Slow
            }
        }
        (Some(f), None) => {
            tracing::info!(
                fast_model = %f.model_id,
                picked = %f.model_id,
                "inference_adapter: only Fast slot satisfies required; picking Fast"
            );
            Speed::Fast
        }
        (None, Some(s)) => {
            tracing::debug!(
                slow_model = %s.model_id,
                "inference_adapter: only Slow slot satisfies required; picking Slow"
            );
            Speed::Slow
        }
        (None, None) => {
            // Neither slot satisfies `required`. This shouldn't
            // normally happen — the Joiner selected us because
            // OUR manifest promised capability; something's out
            // of sync. Fall back to Slow so the request still
            // gets served, but warn loudly.
            tracing::warn!(
                "inference_adapter: neither slot satisfies required caps — \
                 falling back to Slow. Likely a manifest/provider drift bug."
            );
            Speed::Slow
        }
    }
}

/// v0.3 slot selection: latency class picks the primary slot; hint
/// acts as a veto when the primary slot's model cannot serve the
/// requested specialization. Returns `Speed::Slow` as the conservative
/// default if neither loaded slot satisfies the hint.
fn pick_slot_v03(
    provider: &dyn InferenceProvider,
    req: &InferenceRequirements,
) -> Speed {
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
    let primary_available =
        matches!((primary, &fast, &slow), (Speed::Fast, Some(_), _) | (Speed::Slow, _, Some(_)));

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

    tracing::warn!(
        hint = %hint,
        latency_class = ?class,
        "pick_slot_for_oicp (v0.3): neither slot matches hint — serving from Slow"
    );
    Speed::Slow
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::oicp::Capability;

    fn cand(score: f32, size_gb: Option<f32>, id: &str) -> ModelCandidate {
        ModelCandidate {
            score,
            size_gb,
            model_id: id.into(),
        }
    }

    #[test]
    fn pick_better_higher_score_wins() {
        let a = cand(0.5, Some(5.5), "small");
        let b = cand(1.0, Some(16.5), "big");
        assert_eq!(pick_better(a, b).model_id, "big");
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

    fn model(
        id: &str,
        caps: &[(Capability, u8)],
        size_gb: Option<f32>,
    ) -> sovereign_core::oicp::ProviderModel {
        let capabilities: CapabilityProfile = caps.iter().copied().collect();
        sovereign_core::oicp::ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            capabilities,
            context_tokens: 32_768,
            status: sovereign_core::oicp::ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb,
            claims: Vec::new(),
        }
    }

    fn manifest(models: Vec<sovereign_core::oicp::ProviderModel>) -> ProviderManifest {
        ProviderManifest {
            oicp_version: sovereign_core::oicp::OICP_VERSION.to_string(),
            provider: None,
            models,
            knowledge: None,
            federation: None,
        }
    }

    #[test]
    fn score_manifest_picks_smaller_model_on_tie() {
        let nine = model(
            "qwen-9b",
            &[
                (Capability::Analysis, 3),
                (Capability::General, 3),
                (Capability::Code, 3),
                (Capability::Instruction, 3),
                (Capability::Math, 2),
            ],
            Some(5.5),
        );
        let twenty_seven = model(
            "qwen-27b",
            &[
                (Capability::Analysis, 4),
                (Capability::General, 3),
                (Capability::Code, 3),
                (Capability::Instruction, 4),
                (Capability::Math, 3),
                (Capability::Creative, 3),
            ],
            Some(16.5),
        );
        let m = manifest(vec![twenty_seven, nine]);
        let preferred: CapabilityProfile =
            [(Capability::Analysis, 3), (Capability::General, 3)]
                .into_iter()
                .collect();
        let required = CapabilityProfile::new();
        let winner =
            score_manifest(&m, &required, &preferred).expect("satisfies required");
        assert_eq!(winner.model_id, "qwen-9b");
        assert_eq!(winner.size_gb, Some(5.5));
        assert!((winner.score - 1.0).abs() < 1e-3);
    }

    #[test]
    fn score_manifest_picks_higher_score_over_smaller_size() {
        let small_weak = model(
            "small-weak",
            &[(Capability::Analysis, 2), (Capability::General, 2)],
            Some(2.0),
        );
        let big_strong = model(
            "big-strong",
            &[(Capability::Analysis, 4), (Capability::General, 4)],
            Some(16.5),
        );
        let m = manifest(vec![small_weak, big_strong]);
        let preferred: CapabilityProfile =
            [(Capability::Analysis, 4), (Capability::General, 4)]
                .into_iter()
                .collect();
        let winner =
            score_manifest(&m, &CapabilityProfile::new(), &preferred).expect("scores");
        assert_eq!(winner.model_id, "big-strong");
    }

    #[test]
    fn score_manifest_returns_none_when_required_unmet() {
        let weak = model("weak", &[(Capability::Analysis, 1)], Some(1.0));
        let m = manifest(vec![weak]);
        let required: CapabilityProfile = [(Capability::Analysis, 3)].into_iter().collect();
        let preferred = CapabilityProfile::new();
        assert!(score_manifest(&m, &required, &preferred).is_none());
    }

    // ── pick_slot_for_oicp tests ──────────────────────────────
    //
    // These tests guard the peer-side adapter behaviour exposed
    // in the original demo: a Founder with Qwen3.5-9B in Fast and
    // Qwen3.5-27B in Slow must serve a `preferred={Analysis:3,
    // General:3}` request from the 9B (smaller, satisfies
    // requirement perfectly), not from the 27B. Without this, the
    // Joiner's careful OICP routing decision is silently overridden
    // by a hardcoded `Speed::Slow` on the serving side.

    use async_trait::async_trait;
    use sovereign_core::error::Result;
    use sovereign_core::oicp::{
        CapabilityRequirements, InferenceRequirements,
    };
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, ProviderCapabilities,
    };

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
        ) -> Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>,
        > {
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

    fn deepquery_request() -> CompletionRequest {
        // Mirror what `runtime::build_oicp` produces for a
        // DeepQuery: preferred={Analysis:3, General:3}, no
        // required floor.
        let preferred: CapabilityProfile =
            [(Capability::Analysis, 3), (Capability::General, 3)]
                .into_iter()
                .collect();
        let envelope = InferenceRequirements::new()
            .with_capabilities(CapabilityRequirements {
                required: CapabilityProfile::new(),
                preferred,
            });
        CompletionRequest::new("hello").with_oicp(envelope)
    }

    #[test]
    fn pick_slot_picks_fast_when_both_satisfy_deepquery() {
        // The flagship guard: Founder running default.thoughtful
        // (Qwen3.5-9B, 5.5 GB) AND high.thoughtful (Qwen3.5-27B,
        // 16.5 GB). Both fully satisfy DeepQuery's preferred
        // {Analysis:3, General:3}, both score 1.0. Smaller wins
        // → Fast (9B). Without this test, a regression could
        // silently route every federated DeepQuery through the
        // 27B and tank end-to-end latency by 3×.
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0.1".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        assert_eq!(
            pick_slot_for_oicp(&provider, &deepquery_request()),
            Speed::Fast,
        );
    }

    #[test]
    fn pick_slot_picks_slow_when_fast_cannot_satisfy_required() {
        // If the request's `required` floor exceeds Fast's
        // capabilities, Fast drops out of the candidate pool
        // entirely. Slow (the bigger model) is the only choice.
        let provider = StubProvider {
            // Fast = the cpu_only profile's Qwen3-0.6B router
            // model: caps = {General:1, Instruction:2}.
            fast_model: "Qwen3-0.6B-Q4_K_M".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let preferred: CapabilityProfile =
            [(Capability::Analysis, 3)].into_iter().collect();
        let required: CapabilityProfile =
            [(Capability::Analysis, 3)].into_iter().collect();
        let envelope = InferenceRequirements::new().with_capabilities(CapabilityRequirements {
            required,
            preferred,
        });
        let req = CompletionRequest::new("x").with_oicp(envelope);
        assert_eq!(pick_slot_for_oicp(&provider, &req), Speed::Slow);
    }

    #[test]
    fn pick_slot_defaults_to_slow_when_no_oicp_envelope() {
        // Legacy / non-mesh callers: no OICP, no opinion. Preserve
        // the pre-multi-slot behaviour by defaulting to Slow.
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let req = CompletionRequest::new("x");
        assert_eq!(pick_slot_for_oicp(&provider, &req), Speed::Slow);
    }

    #[test]
    fn pick_slot_defaults_to_slow_when_capabilities_empty() {
        // OICP envelope present but capabilities section empty —
        // still no opinion. Default to Slow as above.
        let provider = StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0".into(),
            slow_model: "Qwen3.5-27B.Q8_0".into(),
        };
        let envelope = InferenceRequirements::new().with_capabilities(CapabilityRequirements {
            required: CapabilityProfile::new(),
            preferred: CapabilityProfile::new(),
        });
        let req = CompletionRequest::new("x").with_oicp(envelope);
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
        let envelope = InferenceRequirements::new()
            .with_hint(CapabilityHint::general());
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
            capabilities: CapabilityProfile::default(),
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
        let cand = score_manifest_for_request(&qwen_coder, &req)
            .expect("v0.3 claim scores non-None");
        assert_eq!(cand.model_id, "qwen-coder-32b");
        // Exact hint + latency match → score equals affinity.
        assert!((cand.score - 0.95).abs() < 1e-4);
    }

    #[test]
    fn score_manifest_for_request_falls_back_to_v02_when_no_claims() {
        // Manifest has only legacy capabilities (no claims). Scoring
        // must still work via the v0.2 path.
        let legacy =
            model("legacy", &[(Capability::Analysis, 3)], Some(5.0));
        let m = manifest(vec![legacy]);
        let preferred: CapabilityProfile =
            [(Capability::Analysis, 3)].into_iter().collect();
        let req = InferenceRequirements::new().with_capabilities(
            CapabilityRequirements {
                required: CapabilityProfile::new(),
                preferred,
            },
        );
        let cand = score_manifest_for_request(&m, &req)
            .expect("v0.2 fallback scores");
        assert_eq!(cand.model_id, "legacy");
        assert!((cand.score - 1.0).abs() < 1e-4);
    }
}
