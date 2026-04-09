//! Recipe Registry — catalog management and on-demand recipe fetching.
//!
//! The `RecipeRegistry` is the single source of truth for which corpora are
//! available. It ships a bundled snapshot (index-only, compiled via
//! `include_str!`) so the catalog works offline, and can refresh from the
//! live public registry on GitHub when the network is available.
//!
//! ## Resolution order for `fetch_recipe(id)`:
//!
//! 1. **Local override** — `<overrides_dir>/<id>.toml` on disk.
//!    Used during development and by the delta-update cache (recipe TOMLs
//!    are written here after first install so delta updates work offline).
//! 2. **Registry URL** — fetches the TOML from `toml_url` in the snapshot
//!    (live entry wins over bundled if a background refresh ran).
//! 3. Error if no entry found.
//!
//! SHA-256 verification is applied when `sha256` is non-empty. Empty string
//! skips verification (acceptable during bootstrap before the public repo exists).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::recipe::Recipe;
use crate::types::BuiltinCorpus;

// ── Bundled snapshot ─────────────────────────────────────────────────────────

const BUNDLED_SNAPSHOT: &str = include_str!("../registry_snapshot.toml");

// ── Registry snapshot schema ─────────────────────────────────────────────────

/// Pre-built block inside a registry entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryPrebuilt {
    pub hf_repo: String,
    pub hf_filename: String,
    pub sha256: String,
    pub compatible_embedding_model: String,
}

/// A single corpus entry in the registry index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub size_compressed_gb: f64,
    #[serde(default)]
    pub size_indexed_gb: f64,
    /// URL of the full recipe TOML (e.g. raw GitHub URL).
    pub toml_url: String,
    /// Hex-encoded SHA-256 of the TOML file. Empty = skip verification.
    #[serde(default)]
    pub sha256: String,
    /// Whether this corpus ships with an epistemic enrichment phase.
    /// Stored in the snapshot so the UI can show the enrichment badge
    /// without fetching the full recipe TOML.
    #[serde(default)]
    pub enrichment_enabled: bool,
    /// Whether this corpus can be shared in the mesh network.
    /// False for license-restricted corpora (e.g. SEP).
    #[serde(default = "default_true")]
    pub mesh_sharing: bool,
    /// Optional pre-built LanceDB index available for download.
    #[serde(default)]
    pub prebuilt: Option<RegistryPrebuilt>,
}

fn default_true() -> bool {
    true
}

/// The top-level structure of a `registry.toml` / `registry_snapshot.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub schema_version: u32,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub registry_url: String,
    #[serde(rename = "recipes")]
    pub entries: Vec<RegistryEntry>,
}

// ── RecipeRegistry ───────────────────────────────────────────────────────────

/// Manages the corpus catalog and on-demand recipe TOML fetching.
pub struct RecipeRegistry {
    /// Compiled-in snapshot — always available, possibly stale.
    snapshot: RegistrySnapshot,
    /// Live snapshot fetched from GitHub — replaces snapshot entries when present.
    live: Option<RegistrySnapshot>,
    /// Directory checked first for recipe TOMLs (local overrides and install cache).
    overrides_dir: Option<PathBuf>,
}

impl RecipeRegistry {
    /// Create a registry from the bundled snapshot.
    ///
    /// `overrides_dir` is checked before fetching from the network.
    /// Pass `Some(recipes_dir)` so local files in `corpus-engine/recipes/`
    /// work during development, and so cached recipes work for delta updates.
    pub fn from_bundled(overrides_dir: Option<PathBuf>) -> Self {
        let snapshot = toml::from_str(BUNDLED_SNAPSHOT)
            .expect("bundled registry_snapshot.toml failed to parse");
        Self { snapshot, live: None, overrides_dir }
    }

