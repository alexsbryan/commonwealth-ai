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
    /// User-published recipes from
    /// `~/.sovereign/recipes/registry.toml`. Loaded on demand by
    /// [`Self::with_local_registry`]; entries here win over both
    /// `live` and `snapshot` because the user explicitly chose them.
    /// Used to resolve recipes published via `sovereign recipe publish`.
    local: Option<RegistrySnapshot>,
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
        Self {
            snapshot,
            live: None,
            local: None,
            overrides_dir,
        }
    }

    /// Merge a user-published local registry (`registry.toml` in
    /// `~/.sovereign/recipes/`). Local entries win by `id` over
    /// both `live` and `snapshot`, so `sovereign recipe publish`
    /// can shadow an upstream recipe with a user's iteration of it
    /// without forcing them to push to GitHub first.
    ///
    /// Silently no-ops if `path` doesn't exist or fails to parse —
    /// a malformed local registry shouldn't take down the whole CLI.
    /// The ingest path will fall through to the bundled fallback as
    /// usual.
    pub fn with_local_registry(mut self, path: &Path) -> Self {
        if !path.is_file() {
            return self;
        }
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<RegistrySnapshot>(&text) {
                Ok(local) => {
                    tracing::debug!(
                        path = %path.display(),
                        entries = local.entries.len(),
                        "loaded local registry"
                    );
                    self.local = Some(local);
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        "Failed to parse local registry: {e} — ignoring"
                    );
                }
            },
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    "Failed to read local registry: {e} — ignoring"
                );
            }
        }
        self
    }

    /// Default location of the user-published local registry:
    /// `~/.sovereign/recipes/registry.toml`. Returns `None` when
    /// the home directory cannot be resolved (rare; CI containers
    /// should set HOME).
    pub fn default_local_recipes_dir() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".sovereign").join("recipes"))
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
    /// Precedence: `local` > `live` > `snapshot`. Entries are
    /// deduplicated by `id` — the higher-precedence layer wins.
    /// Local entries are surfaced so `sovereign recipe list` can
    /// show the user's published recipes alongside the upstream
    /// catalog.
    pub fn list_entries(&self) -> Vec<&RegistryEntry> {
        let mut out: Vec<&RegistryEntry> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = Default::default();

        if let Some(local) = &self.local {
            for e in &local.entries {
                if seen.insert(e.id.clone()) {
                    out.push(e);
                }
            }
        }
        if let Some(live) = &self.live {
            for e in &live.entries {
                if seen.insert(e.id.clone()) {
                    out.push(e);
                }
            }
        }
        for e in &self.snapshot.entries {
            if seen.insert(e.id.clone()) {
                out.push(e);
            }
        }
        out
    }

    /// Return true when `id` came from the local user registry
    /// (vs. live / bundled). Used by `recipe list` to render a
    /// "(local)" tag and by the audit-time publish nudge to
    /// distinguish user-authored recipes.
    pub fn is_local_entry(&self, id: &str) -> bool {
        self.local
            .as_ref()
            .map(|l| l.entries.iter().any(|e| e.id == id))
            .unwrap_or(false)
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
    ///
    /// Precedence: `local` > `live` > `snapshot`. Local entries
    /// shadow upstream entries with the same id.
    pub fn find_entry(&self, id: &str) -> Option<&RegistryEntry> {
        if let Some(local) = &self.local {
            if let Some(e) = local.entries.iter().find(|e| e.id == id) {
                return Some(e);
            }
        }
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
    /// 3. Compile-time bundled TOML via [`crate::recipe::bundled_recipe_toml`]
    ///    — last-resort fallback. Lets a corpus install without the
    ///    network when the recipe is part of the bundled snapshot but
    ///    the live URL is unreachable (e.g. recipe not yet pushed to
    ///    GitHub during development, or air-gapped use).
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

        // 2. Fetch from registry URL — falls through to the bundled
        // fallback on any non-success outcome (404, DNS error, sha256
        // mismatch). Logging the URL miss makes the fallback path
        // observable rather than silently swallowed.
        let entry = self
            .find_entry(id)
            .ok_or_else(|| Error::Recipe(format!("No registry entry for corpus '{id}'")))?;

        if !entry.toml_url.is_empty() {
            tracing::debug!(corpus = %id, url = %entry.toml_url, "Fetching recipe TOML from registry");
            match fetch_text(&entry.toml_url).await {
                Ok(text) => {
                    if !entry.sha256.is_empty() {
                        if let Err(e) = verify_sha256(text.as_bytes(), &entry.sha256) {
                            tracing::warn!(
                                corpus = %id,
                                "Recipe URL fetched but SHA-256 mismatch ({e}); trying bundled fallback"
                            );
                        } else {
                            return Recipe::from_toml(&text);
                        }
                    } else {
                        return Recipe::from_toml(&text);
                    }
                }
                Err(e) => {
                    tracing::info!(
                        corpus = %id,
                        url = %entry.toml_url,
                        "Recipe URL unreachable ({e}); trying bundled fallback"
                    );
                }
            }
        }

        // 3. Bundled compile-time fallback.
        if let Some(toml) = crate::recipe::bundled_recipe_toml(id) {
            tracing::debug!(corpus = %id, "Loading recipe from bundled compile-time TOML");
            return Recipe::from_toml(toml);
        }

        Err(Error::Recipe(format!(
            "No recipe available for corpus '{id}': local override absent, registry URL unreachable, and no bundled fallback"
        )))
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
        // 11 entries — original 9 + the wikipedia-catalog /
        // wikipedia-article pair (the catalog corpus + its
        // on-demand single-article content recipe, mirroring the
        // gutenberg/gutenberg-work shape but for Wikipedia's
        // chat-with-everything surface).
        assert_eq!(entries.len(), 11, "snapshot should have 11 entries");
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

    /// User-published recipes from a local registry shadow upstream
    /// entries by id and surface in `list_entries()` with
    /// `is_local_entry() == true`.
    #[test]
    fn local_registry_overrides_bundled_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let local_path = dir.path().join("registry.toml");
        let local_toml = r#"
schema_version = 1
generated_at = "2026-04-28T00:00:00Z"
registry_url = ""

[[recipes]]
id = "wikipedia"
name = "Wikipedia (Local Override)"
toml_url = "file://wikipedia/recipe.toml"
sha256 = ""

[[recipes]]
id = "sec-investigation"
name = "SEC Filings — AI Investigation"
toml_url = "file://sec-investigation/recipe.toml"
sha256 = ""
"#;
        std::fs::write(&local_path, local_toml).unwrap();

        let registry =
            RecipeRegistry::from_bundled(None).with_local_registry(&local_path);

        // Local override wins on id
        let wiki = registry.find_entry("wikipedia").expect("wikipedia present");
        assert_eq!(wiki.name, "Wikipedia (Local Override)");
        assert!(registry.is_local_entry("wikipedia"));

        // Brand-new local recipe surfaces too
        let sec = registry
            .find_entry("sec-investigation")
            .expect("sec-investigation present");
        assert_eq!(sec.name, "SEC Filings — AI Investigation");
        assert!(registry.is_local_entry("sec-investigation"));

        // Bundled-only recipe still listed
        let stack = registry.find_entry("stackexchange");
        assert!(stack.is_some());
        assert!(!registry.is_local_entry("stackexchange"));
    }

    #[test]
    fn missing_local_registry_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("does-not-exist.toml");
        let registry =
            RecipeRegistry::from_bundled(None).with_local_registry(&nonexistent);
        // Same number of entries as the bundled-only registry.
        assert!(registry.list_entries().len() >= 1);
        assert!(!registry.is_local_entry("wikipedia"));
    }

    #[test]
    fn sep_is_not_mesh_sharing() {
        let registry = RecipeRegistry::from_bundled(None);
        let sep = registry.find_entry("sep").expect("sep must be in snapshot");
        assert!(!sep.mesh_sharing, "SEP is license-restricted and must not be mesh-shared");
    }

    #[test]
    fn sep_has_enrichment_enabled() {
        // Wikipedia Core ships with enrichment off (the layered-stack
        // design — Layer 1 prioritises time-to-grounded over atlas
        // depth). Only SEP enables enrichment by default in v1.
        let registry = RecipeRegistry::from_bundled(None);
        let sep = registry.find_entry("sep").expect("sep must be in snapshot");
        assert!(sep.enrichment_enabled, "sep should have enrichment_enabled = true");

        let wp = registry.find_entry("wikipedia").expect("wikipedia must be in snapshot");
        assert!(
            !wp.enrichment_enabled,
            "Wikipedia Core ships with enrichment disabled (Layer 1 design)"
        );
    }

    #[test]
    fn catalog_returns_all_entries() {
        let registry = RecipeRegistry::from_bundled(None);
        let catalog = registry.catalog();
        // 9 → 11 with the addition of wikipedia-catalog +
        // wikipedia-article (the catalog corpus + its on-demand
        // content recipe — mirrors gutenberg/gutenberg-work for
        // Wikipedia's chat-with-everything surface).
        assert_eq!(catalog.len(), 11);
        assert!(catalog.iter().any(|c| c.id == "wikipedia"));
        assert!(catalog.iter().any(|c| c.id == "wikipedia-simple"));
        assert!(catalog.iter().any(|c| c.id == "wikipedia-catalog"));
        assert!(catalog.iter().any(|c| c.id == "wikipedia-article"));
        assert!(catalog.iter().any(|c| c.id == "sep"));
        assert!(catalog.iter().any(|c| c.id == "stackexchange"));
        assert!(catalog.iter().any(|c| c.id == "stackexchange-knowledge"));
        assert!(catalog.iter().any(|c| c.id == "gutenberg"));
        assert!(catalog.iter().any(|c| c.id == "gutenberg-work"));
    }

    /// Every snapshot entry must have a compile-time bundled TOML so
    /// `fetch_recipe` can fall back when the registry URL is unreachable
    /// (recipe not yet pushed to GitHub, air-gapped use, captive-portal
    /// network). A new catalog entry without a `bundled_recipe_toml`
    /// arm would silently regress to "live URL only" — this test pins
    /// that contract.
    #[test]
    fn bundled_recipe_covers_every_snapshot_entry() {
        let registry = RecipeRegistry::from_bundled(None);
        for entry in registry.list_entries() {
            assert!(
                crate::recipe::bundled_recipe_toml(&entry.id).is_some(),
                "snapshot entry '{}' has no compile-time bundled recipe TOML",
                entry.id,
            );
        }
    }

    /// `fetch_recipe` must produce a Recipe even when the URL is broken,
    /// so long as the id is in the bundled set. Asserted here against
    /// `wikipedia-simple` whose live URL is intentionally a 404 in the
    /// snapshot during initial rollout.
    #[tokio::test]
    async fn fetch_recipe_falls_back_to_bundled_on_url_failure() {
        // No overrides_dir → forces the URL → bundled fallback path.
        let registry = RecipeRegistry::from_bundled(None);
        let r = registry.fetch_recipe("wikipedia-simple").await.expect(
            "wikipedia-simple should resolve via bundled fallback even when URL 404s",
        );
        assert_eq!(r.corpus.id, "wikipedia-simple");
    }
}

// ── Path helper (used by engine and xtask) ───────────────────────────────────

/// Resolve the path to the bundled snapshot file within the corpus-engine crate.
/// Returns `None` outside a cargo workspace (e.g. when installed as a binary).
pub fn snapshot_path_in_workspace() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR is set at build time; use it at runtime via env! fallback.
    option_env!("CARGO_MANIFEST_DIR").map(|d| Path::new(d).join("registry_snapshot.toml"))
}
