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
                if score > best_score {
                    best_score = score;
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

/// Score the best matching model in a backend's manifest.
/// Returns None if no model satisfies the requirements.
fn score_backend_manifest(
    manifest: &ProviderManifest,
    requirements: &InferenceRequirements,
) -> Option<f32> {
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
