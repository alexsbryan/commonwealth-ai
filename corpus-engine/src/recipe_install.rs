// SPDX-License-Identifier: AGPL-3.0-or-later
//! Installing a recipe the user wrote — putting the file where
//! [`crate::registry::RecipeRegistry`] will find it.
//!
//! TWO journeys live here, and they are not the same one (which is why
//! neither absorbed the other on 2026-09-12, when the second arrived):
//!
//! - [`register`] — "install THIS FILE": the caller has a path to a
//!   `.toml` an author just wrote. It re-serialises the doc (because it
//!   REWRITES the relative `[acquire] path` on the way through) and
//!   writes no registry entry, because the id it returns is about to be
//!   installed by the caller and resolution step 1 reads the file
//!   directly.
//! - [`RecipeRegistry::install_local_recipe`] — "PUBLISH this recipe":
//!   `svrn recipe publish` and the daemon's
//!   `/internal/corpus/recipes/import` want the recipe to show up in
//!   `list_entries` afterwards, which means a `registry.toml` entry
//!   carrying a sha256 OF THE BYTES ON DISK. So it writes the text
//!   VERBATIM, and both writes are staged-and-renamed.
//!
//! What they DO share is the layout — `<recipes_dir>/<id>/recipe.toml` —
//! and that is [`recipe_path_in`], called by both. Two functions deciding
//! one path is the smell (ARCH principle 8); two functions doing
//! different things to it is not.
//!
//! A corpus is addressed by ID, and the registry resolves that ID through a
//! hand-written recipe only at `<recipes_dir>/<id>/recipe.toml` (`registry.rs`
//! resolution step 1). Nothing in the shipped chain put a file there, so the
//! two commands an author is told to run in sequence did not compose: `recipe
//! validate my-coins.toml` accepted the path and `corpus install my-coins.toml`
//! then answered "No registry entry for corpus 'my-coins.toml'". Registering
//! the file was an undocumented step the author had to know about.
//!
//! This module is that step, done by the command instead of by the author.
//! It is a REGISTRATION, not a substitution (ARCH §18.3): every side effect
//! it has — the id it read, where it copied the file, and any acquire path it
//! made absolute — is returned to the caller so the caller can print it before
//! the install runs.
//!
//! It lives HERE, in corpus-engine, because the registry that reads the file
//! back does (ARCH §10.6: one decider, one name). It was `sovereign-cli-llm`'s
//! `corpus_cmd::recipe_source` until order ei-5b-build-verb, when `corpus-mcp`
//! needed the same step for `corpus ingest <recipe.toml>` — the second host to
//! need it, which is the moment a host's private helper becomes a capability.
//! The CLI is now a caller; there is one implementation, not two.
//!
//! The recipes dir is a PARAMETER, not a derivation: this crate has no opinion
//! about where a host keeps its data root, and both hosts already hold theirs
//! (`CorpusEngine::new` takes it too).

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::recipe::Recipe;
use crate::registry::{RecipeRegistry, RegistryEntry, RegistrySnapshot};

/// What [`register`] did, so the caller can report it and then install `id`.
#[derive(Debug)]
pub struct Registered {
    /// The `[corpus] id` read out of the recipe — what the caller installs.
    pub id: String,
    /// Where the recipe was copied to, i.e. where the registry will find it.
    pub registered_at: PathBuf,
    /// `Some((before, after))` when a relative `[acquire] path` was resolved
    /// against the recipe's own directory.
    pub acquire_rewrite: Option<(String, String)>,
}

/// WHERE a recipe for `id` lives under `recipes_dir` — the one place this
/// layout is decided. `registry.rs`'s resolution step 1 reads exactly this
/// path, and both writers below produce it.
///
/// The dir is exposed alongside the file because both writers need to
/// `create_dir_all` it first. Deriving it back out of the path with
/// `.parent().expect(..)` is a panic that cannot fire and still has to be
/// read by the next person; two functions that agree by construction say
/// it better.
pub fn recipe_dir_in(recipes_dir: &Path, id: &str) -> PathBuf {
    recipes_dir.join(id)
}

/// The recipe file itself: [`recipe_dir_in`] plus `recipe.toml`.
pub fn recipe_path_in(recipes_dir: &Path, id: &str) -> PathBuf {
    recipe_dir_in(recipes_dir, id).join("recipe.toml")
}

