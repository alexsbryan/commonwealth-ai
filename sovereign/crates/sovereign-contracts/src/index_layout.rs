// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where corpus indexes live, and how far indexing got for one corpus.
//!
//! Two questions, one answer each (ARCH §10.6). Both used to live in
//! `sovereign-enrichment-catalog::{paths, corpus_state}`, which is the
//! `ingest` package — and `svrn quality check`'s `CorpusInstalled`
//! precondition, `svrn bench all`'s discovery grading and the chat-ask lane
//! all ask them from `svrn` crates. That edge was two red lines on
//! `boundary-gate` (`sovereign-cli -> sovereign-enrichment-catalog`,
//! `sovereign-cli-llm -> …`), and the alternative every caller reaches for
//! when it cannot name the owner is a second
//! `data_dir().join("indexes").join(id)` — the re-derivation
//! `sovereign_enrichment_catalog::paths`'s module doc records as having
//! stranded the daemon's watched-folder writes from the CLI's reads.
//!
//! Here rather than in that crate because `indexes/` is CORPUS state, not
//! enrichment state (the same thing `paths`'s own doc says about
//! `chapters.json`), and because this crate is a `[[package_leaf]]`: every
//! package may name it, so one derivation can serve all of them.
//!
//! `sovereign_enrichment_catalog::paths::{indexes_dir, index_root}` delegate
//! here; the enrichment tree's own layout stays there.
//!
//! The type is `CorpusIndexState`, not `CorpusState`: [`crate::types`] already
//! owns a `CorpusState` — the installed-corpus bookkeeping ROW, keyed in
//! SQLite. This one is what the filesystem says. One name for one thing.

use std::path::PathBuf;

/// `<data-root>/indexes` — the parent of every corpus index.
///
/// Rooted at [`crate::rebrand::data_dir`], which is the ONE derivation
/// `quality/env-flags.toml` declares for `SOVEREIGN_DATA_DIR`.
#[must_use]
pub fn indexes_dir() -> PathBuf {
    crate::rebrand::data_dir().join("indexes")
}

/// `<data-root>/indexes/<corpus-id>/` — one corpus's index tree.
#[must_use]
pub fn index_root(corpus_id: &str) -> PathBuf {
    indexes_dir().join(corpus_id)
}

/// Atlas / index state for a corpus id, as the filesystem has it.
///
/// Three states, not a `bool`: "indexed but not enriched" is the state a
/// retrieval lane can still score in and an enrichment lane cannot, and
/// collapsing it into either neighbour makes one of those two lanes lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusIndexState {
    /// Atlas dir present with at least atoms.json. Enrichment lane
    /// can score; retrieval lane will score against the live daemon.
    Ready,
    /// Index dir exists but atlas is missing. Retrieval lane can
    /// still attempt to score (bm25 / vector against the chunks).
    /// Enrichment lane will mark this stale.
    IndexedNoAtlas,
    /// Corpus dir doesn't exist locally. Both surfaces mark stale.
    Unindexed,
}

impl CorpusIndexState {
    /// The wire/report spelling, for a precondition line or a lane table.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CorpusIndexState::Ready => "ready",
            CorpusIndexState::IndexedNoAtlas => "indexed (no atlas)",
            CorpusIndexState::Unindexed => "not installed",
        }
    }
}

/// Resolve a corpus_id to its atlas / index state on disk.
#[must_use]
pub fn inspect_corpus_index_state(corpus_id: &str) -> CorpusIndexState {
    let idx = index_root(corpus_id);
    if !idx.exists() {
        return CorpusIndexState::Unindexed;
    }
    let atoms = idx.join("atlas").join("atoms.json");
    if atoms.exists() {
        CorpusIndexState::Ready
    } else {
        CorpusIndexState::IndexedNoAtlas
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The absent case is the one a precondition reads, and it must not
    /// depend on any host state — an id nothing could have created is
    /// `Unindexed` on every machine.
    #[test]
    fn an_id_nothing_created_is_not_installed() {
        assert_eq!(
            inspect_corpus_index_state("qc-no-such-corpus-2f9a1c7e"),
            CorpusIndexState::Unindexed
        );
    }

    /// Three states render three ways. A lane table that spells two of them
    /// the same cannot tell an operator which repair to run.
    #[test]
    fn each_state_has_its_own_spelling() {
        let words = [
            CorpusIndexState::Ready.as_str(),
            CorpusIndexState::IndexedNoAtlas.as_str(),
            CorpusIndexState::Unindexed.as_str(),
        ];
        let mut sorted = words.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "{words:?}");
    }

    /// Structural, not remembered: the index tree must hang off the accessor
    /// `quality/env-flags.toml` declares, the same pin
    /// `sovereign_enrichment_catalog::paths` carries for the enrichment tree.
    #[test]
    fn indexes_hang_off_the_declared_data_root_accessor() {
        let root = crate::rebrand::data_dir();
        assert_eq!(indexes_dir(), root.join("indexes"));
        assert_eq!(index_root("c"), root.join("indexes").join("c"));
    }
}
