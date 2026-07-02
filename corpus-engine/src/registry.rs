// SPDX-License-Identifier: AGPL-3.0-or-later
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

// The bundled catalog is `sovereign-recipes/registry.toml`, vendored by
// build.rs into OUT_DIR (single source of truth — no checked-in snapshot
// copy in this crate). See `corpus-engine/build.rs`.
const BUNDLED_SNAPSHOT: &str = include_str!(concat!(env!("OUT_DIR"), "/registry_snapshot.toml"));

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
    /// If set, this corpus is a layer/satellite of `parent_corpus_id`
    /// (e.g. `wikipedia-simple` and `wikipedia-newsworthy` both declare
    /// `parent_corpus_id = "wikipedia"`). The desktop hides children
    /// from the top-level picker and renders them as toggles under the
    /// parent row. Mirrors the recipe-level field of the same name —
    /// duplicated here so the registry snapshot is self-sufficient and
    /// the desktop doesn't need to parse every recipe TOML to group
    /// the picker.
    #[serde(default)]
    pub parent_corpus_id: Option<String>,
    /// Catalog presentation tier — controls how (and whether) the
    /// desktop's Knowledge tab surfaces this recipe. Curation lives
    /// in the registry snapshot so the UI doesn't grow a parallel
    /// allowlist that drifts whenever a recipe matures.
    ///
    ///   - `"featured"` — robustly built end-to-end. Shown as a normal
    ///     install row.
    ///   - `"preview"` (default) — recipe declared but ingest pipeline
    ///     isn't ready for everyday users. Shown under "Coming soon"
    ///     with the install button disabled.
    ///   - `"hidden"` — never shown in the catalog picker. Used for
    ///     dev/internal corpora (e.g. `alignment`) and recipes that
    ///     are installed automatically by another surface (e.g.
    ///     `conversations-anthropic`, installed by Settings → Imports).
    ///
    /// Layers (`parent_corpus_id` set) ignore this field — they
    /// always render under the parent's row as toggleable chips.
    #[serde(default)]
    pub catalog_status: Option<String>,
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
            Ok(text) => match toml::from_str::<RegistrySnapshot>(&text) {
                Ok(live) => {
                    if live.schema_version > self.snapshot.schema_version {
                        tracing::warn!(
                            live_version = live.schema_version,
                            snapshot_version = self.snapshot.schema_version,
                            "Live registry schema is newer than bundled snapshot — \
                                 falling back to bundled. The snapshot is vendored from \
                                 `sovereign-recipes/registry.toml` at build time: update \
                                 that tree (git pull) and rebuild."
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
            },
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
                parent_corpus_id: e.parent_corpus_id.clone(),
                catalog_status: e.catalog_status.clone(),
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
    /// 1. `<overrides_dir>/<id>.toml` (or `<id>/recipe.toml`) — local file,
    ///    no network. This is `~/.sovereign/recipes/`: user + local-only recipes.
    /// 1b. `$SOVEREIGN_RECIPES_DIR/<id>/recipe.toml` — opt-in dev source dir
    ///    for hot-editing the canonical `sovereign-recipes` tree without a rebuild.
    /// 2. `toml_url` from the registry entry — fetched via HTTP, SHA-256 verified.
    /// 3. Compile-time bundled TOML via [`crate::recipe_builtin::bundled_recipe_toml`]
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

        // 1b. Dev / contributor source dir. `$SOVEREIGN_RECIPES_DIR` points at
        // a `sovereign-recipes` checkout so a contributor can edit the
        // canonical recipe and load it live — no copy into `~/.sovereign`, no
        // rebuild. Explicit opt-in: env unset → skipped, so it never shadows
        // the bundled fallback in normal installs or tests.
        if let Ok(dir) = std::env::var("SOVEREIGN_RECIPES_DIR") {
            if !dir.is_empty() {
                let dir = Path::new(&dir);
                let sub = dir.join(id).join("recipe.toml");
                if sub.is_file() {
                    tracing::debug!(corpus = %id, path = %sub.display(), "Loading recipe from $SOVEREIGN_RECIPES_DIR");
                    return Recipe::from_file(&sub);
                }
                let flat = dir.join(format!("{id}.toml"));
                if flat.is_file() {
                    return Recipe::from_file(&flat);
                }
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
                    // Decide remote-vs-bundled by INTEGRITY, not mere reachability.
                    // The bundled recipe is vendored from the canonical
                    // `sovereign-recipes/` tree at compile time (build.rs), so it
                    // is the reviewed, committed source of truth shipped with this
                    // binary. A remote may only OVERRIDE it when the registry
                    // vouches for the exact bytes (sha256 present and verified) —
                    // that's the hotfix path. An unverified remote (empty sha256)
                    // must never silently shadow the bundled recipe; otherwise a
                    // stale published mirror downgrades a correct in-binary recipe
                    // (the SEP `[prebuilt]` regression: the standalone recipe repo
                    // lagged the monorepo, dropping the prebuilt-snapshot restore).
                    let sha_present = !entry.sha256.is_empty();
                    let sha_verified = if sha_present {
                        match verify_sha256(text.as_bytes(), &entry.sha256) {
                            Ok(()) => true,
                            Err(e) => {
                                tracing::warn!(
                                    corpus = %id,
                                    "Recipe URL fetched but SHA-256 mismatch ({e}); ignoring the remote"
                                );
                                false
                            }
                        }
                    } else {
                        false
                    };
                    let bundled_present = crate::recipe_builtin::bundled_recipe_toml(id).is_some();
                    match choose_recipe_source(sha_present, sha_verified, bundled_present) {
                        RecipeChoice::Remote => return Recipe::from_toml(&text),
                        RecipeChoice::Bundled => {
                            if !sha_present {
                                tracing::warn!(
                                    corpus = %id,
                                    url = %entry.toml_url,
                                    "Recipe URL has no sha256 in the registry; preferring the \
                                     bundled compile-time recipe over the unverified remote to \
                                     avoid silent drift (set a sha256 in registry.toml to let a \
                                     verified remote override the bundled recipe)"
                                );
                            }
                            // fall through to the bundled fallback below.
                        }
                        RecipeChoice::Neither => {
                            // sha mismatch AND no bundled fallback — already warned
                            // above. Fall through to the terminal "no recipe
                            // available" error rather than serve unverified bytes.
                        }
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
        if let Some(toml) = crate::recipe_builtin::bundled_recipe_toml(id) {
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

/// Which source `fetch_recipe` should serve once a remote TOML has been
/// fetched, decided purely from integrity facts (kept side-effect-free so the
/// policy is unit-testable without the network).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipeChoice {
    /// Serve the fetched remote text.
    Remote,
    /// Ignore the remote; serve the bundled compile-time recipe.
    Bundled,
    /// Remote failed integrity (sha mismatch) and there is no bundled
    /// fallback — serve nothing (caller surfaces "no recipe available")
    /// rather than trust unverified bytes.
    Neither,
}

/// Resolution policy for a fetched remote recipe versus the bundled
/// compile-time recipe.
///
/// Invariant (the fail-safe at the heart of the SEP-`[prebuilt]` fix): an
/// UNVERIFIED remote — one the registry carries no `sha256` for — never
/// overrides a bundled recipe. The bundled copy is vendored from the
/// reviewed, committed `sovereign-recipes/` tree at build time, so a stale
/// published mirror must not be able to silently downgrade it. A remote may
/// override bundled ONLY when the registry vouches for its exact bytes
/// (`sha256` present and verified) — the deliberate hotfix path. When no
/// bundled recipe exists (a registry-only corpus not shipped in this binary),
/// the remote is the only source and is used regardless, since refusing it
/// would make the corpus uninstallable.
///
/// `sha256_verified` is only meaningful when `sha256_present` is true; callers
/// pass `false` for it otherwise.
fn choose_recipe_source(
    sha256_present: bool,
    sha256_verified: bool,
    bundled_present: bool,
) -> RecipeChoice {
    match (sha256_present, sha256_verified, bundled_present) {
        // Integrity-verified remote: the hotfix path — wins over bundled.
        (true, true, _) => RecipeChoice::Remote,
        // sha present but mismatched: never serve the bad bytes. Prefer
        // bundled if we have it; otherwise refuse.
        (true, false, true) => RecipeChoice::Bundled,
        (true, false, false) => RecipeChoice::Neither,
        // No sha (unverified): protect the bundled recipe from silent drift…
        (false, _, true) => RecipeChoice::Bundled,
        // …but a registry-only corpus has no bundled fallback, so use the
        // remote — it's the only source available.
        (false, _, false) => RecipeChoice::Remote,
    }
}

#[cfg(test)]
mod resolution_policy_tests {
    use super::{choose_recipe_source, RecipeChoice};

    #[test]
    fn verified_remote_overrides_bundled() {
        // A sha-verified remote is the deliberate hotfix path: it wins even
        // when a bundled recipe ships in the binary.
        assert_eq!(choose_recipe_source(true, true, true), RecipeChoice::Remote);
        assert_eq!(
            choose_recipe_source(true, true, false),
            RecipeChoice::Remote
        );
    }

    #[test]
    fn unverified_remote_yields_to_bundled() {
        // THE FIX. Empty sha256 = no integrity guarantee → must not shadow the
        // build-time-vendored bundled recipe. This is exactly the SEP case:
        // stale standalone mirror (no [prebuilt]) vs correct bundled recipe.
        assert_eq!(
            choose_recipe_source(false, false, true),
            RecipeChoice::Bundled
        );
    }

    #[test]
    fn unverified_remote_used_only_when_no_bundled() {
        // A registry-only corpus not compiled into this binary still installs
        // from the (unverifiable) remote — it is the only source.
        assert_eq!(
            choose_recipe_source(false, false, false),
            RecipeChoice::Remote
        );
    }

    #[test]
    fn sha_mismatch_prefers_bundled_then_refuses() {
        // A present-but-mismatched sha is a hard integrity failure: fall back
        // to bundled when available, otherwise refuse rather than serve
        // tampered/wrong content.
        assert_eq!(
            choose_recipe_source(true, false, true),
            RecipeChoice::Bundled
        );
        assert_eq!(
            choose_recipe_source(true, false, false),
            RecipeChoice::Neither
        );
    }
}

// ── Path helper (used by engine and xtask) ───────────────────────────────────

/// Resolve the path to the canonical registry catalog
/// (`sovereign-recipes/registry.toml`, the single source of truth that
/// build.rs vendors into the bundled snapshot). Returns `None` outside a
/// cargo workspace (e.g. when installed as a standalone binary).
pub fn snapshot_path_in_workspace() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR is set at build time; use it at runtime via env! fallback.
    // corpus-engine/ → ../sovereign-recipes/registry.toml.
    option_env!("CARGO_MANIFEST_DIR").and_then(|d| {
        Path::new(d)
            .parent()
            .map(|ws| ws.join("sovereign-recipes").join("registry.toml"))
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_snapshot_parses() {
        let registry = RecipeRegistry::from_bundled(None);
        let entries = registry.list_entries();
        // Original 9 + wikipedia-catalog + wikipedia-article
        // (chat-with-everything surface) + wikipedia-newsworthy
        // (Portal:Current_events freshness daemon) + the 2026-05
        // conversations-anthropic recipe (private threaded-turn ingest) +
        // the uap-blue-book trio (hero / scans / metadata index) + the
        // 2026-06 conversations-chatgpt recipe (private ChatGPT import).
        // (`alignment` removed 2026-06-19 — it synced the author's
        // ~/.claude and is not part of the open-source recipe set.)
        assert_eq!(entries.len(), 26, "snapshot should have 26 entries");
    }

    #[test]
    fn bundled_snapshot_has_required_fields() {
        let registry = RecipeRegistry::from_bundled(None);
        for entry in registry.list_entries() {
            assert!(!entry.id.is_empty(), "entry id must not be empty");
            assert!(!entry.name.is_empty(), "entry name must not be empty");
            assert!(
                !entry.toml_url.is_empty(),
                "entry toml_url must not be empty"
            );
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

        let registry = RecipeRegistry::from_bundled(None).with_local_registry(&local_path);

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
        let registry = RecipeRegistry::from_bundled(None).with_local_registry(&nonexistent);
        // Same number of entries as the bundled-only registry.
        assert!(!registry.list_entries().is_empty());
        assert!(!registry.is_local_entry("wikipedia"));
    }

    #[test]
    fn sep_is_not_mesh_sharing() {
        let registry = RecipeRegistry::from_bundled(None);
        let sep = registry.find_entry("sep").expect("sep must be in snapshot");
        assert!(
            !sep.mesh_sharing,
            "SEP is license-restricted and must not be mesh-shared"
        );
    }

    #[test]
    fn sep_has_enrichment_enabled() {
        // Wikipedia Core ships with enrichment off (the layered-stack
        // design — Layer 1 prioritises time-to-grounded over atlas
        // depth). Only SEP enables enrichment by default in v1.
        let registry = RecipeRegistry::from_bundled(None);
        let sep = registry.find_entry("sep").expect("sep must be in snapshot");
        assert!(
            sep.enrichment_enabled,
            "sep should have enrichment_enabled = true"
        );

        let wp = registry
            .find_entry("wikipedia")
            .expect("wikipedia must be in snapshot");
        assert!(
            !wp.enrichment_enabled,
            "Wikipedia Core ships with enrichment disabled (Layer 1 design)"
        );
    }

    #[test]
    fn catalog_returns_all_entries() {
        let registry = RecipeRegistry::from_bundled(None);
        let catalog = registry.catalog();
        // 9 originals + wikipedia-catalog / wikipedia-article
        //   (chat-with-everything) + wikipedia-newsworthy
        //   (Portal:Current_events daemon) + conversations-anthropic
        //   (2026-05 threaded-turn ingest) + the uap-blue-book trio
        //   (hero / scans / index) + conversations-chatgpt (2026-06
        //   ChatGPT import). (`alignment` removed 2026-06-19.)
        assert_eq!(catalog.len(), 26);
        assert!(catalog.iter().any(|c| c.id == "uap-blue-book"));
        assert!(catalog.iter().any(|c| c.id == "uap-blue-book-index"));
        assert!(catalog.iter().any(|c| c.id == "conversations-chatgpt"));
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
                crate::recipe_builtin::bundled_recipe_toml(&entry.id).is_some(),
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
        let r = registry
            .fetch_recipe("wikipedia-simple")
            .await
            .expect("wikipedia-simple should resolve via bundled fallback even when URL 404s");
        assert_eq!(r.corpus.id, "wikipedia-simple");
    }
}
