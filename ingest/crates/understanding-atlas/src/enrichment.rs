// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure enrichment surface, split out of `corpus-engine`'s host shells.
//!
//! `REVIEW-build-understanding-crate-tree` decided the module tree: it
//! PRESERVES the source paths, so an intra-tier `crate::enrichment::<m>` reach
//! resolves unchanged once its file moves here. The two MIXED host shells
//! (`corpus-engine`'s `enrichment/atlas/mod.rs` and `enrichment/ontology/mod.rs`)
//! are split by what each declaration owns: the language re-exports and the
//! pure submodules live here, the host submodules and `atlas_teardown` /
//! `validate` stay in the engine. This module holds the pure half as the batch
//! moves below fill it.

pub mod atlas;
pub mod ontology;
