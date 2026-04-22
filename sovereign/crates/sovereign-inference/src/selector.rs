use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use sovereign_core::oicp::{self, InferenceRequirements, ProviderManifest, ShardingPrivacy};
use sovereign_core::types::CompletionRequest;
use sovereign_core::Result;

use crate::health::HealthTracker;

pub struct BackendEntry {
    pub name: String,
    pub health: Arc<HealthTracker>,
    pub priority: u32,
    pub cost_per_token: Option<f64>,
    /// Whether this backend is local (embedded inference, not remote).
    pub is_local: bool,
    /// Cached OICP manifest. Updated during health checks.
    /// None if the backend doesn't support OICP.
    pub oicp_manifest: Arc<RwLock<Option<ProviderManifest>>>,
    /// Fractional availability for inference (0.0–1.0). Set from the peer's
    /// gossiped NodeCapabilities.inference_availability. 1.0 = fully idle.
    /// Used by CapabilityAwareSelector to route away from busy peers.
    pub inference_availability: f32,
}

impl BackendEntry {
    pub fn new_local(name: &str, health: Arc<HealthTracker>, priority: u32) -> Self {
        Self {
            name: name.to_string(),
            health,
            priority,
            cost_per_token: None,
            is_local: true,
            oicp_manifest: Arc::new(RwLock::new(None)),
            inference_availability: 1.0,
        }
    }

    pub fn new_remote(
        name: &str,
        health: Arc<HealthTracker>,
        priority: u32,
        cost_per_token: Option<f64>,
    ) -> Self {
        Self {
            name: name.to_string(),
            health,
            priority,
            cost_per_token,
            is_local: false,
            oicp_manifest: Arc::new(RwLock::new(None)),
            inference_availability: 1.0,
        }
    }
}

/// Selects a backend for a given request.
/// Internal to HybridProvider — not exposed on the Runtime.
#[async_trait]
pub trait BackendSelector: Send + Sync {
    async fn select(
        &self,
        request: &CompletionRequest,
        backends: &[BackendEntry],
    ) -> Result<usize>;
}

/// Use the highest-priority healthy backend. Fall through on failure.
pub struct PrioritySelector;

#[async_trait]
impl BackendSelector for PrioritySelector {
    async fn select(
        &self,
        _request: &CompletionRequest,
        backends: &[BackendEntry],
    ) -> Result<usize> {
        let mut candidates: Vec<(usize, u32)> = backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.health.is_healthy())
            .map(|(i, b)| (i, b.priority))
            .collect();

        candidates.sort_by_key(|(_, p)| *p);

        candidates.first().map(|(i, _)| *i).ok_or_else(|| {
            sovereign_core::Error::Inference("No healthy backends available".to_string())
        })
    }
}

/// Minimize estimated cost. Prefer local (no cost), then cheapest remote.
pub struct CostMinimizingSelector;

#[async_trait]
impl BackendSelector for CostMinimizingSelector {
    async fn select(
        &self,
        _request: &CompletionRequest,
        backends: &[BackendEntry],
    ) -> Result<usize> {
        backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.health.is_healthy())
            .min_by(|(_, a), (_, b)| {
                let cost_a = a.cost_per_token.unwrap_or(0.0);
                let cost_b = b.cost_per_token.unwrap_or(0.0);
                cost_a.partial_cmp(&cost_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .ok_or_else(|| {
                sovereign_core::Error::Inference("No healthy backends available".to_string())
            })
    }
}

/// Minimize estimated latency using health_score * inverse_latency.
pub struct LatencyMinimizingSelector;

#[async_trait]
impl BackendSelector for LatencyMinimizingSelector {
    async fn select(
        &self,
        _request: &CompletionRequest,
        backends: &[BackendEntry],
    ) -> Result<usize> {
        backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.health.is_healthy())
            .min_by(|(_, a), (_, b)| {
                let latency_a = a.health.latency_ms().max(1);
                let latency_b = b.health.latency_ms().max(1);
                latency_a.cmp(&latency_b)
            })
            .map(|(i, _)| i)
            .ok_or_else(|| {
                sovereign_core::Error::Inference("No healthy backends available".to_string())
            })
    }
}

/// Never send requests to remote backends unless all local backends are unhealthy.
pub struct LocalFirstSelector;

#[async_trait]
impl BackendSelector for LocalFirstSelector {
    async fn select(
        &self,
        _request: &CompletionRequest,
        backends: &[BackendEntry],
    ) -> Result<usize> {
        let local = backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.health.is_healthy() && b.is_local)
            .min_by_key(|(_, b)| b.priority);

        if let Some((i, _)) = local {
            return Ok(i);
        }

        backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.health.is_healthy())
            .min_by_key(|(_, b)| b.priority)
            .map(|(i, _)| i)
            .ok_or_else(|| {
                sovereign_core::Error::Inference("No healthy backends available".to_string())
            })
    }
}

// ─── OICP Capability-Aware Selector ───────────────────────────

/// Selects the backend whose available models best match the request's
/// OICP requirements. Falls back to priority ordering if no backend
/// has an OICP manifest or if the request has no OICP requirements.
pub struct CapabilityAwareSelector {
    pub fallback: Box<dyn BackendSelector>,
}

#[async_trait]
impl BackendSelector for CapabilityAwareSelector {
    async fn select(
        &self,
        request: &CompletionRequest,
        backends: &[BackendEntry],
    ) -> Result<usize> {
        let requirements = match &request.oicp {
            Some(r) => r,
            None => return self.fallback.select(request, backends).await,
        };

        // Privacy check: if LocalOnly (the OICP §3.1 default), only consider
        // local backends.
        if requirements.sharding() == ShardingPrivacy::LocalOnly {
            if let Some(idx) = backends
                .iter()
                .position(|b| b.health.is_healthy() && b.is_local)
            {
                return Ok(idx);
            }
            return self.fallback.select(request, backends).await;
        }

        // Score each backend that has an OICP manifest.
        let mut best_idx: Option<usize> = None;
        let mut best_score: f32 = -1.0;

        for (idx, backend) in backends.iter().enumerate() {
            if !backend.health.is_healthy() {
                continue;
            }

            let manifest_guard = backend.oicp_manifest.read().await;
            let manifest = match manifest_guard.as_ref() {
                Some(m) => m,
                None => continue,
            };

            if let Some(score) = score_backend_manifest(manifest, requirements) {
                let availability = backend.inference_availability.clamp(0.20, 1.0);
                let weighted = score * availability;
                if weighted > best_score {
                    best_score = weighted;
                    best_idx = Some(idx);
                }
            }
        }

        match best_idx {
            Some(idx) => Ok(idx),
            None => self.fallback.select(request, backends).await,
        }
    }
}

