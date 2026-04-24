//! Pick 3 representative chunks from a freshly-ingested corpus to
//! display on the completion screen (spec §5.4).
//!
//! The chunks must:
//!   1. Be short enough to fit on screen (length score centred on 140
//!      tokens, linear falloff).
//!   2. Come from different source documents where possible (diversity
//!      penalty).
//!   3. Be shown verbatim — no summarisation, no paraphrasing.
//!
//! This is a *display* scorer, not a relevance scorer. It assumes the
//! input is already the top-N search results for a generic seed query
//! ("key people decisions dates"). The manager does that search and
//! hands the result here.

use std::collections::HashSet;

use corpus_engine::ScoredChunk;

use super::progress::ExcerptChunk;

const TARGET_TOKENS: f32 = 140.0;
const TOKEN_TOLERANCE: f32 = 140.0;
const DIVERSITY_PENALTY: f32 = 0.2;

/// Content-quality bias — nudges the scorer away from boilerplate
/// (meta-text that points at the real content without containing
/// it) and toward claim-dense chunks (self-contained sentences
/// that say something).
///
/// Meta-markers like "this chapter" / "the summary information uses
/// key figures from the other chapters" are a strong smell that the
/// paragraph is scaffolding, not substance — the paragraph that
/// triggered the rewrite was exactly that shape and kept getting
/// picked on length alone.
const BOILERPLATE_MARKERS: &[&str] = &[
    "this chapter",
    "this volume",
    "this book",
    "this report",
    "this paper",
    "this article",
    "these results are",
    "the summary information",
    "the following table",
    "the following figure",
    "table of contents",
    "as discussed in",
    "as described in",
    "as shown in figure",
    "as shown in table",
    "chapters a",
    "chapters b",
    "chapters c",
    "appendix ",
    "copyright ",
    "all rights reserved",
    "acknowledgments",
    "references",
    "bibliography",
];

/// Claim-markers — nudges toward paragraphs that say something
/// rather than describe structure. Phrases, not single words, so
/// we don't over-match innocent uses of "shows" or "finds".
const CLAIM_MARKERS: &[&str] = &[
    "argues that",
    "shows that",
    "demonstrates that",
    "proves that",
    "concludes that",
    "finds that",
    "suggests that",
    "establishes that",
    "reveals that",
    "contends that",
    "the main argument",
    "the central claim",
    "the central thesis",
    "the key finding",
    "the main finding",
    "contradicts",
    "challenges the",
    "refutes",
    "counter to",
    "in contrast to",
];

const BOILERPLATE_PENALTY: f32 = 0.35;
const CLAIM_BOOST: f32 = 1.45;

