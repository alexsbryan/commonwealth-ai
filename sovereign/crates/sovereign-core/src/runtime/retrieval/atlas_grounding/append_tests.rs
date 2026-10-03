// SPDX-License-Identifier: AGPL-3.0-or-later
//! The late summary append and its chunk, in the pipeline's unit. Beside
//! `atlas_grounding.rs` under `#[path]` so the step file stays under the
//! ARCH §3.1 band.

use super::{append_atlas_summaries, atlas_summary_chunk};
use corpus_engine_atlas_reader::evidence_site::EvidenceSite;
use corpus_engine_atlas_reader::ground::SummaryNode;
use corpus_index::{index::ChunkProvenance, types::ScoredChunk};

fn leaf(i: usize) -> ScoredChunk {
    ScoredChunk {
        content: format!("leaf {i}"),
        title: Some("abduction".into()),
        url: None,
        corpus_id: "sep".into(),
        score: 0.9,
        metadata: Default::default(),
        chunk_id: Some(i as u64),
        source_doc_id: None,
        vector_distance: Some(0.1),
        // A real acquired LEAF: `Grain::Leaf`, so the reserve's grain
        // predicate must NOT pick it up. Using a manufactured summary here
        // would make every assertion below vacuous.
        provenance: ChunkProvenance::acquired_from_estate("sep"),
    }
}

fn node(score: f32) -> SummaryNode {
    SummaryNode {
        atom_id: "summary-abcdef0123456789".into(),
        site: EvidenceSite::derive("sep-abduction"),
        text: "a rollup".into(),
        score,
    }
}

/// The failure this whole placement exists to prevent, in the direction it
/// actually happened: a summary appended at the TAIL of a full pool is cut
/// by the char budget and the pool truncate before the prompt ever sees it
/// (measured on `summary_proof_theory`: pool=40, admitted=28,
/// raptor_admitted=0 of 8; invariant 3035f3a4). So the append must leave
/// the summary at the HEAD.
#[test]
fn a_late_appended_summary_lands_at_the_head_not_the_tail() {
    let mut pool: Vec<ScoredChunk> = (0..5).map(leaf).collect();
    let appended = append_atlas_summaries(&mut pool, &[node(0.5)], "test");
    assert_eq!(appended, 1);
    assert_eq!(pool.len(), 6, "the chunk SET must be unchanged in size");
    assert_eq!(
        pool[0].provenance.producer(),
        Some("atlas_summary"),
        "the summary must be first, or the budget cuts it"
    );
    // Order-only: every leaf is still present, in its original order.
    let leaves: Vec<&str> = pool[1..].iter().map(|c| c.content.as_str()).collect();
    assert_eq!(
        leaves,
        vec!["leaf 0", "leaf 1", "leaf 2", "leaf 3", "leaf 4"]
    );
}

/// The other direction (§18.6): with no summaries the pool is returned
/// untouched — no reserve, no reorder, no allocation of a different order.
/// A "fix" that reordered every pool would be invisible in the test above
/// and would perturb every lane that has no summaries at all.
#[test]
fn a_walk_that_reached_no_summary_leaves_the_pool_alone() {
    let mut pool: Vec<ScoredChunk> = (0..5).map(leaf).collect();
    let before: Vec<String> = pool.iter().map(|c| c.content.clone()).collect();
    let appended = append_atlas_summaries(&mut pool, &[], "test");
    assert_eq!(appended, 0);
    let after: Vec<String> = pool.iter().map(|c| c.content.clone()).collect();
    assert_eq!(before, after);
}

/// The chunk carries the SUMMARY grain (so every quotability gate and the
/// reserve treat it as one) and its OWN producer (so a trace can say which
/// of the two paths put it in the pool). Both halves matter: one grain,
/// two producers.
#[test]
fn the_summary_chunk_is_summary_grain_with_its_own_producer() {
    let c = atlas_summary_chunk(&node(0.7));
    assert_eq!(c.provenance.grain(), kernel_types::Grain::Summary);
    assert_eq!(c.provenance.producer(), Some("atlas_summary"));
    // The chunk is attributed to the corpus that HOLDS the chunks, not to
    // the per-article atlas id — the same corpus a fetched chunk of that
    // article would carry, so the per-corpus ledgers stay comparable.
    assert_eq!(c.corpus_id, "sep");
    assert_eq!(c.title.as_deref(), Some("abduction"));
}
/// The chunk → atlas id derivation, in both shapes, and its agreement
/// with `corpus_engine::…evidence_site::EvidenceSite`'s reading in the other direction. Failing input:
/// drop the self-hosted candidate, or emit the child for a titleless
/// chunk.
#[test]
fn candidate_atlas_ids_covers_both_layouts_and_agrees_with_evidence_site() {
    use crate::runtime::retrieval::candidate_atlas_ids;
    let ids = candidate_atlas_ids("sep", Some("freewill"));
    assert_eq!(ids, vec!["sep".to_string(), "sep-freewill".to_string()]);
    // The inverse holds: the child id reads back to the parent corpus.
    assert_eq!(
        EvidenceSite::derive("sep-freewill").chunk_corpus().as_str(),
        "sep"
    );

    // A chunk with no title has exactly one candidate — its own corpus.
    assert_eq!(
        candidate_atlas_ids("wikipedia", None),
        vec!["wikipedia".to_string()]
    );
    assert_eq!(
        candidate_atlas_ids("wikipedia", Some("   ")),
        vec!["wikipedia".to_string()]
    );
    // …and a chunk titled after its own corpus yields ONE candidate, not
    // a `bk-1-bk-1` that addresses nothing. This is the literary shape,
    // not a corner case: every chunk of `brothers-karamazov-book-1` is
    // titled with its corpus id.
    assert_eq!(
        candidate_atlas_ids("bk-1", Some("bk-1")),
        vec!["bk-1".to_string()]
    );
}
