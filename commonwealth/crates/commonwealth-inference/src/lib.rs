pub mod inference_plan;
pub mod ledger;
pub mod ledger_store;
pub mod model;
pub mod model_aliases;
pub mod oicp;
pub mod oicp_registry;
pub mod store_adapter;

pub mod scheduler;
pub mod orchestrator;

pub use commonwealth_core::{Error, Result};

// Convenient re-exports for callers.
pub use inference_plan::{InferencePlan, LayerRange, ModelTransition, ShardAssignment, ShardPlan, TransitionState};
pub use ledger::{ContributionUnit, FairnessPolicy, LedgerEntry, LedgerEntryKind};
pub use model::{ModelArchitecture, ModelAvailability, ModelInfo};
pub use model_aliases::{AliasResolution, ModelAliasConfig, ModelAliasTable};
pub use oicp::{
    Capability, CapabilityProfile, CapabilityRequirements, InferenceRequirements,
    KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse, LatencyPreference,
    MatchQuality, OicpResponseMeta, ProviderManifest, ProviderModel, ShardingPrivacy, OICP_VERSION,
};
pub use store_adapter::InferenceStateStore;
