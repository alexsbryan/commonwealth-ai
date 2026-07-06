// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe variant-catalog descriptor — the generated bridge between the
//! recipe config enums and the authoring JSON Schema.
//!
//! corpus-engine owns the recipe types (`recipe.rs`), so it owns their catalog.
//! `tests/recipe_schema.rs` regenerates the checked-in artifact
//! `sovereign-recipes/schema/recipe_schema_descriptor.json` from those types
//! (drift-gated). This module embeds that artifact and exposes it as a typed
//! const, so a consumer references
//! `corpus_engine::recipe_schema::RECIPE_SCHEMA_DESCRIPTOR_JSON` instead of
//! counting `../` from its own source file to a repo-root path. Moving the
//! consuming crate can't break the reference — cargo resolves the dependency,
//! and the one unavoidable repo-relative hop lives here, once.
//!
//! (This replaced a `sovereign-tools/build.rs` that reached *across* the crate
//! boundary to parse `corpus-engine/src/recipe.rs` with `syn` at build time — a
//! source-tree path no package split survived.)

/// The checked-in recipe variant-catalog descriptor, as raw JSON. Shape:
/// `{ "acquire": [{key, required}], "extract": [{key, required}],
///    "chunk": [key, …], "filter": [key, …], "pattern": [key, …],
///    "comparison": [key, …] }`.
///
/// Anchored at `CARGO_MANIFEST_DIR` — this crate sits directly under the repo
/// root, so the single repo-relative hop is one `..` and is stable (the engine
/// crate does not move).
pub const RECIPE_SCHEMA_DESCRIPTOR_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../sovereign-recipes/schema/recipe_schema_descriptor.json"
));
