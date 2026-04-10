use commonwealth_core::ids::ModelId;
use crate::model::ModelInfo;
use crate::inference_plan::{ModelTransition, ShardPlan, TransitionState};

/// Default threshold: only swap if the improvement is > 0.3 (on 0-1 scale).
pub const SWAP_THRESHOLD: f32 = 0.3;

/// Manages the portfolio of simultaneously loaded models.
pub struct ModelPortfolio {
    /// Currently loaded models with their shard plans.
    pub loaded_models: Vec<(ModelId, ShardPlan)>,
    /// In-progress model transitions (swap old for new).
    pub transitions: Vec<ModelTransition>,
    /// Incremented on every portfolio change (used for cache invalidation).
    pub version: u64,
}

impl ModelPortfolio {
    pub fn new() -> Self {
        Self {
            loaded_models: Vec::new(),
            transitions: Vec::new(),
            version: 0,
        }
    }

    /// Check if the mesh has enough aggregate VRAM to load an additional model
    /// alongside the currently loaded ones.
    pub fn can_load_additional(&self, model: &ModelInfo, total_available_vram_gb: f32) -> bool {
        let current_usage: f32 = self
            .loaded_models
            .iter()
            .map(|(_, plan)| estimate_model_vram_gb(plan))
            .sum();
        let needed = model.size_bytes as f32 / 1_073_741_824.0;
        current_usage + needed <= total_available_vram_gb
    }

    /// Decide whether to swap an existing model for a better one.
    /// Only swap if the improvement justifies the 30-60 second load cost.
    pub fn should_swap(current_best_score: f32, potential_best_score: f32) -> bool {
        potential_best_score - current_best_score >= SWAP_THRESHOLD
    }

    /// Add a model to the portfolio.
    pub fn add_model(&mut self, model_id: ModelId, plan: ShardPlan) {
        self.loaded_models.push((model_id, plan));
        self.version += 1;
    }

    /// Remove a model from the portfolio.
    pub fn remove_model(&mut self, model_id: ModelId) -> Option<ShardPlan> {
        if let Some(pos) = self
            .loaded_models
            .iter()
            .position(|(id, _)| *id == model_id)
        {
            let (_, plan) = self.loaded_models.remove(pos);
            self.version += 1;
            Some(plan)
        } else {
            None
        }
    }

    /// Begin a graceful model transition (old model keeps serving while new loads).
    pub fn begin_transition(
        &mut self,
        outgoing: ShardPlan,
        incoming: ShardPlan,
    ) -> &ModelTransition {
        let transition = ModelTransition {
            outgoing,
            incoming,
            state: TransitionState::Loading,
        };
        self.transitions.push(transition);
        self.transitions.last().unwrap()
    }

    /// Advance a transition to the next state. Returns the new state.
    pub fn advance_transition(&mut self, index: usize) -> Option<TransitionState> {
        let transition = self.transitions.get_mut(index)?;
        transition.state = match transition.state {
            TransitionState::Loading => TransitionState::Ready,
            TransitionState::Ready => TransitionState::Complete,
            TransitionState::Complete => TransitionState::Complete,
        };
        Some(transition.state)
    }

    /// Apply a completed transition: remove old model, add new one.
    /// Returns true if the transition was applied.
    pub fn apply_completed_transitions(&mut self) -> usize {
        let mut applied = 0;
        let completed: Vec<ModelTransition> = self
            .transitions
            .drain(..)
            .filter(|t| t.state == TransitionState::Complete)
            .collect();

        for transition in completed {
            self.remove_model(transition.outgoing.model);
            self.add_model(transition.incoming.model, transition.incoming);
            applied += 1;
        }
        applied
    }

    /// Get the shard plan for a specific model.
    pub fn get_plan(&self, model_id: ModelId) -> Option<&ShardPlan> {
        self.loaded_models
            .iter()
            .find(|(id, _)| *id == model_id)
            .map(|(_, plan)| plan)
    }

    /// Number of loaded models.
    pub fn model_count(&self) -> usize {
        self.loaded_models.len()
    }

    /// Number of in-progress transitions.
    pub fn transition_count(&self) -> usize {
        self.transitions.len()
    }

    /// Check if a specific model is currently loaded.
    pub fn is_loaded(&self, model_id: ModelId) -> bool {
        self.loaded_models.iter().any(|(id, _)| *id == model_id)
    }
}

impl Default for ModelPortfolio {
    fn default() -> Self {
        Self::new()
    }
}

