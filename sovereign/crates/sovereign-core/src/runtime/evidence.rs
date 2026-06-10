// SPDX-License-Identifier: AGPL-3.0-or-later
//! Evidence-shape signals and synthesis routing.
//!
//! Two concerns:
//!
//! 1. **`EvidenceShape`** — the structured snapshot of "what does the top-K
//!    look like?" produced by `compute_evidence_shape`. Carries the
//!    score-concentration, source-dominance, title-overlap, and content-
//!    token-coverage signals the retrieval-miss gate and the Fast/Primary
//!    routing heuristic both read.
//!
//! 2. **`SynthesisRoute`** + `route_from_evidence` — the heuristic that
//!    maps a shape onto Fast (concentrated single-source) vs. Primary
//!    (multi-source synthesis or weak retrieval). Sibling
//!    `Runtime::prepare_knowledge_query_plan` consumes the route to
//!    pick a slot.
//!
//! Pure functions throughout — no `Runtime`, no I/O. The KQ planner and
//! the retrieval-helpers module (`runtime/retrieval_helpers.rs`,
//! sequenced next) both depend on `extract_tokens` and the
//! `EVIDENCE_*` constants exported here.

use corpus_engine::ScoredChunk;

use crate::types::{Intent, Operation};

/// Minimum token length for a query word to count toward title-match
/// or content-coverage. Short tokens like "the", "and", "can", "you"
/// are ignored regardless. Stopwords are dropped on top of this floor
/// (see `extract_tokens`).
pub(crate) const EVIDENCE_TITLE_MIN_TOKEN_LEN: usize = 4;

/// Coverage threshold below which retrieval is considered to have no
/// signal (genuinely dispersed noise). `coverage = fraction of the
/// query's content tokens that appear in the concatenated top-K chunk
/// text`. Calibrated from observation: a legitimately on-topic
/// retrieval against Wikipedia surfaces ≥ 60% of substantive query
/// tokens in its top chunks; truly off-target retrieval (the
/// "Commonwealth scheduler" failure mode this gate was designed for)
/// surfaces < 20%. Sitting the threshold at 0.4 catches the noise case
/// without nipping at marginal-but-real retrievals.
pub(crate) const EVIDENCE_MIN_TOKEN_COVERAGE: f32 = 0.4;

/// `top1_score / median(top_k_scores)` above this ratio marks the
/// retrieval as *concentrated* — the top hit stands clearly above the
/// middle of the distribution. Median (not top-3) because a single
/// high-scoring but irrelevant neighbor (e.g. a conversation-history
/// chunk that vector-matches the query phrasing) can drag top-3 up
/// and kill the signal. Median is robust to one noisy neighbor.
pub(crate) const EVIDENCE_MEDIAN_RATIO_THRESHOLD: f32 = 1.5;

/// Minimum chunk count in the top-k sharing the same `(corpus_id, title)`
/// as the top chunk, for "single source owns this" to fire. 2+ hits to the
/// same document is a strong single-source signal even without title_match.
pub(crate) const EVIDENCE_MIN_TOP_SOURCE_REPEAT: usize = 2;

/// Decisive threshold: this many repeats of the top source in top-k routes
/// Fast regardless of other signals — the retrieval has clearly landed on
/// one document multiple times. Cheaper than re-deriving median_ratio on
/// edge cases.
pub(crate) const EVIDENCE_DECISIVE_TOP_SOURCE_REPEAT: usize = 3;

/// Structured snapshot of retrieval shape. Every field is computed
/// once in `compute_evidence_shape`; downstream callers (the routing
/// heuristic, the retrieval-miss gate, the dominant-source expander)
/// read the snapshot rather than re-deriving signals from the raw
/// chunk list.
///
/// Scale notes — every score field is on the corpus-engine hybrid
/// (RRF) scale (cosine + FTS each contribute 1/(60+rank)):
/// - Rank-1 with both signals ≈ 0.033.
/// - Rank-1 with only vector OR only FTS ≈ 0.0167 (1/60).
/// - Single-doc lookups typically see top_source_repeat ≥ 2 and
///   median_ratio ≥ 1.8.
/// - Multi-source synthesis typically sees top_source_repeat = 1 and
///   median_ratio ≤ 1.2.
#[derive(Debug, Clone)]
pub struct EvidenceShape {
    pub(crate) count: usize,
    pub(crate) top1_score: f32,
    pub(crate) median_score: f32,
    /// `top1_score / median_score`. ∞ when median is zero.
    pub(crate) median_ratio: f32,
    /// Count of chunks in top-k with the same `(corpus_id, title)` as
    /// the top chunk. ≥ 2 means the same document shows up multiple
    /// times, which is the strongest single-source signal we have.
    pub(crate) top_source_repeat_count: usize,
    pub(crate) distinct_sources: usize,
    /// True iff *any* chunk in the top-K has a title sharing a content
    /// token with the query (after stopword + min-length filter).
    /// Originally top-1 only; broadened so the signal isn't lost when
    /// cross-corpus pollution edges the canonical article out of slot
    /// 1. A positive title_match is *positive evidence* that retrieval
    /// landed on the right document — even if the model has to look
    /// past the top score to find it.
    pub(crate) title_match: bool,
    /// Fraction of the query's content tokens (≥ 4 chars, stopwords
    /// dropped) that appear in the concatenated top-K chunk text.
    /// 0.0 when the query has no content tokens (all-stopwords query).
    /// Range [0, 1]. The single most-important signal for the
    /// off-target gate: retrieval-without-signal scores near 0,
    /// on-topic retrieval scores 0.6+.
    pub(crate) query_token_coverage: f32,
    /// `(corpus_id, title)` of the top-scoring chunk — the identity
    /// the source-expansion path uses to pull more chunks from the
    /// dominant document. Empty when chunks is empty.
    pub(crate) top_source_key: (String, String),
    /// Human-readable `corpus_id::title` for logging only.
    pub(crate) top_source_label: String,
}