    /// Attempt a background refresh from the live registry URL.
    ///
    /// Silently ignores network errors — callers must not depend on this
    /// succeeding. After a successful refresh, `list_entries()` and
    /// `fetch_recipe()` prefer live entries.
    pub async fn refresh(&mut self) {
        let url = if self.snapshot.registry_url.is_empty() {
            return;
        } else {
            self.snapshot.registry_url.clone()
        };

        match fetch_text(&url).await {
            Ok(text) => {
                match toml::from_str::<RegistrySnapshot>(&text) {
                    Ok(live) => {
                        if live.schema_version > self.snapshot.schema_version {
                            tracing::warn!(
                                live_version = live.schema_version,
                                snapshot_version = self.snapshot.schema_version,
                                "Live registry schema is newer than bundled snapshot — \
                                 falling back to bundled. Run `cargo xtask update-registry-snapshot`."
                            );
                        } else {
                            tracing::debug!(
                                entries = live.entries.len(),
                                generated_at = %live.generated_at,
                                "Registry refreshed from live URL"
                            );
                            self.live = Some(live);
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Failed to parse live registry: {e}");
                    }
                }
            }
            Err(e) => {
                tracing::debug!("Failed to fetch live registry: {e}");
            }
        }
    }

    /// List all known registry entries.
    ///
    /// Live entries (from a background refresh) take precedence over
    /// bundled entries. Entries are deduplicated by `id` — live wins.
    pub fn list_entries(&self) -> Vec<&RegistryEntry> {
        if let Some(live) = &self.live {
            // Live overrides bundled for matching IDs.
            let mut out: Vec<&RegistryEntry> = live.entries.iter().collect();
            for entry in &self.snapshot.entries {
                if !out.iter().any(|e| e.id == entry.id) {
                    out.push(entry);
                }
            }
            out
        } else {
            self.snapshot.entries.iter().collect()
        }
    }

    /// Return the catalog as `BuiltinCorpus` structs (no network required).
    ///
    /// Used by `CorpusEngine::builtin_corpora()` and `list_corpora()`.
    pub fn catalog(&self) -> Vec<BuiltinCorpus> {
        self.list_entries()
            .into_iter()
            .map(|e| BuiltinCorpus {
                id: e.id.clone(),
                name: e.name.clone(),
                description: e.description.clone(),
                size_compressed_gb: e.size_compressed_gb,
                size_indexed_gb: e.size_indexed_gb,
                license: e.license.clone(),
                mesh_sharing: e.mesh_sharing,
            })
            .collect()
    }

    /// Return the registry entry for `id`, if known.
    pub fn find_entry(&self, id: &str) -> Option<&RegistryEntry> {
        // Prefer live over bundled.
        if let Some(live) = &self.live {
            if let Some(e) = live.entries.iter().find(|e| e.id == id) {
                return Some(e);
            }
        }
        self.snapshot.entries.iter().find(|e| e.id == id)
    }

    /// Fetch and parse the recipe TOML for `id`.
    ///
    /// Resolution order:
    /// 1. `<overrides_dir>/<id>.toml` — local file, no network.
    /// 2. `toml_url` from the registry entry — fetched via HTTP, SHA-256 verified.
    pub async fn fetch_recipe(&self, id: &str) -> Result<Recipe> {
        // 1. Local override.
        if let Some(dir) = &self.overrides_dir {
            let candidate = dir.join(format!("{id}.toml"));
            if candidate.is_file() {
                tracing::debug!(corpus = %id, path = %candidate.display(), "Loading recipe from local override");
                return Recipe::from_file(&candidate);
            }
            // Also check subdirectory layout: <overrides_dir>/<id>/recipe.toml
            let sub = dir.join(id).join("recipe.toml");
            if sub.is_file() {
                tracing::debug!(corpus = %id, path = %sub.display(), "Loading recipe from local override (subdir)");
                return Recipe::from_file(&sub);
            }
        }

        // 2. Fetch from registry URL.
        let entry = self
            .find_entry(id)
            .ok_or_else(|| Error::Recipe(format!("No registry entry for corpus '{id}'")))?;

        if entry.toml_url.is_empty() {
            return Err(Error::Recipe(format!(
                "Registry entry for '{id}' has no toml_url"
            )));
        }

        tracing::debug!(corpus = %id, url = %entry.toml_url, "Fetching recipe TOML from registry");
        let text = fetch_text(&entry.toml_url).await?;

        if !entry.sha256.is_empty() {
            verify_sha256(text.as_bytes(), &entry.sha256).map_err(|e| {
                Error::Recipe(format!("SHA-256 mismatch for corpus '{id}': {e}"))
            })?;
        }

        Recipe::from_toml(&text)
    }

    /// Cache a fetched recipe TOML to the overrides directory so future
    /// calls to `fetch_recipe()` and `load_recipe()` (delta updates) work
    /// without network access.
    ///
    /// Writes to `<overrides_dir>/<id>.toml`. Creates the directory if needed.
    /// Silently skips if `overrides_dir` is not set.
    pub async fn cache_recipe(&self, id: &str) -> Result<Option<PathBuf>> {
        let dir = match &self.overrides_dir {
            Some(d) => d,
            None => return Ok(None),
        };

        // Skip if already cached.
        let dest = dir.join(format!("{id}.toml"));
        if dest.is_file() {
            return Ok(Some(dest));
        }

        let entry = self
            .find_entry(id)
            .ok_or_else(|| Error::Recipe(format!("No registry entry for corpus '{id}'")))?;

        if entry.toml_url.is_empty() {
            return Ok(None);
        }

        let text = fetch_text(&entry.toml_url).await?;

        if !entry.sha256.is_empty() {
            verify_sha256(text.as_bytes(), &entry.sha256)
                .map_err(|e| Error::Recipe(format!("SHA-256 mismatch for '{id}': {e}")))?;
        }

        std::fs::create_dir_all(dir)?;
        std::fs::write(&dest, &text)?;
        tracing::debug!(corpus = %id, path = %dest.display(), "Cached recipe TOML to overrides dir");
        Ok(Some(dest))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn fetch_text(url: &str) -> Result<String> {
    let response = reqwest::get(url).await?;

    if !response.status().is_success() {
        return Err(Error::Recipe(format!(
            "HTTP {} fetching {url}",
            response.status()
        )));
    }

    Ok(response.text().await?)
}

fn verify_sha256(data: &[u8], expected_hex: &str) -> std::result::Result<(), String> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    let result: sha2::digest::Output<sha2::Sha256> = hasher.finalize();
    let actual = format!("{result:x}");
    if actual != expected_hex.to_lowercase() {
        return Err(format!("expected {expected_hex}, got {actual}"));
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_snapshot_parses() {
        let registry = RecipeRegistry::from_bundled(None);
        let entries = registry.list_entries();
        assert_eq!(entries.len(), 6, "snapshot should have 6 entries");
    }

    #[test]
    fn bundled_snapshot_has_required_fields() {
        let registry = RecipeRegistry::from_bundled(None);
        for entry in registry.list_entries() {
            assert!(!entry.id.is_empty(), "entry id must not be empty");
            assert!(!entry.name.is_empty(), "entry name must not be empty");
            assert!(!entry.toml_url.is_empty(), "entry toml_url must not be empty");
        }
    }

    #[test]
    fn sep_is_not_mesh_sharing() {
        let registry = RecipeRegistry::from_bundled(None);
        let sep = registry.find_entry("sep").expect("sep must be in snapshot");
        assert!(!sep.mesh_sharing, "SEP is license-restricted and must not be mesh-shared");
    }

    #[test]
    fn wikipedia_and_sep_have_enrichment_enabled() {
        let registry = RecipeRegistry::from_bundled(None);
        for id in &["wikipedia", "sep"] {
            let entry = registry.find_entry(id).unwrap_or_else(|| panic!("{id} must be in snapshot"));
            assert!(entry.enrichment_enabled, "{id} should have enrichment_enabled = true");
        }
    }

    #[test]
    fn catalog_returns_all_entries() {
        let registry = RecipeRegistry::from_bundled(None);
        let catalog = registry.catalog();
        assert_eq!(catalog.len(), 6);
        assert!(catalog.iter().any(|c| c.id == "wikipedia"));
        assert!(catalog.iter().any(|c| c.id == "sep"));
    }
}

// ── Path helper (used by engine and xtask) ───────────────────────────────────

/// Resolve the path to the bundled snapshot file within the corpus-engine crate.
/// Returns `None` outside a cargo workspace (e.g. when installed as a binary).
pub fn snapshot_path_in_workspace() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR is set at build time; use it at runtime via env! fallback.
    option_env!("CARGO_MANIFEST_DIR").map(|d| Path::new(d).join("registry_snapshot.toml"))
}
