// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod insight_store;
pub mod memory;
pub mod migrations;
#[cfg(feature = "postgres")]
pub mod postgres;
/// The recipe-project store moved into the extractable `sovereign-recipe-author`
/// package (self-contained rusqlite, no store-crate coupling); re-exported here
/// at the old path so existing importers (`sovereign_store::recipe_project_store`)
/// keep working unchanged.
pub use sovereign_recipe_author::recipe_project_store;
pub mod sqlite;
pub mod state_store_checker;

pub use sovereign_core;