/// Test-only constructor for `EvidenceShape`. Builds a synthetic
/// shape with the named dimensions plus sensible scoring defaults —
/// integration tests drive retrieval-miss pathways without needing
/// a real corpus engine. Not intended for production call sites;
/// the real path goes through `compute_evidence_shape`.
pub fn build_test_evidence_shape(
    count: usize,
    distinct_sources: usize,
    title_match: bool,
    top_source_repeat_count: usize,
) -> EvidenceShape {
    EvidenceShape {
        count,
        top1_score: 0.02,
        median_score: 0.017,
        median_ratio: 1.1,
        top_source_repeat_count,
        distinct_sources,
        title_match,
        // `1.0` matches the test's intent: callers of
        // `build_test_evidence_shape` are constructing positive-evidence
        // shapes where token coverage is implicitly assumed full. Tests
        // that need to probe coverage-driven bail-outs construct chunks
        // and call `compute_evidence_shape` directly.
        query_token_coverage: 1.0,
        top_source_key: ("test-corpus".to_string(), "Test Note".to_string()),
        top_source_label: "test-corpus::Test Note".to_string(),
    }
}

impl EvidenceShape {
    /// Retrieval-miss signal: does the top-K contain *any* content
    /// related to the user's question?
    ///
    /// Returns `true` only when retrieval is genuinely dispersed
    /// noise — chunks came back, but their content has no overlap
    /// with the query's substantive tokens AND no title in the
    /// top-K touches the query. That's the actual "the corpora
    /// didn't have what was asked" shape, and the only case where
    /// suppressing synthesis prevents fabrication.
    ///
    /// Replaces an earlier shape-only heuristic (`!title_match` on
    /// the top-1 chunk + `distinct_sources >= 3` + no source repeat)
    /// that conflated two different shapes:
    ///   1. true noise — chunks unrelated to the query, but the
    ///      hybrid scorer returned something anyway,
    ///   2. legitimate multi-article synthesis — chunks span 3-5
    ///      relevant Wikipedia articles, no single one dominates.
    /// The old test fired on both; this one separates them by
    /// looking at whether the chunks *actually contain* substantive
    /// tokens from the question.
    ///
    /// Conditions for an off-target verdict:
    ///   - retrieval returned at least one chunk (empty is "no
    ///     data", handled by a sibling parametric-knowledge branch),
    ///   - no chunk title in the top-K shares a content token with
    ///     the query (`title_match == false`),
    ///   - the concatenated top-K content covers fewer than
    ///     `EVIDENCE_MIN_TOKEN_COVERAGE` of the query's content
    ///     tokens — i.e., the question's substantive words don't
    ///     appear in what came back,
    ///   - retrieval fanned out across ≥ 3 distinct sources (a
    ///     single dominating source is never "dispersed", even when
    ///     coverage is low — the model can read the document and
    ///     decide for itself).
    pub(crate) fn is_off_target(&self) -> bool {
        self.count > 0
            && !self.title_match
            && self.query_token_coverage < EVIDENCE_MIN_TOKEN_COVERAGE
            && self.distinct_sources >= 3
    }
}

/// Which synthesis path to take given the evidence shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SynthesisRoute {
    /// Fast slot (9B/1.7B), small max_tokens, no thinking. For concentrated
    /// entity-lookup / single-source summarise cases.
    FastFocused,
    /// Primary slot (large model), full thinking budget. For genuine
    /// cross-source synthesis or weak retrieval where careful reasoning
    /// about what's NOT known actually helps.
    PrimarySynthesis,
}

/// Identity used for source-dominance: corpus_id + document title, since a
/// single corpus can host many unrelated documents.
pub(crate) fn chunk_source_key(c: &ScoredChunk) -> (String, String) {
    (c.corpus_id.clone(), c.title.clone().unwrap_or_default())
}

/// Whether a non-dominant chunk qualifies as a "grounding" signal
/// alongside the expanded dominant source.
///
/// Excludes:
/// 1. `conversation-history` corpus chunks — previous user/assistant
///    turns aren't topical sources for a knowledge query. Including
///    them invites the model to acknowledge them as citable material
///    and burn output tokens (observed: a Schrödinger-PDF user message
///    made the Joan Robinson answer truncate mid-sentence trying to
///    address it).
/// 2. Untitled chunks — real knowledge sources have titles. Untitled
///    rows are almost always raw messages or extraction artifacts.
pub(crate) fn is_grounding_candidate(chunk: &ScoredChunk) -> bool {
    if chunk.corpus_id == "conversation-history" {
        return false;
    }
    let title = chunk.title.as_deref().unwrap_or("");
    !title.trim().is_empty()
}

/// Extract ≥N-char tokens from `text`, lowercased, stopwords removed.
pub(crate) fn extract_tokens(text: &str, min_len: usize) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "about", "above", "after", "again", "also", "been", "being", "both", "could", "does",
        "doing", "down", "each", "from", "have", "having", "here", "just", "like", "make", "many",
        "more", "most", "much", "need", "only", "other", "over", "should", "some", "such", "tell",
        "than", "that", "their", "them", "then", "there", "these", "they", "this", "those", "upon",
        "very", "want", "were", "what", "when", "where", "which", "while", "will", "with", "would",
        "your",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= min_len)
        .map(|s| s.to_lowercase())
        .filter(|s| !STOPWORDS.contains(&s.as_str()))
        .collect()
}

