// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe variant-catalog descriptor — the generated bridge between the
//! recipe config enums and the authoring JSON Schema.
//!
//! `corpus-engine` owns the recipe *types* (`recipe.rs`) and regenerates the
//! checked-in artifact `sovereign-recipes/schema/recipe_schema_descriptor.json`
//! from them (drift-gated in `corpus-engine/tests/recipe_schema.rs`). The raw
//! artifact is embedded *here* — in the contract crate both the engine and the
//! recipe-authoring package depend on — so a consumer references a typed const
//! rather than counting `../` across a crate boundary. `corpus_engine`
//! re-exports it at `corpus_engine::recipe_schema::RECIPE_SCHEMA_DESCRIPTOR_JSON`
//! for its existing callers; the recipe-author tools read it from here, which is
//! what lets that stack drop its `corpus-engine` dependency.

/// The checked-in recipe variant-catalog descriptor, as raw JSON. Shape:
/// `{ "acquire": [{key, required}], "extract": [{key, required}],
///    "chunk": [key, …], "filter": [key, …], "pattern": [key, …],
///    "comparison": [key, …] }`.
///
/// Anchored at `CARGO_MANIFEST_DIR`: this crate lives at
/// `sovereign/crates/sovereign-contracts`, so the repo-relative hop is three
/// `..` segments and is stable (the crate does not move relative to the repo
/// root). The single repo-relative reference to the artifact lives here, once.
pub const RECIPE_SCHEMA_DESCRIPTOR_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sovereign-recipes/schema/recipe_schema_descriptor.json"
));
