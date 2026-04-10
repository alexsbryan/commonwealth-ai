use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use commonwealth_core::ids::ModelId;
use crate::model::ModelInfo;
use crate::oicp::{self, CapabilityRequirements};

/// Cached resolution from OICP requirements to the best matching model.
/// Recomputed only when the model portfolio changes.
pub struct OicpModelCache {
    resolution_cache: HashMap<u64, (ModelId, f32)>,
    portfolio_version: u64,
}

impl OicpModelCache {
    /// Build a new cache. The portfolio_version is used for staleness checks.
    pub fn new(portfolio_version: u64) -> Self {
        Self {
            resolution_cache: HashMap::new(),
            portfolio_version,
        }
    }

    /// Resolve OICP requirements to a model from cache.
    pub fn resolve(&self, requirements: &CapabilityRequirements) -> Option<(ModelId, f32)> {
        let key = hash_requirements(requirements);
        self.resolution_cache.get(&key).copied()
    }

    /// Resolve from cache, falling back to computation on cache miss.
    /// Inserts the result into the cache on miss.
    pub fn resolve_or_compute(
        &mut self,
        requirements: &CapabilityRequirements,
        models: &[&ModelInfo],
    ) -> Option<(ModelId, f32)> {
        let key = hash_requirements(requirements);

        if let Some(&result) = self.resolution_cache.get(&key) {
            return Some(result);
        }

        let result = compute_best_model(requirements, models)?;
        self.resolution_cache.insert(key, result);
        Some(result)
    }

    /// Clear the cache (call when portfolio changes).
    pub fn invalidate(&mut self) {
        self.resolution_cache.clear();
    }

    /// Check if the cache is stale relative to the current portfolio version.
    pub fn is_stale(&self, current_version: u64) -> bool {
        self.portfolio_version != current_version
    }

    /// Update the portfolio version (after rebuilding).
    pub fn set_version(&mut self, version: u64) {
        self.portfolio_version = version;
    }

    /// Number of cached resolutions.
    pub fn cache_size(&self) -> usize {
        self.resolution_cache.len()
    }
}

/// Deterministic hash of capability requirements for use as cache key.
fn hash_requirements(requirements: &CapabilityRequirements) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Hash required capabilities in sorted order for determinism.
    let mut required: Vec<_> = requirements.required.iter().collect();
    required.sort_by_key(|(cap, _)| format!("{cap:?}"));
    for (cap, level) in &required {
        format!("{cap:?}").hash(&mut hasher);
        level.hash(&mut hasher);
    }

    // Hash preferred capabilities in sorted order.
    let mut preferred: Vec<_> = requirements.preferred.iter().collect();
    preferred.sort_by_key(|(cap, _)| format!("{cap:?}"));
    for (cap, level) in &preferred {
        format!("{cap:?}").hash(&mut hasher);
        level.hash(&mut hasher);
    }

    hasher.finish()
}

