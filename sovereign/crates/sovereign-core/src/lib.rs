// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod atlas_context;
pub mod context;
pub mod conv_briefing;
pub mod conv_entity_graph;
pub mod conv_frame;
pub mod conv_tiered;
pub mod dossier;
pub mod embed_fn;
pub mod executor;
pub mod health_monitor;
pub mod insight;
pub mod memory;
pub mod memory_compaction;
pub mod mesh_measurements;
pub mod mobile_host;
pub mod model_family;
pub mod models_manifest;
pub mod quote_verification;
pub use oicp_types as oicp;
pub mod archive_classifier;
pub mod claim_class_classifier;
pub mod current_info_classifier;
pub mod effort_classifier;
pub mod lessons;
pub mod pipeline;
pub mod plan_grammar;
pub mod planner;
pub mod query_session;
pub mod role;
pub mod router;
pub mod router_axis;
pub mod router_bootstrap;
pub mod router_calibration;
pub mod router_drift;
pub mod router_embed;
pub mod router_embed_cache;
pub mod router_instruction;
pub mod runtime;
pub mod scope_classifier;
pub mod stubs;
pub mod time;
pub mod title;
pub mod tool_loop;

// The daemon↔package contract lives in `sovereign-contracts`; re-export every
// item at its historical `sovereign_core::{error, traits, registry, types,
// observer, health, skills, intent_policy, mcp_config, setup_config, rebrand,
// tool_result_cache}` path so every existing importer is unaffected.
pub use sovereign_contracts::{
    error, health, intent_policy, mcp_config, observer, rebrand, registry, setup_config, skills,
    slot_policy, tool_result_cache, traits, types,
};

// Re-export commonly used items at the crate root.
//
// `traits::*` / `types::*` are BOUNDED globs (quality program R1,
// 2026-07-11): they alias sovereign-contracts modules whose surfaces are
// explicit lists (traits.rs declares every item; types/mod.rs re-exports its
// submodules item-by-item), so this root widens only via a reviewable edit
// there — with the api-gate snapshot as the net. `model_family` is explicit
// outright.
pub use error::{Error, Result};
pub use model_family::{
    EmbedModelInfo, EmbedQuirks, ModelFamily, ModelQuirks, NormalizationStrategy, PoolingStrategy,
    RerankQuirks, ThinkingControl,
};
pub use registry::ToolRegistry;
pub use runtime::Runtime;
pub use skills::SkillRegistry;
pub use traits::*;
pub use types::*;