/// Median of `values`. Assumes non-empty; callers must guard.
pub(crate) fn median_f32(values: &[f32]) -> f32 {
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Compute retrieval-shape signals over the top-k chunks. `query` is the
/// raw user message — used only for the title-match signal.
pub(crate) fn compute_evidence_shape(chunks: &[ScoredChunk], query: &str) -> EvidenceShape {
    if chunks.is_empty() {
        return EvidenceShape {
            count: 0,
            top1_score: 0.0,
            median_score: 0.0,
            median_ratio: 0.0,
            top_source_repeat_count: 0,
            distinct_sources: 0,
            title_match: false,
            query_token_coverage: 0.0,
            top_source_key: (String::new(), String::new()),
            top_source_label: String::new(),
        };
    }

    let top1_score = chunks[0].score;
    let scores: Vec<f32> = chunks.iter().map(|c| c.score).collect();
    let median_score = median_f32(&scores);
    let median_ratio = if median_score > 0.0 {
        top1_score / median_score
    } else {
        f32::INFINITY
    };

    let top_key = chunk_source_key(&chunks[0]);
    let top_source_repeat_count = chunks
        .iter()
        .filter(|c| chunk_source_key(c) == top_key)
        .count();

    let distinct_sources = {
        let mut keys: Vec<_> = chunks.iter().map(chunk_source_key).collect();
        keys.sort();
        keys.dedup();
        keys.len()
    };

    // Title-match across the entire top-K — not just slot 1 — because
    // cross-corpus retrieval routinely lands the canonical article at
    // rank 2-3 when an off-domain corpus has a high vector-similarity
    // false positive on common query terms. A title-token overlap
    // anywhere in top-K is positive evidence that the right document
    // is in the prompt.
    let query_tokens = extract_tokens(query, EVIDENCE_TITLE_MIN_TOKEN_LEN);
    let title_match = !query_tokens.is_empty()
        && chunks.iter().any(|c| {
            let title = c.title.as_deref().unwrap_or("");
            if title.is_empty() {
                return false;
            }
            let title_tokens = extract_tokens(title, EVIDENCE_TITLE_MIN_TOKEN_LEN);
            query_tokens
                .iter()
                .any(|q| title_tokens.iter().any(|t| t == q))
        });

    // Content-token coverage — fraction of the query's substantive
    // tokens that show up *anywhere* in the concatenated top-K chunk
    // text. This is the single grounded signal for "did retrieval
    // return content related to what was asked": a real
    // retrieval-miss (chunks unrelated to the query) scores near 0,
    // a legitimate retrieval scores 0.5-1.0 even when no single
    // article dominates. Replaces the shape-only proxy that was
    // declaring multi-article syntheses "off-target" simply because
    // no source repeated.
    let query_token_coverage = if query_tokens.is_empty() {
        0.0
    } else {
        let haystack: String = chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        let hits = query_tokens
            .iter()
            .filter(|q| haystack.contains(q.as_str()))
            .count();
        hits as f32 / query_tokens.len() as f32
    };

    let top_source_label = format!("{}::{}", top_key.0, top_key.1);

    EvidenceShape {
        count: chunks.len(),
        top1_score,
        median_score,
        median_ratio,
        top_source_repeat_count,
        distinct_sources,
        title_match,
        query_token_coverage,
        top_source_key: top_key,
        top_source_label,
    }
}

/// Apply the routing heuristic. Returns `FastFocused` when the retrieval
/// looks like a single-source lookup; otherwise `PrimarySynthesis`.
///
/// Three independent Fast-path triggers, listed in descending strength:
///   1. **Decisive repeat**: ≥ 3 chunks in top-k share the same
///      `(corpus_id, title)`. One document clearly owns the answer.
///   2. **Concentrated repeat**: ≥ 2 repeats AND median_ratio ≥ threshold.
///      The top document dominates both by count and by score steepness.
///   3. **Entity match**: top chunk's title contains a non-stopword query
///      token AND median_ratio ≥ threshold. For single-chunk strong hits.
///
/// Everything else (including weak retrieval with flat scores) routes to
/// Primary — thinking actually earns its keep when the model has to reason
/// carefully about what it does and doesn't know.
pub(crate) fn route_from_evidence(shape: &EvidenceShape) -> SynthesisRoute {
    if shape.count == 0 {
        // Caller handles empty retrieval on its own parametric path;
        // we return Fast only as a default, but in practice it isn't used.
        return SynthesisRoute::FastFocused;
    }

    if shape.top_source_repeat_count >= EVIDENCE_DECISIVE_TOP_SOURCE_REPEAT {
        return SynthesisRoute::FastFocused;
    }

    let concentrated = shape.median_ratio >= EVIDENCE_MEDIAN_RATIO_THRESHOLD;

    if concentrated && shape.top_source_repeat_count >= EVIDENCE_MIN_TOP_SOURCE_REPEAT {
        return SynthesisRoute::FastFocused;
    }

    if concentrated && shape.title_match {
        return SynthesisRoute::FastFocused;
    }

    SynthesisRoute::PrimarySynthesis
}

/// Why a synthesis route was chosen — a typed reason that travels with the
/// route so "why did THIS query hit the fast/primary slot?" is answerable from
/// one trace field. The session proved the live path was mis-identified three
/// times when this ladder was inlined in the handler; the typed reason makes
/// the WHY explicit. Ordered by the priority in which they apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteReason {
    /// `ComparisonQuery`: a bounded-axes contrast — the Fast slot's constrained
    /// prompt does the structuring the primary model would otherwise do, so we
    /// pin Fast regardless of evidence shape.
    ComparisonPin,
    /// Atom-enumeration fired: the directed set is many low-cosine entity
    /// chunks, so the evidence shape reads single-focus; pin Primary so it
    /// writes the full list cleanly instead of narrating per-passage on Fast.
    AtomEnumPin,
    /// No pin applied — the evidence-shape heuristic (`route_from_evidence`)
    /// chose the route.
    EvidenceShape,
}

impl SynthesisRoute {
    /// The role-layer [`crate::role::Tier`] this route runs on — the slot the
    /// Synthesizer role executes on for this turn. The projection that connects
    /// the synthesis-routing internals to the role vocabulary.
    pub(crate) fn tier(self) -> crate::role::Tier {
        match self {
            SynthesisRoute::FastFocused => crate::role::Tier::Fast,
            SynthesisRoute::PrimarySynthesis => crate::role::Tier::Primary,
        }
    }
}

/// The synthesis-route decision: the chosen route, the reason for it, and the
/// role-layer tier (the Synthesizer's slot) it resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteDecision {
    pub(crate) route: SynthesisRoute,
    pub(crate) reason: RouteReason,
    pub(crate) tier: crate::role::Tier,
}

