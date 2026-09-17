// SPDX-License-Identifier: AGPL-3.0-or-later
// Contract crate: the public surface IS the product — every pub item needs
// docs (count-ratcheted by lint-gate, never a hard deny).
#![warn(missing_docs)]
//! OICP — Open Inference Capabilities Protocol v0.4.0
//!
//! Canonical types per the specification at
//! `commonwealth/docs/oicp-v0.4.md` (v0.4 extends v0.3 additively;
//! `oicp-v0.3.md` remains the fallback path). Consumed by both the
//! Sovereign and Commonwealth workspaces via path dependency.
//!
//! v0.3 replaces the v0.2 capability-profile vocabulary with
//! specialization-aware routing: capability hints, latency classes,
//! per-model claims. The protocol is intentionally small at launch —
//! two standardized hints (`general`, `code`), three latency classes,
//! and an explicit extension track (`x:<tag>`) for everything else.
//!
//! v0.4 makes a host's constraint machinery and knowledge plane
//! discoverable enough that a client built only against "OICP manifest
//! + OpenAI-compatible HTTP" can run the workflow / recipe-authoring
//! stack against any conforming host: provider-level `features`
//! advertisement (§2), `EmbedModelInfo.query_instruction_prefix` (§4),
//! the ingest extension (§5), and model fingerprints (§6). Every v0.4
//! field is serde-defaulted; an empty v0.4 value serializes identically
//! to a v0.3 manifest.

pub mod capability;
pub mod completion;
pub mod error;
// Crate-private: `model_aliases` is its only caller and always was. It was
// `pub mod glob` in `commonwealth-core` and the move is what makes the
// privacy true rather than intended — a `git grep` before the move found zero
// consumers outside the module that travels with it.
mod glob;
pub mod inference_service;
pub mod ingest;
pub mod job;
pub mod jsonrpc;
pub mod knowledge;
pub mod manifest;
pub mod model_aliases;
pub mod openai_types;
pub mod origin;
/// Per-pipeline context-injection flags carried by a resolved pipeline alias —
/// moved down from `serving-policy` so the middleware seam can name it without
/// a `sovereign-contracts → serving-policy` edge (domains
/// `REVIEW-build-middleware-seam`).
pub mod pipeline_context;
pub mod registry;
pub mod requirements;
pub mod response;
pub mod responses_types;
pub mod scoring;
pub mod slot;
pub mod tenant;
pub mod tool;
/// Tool-call extraction from free-form model output — the lenient parser the
/// serving host and the embedded engine both need, pure `serde_json` + `std`.
/// Moved down from `sovereign-inference` (domains
/// `REVIEW-build-serving-drop-inference`) so the host reaches it without
/// linking the inference stack.
pub mod tool_calls;
pub mod version;

pub use completion::{
    latency_to_speed, speed_to_latency, CompletionRequest, CompletionResponse, Depth, FinishReason,
    PromptShape, ProviderCapabilities, SamplingMode, Speed, StreamFrame, StreamUsage, ToolSchema,
    TurnAdmission,
};
pub use error::{InferenceError, InferenceResult};
pub use job::{
    InvalidJobKind, Isolation, JobExecutorDescriptor, JobKind, JobRequirements, JobUnit,
    OfferedRepo, WorkOffer,
};
pub use jsonrpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION};
pub use origin::OriginKind;
pub use slot::{
    ComputeChildStatus, ResidentSlot, SlotDecodeEvidence, SlotPlacement, WorkerPlacement,
};
pub use tenant::{InvalidTenantId, TenantId};
pub use tool::{Effect, Idempotency, Latency, Scope, ToolDescriptor, ToolExample};

pub use capability::{
    infer_hint_from_profile, proficiency, Capability, CapabilityClaim, CapabilityHint,
    CapabilityProfile, InvalidCapabilityHint, LatencyClass, ProficiencyLevel,
};
pub use inference_service::{
    EditSlotStatus, FimCompletionRequest, FimStreamStart, LocalInferenceError,
};
pub use ingest::{
    CorpusIngestProgress, CorpusInstallRequest, CorpusInstallResponse, CorpusProgressResponse,
    IngestEndpoints, IngestPhase, RecipeStageReport, RecipeTestOptions, RecipeTestReport,
    RecipeTestRequest,
};
pub use knowledge::{
    KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse, LandscapeDigestEntry,
    LandscapeDigestRequest, LandscapeDigestResponse,
};
pub use manifest::{
    features, CorpusDescriptor, EmbedModelInfo, FederationManifest, KnowledgeManifest, ModelStatus,
    NormalizationStrategy, PeerDescriptor, PoolingStrategy, ProviderInfo, ProviderManifest,
    ProviderModel, ProviderType,
};
pub use model_aliases::{AliasResolution, ModelAlias, ModelAliasConfig, ModelAliasTable};
pub use pipeline_context::PipelineContextConfig;
pub use registry::{ExtensionRegistry, ExtensionStats};
pub use requirements::{InferenceRequirements, PrivacyRequirements, ShardingPrivacy};
pub use response::{MatchQuality, OicpResponseMeta};
pub use scoring::{
    apply_throughput_observation, best_claim_for_request, cold_start_weight, effective_affinity,
    hint_match_score, latency_match_score, load_penalty, locality_bonus, pick_better,
    score_claim_for_request, score_with_adjustments, throughput_factor, throughput_factor_source,
    BenchmarkResult, NodeLocality, NodeObservations, ScoreBreakdown, ScoredClaim,
    COLD_START_MIN_WEIGHT, COLD_START_SAMPLES, CONFIDENCE_SAMPLES, HINT_GENERAL_FALLBACK_SCORE,
    LATENCY_ADJACENT_SCORE, LATENCY_TWO_CLASS_SCORE, LOAD_COEFFICIENT, LOCALITY_FAR_BONUS,
    LOCALITY_LOCAL_BONUS, LOCALITY_NEAR_BONUS, SCORING_EPSILON, THROUGHPUT_EWMA_ALPHA,
    THROUGHPUT_FLOOR, THROUGHPUT_OBSERVATION_THRESHOLD, THROUGHPUT_REFERENCE_TG_TOK_S,
};
pub use version::OICP_VERSION;
