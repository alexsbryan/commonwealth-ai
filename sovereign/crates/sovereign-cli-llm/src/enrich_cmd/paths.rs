// SPDX-License-Identifier: AGPL-3.0-or-later
//! Filesystem conventions for the `svrn enrich` admin harness.
//!
//! Layout under `~/.sovereign/enrichment/<corpus-id>/`:
//!
//! ```text
//! config.json              # written by `enrich init`, read by every other subcommand
//! exemplars/               # one phase<N>.json per phase with the developer's bank
//! cache/                   # one phase<N>.json per phase with the latest full-run output
//! runs/                    # <phase-id>-<mode>-<NNN>.json per run (append-only)
//! ```
//!
//! The chapter manifest is NOT here — it lives at
//! `~/.sovereign/indexes/<corpus-id>/chapters.json` alongside any
//! future LanceDB index, because it's corpus state, not enrichment
//! state.

use std::path::PathBuf;

use sovereign_cli_shared::dirs::{sovereign_indexes, sovereign_root};

/// Root of the enrichment state tree for one corpus.
pub fn enrichment_root(corpus_id: &str) -> PathBuf {
    sovereign_root().join("enrichment").join(corpus_id)
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

/// `~/.sovereign/indexes/<corpus-id>/` — where the chapter manifest
/// lives (and where a future LanceDB index would, too).
pub fn index_root(corpus_id: &str) -> PathBuf {
    sovereign_indexes().join(corpus_id)
}

pub fn chapters_manifest_path(corpus_id: &str) -> PathBuf {
    index_root(corpus_id).join("chapters.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_nest_under_sovereign_root() {
        // Race-immune assertion: `sovereign_root()` resolves `$HOME`
        // every call, and HOME is process-wide state several tests
        // mutate via `enrich_cmd::test_env::scoped_home`. The earlier
        // shape — compute `root`, then compute `enrichment_root("x")`,
        // then `starts_with(root)` — was racy because the two
        // resolutions could read different HOMEs. We sidestep the
        // race by asserting structural composition: the enrichment
        // root must end in `enrichment/<corpus-id>`, by definition
        // of [`enrichment_root`], regardless of what HOME points at
        // when the test runs.
        assert!(enrichment_root("x").ends_with("enrichment/x"));
        assert!(exemplars_dir("x").ends_with("exemplars"));
        assert!(cache_dir("x").ends_with("cache"));
        assert!(runs_dir("x").ends_with("runs"));
        assert!(config_path("x").ends_with("config.json"));
    }

    #[test]
    fn chapters_manifest_sits_with_index() {
        // Same race story as `paths_nest_under_sovereign_root` —
        // assert by suffix structure, not HOME-rooted prefix.
        assert!(chapters_manifest_path("x").ends_with("indexes/x/chapters.json"));
        assert!(chapters_manifest_path("x").ends_with("chapters.json"));
    }
}
