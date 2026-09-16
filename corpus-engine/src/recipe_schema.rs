// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe variant-catalog descriptor — the generated bridge between the
//! recipe config enums and the authoring JSON Schema.
//!
//! corpus-engine owns the recipe types (`recipe.rs`), so `tests/recipe_schema.rs`
//! regenerates the checked-in artifact
//! `sovereign-recipes/schema/recipe_schema_descriptor.json` from those types
//! (drift-gated). `build.rs` vendors that artifact into `OUT_DIR`, and this
//! module embeds it from there — no cross-crate source-tree path.
//!
//! The recipe-authoring package cannot carry a `corpus-engine` dependency, so
//! it receives this descriptor INJECTED (the monolith holds both sides). The
//! shared leaf `sovereign-contracts` used to embed the artifact itself; that
//! embed climbed out of the leaf's crate root, which is why the package lift
//! could not resolve it in a flat-copy sandbox.
//!
//! (The descriptor itself replaced a `sovereign-tools/build.rs` that reached
//! *across* the crate boundary to parse `corpus-engine/src/recipe.rs` with
//! `syn` at build time — a source-tree path no package split survived.)

pub const RECIPE_SCHEMA_DESCRIPTOR_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/recipe_schema_descriptor.json"));
