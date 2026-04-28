use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use sovereign_core::oicp::{
    self, cold_start_weight, effective_affinity, load_penalty, locality_bonus,
    throughput_factor, throughput_factor_source, BenchmarkResult,
    InferenceRequirements, NodeLocality, NodeObservations, ProviderManifest,
    ShardingPrivacy,
};
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
    /// Rolling observations this process has recorded for the
    /// backend. Updated from the outside via
    /// `HybridProvider::record_*` helpers as requests complete;
    /// consumed by [`CapabilityAwareSelector`] during the
    /// observation-adjusted scoring pass.
    pub observations: Arc<RwLock<NodeObservations>>,
    /// Where this backend sits relative to us. Local slot → `Local`;
    /// LAN peer → `Near`; mesh peer over the public internet →
    /// `Far`. Set once at construction based on how the backend was
    /// registered; not currently re-evaluated at runtime.
    pub locality: NodeLocality,
    /// Baseline-model throughput benchmark for this backend. Populated
    /// for local backends after the daemon's startup probe; `None`
    /// for remote (OpenAI-compatible) backends and for backends whose
    /// benchmark hasn't completed yet. When `None`, the scheduler
    /// falls back to observation-driven throughput scoring (which is
    /// also `None` until [`THROUGHPUT_OBSERVATION_THRESHOLD`] samples
    /// accumulate); the scoring multiplier degrades to neutral 1.0
    /// without it.
    pub benchmark: Arc<RwLock<Option<BenchmarkResult>>>,
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
            observations: Arc::new(RwLock::new(NodeObservations::default())),
            locality: NodeLocality::Local,
            benchmark: Arc::new(RwLock::new(None)),
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
            observations: Arc::new(RwLock::new(NodeObservations::default())),
            // Remote backends default to `Far`; callers that know
            // the backend is on the same LAN can reassign to `Near`
            // after construction.
            locality: NodeLocality::Far,
            benchmark: Arc::new(RwLock::new(None)),
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

        // Score each backend that has an OICP manifest, folding in
        // observation-adjusted affinity + load penalty + locality
        // bonus + cold-start weight per v0.3 §7.
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

            let Some((claim_score, claim_affinity, model_size_gb)) =
                best_score_for_manifest(manifest, requirements)
            else {
                continue;
            };

            let obs = backend.observations.read().await.clone();
            let observation_mult = if claim_affinity > 0.0 {
                effective_affinity(claim_affinity, &obs) / claim_affinity
            } else {
                1.0
            };

            let load = load_penalty(&obs);
            let locality = locality_bonus(backend.locality);
            let cold_start = cold_start_weight(obs.samples);

            let benchmark_guard = backend.benchmark.read().await;
            let bench_ref = benchmark_guard.as_ref();
            let candidate_size = model_size_gb.unwrap_or(0.0);
            let throughput =
                throughput_factor(&obs, candidate_size, bench_ref);
            tracing::debug!(
                backend = %backend.name,
                idx,
                factor = throughput,
                source = throughput_factor_source(&obs, bench_ref),
                obs_samples = obs.samples,
                obs_tg_tok_s = obs.tg_tok_s_ewma,
                candidate_size_gb = candidate_size,
                "oicp_select: throughput_factor"
            );
            drop(benchmark_guard);

            let availability = backend.inference_availability.clamp(0.20, 1.0);

            let weighted = claim_score
                * observation_mult
                * load
                * locality
                * cold_start
                * throughput
                * availability;
            if weighted > best_score {
                best_score = weighted;
                best_idx = Some(idx);
            }
        }

        match best_idx {
            Some(idx) => Ok(idx),
            None => self.fallback.select(request, backends).await,
        }
    }
}

/// Best (claim_score, claim_affinity, model_size_gb) triple across
/// all available (model, claim) pairs in a manifest. `None` when no
/// pair can serve the request (hard gate failure or wrong
/// specialization).
///
/// `model_size_gb` is the winning model's advertised size, used by
/// [`throughput_factor`] to extrapolate from a baseline benchmark.
/// Some manifests (older peers, locally-built minimal manifests)
/// don't carry size; those return `None` and the throughput layer
/// falls through to observation-only scoring.
fn best_score_for_manifest(
    manifest: &ProviderManifest,
    requirements: &InferenceRequirements,
) -> Option<(f32, f32, Option<f32>)> {
    let mut best: Option<(f32, f32, Option<f32>)> = None;
    for model in manifest.models.iter().filter(|m| m.status.available) {
        for claim in &model.claims {
            let Some(score) =
                oicp::score_claim_for_request(claim, requirements)
            else {
                continue;
            };
            let claim_affinity = claim.effective_affinity();
            let candidate = (score, claim_affinity, model.size_gb);
            best = Some(match best {
                Some((best_score, _, _)) if best_score >= score => {
                    best.unwrap()
                }
                _ => candidate,
            });
        }
    }
    best
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::HealthTracker;
    use sovereign_core::oicp::{
        CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass,
        ModelStatus, ProviderManifest, ProviderModel,
    };
    use sovereign_core::types::CompletionRequest;

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
        let high_prio = BackendEntry::new_local("high", Arc::new(HealthTracker::new()), 1);
        let low_prio = BackendEntry::new_local("low", Arc::new(HealthTracker::new()), 2);
        let selector = CapabilityAwareSelector { fallback: Box::new(PrioritySelector) };
        let reqs = InferenceRequirements::new()
            .with_sharding(ShardingPrivacy::MeshAllowed)
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal);
        let request = CompletionRequest::new("no manifest").with_oicp(reqs);
        let selected = selector
            .select(&request, &[high_prio, low_prio])
            .await
            .unwrap();
        assert_eq!(selected, 0, "when no backend has a manifest, PrioritySelector must win");
    }

    // -----------------------------------------------------------
    // v0.3 §6 — claim-based selection in CapabilityAwareSelector
    // -----------------------------------------------------------

    fn manifest_with_claims(claims: Vec<CapabilityClaim>) -> ProviderManifest {
        ProviderManifest::new(vec![ProviderModel {
            id: "v03-model".into(),
            base_model: None,
            quantization: None,
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
