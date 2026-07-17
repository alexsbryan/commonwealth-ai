// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge-level demand-aware set composition — ONE declared objective
//! replacing the heuristic pile (per-article cap → four reserve
//! passes → truncate) that composed the merged pool before it.
//!
//! Why (2026-07-17 bucket-1 forensic, RETRIEVAL_REDESIGN.md): ~10 of
//! 21 missing wiki sources were entities the QUESTION NAMES — their
//! chunks either never entered the pool (fixed by the entity-
//! obligations fetch, `fetch_entity_obligations`) or entered and were
//! ordered out by heuristics that each solve one failure mode and
//! know nothing of the others. Two merge-ordering knobs (front-pull
//! reserve, conditional gap-fill) measured −7 facts and null
//! respectively: ordering heuristics stacked on ordering heuristics
//! trade one question's win for another's loss. This module states
//! the composition objective directly:
//!
//! 1. **Pins** — atom-enum and RAPTOR virtual chunks keep their
//!    existing invariants (they carry no query embedding, sort last,
//!    and die at any rank-based cut; RAPTOR slots are additive on top
//!    of the budget exactly as the legacy truncate treated them).
//! 2. **Demands** — each question-named entity gets its single best
//!    title-matching chunk if the pool holds one. A question that
//!    names an entity has declared what the answer needs; supply is
//!    the obligations fetch's job, presence is guaranteed here.
//! 3. **Greedy diminishing-returns fill** — remaining slots maximise
//!    `1/(rank + RANK_K) · ARTICLE_DECAY^(already-selected-from-
//!    article)`: high-ranked chunks are valuable, every additional
//!    chunk from the same article is worth a constant fraction of the
//!    previous one. Article diversity and within-article depth stop
//!    being competing special cases — they are one curve.
//!
//! Output preserves original rank order (downstream expansion
//! strategies read the ordering as a ranking). Gated by
//! `SOVEREIGN_MERGE_SELECT` while the A/B against the legacy stack
//! runs; the legacy path is byte-identical when the flag is off.

use corpus_engine::ScoredChunk;

/// Rank-value smoothing constant. Deliberately steeper than RRF's
/// k=60 (that heritage is for fusing MANY rank lists; here rank is
/// the single relevance proxy, so adjacent-rank differences must
/// stay meaningful against the article-decay term).
const RANK_K: f64 = 20.0;

/// Value multiplier per already-selected chunk of the same article.
/// With the within-article strength floor below, 0.7 yields the
/// observed good-pool shape: ~3 chunks from the dominant article,
/// 1-2 from each supporting article, breadth in the tail. (Without
/// the floor this constant was bank-contested — sep measured best at
/// 0.7, wiki at 0.75+ — because depth chunks were double-charged:
/// once by their own global rank, once by the decay.)
const ARTICLE_DECAY: f64 = 0.7;