/// Score the best matching (model, claim) pair in a backend's manifest.
///
/// As of v0.3, scoring prefers the claim-based path
/// (`oicp::score_claim_for_request`) whenever any model publishes
/// claims. When no model publishes claims the function falls back to
/// the legacy v0.2 capability-profile path. The fallback goes away
/// in PR-C once every producer emits claims.
///
/// Returns `None` when no (model, claim) pair — or, on the v0.2 path,
/// no model — can serve the request.
fn score_backend_manifest(
    manifest: &ProviderManifest,
    requirements: &InferenceRequirements,
) -> Option<f32> {
    let any_claims = manifest.models.iter().any(|m| !m.claims.is_empty());

    if any_claims {
        // v0.3 path. A claim's own max_context is already the hard
        // gate — we don't need the manifest-level min_tokens filter.
        return manifest
            .models
            .iter()
            .filter(|m| m.status.available)
            .flat_map(|m| m.claims.iter())
            .filter_map(|c| oicp::score_claim_for_request(c, requirements))
            .fold(None, |acc, s| Some(acc.map_or(s, |a: f32| a.max(s))));
    }

    // v0.2 fallback. Removed in PR-C.
    let required = requirements.required();
    let preferred = requirements.preferred();
    let min_tokens = requirements.min_tokens();

    let scores: Vec<f32> = manifest
        .models
        .iter()
        .filter(|m| m.status.available)
        .filter(|m| oicp::satisfies_required(&m.capabilities, required))
        .filter(|m| min_tokens.map_or(true, |min| m.context_tokens >= min))
        .map(|m| oicp::score_preferred(&m.capabilities, preferred))
        .collect();

    if scores.is_empty() {
        None
    } else {
        Some(scores.into_iter().fold(0.0f32, f32::max))
    }
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::HealthTracker;
    use sovereign_core::oicp::{
        Capability, CapabilityClaim, CapabilityHint, CapabilityProfile,
        CapabilityRequirements, InferenceRequirements, LatencyClass, ModelStatus,
        ProviderManifest, ProviderModel,
    };
    use sovereign_core::types::CompletionRequest;

    fn make_manifest(code_score: u8) -> ProviderManifest {
        let mut caps = CapabilityProfile::default();
        caps.insert(Capability::Code, code_score);
        ProviderManifest::new(vec![ProviderModel {
            id: "test-model".into(),
            base_model: None,
            quantization: None,
            capabilities: caps,
            context_tokens: 4096,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: None,
            claims: Vec::new(),
        }])
    }

    async fn entry_with_manifest_and_availability(
        name: &str,
        priority: u32,
        availability: f32,
        manifest: ProviderManifest,
    ) -> BackendEntry {
        let mut entry = BackendEntry::new_local(name, Arc::new(HealthTracker::new()), priority);
        entry.inference_availability = availability;
        *entry.oicp_manifest.write().await = Some(manifest);
        entry
    }

    fn request_requiring_code(min_level: u8) -> CompletionRequest {
        let mut required = CapabilityProfile::default();
        required.insert(Capability::Code, min_level);
        // Use MeshAllowed so the selector doesn't short-circuit to LocalOnly.
        let reqs = InferenceRequirements::new()
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_capabilities(CapabilityRequirements { required, preferred: Default::default() });
        CompletionRequest::new("test").with_oicp(reqs)
    }

    #[tokio::test]
    async fn availability_weighted_prefers_idle_over_hot_when_preferred_scores_equal() {
        // Both backends have the same manifest (Code=5).
        // preferred = {Code: 5} → score_preferred = min(5/5, 1.0) = 1.0 for both.
        // hot:  1.0 × 0.20 = 0.20
        // idle: 1.0 × 1.00 = 1.00  → idle wins
        let manifest = make_manifest(5);
        let hot = entry_with_manifest_and_availability("hot", 1, 0.20, manifest.clone()).await;
        let idle = entry_with_manifest_and_availability("idle", 2, 1.00, manifest).await;

        let mut required = CapabilityProfile::default();
        required.insert(Capability::Code, 1);
        let mut preferred_caps = CapabilityProfile::default();
        preferred_caps.insert(Capability::Code, 5);
        let reqs = InferenceRequirements::new()
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_capabilities(CapabilityRequirements { required, preferred: preferred_caps });
        let request = CompletionRequest::new("test").with_oicp(reqs);

        let selector = CapabilityAwareSelector { fallback: Box::new(PrioritySelector) };
        let selected = selector.select(&request, &[hot, idle]).await.unwrap();
        assert_eq!(selected, 1, "idle backend (1.00 weight) must beat hot backend (0.20 weight) with equal preferred scores");
    }

    #[tokio::test]
    async fn falls_back_to_priority_when_no_oicp_requirements() {
        let high_prio = BackendEntry::new_local("high", Arc::new(HealthTracker::new()), 1);
        let low_prio = BackendEntry::new_local("low", Arc::new(HealthTracker::new()), 2);

        let selector = CapabilityAwareSelector { fallback: Box::new(PrioritySelector) };
        let request = CompletionRequest::new("no oicp");
        let selected = selector.select(&request, &[high_prio, low_prio]).await.unwrap();
        assert_eq!(selected, 0, "without OICP requirements, PrioritySelector must pick priority=1");
    }

    #[tokio::test]
    async fn falls_back_to_priority_when_no_backend_has_manifest() {
        // Neither backend has an OICP manifest — fall through to PrioritySelector.
        let high_prio = BackendEntry::new_local("high", Arc::new(HealthTracker::new()), 1);
        let low_prio = BackendEntry::new_local("low", Arc::new(HealthTracker::new()), 2);

        let selector = CapabilityAwareSelector { fallback: Box::new(PrioritySelector) };
        let selected = selector
            .select(&request_requiring_code(1), &[high_prio, low_prio])
            .await
            .unwrap();
        assert_eq!(selected, 0, "when no backend has a manifest, PrioritySelector must win");
    }

    #[tokio::test]
    async fn superior_capability_beats_idle_peer_when_gap_is_large_enough() {
        // score_preferred uses ratio = min(have/want, 1.0) averaged across preferred dims.
        // preferred = {Code:10, Analysis:10}:
        //   strong {Code:10, Analysis:10}: score = 1.0 → weighted = 1.0 × 0.20 = 0.20
        //   weak   {Code: 1, Analysis: 1}: score = 0.1 → weighted = 0.1 × 1.00 = 0.10
        // → hot strong wins (0.20 > 0.10).
        let mut pref = CapabilityProfile::default();
        pref.insert(Capability::Code, 10);
        pref.insert(Capability::Analysis, 10);
        let mut req_caps = CapabilityProfile::default();
        req_caps.insert(Capability::Code, 1);
        let reqs = InferenceRequirements::new()
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_capabilities(CapabilityRequirements { required: req_caps, preferred: pref });
        let request = CompletionRequest::new("test").with_oicp(reqs);

        let mut strong_caps_map = CapabilityProfile::default();
        strong_caps_map.insert(Capability::Code, 10);
        strong_caps_map.insert(Capability::Analysis, 10);
        let mut weak_caps_map = CapabilityProfile::default();
        weak_caps_map.insert(Capability::Code, 1);
        weak_caps_map.insert(Capability::Analysis, 1);

        let make_multi_manifest = |caps: CapabilityProfile| {
            ProviderManifest::new(vec![ProviderModel {
                id: "m".into(),
                base_model: None,
                quantization: None,
                capabilities: caps,
                context_tokens: 4096,
                status: ModelStatus { available: true, loaded: true,
                    estimated_tokens_per_sec: None, estimated_ttft_ms: None,
                    estimated_load_time_sec: None },
                size_gb: None,
                claims: Vec::new(),
            }])
        };

        let hot_strong = entry_with_manifest_and_availability(
            "hot-strong", 1, 0.20, make_multi_manifest(strong_caps_map)
        ).await;
        let idle_weak = entry_with_manifest_and_availability(
            "idle-weak", 2, 1.00, make_multi_manifest(weak_caps_map)
        ).await;

        let selector = CapabilityAwareSelector { fallback: Box::new(PrioritySelector) };
        let selected = selector.select(&request, &[hot_strong, idle_weak]).await.unwrap();
        assert_eq!(selected, 0, "hot node with 10× better preferred-capability score must beat the idle peer's 5× availability bonus");
    }

    // -----------------------------------------------------------
    // v0.3 §6 — claim-based selection in CapabilityAwareSelector
    // -----------------------------------------------------------

    fn manifest_with_claims(claims: Vec<CapabilityClaim>) -> ProviderManifest {
        ProviderManifest::new(vec![ProviderModel {
            id: "v03-model".into(),
            base_model: None,
            quantization: None,
            capabilities: CapabilityProfile::default(),
            context_tokens: 32_000,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: None,
            claims,
        }])
    }

    fn v03_request(
        hint: CapabilityHint,
        lc: LatencyClass,
        ctx: u32,
        out: u32,
    ) -> CompletionRequest {
        let reqs = InferenceRequirements::new()
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_hint(hint)
            .with_latency_class(lc)
            .with_context_tokens(ctx)
            .with_max_output_tokens(out);
        CompletionRequest::new("test").with_oicp(reqs)
    }

    #[tokio::test]
    async fn claim_based_code_request_routes_to_specialist() {
        // §6.2 coder-collective scenario within the local selector.
        let qwen_coder = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::code(),
            LatencyClass::Normal,
            32_000,
            4_000,
            0.95,
        )]);
        let llama_70b = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            64_000,
            4_000,
            0.85,
        )]);
        let general_peer = entry_with_manifest_and_availability(
            "llama", 1, 1.0, llama_70b,
        )
        .await;
        let coder_peer = entry_with_manifest_and_availability(
            "qwen-coder", 2, 1.0, qwen_coder,
        )
        .await;
        let request = v03_request(
            CapabilityHint::code(),
            LatencyClass::Normal,
            16_000,
            2_000,
        );
        let selector = CapabilityAwareSelector {
            fallback: Box::new(PrioritySelector),
        };
        let selected = selector
            .select(&request, &[general_peer, coder_peer])
            .await
            .unwrap();
        assert_eq!(
            selected, 1,
            "code request must route to the code specialist via claim-based scoring"
        );
    }

    #[tokio::test]
    async fn claim_based_context_gate_eliminates_undersized_peer() {
        let local_small = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Fast,
            8_000,
            1_000,
            0.9,
        )]);
        let peer_large = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            64_000,
            4_000,
            0.75,
        )]);
        let small_entry = entry_with_manifest_and_availability(
            "local", 1, 1.0, local_small,
        )
        .await;
        let large_entry = entry_with_manifest_and_availability(
            "peer", 2, 1.0, peer_large,
        )
        .await;
        let request = v03_request(
            CapabilityHint::general(),
            LatencyClass::Normal,
            16_000,
            2_000,
        );
        let selector = CapabilityAwareSelector {
            fallback: Box::new(PrioritySelector),
        };
        let selected = selector
            .select(&request, &[small_entry, large_entry])
            .await
            .unwrap();
        assert_eq!(
            selected, 1,
            "request's 16K context exceeds small claim's 8K gate — must route to large peer"
        );
    }

    #[tokio::test]
    async fn claim_path_wins_when_any_claim_present_even_if_capabilities_empty() {
        // This confirms the dispatch rule: the moment any model
        // publishes a claim, the v0.2 path is not consulted.
        let only_claims = manifest_with_claims(vec![CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            16_000,
            2_000,
            0.7,
        )]);
        let entry = entry_with_manifest_and_availability(
            "claims-only", 1, 1.0, only_claims,
        )
        .await;
        let request = v03_request(
            CapabilityHint::general(),
            LatencyClass::Normal,
            8_000,
            1_000,
        );
        let selector = CapabilityAwareSelector {
            fallback: Box::new(PrioritySelector),
        };
        let selected =
            selector.select(&request, &[entry]).await.unwrap();
        assert_eq!(selected, 0);
    }
}
