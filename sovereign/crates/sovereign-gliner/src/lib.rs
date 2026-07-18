// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure-Rust GLiNER per-chunk entity extraction, extracted from
//! `sovereign-tools` (2026-07-17).
//!
//! GLiNER pulls the ONNX stack (`gline-rs` → `orp` → `ort` pinned to
//! `=2.0.0-rc.9`). Living behind an OPTIONAL `gliner-ner` feature on the
//! widely-shared `sovereign-tools` crate meant Cargo feature-unification
//! compiled that crate two different ways depending on who built it — the
//! split that repeatedly broke `-p` builds and the daemon/desktop/server/
//! cli-llm consumers. As its own crate, this surface is a MANDATORY dep of
//! exactly the four binaries that need it and absent everywhere else, so
//! there is no feature and no unification split.
//!
//! - [`gliner_ner`] — the `GlinerExtractor` model wrapper + model-management
//!   helpers (`models_root`, `probe_model_available`, `download_model`, …).
//! - [`GlinerChunkExtractor`] — the corpus-engine `ChunkEntityExtractor`
//!   impl for the daemon ingest path.
//! - [`load_gliner_extractor`] — the daemon/desktop bootstrap that wires
//!   both together over the canonical state store.

pub mod gliner_ner;

mod bootstrap;
mod chunk_extractor;

pub use bootstrap::load_gliner_extractor;
pub use chunk_extractor::GlinerChunkExtractor;