/// Rough estimate of VRAM consumed by a loaded model based on its shard plan.
fn estimate_model_vram_gb(plan: &ShardPlan) -> f32 {
    // Rough heuristic: total layers * ~0.25 GB per layer for Q4_K_M.
    let total_layers: u32 = plan.assignments.iter().map(|a| a.layers.count()).sum();
    total_layers as f32 * 0.25
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::{ModelId, NodeId};
    use crate::model::ModelArchitecture;
    use crate::oicp::CapabilityProfile;
    use crate::inference_plan::{LayerRange, ShardAssignment};
    use std::collections::HashMap;

    fn test_model(id: u128, size_gb: u64) -> ModelInfo {
        ModelInfo {
            id: ModelId::from_u128(id),
            name: format!("model-{id}"),
            repo: "test/model".into(),
            file: "model.gguf".into(),
            size_bytes: size_gb * 1_073_741_824,
            total_layers: 64,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: CapabilityProfile::default(),
            quantization: "Q4_K_M".into(),
        }
    }

    fn test_plan(model_id: u128) -> ShardPlan {
        ShardPlan {
            model: ModelId::from_u128(model_id),
            entry_node: NodeId::from_u128(1),
            assignments: vec![ShardAssignment {
                node_id: NodeId::from_u128(1),
                layers: LayerRange::new(0, 64),
                gpu_index: 0,
                rpc_address: "127.0.0.1:50051".parse().unwrap(),
            }],
            estimated_tokens_per_sec: 40.0,
            estimated_ttft_ms: 1000,
        }
    }

    #[test]
    fn empty_portfolio() {
        let portfolio = ModelPortfolio::new();
        assert_eq!(portfolio.model_count(), 0);
        assert_eq!(portfolio.version, 0);
    }

    #[test]
    fn add_and_remove_model() {
        let mut portfolio = ModelPortfolio::new();
        portfolio.add_model(ModelId::from_u128(1), test_plan(1));
        assert_eq!(portfolio.model_count(), 1);
        assert_eq!(portfolio.version, 1);
        assert!(portfolio.is_loaded(ModelId::from_u128(1)));

        portfolio.remove_model(ModelId::from_u128(1));
        assert_eq!(portfolio.model_count(), 0);
        assert_eq!(portfolio.version, 2);
    }

    #[test]
    fn can_load_additional_with_headroom() {
        let mut portfolio = ModelPortfolio::new();
        portfolio.add_model(ModelId::from_u128(1), test_plan(1));
        // Current model uses ~64 * 0.25 = 16 GB. New model is 17 GB. Total 33.
        let model = test_model(2, 17);
        assert!(portfolio.can_load_additional(&model, 50.0)); // 50 GB available
        assert!(!portfolio.can_load_additional(&model, 30.0)); // 30 GB not enough
    }

    #[test]
    fn should_swap_threshold() {
        assert!(!ModelPortfolio::should_swap(0.7, 0.8)); // +0.1 < 0.3
        assert!(!ModelPortfolio::should_swap(0.7, 0.9)); // +0.2 < 0.3
        assert!(ModelPortfolio::should_swap(0.5, 0.9)); // +0.4 >= 0.3
        assert!(ModelPortfolio::should_swap(0.0, 1.0)); // +1.0 >= 0.3
    }

    #[test]
    fn transition_state_machine() {
        let mut portfolio = ModelPortfolio::new();
        portfolio.add_model(ModelId::from_u128(1), test_plan(1));

        portfolio.begin_transition(test_plan(1), test_plan(2));
        assert_eq!(portfolio.transition_count(), 1);

        let state = portfolio.advance_transition(0).unwrap();
        assert_eq!(state, TransitionState::Ready);

        let state = portfolio.advance_transition(0).unwrap();
        assert_eq!(state, TransitionState::Complete);
    }

    #[test]
    fn apply_completed_transitions() {
        let mut portfolio = ModelPortfolio::new();
        portfolio.add_model(ModelId::from_u128(1), test_plan(1));

        portfolio.begin_transition(test_plan(1), test_plan(2));
        portfolio.advance_transition(0); // Ready
        portfolio.advance_transition(0); // Complete

        let applied = portfolio.apply_completed_transitions();
        assert_eq!(applied, 1);
        assert!(!portfolio.is_loaded(ModelId::from_u128(1)));
        assert!(portfolio.is_loaded(ModelId::from_u128(2)));
        assert_eq!(portfolio.transition_count(), 0);
    }

    #[test]
    fn version_increments_on_changes() {
        let mut portfolio = ModelPortfolio::new();
        assert_eq!(portfolio.version, 0);

        portfolio.add_model(ModelId::from_u128(1), test_plan(1));
        assert_eq!(portfolio.version, 1);

        portfolio.add_model(ModelId::from_u128(2), test_plan(2));
        assert_eq!(portfolio.version, 2);

        portfolio.remove_model(ModelId::from_u128(1));
        assert_eq!(portfolio.version, 3);
    }

    #[test]
    fn get_plan_returns_correct_model() {
        let mut portfolio = ModelPortfolio::new();
        portfolio.add_model(ModelId::from_u128(1), test_plan(1));
        portfolio.add_model(ModelId::from_u128(2), test_plan(2));

        let plan = portfolio.get_plan(ModelId::from_u128(1)).unwrap();
        assert_eq!(plan.model, ModelId::from_u128(1));

        assert!(portfolio.get_plan(ModelId::from_u128(99)).is_none());
    }
}
