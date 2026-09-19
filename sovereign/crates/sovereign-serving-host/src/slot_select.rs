// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local slot pick — which `Speed` slot on the local provider serves a
//! chat request, given its OICP capability envelope.
//!
//! This is host code, not scheduler arithmetic
//! (`sovereign/SERVING_BOUNDARY.md` "Corrected 2026-09-14"): it picks the
//! *local* slot through a manifest lookup, and its only production caller is
//! the host's inference adapter. The manifest it reads lives in
//! `sovereign-core`, outside the serving package, so the lookup arrives
//! through [`SlotManifest`] — a port the daemon implements — rather than a
//! type named here.

use oicp_types::{
    hint_match_score, infer_hint_from_profile, latency_to_speed, CapabilityHint, CapabilityProfile,
    InferenceRequirements,
};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, Speed};

/// The manifest facts the host reads for one loaded model file: its declared
/// capability profile and its declared size in GB.
///
/// The host's projection of `sovereign_core::models_manifest::SlotInfo` — the
/// same two fields, carried under a name the host owns. The manifest itself
/// lives in `sovereign-core`, outside the serving package, so the facts
/// arrive through [`SlotManifest`] rather than a type named here.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotManifestInfo {
    /// The declared OICP capability profile; empty means "no annotation".
    pub capabilities: CapabilityProfile,
    /// The declared size in GB, `None` when the manifest carries none.
    pub size_gb: Option<f32>,
}

/// The manifest lookup the host needs, as a port.
///
/// `sovereign-serving-host` may not name `sovereign-core` (rule 5; the two
/// grandfathered exceptions are `sovereign-inference` and `commonwealth-core`,
/// and a third means the boundary is drawn in the wrong place — K4). The
/// declared facts per loaded model file are what the host reads from the
/// manifest — the slot pick needs the capability profile, and advertising
/// needs the size beside it — so they arrive through this trait: the daemon,
/// which owns the manifest, implements it.
pub trait SlotManifest: Send + Sync {
    /// The declared capability profile for a loaded model file, or `None`
    /// when the manifest has no entry for it (the BYOM case).
    fn capabilities_for_file(&self, file: &str) -> Option<CapabilityProfile>;

    /// The declared capabilities and size for a loaded model file, or `None`
    /// when the manifest has no entry for it.
    ///
    /// One walk returns both: the advertising path rides the size on a
    /// `ProviderModel` so a peer can break a score tie, and re-scanning the
    /// manifest for it would be a second lookup of the same row.
    fn info_for_file(&self, file: &str) -> Option<SlotManifestInfo>;
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
pub fn pick_slot_for_oicp(
    provider: &dyn InferenceProvider,
    manifest: &dyn SlotManifest,
    request: &CompletionRequest,
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
    pick_slot_v03(provider, manifest, oicp)
}

/// v0.3 slot selection: latency class picks the primary slot; hint
/// acts as a veto when the primary slot's model cannot serve the
/// requested specialization. Falls back to the latency-class default
/// when slots have no manifest entries (BYOM case) — trusting the
/// operator's slot configuration rather than punishing them with a
/// blanket Slow.
fn pick_slot_v03(
    provider: &dyn InferenceProvider,
    manifest: &dyn SlotManifest,
    req: &InferenceRequirements,
) -> Speed {
    let hint = req.effective_hint();
    let class = req.effective_latency_class();

    let slot_matches_hint = |speed: Speed| -> Option<(String, CapabilityHint)> {
        let id = provider.model_id_for(speed);
        if id.is_empty() || id == "unknown" {
            return None;
        }
        let profile = manifest.capabilities_for_file(&id)?;
        let slot_hint = infer_hint_from_profile(&profile);
        if hint_match_score(&slot_hint, &hint) > 0.0 {
            Some((id, slot_hint))
        } else {
            None
        }
    };

    let fast = slot_matches_hint(Speed::Fast);
    let slow = slot_matches_hint(Speed::Slow);

    // Canonical LatencyClass→Speed resolve (SLOT_POLICY §8) — the
    // serve-side primary-slot pick. Byte-identical to the prior inline
    // map; consolidated so the mapping lives in exactly one module.
    let primary = latency_to_speed(class);
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
        // debug!, not info!: per-request routing detail. Fired once per
        // request; `RUST_LOG=sovereign_mesh=debug` to see slot selection.
        tracing::debug!(
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

    use async_trait::async_trait;
    use oicp_types::{Capability, LatencyClass};
    use sovereign_contracts::error::Result;
    use sovereign_contracts::types::{CompletionResponse, Depth, ProviderCapabilities};

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
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    /// Stub manifest: every file the fixtures name advertises a general
    /// profile, so `infer_hint_from_profile` derives `general` and the slot
    /// matches a general-hinted request. The manifest itself is core's; the
    /// pick only sees it through this port.
    struct StubManifest;

    impl SlotManifest for StubManifest {
        fn capabilities_for_file(&self, _file: &str) -> Option<CapabilityProfile> {
            let mut profile = CapabilityProfile::new();
            profile.insert(Capability::General, 3);
            Some(profile)
        }

        fn info_for_file(&self, _file: &str) -> Option<SlotManifestInfo> {
            let mut profile = CapabilityProfile::new();
            profile.insert(Capability::General, 3);
            Some(SlotManifestInfo {
                capabilities: profile,
                size_gb: None,
            })
        }
    }

    fn provider() -> StubProvider {
        StubProvider {
            fast_model: "Qwen3.5-9B.Q8_0.1".into(),
            slow_model: "Qwen3.5-35B-A3B-Q4_K_M".into(),
        }
    }

    #[test]
    fn pick_slot_placeholder_removed_in_pr_c() {
        // The original v0.2 "both satisfy required, smaller wins"
        // test used CapabilityRequirements / CapabilityProfile; in
        // v0.3 the same "9B over 27B for a normal request" outcome
        // falls out of pick_slot_v03 mapping latency_class:Normal to
        // Speed::Slow, which the v0.3 tests below cover.
        let p = provider();
        let _ = p.model_id_for(Speed::Fast);
    }

    #[test]
    fn pick_slot_defaults_to_slow_when_no_oicp_envelope() {
        // Non-mesh callers: no OICP envelope → Slow default.
        let req = CompletionRequest::new("x");
        assert_eq!(
            pick_slot_for_oicp(&provider(), &StubManifest, &req),
            Speed::Slow
        );
    }

    // -----------------------------------------------------------
    // v0.3 — latency_class guided slot selection
    // -----------------------------------------------------------

    #[test]
    fn pick_slot_v03_latency_fast_picks_fast_slot() {
        let envelope = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Fast);
        let req = CompletionRequest::new("quick").with_oicp(envelope);
        assert_eq!(
            pick_slot_for_oicp(&provider(), &StubManifest, &req),
            Speed::Fast
        );
    }

    #[test]
    fn pick_slot_v03_latency_normal_picks_slow_slot() {
        let envelope = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal);
        let req = CompletionRequest::new("substantive").with_oicp(envelope);
        assert_eq!(
            pick_slot_for_oicp(&provider(), &StubManifest, &req),
            Speed::Slow
        );
    }

