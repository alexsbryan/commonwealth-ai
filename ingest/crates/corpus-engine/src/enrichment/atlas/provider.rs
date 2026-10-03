// SPDX-License-Identifier: AGPL-3.0-or-later
//! The walk provider, its declaration vocabulary and the class-composite
//! opener all live in the `corpus-engine-atlas-reader` leaf (FIVE_PROGRAMS §12
//! decision 1); the opener and the wiki-class provider it composes moved there
//! in pb-corpus-mcp-reads, so a program walks an atlas without this crate.
//! Re-exported here at the historical paths.

pub use std::sync::Arc;

pub use corpus_engine_atlas_reader::opener::{open_walk_provider, open_walk_provider_blocking};
pub use corpus_engine_atlas_reader::provider::{AtlasProvider, NavigationSource};
