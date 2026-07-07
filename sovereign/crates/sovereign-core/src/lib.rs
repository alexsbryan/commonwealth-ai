// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod atlas_context;
pub mod context;
pub mod conv_briefing;
pub mod conv_entity_graph;
pub mod conv_tiered;
pub mod dossier;
pub mod executor;
pub mod health_monitor;
pub mod insight;
pub mod memory;
pub mod memory_compaction;
pub mod mobile_host;
pub mod model_family;
pub mod models_manifest;
pub mod quote_verification;
pub use oicp_types as oicp;
pub mod current_info_classifier;
pub mod effort_classifier;
pub mod gap;
pub mod pipeline;
pub mod planner;
pub mod query_session;
pub mod role;
pub mod router;
pub mod router_bootstrap;
pub mod router_embed;
pub mod router_embed_cache;
pub mod runtime;
pub mod scope_classifier;
pub mod stubs;
pub mod title;
pub mod tool_loop;

// The daemon↔package contract lives in `sovereign-contracts`; re-export every
// item at its historical `sovereign_core::{error, traits, registry, types,
// observer, health, skills, intent_policy, mcp_config, setup_config, rebrand,
// tool_result_cache}` path so every existing importer is unaffected.
pub use sovereign_contracts::{
    error, health, intent_policy, mcp_config, observer, rebrand, registry, setup_config, skills,
    tool_result_cache, traits, types,
};

// Re-export commonly used items at the crate root.
pub use error::{Error, Result};
pub use model_family::*;
pub use registry::ToolRegistry;
pub use runtime::Runtime;
pub use skills::SkillRegistry;
pub use traits::*;
pub use types::*;