/// Pick up to 3 excerpts from `candidates`, ranked by a length +
/// diversity score. `candidates` should be the top ~20 search hits;
/// anything larger just slows scoring for marginal improvement.
pub fn select_excerpts(candidates: &[ScoredChunk]) -> Vec<ExcerptChunk> {
    let mut chosen: Vec<&ScoredChunk> = Vec::new();
    let mut used_sources: HashSet<String> = HashSet::new();

    // Greedy top-3: on each pass, pick the highest-scoring candidate
    // that isn't already chosen. Diversity penalty re-weights each
    // pass.
    let mut remaining: Vec<&ScoredChunk> = candidates.iter().collect();
    while chosen.len() < 3 && !remaining.is_empty() {
        let (best_idx, _) = remaining
            .iter()
            .enumerate()
            .map(|(i, c)| (i, display_score(c, &used_sources)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .expect("remaining is non-empty");
        let winner = remaining.remove(best_idx);
        used_sources.insert(source_key(winner));
        chosen.push(winner);
    }

    chosen
        .into_iter()
        .map(|c| ExcerptChunk {
            text: c.content.clone(),
            source_name: c
                .title
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            page_ref: None, // PDFs carry no page metadata in today's pipeline.
        })
        .collect()
}

/// The seed query handed to `CorpusIndex::search` before this scorer
/// runs. Biased toward claim-dense content — the completion screen
/// wants "look, here's what your corpus says" not "look, here's
/// some filler". A generic query ("key people decisions dates")
/// pulled methodology and ToC paragraphs too often.
pub const SEED_QUERY: &str =
    "main argument central thesis key finding conclusion what matters";

fn display_score(c: &ScoredChunk, used_sources: &HashSet<String>) -> f32 {
    let len_score = length_score(&c.content);
    let already_used = used_sources.contains(&source_key(c));
    let diversity_weight = if already_used { DIVERSITY_PENALTY } else { 1.0 };
    let quality = content_quality_bias(&c.content);
    // We intentionally DON'T multiply by `c.score` (relevance) here —
    // the relevance score orders the candidates coming in; this
    // scorer reorders for display fitness. Mixing them would bias
    // toward highly-relevant-but-too-long chunks.
    len_score * diversity_weight * quality
}

/// Inspect the first ~600 chars (where boilerplate/thesis signals
/// live) for markers; combine into a single multiplier.
pub(crate) fn content_quality_bias(text: &str) -> f32 {
    // Lowercase once, prefix only — we don't need to scan 2000
    // chars to spot "this chapter" at the opener.
    let head_len = text.len().min(640);
    let head = text[..head_len].to_ascii_lowercase();

    let mut bias: f32 = 1.0;
    if BOILERPLATE_MARKERS.iter().any(|m| head.contains(m)) {
        bias *= BOILERPLATE_PENALTY;
    }
    if CLAIM_MARKERS.iter().any(|m| head.contains(m)) {
        bias *= CLAIM_BOOST;
    }
    bias
}

pub(crate) fn length_score(content: &str) -> f32 {
    let tokens = approx_token_count(content) as f32;
    let distance = (tokens - TARGET_TOKENS).abs();
    (1.0 - distance / TOKEN_TOLERANCE).max(0.0)
}

fn approx_token_count(text: &str) -> usize {
    // Rough English approximation: one token ≈ 0.75 words. Splitting
    // on whitespace + scaling gives us close enough for the scoring
    // heuristic. We deliberately don't tokenise with a real BPE —
    // this is a length-based fitness function, not a budget check.
    let words = text.split_whitespace().count();
    (words as f32 / 0.75).round() as usize
}

fn source_key(c: &ScoredChunk) -> String {
    c.title.clone().unwrap_or_else(|| c.corpus_id.clone())
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(content: &str, title: &str, score: f32) -> ScoredChunk {
        ScoredChunk {
            content: content.into(),
            title: Some(title.into()),
            url: None,
            corpus_id: "test".into(),
            score,
            metadata: Default::default(),
        }
    }

    fn words(n: usize) -> String {
        (0..n).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn length_score_peaks_near_target() {
        // ~140 tokens = 140 * 0.75 ≈ 105 words, but our approximator
        // inverts that: 105 words / 0.75 = 140 tokens → score ≈ 1.0.
        let on_target = words(105);
        let far_from_target = words(500);
        assert!(length_score(&on_target) > length_score(&far_from_target));
        assert!(length_score(&on_target) > 0.95);
    }

    #[test]
    fn length_score_clamps_to_zero_for_very_long() {
        // A 400-token chunk has distance = 260, > tolerance = 140,
        // so the score clamps to 0.
        let huge = words(400);
        assert_eq!(length_score(&huge), 0.0);
    }

    #[test]
    fn length_score_clamps_to_zero_for_empty() {
        // 0 tokens → distance = 140, ratio = 1.0, score = 0.
        assert_eq!(length_score(""), 0.0);
    }

    #[test]
    fn prefers_chunks_from_different_sources() {
        let candidates = vec![
            chunk(&words(105), "Alpha", 0.9),
            chunk(&words(105), "Alpha", 0.8), // same source
            chunk(&words(105), "Beta", 0.7),
            chunk(&words(105), "Gamma", 0.6),
        ];
        let picks = select_excerpts(&candidates);
        assert_eq!(picks.len(), 3);
        // The second Alpha should NOT be in the picks — Beta and
        // Gamma's length scores tie with Alpha but they win due to
        // the diversity bonus.
        let sources: Vec<_> = picks.iter().map(|p| p.source_name.as_str()).collect();
        assert!(sources.contains(&"Alpha"));
        assert!(sources.contains(&"Beta"));
        assert!(sources.contains(&"Gamma"));
    }

    #[test]
    fn returns_fewer_than_three_when_input_is_short() {
        let candidates = vec![chunk(&words(105), "Alpha", 1.0)];
        let picks = select_excerpts(&candidates);
        assert_eq!(picks.len(), 1);
    }

    #[test]
    fn returns_empty_for_empty_input() {
        let picks = select_excerpts(&[]);
        assert!(picks.is_empty());
    }

    #[test]
    fn boilerplate_chunks_lose_to_claim_dense_chunks_of_similar_length() {
        // The water-quality-report bug: a methodology paragraph that
        // points at findings (but doesn't contain them) beat a
        // self-contained thesis on length alone. The content-quality
        // bias flips the ordering without needing a relevance score.
        let boilerplate =
            "These results are then placed in further context by \
             considering the relative degradation of surface water \
             and groundwater and streamflow alteration. The summary \
             information uses key figures from the other chapters \
             of this volume to illustrate major findings across the \
             regional assessment framework used in the report.";
        let claim =
            "Schrödinger argues that life sustains order by feeding \
             on negative entropy drawn from its environment, and \
             that the apparent violation of the second law within \
             a living cell is accounted for by the thermodynamic \
             flow across the cell's boundary with its surroundings.";

        let candidates = vec![
            chunk(boilerplate, "Water Quality", 0.9),
            chunk(claim, "What Is Life", 0.8),
        ];
        let picks = select_excerpts(&candidates);
        assert_eq!(picks.len(), 2);
        // The claim-dense chunk should be first — the ordering
        // is what shows up as the primary excerpt on screen.
        assert_eq!(
            picks[0].source_name, "What Is Life",
            "claim-dense chunk should win display ordering over boilerplate"
        );
    }

    #[test]
    fn content_quality_bias_penalises_and_boosts_as_expected() {
        let meta = "This chapter surveys the methodology used \
                    across the following sections.";
        let thesis = "The author argues that the conventional view \
                      misreads the evidence.";
        let plain = "The committee met on Tuesday to review the \
                     proposed revisions to the plan.";

        assert!(content_quality_bias(meta) < 1.0);
        assert!(content_quality_bias(thesis) > 1.0);
        // Plain text with no markers stays at the 1.0 baseline.
        assert!((content_quality_bias(plain) - 1.0).abs() < 1e-4);
    }
}
