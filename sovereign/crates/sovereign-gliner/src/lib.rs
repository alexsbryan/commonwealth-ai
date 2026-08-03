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
//! - [`gliner_ner`] — the v1 `GlinerExtractor` model wrapper + the
//!   model-management helpers shared by BOTH generations (`models_root`,
//!   `model_spec`, `probe_model_available`, `download_model`, …).
//! - [`gliner2`] — the GLiNER2 backend on bare `ort` (P2.1). Faster
//!   (2.52×) and ~4.8× lighter than v1, measured 2026-08-02; not yet the
//!   default on any ingest path.
//! - [`GlinerChunkExtractor`] — the corpus-engine `ChunkEntityExtractor`
//!   impl for the daemon ingest path.
//! - [`load_gliner_extractor`] — the daemon/desktop bootstrap that wires
//!   both together over the canonical state store.
//!
//! The two backends are separate types, not one type with a model knob:
//! the generations have different ONNX input contracts, and each
//! constructor refuses the other's model ids by generation rather than
//! failing deep inside `ort` with a shape error.

pub mod gliner2;
pub mod gliner_ner;

mod bootstrap;
mod chunk_extractor;

pub use bootstrap::load_gliner_extractor;
pub use chunk_extractor::GlinerChunkExtractor;