/// Does this argument name a recipe FILE rather than a corpus id?
///
/// Deliberately narrow: the file has to exist and end in `.toml`. A corpus
/// whose id merely looks path-like still resolves as an id, and a typo'd
/// path still reaches the registry and gets its error rather than a
/// confusing local one.
pub fn looks_like_recipe_path(arg: &str) -> bool {
    let p = Path::new(arg);
    p.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        && p.is_file()
}

/// Copy `path` into `recipes_dir` under its declared id.
///
/// Returns the id to install. Errors are strings because this is a host seam
/// and each one is printed verbatim to the user.
pub fn register(path: &Path, recipes_dir: &Path) -> Result<Registered, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut doc: toml::Value =
        toml::from_str(&raw).map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;

    let id = doc
        .get("corpus")
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{} has no `[corpus] id`", path.display()))?
        .to_string();
    if id.is_empty() || id == "REPLACE_ME" {
        return Err(format!(
            "{} still has the scaffold's `id = \"REPLACE_ME\"` — give the corpus a stable id first",
            path.display()
        ));
    }

    // A relative `[acquire] path` in a recipe means "beside the recipe" to
    // everyone who writes one, and means "beside the running process's working
    // directory" to the acquirer, which resolves it at ingest time in a
    // process that was started somewhere else entirely. Resolve it here,
    // where the recipe's own location is still known, and say so.
    let recipe_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let acquire_rewrite = resolve_acquire_path(&mut doc, &recipe_dir)?;

    let dir = recipe_dir_in(recipes_dir, &id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let dest = recipe_path_in(recipes_dir, &id);
    let body =
        toml::to_string_pretty(&doc).map_err(|e| format!("re-serializing the recipe: {e}"))?;
    std::fs::write(&dest, body).map_err(|e| format!("writing {}: {e}", dest.display()))?;

    Ok(Registered {
        id,
        registered_at: dest,
        acquire_rewrite,
    })
}

