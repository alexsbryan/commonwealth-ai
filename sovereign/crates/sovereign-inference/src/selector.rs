use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::types::CompletionRequest;
use sovereign_core::Result;

use crate::health::HealthTracker;

pub struct BackendEntry {
    pub name: String,
    pub health: Arc<HealthTracker>,
    pub priority: u32,
    pub cost_per_token: Option<f64>,
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
        // Local backends have no cost_per_token (None = local).
        let local = backends
            .iter()
            .enumerate()
            .filter(|(_, b)| b.health.is_healthy() && b.cost_per_token.is_none())
            .min_by_key(|(_, b)| b.priority);

        if let Some((i, _)) = local {
            return Ok(i);
        }

        // Fall back to any healthy backend.
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
