// SPDX-License-Identifier: AGPL-3.0-or-later
//! Meta-atlas substrate — cross-corpus stream-tagged index that
//! retrieval consumes to inject stream-grouped anchored chunks.
//!
//! The meta-atlas runs across every installed atlas and builds:
//!
//!   - **Per-atom articulation classification** (this module's
//!     [`classifier`] sub-module). Rule-based, deterministic,
//!     sub-second across 1.6M wiki atoms. Reads atom shape + chunk
//!     preview to emit an [`crate::stream_axes::ArticulationVector`].
//!     This is the substrate that handles heterogeneous user
//!     corpora (vaults, watched folders) without needing recipe-
//!     level declarations.
//!
//!   - **Per-corpus stability tag** (Stage 2, via
//!     [`crate::stream_axes::Stability`]).
//!
//!   - **Cross-corpus canonical-entity clustering** (Stage 3).
//!     Reuses the equivalence-class machinery from
//!     [`crate::atlas_canonical`] as the per-key grouping step.
//!
//! Today (Stage 1): only the classifier module is wired. Builder,
//! persistence, and retrieval-time consumption land in subsequent
//! stages.

pub mod builder;
pub mod classifier;
pub mod index;

/// Cross-corpus topic-to-topic ontological bridge (SEP ↔ Wikipedia) —
/// promotes the name-equality meta-atom into a typed concept-alignment
/// graph. See [`bridge`] module docs.
pub mod bridge;

pub use bridge::BridgeIndex;
pub use builder::{
    build_meta_atlas, default_meta_atlas_path, read_meta_atlas, rebuild_for_corpus,
    write_meta_atlas, Anchor, AtlasSeen, MetaAtlasFile, MetaAtom,
};
pub use classifier::{classify_articulation, classify_by_chunk_preview};
pub use index::MetaAtlasIndex;
