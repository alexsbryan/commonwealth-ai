// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe registry catalog — a read-only view over the checked-in
//! `sovereign-recipes/registry.toml`.
//!
//! `RegistryBrowseTool` lists recipes so the authoring agent can pattern off an
//! existing one. It did this through `corpus_engine::RecipeRegistry` — a runtime
//! dependency the extractable authoring package cannot carry. The catalog is
//! static checked-in data, so the tool needs no injected seam: it parses the
//! bundled TOML embedded here and merges the user's locally-published registry.
//!
//! The raw TOML is embedded in this crate (the shared contract, stable relative
//! to the repo root) so a consumer references a typed const rather than counting
//! `../` across a crate boundary — the same convention as
//! [`crate::recipe::schema::RECIPE_SCHEMA_DESCRIPTOR_JSON`]. `corpus-engine`
//! keeps its own build.rs-vendored copy for the richer `RecipeRegistry`
//! (network refresh, TOML fetch); both derive from the one source file and
//! cannot drift.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;

/// The canonical corpus catalog, embedded as raw TOML.
///
/// Anchored at `CARGO_MANIFEST_DIR` (this crate lives at
/// `sovereign/crates/sovereign-contracts`, three `..` from the repo root; the
/// crate does not move relative to the root). The single repo-relative
/// reference to the artifact lives here, once.
pub const RECIPE_REGISTRY_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sovereign-recipes/registry.toml"
));

/// The subset of a registry entry the browse tool renders. Unlisted fields in
/// the TOML (`toml_url`, `sha256`, `prebuilt`, …) are ignored by serde.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryEntryView {
    /// Recipe id — the registry key, what `recipe:<id>` references.
    pub id: String,
    /// Human-readable corpus name.
    pub name: String,
    /// Short corpus description; empty when the TOML omits it.
    #[serde(default)]
    pub description: String,
    /// Upstream data license string; empty when unspecified.
    #[serde(default)]
    pub license: String,
    /// Download size, GB (0.0 when unspecified).
    #[serde(default)]
    pub size_compressed_gb: f64,
    /// On-disk indexed size, GB (0.0 when unspecified).
    #[serde(default)]
    pub size_indexed_gb: f64,
    /// Whether the recipe ships an enrichment phase.
    #[serde(default)]
    pub enrichment_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistrySnapshotView {
    #[serde(rename = "recipes", default)]
    entries: Vec<RegistryEntryView>,
}

/// Parse a registry snapshot TOML, returning its entries. Returns an empty
/// vec on a parse error — a malformed registry must not take down the caller
/// (mirrors `RecipeRegistry::with_local_registry`'s silent-ignore contract).
pub fn parse_registry(toml_str: &str) -> Vec<RegistryEntryView> {
    match toml::from_str::<RegistrySnapshotView>(toml_str) {
        Ok(snap) => snap.entries,
        Err(_) => Vec::new(),
    }
}

/// The user-published local registry path: `~/.sovereign/recipes/registry.toml`.
/// `None` when `HOME` is unresolvable. Mirrors
/// `RecipeRegistry::default_local_recipes_dir().join("registry.toml")`.
pub fn default_local_registry_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".sovereign").join("recipes").join("registry.toml"))
}

/// One row in the merged catalog: an entry plus whether it came from the user's
/// local registry (vs the bundled snapshot).
#[derive(Debug, Clone)]
pub struct CatalogRow {
    /// The registry entry.
    pub entry: RegistryEntryView,
    /// True when the entry came from the user's local registry rather than the bundled snapshot.
    pub is_local: bool,
}

/// The bundled catalog merged with the user's local registry, matching
/// `RecipeRegistry::list_entries()` precedence: local entries first (deduped by
/// id), then bundled entries whose id was not already seen. `is_local` mirrors
/// `RecipeRegistry::is_local_entry`. `live` (network refresh) is never consulted
/// — the browse tool never refreshes.
pub fn merged_catalog() -> Vec<CatalogRow> {
    let bundled = parse_registry(RECIPE_REGISTRY_TOML);
    let local = default_local_registry_path()
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse_registry(&t))
        .unwrap_or_default();
    let local_ids: BTreeSet<String> = local.iter().map(|e| e.id.clone()).collect();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<CatalogRow> = Vec::new();
    for e in local.into_iter().chain(bundled) {
        if !seen.insert(e.id.clone()) {
            continue;
        }
        let is_local = local_ids.contains(&e.id);
        out.push(CatalogRow { entry: e, is_local });
    }
    out
}
