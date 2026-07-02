// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retrieval-pipeline helpers.
//!
//! Two families live here:
//!
//! 1. **Pre-search shaping** — `collect_hot_corpora`, `build_per_corpus_k_overrides`,
//!    and `build_retrieval_query`. These read conversation state to bias the
//!    cross-corpus search before it runs (widen the K for hot corpora, prepend
//!    the topic anchor to the embedding query).
//!
//! 2. **Post-search shaping** — `cross_corpus_sort_cmp`, `reweight_by_query_relevance`,
//!    `inject_meta_atlas_hits`, `drop_no_overlap_chunks`, and `atlas_grounding_enabled`.
//!    These run over the merged chunk pool to drop noise, re-weight by
//!    query relevance, inject canonical-entity hits, and gate atlas grounding.
//!
//! Both families depend on `extract_tokens` + `EVIDENCE_TITLE_MIN_TOKEN_LEN`
//! from `super::evidence` — query-token analysis is the shared primitive.

use std::collections::HashMap;

use corpus_engine::ScoredChunk;

use crate::types::{ConversationContext, Message, Role};

use super::evidence::{extract_tokens, EVIDENCE_TITLE_MIN_TOKEN_LEN};
use super::text_utils::truncate_with_ellipsis;

/// Char budget for the topic anchor prepended to the retrieval
/// query. Topic strings from `update_topic_context` are short by
/// design (the Fast-slot extractor targets a 3-12 word phrase) but
/// we cap defensively in case the classifier returns a longer
/// label.
pub(crate) const RETRIEVAL_QUERY_TOPIC_CHARS: usize = 120;

/// Pre-merge K boost ceiling. Hot corpora — those the conversation
/// has already drawn from — get their per-corpus retrieval K
/// scaled up by `share * range` on top of the base K. A corpus
/// that supplied 60% of past chunks gets K = base + 30 candidates
/// in the next turn's pool; a corpus at the share floor gets the
/// base K.
///
/// Pre-merge (not post-merge) because the cross-corpus merge filter
/// is where wikipedia chunks were getting dropped in the marathon
/// bench — post-multiplier on the merged result couldn't recover
/// what merge had already filtered out. Surfaced by
/// `sovereign/bench/wikipedia_learn` 2026-05-17 (v12 / v13 → v14):
/// retrieval-only single-shot returned 5 Ada Lovelace chunks at
/// 100% recall, but the synth path's KQ_PER_CORPUS_LIMIT=20 cap
/// across 10+ competing corpora left wikipedia with only 2 slots
/// in the merged set, neither of them Lovelace.
pub(crate) const HOT_CORPUS_K_RANGE: usize = 50;

/// Minimum share before a corpus gets a pre-merge K boost. Below
/// the floor the corpus uses base K — keeps long-tail contaminants
/// (one accidental chunk in turn 2) from inflating their pool. A
/// corpus that contributed roughly one chunk per ten across the
/// conversation (~10%) is the threshold.
pub(crate) const HOT_CORPUS_MIN_SHARE: f32 = 0.10;

/// Hard cap on how many corpora can receive a pre-merge K boost.
/// Each boost adds search work; without a cap, a conversation that
/// has touched many corpora could fan out to all of them and pay
/// 20-corpus × K=50 = 1000-chunk worst-case search latency. Three
/// corpora is the cap because the synth prompt routinely gets one
/// dominant corpus plus 1-2 supporting tiers — beyond that the
/// extra candidates don't survive the merge anyway.
pub(crate) const HOT_CORPUS_MAX_BOOSTED: usize = 3;

