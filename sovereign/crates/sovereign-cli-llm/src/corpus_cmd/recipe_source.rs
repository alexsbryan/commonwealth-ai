// SPDX-License-Identifier: AGPL-3.0-or-later
//! Installing a recipe the user wrote, by path.
//!
//! `svrn corpus install` addresses a corpus by ID, and the daemon resolves
//! that ID through the registry — which reaches a hand-written recipe only
//! at `~/.svrnmesh/recipes/<id>/recipe.toml` (`registry.rs` resolution step
//! 1). Nothing in the shipped chain put a file there, so the two commands an
//! author is told to run in sequence did not compose: `recipe validate
//! my-coins.toml` accepted the path and `corpus install my-coins.toml` then
//! answered "No registry entry for corpus 'my-coins.toml'". Registering the
//! file was an undocumented step the author had to know about.
//!
//! This module is that step, done by the command instead of by the author.
//! It is a REGISTRATION, not a substitution (ARCH §18.3): every side effect
//! it has — the id it read, where it copied the file, and any acquire path
//! it made absolute — is printed before the install request goes out.

use std::path::{Path, PathBuf};

/// What [`register`] did, so the caller can report it and then install `id`.
pub(super) struct Registered {
    /// The `[corpus] id` read out of the recipe — what the daemon installs.
    pub id: String,
    /// Where the recipe was copied to, i.e. where the registry will find it.
    pub registered_at: PathBuf,
    /// `Some((before, after))` when a relative `[acquire] path` was resolved
    /// against the recipe's own directory.
    pub acquire_rewrite: Option<(String, String)>,
}

/// Does this argument name a recipe FILE rather than a corpus id?
///
/// Deliberately narrow: the file has to exist and end in `.toml`. A corpus
/// whose id merely looks path-like still resolves as an id, and a typo'd
/// path still reaches the daemon and gets the registry's error rather than
/// a confusing local one.
pub(super) fn looks_like_recipe_path(arg: &str) -> bool {
    let p = Path::new(arg);
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml")) && p.is_file()
}

/// Copy `path` into the local recipe overrides dir under its declared id.
///
/// Returns the id to install. Errors are strings because this is a CLI seam
/// and each one is printed verbatim to the user.
pub(super) fn register(path: &Path) -> Result<Registered, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut doc: toml::Value = toml::from_str(&raw)
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;

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
    // everyone who writes one, and means "beside the DAEMON's working
    // directory" to the acquirer, which resolves it at ingest time in a
    // process that was started somewhere else entirely. Resolve it here,
    // where the recipe's own location is still known, and say so.
    let recipe_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let acquire_rewrite = resolve_acquire_path(&mut doc, &recipe_dir)?;

    let dir = recipes_dir().join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let dest = dir.join("recipe.toml");
    let body = toml::to_string_pretty(&doc).map_err(|e| format!("re-serializing the recipe: {e}"))?;
    std::fs::write(&dest, body).map_err(|e| format!("writing {}: {e}", dest.display()))?;

    Ok(Registered {
        id,
        registered_at: dest,
        acquire_rewrite,
    })
}

/// Where the registry looks for user recipes (`registry.rs` step 1).
fn recipes_dir() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root().join("recipes")
}

/// Make a relative `[acquire] path` absolute against `recipe_dir`.
///
/// A relative path in a recipe means "beside the recipe" to the person who
/// wrote it and "beside the DAEMON's working directory" to the acquirer,
/// which resolves it in a process started somewhere else entirely. This
/// function is where those two readings are reconciled, and the recipe's
/// reading wins — it is the only one the author can see.
///
/// Left alone: absolute paths, `~`-relative paths (the acquirer expands
/// those itself), and `{placeholder}` paths bound at install time from
/// `[recipe.parameters]`.
///
/// A relative path with nothing beside the recipe is an ERROR here rather
/// than a pass-through (ARCH §18.3). Passing it through does not make it
/// work — it makes it fail twenty seconds later, inside the daemon, as
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let err = register(&p).unwrap_err();
        assert!(err.contains("REPLACE_ME"), "{err}");
    }

    #[test]
    fn a_relative_acquire_path_resolves_against_the_recipe_not_the_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "book.md", "# hi");
        let mut doc: toml::Value =
            toml::from_str("[acquire]\npath = \"book.md\"\n").unwrap();
        let (before, after) = resolve_acquire_path(&mut doc, tmp.path()).unwrap().unwrap();
        assert_eq!(before, "book.md");
        assert!(after.ends_with("book.md") && after.starts_with('/'), "{after}");
        assert_eq!(doc["acquire"]["path"].as_str().unwrap(), after);
    }

    #[test]
    fn a_relative_path_with_nothing_beside_the_recipe_fails_here_not_in_the_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let mut doc: toml::Value =
            toml::from_str("[acquire]\npath = \"missing.md\"\n").unwrap();
        let err = resolve_acquire_path(&mut doc, tmp.path()).unwrap_err();
        assert!(err.contains("missing.md"), "{err}");
        assert!(
            err.contains("recipe's own directory"),
            "the error must say which directory it resolved against: {err}"
        );
    }

    #[test]
    fn an_absolute_or_parameterised_path_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        for p in ["/data/book.md", "~/book.md", "{path}"] {
            let mut doc: toml::Value =
                toml::from_str(&format!("[acquire]\npath = \"{p}\"\n")).unwrap();
            assert!(resolve_acquire_path(&mut doc, tmp.path()).unwrap().is_none(), "{p}");
        }
    }
}
