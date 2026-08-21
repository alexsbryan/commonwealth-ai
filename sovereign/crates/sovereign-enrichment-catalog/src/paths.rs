// SPDX-License-Identifier: AGPL-3.0-or-later
//! Filesystem conventions for the enrichment store.
//!
//! Layout under `<data-root>/enrichment/<corpus-id>/`:
//!
//! ```text
//! config.json              # written by `enrich init` (and by the daemon's
//!                          # watched-folder driver), read by every other
//!                          # subcommand and by the desktop's corpus list
//! exemplars/               # one phase<N>.json per phase with the developer's bank
//! cache/                   # one phase<N>.json per phase with the latest full-run output
//! runs/                    # <phase-id>-<mode>-<NNN>.json per run (append-only)
//! ```
//!
//! The chapter manifest is NOT here — it lives at
//! `<data-root>/indexes/<corpus-id>/chapters.json` alongside any
//! future LanceDB index, because it's corpus state, not enrichment
//! state.
//!
//! ── THE ROOT ACCESSOR, AND WHY IT CHANGED ────────────────────────────────
//!
//! [`data_root`] delegates to `sovereign_contracts::rebrand::data_dir()`.
//! That is the derivation `quality/env-flags.toml` declares for
//! `SOVEREIGN_DATA_DIR`: "ONE derivation: `sovereign_contracts::rebrand::
//! data_dir()` — read sites must not re-derive the fallback chain."
//!
//! Until this crate existed, the CLI's copy of these functions rooted at
//! `sovereign_cli_shared::dirs::sovereign_root()`, which forwards to
//! `rebrand::svrnmesh_root()` and does NOT honour the override — while the
//! daemon's watched-folder driver (`sovereign_tools::local_corpus::watched::
//! enrich`) and the desktop both rooted at `data_dir()`, which does. The two
//! agree whenever the override is unset, so the split was invisible on a
//! normal host; with it set, the daemon wrote `config.json` under the
//! override and then spawned `svrn enrich build <id>`, which looked under
//! `~/.svrnmesh` and reported "no enrichment config for corpus <id>".
//! Reader and writer must agree — the same rule `rebrand::projects_json`
//! states for its own path. Merging the copies forced a choice; this is it,
//! and it is a BEHAVIOUR CHANGE for `svrn enrich …` under the override.

use std::path::PathBuf;

/// The per-user data root every enrichment path hangs from.
///
/// One accessor (ARCH_PRINCIPLES §10.6). Do not inline
/// `rebrand::data_dir()` at a call site in this crate — the module doc above
/// is the reason this indirection is named rather than expanded.
pub fn data_root() -> PathBuf {
    sovereign_contracts::rebrand::data_dir()
}

/// `<data-root>/enrichment` — the parent of every enrichment workspace.
/// This is the directory [`crate::catalog`] enumerates.
pub fn enrichment_dir() -> PathBuf {
    data_root().join("enrichment")
}

/// Root of the enrichment state tree for one corpus.
pub fn enrichment_root(corpus_id: &str) -> PathBuf {
    enrichment_dir().join(corpus_id)
}

pub fn config_path(corpus_id: &str) -> PathBuf {
    enrichment_root(corpus_id).join("config.json")
}

pub fn exemplars_dir(corpus_id: &str) -> PathBuf {
    enrichment_root(corpus_id).join("exemplars")
}

pub fn cache_dir(corpus_id: &str) -> PathBuf {
    enrichment_root(corpus_id).join("cache")
}

pub fn runs_dir(corpus_id: &str) -> PathBuf {
    enrichment_root(corpus_id).join("runs")
}

/// `<data-root>/indexes` — the parent of every corpus index.
pub fn indexes_dir() -> PathBuf {
    data_root().join("indexes")
}

/// `<data-root>/indexes/<corpus-id>/` — where the chapter manifest
/// lives (and where a future LanceDB index would, too).
pub fn index_root(corpus_id: &str) -> PathBuf {
    indexes_dir().join(corpus_id)
}

pub fn chapters_manifest_path(corpus_id: &str) -> PathBuf {
    index_root(corpus_id).join("chapters.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_nest_under_data_root() {
        // Race-immune assertion: `data_root()` resolves `$HOME` every call,
        // and HOME is process-wide state several tests mutate. The earlier
        // shape — compute `root`, then compute `enrichment_root("x")`, then
        // `starts_with(root)` — was racy because the two resolutions could
        // read different HOMEs. We sidestep the race by asserting structural
        // composition, which holds regardless of what HOME points at when the
        // test runs.
        assert!(enrichment_root("x").ends_with("enrichment/x"));
        assert!(exemplars_dir("x").ends_with("exemplars"));
        assert!(cache_dir("x").ends_with("cache"));
        assert!(runs_dir("x").ends_with("runs"));
        assert!(config_path("x").ends_with("config.json"));
    }

    #[test]
    fn chapters_manifest_sits_with_index() {
        // Same race story as `paths_nest_under_data_root` — assert by suffix
        // structure, not HOME-rooted prefix.
        assert!(chapters_manifest_path("x").ends_with("indexes/x/chapters.json"));
        assert!(chapters_manifest_path("x").ends_with("chapters.json"));
    }

    /// The one that would have caught the split brain this crate closed.
    ///
    /// Every enrichment path must hang off the SAME accessor the env registry
    /// declares for `SOVEREIGN_DATA_DIR`. Structural, not remembered
    /// (ARCH_PRINCIPLES §7): re-rooting any of these on
    /// `rebrand::svrnmesh_root()` — which silently ignores the override —
    /// fails here rather than three months later as an empty corpus list.
    #[test]
    fn every_path_hangs_off_the_declared_data_root_accessor() {
        let root = sovereign_contracts::rebrand::data_dir();
        assert_eq!(data_root(), root, "data_root must BE rebrand::data_dir()");
        assert_eq!(enrichment_dir(), root.join("enrichment"));
        assert_eq!(indexes_dir(), root.join("indexes"));
        assert_eq!(
            config_path("c"),
            root.join("enrichment").join("c").join("config.json"),
            "the writer (daemon watched-folder driver) and the reader \
             (`svrn enrich build`) must resolve one path"
        );
        assert_eq!(
            chapters_manifest_path("c"),
            root.join("indexes").join("c").join("chapters.json")
        );
    }
}