/// Resolve the synthesis route in ONE place — the capability decision (Router
/// role → slot/tier). Pure + total so it's unit-testable against the legacy
/// ladder truth table; the caller emits the glassbox trace (it also has the
/// `operation` axis + full shape detail to log alongside the `reason`).
///
/// Priority ladder — PRESERVED byte-for-byte from the former inline
/// `KnowledgeQuery` handler logic:
///   1. `ComparisonQuery` → `FastFocused`               (`ComparisonPin`)
///   2. `has_atom_enum`   → `PrimarySynthesis`           (`AtomEnumPin`)
///   3. else              → `route_from_evidence(shape)` (`EvidenceShape`)
pub(crate) fn resolve_synthesis_route(
    intent: &Intent,
    has_atom_enum: bool,
    shape: &EvidenceShape,
) -> RouteDecision {
    let (route, reason) = if matches!(intent, Intent::ComparisonQuery) {
        (SynthesisRoute::FastFocused, RouteReason::ComparisonPin)
    } else if has_atom_enum {
        (SynthesisRoute::PrimarySynthesis, RouteReason::AtomEnumPin)
    } else {
        (route_from_evidence(shape), RouteReason::EvidenceShape)
    };
    RouteDecision {
        route,
        reason,
        tier: route.tier(),
    }
}

#[cfg(test)]
mod route_resolver_tests {
    use super::*;
    use crate::types::Intent;

    /// A shape `route_from_evidence` sends to Fast (decisive ≥3 same-source
    /// repeat) and one it sends to Primary (flat, no repeat, no title match).
    fn fast_shape() -> EvidenceShape {
        build_test_evidence_shape(5, 1, false, 3)
    }
    fn primary_shape() -> EvidenceShape {
        build_test_evidence_shape(5, 4, false, 1)
    }

    /// Re-implements the PRE-refactor inline ladder so the resolver is pinned
    /// byte-for-byte against the historical logic.
    fn legacy(intent: &Intent, has_atom_enum: bool, shape: &EvidenceShape) -> SynthesisRoute {
        if matches!(intent, Intent::ComparisonQuery) {
            SynthesisRoute::FastFocused
        } else if has_atom_enum {
            SynthesisRoute::PrimarySynthesis
        } else {
            route_from_evidence(shape)
        }
    }

    #[test]
    fn test_shapes_route_as_expected() {
        assert_eq!(
            route_from_evidence(&fast_shape()),
            SynthesisRoute::FastFocused
        );
        assert_eq!(
            route_from_evidence(&primary_shape()),
            SynthesisRoute::PrimarySynthesis
        );
    }

    #[test]
    fn resolver_matches_legacy_ladder_truth_table() {
        let intents = [
            Intent::ComparisonQuery,
            Intent::KnowledgeQuery,
            Intent::DeepQuery,
            Intent::SimpleQuery,
        ];
        for intent in &intents {
            for &has_atom_enum in &[false, true] {
                for shape in [fast_shape(), primary_shape()] {
                    let got = resolve_synthesis_route(intent, has_atom_enum, &shape);
                    let want = legacy(intent, has_atom_enum, &shape);
                    assert_eq!(
                        got.route, want,
                        "route mismatch: intent={intent:?} atom_enum={has_atom_enum} repeat={}",
                        shape.top_source_repeat_count
                    );
                }
            }
        }
    }

    #[test]
    fn tier_tracks_route() {
        use crate::role::Tier;
        let f = resolve_synthesis_route(&Intent::ComparisonQuery, false, &primary_shape());
        assert_eq!(f.route, SynthesisRoute::FastFocused);
        assert_eq!(f.tier, Tier::Fast);
        let p = resolve_synthesis_route(&Intent::KnowledgeQuery, true, &fast_shape());
        assert_eq!(p.route, SynthesisRoute::PrimarySynthesis);
        assert_eq!(p.tier, Tier::Primary);
    }

    #[test]
    fn reasons_follow_priority() {
        // Comparison wins even with atom_enum set + a primary-leaning shape.
        assert_eq!(
            resolve_synthesis_route(&Intent::ComparisonQuery, true, &primary_shape()).reason,
            RouteReason::ComparisonPin
        );
        // Atom-enum wins for non-comparison regardless of shape.
        assert_eq!(
            resolve_synthesis_route(&Intent::KnowledgeQuery, true, &fast_shape()).reason,
            RouteReason::AtomEnumPin
        );
        // Else delegates to the evidence-shape heuristic.
        assert_eq!(
            resolve_synthesis_route(&Intent::KnowledgeQuery, false, &fast_shape()).reason,
            RouteReason::EvidenceShape
        );
    }
}

/// Map a referential intent (+ the atom-enum flag) to its MECE cognitive
/// [`Operation`] — the *what-the-answer-does* axis, decoupled from *effort*
/// (which tier serves it). Returns `None` for non-referential intents
/// (`Metalingual`/`Conation`/`Commissive`/`Expressive`/actions), which route
/// through their own handlers and are outside the operation × effort frame.
///
/// Precedence mirrors the legacy route ladder at
/// `prepare_knowledge_query_plan` (`ComparisonQuery` is checked *before* the
/// atom-enum pin, so a comparison that also carries an atom-enum set is still
/// `Compare`). Behaviour-preserving: this only *names* the operation today;
/// nothing routes on it yet (Step 2 wires effort → tier). See
/// `sovereign/docs/QUERY_TAXONOMY_MECE.md`.
pub(crate) fn operation_of(intent: &Intent, has_atom_enum: bool) -> Option<Operation> {
    match intent {
        // Comparison is its own operation and is checked first (mirrors the
        // legacy ladder: Comparison pinned before the atom-enum pin).
        Intent::ComparisonQuery => Some(Operation::Compare),
        // Within the referential Answer family, the atom-enum flag promotes
        // the turn to Enumerate (a set/roster). The flag is only meaningful
        // here — it is set during corpus retrieval for set-questions — so it
        // never reaches the non-referential arm below.
        Intent::SimpleQuery | Intent::KnowledgeQuery | Intent::DeepQuery => {
            if has_atom_enum {
                Some(Operation::Enumerate)
            } else {
                Some(Operation::Answer)
            }
        }
        // Non-referential intents (Jakobson/speech-act + actions) have no
        // referential Operation, regardless of any flag.
        _ => None,
    }
}

/// Which cohesion expander a knowledge turn should run after evidence-
/// shape routing. Three mutually-exclusive strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpansionStrategy {
    /// Pull the whole dominant document — the turn landed decisively on
    /// one source and wants depth, not breadth.
    DominantSource,
    /// Pull a few chunks from each of the top-N distinct sources — the
    /// turn needs breadth across several articles.
    TopSources,
    /// Initial retrieval already has the right shape; expand nothing.
    NoExpansion,
}

