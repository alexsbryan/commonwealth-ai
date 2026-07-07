// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure RAG tools: `ChunkTool` (paragraph chunking) and `SectionTool`
//! (section-aware chunking over `sovereign_contracts::recipe::sections`
//! detectors). The document-parsing (`parse`) and ingest (`ingest`) helpers stay
//! in `sovereign-tools` — `parse` reaches into `local_corpus` PDF extraction, so
//! it is not leaf-pure and cannot live here.

pub mod chunk;
pub mod section;