/// Compute the best model for the given requirements by iterating all models.
fn compute_best_model(
    requirements: &CapabilityRequirements,
    models: &[&ModelInfo],
) -> Option<(ModelId, f32)> {
    let mut best: Option<(ModelId, f32)> = None;

    for model in models {
        if !oicp::satisfies_required(&model.oicp_capabilities, &requirements.required) {
            continue;
        }
        let score =
            oicp::score_preferred(&model.oicp_capabilities, &requirements.preferred);
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((model.id, score));
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelArchitecture;
    use crate::oicp::{Capability, CapabilityProfile};

    fn make_model(id: u128, caps: &[(Capability, u8)]) -> ModelInfo {
        let mut profile = CapabilityProfile::new();
        for &(cap, level) in caps {
            profile.insert(cap, level);
        }
        ModelInfo {
            id: ModelId::from_u128(id),
            name: format!("model-{id}"),
            repo: "test/model".into(),
            file: "model.gguf".into(),
            size_bytes: 17_000_000_000,
            total_layers: 64,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: profile,
            quantization: "Q4_K_M".into(),
        }
    }

    fn make_requirements(
        required: &[(Capability, u8)],
        preferred: &[(Capability, u8)],
    ) -> CapabilityRequirements {
        let mut req_profile = CapabilityProfile::new();
        for &(cap, level) in required {
            req_profile.insert(cap, level);
        }
        let mut pref_profile = CapabilityProfile::new();
        for &(cap, level) in preferred {
            pref_profile.insert(cap, level);
        }
        CapabilityRequirements {
            required: req_profile,
            preferred: pref_profile,
        }
    }

    #[test]
    fn resolve_finds_cached_result() {
        let coder = make_model(1, &[(Capability::Code, 4), (Capability::General, 2)]);
        let general = make_model(2, &[(Capability::General, 3), (Capability::Analysis, 3)]);
        let models: Vec<&ModelInfo> = vec![&coder, &general];

        let mut cache = OicpModelCache::new(1);

        let reqs = make_requirements(&[(Capability::Code, 2)], &[(Capability::Code, 4)]);
        let result = cache.resolve_or_compute(&reqs, &models);

        assert!(result.is_some());
        let (model_id, _score) = result.unwrap();
        assert_eq!(model_id, ModelId::from_u128(1)); // Coder model wins.

        // Resolve again — should hit cache.
        let cached = cache.resolve(&reqs);
        assert_eq!(cached.unwrap().0, ModelId::from_u128(1));
    }

    #[test]
    fn resolve_returns_none_when_no_model_satisfies() {
        let general = make_model(1, &[(Capability::General, 3)]);
        let models: Vec<&ModelInfo> = vec![&general];

        let mut cache = OicpModelCache::new(1);
        let reqs = make_requirements(&[(Capability::Code, 4)], &[]);
        let result = cache.resolve_or_compute(&reqs, &models);

        assert!(result.is_none());
    }

    #[test]
    fn cache_hit_consistent_with_computation() {
        let coder = make_model(1, &[(Capability::Code, 4), (Capability::Instruction, 3)]);
        let general = make_model(
            2,
            &[
                (Capability::General, 3),
                (Capability::Analysis, 3),
                (Capability::Code, 2),
            ],
        );
        let models: Vec<&ModelInfo> = vec![&coder, &general];

        let reqs = make_requirements(&[(Capability::Code, 2)], &[(Capability::Code, 3)]);

        // Direct computation.
        let direct = compute_best_model(&reqs, &models);

        // Via cache.
        let mut cache = OicpModelCache::new(1);
        let cached = cache.resolve_or_compute(&reqs, &models);

        assert_eq!(direct, cached);
    }

    #[test]
    fn invalidate_clears_cache() {
        let model = make_model(1, &[(Capability::General, 3)]);
        let models: Vec<&ModelInfo> = vec![&model];

        let mut cache = OicpModelCache::new(1);
        let reqs = make_requirements(&[], &[(Capability::General, 3)]);
        cache.resolve_or_compute(&reqs, &models);
        assert_eq!(cache.cache_size(), 1);

        cache.invalidate();
        assert_eq!(cache.cache_size(), 0);
        assert!(cache.resolve(&reqs).is_none());
    }

    #[test]
    fn is_stale_detects_version_mismatch() {
        let cache = OicpModelCache::new(1);
        assert!(!cache.is_stale(1));
        assert!(cache.is_stale(2));
    }

    #[test]
    fn resolve_or_compute_falls_back_on_miss() {
        let model = make_model(1, &[(Capability::General, 3)]);
        let models: Vec<&ModelInfo> = vec![&model];

        let mut cache = OicpModelCache::new(1);
        let reqs = make_requirements(&[], &[(Capability::General, 3)]);

        // Cache is empty — should compute.
        assert!(cache.resolve(&reqs).is_none());
        let result = cache.resolve_or_compute(&reqs, &models);
        assert!(result.is_some());

        // Now cache has it.
        assert!(cache.resolve(&reqs).is_some());
        assert_eq!(cache.cache_size(), 1);
    }

    #[test]
    fn deterministic_hashing() {
        let reqs = make_requirements(
            &[(Capability::Code, 2), (Capability::General, 1)],
            &[(Capability::Code, 4)],
        );
        let h1 = hash_requirements(&reqs);
        let h2 = hash_requirements(&reqs);
        assert_eq!(h1, h2);
    }
}
