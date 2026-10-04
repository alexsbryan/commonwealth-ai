// SPDX-License-Identifier: AGPL-3.0-or-later
//! SEC typed-fact store. The store, its lookups, derivations and coverage are
//! the atlas-reader leaf's (`corpus_engine_atlas_reader::sec_facts`), so a read
//! path answers from it without the engine (pb-ingest-dial-tools); re-exported
//! here at the historical path. What stays is the recipe half of discovery:
//! which corpora's recipes DECLARE the store authoritative.

pub use corpus_engine_atlas_reader::sec_facts::*; // shim: moved by pb-ingest-dial-tools

mod discovery;

pub use discovery::{authoritative_store, discover_authoritative_stores, recipe_authority_tool};
