// SPDX-License-Identifier: AGPL-3.0-or-later
//! The leaf-pure RAG tool: `ChunkTool` (paragraph chunking).
//!
//! `section` LEFT at sv-surface svt-7 (2026-09-12) for
//! `sovereign_tools::rag::section`, and it is the only thing that moved. It
//! reaches `corpus_engine_sections::{ChapterRegexDetector, SectionDetector}`
//! — a `regex` + `tracing` leaf, and reaching DOWN to it was never the
//! problem. THIS crate is: it is the one runtime-layer crate
//! `[thin_surfaces].may_reach` permits, so everything it links, every thin
//! client links, by reachability rather than by intent. That edge was the
//! last `from = "sovereign-desktop"` row in quality/ARCH_LAYERS.toml, held
//! open for a tool no client runs.
//!
//! `chunk` stays because it carries no such edge and three registries reach
//! it from here — including the corpus-engine-free studio bundle, which would
//! have lost a pure paragraph chunker for nothing.
//!
//! The document-parsing (`parse`) and ingest (`ingest`) helpers were always in
//! `sovereign-tools` — `parse` reaches into `local_corpus` PDF extraction, so
//! it is not leaf-pure and cannot live here.

pub mod chunk;
