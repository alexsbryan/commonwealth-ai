//! Which mechanism actually wrote each RAPTOR summary — read off the
//! nodes' own `summarizer_model` stamps.
//!
//! In [`SummaryMode::Abstractive`](sovereign_tools::raptor_atlas::SummaryMode)
//! extraction is the FALLBACK path: `raptor_atlas` drops to it when the
//! summary LLM call errors, when the response fails to parse, or when the
//! verification gate fails a summary (a verifier that cannot be reached is
//! NOT a pass). Each of those stamps
//! [`EXTRACTIVE_SUMMARIZER`](sovereign_tools::raptor_atlas::EXTRACTIVE_SUMMARIZER),
//! so the tree records its own provenance faithfully.
//!
//! Nothing read those stamps back until 2026-09-21. `raptor-pilot-and-his-wife`
//! persisted 14 nodes of which 13 were verbatim extraction — both level-1
//! nodes among them — and `enrich raptor` printed "documents built: 1 ·
//! nodes persisted: 14" and exited 0. Two RAPTOR boards were read off that
//! tree before anyone looked, measuring source passages against source
//! passages duplicated.

use sovereign_core::conv_tiered::ConvRaptorNodeRow;

/// Split persisted nodes into `(abstractive, extractive)` by the mechanism
/// that wrote their summary.
///
/// Extractive nodes stamp [`EXTRACTIVE_SUMMARIZER`](sovereign_tools::raptor_atlas::EXTRACTIVE_SUMMARIZER);
/// an abstractive node stamps the concrete model stem. A node with an EMPTY
/// stamp is a pre-stamping row ([`ConvRaptorNodeRow::summarizer_model`]) and
/// counts as neither — it predates the distinction, so calling it either way
/// would invent provenance the row does not carry.
pub fn summariser_census(nodes: &[ConvRaptorNodeRow]) -> (usize, usize) {
    let mut abstractive = 0usize;
    let mut extractive = 0usize;
    for n in nodes {
        match n.summarizer_model.as_str() {
            "" => {}
            sovereign_tools::raptor_atlas::EXTRACTIVE_SUMMARIZER => extractive += 1,
            _ => abstractive += 1,
        }
    }
    (abstractive, extractive)
}

/// Accumulate one document's nodes into a run's running totals.
///
/// Every path that leaves a tree in place for a reader — built, resumed, or
/// fresh under `--refresh-stale` — folds its nodes in here, so the run's
/// verdict covers the tree as it will be USED rather than only the part this
/// invocation happened to rebuild.
/// Returns this document's own `(abstractive, extractive)` for per-document
/// reporting, having already folded it into the totals.
pub fn census(
    nodes: &[ConvRaptorNodeRow],
    abstractive: &mut usize,
    extractive: &mut usize,
) -> (usize, usize) {
    let (a, e) = summariser_census(nodes);
    *abstractive += a;
    *extractive += e;
    (a, e)
}

/// Did an abstractive build actually produce an abstractive tree?
///
/// The bar is a MAJORITY rather than "any fallback": occasional extraction is
/// the documented, intended behaviour (`raptor_atlas` — "a verbatim extractive
/// summary is strictly better than no node"), so failing on a single node
/// would contradict the design. What is not intended is the exceptional path
/// becoming the norm.
///
/// A zero-abstractive bar would have passed the build that minted this, which
/// is the whole reason the bar is where it is.
pub fn abstractive_build_is_sound(abstractive: usize, extractive: usize) -> bool {
    extractive <= abstractive
}