/// Make a relative `[acquire] path` absolute against `recipe_dir`.
///
/// A relative path in a recipe means "beside the recipe" to the person who
/// wrote it and "beside the running process's working directory" to the
/// acquirer, which resolves it in a process started somewhere else entirely.
/// This function is where those two readings are reconciled, and the recipe's
/// reading wins — it is the only one the author can see.
///
/// Left alone: absolute paths, `~`-relative paths (the acquirer expands
/// those itself), and `{placeholder}` paths bound at install time from
/// `[recipe.parameters]`.
///
/// A relative path with nothing beside the recipe is an ERROR here rather
/// than a pass-through (ARCH §18.3). Passing it through does not make it
/// work — it makes it fail twenty seconds later, inside the ingest, as
/// `Local source not found: <the path>` in a log the author is not reading.
/// The failure is the same; only the place it is legible differs.
fn resolve_acquire_path(
    doc: &mut toml::Value,
    recipe_dir: &Path,
) -> Result<Option<(String, String)>, String> {
    let Some(before) = doc
        .get("acquire")
        .and_then(|a| a.get("path"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    if before.starts_with('/') || before.starts_with('~') || before.contains('{') {
        return Ok(None);
    }
    let candidate = recipe_dir.join(&before);
    if !candidate.exists() {
        return Err(format!(
            "`[acquire] path = \"{before}\"` is relative, and nothing is at {} — \
             a relative acquire path is resolved against the recipe's own directory. \
             Point it at a file beside the recipe, or give an absolute path.",
            candidate.display()
        ));
    }
    // `candidate` is already an existing, absolute-by-construction path — we
    // just proved `exists()` on it. Canonicalising only resolves symlinks and
    // `..`, so a failure here loses tidiness, not correctness, and the
    // un-canonical path acquires the same file. Not a swallowed failure.
    let after = std::fs::canonicalize(&candidate)
        .unwrap_or(candidate)
        .to_string_lossy()
        .into_owned();
    doc["acquire"]["path"] = toml::Value::String(after.clone());
    Ok(Some((before, after)))
}

impl RecipeRegistry {
    /// Install an authored recipe into THIS registry's overrides dir: the
    /// TOML at `<overrides_dir>/<id>/recipe.toml` and an upserted entry in
    /// `<overrides_dir>/registry.toml`. Returns the recipe's path.
    ///
    /// The ONE decider for "a user published a recipe" (ARCH principle 8).
    /// Two copies of this loop preceded it — `svrn recipe publish`'s
    /// `upsert_local_registry_entry` and the desktop's
    /// `corpus_import_recipe` — and they could not agree by construction,
    /// because each resolved its own local root: the desktop's wrote into
    /// `~/.svrnmesh/recipes` whatever recipes dir the daemon was actually
    /// commissioned with. Installing through the registry that will RESOLVE
    /// the recipe is what makes the round trip hold, and is why this takes
    /// no path argument.
    ///
    /// `toml_text` is written VERBATIM, not reserialised: the digest in the
    /// registry entry is over these bytes, so a round trip through
    /// `toml::to_string` would record a sha256 of something other than what
    /// is on disk. Both writes are staged-and-renamed.
    ///
    /// An `overrides_dir` of `None` is an error naming it, never a silent
    /// skip (ARCH principle 6) — a caller with no local root did not
    /// publish anything, and `cache_recipe`'s `Ok(None)` above is about an
    /// absent CACHE, which is a different fact.
    pub fn install_local_recipe(
        &self,
        recipe: &Recipe,
        toml_text: &str,
    ) -> crate::error::Result<PathBuf> {
        let local_root = self.overrides_dir().ok_or_else(|| {
            Error::Recipe(
                "this registry has no overrides dir, so there is nowhere to install a \
                 recipe; build it with `from_bundled(Some(recipes_dir))`"
                    .to_string(),
            )
        })?;
        if recipe.corpus.id.is_empty() {
            return Err(Error::Recipe(
                "recipe `[corpus] id` must not be empty".to_string(),
            ));
        }

        std::fs::create_dir_all(recipe_dir_in(local_root, &recipe.corpus.id))?;
        let recipe_path = recipe_path_in(local_root, &recipe.corpus.id);
        write_atomically(&recipe_path, toml_text.as_bytes())?;

        let registry_path = local_root.join("registry.toml");
        // ABSENT and UNREADABLE are not the same fact, and the difference
        // matters here because the next thing this function does is
        // overwrite the file: a first publish has no registry (fresh
        // snapshot), while a registry we cannot READ — a permission, a
        // directory in its place — must refuse rather than be replaced by
        // one entry. Both prior copies of this loop said
        // `unwrap_or_default()` and would have silently discarded the
        // user's whole published set (ARCH principle 6).
        let existing = match std::fs::read_to_string(&registry_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(Error::Recipe(format!(
                    "local registry at {} exists but cannot be read ({e}); refusing to \
                     replace it",
                    registry_path.display()
                )))
            }
        };
        // A local registry we cannot parse is replaced rather than refused:
        // both prior copies did this, and the alternative is a user whose
        // one corrupt file blocks every future publish.
        let mut snapshot: RegistrySnapshot = toml::from_str(&existing).unwrap_or_else(|_| {
            if !existing.is_empty() {
                tracing::warn!(
                    path = %registry_path.display(),
                    "local registry did not parse — replacing it with a fresh snapshot"
                );
            }
            RegistrySnapshot {
                schema_version: 1,
                generated_at: String::new(),
                registry_url: String::new(),
                entries: Vec::new(),
            }
        });
        snapshot.entries.retain(|e| e.id != recipe.corpus.id);
        snapshot.entries.push(RegistryEntry {
            id: recipe.corpus.id.clone(),
            name: recipe.corpus.name.clone(),
            description: recipe.corpus.description.clone(),
            license: recipe.corpus.license.clone(),
            size_compressed_gb: recipe.corpus.size_compressed_gb,
            size_indexed_gb: recipe.corpus.size_indexed_gb,
            toml_url: format!("file://{}/recipe.toml", recipe.corpus.id),
            sha256: sha256_hex(toml_text.as_bytes()),
            enrichment_enabled: recipe
                .enrichment
                .as_ref()
                .map(|e| e.enabled)
                .unwrap_or(false),
            mesh_sharing: recipe.corpus.mesh_sharing,
            prebuilt: None,
            parent_corpus_id: recipe.corpus.parent_corpus_id.clone(),
            catalog_status: None,
        });
        snapshot.generated_at = chrono::Utc::now().to_rfc3339();

        let serialized = toml::to_string_pretty(&snapshot)
            .map_err(|e| Error::Recipe(format!("serialise local registry: {e}")))?;
        write_atomically(&registry_path, serialized.as_bytes())?;

        tracing::debug!(
            corpus = %recipe.corpus.id,
            path = %recipe_path.display(),
            entries = snapshot.entries.len(),
            "installed recipe into the local registry"
        );
        Ok(recipe_path)
    }
}

/// Lowercase hex SHA-256 — the digest form every `registry.toml` entry
/// carries, and the one [`verify_sha256`] below checks against.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Stage beside the destination, then rename: a reader never sees a
/// half-written recipe or registry. `.part` is the suffix both prior
/// copies used.
fn write_atomically(path: &Path, bytes: &[u8]) -> crate::error::Result<()> {
    let part = path.with_extension("toml.part");
    std::fs::write(&part, bytes)?;
    std::fs::rename(&part, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A minimal recipe that passes `Recipe::from_toml`, parameterised by
    /// id so each test owns its own entry.
    fn round_trip_recipe_toml(id: &str) -> String {
        format!(
            r#"
[corpus]
id = "{id}"
name = "NAME"
description = "d"
license = "Public Domain"
size_compressed_gb = 0.001
size_indexed_gb = 0.001

[acquire]
type = "bulk_download"
url = "https://example.invalid/x.txt"

[extract]
type = "plaintext"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 256

[index]
fts = true
vector = true
"#
        )
    }

    /// The round trip the recipe-import route depends on: a recipe
    /// installed through a registry is then RESOLVED by the same registry,
    /// out of the overrides dir it was installed into — the fault being
    /// prevented is a publisher writing where the resolver does not read.
    #[tokio::test]
    async fn install_local_recipe_lands_where_fetch_recipe_resolves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = RecipeRegistry::from_bundled(Some(tmp.path().to_path_buf()));
        let toml_text = round_trip_recipe_toml("install-round-trip");
        let recipe = Recipe::from_toml(&toml_text).expect("recipe parses");
        let path = registry
            .install_local_recipe(&recipe, &toml_text)
            .expect("install succeeds");

        assert_eq!(path, tmp.path().join("install-round-trip/recipe.toml"));
        // Written verbatim: the digest in the entry is over THESE bytes.
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            toml_text
        );
        let registry_toml =
            std::fs::read_to_string(tmp.path().join("registry.toml")).expect("registry written");
        let snapshot: RegistrySnapshot =
            toml::from_str(&registry_toml).expect("registry parses back");
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].id, "install-round-trip");
        assert_eq!(snapshot.entries[0].sha256, sha256_hex(toml_text.as_bytes()));
        assert!(!snapshot.generated_at.is_empty());
        // No `.part` survives either write.
        assert!(!tmp.path().join("registry.toml.part").exists());

        // The resolver reads the subdir layout off disk, so the recipe is
        // installable with no reload of this registry.
        let resolved = registry
            .fetch_recipe("install-round-trip")
            .await
            .expect("the same registry resolves what it installed");
        assert_eq!(resolved.corpus.id, "install-round-trip");
    }

    /// A second install of the same id replaces the entry rather than
    /// appending a duplicate, and a corrupt local registry is replaced
    /// rather than blocking every future publish.
    #[test]
    fn install_local_recipe_upserts_and_survives_a_corrupt_registry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("registry.toml"), "this is not toml {{{")
            .expect("seed a corrupt registry");
        let registry = RecipeRegistry::from_bundled(Some(tmp.path().to_path_buf()));
        let base = round_trip_recipe_toml("upsert-me");
        let first = Recipe::from_toml(&base).expect("parses");
        registry
            .install_local_recipe(&first, &base)
            .expect("first install");
        let renamed_text = base.replace("name = \"NAME\"", "name = \"RENAMED\"");
        let second = Recipe::from_toml(&renamed_text).expect("parses");
        registry
            .install_local_recipe(&second, &renamed_text)
            .expect("second install");

        let snapshot: RegistrySnapshot = toml::from_str(
            &std::fs::read_to_string(tmp.path().join("registry.toml")).expect("read"),
        )
        .expect("parses");
        assert_eq!(snapshot.entries.len(), 1, "upsert, not append");
        assert_eq!(snapshot.entries[0].name, "RENAMED");
        assert_eq!(
            snapshot.entries[0].sha256,
            sha256_hex(renamed_text.as_bytes()),
            "the digest follows the bytes that are now on disk"
        );
    }

    /// A registry file that EXISTS and cannot be read refuses, rather
    /// than being replaced by a one-entry snapshot. A directory in its
    /// place is the failing input: `read_to_string` returns an error that
    /// is not `NotFound`, which is exactly the case `unwrap_or_default()`
    /// used to swallow.
    #[test]
    fn install_local_recipe_refuses_an_unreadable_registry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join("registry.toml"))
            .expect("put a directory where the registry file goes");
        let registry = RecipeRegistry::from_bundled(Some(tmp.path().to_path_buf()));
        let toml_text = round_trip_recipe_toml("unreadable-registry");
        let recipe = Recipe::from_toml(&toml_text).expect("parses");

        let err = registry
            .install_local_recipe(&recipe, &toml_text)
            .expect_err("an unreadable registry refuses");
        assert!(
            err.to_string().contains("cannot be read"),
            "the refusal says the registry could not be read: {err}"
        );
        // And it is still there — not replaced by one entry.
        assert!(tmp.path().join("registry.toml").is_dir());
    }

    /// A registry with no overrides dir REFUSES rather than quietly
    /// installing nowhere (ARCH principle 6).
    #[test]
    fn install_local_recipe_refuses_without_an_overrides_dir() {
        let registry = RecipeRegistry::from_bundled(None);
        let toml_text = round_trip_recipe_toml("nowhere");
        let recipe = Recipe::from_toml(&toml_text).expect("parses");
        let err = registry
            .install_local_recipe(&recipe, &toml_text)
            .expect_err("no overrides dir is an error, not a silent skip");
        assert!(
            err.to_string().contains("overrides dir"),
            "the refusal names what is missing: {err}"
        );
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn a_corpus_id_is_not_mistaken_for_a_path() {
        assert!(!looks_like_recipe_path("wessex-hoard"));
        assert!(!looks_like_recipe_path("does-not-exist.toml"));
    }

    #[test]
    fn an_existing_toml_file_is_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "r.toml", "");
        assert!(looks_like_recipe_path(p.to_str().unwrap()));
    }

    #[test]
    fn the_scaffold_placeholder_id_is_refused_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "r.toml", "[corpus]\nid = \"REPLACE_ME\"\n");
        let err = register(&p, tmp.path()).unwrap_err();
        assert!(err.contains("REPLACE_ME"), "{err}");
    }

    #[test]
    fn a_relative_acquire_path_resolves_against_the_recipe_not_the_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "book.md", "# hi");
        let mut doc: toml::Value = toml::from_str("[acquire]\npath = \"book.md\"\n").unwrap();
        let (before, after) = resolve_acquire_path(&mut doc, tmp.path()).unwrap().unwrap();
        assert_eq!(before, "book.md");
        assert!(
            after.ends_with("book.md") && after.starts_with('/'),
            "{after}"
        );
        assert_eq!(doc["acquire"]["path"].as_str().unwrap(), after);
    }

    #[test]
    fn a_relative_path_with_nothing_beside_the_recipe_fails_here_not_later() {
        let tmp = tempfile::tempdir().unwrap();
        let mut doc: toml::Value = toml::from_str("[acquire]\npath = \"missing.md\"\n").unwrap();
        let err = resolve_acquire_path(&mut doc, tmp.path()).unwrap_err();
        assert!(err.contains("missing.md"), "{err}");
    }

    #[test]
    fn an_absolute_acquire_path_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let mut doc: toml::Value = toml::from_str("[acquire]\npath = \"/abs/book.md\"\n").unwrap();
        assert!(resolve_acquire_path(&mut doc, tmp.path())
            .unwrap()
            .is_none());
        assert_eq!(doc["acquire"]["path"].as_str().unwrap(), "/abs/book.md");
    }

    /// The whole point of the move: a recipe registered by one host lands
    /// exactly where the registry's step-1 lookup reads it back.
    #[test]
    fn register_lands_the_file_where_the_registry_looks() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "coins.md", "## one\nbody\n");
        let recipe = write(
            tmp.path(),
            "my-coins.toml",
            "[corpus]\nid = \"my-coins\"\n\n[acquire]\ntype = \"local_file\"\npath = \"coins.md\"\n",
        );
        let recipes = tmp.path().join("recipes");
        let reg = register(&recipe, &recipes).unwrap();
        assert_eq!(reg.id, "my-coins");
        assert_eq!(reg.registered_at, recipes.join("my-coins/recipe.toml"));
        assert!(reg.registered_at.is_file());
        let (before, after) = reg.acquire_rewrite.unwrap();
        assert_eq!(before, "coins.md");
        assert!(after.starts_with('/'), "{after}");
    }
}