/// `SOVEREIGN_MERGE_SELECT`: demand-aware composition (entity fetch
/// obligations + merge_demand_select) in place of the legacy
/// cap/reserves/truncate stack. Default ON (promoted 2026-07-17
/// after the re-verdict battery: +2 structural sources — Isaac
/// Newton via obligations, news breadth — vs −2 saturated-budget
/// flicker facts; both hard gates held, all canaries byte-identical).
/// "0"/"false"/"off"/"no" restores the legacy stack byte-identically.
pub(crate) fn merge_select_enabled() -> bool {
    match std::env::var("SOVEREIGN_MERGE_SELECT") {
        Ok(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    }
}

/// Whether the entity-obligation lane also fetches GLiNER-extracted
/// CONCEPT articles (lowercase abstract nouns the uppercase-only
/// heuristic can't see — "determinism", "colonialism").
///
/// Ships DARK (default OFF). The lane is correct and fires, but a
/// limit=30 scoreboard A/B (2026-07-17, `eval run --prod-pipeline
/// --isolate --limit 30`) measured ZERO source lift on its entire
/// addressable set — base retrieval already surfaces the concept
/// articles at the real bench limit, so the checkpoint's "lowercase
/// extraction gap" did not reproduce. Kept behind
/// `SOVEREIGN_CONCEPT_OBLIGATIONS=1` for future banks/corpora where
/// base retrieval genuinely misses a named concept.
pub(crate) fn concept_obligations_enabled() -> bool {
    matches!(
        std::env::var("SOVEREIGN_CONCEPT_OBLIGATIONS")
            .ok()
            .as_deref(),
        Some("1" | "true" | "on" | "yes")
    )
}

fn source_tag_is(c: &ScoredChunk, tag: &str) -> bool {
    c.metadata.get("source").map(|s| s == tag).unwrap_or(false)
}

/// Compose the final merged pool: pins + per-entity demand slots +
/// greedy diminishing-returns fill, to `budget` chunks (RAPTOR pins
/// additive on top, mirroring the legacy truncate's `+ raptor_n`).
pub(crate) fn merge_demand_select(
    chunks: Vec<ScoredChunk>,
    entities: &[String],
    budget: usize,
) -> Vec<ScoredChunk> {
    if chunks.len() <= budget || budget == 0 {
        return chunks;
    }
    let n = chunks.len();
    let mut selected = vec![false; n];
    let mut spent = 0usize;

    let title_lower: Vec<String> = chunks
        .iter()
        .map(|c| c.title.as_deref().unwrap_or("").to_lowercase())
        .collect();

    // 1. Pins. RAPTOR is additive (does not consume budget); atom-enum
    //    consumes budget as it did under the legacy reserve.
    for (i, c) in chunks.iter().enumerate() {
        if source_tag_is(c, "raptor") {
            selected[i] = true;
        } else if source_tag_is(c, "atom-enum") && spent < budget {
            selected[i] = true;
            spent += 1;
        }
    }

    // 2. Demand slots: best (earliest-ranked) title-matching chunk per
    //    named entity. Obligation-fetched chunks compete here on rank
    //    like everything else — the guarantee is the entity's
    //    presence, not a particular provenance.
    for entity in entities {
        if spent >= budget {
            break;
        }
        let e = entity.to_lowercase();
        if e.is_empty() {
            continue;
        }
        let already = (0..n).any(|i| selected[i] && title_lower[i].contains(&e));
        if already {
            continue;
        }
        if let Some(i) = (0..n).find(|&i| !selected[i] && title_lower[i].contains(&e)) {
            selected[i] = true;
            spent += 1;
        }
    }

    // 3. Greedy diminishing-returns fill. Article counts start from
    //    the pins + demand slots so the fill sees the whole picture.
    //
    //    A chunk's value is the BETTER of its own global-rank value
    //    and its ARTICLE's best-chunk value (the within-article
    //    strength floor, 2026-07-17 refinement): a load-bearing
    //    article's 4th chunk at global rank 31 is evidence for an
    //    article the pool has already endorsed three times — valuing
    //    it purely by its own rank double-charged depth (once by
    //    rank, once by the decay) and measured −4/−5 wiki depth facts
    //    against the legacy stack. The decay term alone now carries
    //    the depth-vs-breadth trade.
    let mut article_count: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut article_best: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for i in 0..n {
        article_best.entry(title_lower[i].as_str()).or_insert(i);
        if selected[i] {
            *article_count.entry(title_lower[i].as_str()).or_insert(0) += 1;
        }
    }
    while spent < budget {
        let mut best: Option<(usize, f64)> = None;
        for i in 0..n {
            if selected[i] {
                continue;
            }
            let t = title_lower[i].as_str();
            let dup = article_count.get(t).copied().unwrap_or(0);
            let rank_term = 1.0 / (i as f64 + RANK_K);
            let article_term = 1.0
                / (article_best.get(t).copied().unwrap_or(i) as f64 + RANK_K);
            let value = rank_term.max(article_term) * ARTICLE_DECAY.powi(dup as i32);
            if best.map(|(_, bv)| value > bv).unwrap_or(true) {
                best = Some((i, value));
            }
        }
        let Some((i, _)) = best else { break };
        selected[i] = true;
        *article_count.entry(title_lower[i].as_str()).or_insert(0) += 1;
        spent += 1;
    }

    let kept: Vec<ScoredChunk> = chunks
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| selected[i].then_some(c))
        .collect();
    tracing::info!(
        target: "retrieval_audit",
        event = "merge_select",
        pool = n,
        kept = kept.len(),
        entities = entities.len(),
        "retrieval_audit: merge_select"
    );
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn chunk(title: &str, tag: Option<&str>) -> ScoredChunk {
        let mut metadata = HashMap::new();
        if let Some(t) = tag {
            metadata.insert("source".to_string(), t.to_string());
        }
        ScoredChunk {
            content: format!("body of {title}"),
            title: Some(title.to_string()),
            url: None,
            corpus_id: "test".to_string(),
            score: 1.0,
            metadata,
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn passthrough_when_pool_fits() {
        let pool: Vec<_> = (0..5).map(|i| chunk(&format!("A{i}"), None)).collect();
        let out = merge_demand_select(pool, &[], 10);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn named_entity_absent_from_topk_gets_its_slot() {
        // 25 chunks of one dominant article, the entity's chunk last.
        let mut pool: Vec<_> = (0..25).map(|_| chunk("Dominant Article", None)).collect();
        pool.push(chunk("Isaac Newton", None));
        let out = merge_demand_select(pool, &["Newton".to_string()], 10);
        assert!(out.iter().any(|c| c.title.as_deref() == Some("Isaac Newton")));
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn diminishing_returns_diversifies_without_starving_depth() {
        // 20 chunks of article A then 10 distinct articles: the fill
        // must keep several A chunks (depth) AND all-distinct articles
        // it can afford (breadth) — not 10×A, not 1×A.
        let mut pool: Vec<_> = (0..20).map(|_| chunk("A", None)).collect();
        for i in 0..10 {
            pool.push(chunk(&format!("B{i}"), None));
        }
        let out = merge_demand_select(pool, &[], 10);
        let a_count = out
            .iter()
            .filter(|c| c.title.as_deref() == Some("A"))
            .count();
        assert!((2..=5).contains(&a_count), "a_count = {a_count}");
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn raptor_pins_are_additive_and_atom_enum_consumes_budget() {
        let mut pool: Vec<_> = (0..30).map(|i| chunk(&format!("T{i}"), None)).collect();
        pool.push(chunk("summary", Some("raptor")));
        pool.push(chunk("atoms", Some("atom-enum")));
        let out = merge_demand_select(pool, &[], 10);
        assert!(out.iter().any(|c| c.title.as_deref() == Some("summary")));
        assert!(out.iter().any(|c| c.title.as_deref() == Some("atoms")));
        // 10 budget slots (atom-enum inside) + 1 additive raptor pin.
        assert_eq!(out.len(), 11);
    }

    #[test]
    fn output_preserves_rank_order() {
        let mut pool: Vec<_> = (0..30).map(|i| chunk(&format!("T{i}"), None)).collect();
        pool.push(chunk("Zed", None));
        let out = merge_demand_select(pool, &["Zed".to_string()], 10);
        let titles: Vec<_> = out.iter().map(|c| c.title.clone().unwrap()).collect();
        let mut sorted = titles.clone();
        sorted.sort_by_key(|t| {
            if t == "Zed" {
                usize::MAX
            } else {
                t[1..].parse::<usize>().unwrap()
            }
        });
        assert_eq!(titles, sorted);
    }
}
