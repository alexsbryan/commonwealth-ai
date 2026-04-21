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
/// runs. Deliberately generic — we want a cross-section of the corpus,
/// not a topical slice.
pub const SEED_QUERY: &str = "key people decisions dates context";

fn display_score(c: &ScoredChunk, used_sources: &HashSet<String>) -> f32 {
    let len_score = length_score(&c.content);
    let already_used = used_sources.contains(&source_key(c));
    let diversity_weight = if already_used { DIVERSITY_PENALTY } else { 1.0 };
    // We intentionally DON'T multiply by `c.score` (relevance) here —
    // the relevance score orders the candidates coming in; this
    // scorer reorders for display fitness. Mixing them would bias
    // toward highly-relevant-but-too-long chunks.
    len_score * diversity_weight
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
}
