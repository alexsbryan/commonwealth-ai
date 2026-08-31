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

#[path = "main/brief_fixtures.rs"]
mod brief_fixtures;
#[path = "main/corpus_store_readiness.rs"]
mod corpus_store_readiness;
#[path = "main/delegate_browser_worker.rs"]
mod delegate_browser_worker;
#[path = "main/duckduckgo_real_e2e.rs"]
mod duckduckgo_real_e2e;
#[path = "main/e2e_code_intel.rs"]
mod e2e_code_intel;
#[path = "main/enrichment_health_e2e.rs"]
mod enrichment_health_e2e;
#[path = "main/knowledge_view_e2e.rs"]
mod knowledge_view_e2e;
#[path = "main/local_corpus_e2e.rs"]
mod local_corpus_e2e;
#[path = "main/mcp_surface_e2e.rs"]
mod mcp_surface_e2e;
#[path = "main/obsidian_live_sync_e2e.rs"]
mod obsidian_live_sync_e2e;
#[path = "main/playwright_actuator.rs"]
mod playwright_actuator;
#[path = "main/rag_tests.rs"]
mod rag_tests;
#[path = "main/recipe_author_tools.rs"]
mod recipe_author_tools;
#[path = "main/smoke_tests.rs"]
mod smoke_tests;
#[path = "main/tavily_real_e2e.rs"]
mod tavily_real_e2e;
#[path = "main/tool_tests.rs"]
mod tool_tests;
#[path = "main/watched_folder_e2e.rs"]
mod watched_folder_e2e;