/// Decide the cohesion-expansion strategy for a knowledge turn —
/// **intent-aware** by design.
///
/// `route_from_evidence` answers "which slot" (Fast vs Primary) from
/// shape alone. This answers "which expander", and shape alone is *not*
/// enough: a `ComparisonQuery` is a contrast across ≥2 subjects, so it
/// must never collapse onto a single dominant source — even when the
/// evidence shape looks concentrated. On a cross-corpus index that
/// happens constantly: one dense article (e.g. SEP's
/// `einstein-philscience`) wins source-dominance for an "Einstein vs
/// Newton" query and the dominant-source expander then strips the
/// comparison down to one side. The 2026-05-25 synth audit caught
/// exactly this — comparative wiki questions scored 2/11 sources
/// because dominant-source expansion collapsed them onto SEP. See
/// `docs/RERANK_EXPERIMENT.md` for the SEP-vs-wiki single-source-vs-
/// breadth structural split this guards against.
///
/// Returns the strategy plus a short grep-friendly reason for the
/// `retrieval_audit` glassbox trace.
pub(crate) fn decide_expansion_strategy(
    intent: &Intent,
    route: SynthesisRoute,
    shape: &EvidenceShape,
) -> (ExpansionStrategy, &'static str) {
    // Comparisons are the one intent that structurally needs breadth
    // regardless of how concentrated the pool looks.
    let needs_breadth = matches!(intent, Intent::ComparisonQuery);

    // Depth: a clearly-dominant single source — but never for a
    // comparison, which would defeat the contrast.
    if matches!(route, SynthesisRoute::FastFocused)
        && shape.top_source_repeat_count >= EVIDENCE_MIN_TOP_SOURCE_REPEAT
        && !needs_breadth
    {
        return (ExpansionStrategy::DominantSource, "fast_single_source");
    }

    // Breadth: multi-source synthesis (Primary route) OR any comparison,
    // as long as ≥2 distinct sources are actually present to spread over.
    if (matches!(route, SynthesisRoute::PrimarySynthesis) || needs_breadth)
        && shape.distinct_sources >= 2
    {
        let reason = if needs_breadth {
            "comparison_breadth"
        } else {
            "multi_source_synthesis"
        };
        return (ExpansionStrategy::TopSources, reason);
    }

    // Nothing to expand. Distinguish a comparison that lacked the ≥2
    // distinct sources to spread over (a retrieval-recall problem worth
    // seeing in the trace) from an ordinary concentrated/weak turn.
    let reason = if needs_breadth {
        "comparison_single_source_pool"
    } else {
        "no_expansion"
    };
    (ExpansionStrategy::NoExpansion, reason)
}

/// Heading-aware chunkers (and many extractors) prepend the document
/// title to each chunk body so the stored row is self-describing. When
/// the prompt formatter also emits a `[Source: title]` label line
/// immediately above, the title ends up duplicated — the model reads
///
///   [Source: Joan Robinson]
///   Joan Robinson
///
///   Theory of Employment, Interest and Money...
///
/// as author-book attribution and cheerfully misattributes *The
/// General Theory* to Robinson. This strips the duplicate when the
/// body starts with exactly the title followed by a newline.
///
/// Match is conservative: the title must be the *first line* of the
/// body (so it doesn't accidentally eat a sentence that happens to
/// begin with the title).
pub(crate) fn strip_leading_title_duplicate<'a>(body: &'a str, title: Option<&str>) -> &'a str {
    let title = match title {
        Some(t) if !t.is_empty() => t,
        _ => return body,
    };
    // Body must start with the title followed by a newline (perhaps
    // preceded only by trailing whitespace on the title line).
    let after = match body.strip_prefix(title) {
        Some(rest) => rest,
        None => return body,
    };
    let after = after.trim_start_matches([' ', '\t']);
    match after.strip_prefix('\n') {
        Some(rest) => rest.trim_start_matches(['\n', ' ', '\t']),
        None => body,
    }
}

#[cfg(test)]
mod grounding_filter_tests {
    use super::is_grounding_candidate;
    use corpus_engine::ScoredChunk;
    use std::collections::HashMap;

