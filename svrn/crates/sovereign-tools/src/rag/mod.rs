// SPDX-License-Identifier: AGPL-3.0-or-later
//! The RAG workflow tools: `chunk` (paragraph chunking), `section`
//! (section-aware chunking), `parse` (document parsing) and `ingest`.
//!
//! `chunk` lives in the leaf-pure `sovereign-tools-base` and is re-exported
//! here, so `sovereign_tools::rag::chunk` is unchanged for every caller.
//!
//! `section` CAME BACK from there at sv-surface svt-7 (2026-09-12) and is
//! defined here again. Not because it stopped being leaf-pure — it reaches
//! `corpus_engine_sections::{ChapterRegexDetector, SectionDetector}`, a
//! `regex` + `tracing` leaf, in the correct direction. Because
//! `sovereign-tools-base` is the ONE runtime-layer crate
//! `[thin_surfaces].may_reach` lets a thin client link, so its dependencies
//! are a thin client's dependencies by reachability: this edge was the last
//! `from = "sovereign-desktop"` exception row in quality/ARCH_LAYERS.toml,
//! standing for a workflow tool no client calls.
//!
//! What that costs, and where it is paid:
//! `sovereign_workflow_host::standard_registry` builds from tools-base alone
//! (it ships with the authoring package and must not link corpus-engine), so
//! it registers `chunk` and no longer registers `section`. Every host that
//! links this crate injects it back through `workflow_corpus_tools()` — the
//! seam that already exists for exactly this, and how the corpus/atlas tools
//! have been restored since B:P9d.

pub use sovereign_tools_base::rag::chunk;

pub mod ingest;
pub mod parse;
pub mod section;
