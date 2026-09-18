// SPDX-License-Identifier: AGPL-3.0-or-later
//! Arithmetic over the language.
//!
//! Understanding's pure tier (DE "Step 7, redrawn", "The shape"): the
//! arithmetic over the published language — analysis, resolution by name,
//! classification, the ground walk, domains, ontology, reconciliation,
//! pipeline types and schemas, meta-atlas and traversal logic. It may name
//! the language (`understanding-vocab`), `kernel-types` and the row types the
//! index leaf (`corpus-index`) publishes; it carries `no async fn`.
//!
//! # The module tree (decided by `REVIEW-build-understanding-crate-tree`)
//!
//! It PRESERVES the source paths under `corpus-engine/src/`, so a moved pure
//! file's intra-tier `crate::enrichment::<m>` / `crate::meta_atlas::<m>` /
//! `crate::atlas_traversal::<m>` reach resolves with NO rewrite. The two MIXED
//! host shells are split by owner: this crate holds the pure submodules and
//! the language re-exports, the engine's shell keeps the host submodules
//! (`enrichment/atlas/mod.rs` keeps `atlas_teardown`, `enrichment/ontology/mod.rs`
//! keeps `validate` and the `recipe_ontology::language` re-exports). The batch
//! move rows below add each pure module declaration as its file arrives.
//!
//! # The four engine-leaf shims
//!
//! A moved pure file reaches four corpus-engine items that are not tier-local:
//! `crate::error` and `crate::types` (both re-export the `corpus-index` leaf
//! since `REVIEW-build-index-read-port`), the external `oplog` crate, and
//! `crate::atlas_canonical` (the vocabulary's `canonical`). Re-exported at the
//! historical names below so the move is mechanical.

// The read-port leaf, at the historical `crate::error` / `crate::types` paths.
pub use corpus_index::error;
pub use corpus_index::types;
// The journal crate, at the historical `crate::oplog` path.
pub use ::oplog;
// The canonical-name fold, at the historical `crate::atlas_canonical` path.
pub use understanding_vocab::canonical as atlas_canonical;
// The per-atom articulation half of `stream_axes` (DE "The read-port leaf,
// measured again": the per-atom types go to the language, the per-corpus
// stability half stays with Ingest). The pure files' `crate::stream_axes::
// Articulation*` reaches repoint here in the batch rows.
pub use understanding_vocab::articulation;

pub mod enrichment;

#[cfg(test)]
mod tests {
    /// The scaffolding is a claim: the four engine-leaf shims resolve and the
    /// language re-export surface compiles. Failing input: a shim renamed away
    /// or a `understanding-vocab` re-export that no longer exists.
    #[test]
    fn engine_leaf_shims_resolve() {
        fn _error_is_result() -> crate::error::Result<()> {
            Ok(())
        }
        let _fold: fn(&str) -> String = crate::atlas_canonical::lookup_key;
        let _embed_dim: usize = crate::types::DEFAULT_EMBED_DIM;
        let _atom: Option<crate::enrichment::atlas::AtomEnvelope> = None;
    }
}