    fn chunk(corpus_id: &str, title: Option<&str>) -> ScoredChunk {
        ScoredChunk {
            content: "body".into(),
            title: title.map(|t| t.into()),
            url: None,
            corpus_id: corpus_id.into(),
            score: 0.03,
            metadata: HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    /// Named chunks from knowledge corpora are valid grounding.
    #[test]
    fn titled_knowledge_corpus_chunk_is_candidate() {
        assert!(is_grounding_candidate(&chunk(
            "sep",
            Some("cambridge-capital-controversy")
        )));
    }

    /// Conversation-history is never a grounding candidate regardless
    /// of title. Reason: previous user/assistant turns are not topical
    /// sources.
    #[test]
    fn conversation_history_never_grounds() {
        assert!(!is_grounding_candidate(&chunk(
            "conversation-history",
            Some("anything"),
        )));
        assert!(!is_grounding_candidate(&chunk(
            "conversation-history",
            Some(""),
        )));
        assert!(!is_grounding_candidate(&chunk(
            "conversation-history",
            None
        )));
    }

    /// Untitled chunks (empty or whitespace-only title, or None) are
    /// filtered — real sources have real titles.
    #[test]
    fn untitled_chunks_are_filtered() {
        assert!(!is_grounding_candidate(&chunk("folder-xyz", Some(""))));
        assert!(!is_grounding_candidate(&chunk("folder-xyz", Some("   "))));
        assert!(!is_grounding_candidate(&chunk("folder-xyz", None)));
    }
}

#[cfg(test)]
mod strip_title_tests {
    use super::strip_leading_title_duplicate;

    /// The exact Joan Robinson case: obsidian chunker prepended the note
    /// title followed by a blank line, which combined with the prompt's
    /// [Source: X] label produced an author-book attribution pattern.
    /// Stripping the duplicate must leave just the content body.
    #[test]
    fn strips_joan_robinson_pattern() {
        let body = "Joan Robinson\n\nTheory of Employment, Interest and Money_—the book that would reshape how governments understood their role in the economy.";
        let stripped = strip_leading_title_duplicate(body, Some("Joan Robinson"));
        assert_eq!(
            stripped,
            "Theory of Employment, Interest and Money_—the book that would reshape how governments understood their role in the economy."
        );
    }

    /// Single newline (no blank line) should also strip.
    #[test]
    fn strips_single_newline_separator() {
        let body = "Joan Robinson\nContent continues here.";
        assert_eq!(
            strip_leading_title_duplicate(body, Some("Joan Robinson")),
            "Content continues here."
        );
    }

    /// Trailing whitespace on the title line must not defeat the match.
    #[test]
    fn strips_title_with_trailing_whitespace() {
        let body = "Joan Robinson  \n\nContent.";
        assert_eq!(
            strip_leading_title_duplicate(body, Some("Joan Robinson")),
            "Content."
        );
    }

    /// A chunk whose body starts with the title as part of a sentence
    /// (not followed by a newline) must NOT be stripped — the title is
    /// genuinely part of the prose and removing it would break meaning.
    #[test]
    fn leaves_title_in_sentence_alone() {
        let body = "Joan Robinson was a British economist who challenged mainstream theory.";
        assert_eq!(
            strip_leading_title_duplicate(body, Some("Joan Robinson")),
            "Joan Robinson was a British economist who challenged mainstream theory."
        );
    }

    /// No title (None) or empty title: passthrough.
    #[test]
    fn noop_on_empty_title() {
        let body = "Some content.";
        assert_eq!(strip_leading_title_duplicate(body, None), body);
        assert_eq!(strip_leading_title_duplicate(body, Some("")), body);
    }

    /// Body that doesn't start with the title: passthrough.
    #[test]
    fn noop_when_title_absent() {
        let body = "Some other opening.";
        assert_eq!(
            strip_leading_title_duplicate(body, Some("Joan Robinson")),
            body
        );
    }

    /// Partial match (title is a prefix of the first word) must not strip.
    #[test]
    fn does_not_strip_title_as_word_prefix() {
        let body = "Joanne Rowling authored Harry Potter.";
        assert_eq!(strip_leading_title_duplicate(body, Some("Joan")), body);
    }
}

#[cfg(test)]
mod evidence_shape_tests {
    use super::{compute_evidence_shape, route_from_evidence, SynthesisRoute};
    use corpus_engine::ScoredChunk;
    use std::collections::HashMap;

    fn chunk(corpus: &str, title: &str, score: f32) -> ScoredChunk {
        ScoredChunk {
            content: format!("{title} body"),
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

    /// The Joan Robinson case replicated from production logs:
    /// obsidian owns the answer (3 hits across top-8: ranks 1, 2, 4)
    /// but a conversation-history chunk at rank 3 (0.0320) happens to
    /// vector-match the query phrasing "can you tell me about X".
    /// That interloper was enough to kill a top1/top3 concentration
    /// signal in v1; median-ratio + top_source_repeat must still route
    /// FastFocused despite the noisy neighbor.
    #[test]
    fn joan_robinson_routes_fast() {
        let chunks = vec![
            chunk("obsidian", "Joan Robinson", 0.0325),
            chunk("obsidian", "Joan Robinson", 0.0323),
            chunk("conversation-history", "", 0.0320), // noisy neighbor
            chunk("obsidian", "Joan Robinson", 0.0167), // 3rd hit to same note
            chunk("sep", "emily-elizabeth-jones", 0.0167),
            chunk("folder", "From Dictatorship to Democracy", 0.0167),
            chunk("folder", "ThePrince", 0.0167),
            chunk("obsidian", "Benchmark", 0.0161),
        ];
        let shape = compute_evidence_shape(&chunks, "Can you tell me about Joan Robinson?");
        assert_eq!(shape.count, 8);
        assert!(
            shape.title_match,
            "'robinson' must match the top chunk's title"
        );
        assert_eq!(
            shape.top_source_repeat_count, 3,
            "3 hits to obsidian/Joan Robinson"
        );
        // median_ratio = top1 / median(scores) = 0.0325 / ~0.0167 ≈ 1.95
        assert!(
            shape.median_ratio >= 1.5,
            "median_ratio = {}",
            shape.median_ratio
        );
        let route = route_from_evidence(&shape);
        assert_eq!(route, SynthesisRoute::FastFocused, "shape = {shape:?}");
    }

    /// Multi-source synthesis: ~5 sources at near-equal scores,
    /// top chunk does not repeat, no title match. Must route Primary.
    #[test]
    fn multi_source_synthesis_routes_primary() {
        let chunks = vec![
            chunk("obsidian", "Cambridge Controversy", 0.033),
            chunk("sep", "capital", 0.030),
            chunk("wiki", "Joan Robinson", 0.029),
            chunk("folder", "Samuelson Note", 0.028),
            chunk("conversation-history", "", 0.027),
            chunk("obsidian", "Reswitching", 0.026),
        ];
        let shape = compute_evidence_shape(
            &chunks,
            "How did different economic schools respond to the Cambridge Capital Controversies?",
        );
        assert_eq!(shape.top_source_repeat_count, 1);
        assert!(
            shape.median_ratio < 1.5,
            "median_ratio = {}",
            shape.median_ratio
        );
        assert!(shape.distinct_sources > 2);
        let route = route_from_evidence(&shape);
        assert_eq!(route, SynthesisRoute::PrimarySynthesis);
    }

    /// One source dominates the top-k but the user's query doesn't
    /// name-match the title. ≥ 3 repeats alone must trigger Fast via
    /// the decisive path.
    #[test]
    fn single_source_no_title_match_routes_fast_on_repeat() {
        let chunks = vec![
            chunk("obsidian", "Productivity Paradox", 0.040),
            chunk("obsidian", "Productivity Paradox", 0.038),
            chunk("obsidian", "Productivity Paradox", 0.025),
            chunk("obsidian", "Productivity Paradox", 0.024),
            chunk("sep", "economics", 0.016),
        ];
        let shape = compute_evidence_shape(&chunks, "what slowed down the economy in the 1970s");
        assert!(
            shape.top_source_repeat_count >= 3,
            "repeat = {}",
            shape.top_source_repeat_count
        );
        let route = route_from_evidence(&shape);
        assert_eq!(route, SynthesisRoute::FastFocused);
    }

    /// Weak retrieval: everything scores low and flat. No repeats, no
    /// concentration. Must route Primary so thinking can help.
    #[test]
    fn weak_retrieval_routes_primary() {
        let chunks = vec![
            chunk("obsidian", "Stray Thought", 0.017),
            chunk("sep", "peripheral-entry", 0.016),
            chunk("folder", "Other", 0.016),
            chunk("wiki", "Unrelated", 0.016),
        ];
        let shape = compute_evidence_shape(&chunks, "tell me about quantum field theory");
        assert_eq!(shape.top_source_repeat_count, 1);
        assert!(
            shape.median_ratio < 1.2,
            "median_ratio = {}",
            shape.median_ratio
        );
        let route = route_from_evidence(&shape);
        assert_eq!(route, SynthesisRoute::PrimarySynthesis);
    }

    /// Regression: one obsidian hit + conv-history + strong vector
    /// neighbors at similar scores. Only 1 repeat of the top source —
    /// must route Primary even when median_ratio is modest. Guards
    /// against "false positive Fast" on weak-but-noisy retrieval.
    #[test]
    fn weak_single_hit_with_noisy_neighbors_routes_primary() {
        let chunks = vec![
            chunk("obsidian", "Joan Robinson", 0.0325),
            chunk("conversation-history", "", 0.0320),
            chunk("sep", "random-entry", 0.0315),
            chunk("folder", "random-file", 0.0310),
            chunk("wiki", "random", 0.0300),
        ];
        let shape = compute_evidence_shape(&chunks, "Can you tell me about Joan Robinson?");
        assert_eq!(shape.top_source_repeat_count, 1);
        assert!(shape.title_match);
        // median_ratio is only ~1.03 here — concentration fails.
        assert!(shape.median_ratio < 1.2);
        let route = route_from_evidence(&shape);
        assert_eq!(
            route,
            SynthesisRoute::PrimarySynthesis,
            "single strong hit with diverse clustered neighbors must not force Fast"
        );
    }

    /// Stopwords in the query must not trigger title_match. The
    /// only non-stopword overlap available here is "tell" (stopword)
    /// and "this" (stopword) — a title whose only query-overlap is
    /// stopwords must NOT match.
    #[test]
    fn stopwords_do_not_title_match() {
        let chunks = vec![
            chunk("obsidian", "This Tell Which When Where", 0.030),
            chunk("sep", "other", 0.016),
            chunk("folder", "other-b", 0.016),
        ];
        let shape = compute_evidence_shape(&chunks, "tell me about this when where which");
        assert!(
            !shape.title_match,
            "only overlap is stopwords — should not count"
        );
    }

    /// Empty retrieval must not panic. Returns Fast as a default
    /// but callers take the parametric-knowledge branch before
    /// the route ever looks at a chunk.
    #[test]
    fn empty_retrieval_is_safe() {
        let chunks: Vec<ScoredChunk> = Vec::new();
        let shape = compute_evidence_shape(&chunks, "anything");
        assert_eq!(shape.count, 0);
        assert_eq!(shape.distinct_sources, 0);
        let route = route_from_evidence(&shape);
        assert_eq!(route, SynthesisRoute::FastFocused);
    }

    // ── PR5 is_off_target coverage ────────────────────────────────

    /// The "Commonwealth scheduler" failure mode from real logs:
    /// 8 chunks, 2 each across 4 unrelated corpora, no title match,
    /// no source repeat. `is_off_target()` must fire so the runtime
    /// diverts to clarification instead of synthesizing a
    /// fabrication against dispersed noise.
    #[test]
    fn commonwealth_scheduler_shape_is_off_target() {
        // Every chunk has a unique (corpus_id, title) so nothing
        // concentrates — maximum dispersion, the classic
        // retrieval-miss shape captured from the production log.
        let chunks = vec![
            chunk("folder", "The Prince", 0.0170),
            chunk("folder", "political-theory", 0.0167),
            chunk("obsidian", "Cartoon Reel", 0.0167),
            chunk("obsidian", "Other Note", 0.0167),
            chunk("sep", "utilitarianism", 0.0167),
            chunk("sep", "consequentialism", 0.0167),
            chunk("wiki", "capitalism", 0.0161),
            chunk("wiki", "republic", 0.0160),
        ];
        let shape = compute_evidence_shape(&chunks, "Tell me about the Commonwealth scheduler");
        assert!(shape.distinct_sources >= 3);
        assert!(!shape.title_match);
        assert_eq!(
            shape.top_source_repeat_count, 1,
            "no concentration — every (corpus, title) is unique"
        );
        assert!(
            shape.is_off_target(),
            "dispersed noise must read as off-target: {shape:?}"
        );
    }

    /// Positive control: the concentrated Joan Robinson shape is
    /// decidedly NOT a miss. Guards against a regression where
    /// is_off_target eats into legitimate single-source retrieval.
    #[test]
    fn joan_robinson_shape_is_not_off_target() {
        let chunks = vec![
            chunk("obsidian", "Joan Robinson", 0.0325),
            chunk("obsidian", "Joan Robinson", 0.0323),
            chunk("conversation-history", "", 0.0320),
            chunk("obsidian", "Joan Robinson", 0.0167),
            chunk("sep", "emily-elizabeth-jones", 0.0167),
            chunk("folder", "From Dictatorship to Democracy", 0.0167),
            chunk("folder", "ThePrince", 0.0167),
            chunk("obsidian", "Benchmark", 0.0161),
        ];
        let shape = compute_evidence_shape(&chunks, "Can you tell me about Joan Robinson?");
        assert!(shape.title_match);
        assert!(
            !shape.is_off_target(),
            "title match + 3 repeats must clear off-target: {shape:?}"
        );
    }

    /// Empty retrieval is handled by the parametric-knowledge branch
    /// upstream, not by is_off_target. Count==0 must read as NOT
    /// off-target so the diversion logic doesn't fire on a no-hits
    /// case it can't improve.
    #[test]
    fn empty_retrieval_is_not_off_target() {
        let chunks: Vec<ScoredChunk> = Vec::new();
        let shape = compute_evidence_shape(&chunks, "anything");
        assert!(!shape.is_off_target());
    }

    /// Two-source dispersion is not enough. Must have ≥ 3 distinct
    /// sources to read as genuinely dispersed.
    #[test]
    fn two_source_split_is_not_off_target() {
        let chunks = vec![
            chunk("obsidian", "Note A", 0.020),
            chunk("sep", "entry-a", 0.018),
        ];
        let shape = compute_evidence_shape(&chunks, "some question");
        assert_eq!(shape.distinct_sources, 2);
        assert!(
            !shape.is_off_target(),
            "2 sources is below the dispersion threshold"
        );
    }

    /// A title match rescues a dispersed shape from off-target.
    /// The query clearly intersected a document's title — that's
    /// enough grounding to synthesize against.
    #[test]
    fn title_match_overrides_dispersion() {
        let chunks = vec![
            chunk("obsidian", "Scheduler Design Doc", 0.020),
            chunk("sep", "utilitarianism", 0.017),
            chunk("folder", "unrelated", 0.017),
            chunk("wiki", "other", 0.017),
        ];
        let shape = compute_evidence_shape(&chunks, "tell me about the scheduler design");
        assert!(shape.title_match);
        assert!(!shape.is_off_target());
    }
}

#[cfg(test)]
mod operation_tests {
    use super::operation_of;
    use crate::types::{Intent, Operation};

    #[test]
    fn operation_of_maps_referential_intents() {
        // Comparison is its own operation, and dominates the atom-enum flag
        // (mirrors the legacy route ladder: Comparison checked first).
        assert_eq!(operation_of(&Intent::ComparisonQuery, false), Some(Operation::Compare));
        assert_eq!(operation_of(&Intent::ComparisonQuery, true), Some(Operation::Compare));
        // Atom-enum flag → Enumerate, regardless of the carrier intent.
        assert_eq!(operation_of(&Intent::KnowledgeQuery, true), Some(Operation::Enumerate));
        assert_eq!(operation_of(&Intent::DeepQuery, true), Some(Operation::Enumerate));
        // Simple / Knowledge / Deep all collapse to one Answer operation —
        // they differ only in effort, not in what the answer does.
        assert_eq!(operation_of(&Intent::SimpleQuery, false), Some(Operation::Answer));
        assert_eq!(operation_of(&Intent::KnowledgeQuery, false), Some(Operation::Answer));
        assert_eq!(operation_of(&Intent::DeepQuery, false), Some(Operation::Answer));
    }

    #[test]
    fn operation_of_is_none_for_non_referential_intents() {
        // Jakobson/speech-act + action intents are outside the operation×effort
        // frame — they keep their own handlers.
        assert_eq!(operation_of(&Intent::MetalingualQuery, false), None);
        assert_eq!(operation_of(&Intent::ConationQuery, false), None);
        assert_eq!(operation_of(&Intent::CommissiveQuery, false), None);
        assert_eq!(operation_of(&Intent::ExpressiveQuery, false), None);
        assert_eq!(operation_of(&Intent::ComplexTask, false), None);
        // ...even when the atom-enum flag is set, a non-referential intent
        // has no referential Operation.
        assert_eq!(operation_of(&Intent::ExpressiveQuery, true), None);
    }
}

#[cfg(test)]
mod expansion_strategy_tests {
    use super::{
        build_test_evidence_shape, decide_expansion_strategy, ExpansionStrategy, SynthesisRoute,
    };
    use crate::types::Intent;

    #[test]
    fn comparison_never_collapses_onto_dominant_source() {
        // Concentrated pool (top source repeats 4×, title match) that
        // WOULD trigger dominant-source expansion for a normal query.
        // A comparison must instead spread across its distinct sources.
        // This is the 2026-05-25 wiki regression guard: comparative
        // questions were collapsing onto a single dense SEP article and
        // scoring 0 of the compared wiki sources. See RERANK_EXPERIMENT.md.
        let shape = build_test_evidence_shape(10, 3, true, 4);
        let (strategy, reason) = decide_expansion_strategy(
            &Intent::ComparisonQuery,
            SynthesisRoute::FastFocused,
            &shape,
        );
        assert_eq!(strategy, ExpansionStrategy::TopSources);
        assert_eq!(reason, "comparison_breadth");
    }

    #[test]
    fn comparison_with_single_source_pool_expands_nothing() {
        // Only one distinct source present — no breadth to spread over.
        // Better to expand nothing than to collapse depth-first.
        let shape = build_test_evidence_shape(6, 1, true, 4);
        let (strategy, reason) = decide_expansion_strategy(
            &Intent::ComparisonQuery,
            SynthesisRoute::FastFocused,
            &shape,
        );
        assert_eq!(strategy, ExpansionStrategy::NoExpansion);
        assert_eq!(reason, "comparison_single_source_pool");
    }

    #[test]
    fn concentrated_non_comparison_still_takes_dominant_source() {
        // The fix must NOT regress the single-source-lookup case.
        let shape = build_test_evidence_shape(10, 2, true, 3);
        let (strategy, _) =
            decide_expansion_strategy(&Intent::KnowledgeQuery, SynthesisRoute::FastFocused, &shape);
        assert_eq!(strategy, ExpansionStrategy::DominantSource);
    }

    #[test]
    fn multi_source_primary_takes_top_sources() {
        let shape = build_test_evidence_shape(10, 4, false, 1);
        let (strategy, reason) = decide_expansion_strategy(
            &Intent::KnowledgeQuery,
            SynthesisRoute::PrimarySynthesis,
            &shape,
        );
        assert_eq!(strategy, ExpansionStrategy::TopSources);
        assert_eq!(reason, "multi_source_synthesis");
    }

    #[test]
    fn weak_non_comparison_fast_focused_expands_nothing() {
        // FastFocused but below the repeat floor and single-source:
        // no dominant signal, nothing to spread — expand nothing.
        let shape = build_test_evidence_shape(4, 1, false, 1);
        let (strategy, _) =
            decide_expansion_strategy(&Intent::KnowledgeQuery, SynthesisRoute::FastFocused, &shape);
        assert_eq!(strategy, ExpansionStrategy::NoExpansion);
    }
}
