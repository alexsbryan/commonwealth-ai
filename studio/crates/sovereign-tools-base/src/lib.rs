// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-tools-base` — the pure, leaf-dependency workflow tools.
//!
//! The subset of `sovereign-tools` that a package (the workflow/recipe authoring
//! stack) needs and that depends only on the contract crate + ordinary
//! async/HTTP/serde leaves — no corpus-engine, no llama.cpp, no mesh. These are
//! the 11 tools `standard_registry` registers that are genuinely self-contained:
//!
//! - `shell` — run a subprocess
//! - `web` — fetch + extract page text (`WebFetchTool`) and the search agent
//!   (`WebSearchTool`, over the `search` prompt assets)
//! - `rag::chunk` / `rag::section` — paragraph / section-aware chunking
//! - `read_file` / `write_file` / `read_json` / `write_json` — filesystem tools
//! - `read_csv` — parse a CSV into rows
//! - `zip` — zip two arrays element-wise
//! - `vector_mean` — average a set of embedding vectors
//! - `mcp` — the Model Context Protocol client (stdio + HTTP transports,
//!   discovery, secret store) that connects external MCP servers as tools
//!
//! `sovereign-tools` re-exports every module here at its historical path
//! (`sovereign_tools::{shell, web, mcp, rag::chunk, ...}`), so all existing
//! callers are unaffected — this is a pure relocation. Tools that need
//! corpus-engine/heavy deps (extract, corpus_store/search, atlas_*) stay in
//! `sovereign-tools` and are injected into the registry via `extra_tools`.

pub mod mcp;
pub mod rag;
pub mod read_csv;
pub mod read_file;
pub mod read_json;
pub mod search;
pub mod shell;
pub mod vector_mean;
pub mod web;
pub mod write_file;
pub mod write_json;
pub mod zip;