/// The census's verdict on a build that ASKED for abstraction: `None` when the
/// tree is sound, otherwise the message explaining why it is not.
///
/// Four verdicts, not two (ARCH §18.1). A census that could not READ every
/// document has not judged anything, and a could-not-judge is owed rather than
/// free — it must not exit 0 wearing the face of a clean build. That case is
/// checked FIRST, because a partial count that happens to look sound is the
/// most persuasive wrong answer available.
///
/// Extractive-mode builds never reach here: there is nothing to fall back from.
pub fn census_refusal(
    corpus_id: &str,
    abstractive: usize,
    extractive: usize,
    unreadable: usize,
    index_path: &std::path::Path,
) -> Option<String> {
    if unreadable > 0 {
        tracing::warn!(
            corpus = corpus_id,
            unreadable,
            "raptor: summariser census could-not-judge — node reads failed"
        );
        return Some(format!(
            "error: the summariser census could not judge this build — {unreadable} \
             document(s) had unreadable nodes (see the warn events for the store errors).\n  \
             {abstractive} model-written and {extractive} extractive nodes were counted from \
             the documents that DID read, but a partial census certifies nothing."
        ));
    }
    if abstractive_build_is_sound(abstractive, extractive) {
        return None;
    }
    tracing::warn!(
        corpus = corpus_id,
        abstractive,
        extractive,
        "raptor: abstractive build came back majority-extractive"
    );
    Some(format!(
        "error: --summary-mode abstractive, but {extractive} of {} persisted nodes fell back \
         to extractive (verbatim source sentences, no model prose).\n  \
         The tree is not an abstractive tree; anything read off it measures extraction.\n  \
         Re-run with RUST_LOG=sovereign_tools::raptor_atlas=warn to see which branch fired \
         (LLM error / parse failure / failed verification).\n  \
         A real rebuild needs the CHECKPOINT gone, not just --force: a manifest with \
         `completed_at` set resumes as \"skipping LLM build\" and replays these same nodes \
         in 0.0s. Delete {}/_raptor_checkpoint first.",
        abstractive + extractive,
        index_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_tools::raptor_atlas::EXTRACTIVE_SUMMARIZER;

    fn node(summarizer: &str) -> ConvRaptorNodeRow {
        ConvRaptorNodeRow {
            node_id: String::new(),
            corpus_id: String::new(),
            conv_uuid: String::new(),
            level: 0,
            summary: String::new(),
            summary_embedding: Vec::new(),
            centroid_embedding: Vec::new(),
            children_node_ids_json: String::new(),
            direct_member_chunk_ids_json: None,
            evidence_chunk_ids_json: String::new(),
            quote_spans_json: String::new(),
            primary_entities_json: String::new(),
            cluster_coherence: 0.0,
            created_at: 0,
            prompt_version: String::new(),
            summarizer_model: summarizer.to_string(),
        }
    }

    #[test]
    fn the_census_reads_the_mechanism_off_the_stamp() {
        let nodes = vec![
            node("Qwopus3.5-4B-v3-MTP-Q8_0"),
            node(EXTRACTIVE_SUMMARIZER),
            node(""), // pre-stamping row: neither, never invented
        ];
        assert_eq!(summariser_census(&nodes), (1, 1));
    }

    /// The shape of the build this gate was minted for:
    /// `raptor-pilot-and-his-wife`, built 2026-09-21T07:27Z — 14 nodes, 13 of
    /// them extractive fallback, one from `Qwopus3.5-4B-v3-MTP-Q8_0`.
    #[test]
    fn a_mostly_extractive_abstractive_build_is_not_sound() {
        let mut nodes = vec![node("Qwopus3.5-4B-v3-MTP-Q8_0")];
        nodes.extend(std::iter::repeat_with(|| node(EXTRACTIVE_SUMMARIZER)).take(13));
        let (abstractive, extractive) = summariser_census(&nodes);
        assert_eq!((abstractive, extractive), (1, 13));
        assert!(!abstractive_build_is_sound(abstractive, extractive));
        assert!(
            abstractive > 0,
            "a zero-abstractive bar would have PASSED this build — hence the majority bar"
        );
    }

    #[test]
    fn an_occasional_fallback_is_intended_and_stays_sound() {
        let mut nodes = vec![node(EXTRACTIVE_SUMMARIZER)];
        nodes.extend(std::iter::repeat_with(|| node("some-model")).take(13));
        let (abstractive, extractive) = summariser_census(&nodes);
        assert!(abstractive_build_is_sound(abstractive, extractive));
    }

    /// An all-extractive tree in ABSTRACTIVE mode is the pure form of the
    /// failure; `SummaryMode::Extractive` builds never reach this check.
    /// An unreadable document outranks a sound-looking count: a partial census
    /// that happens to look fine is the most persuasive wrong answer here.
    #[test]
    fn an_unreadable_document_is_could_not_judge_even_when_the_counts_look_sound() {
        let msg = census_refusal("c", 14, 0, 1, std::path::Path::new("/tmp/x"))
            .expect("a partial census must never certify");
        assert!(msg.contains("could not judge"), "{msg}");
        assert!(
            census_refusal("c", 14, 0, 0, std::path::Path::new("/tmp/x")).is_none(),
            "the same counts with nothing unreadable are sound"
        );
    }

    #[test]
    fn the_refusal_names_the_checkpoint_not_force() {
        let msg = census_refusal("c", 1, 13, 0, std::path::Path::new("/tmp/x")).expect("13>1");
        assert!(msg.contains("_raptor_checkpoint"), "{msg}");
    }

    #[test]
    fn an_entirely_extractive_tree_is_not_sound() {
        let nodes: Vec<_> = std::iter::repeat_with(|| node(EXTRACTIVE_SUMMARIZER))
            .take(14)
            .collect();
        let (abstractive, extractive) = summariser_census(&nodes);
        assert_eq!((abstractive, extractive), (0, 14));
        assert!(!abstractive_build_is_sound(abstractive, extractive));
    }
}
