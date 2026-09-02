// SPDX-License-Identifier: AGPL-3.0-or-later
//! One integration-test binary for this crate.
//!
//! Each former `tests/<name>.rs` is now `tests/main/<name>.rs`, declared
//! below, so cargo links ONE executable instead of one per file. Every
//! test still runs; its name gains the module path as a prefix, so a
//! filter that named a file now names a module:
//!
//!     cargo test -p <crate> --test main <module>::
//!
//! `#[path]` is load-bearing: `tests/main.rs` is a CRATE ROOT, so a bare
//! `mod foo;` resolves to `tests/foo.rs` — which cargo would then also
//! link as its own test binary, which is the thing this file exists to
//! stop. The attribute keeps the sources in `tests/main/`, a directory
//! cargo does not scan for targets.
//!
//! Files still sitting directly in `tests/` are there on purpose — they
//! need process isolation, or a `.config/nextest.toml` override keys on
//! their binary name. Do not fold those in.

#[path = "main/architecture_over_enron_e2e.rs"]
mod architecture_over_enron_e2e;
#[path = "main/atlas_code_structure.rs"]
mod atlas_code_structure;
#[path = "main/atlas_narrative_markdown.rs"]
mod atlas_narrative_markdown;
#[path = "main/atoms_schema_back_compat.rs"]
mod atoms_schema_back_compat;
#[path = "main/chatgpt_real_export.rs"]
mod chatgpt_real_export;
#[path = "main/custom_ontology_pipeline_e2e.rs"]
mod custom_ontology_pipeline_e2e;
#[path = "main/described_asset_e2e.rs"]
mod described_asset_e2e;
#[path = "main/enrichment_requested_flag.rs"]
mod enrichment_requested_flag;
#[path = "main/filter_pipeline.rs"]
mod filter_pipeline;
#[path = "main/http_api_pagination_e2e.rs"]
mod http_api_pagination_e2e;
#[path = "main/index_cache_residency.rs"]
mod index_cache_residency;
#[path = "main/ingest_failure_modes.rs"]
mod ingest_failure_modes;
#[path = "main/investigation_pipeline_e2e.rs"]
mod investigation_pipeline_e2e;
#[path = "main/newsworthy_integration.rs"]
mod newsworthy_integration;
#[path = "main/on_demand_guard.rs"]
mod on_demand_guard;
#[path = "main/ontology_prompt_snapshots.rs"]
mod ontology_prompt_snapshots;
#[path = "main/ontology_recipe.rs"]
mod ontology_recipe;
#[path = "main/parquet_ingest_e2e.rs"]
mod parquet_ingest_e2e;
#[path = "main/probe_index_residency.rs"]
mod probe_index_residency;
#[path = "main/query_sharing_cache_invalidation.rs"]
mod query_sharing_cache_invalidation;
#[path = "main/recipe_back_compat.rs"]
mod recipe_back_compat;
#[path = "main/recipe_domain_gate.rs"]
mod recipe_domain_gate;
#[path = "main/recipe_schema.rs"]
mod recipe_schema;
#[path = "main/recipe_templates.rs"]
mod recipe_templates;
#[path = "main/sharding_round_trip_e2e.rs"]
mod sharding_round_trip_e2e;
#[path = "main/snapshot_restore_e2e.rs"]
mod snapshot_restore_e2e;
#[path = "main/watcher_e2e.rs"]
mod watcher_e2e;