/// Build a histogram of `corpus_id` usage from prior assistant
/// turns in the conversation. Reads `metadata.retrieved_chunks` —
/// the same field the desktop reading-surface consumes for
/// citations — so the histogram reflects what was actually shown
/// to the user, not what was speculatively retrieved.
///
/// Cold conversations (no prior assistant turn with retrieved
/// chunks) return an empty map and the boost is a no-op. As the
/// conversation accretes around a topic, the corpora that served
/// the user well accumulate hits and start to outweigh
/// off-domain matches from the user's other installed indexes.
/// Surfaced by `sovereign/bench/wikipedia_learn` 2026-05-17:
/// einstein chain T2-T4 needed Wikipedia weighting against the
/// user's own code/vault corpora that were out-ranking the
/// wikipedia articles on bare-keyword overlap.
pub(crate) fn collect_hot_corpora(messages: &[Message]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for m in messages {
        if m.role != Role::Assistant {
            continue;
        }
        let Some(metadata) = m.metadata.as_ref() else {
            continue;
        };
        let Some(chunks) = metadata.get("retrieved_chunks").and_then(|v| v.as_array()) else {
            continue;
        };
        for c in chunks {
            if let Some(cid) = c.get("corpus_id").and_then(|v| v.as_str()) {
                if !cid.is_empty() {
                    *counts.entry(cid.to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

/// Build a per-corpus K override map from the hot-corpora
/// histogram. Corpora with a high share of prior conversation
/// chunks get their retrieval K scaled up so they contribute more
/// candidates to the cross-corpus merge layer. Returns `None`
/// when the histogram is empty (no prior turns) or no corpus
/// clears `HOT_CORPUS_MIN_SHARE`.
///
/// Replaces an earlier post-merge score-multiplier approach
/// (v11/v12/v13). Post-merge can't recover candidates the merge
/// has already dropped — and on conversations with many competing
/// corpora, the merge dropped exactly the hot ones the user was
/// learning from. Pre-merge widens the pool, letting the merge
/// see the strong candidates it would otherwise filter out.
pub(crate) fn build_per_corpus_k_overrides(
    hot_corpora: &HashMap<String, usize>,
    base_k: usize,
) -> Option<HashMap<String, usize>> {
    if hot_corpora.is_empty() {
        return None;
    }
    let total: usize = hot_corpora.values().sum();
    if total == 0 {
        return None;
    }
    let total_f = total as f32;
    // Sort by share descending so we pick the top-N corpora to
    // boost. Cap at HOT_CORPUS_MAX_BOOSTED to bound the per-turn
    // search cost. Ties (rare with integer counts on long arcs)
    // break by name.
    let mut ranked: Vec<(&String, f32)> = hot_corpora
        .iter()
        .map(|(k, &v)| (k, v as f32 / total_f))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    let mut overrides: HashMap<String, usize> = HashMap::new();
    let top_corpus = ranked
        .first()
        .map(|(k, s)| ((*k).clone(), *s))
        .unwrap_or_default();
    for (corpus_id, share) in ranked.into_iter().take(HOT_CORPUS_MAX_BOOSTED) {
        if share < HOT_CORPUS_MIN_SHARE {
            break;
        }
        let boost = (share * HOT_CORPUS_K_RANGE as f32).round() as usize;
        overrides.insert(corpus_id.clone(), base_k + boost);
    }
    if overrides.is_empty() {
        return None;
    }
    let (top_id, top_share) = top_corpus;
    tracing::info!(
        hot_corpora_count = hot_corpora.len(),
        boosted_count = overrides.len(),
        top_corpus = %top_id,
        top_corpus_share = top_share,
        base_k,
        "retrieval: built per-corpus K overrides for hot corpora"
    );
    Some(overrides)
}

/// Build the query string used for *retrieval embedding*.
///
/// When the conversation has an established topic
/// (`ConversationContext::topic_context.topic`), prepend it to the
/// embedding text so follow-up turns inherit the conversation's
/// anchor. The topic is already maintained by
/// `context::update_topic_context` (a per-turn Fast-slot
/// extraction designed specifically to disambiguate follow-up
/// questions) — we are *consuming* that signal at retrieval time,
/// not introducing new heuristics. When the topic is absent
/// (turn 0, fresh conversation, classifier declined to extract),
/// the bare message is used unchanged.
///
/// This is the principled answer to a follow-up like "What did he
/// publish in 1905?" — the topic extractor sees Einstein in the
/// arc and writes `topic = "Albert Einstein"`; the retrieval query
/// becomes "Albert Einstein: What did he publish in 1905?" so the
/// embedder lands on Einstein-relevant chunks without the bench
/// author writing any per-domain rule.
///
/// Affects the embedding only. The downstream BM25 / keyword leg
/// of `search_corpus_indexes` still receives the bare `message`.
pub(crate) fn build_retrieval_query(message: &str, context: &ConversationContext) -> String {
    let topic_opt = context
        .topic_context
        .as_ref()
        .and_then(|tc| tc.topic.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    tracing::info!(
        has_topic_context = context.topic_context.is_some(),
        topic = ?topic_opt,
        prior_messages = context.conversation.messages.len(),
        message_chars = message.len(),
        "retrieval: build_retrieval_query"
    );
    let Some(topic) = topic_opt else {
        return message.to_string();
    };
    let anchor = truncate_with_ellipsis(topic, RETRIEVAL_QUERY_TOPIC_CHARS);
    format!("{anchor}: {}", message.trim())
}

/// Cross-corpus merge sort key.
///
/// **Primary**: `vector_distance` (asc) — raw cosine distance from
/// the query embedding to the chunk's stored embedding. This is the
/// only signal that's apples-to-apples across different corpora,
/// because every other score (`_relevance_score` RRF, `_score` BM25)
/// is a per-index reranker whose scale depends on the corpus's own
/// rank distribution. Without this, a small corpus's top-1 hit
/// (RRF ≈ 0.033) beats a large corpus's semantically-better answer
/// that landed at rank-1 in only one of (vector, FTS) and so got
/// RRF ≈ 0.017. Real symptom (2026-05-03): SEP "compatibilism"
/// dropped out of the top 8 for "Is free will compatible with
/// determinism?" while conversation-history echoes of the user's
/// own past probes won.
///
/// **Fallback**: RRF `score` (desc) for chunks that have no
/// `vector_distance` — FTS-only paths, mesh-served hits whose
/// remote search didn't include the embedding column, synthetic
/// atlas-virtual chunks. Chunks with a real `vector_distance`
/// always rank above chunks without (None is treated as +infinity).
pub(crate) fn cross_corpus_sort_cmp(a: &ScoredChunk, b: &ScoredChunk) -> std::cmp::Ordering {
    match (a.vector_distance, b.vector_distance) {
        (Some(ad), Some(bd)) => ad.partial_cmp(&bd).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b
            .score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal),
    }
}

/// Reweight every chunk's `score` by how much of the query it
/// actually matches in its title + body, then leave the result on
/// the same comparable scale across corpora.
///
/// Replaces an earlier `normalise_scores_per_corpus` that divided
/// each corpus's chunks by *that corpus's* max score. The old form
/// was fine for raw BM25 (where IDF differences across corpus sizes
/// can make a small-corpus outlier outscore a real match elsewhere)
/// but wrong for the RRF-fused scores that corpus-engine's hybrid
/// search actually returns: RRF rank-1 across corpora ALREADY has
/// the same score (~0.033 with k=60), so per-corpus normalisation
/// equalised every corpus's top hit to 1.0 and destroyed
/// cross-corpus ranking. Observed in practice: a sep-al-farabi
/// philosophy chunk and a Wikipedia "Operation Barbarossa" chunk
/// both ended up at score 1.0 for the query "Why did Operation
/// Barbarossa fail?", and the merge sort flooded the prompt with
/// off-domain SEP entries.
///
/// The reweight signal here is the same `extract_tokens` filter the
/// off-target gate uses — substantive ≥ 4-char tokens, stopwords
/// dropped — applied separately to each chunk's title and body.
/// Title overlap counts double, since a title-token match is the
/// strongest evidence that retrieval landed on the right document.
/// A chunk with neither title nor content overlap with the query
/// keeps its raw RRF score and naturally sinks; a chunk that
/// genuinely matches the query rises.
///
/// Trade-off: the substring `contains` check on content can fire on
/// false positives (e.g. "operation" matches "operationalism"). The
/// title-overlap term uses token equality (no substring) so the
/// dominant signal stays clean; content_overlap is a weaker
/// secondary boost that doesn't outweigh title alone.
pub(crate) fn reweight_by_query_relevance(chunks: &mut [ScoredChunk], query: &str) {
    let query_tokens = extract_tokens(query, EVIDENCE_TITLE_MIN_TOKEN_LEN);
    if query_tokens.is_empty() {
        // All-stopword or all-short-token query (rare). Nothing to
        // reweight against — leave RRF order intact and trust the
        // off-target gate downstream.
        return;
    }
    let qn = query_tokens.len() as f32;
    for c in chunks.iter_mut() {
        let title = c.title.as_deref().unwrap_or("");
        let title_tokens = extract_tokens(title, EVIDENCE_TITLE_MIN_TOKEN_LEN);
        let title_overlap = if title_tokens.is_empty() {
            0.0_f32
        } else {
            let hits = query_tokens
                .iter()
                .filter(|q| title_tokens.iter().any(|t| t == *q))
                .count();
            hits as f32 / qn
        };
        let content_lower = c.content.to_lowercase();
        let content_hits = query_tokens
            .iter()
            .filter(|q| content_lower.contains(q.as_str()))
            .count();
        let content_overlap = content_hits as f32 / qn;
        // Title double-weight + content single-weight, additive into a
        // [0, 3]-bounded multiplier. A chunk with full title overlap
        // and full content overlap gets a 4x boost; a chunk with
        // nothing relevant stays at 1x.
        let relevance = 2.0 * title_overlap + content_overlap;
        c.score *= 1.0 + relevance;
        // Code-intelligence-in-chat: a code-intel SUMMARY chunk is the curated
        // user-vocabulary bridge to a symbol (and the call-graph trace anchor),
        // worth far more for a "how does X work / what calls it" question than a
        // raw code chunk — and raw code chunks, vastly more numerous in a code
        // corpus, otherwise crowd the summaries below the trace window (measured
        // in the Inc 3/4 grade: gate_held_answer's summary fell to ~#11). Promote
        // them so the bridge + trace engage. This is SELF-GATING by relevance:
        // it scales the already-relevance-weighted score, so a summary with no
        // query overlap stays low (3x a small score is small) — only a summary
        // that actually matches the query is lifted. No-op for non-code corpora,
        // which carry no such chunks.
        if c.metadata.get("source").map(String::as_str) == Some(CODE_INTEL_SUMMARY_SOURCE) {
            c.score *= CODE_INTEL_SUMMARY_BOOST;
            // The CROSS-CORPUS sort orders by `vector_distance`, not `score`
            // (see `cross_corpus_sort_cmp` — it only falls back to `score` when
            // both distances are `None`), so a score-only boost is invisible to
            // the merged ranking. Promote the summary on the actual sort key too:
            // pull its distance toward the query so a relevant summary clears the
            // raw-chunk crowd. `None` (FTS-only paths) keeps the score boost above.
            if let Some(d) = c.vector_distance.as_mut() {
                *d *= CODE_INTEL_SUMMARY_DISTANCE_FACTOR;
            }
        }
    }
}

/// Metadata marker `code_intel::store` stamps on every per-symbol summary chunk.
const CODE_INTEL_SUMMARY_SOURCE: &str = "code_intel_summary";

/// Relevance-stacked boost for code-intel summary chunks (see
/// [`reweight_by_query_relevance`]). Applied to `score` for the score-based gates
/// (off-target / grounding); the cross-corpus *sort* is handled by the distance
/// factor below. Self-gating — it multiplies the already-relevance-weighted
/// score, never a flat add.
const CODE_INTEL_SUMMARY_BOOST: f32 = 3.0;

/// Multiplier on a code-intel summary chunk's `vector_distance` — the key
/// `cross_corpus_sort_cmp` actually sorts by. `< 1.0` pulls the summary closer to
/// the query so a relevant one ranks above the (far more numerous) raw code
/// chunks that would otherwise bury it. 0.6 ≈ a 40% pull, enough to clear the
/// observed ~#11 gap without ignoring genuine distance differences.
const CODE_INTEL_SUMMARY_DISTANCE_FACTOR: f32 = 0.6;

/// Cross-corpus retrieval discipline (env-gated prototype, default OFF — a
/// no-op unless at least one knob is set, so production behaviour is
/// byte-identical).
///
/// After the flat cross-corpus merge, an N-corpus fan-out pours
/// N×per-corpus-limit chunks into one pool (33 corpora × 20 ≈ 660). The
/// cross-corpus sort is primarily by cosine distance, so a single relevant
/// chunk in one corpus competes with every other corpus's top-K — and at that
/// scale, spurious-low-distance noise from the other 32 corpora can bury it
/// below the merged truncation or below the grounding gate's support threshold.
/// That is the observed "I couldn't find a matching internal source" decline on
/// answerable questions when many corpora are installed; scoping to the one
/// relevant corpus makes it answerable. Two cheap, structural corrections, each
/// independently gated:
///
///   * PER-CORPUS CAP (`SOVEREIGN_KQ_PER_CORPUS_CAP`): keep at most `cap` chunks
///     per corpus. Applied AFTER the cross-corpus sort, so `retain` keeps each
///     corpus's top-`cap` by cross-corpus rank and drops the long tail that
///     only adds cross-corpus noise.
///   * CROSS-CORPUS RELATIVE-SCORE FLOOR (`SOVEREIGN_KQ_XCORPUS_FLOOR`, 0..1):
///     drop chunks whose cosine similarity (`1 - vector_distance`, the one
///     signal that IS comparable across corpora) is below `floor ×
///     best_similarity`. FTS-only chunks (no `vector_distance`) are a different
///     signal and are kept.
///
/// Pure + glassbox: emits a `cross_corpus_discipline` audit event with the
/// before/after counts and the similarity cutoff so a chaos/bench re-run can
/// SEE the effect against the existing `merged_pool` audit.
pub(crate) fn apply_cross_corpus_discipline(chunks: &mut Vec<ScoredChunk>, label: &str) {
    let cap = std::env::var("SOVEREIGN_KQ_PER_CORPUS_CAP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0);
    let floor = std::env::var("SOVEREIGN_KQ_XCORPUS_FLOOR")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|f| *f > 0.0 && *f < 1.0);
    if cap.is_none() && floor.is_none() {
        return; // production default — no behaviour change
    }
    let before = chunks.len();

    // 1. Per-corpus cap — applied post-sort, so retain keeps each corpus's
    //    top-`cap` by cross-corpus rank.
    if let Some(cap) = cap {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        chunks.retain(|c| {
            let n = seen.entry(c.corpus_id.clone()).or_insert(0);
            *n += 1;
            *n <= cap
        });
    }
    let after_cap = chunks.len();

    // 2. Cross-corpus relative-score floor on cosine similarity — the only
    //    cross-corpus-comparable signal. FTS-only chunks are kept (different
    //    signal, not floored out).
    let mut sim_cutoff = 0.0_f32;
    if let Some(floor) = floor {
        let best_sim = chunks
            .iter()
            .filter_map(|c| c.vector_distance.map(|d| 1.0 - d))
            .fold(f32::NEG_INFINITY, f32::max);
        if best_sim.is_finite() {
            sim_cutoff = floor * best_sim;
            chunks.retain(|c| match c.vector_distance {
                Some(d) => (1.0 - d) >= sim_cutoff,
                None => true,
            });
        }
    }

    if tracing::enabled!(target: "retrieval_audit", tracing::Level::INFO) {
        tracing::info!(
            target: "retrieval_audit",
            event = "cross_corpus_discipline",
            label = label,
            cap = ?cap,
            floor = ?floor,
            sim_cutoff = sim_cutoff,
            before = before,
            after_cap = after_cap,
            after = chunks.len(),
            "retrieval_audit: cross_corpus_discipline"
        );
    }
}

/// Blend a topic-anchor embedding with the live query embedding so a
/// cross-corpus fetch is steered by *what the user actually asked*, not
/// only by the bridged topic. `anchor` (the linked topic's embedding)
/// keeps the pull inside the right region of the other corpus so it
/// can't wander into unrelated material; `query` then selects the chunk
/// that answers *this* question within that region.
///
/// `anchor_weight ∈ [0, 1]` is the topic-vs-query mix (`0.5` = equal,
/// `1.0` = the prior topic-only behaviour). The result is L2-normalised:
/// an ANN ranks on direction, so the renorm doesn't change ranking — it
/// keeps the vector tidy for any downstream cosine and makes the blend
/// independent of the two inputs' magnitudes.
///
/// Defensive fallback: returns `anchor` verbatim when the query embedding
/// is empty or dimension-mismatched, so a missing/garbled query
/// embedding is never *worse* than the topic-only fetch it replaces.
pub(crate) fn blend_query_aware(anchor: &[f32], query: &[f32], anchor_weight: f32) -> Vec<f32> {
    if query.is_empty() || query.len() != anchor.len() {
        return anchor.to_vec();
    }
    let qw = 1.0 - anchor_weight;
    let mut out: Vec<f32> = anchor
        .iter()
        .zip(query)
        .map(|(a, q)| anchor_weight * a + qw * q)
        .collect();
    let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut out {
            *x /= norm;
        }
    }
    out
}

/// Inject canonical-entity boost hits into the merge bag. Each newly
/// injected chunk gets a small score lift above `top_score` so it
/// survives `chunks.truncate(KQ_MERGED_LIMIT)`. Existing chunks with
/// the same `(corpus_id, chunk_id)` are skipped (no double-injection)
/// but counted as "displaced by their score-lift sibling" rather than
/// added — the merge still has them, and the upstream caller still
/// gets credit via the eventual title-coverage signal.
///
/// `rank` is the running boost rank; mutated so successive calls keep
/// their relative ordering.
///
/// Returns the count of meta-atlas-anchored chunks in the bag for
/// this batch — counts both freshly injected chunks AND already-
/// present chunks whose score got lifted to the boost band.
///
/// `articulation` and `stability` (when present) are written into
/// each affected chunk's `metadata` so the synthesis-prompt
/// formatter ([`super::format_scored_chunks_with_kinds`]) can sub-section
/// the corpus bucket by stream.
///
/// Move 5.1: already-present chunks now get their score lifted +
/// metadata tagged in place, rather than being skipped. Reason:
/// the meta-atlas anchor's canonical chunk is often already at
/// cosine-top when the corpus is wiki-shaped and dominant; skipping
/// silently made the boost a no-op in single-corpus deployments.
/// The re-rank ensures the synthesis-prompt formatter sees the
/// stream tag and sectioning applies regardless of whether the
/// chunk was net-new or cosine-discovered.
pub(crate) fn inject_meta_atlas_hits(
    chunks: &mut Vec<ScoredChunk>,
    hits: Vec<ScoredChunk>,
    expected_corpus: &str,
    articulation: &str,
    stability: Option<&str>,
    top_score: f32,
    rank: &mut usize,
) -> usize {
    if hits.is_empty() {
        return 0;
    }
    let mut affected = 0usize;
    for hit in hits {
        if hit.corpus_id != expected_corpus {
            // search_corpora_filtered's name_match is substring;
            // tighten to exact corpus_id here so a stray match
            // from a wider sibling doesn't sneak in.
            continue;
        }
        *rank += 1;
        // Above any current chunk by a deterministic margin. The
        // 1e-4 floor leaves room for reweight_by_query_relevance
        // (multiplies by up to 4×) without rank reshuffling within
        // the meta-atlas cohort.
        let lifted_score = top_score + 1e-4 * (*rank as f32);
        if let Some(existing) = chunks.iter_mut().find(|c| {
            c.corpus_id == hit.corpus_id && c.chunk_id.is_some() && c.chunk_id == hit.chunk_id
        }) {
            // Already-present: lift score + tag metadata in place.
            existing.score = lifted_score;
            existing
                .metadata
                .insert("source".to_string(), "meta_atlas_boost".to_string());
            existing
                .metadata
                .insert("articulation".to_string(), articulation.to_string());
            if let Some(s) = stability {
                existing
                    .metadata
                    .insert("stability".to_string(), s.to_string());
            }
            affected += 1;
            continue;
        }
        let mut hit = hit;
        hit.score = lifted_score;
        hit.metadata
            .insert("source".to_string(), "meta_atlas_boost".to_string());
        hit.metadata
            .insert("articulation".to_string(), articulation.to_string());
        if let Some(s) = stability {
            hit.metadata.insert("stability".to_string(), s.to_string());
        }
        chunks.push(hit);
        affected += 1;
    }
    affected
}

/// Production safety valve for atlas-grounded retrieval. Reads
/// `SOVEREIGN_ATLAS_GROUNDING` at every call (cheap — one env
/// lookup) so an operator can flip the toggle without restarting
/// the daemon (set it before the next query). Anything that parses
/// to "0" / "false" / "off" / "no" disables; missing or any other
/// value enables.
pub(crate) fn atlas_grounding_enabled() -> bool {
    match std::env::var("SOVEREIGN_ATLAS_GROUNDING") {
        Ok(v) => !matches!(
            v.to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

/// Noise floor: drop chunks whose title nor content (lowercased)
/// contains any substantive query token. Anything with even one
/// overlap is kept; reweight + sort + cap downstream handle ranking.
///
/// Title-expand augmentation explored (v36 / 2026-05-18). Adding
/// title-expand tokens to the survival set kept ALL chunks from the
/// named article (because every chunk's title contains the title
/// token), turning retrieval monocultural — `expand_from_dominant_
/// source` then doubled down on the already-dominant article. On a
/// 13-thread eval, 6 threads regressed on `fact_recall` (buddhism
/// -0.24, darwin -0.15, industrial -0.14, einstein -0.13, wwii -0.10,
/// columbus -0.10) vs the v28 baseline. Marathon T6/T7/T8 stayed
/// clean (no v33-style bypass), but the cross-thread cost was net
/// negative. Reverted. See `bench/wikipedia_learn/V36_FINDINGS.md`
/// for the full trace + the principled reservation-only path
/// (option C) handed to the next iteration.
pub(crate) fn drop_no_overlap_chunks(chunks: Vec<ScoredChunk>, query: &str) -> Vec<ScoredChunk> {
    let query_tokens = extract_tokens(query, EVIDENCE_TITLE_MIN_TOKEN_LEN);
    if query_tokens.is_empty() {
        return chunks;
    }
    chunks
        .into_iter()
        .filter(|c| {
            let title = c.title.as_deref().unwrap_or("");
            let title_tokens = extract_tokens(title, EVIDENCE_TITLE_MIN_TOKEN_LEN);
            let title_hit = query_tokens
                .iter()
                .any(|q| title_tokens.iter().any(|t| t == q));
            if title_hit {
                return true;
            }
            let content_lower = c.content.to_lowercase();
            query_tokens
                .iter()
                .any(|q| content_lower.contains(q.as_str()))
        })
        .collect()
}

#[cfg(test)]
mod query_relevance_tests {
    use super::{blend_query_aware, reweight_by_query_relevance};
    use corpus_engine::ScoredChunk;
    use std::collections::HashMap;

    fn chunk(corpus: &str, title: &str, content: &str, score: f32) -> ScoredChunk {
        ScoredChunk {
            content: content.into(),
            title: Some(title.into()),
            url: None,
            corpus_id: corpus.into(),
            score,
            metadata: HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    /// The Operation Barbarossa failure mode this reweight was
    /// designed for: an off-domain corpus (sep-al-farabi, philosophy
    /// entries) returns RRF rank-1 hits at the same numeric score
    /// as Wikipedia's canonical article. Pre-reweight, the merge
    /// sort treats them as ties and floods the top-K with off-topic
    /// chunks. Post-reweight, the Wikipedia chunk's title- and
    /// content-overlap with the query boost it above the SEP chunk.
    #[test]
    fn wikipedia_chunk_outranks_off_domain_after_reweight() {
        let mut chunks =
            vec![
            // sep-al-farabi: an unrelated philosophy entry whose
            // RRF rank-1 happens to match the numeric score of
            // Wikipedia's hit. Title doesn't share tokens with the
            // query; content has at most a marginal substring.
            chunk("sep", "operationalism", "operationalism is a philosophy", 0.0328),
            // Wikipedia: the canonical article. Title shares two
            // tokens with the query, content carries every
            // substantive token.
            chunk(
                "wikipedia",
                "Operation Barbarossa",
                "Operation Barbarossa was the failed German invasion of the Soviet Union in 1941.",
                0.0328,
            ),
        ];
        reweight_by_query_relevance(&mut chunks, "Why did Operation Barbarossa fail?");
        chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        assert_eq!(
            chunks[0].title.as_deref(),
            Some("Operation Barbarossa"),
            "Wikipedia's canonical article must outrank the off-domain corpus's tied RRF hit \
             after reweight; got order {:?}",
            chunks.iter().map(|c| c.title.clone()).collect::<Vec<_>>()
        );
    }

    /// Reweight must preserve relative order within a single corpus
    /// when chunks have the same overlap profile — multiplicative
    /// boosts that depend only on title/content tokens shouldn't
    /// shuffle hits whose only difference is the underlying RRF
    /// score.
    #[test]
    fn within_corpus_ranking_is_stable_under_reweight() {
        let mut chunks = vec![
            chunk(
                "wiki",
                "Yalta Conference",
                "Yalta Conference details",
                0.030,
            ),
            chunk("wiki", "Yalta Conference", "Yalta Conference more", 0.020),
            chunk("wiki", "Yalta Conference", "Yalta Conference still", 0.010),
        ];
        reweight_by_query_relevance(&mut chunks, "Yalta Conference leaders");
        // Each chunk has identical title and content overlap, so the
        // boost factor is constant; sort order should match
        // descending raw score.
        assert!(chunks[0].score > chunks[1].score);
        assert!(chunks[1].score > chunks[2].score);
    }

    /// All-stopword query (or an all-short-token query) should be a
    /// no-op — there's nothing meaningful to reweight against, and
    /// the off-target gate downstream has its own handling.
    #[test]
    fn no_query_tokens_is_a_noop() {
        let mut chunks = vec![chunk("wiki", "Some Title", "Some Content", 0.020)];
        let before = chunks[0].score;
        reweight_by_query_relevance(&mut chunks, "the and you");
        assert_eq!(chunks[0].score, before);
    }

    /// Empty input must not panic.
    #[test]
    fn empty_input_is_a_noop() {
        let mut chunks: Vec<ScoredChunk> = Vec::new();
        reweight_by_query_relevance(&mut chunks, "any query");
        assert!(chunks.is_empty());
    }

    /// A chunk with zero overlap (no title-token match, no content-
    /// token substring) keeps its raw RRF score. This is the
    /// signal: chunks that don't actually answer the query don't
    /// get artificially boosted just because their corpus had a hit.
    #[test]
    fn no_overlap_keeps_raw_score() {
        let mut chunks = vec![chunk(
            "off-domain",
            "Walter Chatton",
            "medieval scholastic philosopher",
            0.0167,
        )];
        reweight_by_query_relevance(&mut chunks, "How did the Battle of Midway end?");
        assert!(
            (chunks[0].score - 0.0167).abs() < 1e-6,
            "off-domain chunk with no overlap should keep its baseline RRF score; got {}",
            chunks[0].score
        );
    }

    #[test]
    fn blend_query_aware_steers_toward_query_yet_stays_unit() {
        // Two distinct unit directions: the blend must sit between them
        // and stay unit-length so an ANN sees a clean steered query.
        let anchor = vec![1.0_f32, 0.0, 0.0];
        let query = vec![0.0_f32, 1.0, 0.0];
        let blended = blend_query_aware(&anchor, &query, 0.5);
        let norm = blended.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "result must be unit length; got {norm}"
        );
        // Equal weight → symmetric mix that carries BOTH directions.
        assert!(
            (blended[0] - blended[1]).abs() < 1e-5,
            "0.5 blend must be symmetric"
        );
        assert!(
            blended[0] > 0.0 && blended[1] > 0.0,
            "must carry anchor AND query"
        );
    }

    #[test]
    fn blend_query_aware_falls_back_to_anchor_when_query_unusable() {
        let anchor = vec![1.0_f32, 0.0, 0.0];
        // Empty query embedding → topic-only fetch (never worse than before).
        assert_eq!(blend_query_aware(&anchor, &[], 0.5), anchor);
        // Dimension mismatch → same defensive fallback, no panic.
        assert_eq!(blend_query_aware(&anchor, &[1.0, 2.0], 0.5), anchor);
        // anchor_weight = 1.0 → pure (normalised) anchor: the prior behaviour.
        let pure = blend_query_aware(&anchor, &[0.0, 1.0, 0.0], 1.0);
        assert!((pure[0] - 1.0).abs() < 1e-5 && pure[1].abs() < 1e-5);
    }
}
