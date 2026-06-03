pub mod inference_plan;
pub mod model;
pub mod model_aliases;
pub use commonwealth_core::oicp;
pub mod oicp_registry;
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
