// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe variant-catalog descriptor — the generated bridge between the
//! recipe config enums and the authoring JSON Schema.
//!
//! corpus-engine owns the recipe types (`recipe.rs`), so `tests/recipe_schema.rs`
//! regenerates the checked-in artifact
//! `sovereign-recipes/schema/recipe_schema_descriptor.json` from those types
//! (drift-gated). The raw artifact itself is embedded once in
//! [`sovereign_contracts::recipe::schema`] — the contract crate both this engine
//! and the recipe-authoring package depend on — and re-exported here so existing
//! `corpus_engine::recipe_schema::RECIPE_SCHEMA_DESCRIPTOR_JSON` callers are
//! unaffected. Housing the const in contracts is what lets the recipe-author
//! stack read it without a `corpus-engine` dependency.
//!
//! (This replaced a `sovereign-tools/build.rs` that reached *across* the crate
//! boundary to parse `corpus-engine/src/recipe.rs` with `syn` at build time — a
//! source-tree path no package split survived.)

pub use sovereign_contracts::recipe::schema::RECIPE_SCHEMA_DESCRIPTOR_JSON;