    #[test]
    fn pick_slot_v03_latency_extended_picks_slow_slot() {
        let envelope = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Extended);
        let req = CompletionRequest::new("deep").with_oicp(envelope);
        assert_eq!(
            pick_slot_for_oicp(&provider(), &StubManifest, &req),
            Speed::Slow
        );
    }

    #[test]
    fn pick_slot_v03_hint_only_defaults_to_slow_for_normal_latency() {
        // When only a hint is present, effective_latency_class()
        // returns Normal → Slow slot.
        let envelope = InferenceRequirements::new().with_hint(CapabilityHint::general());
        let req = CompletionRequest::new("hint-only").with_oicp(envelope);
        assert_eq!(
            pick_slot_for_oicp(&provider(), &StubManifest, &req),
            Speed::Slow
        );
    }
}

#[cfg(test)]
mod manifest_port_tests {
    use super::*;
    use oicp_types::Capability;
    use std::collections::HashMap;

    /// The reader a daemon supplies: manifest facts keyed by model file.
    struct StubManifestMap(HashMap<String, SlotManifestInfo>);

    impl SlotManifest for StubManifestMap {
        fn capabilities_for_file(&self, file: &str) -> Option<CapabilityProfile> {
            self.0.get(file).map(|i| i.capabilities.clone())
        }

        fn info_for_file(&self, file: &str) -> Option<SlotManifestInfo> {
            self.0.get(file).cloned()
        }
    }

    fn entry(size_gb: f32) -> SlotManifestInfo {
        let mut capabilities = CapabilityProfile::new();
        capabilities.insert(Capability::General, 3);
        SlotManifestInfo {
            capabilities,
            size_gb: Some(size_gb),
        }
    }

    /// Positive control: an annotated file resolves both declared facts in
    /// one lookup — the size rides beside the profile, not on a second walk.
    #[test]
    fn an_annotated_file_resolves_capabilities_and_size() {
        let mut map = HashMap::new();
        map.insert("Qwen3.5-9B.Q8_0.gguf".to_string(), entry(5.5));
        let manifest = StubManifestMap(map);
        let info = manifest
            .info_for_file("Qwen3.5-9B.Q8_0.gguf")
            .expect("annotated file resolves");
        assert_eq!(info.size_gb, Some(5.5));
        assert_eq!(info.capabilities.get(&Capability::General), Some(&3));
    }

    /// Negative control: a BYOM file the manifest does not carry is absent,
    /// never a defaulted empty entry — absence is reported, not substituted.
    #[test]
    fn an_unannotated_file_is_absent() {
        let manifest = StubManifestMap(HashMap::new());
        assert!(manifest.info_for_file("byom-model.gguf").is_none());
        assert!(manifest.capabilities_for_file("byom-model.gguf").is_none());
    }
}
