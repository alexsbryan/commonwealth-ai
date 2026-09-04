// SPDX-License-Identifier: AGPL-3.0-or-later
//! Is a corpus installed on this host, and how far did indexing get?
//!
//! One question, one answer (ARCH §10.6). It lived in
//! `sovereign-cli-llm/src/bench_cmd/discover.rs`, which is the right place
//! for a bench-discovery detail and the wrong place for a fact about the
//! STORE: `svrn quality check` has to answer "is corpus `<id>` installed?"
//! before it runs a lane against it, and `sovereign-cli` cannot see
//! `sovereign-cli-llm` (that edge means llama.cpp). The alternative was a
//! second `data_dir().join("indexes").join(id)` at the precondition site —
//! a re-derivation of the layout this crate exists to own, and the exact
//! shape of the reader/writer split documented in [`crate::paths`].
//!
//! `bench_cmd::discover` re-exports these, so its callers are unchanged.

use crate::paths::index_root;

/// Atlas / index state for a corpus id.
///
/// Three states, not a `bool`: "indexed but not enriched" is the state a
/// retrieval lane can still score in and an enrichment lane cannot, and
/// collapsing it into either neighbour makes one of those two lanes lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusState {
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

impl CorpusState {
    /// The wire/report spelling, for a precondition line or a lane table.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CorpusState::Ready => "ready",
            CorpusState::IndexedNoAtlas => "indexed (no atlas)",
            CorpusState::Unindexed => "not installed",
        }
    }
}

/// Resolve a corpus_id to its atlas / index state on disk.
#[must_use]
pub fn inspect_corpus_state(corpus_id: &str) -> CorpusState {
    let idx = index_root(corpus_id);
    if !idx.exists() {
        return CorpusState::Unindexed;
    }
    let atoms = idx.join("atlas").join("atoms.json");
    if atoms.exists() {
        CorpusState::Ready
    } else {
        CorpusState::IndexedNoAtlas
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
            inspect_corpus_state("qc-no-such-corpus-2f9a1c7e"),
            CorpusState::Unindexed
        );
    }

    /// Three states render three ways. A lane table that spells two of them
    /// the same cannot tell an operator which repair to run.
    #[test]
    fn each_state_has_its_own_spelling() {
        let words = [
            CorpusState::Ready.as_str(),
            CorpusState::IndexedNoAtlas.as_str(),
            CorpusState::Unindexed.as_str(),
        ];
        let mut sorted = words.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "{words:?}");
    }
}
