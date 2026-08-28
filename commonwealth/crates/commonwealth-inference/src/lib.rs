// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod inference_plan;
// `model`, `model_aliases` and `oicp_registry` were forks of
// `commonwealth-core`'s modules of the same names: identical public surface,
// identical bodies, only the `crate::` imports rewritten — and
// `model_aliases` reached across the crate boundary with
// `include_str!("../../commonwealth-core/src/default_aliases.toml")` to load
// its data, a build-time edge Cargo does not model. Every production user
// (`commonwealth-api`, `commonwealth-test-harness`) went through THIS crate,
// so core's 13 tests covered a copy nobody ran and the copy everyone ran had
// one. Re-exported, not re-declared: one decider, one name
// (`ARCH_PRINCIPLES` §10.6). Surfaced by nc-22c shape matching, which sees a
// fork that was renamed or copied — the case a name-based census cannot.
pub use commonwealth_core::{model, model_aliases, oicp, oicp_registry};
pub mod plan;
pub mod store_adapter;
pub mod tier_router;
pub mod topology;

pub mod orchestrator;
pub mod scheduler;

pub use commonwealth_core::{Error, Result};

// Convenient re-exports for callers.
pub use inference_plan::{
    InferencePlan, LayerRange, ModelTransition, ShardAssignment, ShardPlan, TransitionState,
};
pub use model::{ModelArchitecture, ModelAvailability, ModelInfo};
pub use model_aliases::{AliasResolution, ModelAliasConfig, ModelAliasTable};
pub use oicp::{
    Capability, CapabilityClaim, CapabilityHint, CapabilityProfile, InferenceRequirements,
    KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse, LatencyClass, MatchQuality,
    OicpResponseMeta, ProviderManifest, ProviderModel, ShardingPrivacy, OICP_VERSION,
};
pub use plan::{
    LoadPolicy, MeshPlan, NodeRole, PlanTrigger, RequestRouter, RoutingCondition, RoutingRule,
    SchedulingStrategy, Tier, TierQueueDepths, UnavailableReason,
};
pub use store_adapter::InferenceStateStore;
pub use topology::TopologyEvent;
