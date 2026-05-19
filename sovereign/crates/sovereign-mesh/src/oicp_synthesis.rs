//! Synthesize this node's OICP `ProviderManifest` from a running
//! `InferenceProvider`.
//!
//! Why this is its own module: building the manifest is a separate
//! concern from adapting wire requests to the core types. Both
//! `inference_adapter` (peer-facing manifest at
//! `/oicp/v1/capabilities`) and `peer_inference::MeshInferenceProvider`
//! (local-side self-scoring) consume the same builder so the two
//! views can't drift.

use commonwealth_inference::oicp::{
    Capability, CapabilityClaim, CapabilityHint, CapabilityProfile, LatencyClass,
    ModelStatus, ProviderInfo, ProviderManifest, ProviderModel, ProviderType,
    OICP_VERSION,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Speed;

/// Resolve a single human-readable model name from a provider,
/// preferring the Slow (synthesis) slot. Used by both the adapter
/// (so peer-side manifest and response `model` fields agree) and
/// by `MeshInferenceProvider` (so local-side scoring uses the same
/// identity the peer would see).
pub fn resolve_primary_model_name(provider: &dyn InferenceProvider) -> String {
    let slow = provider.model_id_for(Speed::Slow);
    if !slow.is_empty() && slow != "unknown" {
        return slow;
    }
    let fast = provider.model_id_for(Speed::Fast);
    if !fast.is_empty() && fast != "unknown" {
        return fast;
    }
    "sovereign-local".to_string()
}

/// Build this node's OICP `ProviderManifest` — one `ProviderModel`
/// entry per loaded chat slot (Fast + Slow), each with the
/// capability profile + size_gb declared for it in
/// `sovereign/models.toml`. Shared between the server adapter (what
/// peers fetch at `/oicp/v1/capabilities`) and the client-side
/// `MeshInferenceProvider` (what local scores itself against) so
/// the two never disagree about our own declared capabilities.
///
/// Why both slots: on a Founder running the `high` profile, Fast
/// is Qwen3-1.7B (routing only) and Slow is Qwen3.5-27B (flagship
/// synthesis). A Joiner whose preferred profile is `{Analysis:3,
/// General:3}` should consider the 9B-class `default.thoughtful`
/// on its own side vs. the 27B on the Founder's — but if the
/// Founder also has a 9B loaded in the Fast slot and it already
/// satisfies `preferred`, the smaller/faster 9B should win over
/// the 27B. That tie-break only works if both are visible in the
/// manifest. Previously we advertised only the Slow-slot model,
/// so a Founder with a perfectly-adequate 9B available looked
/// like it was only offering the 27B, and the selector routed
/// requests to the bigger model unnecessarily.
///
/// Capability resolution (per slot):
///   1. Match the loaded model's filename against every slot in
///      the bundled `ModelsManifest`. A user on the `default`
///      profile with Qwen3.5-9B picks up `general=3, analysis=3,
///      ...` from the manifest automatically.
///   2. Fall back to `General=2, Analysis=2` (Moderate) when the
///      loaded model isn't in the manifest. That's the BYOM path
///      — the user pointed Sovereign at their own GGUF we don't
///      have profiling data for. Conservative but honest.
///
/// The Embed slot is deliberately excluded from the manifest: it
/// is not a chat-completion candidate and peer selection never
/// consults it. Duplicate entries (same model id in Fast + Slow
/// — happens when the provider collapses slots) are coalesced to
/// one, since the OICP scorer treats identical models identically.
pub fn build_self_manifest(provider: &dyn InferenceProvider) -> ProviderManifest {
    let mut seen_ids = std::collections::HashSet::new();
    let mut models: Vec<ProviderModel> = Vec::new();
    // Speed::Fast first, then Speed::Slow. Iterating in this order
    // means that if Fast and Slow resolve to the same underlying
    // model name (e.g. a stripped-down test harness that only
    // loads one model), we keep the Fast-labelled one — arbitrary
    // tie-break; they're the same model either way.
    for speed in [Speed::Fast, Speed::Slow] {
        let model_name = provider.model_id_for(speed);
        if model_name.is_empty() || model_name == "unknown" {
            continue;
        }
        if !seen_ids.insert(model_name.clone()) {
            continue;
        }
        let info = sovereign_core::models_manifest::DEFAULT_MANIFEST
            .info_for_file(&model_name);
        let (capabilities, size_gb) = match info {
            Some(slot) => (slot.capabilities, slot.size_gb),
            None => {
                tracing::debug!(
                    model = %model_name,
                    ?speed,
                    "build_self_manifest: no OICP entry in models.toml — using defaults"
                );
                let mut caps = std::collections::HashMap::new();
                caps.insert(Capability::General, 2u8);
                caps.insert(Capability::Analysis, 2u8);
                (caps, None)
            }
        };
        tracing::info!(
            model = %model_name,
            ?speed,
            caps = ?capabilities,
            size_gb = ?size_gb,
            "build_self_manifest: advertised slot"
        );
        let claims = synthesize_slot_claims(speed, &model_name, &capabilities);
        models.push(ProviderModel {
            id: model_name,
            base_model: None,
            quantization: None,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb,
            claims,
        });
    }

    // Stable mesh aliases for the Slow slot. Advertising
    // `commonwealth/primary` (and the short `primary`) as
    // additional ProviderModel entries makes them routable across
    // the mesh: any node whose Slow slot is loaded shows up as a
    // candidate when another node asks for "primary", regardless
    // of which underlying GGUF each node has loaded (Q4_K_L vs
    // Q6_K, Qwopus vs Darwin, etc.). The receiving daemon
    // resolves the alias locally on inbound /v1/chat/completions,
    // so the call lands on whatever Slow slot is hot.
    //
    // Why both forms: `commonwealth/primary` is the namespaced
    // canonical id; `primary` is the unqualified shortcut the
    // OpenAI-compatible client surface accepts. Advertising both
    // keeps either form's load-balancer view in sync.
    //
    // We re-use the Slow slot's claims/size_gb/capabilities so
    // alias rows are scored identically to the underlying slot.
    // Skipped if there's no Slow slot loaded — a node without a
    // primary can't honour a "primary" request, so it shouldn't
    // appear as a candidate.
    let slow_model_name = provider.model_id_for(Speed::Slow);
    if !slow_model_name.is_empty() && slow_model_name != "unknown" {
        let info = sovereign_core::models_manifest::DEFAULT_MANIFEST
            .info_for_file(&slow_model_name);
        let (capabilities, size_gb) = match info {
            Some(slot) => (slot.capabilities, slot.size_gb),
            None => {
                let mut caps = std::collections::HashMap::new();
                caps.insert(Capability::General, 2u8);
                caps.insert(Capability::Analysis, 2u8);
                (caps, None)
            }
        };
        for alias_id in ["commonwealth/primary", "primary"] {
            if !seen_ids.insert(alias_id.to_string()) {
                continue;
            }
            // Synthesize claims with the alias id so the
            // ProviderModel emits coherent attribution if a peer
            // hits this row. The underlying slot is still Slow,
            // so callers get the same latency/quality profile.
            let claims = synthesize_slot_claims(Speed::Slow, alias_id, &capabilities);
            tracing::info!(
                alias = %alias_id,
                target = %slow_model_name,
                caps = ?capabilities,
                "build_self_manifest: advertised primary alias"
            );
            models.push(ProviderModel {
                id: alias_id.to_string(),
                base_model: None,
                quantization: None,
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded: true,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb,
                claims,
            });
        }
    }

    // Mirror of the primary-alias block above for the Fast slot.
    // Symmetric `commonwealth/fast` + bare `fast` advertisement
    // makes the Fast slot routable by alias across the mesh, the
    // same way the Slow slot is via `commonwealth/primary`. Without
    // this, a caller asking for `commonwealth/fast` hits "no node
    // advertises model 'commonwealth/fast'" because the Fast slot
    // only advertised its concrete GGUF id (e.g.
    // `Qwen3.5-9B-UD-MTP-Q6_K_XL`). Observed 2026-05-19: search-gym
    // judge calls and any other Fast-aliased traffic returned 503
    // until this block landed.
    //
    // Skipped if there's no Fast slot loaded, mirroring the primary
    // block's gate.
    let fast_model_name = provider.model_id_for(Speed::Fast);
    if !fast_model_name.is_empty() && fast_model_name != "unknown" {
        let info = sovereign_core::models_manifest::DEFAULT_MANIFEST
            .info_for_file(&fast_model_name);
        let (capabilities, size_gb) = match info {
            Some(slot) => (slot.capabilities, slot.size_gb),
            None => {
                let mut caps = std::collections::HashMap::new();
                caps.insert(Capability::General, 2u8);
                caps.insert(Capability::Analysis, 2u8);
                (caps, None)
            }
        };
        for alias_id in ["commonwealth/fast", "fast"] {
            if !seen_ids.insert(alias_id.to_string()) {
                continue;
            }
            let claims = synthesize_slot_claims(Speed::Fast, alias_id, &capabilities);
            tracing::info!(
                alias = %alias_id,
                target = %fast_model_name,
                caps = ?capabilities,
                "build_self_manifest: advertised fast alias"
            );
            models.push(ProviderModel {
                id: alias_id.to_string(),
                base_model: None,
                quantization: None,
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded: true,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb,
                claims,
            });
        }
    }

    // PR-E2: Code specialist. Separate ProviderModel entry so peer
    // schedulers can see the `code` hint claim without having to
    // first elicit a hot-swap. Only emitted when the provider
    // actually has a Code specialist configured — the default
    // `InferenceProvider::code_model_id()` returns `None`, so
    // single-model / remote providers skip this branch.
    //
    // We mark the code slot `loaded: false` because it shares the
    // lazy chat mutex with the primary — one of them can be
    // resident at a time. That lets the selector treat code and
    // primary as equally "warm-ish" rather than double-counting
    // either one.
    if let Some(code_name) = provider.code_model_id() {
        if !code_name.is_empty()
            && code_name != "unknown"
            && seen_ids.insert(code_name.clone())
        {
            let info = sovereign_core::models_manifest::DEFAULT_MANIFEST
                .info_for_file(&code_name);
            let (capabilities, size_gb) = match info {
                Some(slot) => (slot.capabilities, slot.size_gb),
                None => {
                    let mut caps = std::collections::HashMap::new();
                    caps.insert(Capability::Code, 3u8);
                    caps.insert(Capability::General, 2u8);
                    (caps, None)
                }
            };
            tracing::info!(
                model = %code_name,
                caps = ?capabilities,
                size_gb = ?size_gb,
                "build_self_manifest: advertised code specialist"
            );
            // Code slot always advertises as Slow-tier — it carries
            // the full context window and long output budget, and a
            // code-hinted request is never a routing/classification
            // Fast call. The hint is forced to `code` regardless of
            // the filename heuristic, because this slot's entire
            // reason for existing is the `code` claim.
            let code_hint_claims = synthesize_code_slot_claims(&code_name, &capabilities);
            models.push(ProviderModel {
                id: code_name,
                base_model: None,
                quantization: None,
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded: false,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb,
                claims: code_hint_claims,
            });
        }
    }
    // Provider name reflects the configured chat primary — that's
    // the name response attribution uses ("qwen3.5-27b @ peer
    // BeefyMac"). Ask the provider directly for its Slow slot
    // (with Fast and a "sovereign-local" sentinel as fallbacks)
    // rather than guessing across advertised slots. The earlier
    // max-by-size heuristic mis-identified the primary when a
    // larger code-slot GGUF outweighed the chat primary — e.g.
    // Qwopus3.6-35B Q8 (~34 GB code slot) shadowing Darwin-36B Q6
    // (~28 GB primary) so peers attributed every reply to the
    // code model.
    let provider_name = resolve_primary_model_name(provider);
    ProviderManifest {
        oicp_version: OICP_VERSION.to_string(),
        provider: Some(ProviderInfo {
            name: Some(provider_name),
            provider_type: Some(ProviderType::Mesh),
        }),
        models,
        knowledge: None,
        federation: None,
    }
}

/// Synthesize v0.3 capability claims for a loaded slot.
///
/// Two-slot model: Fast carries a `LatencyClass::Fast` claim with a
/// reduced context window (routing / classification is always short)
/// and a small output budget; Slow carries a `LatencyClass::Normal`
/// claim with the full advertised context. The hint tracks the
/// model's code specialization by name ("coder" / "code-llama"
/// variants → `code`), falling back to `general`.
///
/// Affinity is derived from the v0.2 proficiency for the relevant
/// capability (Code proficiency for code-hint claims; max of
/// General/Analysis/Instruction proficiencies for general). This
/// keeps the two advertising surfaces in agreement until PR-E
/// replaces the heuristic with structured per-model config.
pub(crate) fn synthesize_slot_claims(
    speed: Speed,
    model_name: &str,
    profile: &CapabilityProfile,
) -> Vec<CapabilityClaim> {
    let lower = model_name.to_lowercase();
    let is_code_specialist = lower.contains("coder")
        || lower.contains("code-llama")
        || lower.contains("codellama")
        || lower.contains("deepseek-coder");

    let (hint, affinity) = if is_code_specialist {
        let code = profile.get(&Capability::Code).copied().unwrap_or(0);
        (
            CapabilityHint::code(),
            (code as f32 / 4.0).clamp(0.0, 1.0),
        )
    } else {
        let best = [
            Capability::General,
            Capability::Analysis,
            Capability::Instruction,
        ]
        .into_iter()
        .map(|c| profile.get(&c).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
        (
            CapabilityHint::general(),
            (best as f32 / 4.0).clamp(0.0, 1.0),
        )
    };

    // Per-slot context/output envelopes:
    //
    // - Fast advertises **two** claims, per OICP-v0.3 §2.3 ("a node
    //   running a single model may publish one fast-latency claim
    //   with short context, higher affinity, and one … with longer
    //   context, lower affinity"):
    //
    //     * **FastShort** — `max_context=2_048`, `max_output=512`.
    //       Routes Phase 1b coverage / Phase 3 cluster naming /
    //       Phase 5 / Phase 6 / interactive routing calls through the
    //       continuous-batched companion context (`n_seq_max=8`),
    //       worth a 2.1–2.8× wall-clock win measured in
    //       `bench_decode_batch.rs`. Higher affinity so it wins when
    //       both claims pass the gates.
    //     * **FastLong** — `max_context=8_000`, `max_output=24_576`.
    //       Catches Phase 1 chapter ingestion (which asks for the
    //       full output budget) and any other call FastShort's hard
    //       gates eliminate. Routes through the original `fast` slot
    //       at `n_seq_max=1`. Slightly lower affinity so the
    //       scheduler prefers FastShort whenever both gates pass.
    //
    //   Hard gates do all the routing work — composers that attach
    //   `max_output_tokens=512` (Phase 1b/3/5/6) automatically land
    //   on FastShort; Phase 1 with `max_output_tokens` unset (or
    //   ≥513) falls through to FastLong.
    //
    // - Slow carries a single Normal-latency claim with the full
    //   advertised context — unchanged from the prior advertisement.
    match speed {
        Speed::Fast => vec![
            CapabilityClaim::new(
                hint.clone(),
                LatencyClass::Fast,
                2_048,
                512,
                (affinity + 0.05).clamp(0.0, 1.0),
            ),
            CapabilityClaim::new(
                hint,
                LatencyClass::Fast,
                8_000,
                24_576,
                affinity,
            ),
        ],
        Speed::Medium | Speed::Slow => vec![CapabilityClaim::new(
            hint,
            LatencyClass::Normal,
            32_768,
            4_000,
            affinity,
        )],
    }
}

/// PR-E2: synthesize claims for a dedicated Code specialist model.
///
/// Unlike `synthesize_slot_claims`, the hint is pinned to `code`
/// regardless of filename — this model is the code slot by
/// configuration, not by heuristic. Affinity is read from the v0.2
/// Code proficiency (same source of truth the filename heuristic
/// consults) so peer scoring lines up with what the local provider
/// would self-report for the same model.
///
/// Latency is `LatencyClass::Normal` — the slot hot-swaps with the
/// primary in `EmbeddedLlamaCpp`, so first-request TTFT is dominated
/// by a 5–30s reload. Advertising it as Fast would produce incorrect
/// routing for single-turn classification calls. Output budget is
/// generous (4000 tokens) so long refactors and documentation
/// generation aren't clipped mid-diff.
pub(crate) fn synthesize_code_slot_claims(
    model_name: &str,
    profile: &CapabilityProfile,
) -> Vec<CapabilityClaim> {
    let code = profile.get(&Capability::Code).copied().unwrap_or(0);
    // Floor affinity at 0.5 when the filename smells like a code
    // model even if the manifest doesn't report Code proficiency
    // — a BYOM code GGUF not yet in `models.toml` should still be
    // discoverable as code-capable by peers.
    let lower = model_name.to_lowercase();
    let filename_signals_code = lower.contains("coder")
        || lower.contains("code-llama")
        || lower.contains("codellama")
        || lower.contains("deepseek-coder");
    let affinity_floor = if filename_signals_code { 0.5 } else { 0.0 };
    let affinity = ((code as f32 / 4.0).clamp(0.0, 1.0)).max(affinity_floor);

    vec![CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_768,
        4_000,
        affinity,
    )]
}

#[cfg(test)]
mod self_manifest_tests {
    //! PR-E2: verify `build_self_manifest` surfaces a third
    //! ProviderModel with a `code`-hinted claim when the underlying
    //! provider reports `code_model_id() == Some(...)`. Regression
    //! coverage for the "peer can't see my code slot" bug that
    //! would cause mesh routing to round-trip code requests through
    //! a hot-swap instead of picking the configured specialist.
    use super::{build_self_manifest, synthesize_code_slot_claims};
    use async_trait::async_trait;
    use commonwealth_inference::oicp::{Capability, CapabilityHint, CapabilityProfile, LatencyClass};
    use futures::Stream;
    use sovereign_core::traits::InferenceProvider;
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
    };
    use sovereign_core::Result;
    use std::pin::Pin;

    /// Minimal stub that mimics the three-slot shape: fast + primary
    /// + optional code. We intentionally don't need a real model —
    /// only the metadata methods are consulted by
    /// `build_self_manifest`.
    struct SlotStub {
        fast_id: &'static str,
        primary_id: &'static str,
        code_id: Option<&'static str>,
    }

    #[async_trait]
    impl InferenceProvider for SlotStub {
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
            unreachable!("build_self_manifest never calls complete")
        }
        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unreachable!("build_self_manifest never calls complete_stream")
        }
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            unreachable!("build_self_manifest never calls embed")
        }
        fn model_id_for(&self, speed: Speed) -> String {
            match speed {
                Speed::Fast => self.fast_id.to_string(),
                Speed::Medium | Speed::Slow => self.primary_id.to_string(),
            }
        }
        fn code_model_id(&self) -> Option<String> {
            self.code_id.map(|s| s.to_string())
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 32_768,
                supports_structured_output: false,
                relative_speed: Speed::Medium,
                relative_reasoning: Depth::Deep,
            }
        }
    }

    #[test]
    fn manifest_omits_code_entry_when_no_code_slot() {
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "primary.Q5_K_M",
            code_id: None,
        };
        let manifest = build_self_manifest(&stub);
        // No model in the manifest carries a `code` claim when
        // the provider has no code slot configured.
        let any_code_claim = manifest.models.iter().flat_map(|m| m.claims.iter())
            .any(|c| c.hint == CapabilityHint::code());
        assert!(
            !any_code_claim,
            "manifest leaked a code claim even without a code slot: {:#?}",
            manifest.models
        );
        // Fast + primary GGUF + 2 primary aliases (commonwealth/primary, primary).
        assert_eq!(manifest.models.len(), 4, "expected fast + primary + 2 aliases: {:#?}", manifest.models);
    }

    #[test]
    fn manifest_emits_routable_primary_aliases() {
        // Each node must advertise `commonwealth/primary` and the
        // short `primary` so a peer requesting either form can
        // see this node as a candidate and load-balance to it.
        // Without the aliases, peer routing only works when both
        // nodes have the same underlying GGUF id loaded —
        // brittle the moment one node swaps quants.
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "primary.Q5_K_M",
            code_id: None,
        };
        let manifest = build_self_manifest(&stub);
        let alias_ids: Vec<&str> = manifest
            .models
            .iter()
            .filter(|m| m.id == "commonwealth/primary" || m.id == "primary")
            .map(|m| m.id.as_str())
            .collect();
        assert!(
            alias_ids.iter().any(|id| *id == "commonwealth/primary"),
            "manifest must advertise `commonwealth/primary`: {:#?}",
            manifest.models
        );
        assert!(
            alias_ids.iter().any(|id| *id == "primary"),
            "manifest must advertise the short `primary` alias: {:#?}",
            manifest.models
        );
        // Alias entries claim `loaded: true` because the
        // underlying Slow slot is loaded — anything else would
        // make the load-balancer skip this node when it should
        // be a viable candidate.
        for m in manifest.models.iter().filter(|m| alias_ids.contains(&m.id.as_str())) {
            assert!(m.status.loaded, "alias `{}` must claim loaded=true", m.id);
            assert!(m.status.available, "alias `{}` must claim available=true", m.id);
        }
    }

    #[test]
    fn manifest_emits_code_entry_with_aliases() {
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "primary.Q5_K_M",
            code_id: Some("qwen-coder-32b-instruct.Q4_K_M"),
        };
        let manifest = build_self_manifest(&stub);
        // fast + primary GGUF + 2 primary aliases + code.
        assert_eq!(
            manifest.models.len(),
            5,
            "expected fast + primary + 2 aliases + code: {:#?}",
            manifest.models
        );
        let code_model = manifest
            .models
            .iter()
            .find(|m| m.id == "qwen-coder-32b-instruct.Q4_K_M")
            .expect("code model should be in manifest");
        assert!(
            !code_model.status.loaded,
            "code slot shares the lazy chat mutex — must not claim to be resident"
        );
        let code_claim = code_model
            .claims
            .iter()
            .find(|c| c.hint == CapabilityHint::code())
            .expect("code model must carry a `code` hint claim");
        assert_eq!(code_claim.latency_class, LatencyClass::Normal);
        // Affinity floors at 0.5 even when v2 proficiency data is
        // missing, because the filename signals code-specialist
        // strongly enough.
        assert!(
            code_claim.affinity >= 0.5,
            "code affinity should floor at 0.5 for filename-signalled BYOM coders: {}",
            code_claim.affinity
        );
    }

    #[test]
    fn manifest_does_not_duplicate_when_code_id_equals_primary_id() {
        // Defensive: misconfig where the user points the code slot
        // at the same GGUF as the primary. The manifest should not
        // emit a duplicate model entry with a different hint.
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "shared.Q5_K_M",
            code_id: Some("shared.Q5_K_M"),
        };
        let manifest = build_self_manifest(&stub);
        let ids: Vec<_> = manifest.models.iter().map(|m| m.id.clone()).collect();
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(ids.len(), uniq.len(), "no duplicate ids: {ids:?}");
    }

    #[test]
    fn synthesize_code_claim_uses_v2_proficiency_when_available() {
        let mut profile: CapabilityProfile = std::collections::HashMap::new();
        profile.insert(Capability::Code, 4u8); // max proficiency
        let claims = synthesize_code_slot_claims("my-custom-coder", &profile);
        assert_eq!(claims.len(), 1);
        // v2 proficiency 4/4 → affinity 1.0.
        assert!((claims[0].affinity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn synthesize_code_claim_uses_floor_for_unknown_byom_coder() {
        // Empty profile → 0 proficiency → floor kicks in because the
        // filename matches the heuristic.
        let profile: CapabilityProfile = std::collections::HashMap::new();
        let claims = synthesize_code_slot_claims("codellama-13b-byom.Q4_K_M", &profile);
        assert!(claims[0].affinity >= 0.5);
    }

    #[test]
    fn synthesize_code_claim_no_floor_for_non_code_filename() {
        // If the filename doesn't signal code and no v2 proficiency
        // is known, affinity stays at 0.0 — peers will rank this
        // slot behind ones with real coding signal.
        let profile: CapabilityProfile = std::collections::HashMap::new();
        let claims = synthesize_code_slot_claims("mystery-model.Q4_K_M", &profile);
        assert_eq!(claims[0].affinity, 0.0);
    }

    #[test]
    fn manifest_provider_name_is_chat_primary_not_largest_slot() {
        // Regression for the "peer attributes replies to the code
        // slot" bug observed 2026-05-10: a user swapped the chat
        // primary to Darwin-36B Q6 (~28 GB) while leaving a larger
        // Qwopus3.6-35B Q8 (~34 GB) in the code slot. The OICP
        // `provider.name` peers see came out as the code model
        // because the picker maxed by size_gb instead of asking
        // the provider for its Speed::Slow slot.
        //
        // The provider's primary is what attribution should
        // reflect, regardless of whether some other configured
        // slot happens to be a larger file on disk.
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "chat-primary.Q5_K_M",
            code_id: Some("huge-code-specialist.Q8_0"),
        };
        let manifest = build_self_manifest(&stub);
        let provider = manifest.provider.expect("provider block populated");
        assert_eq!(
            provider.name.as_deref(),
            Some("chat-primary.Q5_K_M"),
            "provider.name must reflect the configured chat primary, \
             not whichever advertised slot happens to be largest"
        );
    }
}
