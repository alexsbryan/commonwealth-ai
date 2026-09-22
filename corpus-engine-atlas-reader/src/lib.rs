// SPDX-License-Identifier: AGPL-3.0-or-later
//! The resolved-atlas READ surface, carved read-only out of
//! `corpus-engine/src/enrichment/atlas` (FIVE_PROGRAMS §12 decision 1).
//!
//! The atlas is WRITTEN by the ingest program's enrichment pipeline and READ
//! by svrn (grounding, `svrn code`), cmnwlth (mesh status) and the MCP
//! surfaces. This leaf is the readers' side: raw reads over the atlas
//! directory's files, the projection record, citations, the axis catalog and
//! the read-only section cache. It never writes the atlas — the write paths
//! (pipeline runs, store builds, ANN backfills, seed population) stay in
//! corpus-engine, which re-imports this leaf's modules at their historical
//! paths (ARCH §10.6 — a re-export, never a twin).
//!
//! `understanding-vocab` keeps the vocabulary door it already had (`atoms`,
//! `edges`, `ontology`, `taxonomy`); the store (lancedb/arrow/memmap) does not
//! widen it — it lands here when the store's read half moves.

pub mod ann_store;
pub mod axis_catalog;
pub mod citation;
pub mod context_filter;
pub mod evidence_site;
pub mod projection;
pub mod raw;
pub mod section_cache;
pub mod store;
pub mod summary;
