// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a knowledge search spends its time: one debug event per stage, each
//! with its elapsed time, under one `search` span per request, so a stage
//! table can be built from the log alone. The stages are named here and only
//! here; every site emits through [`SearchStage::emit`].
//!
//! Off by default: the events and the span are DEBUG under [`TARGET`], so a
//! daemon shows them with `RUST_LOG=…,search.stage=debug`. The embed slot's
//! own wait is the engine's to report (`slot_wait_ms` on its
//! `inference.embed: done` event), and lands under the same span.

use std::time::Duration;

/// The tracing target every stage event and the `search` span carry.
pub const TARGET: &str = "search.stage";

/// The stages of one knowledge search, in the order they run. A passage
/// search runs [`SearchStage::PASSAGE_ROUTE`]; a sentence search
/// (`granularity: sentence`, OICP v0.6 §2.11) runs
/// [`SearchStage::SENTENCE_ROUTE`], which never locates a passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStage {
    /// The handler's own admission (this route carries no admission layer):
    /// the scope check, the corpora this caller may read, the mesh plan.
    Admission,
    /// The query's vector: embedded here (`count = 1`, or 0 when the embed
    /// failed; the engine's slot wait included), or supplied by the caller
    /// (`count = 0`, `elapsed_us = 0`).
    QueryEmbed,
    /// One corpus's index query (`CorpusIndex::search` / `search_in_texts`).
    IndexQuery,
    /// One corpus's hits' text names and document records.
    Records,
    /// One corpus's stored-text reads, each text once.
    TextRead,
    /// One corpus's hits located in their texts (`oicp_evidence::span_of`).
    SpanOf,
    /// A sentence search's scoring (`routes_knowledge_sentences`): the scan
    /// of the scope's sentence vectors on the index route (`count` the
    /// sentences scanned), or the passage hits' sentences on the passages
    /// route (`count` the sentences scored).
    SentenceScan,
}

impl SearchStage {
    pub const ALL: [SearchStage; 7] = [
        SearchStage::Admission,
        SearchStage::QueryEmbed,
        SearchStage::IndexQuery,
        SearchStage::Records,
        SearchStage::TextRead,
        SearchStage::SpanOf,
        SearchStage::SentenceScan,
    ];

    /// The stages a passage search emits.
    pub const PASSAGE_ROUTE: [SearchStage; 6] = [
        SearchStage::Admission,
        SearchStage::QueryEmbed,
        SearchStage::IndexQuery,
        SearchStage::Records,
        SearchStage::TextRead,
        SearchStage::SpanOf,
    ];

    /// The stages a sentence search answered from its index emits: its hits
    /// are read from their stored addresses, so neither `index_query` nor
    /// `span_of` runs.
    pub const SENTENCE_ROUTE: [SearchStage; 4] = [
        SearchStage::QueryEmbed,
        SearchStage::Admission,
        SearchStage::SentenceScan,
        SearchStage::TextRead,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SearchStage::Admission => "admission",
            SearchStage::QueryEmbed => "query_embed",
            SearchStage::IndexQuery => "index_query",
            SearchStage::Records => "records",
            SearchStage::TextRead => "text_read",
            SearchStage::SpanOf => "span_of",
            SearchStage::SentenceScan => "sentence_scan",
        }
    }

    /// The stage's one event. `corpus` names the corpus a per-corpus stage
    /// ran over; `count` is how many things it handled (hits, texts).
    pub(crate) fn emit(self, elapsed: Duration, corpus: Option<&str>, count: usize) {
        tracing::debug!(
            target: TARGET,
            stage = self.as_str(),
            elapsed_us = elapsed.as_micros() as u64,
            corpus,
            count,
            "search stage"
        );
    }
}

/// The span one search's stage events sit under. `query` is the first 12
/// hex of the query text's sha256, so a reader matches its own searches
/// without the text being logged.
pub(crate) fn search_span(route: &'static str, query_text: &str) -> tracing::Span {
    let hex = kernel_types::Sha256Hash::of_str(query_text).to_hex();
    tracing::debug_span!(target: TARGET, "search", route, query = &hex[..12])
}
