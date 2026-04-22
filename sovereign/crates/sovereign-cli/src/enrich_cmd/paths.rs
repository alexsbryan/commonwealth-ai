//! Filesystem conventions for the `sovereign enrich` admin harness.
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

use crate::util::dirs::{sovereign_indexes, sovereign_root};

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
        let root = sovereign_root();
        assert!(enrichment_root("x").starts_with(&root));
        assert!(exemplars_dir("x").ends_with("exemplars"));
        assert!(cache_dir("x").ends_with("cache"));
        assert!(runs_dir("x").ends_with("runs"));
        assert!(config_path("x").ends_with("config.json"));
    }

    #[test]
    fn chapters_manifest_sits_with_index() {
        assert!(chapters_manifest_path("x").starts_with(sovereign_indexes()));
        assert!(chapters_manifest_path("x").ends_with("chapters.json"));
    }
}
