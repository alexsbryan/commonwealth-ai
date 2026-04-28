use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::{Stream, StreamExt};

use crate::context::{build_context, format_history_as_prompt};
use crate::error::{Error, Result};
use crate::executor::{Executor, TaskContext};
use crate::memory;
use crate::query_session::{SessionStore, SharedSessionStore};
use crate::registry::ToolRegistry;
use crate::skills::SkillRegistry;
use crate::traits::{
    ApprovalChannel, InferenceProvider, NoOpRoutingEventSink, Planner, Router,
    RoutingEventSink, StateStore,
};
use crate::types::*;

/// Maximum characters of knowledge context to inject into prompts.
/// ~1000 tokens at ~4 chars/token, leaving room for history + system + response.
const MAX_KNOWLEDGE_CHARS: usize = 4000;

/// Truncate per-chunk content to produce a budget for the total knowledge context.
const MAX_CHUNK_CHARS: usize = 600;

/// Hard ceiling on the size of a single user turn's message.
///
/// ~16k chars ≈ 4k tokens. Keeps every downstream Fast-slot call
/// (working-memory compression, topic-context extraction, router
/// classification, query embedding) safely under typical 8k-token
/// context even when combined with conversation history + system
/// prompts. A 20-page document pasted as a message body is
/// ~40k tokens — it used to hang the pipeline for minutes before
/// this guard; now it errors cleanly and the user sees a hint to
/// use the document-attach flow instead.
///
/// Document-sized inputs belong in the `[Document attached: ...]`
/// prefix path, which routes through map-reduce and scales to
/// arbitrary length.
pub const MAX_TURN_MESSAGE_CHARS: usize = 16_000;

/// Error text shown when a message exceeds `MAX_TURN_MESSAGE_CHARS`.
/// Surfaced unchanged to the user via the Tauri command layer, so it
/// needs to be action-guidance, not a stack trace.
pub(crate) const OVERSIZE_MESSAGE_HINT: &str =
    "This message is too long for the chat pipeline (over 16,000 characters). \
     For document-sized content, attach it as a file instead — Sovereign \
     routes attachments through a map-reduce pipeline designed for long \
     inputs. Or summarise your question into a paragraph or two.";

/// Prepended to all Primary-slot (Speed::Slow) completions.
/// Sets the epistemic contract for fact-based and synthesis responses.
const PRIMARY_BASE_SYSTEM_PROMPT: &str = "You are a precise local assistant with access to \
installed knowledge bases. Accuracy is your highest priority.\n\n\
On factual questions:\n\
- If you are not certain of a specific name, number, date, or list item, say so explicitly. \
\"I am not certain of the complete roster\" is a correct and useful answer. \
A confident but incomplete list is not.\n\
- Never complete a list you do not fully know. A partial list labelled as partial is more \
useful than a fabricated full list.\n\
- If a knowledge base search has been provided, prefer it over memory. \
If the search contradicts your training data, trust the search.\n\n\
On uncertainty:\n\
- \"I don't know\" is an acceptable answer. \"I'm not certain, but...\" followed by \
clearly-labelled general knowledge is acceptable.\n\
- Fabricating specific facts (names, statistics, dates, roster members) to fill a gap \
is never acceptable, even if it would make the response sound more complete.";

/// System prompt for KnowledgeQuery synthesis — three-tier confidence framework.
///
/// Tier 1 (Retrieved): Claims drawn from passages, cited with [Source: title].
/// Tier 2 (Parametric): General knowledge — NO source tag, even when a related source exists.
/// Tier 3 (Inference): Reasoning beyond firm ground, hedged explicitly.
///
/// The attribution rules are calibrated against two opposing failure modes:
///
/// 1. Loose citation: attaching `[Source: X]` to a fact the model pulled from
///    training. Destroys the signal that citations are supposed to carry.
/// 2. Empty citation: emitting no citations at all. Breaks the UI's clickable-
///    citation renderer and makes the retrieval effort invisible to the user.
///
/// Target: cite RETRIEVED claims frequently (every retrieved claim gets a
/// tag), cite PARAMETRIC claims never, never fabricate-and-cite.
const KNOWLEDGE_SYNTHESIS_SYSTEM: &str = "\
You have been given retrieved passages from an installed knowledge base. \
Use them together with your general knowledge to answer the question.\n\
\n\
Three tiers of knowledge, each presented differently:\n\
\n\
RETRIEVED — claims drawn from the passages below.\n\
  Attach [Source: title] immediately after each retrieved claim. Cite \
  LIBERALLY: if the claim came from the passages, tag it. Under-citing \
  retrieved content is a bug — it makes the retrieved evidence invisible \
  to the reader. Tag the claim, not every sentence in a paragraph; one \
  citation per paragraph is fine when all the claims trace to the same \
  source.\n\
\n\
PARAMETRIC — your general knowledge about the topic. Present naturally \
  in prose. DO NOT attach [Source: ...] tags to parametric claims, EVEN \
  WHEN a related source appears in the passages. A [Source: X] tag on \
  parametric content falsely signals \"verified against the corpus\" — \
  reserve it for retrieved claims only.\n\
\n\
INFERENCE — reasoning beyond what sources or general knowledge firmly \
  establish. Introduce with hedged language: \"Drawing from this \
  framework...\", \"This suggests...\", \"The likely position would be...\"\n\
\n\
Example — follow this citation pattern:\n\
  The Cambridge Capital Controversy exposed flaws in neoclassical \
  production functions [Source: Joan Robinson]. Robinson was \
  \"technically vindicated\" but the profession continued using the \
  flawed models anyway [Source: Joan Robinson]. She also taught at \
  Girton College from 1937 — a fact from general knowledge, carrying \
  no source tag because the passages don't state it.\n\
\n\
Notice how every claim drawn from the passages earns a [Source: X] \
tag, while the parametric claim (Girton) carries none. That's the \
target pattern — apply it to your answer.\n\
\n\
Anti-fabrication guardrails:\n\
- NEVER invent an authorship, date, quotation, book title, statistic, or \
  roster and attach a citation to it. If you are unsure whether someone \
  wrote a particular book, say \"I believe\" or \"is often associated \
  with\" rather than asserting authorship.\n\
- Chunks may be cut mid-sentence by the retrieval layer. If a chunk \
  appears to attribute a book or fact in a way that contradicts or \
  surprises your training knowledge, TRUST YOUR TRAINING on the \
  factual attribution and do not assert the chunk's reading. Example: \
  a chunk that reads \"Author X\\n\\nBook Y\" is not necessarily \
  claiming X wrote Y — it may be a title heading followed by a \
  continuing sentence.\n\
- Do not refuse to engage because retrieval was incomplete.\n\
- Do not use [unverified] tags.\n\
- If retrieval found nothing relevant, say so in one sentence, then \
  answer from your general knowledge (with no source tags).\n\
- NEVER invent or complete a list, roster, or statistic you do not fully \
  know.\n\
- CRITICAL — if neither the retrieved passages nor your confident \
  general knowledge cover the specific thing the user asked about, say \
  \"I don't have reliable information on this\" and stop. Do NOT \
  invent a plausible-sounding origin, lineage, author, date, \
  organisation, or framework. A confident-sounding fabrication is \
  worse than an honest 'I don't know' — it poisons the user's mental \
  model of what's real. If the phrase the user used (e.g. a specific \
  project name, person, API) is not something you can speak to with \
  concrete factual confidence, say so plainly.";

/// Thinking directive — orients `<think>` toward substantive reasoning.
///
/// Without this, models default to source-adequacy bookkeeping in their
/// thinking blocks ("Source Analysis: [X] — no substantive content...").
/// This directive redirects the thinking budget toward the intellectual
/// content of the question.
const THINKING_DIRECTIVE: &str = "\
In your <think> block, reason about the SUBSTANCE of the question:\n\
1. What does this question actually ask? What would a complete answer contain?\n\
2. What do the retrieved sources contribute — which specific claims do they ground?\n\
3. What do I know well enough to state directly, even without retrieved support?\n\
4. Where are the genuine gaps — things I am uncertain about or where both \
   sources and my knowledge fall short?\n\
5. How should I frame what I know vs. what I'm inferring vs. what I'm uncertain about?\n\
\n\
Spend your thinking on the substance of the question.\n\
Source inventory (\"source X discusses Y\") belongs in a single brief scan, \
not as the primary content of your reasoning.";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Truncate `content` to at most `max_bytes`, breaking on a word
/// boundary when possible and appending `"..."`.
///
/// Byte index `max_bytes` may land inside a multi-byte UTF-8 scalar
/// (em-dash `—` is 3 bytes, smart quotes 3 bytes, emoji 4). A naive
/// `&content[..max_bytes]` panics `"byte index N is not a char
/// boundary"`. When that panic fires inside the spawned streaming
/// task the mpsc channel drops with zero tokens emitted and the
/// desktop UI sits inert — exactly the failure mode observed on the
/// Joan Robinson turn after source-expansion started pulling chunks
/// containing em-dashes. Walk backward to the nearest char boundary
/// before slicing; if we also find a word boundary within the
/// remaining content, prefer that for readability.
fn truncate_with_ellipsis(content: &str, max_bytes: usize) -> String {
    if content.len() > max_bytes {
        let mut cut = max_bytes;
        while cut > 0 && !content.is_char_boundary(cut) {
            cut -= 1;
        }
        let truncated = &content[..cut];
        match truncated.rfind(' ') {
            Some(pos) => format!("{}...", &truncated[..pos]),
            None => format!("{truncated}..."),
        }
    } else {
        content.to_string()
    }
}

/// Shorthand for the prompt-context truncation budget.
fn truncate_chunk_content(content: &str) -> String {
    truncate_with_ellipsis(content, MAX_CHUNK_CHARS)
}

/// Rescale each chunk's `score` by the max score observed in its
/// own corpus. Result: every corpus's top hit lands at 1.0,
/// lower-ranked hits scale proportionally within their corpus, and
/// cross-corpus comparison becomes meaningful.
///
/// Why this is needed: FTS5 BM25 scores depend on corpus-specific
/// IDF weights and document-length statistics. On a two-corpus
/// setup where one is a 1k-row code index and the other is a
/// 188k-row SEP prose index, a query like
/// *"is free will compatible with determinism?"* can score a code
/// test function at 21 (because the test name's underscore-split
/// tokens are rare enough in the small corpus to fire high IDF)
/// while SEP's genuine `compatibilism` match scores 19. Naive
/// `sort_by(score)` then ranks the test function above the
/// philosophy passage — visible in the UI as a top-listed code
/// chunk for a pure-philosophy query.
///
/// Max-based normalisation is the least-surgical fix. Alternatives
/// considered and rejected:
///   * **Round-robin by corpus**: loses score fidelity; a corpus
///     with only mediocre hits gets equal billing to one with
///     strong hits.
///   * **Z-score**: needs mean + stddev, which are unreliable on
///     5-to-8-element distributions.
///   * **Query/corpus classifier**: the "right" answer, but needs
///     labelled training data and a judgement model.
///
/// This is a heuristic: it doesn't *know* philosophy queries should
/// prefer SEP, but it does prevent one outlier in a small corpus
/// from monopolising the top-N. In-context, that's enough — the
/// synthesis model sees evidence from every corpus that had a real
/// match, weighted by within-corpus rank.
pub(crate) fn normalise_scores_per_corpus(chunks: &mut [corpus_engine::ScoredChunk]) {
    use std::collections::HashMap;
    let mut max_per_corpus: HashMap<String, f32> = HashMap::new();
    for c in chunks.iter() {
        let entry = max_per_corpus.entry(c.corpus_id.clone()).or_insert(c.score);
        if c.score > *entry {
            *entry = c.score;
        }
    }
    for c in chunks.iter_mut() {
        if let Some(&max) = max_per_corpus.get(&c.corpus_id) {
            if max > 0.0 {
                c.score /= max;
            }
        }
    }
    tracing::debug!(
        corpora = ?max_per_corpus.keys().collect::<Vec<_>>(),
        "runtime: normalised per-corpus scores before global merge"
    );
}

// ─── Evidence-shape routing (KnowledgeQuery) ─────────────────────────
//
// After retrieval, we compute a handful of cheap numeric signals over the
// top-k chunks and use them to decide between the Fast slot (tight
// summarise-from-one-source) and the Primary slot (thinking + full budget
// for genuine cross-source synthesis). The decision is logged transparently
// so thresholds can be tuned against real traffic without guessing.

/// Minimum token length for a query word to count toward title-match.
/// Short tokens like "the", "and", "can", "you" are ignored regardless.
const EVIDENCE_TITLE_MIN_TOKEN_LEN: usize = 4;

/// `top1_score / median(top_k_scores)` above this ratio marks the
/// retrieval as *concentrated* — the top hit stands clearly above the
/// middle of the distribution. Median (not top-3) because a single
/// high-scoring but irrelevant neighbor (e.g. a conversation-history
/// chunk that vector-matches the query phrasing) can drag top-3 up
/// and kill the signal. Median is robust to one noisy neighbor.
const EVIDENCE_MEDIAN_RATIO_THRESHOLD: f32 = 1.5;

/// Minimum chunk count in the top-k sharing the same `(corpus_id, title)`
/// as the top chunk, for "single source owns this" to fire. 2+ hits to the
/// same document is a strong single-source signal even without title_match.
const EVIDENCE_MIN_TOP_SOURCE_REPEAT: usize = 2;

/// Decisive threshold: this many repeats of the top source in top-k routes
/// Fast regardless of other signals — the retrieval has clearly landed on
/// one document multiple times. Cheaper than re-deriving median_ratio on
/// edge cases.
const EVIDENCE_DECISIVE_TOP_SOURCE_REPEAT: usize = 3;

/// Fast-path output budget. Enough for a focused summary with citations,
/// not enough to invite the model to ramble. Pairs with `think_budget = 0`.
const FAST_KNOWLEDGE_MAX_TOKENS: u32 = 600;

/// When evidence-shape routes FastFocused and a single source dominates,
/// pull up to this many chunks from that source by title (cohesion, not
/// query similarity). Calibrated for an Obsidian note or Wikipedia article
/// — typical long-form sources have 8–15 chunks so 12 captures most
/// without forcing us to truncate narratively.
const EXPANSION_MAX_FROM_TOP_SOURCE: usize = 12;

/// Non-dominant chunks to keep alongside expanded dominant-source chunks,
/// so the model has grounding breadth (e.g. a contradicting viewpoint, a
/// corroborating passage from a different corpus). 2 is enough to signal
/// "other sources exist" without diluting the dominant narrative.
const EXPANSION_GROUNDING_CHUNKS: usize = 2;

/// Context budget when source-expansion has fired. Raw `MAX_KNOWLEDGE_CHARS`
/// (4000) fits ~8 chunks; expansion needs room for ~12 dominant + 2
/// grounding, each ~500 chars after truncation. 8000 chars ≈ 2k prompt
/// tokens, which adds ~2.7s of prompt_eval on Metal — a worthwhile trade
/// for answers built from the whole source note instead of fragments.
const EXPANDED_KNOWLEDGE_CHARS: usize = 8000;

/// Numeric signals computed from the retrieved chunks. Emitted as one
/// structured `tracing::info!` line per turn so operators can see how
/// the router chose its path.
///
/// Calibration notes (raw RRF scores from hybrid-search):
/// - Rank-1 with both vector+FTS hits ≈ 0.033 (1/60 + 1/60).
/// - Rank-1 with only vector OR only FTS ≈ 0.0167 (1/60).
/// - Single-doc lookups typically see top_source_repeat ≥ 2 and
///   median_ratio ≥ 1.8.
/// - Multi-source synthesis typically sees top_source_repeat = 1 and
///   median_ratio ≤ 1.2.
#[derive(Debug, Clone)]
pub struct EvidenceShape {
    count: usize,
    top1_score: f32,
    median_score: f32,
    /// `top1_score / median_score`. ∞ when median is zero.
    median_ratio: f32,
    /// Count of chunks in top-k with the same `(corpus_id, title)` as
    /// the top chunk. ≥ 2 means the same document shows up multiple
    /// times, which is the strongest single-source signal we have.
    top_source_repeat_count: usize,
    distinct_sources: usize,
    title_match: bool,
    /// `(corpus_id, title)` of the top-scoring chunk — the identity
    /// the source-expansion path uses to pull more chunks from the
    /// dominant document. Empty when chunks is empty.
    top_source_key: (String, String),
    /// Human-readable `corpus_id::title` for logging only.
    top_source_label: String,
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
        top_source_key: ("test-corpus".to_string(), "Test Note".to_string()),
        top_source_label: "test-corpus::Test Note".to_string(),
    }
}

impl EvidenceShape {
    /// PR5 — retrieval-miss signal.
    ///
    /// Returns `true` when retrieval happened but the results are
    /// clearly dispersed noise — chunks spread across many corpora,
    /// no source concentration, no title match. The classic
    /// "the corpora didn't have what the user asked about, but the
    /// hybrid scorer returned something anyway" shape.
    ///
    /// Triggered in practice when:
    ///   - User asks about a specific niche topic (e.g. a project
    ///     name) the installed corpora don't cover.
    ///   - The query's embeddings vaguely correlate with chunks in
    ///     unrelated corpora (Machiavelli, political theory, code)
    ///     and every corpus ships 2 chunks that nominally scored.
    ///
    /// On `true`, the runtime diverts to an Ask-tier clarification
    /// card rather than committing to synthesis against off-target
    /// evidence (which is the shape that produces confident
    /// fabrication).
    ///
    /// Deliberately conservative — all three conditions must hold:
    ///   - retrieval returned at least one chunk (empty is not a
    ///     "miss", it's a "no data" case handled upstream),
    ///   - no title of a retrieved chunk matches the query
    ///     (`title_match == false`),
    ///   - no source concentration
    ///     (`top_source_repeat_count < EVIDENCE_MIN_TOP_SOURCE_REPEAT`),
    ///   - results are fanned out across ≥ 3 distinct corpora.
    fn is_off_target(&self) -> bool {
        self.count > 0
            && !self.title_match
            && self.top_source_repeat_count < EVIDENCE_MIN_TOP_SOURCE_REPEAT
            && self.distinct_sources >= 3
    }
}

/// Which synthesis path to take given the evidence shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SynthesisRoute {
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
fn chunk_source_key(c: &corpus_engine::ScoredChunk) -> (String, String) {
    (
        c.corpus_id.clone(),
        c.title.clone().unwrap_or_default(),
    )
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
fn is_grounding_candidate(chunk: &corpus_engine::ScoredChunk) -> bool {
    if chunk.corpus_id == "conversation-history" {
        return false;
    }
    let title = chunk.title.as_deref().unwrap_or("");
    !title.trim().is_empty()
}

/// Extract ≥N-char tokens from `text`, lowercased, stopwords removed.
fn extract_tokens(text: &str, min_len: usize) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "about", "above", "after", "again", "also", "been", "being",
        "both", "could", "does", "doing", "down", "each", "from", "have",
        "having", "here", "just", "like", "make", "many", "more", "most",
        "much", "need", "only", "other", "over", "should", "some", "such",
        "tell", "than", "that", "their", "them", "then", "there", "these",
        "they", "this", "those", "upon", "very", "want", "were", "what",
        "when", "where", "which", "while", "will", "with", "would", "your",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= min_len)
        .map(|s| s.to_lowercase())
        .filter(|s| !STOPWORDS.contains(&s.as_str()))
        .collect()
}

/// Median of `values`. Assumes non-empty; callers must guard.
fn median_f32(values: &[f32]) -> f32 {
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
fn compute_evidence_shape(chunks: &[corpus_engine::ScoredChunk], query: &str) -> EvidenceShape {
    if chunks.is_empty() {
        return EvidenceShape {
            count: 0,
            top1_score: 0.0,
            median_score: 0.0,
            median_ratio: 0.0,
            top_source_repeat_count: 0,
            distinct_sources: 0,
            title_match: false,
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

    let title_match = {
        let title = chunks[0].title.as_deref().unwrap_or("");
        if title.is_empty() {
            false
        } else {
            let title_tokens = extract_tokens(title, EVIDENCE_TITLE_MIN_TOKEN_LEN);
            let query_tokens = extract_tokens(query, EVIDENCE_TITLE_MIN_TOKEN_LEN);
            query_tokens.iter().any(|q| title_tokens.iter().any(|t| t == q))
        }
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
fn route_from_evidence(shape: &EvidenceShape) -> SynthesisRoute {
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
fn strip_leading_title_duplicate<'a>(body: &'a str, title: Option<&str>) -> &'a str {
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

/// Build a truncated knowledge context string from corpus-engine scored chunks,
/// grouped by provenance tier (corpus vs web) and staying within a character budget.
fn format_scored_chunks(chunks: &[corpus_engine::ScoredChunk], max_chars: usize) -> String {
    let mut corpus_parts = Vec::new();
    let mut web_parts = Vec::new();
    let mut total = 0;

    for c in chunks {
        let body = strip_leading_title_duplicate(&c.content, c.title.as_deref());
        let content = truncate_chunk_content(body);
        let title = c.title.as_deref().unwrap_or(c.corpus_id.as_str());

        let (label, bucket) = if c.url.is_some() {
            (format!("[Web: {title}]"), &mut web_parts)
        } else {
            (format!("[Source: {title}]"), &mut corpus_parts)
        };

        let part = format!("{label}\n{content}");
        let part_len = part.len() + 5; // account for separator

        if total + part_len > max_chars {
            break;
        }

        total += part_len;
        bucket.push(part);
    }

    let mut sections = Vec::new();
    if !corpus_parts.is_empty() {
        sections.push(format!(
            "## From knowledge base\n\n{}",
            corpus_parts.join("\n\n---\n\n")
        ));
    }
    if !web_parts.is_empty() {
        sections.push(format!(
            "## From web search\n\n{}",
            web_parts.join("\n\n---\n\n")
        ));
    }

    if sections.is_empty() {
        String::new()
    } else {
        sections.join("\n\n")
    }
}

/// Shared body of [`Runtime::maybe_collaborate`]. Factored out so the
/// streaming spawn (which doesn't hold a live `&self`) can invoke the
/// same logic via owned `Arc`s. See the method's doc comment for
/// behaviour; this function is called whether or not `auto_collaborate`
/// is enabled — it no-ops when disabled.
pub(crate) async fn run_collaboration(
    inference: &dyn InferenceProvider,
    approval: &dyn ApprovalChannel,
    inference_config: &InferenceConfig,
    conversation_id: &str,
    question: &str,
    response: &str,
    evidence: &str,
) -> String {
    if !inference_config.auto_collaborate {
        return response.to_string();
    }

    let t_start = std::time::Instant::now();

    // 1. Ask the gap-identifier whether anything external would sharpen
    //    the answer. Conservative on any error — we never want this
    //    hook to fail the turn.
    let gap = match crate::gap::identify_gap(inference, question, response, evidence).await {
        Ok(Some(req)) => req,
        Ok(None) => {
            tracing::info!(
                latency_ms = t_start.elapsed().as_millis() as u64,
                "maybe_collaborate: no gap identified — passing through"
            );
            return response.to_string();
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "maybe_collaborate: gap check failed — passing through"
            );
            return response.to_string();
        }
    };

    // 2. Stamp task/step on the request so the UI can correlate it
    //    with the current conversation.
    let mut req = gap;
    req.task_id = conversation_id.to_string();
    req.step_id = 0;

    tracing::info!(
        gap_chars = req.gap.len(),
        "maybe_collaborate: surfacing information request"
    );

    // 3. Surface the card and wait for the user.
    let user_content = approval.request_information(&req).await;
    let content = match user_content {
        Some(c) if !c.trim().is_empty() => c,
        _ => {
            tracing::info!(
                latency_ms = t_start.elapsed().as_millis() as u64,
                "maybe_collaborate: user skipped or provided no content"
            );
            return response.to_string();
        }
    };

    // 4. Refinement synthesis — integrate the user's source. The prompt
    //    asks the model to distinguish corpus-derived content from
    //    user-provided content so provenance stays visible.
    let refine_prompt = format!(
        "The user asked: {question}\n\n\
         Your initial answer (drawn from the local corpus):\n{response}\n\n\
         Additional source the user provided:\n{content}\n\n\
         Refine the answer to integrate the user's source. Be explicit \
         about what came from the corpus vs. what came from the user's \
         source. Mark anything that remains uncertain."
    );

    let refine_req = CompletionRequest {
        prompt: refine_prompt,
        system_message: None,
        preferred_speed: Speed::Slow,
        max_tokens: Some(inference_config.max_tokens),
        temperature: Some(inference_config.temperature),
        think_budget: Some(inference_config.think_budget),
        structured_output: None,
        top_k: inference_config.top_k,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
                    model_id: None,
    };

    match inference.complete(&refine_req).await {
        Ok(c) => {
            tracing::info!(
                had_user_content = true,
                latency_ms = t_start.elapsed().as_millis() as u64,
                refined_chars = c.text.len(),
                "maybe_collaborate: refined answer produced"
            );
            c.text
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "maybe_collaborate: refinement inference failed — falling back to original"
            );
            response.to_string()
        }
    }
}

/// Post-stream refinement primitive: run the gap check and, if the
/// user provides content, overwrite the saved assistant message and
/// emit `message-refined`. Called both from `handle_message_stream`'s
/// spawn (which has owned `Arc`s but no live `&self`) and from the
/// corresponding method on `Runtime`.
pub(crate) async fn run_post_stream_refinement(
    inference: &dyn InferenceProvider,
    approval: &dyn ApprovalChannel,
    store: &dyn StateStore,
    inference_config: &InferenceConfig,
    conversation_id: &str,
    message_id: &str,
    question: &str,
    original_content: &str,
    evidence: &str,
    original_metadata: Option<serde_json::Value>,
) -> Option<String> {
    let refined = run_collaboration(
        inference,
        approval,
        inference_config,
        conversation_id,
        question,
        original_content,
        evidence,
    )
    .await;
    if refined == original_content {
        return None;
    }

    let updated = Message {
        id: message_id.to_string(),
        conversation_id: conversation_id.to_string(),
        role: Role::Assistant,
        content: refined.clone(),
        created_at: now(),
        metadata: original_metadata,
        version: now(),
    };
    if let Err(e) = store.save_message(&updated).await {
        tracing::warn!(
            error = %e,
            message_id = %message_id,
            "post-stream refinement: save_message failed"
        );
        return None;
    }

    approval.emit_message_refined(MessageRefinedPayload {
        conversation_id: conversation_id.to_string(),
        message_id: message_id.to_string(),
        new_content: refined.clone(),
    });
    Some(refined)
}

/// Pre-computed knowledge context shared between streaming and non-streaming
/// response paths. Produced by [`Runtime::prepare_knowledge_context`] so the
/// two paths cannot diverge in how they search, build prompts, or report
/// provenance.
struct KnowledgeContext {
    chunks: Vec<corpus_engine::ScoredChunk>,
    prompt: String,
    system: String,
    speed: Speed,
    search_method: Option<String>,
    sources: Vec<SourceSummary>,
    /// Summaries of retrieved chunks for frontend source linking.
    retrieved_chunks: Vec<serde_json::Value>,
}

/// Everything `handle_knowledge_query` and the streaming KQ branch need
/// to issue a synthesis request. Produced by
/// [`Runtime::prepare_knowledge_query_plan`] so the two paths cannot
/// diverge in retrieval, expansion, or routing behaviour.
///
/// On the empty-retrieval path, `chunks` / `doc_context` /
/// `retrieved_chunks` / `source_map` are all empty and `result_quality`
/// is `"empty"`. The `request` is a parametric-knowledge prompt rather
/// than a retrieval-grounded one.
struct KnowledgeQueryPlan {
    request: CompletionRequest,
    chunks: Vec<corpus_engine::ScoredChunk>,
    /// Formatted chunk text used as evidence for the gap check.
    /// Empty string on the parametric path.
    doc_context: String,
    shape: EvidenceShape,
    route: SynthesisRoute,
    gap_check_enabled: bool,
    search_ms: u64,
    retrieved_chunks: Vec<serde_json::Value>,
    source_map: HashMap<String, usize>,
    /// `"empty"` | `"focused"` | `"synthesis"` | `"routed"` —
    /// surfaced in message metadata for the UI to label the turn.
    result_quality: &'static str,
}

/// Streaming handle returned by [`Runtime::handle_message_stream`].
///
/// Holds the assistant message id (assigned up-front so callers can correlate
/// chunks) and a stream of text chunks. The runtime persists the full message
/// to the store after the stream is exhausted.
pub struct StreamHandle {
    pub message_id: String,
    pub stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
}

/// Intent-implied OICP defaults (v0.3). The classified intent
/// carries a latency signal — "DeepQuery" wants extended thinking
/// budget, "ComplexTask" and "KnowledgeQuery" want solid normal
/// latency — which the scheduler consumes as `latency_class`.
/// `capability_hint` defaults to `general`; code/prose/etc. are left
/// to skill-level overrides since the intent vocabulary doesn't
/// carry a specialization distinction.
///
/// Returns `None` for small-model intents (SimpleQuery, Continuation,
/// SimpleAction) where cross-network latency wouldn't be worth
/// trading for a marginal quality bump — no OICP envelope means
/// the local Fast slot serves without invoking the scheduler.
fn default_oicp_for_intent(intent: &Intent) -> Option<crate::oicp::InferenceRequirements> {
    use crate::oicp::{CapabilityHint, InferenceRequirements, LatencyClass};
    let (hint, latency_class) = match intent {
        Intent::DeepQuery => {
            // Reasoning-heavy: extended class tolerates higher TTFT
            // in exchange for deeper thinking budgets.
            (CapabilityHint::general(), LatencyClass::Extended)
        }
        Intent::ComplexTask => {
            // Tool-using plans want solid normal-latency responses;
            // extended would add round-trip overhead per tool step.
            (CapabilityHint::general(), LatencyClass::Normal)
        }
        Intent::KnowledgeQuery => {
            // Retrieval-driven synthesis over a bounded chunk set.
            (CapabilityHint::general(), LatencyClass::Normal)
        }
        Intent::SimpleQuery
        | Intent::SimpleAction { .. }
        | Intent::Continuation { .. } => {
            return None;
        }
    };
    Some(
        InferenceRequirements::new()
            .with_hint(hint)
            .with_latency_class(latency_class),
    )
}

/// Produce a short human-readable banner for `interpretation-proposed`.
/// Runs without a model call — we'd like the banner to appear before
/// the first token, so an extra Fast-slot turn for phrasing would
/// defeat the "under 2s immediate engagement" requirement.
fn format_interpretation(
    _message: &str,
    primary: &Intent,
    rationale: Option<&str>,
) -> String {
    let intent_phrase = match primary {
        Intent::SimpleQuery => "a quick factual answer",
        Intent::DeepQuery => "a deeper explanation",
        Intent::KnowledgeQuery => "a look in your installed knowledge",
        Intent::SimpleAction { .. } => "a tool call",
        Intent::ComplexTask => "a multi-step task",
        Intent::Continuation { .. } => "a follow-up to earlier work",
    };
    if let Some(r) = rationale {
        format!("I'm reading this as {intent_phrase} ({r}). If that's off, redirect below.")
    } else {
        format!("I'm reading this as {intent_phrase}. If that's off, redirect below.")
    }
}

/// Human label for a redirect chip on the banner.
fn label_for_intent(intent: &Intent) -> String {
    match intent {
        Intent::SimpleQuery => "Give me a quick answer".into(),
        Intent::DeepQuery => "Walk me through it in depth".into(),
        Intent::KnowledgeQuery => "Check my knowledge base".into(),
        Intent::SimpleAction { tool } => format!("Use the {tool} tool"),
        Intent::ComplexTask => "Plan a multi-step task".into(),
        Intent::Continuation { .. } => "Continue prior task".into(),
    }
}

/// Wire-form `Intent` hint used by the desktop → runtime redirect
/// payload. Converting at this boundary keeps
/// [`InterpretationProposed`] and [`ClarificationOption`] trivially
/// serializable — the full `Intent` enum carries a `ToolId` for
/// `SimpleAction`, which is ergonomic in Rust but awkward in JSON.
fn intent_hint(intent: &Intent) -> String {
    match intent {
        Intent::SimpleQuery => "simple_query".into(),
        Intent::DeepQuery => "deep_query".into(),
        Intent::KnowledgeQuery => "knowledge_query".into(),
        Intent::SimpleAction { tool } => format!("simple_action:{tool}"),
        Intent::ComplexTask => "complex_task".into(),
        Intent::Continuation { task_id } => format!("continuation:{task_id}"),
    }
}

/// Inverse of [`intent_hint`] — decode a wire-form hint back into
/// an `Intent`. Unknown variants fall back to `SimpleQuery` so the
/// continuation path never hard-fails; the caller logs the case.
fn parse_intent_hint(hint: &str) -> Intent {
    match hint {
        "simple_query" => Intent::SimpleQuery,
        "deep_query" => Intent::DeepQuery,
        "knowledge_query" => Intent::KnowledgeQuery,
        "complex_task" => Intent::ComplexTask,
        _ if hint.starts_with("simple_action:") => {
            let tool = hint.trim_start_matches("simple_action:").to_string();
            Intent::SimpleAction {
                tool: ToolId::from(tool),
            }
        }
        _ if hint.starts_with("continuation:") => {
            let task_id = hint.trim_start_matches("continuation:").to_string();
            Intent::Continuation {
                task_id: TaskId::from(task_id),
            }
        }
        _ => {
            tracing::warn!(hint, "parse_intent_hint: unknown hint, falling back to SimpleQuery");
            Intent::SimpleQuery
        }
    }
}

/// Build a one-sentence clarifying question for the `Ask` move.
/// Kept short and neutral — the alternatives themselves do most of
/// the disambiguation work; the question just frames the choice.
fn build_clarification_question(_message: &str, primary: &Intent) -> String {
    let read_as = match primary {
        Intent::SimpleQuery => "a quick factual answer",
        Intent::DeepQuery => "a deeper explanation",
        Intent::KnowledgeQuery => "a corpus lookup",
        Intent::SimpleAction { .. } => "an action",
        Intent::ComplexTask => "a multi-step task",
        Intent::Continuation { .. } => "a continuation",
    };
    format!(
        "I could approach this a few ways — my best read is {read_as}, \
         but could you pick what you'd like most?"
    )
}

pub struct Runtime {
    pub inference: Arc<dyn InferenceProvider>,
    pub router: Box<dyn Router>,
    pub planner: Box<dyn Planner>,
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
    pub skills: Arc<SkillRegistry>,
    pub approval: Arc<dyn ApprovalChannel>,
    pub inference_config: InferenceConfig,
    pub corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
    /// Optional mesh-knowledge client. Populated by the desktop
    /// bootstrap when an `EmbeddedDaemon` is running — the Runtime
    /// fans out knowledge queries through its local Commonwealth
    /// daemon at `127.0.0.1:9741/v1/knowledge/search`, which then
    /// searches local + peer corpora. `None` means "no mesh" — the
    /// standalone (pre-mesh) behavior is preserved exactly.
    pub mesh_knowledge: Option<Arc<dyn crate::traits::MeshKnowledgeSource>>,
    /// Optional [`LandscapeDigestProvider`][crate::traits::LandscapeDigestProvider]
    /// (typically the `sovereign-tools` `KnowledgeViewManager`). When
    /// present, Runtime calls it after routing to splice
    /// `knowledge_view_digests` onto the `ConversationContext` — the
    /// landscape-of-terrain summary consumed by the prompt assembly
    /// layer. `None` = pre-KnowledgeView behaviour preserved exactly;
    /// digests stay `None` and the context carries only memories and
    /// corpus chunks.
    pub landscape_digests: Option<Arc<dyn crate::traits::LandscapeDigestProvider>>,
    /// In-memory per-turn scratch store for antifragile routing. Holds
    /// the `RouterClassification` + `RoutingPolicy` + cancellation
    /// token for the in-flight turn; PR2 will also cache retrieval
    /// and partial response so redirects can reuse work. Populated on
    /// every `classify` return; GC'd on next turn or after 30s.
    pub sessions: SharedSessionStore,
    /// Active confidence thresholds. Defaults (0.80 / 0.55) ship with
    /// every Runtime unless overridden by the host. PR4 will mutate
    /// this from structural-signal calibration; PR1 reads it verbatim.
    pub confidence_thresholds: ConfidenceThresholds,
    /// Sink for the three antifragile-routing UI events
    /// (interpretation-proposed, clarification-request, turn-narration).
    /// Desktop bootstrap injects a `TauriRoutingEventSink`; headless
    /// test/CLI harnesses get the default `NoOpRoutingEventSink`.
    pub routing_events: Arc<dyn RoutingEventSink>,
}

impl Runtime {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        router: Box<dyn Router>,
        planner: Box<dyn Planner>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn StateStore>,
        skills: Arc<SkillRegistry>,
        approval: Arc<dyn ApprovalChannel>,
        inference_config: InferenceConfig,
    ) -> Self {
        Self {
            inference,
            router,
            planner,
            tools,
            store,
            skills,
            approval,
            inference_config,
            corpus_engine: None,
            mesh_knowledge: None,
            landscape_digests: None,
            sessions: Arc::new(SessionStore::new()),
            confidence_thresholds: ConfidenceThresholds::default(),
            routing_events: Arc::new(NoOpRoutingEventSink),
        }
    }

    /// Install a `RoutingEventSink` to receive interpretation,
    /// clarification, and narration events. The desktop bootstrap
    /// calls this with a `TauriRoutingEventSink`; headless harnesses
    /// inherit the `NoOpRoutingEventSink` default from `new`.
    pub fn with_routing_events(
        mut self,
        sink: Arc<dyn RoutingEventSink>,
    ) -> Self {
        self.routing_events = sink;
        self
    }

    pub fn with_corpus_engine(mut self, engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        self.corpus_engine = Some(engine);
        self
    }

    /// Install a `KnowledgeView` landscape-digest provider. Typically
    /// the `sovereign-tools::knowledge_view::KnowledgeViewManager`,
    /// constructed alongside the `StateStore` so the same `Arc` can
    /// also be passed as a `StateStoreObserver`.
    ///
    /// Opt-in: leaving this `None` preserves the pre-KnowledgeView
    /// behaviour exactly. Test harnesses that don't wire KnowledgeView
    /// inherit the no-op.
    pub fn with_landscape_digests(
        mut self,
        provider: Arc<dyn crate::traits::LandscapeDigestProvider>,
    ) -> Self {
        self.landscape_digests = Some(provider);
        self
    }

    /// Install a mesh-knowledge client. Only called when the desktop
    /// has an `EmbeddedDaemon` actually running — tests and the
    /// bare CLI path leave this `None`, in which case
    /// `prepare_knowledge_context` behaves exactly as before
    /// (local-only search, `search_method = "LocalOnly"`).
    pub fn with_mesh_knowledge(
        mut self,
        mesh: Arc<dyn crate::traits::MeshKnowledgeSource>,
    ) -> Self {
        self.mesh_knowledge = Some(mesh);
        self
    }

    /// Search all installed corpus-engine LanceDB indexes.
    ///
    /// Returns scored chunks from every installed corpus. If the IVF-PQ
    /// vector index is not built for a corpus, passes an empty embedding
    /// to trigger FTS-only mode (fast Tantivy, avoids the 20–60 second
    /// O(n) full-scan fallback).
    ///
    /// Used by both `handle_knowledge_query` and `handle_simple` so that
    /// installed corpora enrich all intent types, not just KnowledgeQuery.
    async fn search_corpus_indexes(
        &self,
        embedding: &[f32],
        query_text: &str,
        limit: usize,
        label: &str,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let mut chunks = Vec::new();
        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => {
                tracing::warn!("{label}: corpus_engine is None — no corpus search possible");
                return chunks;
            }
        };
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(error = %e, "{label}: installed_indexes() failed");
                return chunks;
            }
        };
        if indexes.is_empty() {
            tracing::warn!("{label}: installed_indexes() returned 0 indexes — nothing to search");
        } else {
            tracing::info!(count = indexes.len(), "{label}: found corpus indexes");
        }

        // Filter 1 — drop Code corpora. Code indexes (produced by
        // `sovereign code index`) are served by the dedicated
        // symbol_lookup / code_search MCP tools; pulling them into
        // chat retrieval lets BM25 keyword overlap on tokens like
        // `main`, `argument`, or `democracy` drown out the actual
        // knowledge corpus for the turn. Keeping them completely
        // out of chat search is the default the user signed up for;
        // an explicit "search my code too" affordance can re-enable
        // them later with a scoped flag.
        let total_indexes = indexes.len();
        let indexes: Vec<_> = indexes
            .into_iter()
            .filter(|info| {
                if matches!(info.kind, corpus_engine::CorpusKind::Knowledge) {
                    true
                } else {
                    tracing::debug!(
                        corpus = %info.corpus_id,
                        kind = ?info.kind,
                        "{label}: skipping code corpus for chat retrieval"
                    );
                    false
                }
            })
            .collect();
        if indexes.len() < total_indexes {
            tracing::info!(
                knowledge = indexes.len(),
                code_skipped = total_indexes - indexes.len(),
                "{label}: filtered code corpora"
            );
        }

        // Filter 2 — drop dimension mismatches. A corpus built with
        // a different embedding model can't serve hybrid search for
        // the current query. When the query embedding is empty
        // (FTS-only path), skip this filter so every remaining
        // (knowledge) index serves its BM25 results.
        let query_dims = embedding.len();
        let total_indexes = indexes.len();
        let eligible: Vec<_> = if query_dims == 0 {
            indexes
        } else {
            indexes
                .into_iter()
                .filter(|info| {
                    if info.embedding_dimensions == query_dims {
                        true
                    } else {
                        tracing::debug!(
                            corpus = %info.corpus_id,
                            stored_dims = info.embedding_dimensions,
                            query_dims,
                            embedding_model = %info.embedding_model,
                            "{label}: skipping corpus — embedding-dimension mismatch"
                        );
                        false
                    }
                })
                .collect()
        };
        if eligible.len() < total_indexes {
            tracing::info!(
                eligible = eligible.len(),
                skipped = total_indexes - eligible.len(),
                query_dims,
                "{label}: dim-filtered index set"
            );
        }

        for info in &eligible {
            tracing::info!(
                corpus = %info.corpus_id,
                path = %info.path.display(),
                chunks = info.chunk_count,
                dims = info.embedding_dimensions,
                embedding_model = %info.embedding_model,
                "{label}: opening index"
            );
            let idx = match engine.open_index(&info.path).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: open_index failed");
                    continue;
                }
            };
            match idx.search(embedding, query_text, limit).await {
                Ok(scored) => {
                    tracing::info!(
                        corpus = %info.corpus_id,
                        results = scored.len(),
                        "{label}: search complete"
                    );
                    chunks.extend(scored);
                }
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: search failed");
                }
            }
        }
        chunks
    }

    /// Source-cohesion expansion.
    ///
    /// When the initial retrieval has clearly landed on a single
    /// dominant document, the best next move is to read THAT DOCUMENT,
    /// not to scatter across marginal matches from other corpora. This
    /// fetches up to `EXPANSION_MAX_FROM_TOP_SOURCE` chunks from the
    /// dominant source by exact title, merges them with the initial
    /// retrieval, dedupes by content, and keeps
    /// `EXPANSION_GROUNDING_CHUNKS` top-scoring non-dominant chunks
    /// for breadth.
    ///
    /// Returns the expanded chunk set (ready to feed to synthesis) and
    /// a structured event-shape tuple `(from_source, grounding,
    /// dropped_noise)` for glass-box logging.
    ///
    /// Preconditions: caller has computed an `EvidenceShape` and
    /// decided this case warrants expansion (FastFocused route +
    /// `top_source_repeat_count >= 2`). This function does not re-check
    /// those conditions — it just expands.
    async fn expand_from_dominant_source(
        &self,
        initial: Vec<corpus_engine::ScoredChunk>,
        shape: &EvidenceShape,
    ) -> (Vec<corpus_engine::ScoredChunk>, usize, usize, usize) {
        use std::collections::HashSet;

        let (top_corpus_id, top_title) = &shape.top_source_key;
        if top_corpus_id.is_empty() || top_title.is_empty() {
            // Nothing to expand — return initial unchanged.
            return (initial, 0, 0, 0);
        }

        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => return (initial, 0, 0, 0),
        };

        // Find the corpus's index path.
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "KnowledgeQuery: source expansion skipped — installed_indexes() failed"
                );
                return (initial, 0, 0, 0);
            }
        };
        let info = match indexes.iter().find(|i| &i.corpus_id == top_corpus_id) {
            Some(i) => i.clone(),
            None => {
                tracing::warn!(
                    top_corpus_id,
                    "KnowledgeQuery: source expansion skipped — corpus not found"
                );
                return (initial, 0, 0, 0);
            }
        };
        let idx = match engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(
                    top_corpus_id,
                    error = %e,
                    "KnowledgeQuery: source expansion skipped — open_index failed"
                );
                return (initial, 0, 0, 0);
            }
        };

        // Fetch by title. The score on returned chunks is uniform 1.0
        // (cohesion pull, not query-similarity) — don't confuse these
        // with RRF-scored search results.
        let t_fetch = std::time::Instant::now();
        let fetched = match idx
            .fetch_chunks_by_title(top_title, EXPANSION_MAX_FROM_TOP_SOURCE)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    top_corpus_id,
                    top_title,
                    error = %e,
                    "KnowledgeQuery: source expansion skipped — fetch_chunks_by_title failed"
                );
                return (initial, 0, 0, 0);
            }
        };
        let fetch_ms = t_fetch.elapsed().as_millis() as u64;

        // Dedupe: track contents we've already seen. The initial
        // retrieval's dominant-source chunks will collide with some of
        // the fetched ones — keep the fetched copy (which is in natural
        // document order) and drop the duplicates.
        let mut seen_contents: HashSet<String> = HashSet::new();
        let mut expanded_dominant: Vec<corpus_engine::ScoredChunk> = Vec::new();
        for c in fetched {
            if seen_contents.insert(c.content.clone()) {
                expanded_dominant.push(c);
            }
        }

        // From the initial retrieval, keep up to
        // EXPANSION_GROUNDING_CHUNKS chunks that are NOT from the
        // dominant source, in descending score order. These are the
        // "grounding" signals — "other sources discuss this too."
        //
        // Two classes of non-dominant chunks are skipped even when
        // they'd fit the budget:
        //
        // 1. `conversation-history` corpus chunks. These are previous
        //    user/assistant turns — a user message that happens to
        //    vector-match "Can you tell me about X" phrasing is not a
        //    corroborating source for a knowledge query, it's phrase
        //    noise. Including it invites the model to acknowledge it
        //    as a topical source and waste output tokens (observed on
        //    the Joan Robinson turn: a Schrödinger-PDF user message
        //    made the model append "Note: The question about
        //    summarizing Erwin Schrödinger's *..." and truncate
        //    against the 600-token cap).
        //
        // 2. Untitled chunks (empty `title`). Real knowledge sources
        //    have titles. Untitled rows are almost always raw
        //    messages, system fragments, or extraction artifacts —
        //    not sources worth citing.
        let dominant_key = shape.top_source_key.clone();
        let mut grounding: Vec<corpus_engine::ScoredChunk> = Vec::new();
        let mut dropped_noise = 0usize;
        let mut dropped_conversation_history = 0usize;
        let mut dropped_untitled = 0usize;
        for c in &initial {
            let key = (
                c.corpus_id.clone(),
                c.title.clone().unwrap_or_default(),
            );
            if key == dominant_key {
                continue; // already expanded
            }
            // Source-quality filter. See `is_grounding_candidate`.
            if c.corpus_id == "conversation-history" {
                dropped_conversation_history += 1;
                continue;
            }
            if !is_grounding_candidate(c) {
                dropped_untitled += 1;
                continue;
            }
            if grounding.len() < EXPANSION_GROUNDING_CHUNKS
                && seen_contents.insert(c.content.clone())
            {
                grounding.push(c.clone());
            } else {
                dropped_noise += 1;
            }
        }

        // Final ordering: dominant source FIRST (natural document
        // order, which maximises narrative coherence), grounding
        // second. The synthesis prompt template doesn't care about
        // ordering semantically but putting the dominant content up
        // top keeps it inside the truncate budget on small context
        // windows.
        let from_source = expanded_dominant.len();
        let grounding_kept = grounding.len();
        let mut merged = expanded_dominant;
        merged.extend(grounding);

        tracing::info!(
            top_source = %shape.top_source_label,
            initial_from_source = shape.top_source_repeat_count,
            additional_fetched = from_source.saturating_sub(shape.top_source_repeat_count),
            total_from_source = from_source,
            grounding_kept,
            dropped_noise,
            dropped_conversation_history,
            dropped_untitled,
            fetch_ms,
            "KnowledgeQuery: source expansion"
        );

        (merged, from_source, grounding_kept, dropped_noise)
    }

    /// Search all knowledge sources, build the prompt with retrieved context,
    /// and assemble provenance metadata. Shared between the streaming and
    /// non-streaming response paths so they cannot diverge.
    async fn prepare_knowledge_context(
        &self,
        message: &str,
        context: &ConversationContext,
        intent: &Intent,
    ) -> KnowledgeContext {
        // Document-attached messages are detected by the
        // `[Document attached: filename]` prefix. We only need to
        // know whether one is attached — the actual document
        // chunking has been moved to `DocumentOperationTool`
        // (routed via ComplexTask), so the parsed-out filename and
        // query text aren't consumed here. We still detect the
        // prefix to short-circuit the embed/search path; without
        // this, a stray document-attached message would burn an
        // embed call producing useless context.
        let attached_source: Option<String> = message
            .strip_prefix("[Document attached: ")
            .and_then(|rest| rest.find(']').map(|end| rest[..end].to_string()));

        let mut all_chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        // corpus_id → human-readable peer name, used at the end to
        // stamp `SourceSummary.from_peer` on any corpus whose hits
        // came in via the mesh. Only populated for corpora we
        // don't host locally (so a corpus present both sides stays
        // tagged as local — we don't pretend to "serve from
        // BeefyMac" a corpus we have right here).
        let mut peer_attribution: HashMap<String, String> = HashMap::new();
        // How many hits came from local (before mesh). Drives the
        // computed `search_method` label. `mesh_hits` is derived
        // later from the peer-attribution map after dedupe.
        let mut local_hits: usize = 0;

        if attached_source.is_some() {
            // Document-attached messages are routed to ComplexTask and should
            // never reach this path — the planner invokes DocumentOperationTool
            // for full map-reduce across all chunks. If we somehow get here,
            // return empty context rather than stuffing a few search results
            // into the prompt.
            tracing::debug!("prepare_knowledge_context called with attached document — skipping (should be ComplexTask)");
        } else {
            // Normal mode: search installed corpora (corpus-engine LanceDB)
            // and corpus-type documents in StateStore. User-uploaded documents
            // are NOT included — they are only surfaced when explicitly
            // attached via [Document attached: ...].
            let corpus_embedding = self.inference.embed_query(message).await.unwrap_or_default();
            let label = format!("{intent:?}");

            // Run the local corpus search and the mesh fan-out
            // concurrently — the mesh call does HTTP (up to ~3s
            // budget per peer), the local call is LanceDB disk I/O,
            // so there's no point serialising them. `tokio::join!`
            // waits for both.
            let local_corpora_fut =
                self.search_corpus_indexes(&corpus_embedding, message, 5, &label);
            let mesh_fut = async {
                match &self.mesh_knowledge {
                    Some(m) => m.search(message, &corpus_embedding, 8).await,
                    None => Vec::new(),
                }
            };
            let (local_scored, mesh_scored) = tokio::join!(local_corpora_fut, mesh_fut);
            local_hits = local_scored.len();
            // Glass-box log: how many hits from local vs. mesh, and
            // which corpora did mesh claim to serve? If mesh_hits > 0
            // but `peer_tagged` is 0, the mesh is only round-tripping
            // local corpora — meaning no peer actually hosts anything
            // we're missing. If both are 0 with a live mesh, the
            // handler on :9741 is either not running or returning
            // empty. Reading this line is how you tell.
            let peer_tagged = mesh_scored
                .iter()
                .filter(|h| h.peer_name.is_some())
                .count();
            let mesh_corpora: std::collections::BTreeSet<&str> = mesh_scored
                .iter()
                .map(|h| h.corpus_id.as_str())
                .collect();
            tracing::info!(
                local_hits = local_scored.len(),
                mesh_hits = mesh_scored.len(),
                mesh_peer_tagged = peer_tagged,
                mesh_corpora = ?mesh_corpora,
                "runtime: knowledge fan-out summary"
            );
            all_chunks.extend(local_scored);

            // Fold mesh hits in, tagging peer attribution per corpus.
            // A corpus that already appears locally doesn't get
            // tagged — we own it, mesh is just parroting.
            let local_corpora_ids: std::collections::HashSet<String> =
                all_chunks.iter().map(|c| c.corpus_id.clone()).collect();
            for hit in mesh_scored {
                if !local_corpora_ids.contains(&hit.corpus_id) {
                    if let Some(name) = &hit.peer_name {
                        peer_attribution
                            .entry(hit.corpus_id.clone())
                            .or_insert_with(|| name.clone());
                    }
                }
                all_chunks.push(corpus_engine::ScoredChunk {
                    content: hit.content,
                    title: hit.title,
                    url: hit.url,
                    corpus_id: hit.corpus_id,
                    score: hit.score,
                    metadata: HashMap::new(),
                });
            }

            // Also search StateStore for corpus-type documents (used by test
            // harness and for corpora ingested directly into the store).
            let embedding = self.inference.embed(message).await.unwrap_or_default();
            let store_chunks = self
                .store
                .search_documents(&embedding, message, 5)
                .await
                .unwrap_or_default();
            for doc in &store_chunks {
                // Only include corpus-type documents, not user uploads.
                if matches!(doc.source_type, SourceType::Corpus { .. }) {
                    all_chunks.push(corpus_engine::ScoredChunk {
                        content: doc.content.clone(),
                        title: Some(doc.source.clone()),
                        url: None,
                        corpus_id: match &doc.source_type {
                            SourceType::Corpus { corpus_id } => corpus_id.clone(),
                            _ => "unknown".to_string(),
                        },
                        score: 0.5,
                        metadata: HashMap::new(),
                    });
                }
            }
        }

        // Put chunks on a comparable score scale before the global
        // merge. See `normalise_scores_per_corpus` for the full
        // rationale — short version: raw BM25 scores aren't
        // comparable across corpora, and on a philosophy query
        // that landed on both a large SEP prose corpus and a small
        // code corpus, the code corpus can produce an outlier
        // score that drowns out the real semantic matches.
        normalise_scores_per_corpus(&mut all_chunks);

        // Dedupe by (corpus_id, content) before truncating so a
        // corpus that appears both locally and via mesh doesn't
        // waste context budget on duplicate chunks.
        all_chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        {
            let mut seen: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            all_chunks.retain(|c| seen.insert((c.corpus_id.clone(), c.content.clone())));
        }
        all_chunks.truncate(8);

        // Count mesh hits that survived dedupe so the search_method
        // label reflects what's actually in the prompt.
        let mesh_hits: usize = all_chunks
            .iter()
            .filter(|c| peer_attribution.contains_key(&c.corpus_id))
            .count();

        // 4. Provenance metadata.
        let installed_corpora = self
            .store
            .list_corpus_states()
            .await
            .unwrap_or_default();
        let corpora_searched = !installed_corpora.is_empty() || self.corpus_engine.is_some();

        // Compose a human-readable label that describes *where* the
        // hits came from. This replaces the old hardcoded "LocalOnly"
        // string — the UI surface is unchanged (still a string in
        // `provenance.search_method`), but the content is now
        // truthful.
        let search_method = if all_chunks.is_empty() {
            if self.mesh_knowledge.is_some() {
                if corpora_searched {
                    Some("LocalAndMesh (no matches)".to_string())
                } else {
                    Some("Mesh (no matches)".to_string())
                }
            } else if corpora_searched {
                Some("LocalOnly (no matches)".to_string())
            } else {
                None
            }
        } else if mesh_hits > 0 && local_hits > 0 {
            Some("LocalAndMesh".to_string())
        } else if mesh_hits > 0 {
            Some("MeshOnly".to_string())
        } else {
            Some("LocalOnly".to_string())
        };

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &all_chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }
        if all_chunks.is_empty() && corpora_searched {
            for cs in &installed_corpora {
                source_map.entry(cs.corpus_id.clone()).or_insert(0);
            }
        }
        let sources: Vec<SourceSummary> = source_map
            .into_iter()
            .map(|(origin, count)| {
                let from_peer = peer_attribution.get(&origin).cloned();
                SourceSummary {
                    origin,
                    count,
                    from_peer,
                }
            })
            .collect();

        // 5. Build prompt with knowledge context.
        let history = format_history_as_prompt(context, 10);
        let prompt = if !all_chunks.is_empty() {
            let doc_context = format_scored_chunks(&all_chunks, MAX_KNOWLEDGE_CHARS);
            if history.is_empty() {
                format!(
                    "Relevant knowledge:\n{doc_context}\n\nUser: {message}\n\nAssistant:"
                )
            } else {
                let short_history = format_history_as_prompt(context, 4);
                format!(
                    "{short_history}\n\nRelevant knowledge:\n{doc_context}\n\nAssistant:"
                )
            }
        } else if history.is_empty() {
            message.to_string()
        } else {
            format!("{history}\n\nAssistant:")
        };

        // 6. System message — layered confidence when knowledge is present.
        let system = if !all_chunks.is_empty() {
            self.build_primary_system_message(
                &format!("{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{THINKING_DIRECTIVE}"),
                context,
            )
        } else {
            self.build_system_message(
                "You are a helpful AI assistant. Respond concisely and accurately.",
                context,
            )
        };

        // 7. Speed upgrade: if knowledge found for SimpleQuery, use Slow.
        let speed = match intent {
            Intent::SimpleQuery => {
                if !all_chunks.is_empty() {
                    Speed::Slow
                } else {
                    Speed::Fast
                }
            }
            Intent::DeepQuery => Speed::Slow,
            _ => Speed::Medium,
        };

        // 8. Build chunk summaries for frontend source linking.
        let retrieved_chunks: Vec<serde_json::Value> = all_chunks
            .iter()
            .map(|c| {
                let snippet = truncate_with_ellipsis(&c.content, 200);
                serde_json::json!({
                    "title": c.title.as_deref().unwrap_or(""),
                    "corpus_id": c.corpus_id,
                    "url": c.url,
                    "snippet": snippet,
                    "provenance_tier": if c.url.is_some() { "web" } else { "corpus" },
                })
            })
            .collect();

        KnowledgeContext {
            chunks: all_chunks,
            prompt,
            system,
            speed,
            search_method,
            sources,
            retrieved_chunks,
        }
    }

    /// Build OICP requirements for non-Fast requests. Composes two
    /// sources — active skills' declared requirements and a set of
    /// intent-implied defaults — and takes the max per-capability so
    /// a skill can always refine beyond what the intent implies
    /// (e.g. `code-review` asking for `code=3` on a ComplexTask
    /// still keeps its `code=3`, and ComplexTask's `Instruction=3`
    /// merges on top).
    ///
    /// Why intent defaults matter: without them, a user asking
    /// "is free will compatible with determinism?" with no active
    /// skill would send a bare `CompletionRequest` with no OICP,
    /// and the mesh `MeshInferenceProvider` would have nothing to
    /// match against. DeepQuery carries a real capability signal
    /// ("reasoning-heavy") that the OICP layer should see.
    ///
    /// Returns `None` when neither source produces any requirements
    /// — e.g. SimpleQuery with no skill activation; the caller keeps
    /// the request local.
    /// Compose a v0.3 [`InferenceRequirements`] for an outbound turn.
    ///
    /// Skill declarations are the baseline — they encode domain
    /// knowledge about what the skill needs (hint, latency class,
    /// context/output envelopes, privacy). Intent-level defaults fill
    /// in properties the active skills didn't declare: a
    /// `DeepQuery` with no active skill still gets a sensible
    /// `LatencyClass::Extended` hint so peer schedulers pick the
    /// right kind of node.
    ///
    /// Merge rule: skill-declared properties win over intent defaults.
    /// This mirrors the pre-v0.3 behaviour where skills could never
    /// be silently downgraded.
    ///
    /// Returns `None` when neither an active skill nor the intent
    /// produces any routing signal — the caller keeps the request
    /// local with whatever default slot policy the runtime uses.
    fn build_oicp(
        &self,
        intent: &Intent,
    ) -> Option<crate::oicp::InferenceRequirements> {
        let from_skills = self.skills.inference_requirements();
        let from_intent = default_oicp_for_intent(intent);

        let skills_empty = from_skills.capability_hint.is_none()
            && from_skills.latency_class.is_none()
            && from_skills.context_tokens.is_none()
            && from_skills.max_output_tokens.is_none();
        if skills_empty && from_intent.is_none() {
            return None;
        }

        // Preserve the sharding value resolved by `SkillRegistry::
        // inference_requirements` — it defaults to `MeshAllowed` and
        // flips to `LocalOnly` only when an active skill has declared
        // it (e.g., `inner-work`). Rebuilding via
        // `InferenceRequirements::new()` would silently reset to
        // `LocalOnly` (the spec default) and block every cross-mesh
        // route, so we copy through explicitly.
        let sharding = from_skills.sharding();
        let mut out = crate::oicp::InferenceRequirements::new()
            .with_sharding(sharding);

        // Hint: skill-declared wins; intent fills in.
        let hint = from_skills
            .capability_hint
            .clone()
            .or_else(|| from_intent.as_ref().and_then(|r| r.capability_hint.clone()));
        if let Some(h) = hint {
            out = out.with_hint(h);
        }

        // Latency class: skill-declared wins; intent fills in.
        let latency_class = from_skills
            .latency_class
            .or_else(|| from_intent.as_ref().and_then(|r| r.latency_class));
        if let Some(lc) = latency_class {
            out = out.with_latency_class(lc);
        }

        // Structural envelopes: skill-declared minimums survive; the
        // intent never imposes a context/output floor on its own.
        if let Some(ctx) = from_skills.context_tokens {
            out = out.with_context_tokens(ctx);
        }
        if let Some(mo) = from_skills.max_output_tokens {
            out = out.with_max_output_tokens(mo);
        }

        Some(out)
    }

    /// Spawn a background task that generates an auto-title for the
    /// conversation if one isn't already set. Non-blocking — failures are
    /// logged and do not affect the caller.
    ///
    /// `try_auto_title` is idempotent: safe to call after every assistant
    /// message save. It exits early when the title is already set or the
    /// conversation doesn't have enough messages yet.
    fn spawn_auto_title(&self, conversation_id: &str) {
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let cid = conversation_id.to_string();
        tokio::spawn(async move {
            if let Err(e) =
                crate::title::try_auto_title(inference.as_ref(), store.as_ref(), &cid).await
            {
                tracing::warn!(
                    conversation_id = %cid,
                    error = %e,
                    "auto-title: generation failed"
                );
            }
        });
    }

    /// Build a system message that includes memory context.
    fn build_system_message(&self, base: &str, context: &ConversationContext) -> String {
        // Invariant check: the Runtime is required to splice
        // `knowledge_view_digests` after routing (via
        // `LandscapeDigestProvider::splice_landscape_digests`). If we
        // reach system-message assembly with the field still `None`,
        // something skipped the splice — most likely a new code path
        // that builds its own ConversationContext and went straight
        // to the LLM. Debug-builds panic loudly so the oversight is
        // caught in tests; release builds proceed without the digest.
        //
        // The guard is tolerant of the no-KnowledgeView configuration
        // (the field stays `None` when `Runtime::with_landscape_digests`
        // wasn't called — e.g. unit-test harnesses). We only assert
        // when a provider is installed.
        if self.landscape_digests.is_some() {
            context.debug_assert_routed();
        }

        let mut parts = vec![base.to_string()];

        if let Some(mem_section) = memory::format_memories_for_prompt(&context.memories) {
            parts.push(mem_section);
        }

        if let Some(wm) = &context.working_memory {
            if let Some(goal) = &wm.current_goal {
                parts.push(format!("Current user goal: {goal}"));
            }
            if !wm.facts.is_empty() {
                parts.push(format!(
                    "Session context:\n- {}",
                    wm.facts.join("\n- ")
                ));
            }
        }

        // KnowledgeView landscape digests — the person's recurring
        // terrain (clusters, fault lines, open questions) that the
        // model reads before answering. Bounded at splice time by
        // `KnowledgeViewManager::splice_into`'s per-view token budget
        // (300 + 200 in v1).
        if let Some(digests) = context.knowledge_view_digests.as_ref() {
            for d in digests {
                let body = d.body.trim();
                if !body.is_empty() {
                    parts.push(body.to_string());
                }
            }
        }

        parts.join("\n\n")
    }

    /// Build a system message for Primary-slot (Speed::Slow) completions.
    /// Prepends `PRIMARY_BASE_SYSTEM_PROMPT` before the caller-supplied base text
    /// so all Primary calls carry the epistemic accuracy contract.
    fn build_primary_system_message(&self, base: &str, context: &ConversationContext) -> String {
        self.build_system_message(
            &format!("{PRIMARY_BASE_SYSTEM_PROMPT}\n\n{base}"),
            context,
        )
    }

    /// Epistemic humility hook: audit the just-produced answer against
    /// its evidence and, if the model judges a specific external source
    /// would materially sharpen the answer, surface an
    /// [`InformationRequest`] card via the approval channel. If the
    /// user pastes content, re-synthesise the answer with that content
    /// folded in. Otherwise return the original response unchanged.
    ///
    /// **Pure-additive**: never makes the answer worse than the
    /// corpus-only baseline — any failure (inference error, parse
    /// failure, user skip) falls back to `response` unchanged. Gated
    /// by `InferenceConfig::auto_collaborate` (default on) so the
    /// whole path is a no-op when disabled.
    ///
    /// Callers pass `evidence` as a plain-text summary of whatever
    /// corpus material grounded the original answer. Empty string is
    /// acceptable (e.g. when corpus retrieval returned nothing).
    pub async fn maybe_collaborate(
        &self,
        conversation_id: &str,
        question: &str,
        response: &str,
        evidence: &str,
    ) -> String {
        run_collaboration(
            self.inference.as_ref(),
            self.approval.as_ref(),
            &self.inference_config,
            conversation_id,
            question,
            response,
            evidence,
        )
        .await
    }

    /// Post-stream refinement hook: runs the gap check against the
    /// already-streamed answer; if the user pastes content, overwrites
    /// the saved assistant message and emits a `message-refined` event
    /// so the UI can replace the bubble. Returns `Some(refined_text)`
    /// when refinement produced new content, `None` otherwise.
    ///
    /// Delegates to `run_post_stream_refinement` so the streaming
    /// spawn (which doesn't hold `&self`) and tests share one code
    /// path.
    pub async fn apply_post_stream_refinement(
        &self,
        conversation_id: &str,
        message_id: &str,
        question: &str,
        original_content: &str,
        evidence: &str,
        original_metadata: Option<serde_json::Value>,
    ) -> Option<String> {
        run_post_stream_refinement(
            self.inference.as_ref(),
            self.approval.as_ref(),
            self.store.as_ref(),
            &self.inference_config,
            conversation_id,
            message_id,
            question,
            original_content,
            evidence,
            original_metadata,
        )
        .await
    }


    /// Extract long-term memories from a conversation and save them.
    /// Call this when a conversation ends (user quits or session ends).
    pub async fn end_conversation(&self, conversation_id: &str) -> Result<()> {
        let context = build_context(self.store.as_ref(), conversation_id, "").await?;
        if context.conversation.messages.len() < 4 {
            return Ok(());
        }

        let memory_rules = self.skills.memory_rules();
        let extracted = memory::extract_long_term_memories(
            self.inference.as_ref(),
            &context.conversation.messages,
            &memory_rules,
        )
        .await?;

        tracing::info!(count = extracted.len(), "memory: extracted long-term memories");
        for mut mem in extracted {
            // Tag each extracted memory with the conversation it
            // came from. Enables the `personal-knowledge`
            // KnowledgeView to surface cluster membership
            // alongside conversation-level metadata (title, skill)
            // at digest time, and makes `memories.source_conversation_id`
            // no longer NULL on fresh writes post-migration.
            mem.source_conversation_id = Some(conversation_id.to_string());
            memory::save_with_contradiction_check(
                self.inference.as_ref(),
                self.store.as_ref(),
                mem,
            )
            .await?;
        }

        // Pull a fresh entity inventory from the LandscapeDigestProvider
        // (typically `sovereign-tools::KnowledgeViewManager`). When
        // present, memories that mention any canonical entity name
        // decay at half rate (Phase 7 — relationship-weighted decay).
        // `None` = uniform decay, identical to the pre-Phase-7 path.
        let inventory = match self.landscape_digests.as_ref() {
            Some(p) => p.entity_inventory().await,
            None => None,
        };
        let pruned = memory::prune_decayed_memories_with_config(
            self.store.as_ref(),
            now(),
            memory::DEFAULT_DECAY_RATE,
            memory::DEFAULT_PRUNE_THRESHOLD,
            inventory.as_ref(),
        )
        .await
        .unwrap_or(0);
        if pruned > 0 {
            tracing::info!(pruned, "memory: pruned decayed memories");
        }

        Ok(())
    }

    /// Stream a chat response token-by-token.
    ///
    /// Builds context, saves the user message, routes the intent, and starts
    /// streaming inference for SimpleQuery / DeepQuery / KnowledgeQuery. The
    /// returned [`StreamHandle`] yields response chunks; once the stream
    /// completes, the assistant message is persisted under `message_id`.
    ///
    /// Returns [`Error::NotImplemented`] for ComplexTask intents — callers
    /// should fall back to [`Self::handle_message`] in that case.
    #[tracing::instrument(
        name = "runtime.handle_message_stream",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    /// PR2 session-continuation entry point. Called when the user
    /// clicks a ClarificationCard option or a NextStepOffer. Takes
    /// the `ResumeSession` hint, synthesises a fresh
    /// `RouterClassification` from it (primary = hinted intent,
    /// confidence = 1.0, MoveKind::Commit by construction), and
    /// dispatches through the regular `handle_message_stream` body —
    /// just with classification pre-decided so no router call is
    /// made. PR2c will additionally reuse the retrieval cache keyed
    /// by `resume.session_id`.
    pub async fn resume_session_stream(
        &self,
        message: &str,
        conversation_id: &str,
        resume: ResumeSession,
    ) -> Result<StreamHandle> {
        tracing::info!(
            session_id = %resume.session_id,
            intent_hint = %resume.intent_hint,
            "runtime: resume session (continuation)"
        );
        let hinted = parse_intent_hint(&resume.intent_hint);
        let synthetic = RouterClassification {
            primary: IntentCandidate {
                intent: hinted,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: Some(format!(
                "session continuation from {}",
                &resume.session_id
            )),
            coarse_intent: Some("CONTINUATION".to_string()),
            self_assessment: None,
        };
        self.handle_message_stream_with_classification(
            message,
            conversation_id,
            Some(synthetic),
        )
        .await
    }

    /// PR2c redirect handler — cancel an in-flight Propose-mode
    /// sampler AND restart synthesis against the alternative intent
    /// the user picked. Reads `session.input` + `session.conversation_id`
    /// from the earlier `SessionStore.begin(...)` call, so the caller
    /// only needs to pass the session id + intent hint. The old
    /// assistant message stays in history (cancelled, possibly
    /// partial) — the new one appears below as a fresh stream.
    ///
    /// PR2c scope: cancel + new stream, no retrieval reuse yet (the
    /// new stream re-runs `prepare_knowledge_query_plan`). Caching is
    /// PR2d — noted in the plan file.
    pub async fn redirect_turn_stream(
        &self,
        session_id: &str,
        intent_hint: &str,
    ) -> Result<StreamHandle> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| Error::NotImplemented(format!("session {session_id} not found")))?;
        tracing::info!(
            session_id,
            intent_hint,
            from_intent = ?session.classification.primary.intent,
            "routing:redirected — cancelling current sampler and re-dispatching"
        );
        // PR4 — structural-signal capture. Update the `routing_log`
        // row for this session's message with
        // `was_redirected = true` + `redirect_to = <intent_hint>`.
        // The hash must match the one `Router::classify` wrote via
        // `log_routing` — both sides use `router::message_hash`.
        // Best-effort; a db failure here doesn't block the redirect.
        let signal_hash = crate::router::message_hash(&session.input);
        let signal_hint = intent_hint.to_string();
        let signal_store = Arc::clone(&self.store);
        tokio::spawn(async move {
            if let Err(e) = signal_store
                .mark_routing_redirected(&signal_hash, &signal_hint)
                .await
            {
                tracing::warn!(error = %e, "routing:redirect_signal write failed");
            } else {
                tracing::info!(
                    hash = %signal_hash,
                    redirect_to = %signal_hint,
                    "routing:redirect_signal captured"
                );
            }
        });
        // Cancel the in-flight sampler so it drains and releases the
        // slot lock before we spawn the replacement stream. Receiver
        // drop (existing semantics) would also work, but the explicit
        // token cancel is observable in `inference:cancelled` logs.
        session.cancel.cancel();
        // Hand off to the same continuation path the Clarification
        // card uses. Same synthetic-classification shape, just
        // tagged so the trace differentiates the two kinds of
        // continuations.
        let hinted = parse_intent_hint(intent_hint);
        let synthetic = RouterClassification {
            primary: IntentCandidate {
                intent: hinted,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: Some(format!("redirect from session {session_id}")),
            coarse_intent: Some("REDIRECT".to_string()),
            self_assessment: None,
        };
        let message = session.input.clone();
        let conversation_id = session.conversation_id.clone();
        drop(session);
        self.handle_message_stream_with_classification(
            &message,
            &conversation_id,
            Some(synthetic),
        )
        .await
    }

    pub async fn handle_message_stream(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        self.handle_message_stream_with_classification(message, conversation_id, None)
            .await
    }

    /// Private inner entry point for [`handle_message_stream`] and
    /// [`resume_session_stream`]. When `preset` is `Some`, the
    /// classifier call is skipped; when `None`, classification runs
    /// as normal.
    async fn handle_message_stream_with_classification(
        &self,
        message: &str,
        conversation_id: &str,
        preset: Option<RouterClassification>,
    ) -> Result<StreamHandle> {
        tracing::info!("runtime: stream turn begin");
        // PR2e — reject oversized turn messages before any Fast-slot
        // work runs. Document-sized inputs belong in the attached-
        // file path; dropping 20 pages into the chat body used to
        // hang `compress_working_memory` for minutes.
        if message.len() > MAX_TURN_MESSAGE_CHARS {
            tracing::warn!(
                message_chars = message.len(),
                limit = MAX_TURN_MESSAGE_CHARS,
                "runtime:oversize_message rejected"
            );
            return Err(Error::InvalidInput(OVERSIZE_MESSAGE_HINT.to_string()));
        }
        // 1. Build context.
        let mut context = build_context(self.store.as_ref(), conversation_id, message).await?;
        tracing::debug!(
            messages = context.conversation.messages.len(),
            memories = context.memories.len(),
            installed_corpora = context.installed_corpora.len(),
            "runtime: stream context built"
        );

        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 1b. Update topic context for turn-aware routing.
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
        )
        .await
        .ok();
        context.topic_context = topic_context;

        // 2. Save user message.
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
            version: now(),
        };
        self.store.save_message(&user_msg).await?;
        context.conversation.messages.push(user_msg);

        // 2b. Tag the conversation with the skill that was active
        // when it started. The store upsert is idempotent — only
        // the first call with a non-NULL skill wins, later calls
        // are no-ops. The KnowledgeView conversational acquirer
        // reads this column to exclude `privacy = local_only`
        // skills (e.g. `inner-work`) from the shared corpus.
        if let Some(skill_id) = self.skills.primary_skill_id_for_conversation() {
            if let Err(e) = self
                .store
                .set_conversation_skill_if_unset(conversation_id, &skill_id)
                .await
            {
                tracing::debug!(
                    conversation_id,
                    error = %e,
                    "failed to tag conversation with skill_id; continuing"
                );
            }
        }

        // 3. Route (or honour a preset classification from a
        // session-continuation call). When `preset` is `Some`, the
        // classifier call is skipped — the UI has already picked the
        // intent via `ClarificationCard` or `NextStepButtons`, and
        // re-classifying the same message would waste a Fast-slot
        // call and risk drifting from the user's explicit choice.
        let tool_descriptors = self.tools.descriptors();
        let classification = if let Some(preset) = preset {
            preset
        } else {
            self.router
                .classify(message, &context, &tool_descriptors)
                .await?
        };

        // Apply routing policy. PR1 only reaches MoveKind::Commit in
        // the dispatcher; Propose/Ask are scaffolded by `decide_policy`
        // but the Runtime treats anything non-Commit as Commit until
        // PR2 wires the UI. We still log the policy so glassbox
        // observers (ARCH §0.1, §9.1) see which tier we'd be in.
        let policy = decide_policy(&classification, &self.confidence_thresholds);
        tracing::debug!(
            tier = ?policy.tier,
            move_kind = ?policy.move_kind,
            primary_intent = ?classification.primary.intent,
            confidence = classification.primary.confidence,
            thresholds_high = policy.thresholds_used.high,
            thresholds_moderate = policy.thresholds_used.moderate,
            "router:policy_applied"
        );

        // Begin an in-memory QuerySession covering this turn. Holds
        // the classification + policy + cancellation token. PR2 will
        // also cache retrieval and partial response here so a
        // `redirect_turn` can reuse work without re-searching.
        self.sessions.sweep_expired();
        let skill_id = self.skills.primary_skill_id_for_conversation();
        let (_session_id, _cancel_token) = self.sessions.begin(
            conversation_id.to_string(),
            skill_id,
            message.to_string(),
            classification.clone(),
            policy.clone(),
        );

        // Destructure the classification fields we still thread as
        // diagnostics into downstream handlers. Preserving these
        // names keeps the handle_knowledge_query / handle_simple call
        // sites untouched so PR1 stays behaviour-preserving.
        let intent = classification.primary.intent.clone();
        let coarse_intent = classification.coarse_intent.clone();
        let self_assessment = classification.self_assessment.clone();

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            tier = ?policy.tier,
            "runtime: stream routed"
        );

        // PR2 — Ask move. Suppress synthesis entirely, emit a
        // `clarification-request` event, save a placeholder assistant
        // message with the clarification metadata so the UI's
        // existing message-metadata listener can render the
        // ClarificationCard (same delivery path as retrieved_chunks).
        // Return an already-closed stream so the desktop relay exits
        // its token loop and promptly fires `message-complete`.
        if matches!(policy.move_kind, MoveKind::Ask) {
            return self
                .handle_ask_move_stream(
                    message,
                    conversation_id,
                    &_session_id,
                    &classification,
                )
                .await;
        }

        // PR2 — Propose move. Emit an `interpretation-proposed` event
        // BEFORE any tokens flow, then fall through to the Commit
        // path so the Fast slot begins streaming immediately. The UI
        // renders the banner on the in-flight message; a subsequent
        // `redirect_turn` cancels the sampler via
        // `session.cancel.cancel()` and re-dispatches with an
        // alternative intent.
        if matches!(policy.move_kind, MoveKind::Propose) {
            let interpretation = format_interpretation(
                message,
                &classification.primary.intent,
                classification.rationale.as_deref(),
            );
            let alternatives = classification
                .alternatives
                .iter()
                .map(|a| ProposedAlternative {
                    label: label_for_intent(&a.intent),
                    intent_hint: intent_hint(&a.intent),
                })
                .collect();
            self.routing_events
                .emit_interpretation_proposed(InterpretationProposed {
                    session_id: _session_id.clone(),
                    conversation_id: conversation_id.to_string(),
                    interpretation,
                    alternatives,
                    confidence: classification.primary.confidence,
                })
                .await;
            tracing::info!(
                session_id = %_session_id,
                "routing:propose — banner emitted, continuing to Commit path"
            );
            // Fall through to Commit path — streaming begins below.
        }

        // Document attached or ComplexTask → fall back to non-streaming.
        // (KnowledgeQuery used to live here too, but that triggered a desktop
        // fallback that re-ran build_context + compress_working_memory +
        // update_topic_context + classify — ~17 seconds of pure duplicated
        // work. Instead we now run KnowledgeQuery inline below and emit the
        // response as a single stream chunk.)
        if message.starts_with("[Document attached: ")
            || matches!(intent, Intent::ComplexTask)
        {
            tracing::info!(
                intent = ?intent,
                "runtime: stream not supported for this intent — falling back"
            );
            return Err(Error::NotImplemented(
                "Streaming not supported for this intent".into(),
            ));
        }

        // 3b. Splice KnowledgeView landscape digests now that routing
        // has resolved. The provider (typically the sovereign-tools
        // KnowledgeViewManager) reads the enriched indexes for each
        // built-in view and writes a markdown summary into
        // `context.knowledge_view_digests` so prompt assembly can
        // surface "here's the person's terrain" before synthesis.
        //
        // IMPORTANT: this MUST run before any intent-specific dispatch
        // (including the inline KnowledgeQuery path below). The final
        // prompt-assembly site asserts `knowledge_view_digests.is_some()`
        // as an invariant — running handle_knowledge_query without
        // splicing panics in types.rs.
        //
        // Pass the resolved primary active skill so the provider can
        // suppress cross-skill context when the active skill is
        // `privacy = "local_only"` (e.g. `inner-work` should not see
        // the conversational-history digest at all).
        if let Some(provider) = &self.landscape_digests {
            let active_skill = self.skills.primary_skill_id_for_conversation();
            provider
                .splice_landscape_digests(&mut context, active_skill.as_deref())
                .await;
        } else {
            // No provider installed — mark the invariant satisfied with
            // an empty digest set so the assert at the prompt-assembly
            // site doesn't fire. Matches the non-streaming path which
            // also runs through a provider or explicit empty default.
            context.set_landscape_digests(Vec::new());
        }

        // KnowledgeQuery: real streaming path. Prepare the synthesis
        // plan synchronously (retrieval + evidence-shape routing +
        // source expansion + request build + retrieved_chunks
        // summaries), then spawn a tokio task that drives
        // `complete_stream_with_id` and forwards each token to the
        // caller as it arrives. This replaces the old one-shot wrapper
        // which made the desktop chat window sit inert for ~35s while
        // the full response was assembled server-side.
        if matches!(intent, Intent::KnowledgeQuery) {
            tracing::info!(
                intent = ?intent,
                "runtime: stream path — KnowledgeQuery with token streaming"
            );
            let plan = self.prepare_knowledge_query_plan(message, &context).await;

            // PR5 — post-retrieval retrieval-miss diversion. Off-
            // target evidence shape (dispersed across ≥3 sources,
            // no source concentration, no title match) was
            // historically the exact input that produced confident
            // parametric fabrication. Suppress synthesis and emit a
            // clarification card instead.
            if plan.shape.is_off_target() {
                tracing::info!(
                    session_id = %_session_id,
                    distinct_sources = plan.shape.distinct_sources,
                    retrieval_count = plan.shape.count,
                    "routing:retrieval_miss — diverting to Ask clarification"
                );
                return self
                    .handle_retrieval_miss_stream(
                        message,
                        conversation_id,
                        &_session_id,
                        &plan.shape,
                        &tool_descriptors,
                    )
                    .await;
            }

            // PR2 narration: report retrieval shape on long turns.
            // Suppressed internally when total elapsed < 5s or cap
            // hit. The session store guards both; this call is safe
            // on short turns — it just returns `None`.
            if plan.shape.top_source_repeat_count >= 2 {
                let txt = format!(
                    "Found {} chunks — {} from one source, so I'll keep the answer focused.",
                    plan.chunks.len(),
                    plan.shape.top_source_repeat_count,
                );
                if let Some(event) = self.sessions.try_emit_narration(
                    &_session_id,
                    NarrationPhase::RetrievalComplete,
                    txt,
                ) {
                    self.routing_events
                        .emit_turn_narration(TurnNarration {
                            session_id: _session_id.clone(),
                            conversation_id: conversation_id.to_string(),
                            event,
                        })
                        .await;
                }
            }

            let message_id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);

            // Everything the spawned task needs — no borrows of `self`.
            let inference = Arc::clone(&self.inference);
            let store = Arc::clone(&self.store);
            let approval = Arc::clone(&self.approval);
            let inference_config = self.inference_config.clone();
            let conversation_id_owned = conversation_id.to_string();
            let message_id_owned = message_id.clone();
            let question = message.to_string();

            let KnowledgeQueryPlan {
                request,
                chunks,
                doc_context,
                shape,
                route,
                gap_check_enabled,
                search_ms,
                retrieved_chunks,
                source_map,
                result_quality,
            } = plan;
            let documents_found = chunks.len();
            let top_source_label = shape.top_source_label.clone();
            let coarse_intent_for_prov = coarse_intent.clone();
            let self_assessment_for_prov = self_assessment.clone();

            // PR3: compute next-step offers against the same
            // retrieval the answer was built from. We do this on the
            // main task (not the spawn) so we can capture the
            // user's message by reference without cloning into the
            // async move. The result is serialised into message
            // metadata inside the spawn.
            let had_dominant_source = shape.top_source_repeat_count >= 2;
            let retrieval_missed = shape.is_off_target();
            let top_source_title_owned = if shape.top_source_key.1.is_empty() {
                None
            } else {
                Some(shape.top_source_key.1.clone())
            };
            let offers = build_next_step_offers(&OfferContext {
                user_message: message,
                top_source_title: top_source_title_owned.as_deref(),
                had_dominant_source,
                retrieved_chunks: &retrieved_chunks,
                session_id: &_session_id,
                retrieval_missed,
            });
            let offers_json = serde_json::to_value(&offers).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "next_steps: serialize failed");
                serde_json::Value::Array(Vec::new())
            });
            let route_for_log = route;

            tokio::spawn(async move {
                let started = std::time::Instant::now();

                let (mut s, model_id) = match inference
                    .complete_stream_with_id(&request)
                    .await
                {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };

                let mut full_text = String::new();
                while let Some(item) = s.next().await {
                    match item {
                        Ok(chunk) => {
                            full_text.push_str(&chunk);
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    }
                }

                // Persist final assistant message with full KQ metadata
                // so the UI citation expander and provenance header
                // have everything they had on the non-streaming path.
                let provenance = ResponseProvenance {
                    intent: "KnowledgeQuery".to_string(),
                    search_method: Some("CorpusEngine".to_string()),
                    sources: source_map
                        .into_iter()
                        .map(|(origin, count)| SourceSummary {
                            origin,
                            count,
                            from_peer: None,
                        })
                        .collect(),
                    inference_backend: model_id,
                    oicp_match: None,
                    total_latency_ms: started.elapsed().as_millis() as u64,
                    tokens_used: 0,
                    coarse_intent: coarse_intent_for_prov,
                    self_assessment: self_assessment_for_prov,
                };
                let metadata_json = serde_json::json!({
                    "streamed": true,
                    "intent": "knowledge_query",
                    "documents_found": documents_found,
                    "search_ms": search_ms,
                    "result_quality": result_quality,
                    "provenance": provenance,
                    "retrieved_chunks": retrieved_chunks,
                    // PR3 — grounded follow-ups rendered as clickable
                    // NextStepButtons under the bubble. Empty array
                    // when retrieval produced nothing to ground an
                    // offer against; the UI hides the row.
                    "next_steps": offers_json,
                });
                let assistant_msg = Message {
                    id: message_id_owned.clone(),
                    conversation_id: conversation_id_owned.clone(),
                    role: Role::Assistant,
                    content: full_text.clone(),
                    created_at: now(),
                    metadata: Some(metadata_json.clone()),
                    version: now(),
                };
                if let Err(e) = store.save_message(&assistant_msg).await {
                    tracing::warn!(
                        conversation_id = %conversation_id_owned,
                        error = %e,
                        "KnowledgeQuery stream: failed to save assistant message"
                    );
                }

                if gap_check_enabled {
                    tracing::debug!(
                        route = ?route_for_log,
                        "KnowledgeQuery stream: scheduling post-stream gap check"
                    );
                    let collab_inference = Arc::clone(&inference);
                    let collab_store = Arc::clone(&store);
                    let collab_approval = Arc::clone(&approval);
                    let collab_config = inference_config.clone();
                    let collab_cid = conversation_id_owned.clone();
                    let collab_mid = message_id_owned.clone();
                    let collab_question = question.clone();
                    let collab_original = full_text.clone();
                    let collab_evidence = doc_context.clone();
                    let collab_metadata = metadata_json;
                    tokio::spawn(async move {
                        run_post_stream_refinement(
                            collab_inference.as_ref(),
                            collab_approval.as_ref(),
                            collab_store.as_ref(),
                            &collab_config,
                            &collab_cid,
                            &collab_mid,
                            &collab_question,
                            &collab_original,
                            &collab_evidence,
                            Some(collab_metadata),
                        )
                        .await;
                    });
                } else {
                    tracing::info!(
                        route = ?route_for_log,
                        top_source = %top_source_label,
                        "KnowledgeQuery stream: skipping gap check (concentrated single-source)"
                    );
                }

                // Auto-title after first exchange — same post-stream
                // hook the non-KQ streaming path uses. Non-blocking.
                let title_inference = Arc::clone(&inference);
                let title_store = Arc::clone(&store);
                let title_cid = conversation_id_owned.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::title::try_auto_title(
                        title_inference.as_ref(),
                        title_store.as_ref(),
                        &title_cid,
                    )
                    .await
                    {
                        tracing::warn!(
                            conversation_id = %title_cid,
                            error = %e,
                            "auto-title: generation failed (KQ stream path)"
                        );
                    }
                });
            });

            let stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
                Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
            return Ok(StreamHandle { message_id, stream });
        }

        // 4. Search knowledge + build prompt (shared with handle_simple).
        let kc = self
            .prepare_knowledge_context(message, &context, &intent)
            .await;

        let oicp = if matches!(intent, Intent::SimpleQuery) {
            None
        } else {
            self.build_oicp(&intent)
        };

        // Model ID is captured from `complete_stream_with_id` once
        // the provider has committed to a routing decision — see
        // the trait docs on that method. Using the pre-stream sync
        // `model_id_for` here would miss peer attribution (the
        // mesh wrapper can only report "I routed to peer X" after
        // its async `select_peer` pass has run).
        let request = CompletionRequest {
            prompt: kc.prompt,
            system_message: Some(kc.system),
            preferred_speed: kc.speed,
            max_tokens: Some(self.inference_config.max_tokens),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(self.inference_config.think_budget),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp,
                    tools: None,
                    tool_choice: None,
                            model_id: None,
        };

        let search_method = kc.search_method;
        let sources = kc.sources;
        let retrieved_chunks = kc.retrieved_chunks;

        // Format the corpus evidence now so the post-stream epistemic-
        // humility hook can feed it to the gap checker. Moved into the
        // streaming spawn; not used before the synthesis completes.
        let evidence = format_scored_chunks(&kc.chunks, MAX_KNOWLEDGE_CHARS);
        let question = message.to_string();

        let intent_label = format!("{intent:?}");
        let message_id = uuid::Uuid::new_v4().to_string();

        // 5. Spawn streaming task.
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let approval = Arc::clone(&self.approval);
        let inference_config = self.inference_config.clone();
        let conversation_id_owned = conversation_id.to_string();
        let message_id_owned = message_id.clone();

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);

        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut full_text = String::new();

            let (mut s, model_id) = match inference
                .complete_stream_with_id(&request)
                .await
            {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            while let Some(item) = s.next().await {
                match item {
                    Ok(chunk) => {
                        full_text.push_str(&chunk);
                        if tx.send(Ok(chunk)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }

            // Persist final assistant message.
            let provenance = ResponseProvenance {
                intent: intent_label,
                search_method,
                sources,
                inference_backend: model_id,
                oicp_match: None,
                total_latency_ms: started.elapsed().as_millis() as u64,
                tokens_used: 0,
                coarse_intent,
                self_assessment,
            };
            let metadata_json = serde_json::json!({
                "streamed": true,
                "provenance": provenance,
                "retrieved_chunks": retrieved_chunks,
            });
            let assistant_msg = Message {
                id: message_id_owned.clone(),
                conversation_id: conversation_id_owned.clone(),
                role: Role::Assistant,
                content: full_text.clone(),
                created_at: now(),
                metadata: Some(metadata_json.clone()),
                version: now(),
            };
            let _ = store.save_message(&assistant_msg).await;

            // Epistemic-humility hook (post-stream): audit the streamed
            // answer and, if the user provides additional content, rewrite
            // the persisted message and emit a `message-refined` event so
            // the UI can update the bubble in place. Runs concurrently
            // with auto-title so neither blocks the other.
            let collab_inference = Arc::clone(&inference);
            let collab_store = Arc::clone(&store);
            let collab_approval = Arc::clone(&approval);
            let collab_config = inference_config.clone();
            let collab_cid = conversation_id_owned.clone();
            let collab_mid = message_id_owned.clone();
            let collab_question = question.clone();
            let collab_evidence = evidence.clone();
            let collab_original = full_text.clone();
            let collab_metadata = metadata_json;
            tokio::spawn(async move {
                run_post_stream_refinement(
                    collab_inference.as_ref(),
                    collab_approval.as_ref(),
                    collab_store.as_ref(),
                    &collab_config,
                    &collab_cid,
                    &collab_mid,
                    &collab_question,
                    &collab_original,
                    &collab_evidence,
                    Some(collab_metadata),
                )
                .await;
            });

            // Auto-title after first exchange. Non-blocking; the stream has
            // already delivered the response to the user.
            let title_inference = Arc::clone(&inference);
            let title_store = Arc::clone(&store);
            let title_cid = conversation_id_owned.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::title::try_auto_title(
                    title_inference.as_ref(),
                    title_store.as_ref(),
                    &title_cid,
                )
                .await
                {
                    tracing::warn!(
                        conversation_id = %title_cid,
                        error = %e,
                        "auto-title: generation failed (stream path)"
                    );
                }
            });
        });

        let stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));

        Ok(StreamHandle { message_id, stream })
    }

    #[tracing::instrument(
        name = "runtime.handle_message",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    pub async fn handle_message(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        // Save the user message first so `handle_turn` sees it in the
        // conversation history during context building and routing.
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
            version: now(),
        };
        self.store.save_message(&user_msg).await?;

        // Tag the conversation with the active skill on first message
        // (idempotent — see the streaming-path equivalent).
        if let Some(skill_id) = self.skills.primary_skill_id_for_conversation() {
            if let Err(e) = self
                .store
                .set_conversation_skill_if_unset(conversation_id, &skill_id)
                .await
            {
                tracing::debug!(
                    conversation_id,
                    error = %e,
                    "failed to tag conversation with skill_id; continuing"
                );
            }
        }

        self.handle_turn(message, conversation_id).await
    }

    /// Run a conversation turn assuming the user message has **already** been
    /// saved as the latest message in the conversation.
    ///
    /// Callers that need to save the user message with custom metadata — for
    /// example the `ask_document` Tauri command which tags the message with
    /// the attached asset id — can call this entry point directly. The
    /// runtime pipeline (context build, working-memory compression, topic
    /// context, routing, synthesis, auto-title) then proceeds identically
    /// to [`Self::handle_message`].
    ///
    /// Build-context reads all existing messages from the store, so the
    /// pre-saved user message is included in the in-memory context without
    /// the caller having to push it explicitly.
    #[tracing::instrument(
        name = "runtime.handle_turn",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    pub async fn handle_turn(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        let turn_start = std::time::Instant::now();
        let has_doc_prefix = message.starts_with("[Document attached: ");
        tracing::info!(has_doc_prefix, "runtime: turn begin");

        // PR2e — same oversize guard the streaming path applies.
        // The `[Document attached: ...]` prefix path is exempt — that
        // one is designed for long inputs and runs through the
        // map-reduce pipeline, not the Fast-slot turn chain.
        if !has_doc_prefix && message.len() > MAX_TURN_MESSAGE_CHARS {
            tracing::warn!(
                message_chars = message.len(),
                limit = MAX_TURN_MESSAGE_CHARS,
                "runtime:oversize_message rejected (non-streaming)"
            );
            return Err(Error::InvalidInput(OVERSIZE_MESSAGE_HINT.to_string()));
        }

        // 1. Build context from store (use message text for memory retrieval).
        //    The user message is already persisted so it shows up here.
        let mut context = build_context(self.store.as_ref(), conversation_id, message).await?;
        tracing::debug!(
            messages = context.conversation.messages.len(),
            memories = context.memories.len(),
            installed_corpora = context.installed_corpora.len(),
            has_document_session = context.document_session.is_some(),
            "runtime: context built"
        );

        // 1b. Compress working memory from conversation history (now including
        //     the latest user message — gives working-memory extraction a
        //     crisper view of current intent).
        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 1c. Update topic context for turn-aware routing. Latest user
        //     message is part of the extraction input.
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
        )
        .await
        .ok();
        context.topic_context = topic_context;

        // 2. Route.
        let tool_descriptors = self.tools.descriptors();
        let classification = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;

        // Same policy-apply + QuerySession hookup as the streaming
        // path. See handle_message_stream for context. PR1 dispatcher
        // only reaches MoveKind::Commit; PR2 will branch.
        let policy = decide_policy(&classification, &self.confidence_thresholds);
        tracing::debug!(
            tier = ?policy.tier,
            move_kind = ?policy.move_kind,
            primary_intent = ?classification.primary.intent,
            confidence = classification.primary.confidence,
            thresholds_high = policy.thresholds_used.high,
            thresholds_moderate = policy.thresholds_used.moderate,
            "router:policy_applied"
        );

        self.sessions.sweep_expired();
        let skill_id = self.skills.primary_skill_id_for_conversation();
        let (_session_id, _cancel_token) = self.sessions.begin(
            conversation_id.to_string(),
            skill_id,
            message.to_string(),
            classification.clone(),
            policy.clone(),
        );

        let intent = classification.primary.intent.clone();
        let coarse_intent = classification.coarse_intent.clone();
        let self_assessment = classification.self_assessment.clone();

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            tier = ?policy.tier,
            "runtime: routed"
        );

        // PR2 — Ask on the non-streaming path. Same semantics as
        // `handle_ask_move_stream`: save a placeholder assistant
        // message with clarification metadata, emit the event, return
        // a Response without running synthesis.
        if matches!(policy.move_kind, MoveKind::Ask) {
            return self
                .handle_ask_move_turn(message, conversation_id, &_session_id, &classification)
                .await;
        }
        // PR2 — Propose on the non-streaming path. Emit the banner
        // event before falling through to synthesis. Redirect from
        // the non-streaming path is a PR2c concern (the desktop runs
        // on the streaming path; CLI users who want to redirect can
        // send a new turn).
        if matches!(policy.move_kind, MoveKind::Propose) {
            let interpretation = format_interpretation(
                message,
                &classification.primary.intent,
                classification.rationale.as_deref(),
            );
            let alternatives = classification
                .alternatives
                .iter()
                .map(|a| ProposedAlternative {
                    label: label_for_intent(&a.intent),
                    intent_hint: intent_hint(&a.intent),
                })
                .collect();
            self.routing_events
                .emit_interpretation_proposed(InterpretationProposed {
                    session_id: _session_id.clone(),
                    conversation_id: conversation_id.to_string(),
                    interpretation,
                    alternatives,
                    confidence: classification.primary.confidence,
                })
                .await;
        }

        // 2b. Splice KnowledgeView landscape digests (same hook as
        // handle_message_stream). No-op when
        // `Runtime::with_landscape_digests` wasn't called at build
        // time. See the streaming path for rationale on `active_skill`.
        if let Some(provider) = &self.landscape_digests {
            let active_skill = self.skills.primary_skill_id_for_conversation();
            provider
                .splice_landscape_digests(&mut context, active_skill.as_deref())
                .await;
        }

        // When a legacy [Document attached: ...] prefix is used, bypass the
        // planner entirely and route to the map-reduce document_operation path.
        if let Some(rest) = message.strip_prefix("[Document attached: ") {
            if let Some(end) = rest.find(']') {
                let source = rest[..end].to_string();
                let user_query = rest[end + 1..].trim().to_string();
                tracing::info!(
                    source = %source,
                    user_query_chars = user_query.len(),
                    "runtime: dispatching to handle_document_operation"
                );
                let result = self
                    .handle_document_operation(&source, &user_query, conversation_id)
                    .await;
                tracing::info!(
                    success = result.is_ok(),
                    total_latency_ms = turn_start.elapsed().as_millis() as u64,
                    "runtime: turn end (document_operation)"
                );
                return result;
            }
        }

        // 3. Dispatch based on intent.
        let dispatch = match intent {
            Intent::ComplexTask => "handle_complex_task",
            Intent::KnowledgeQuery => "handle_knowledge_query",
            _ => "handle_simple",
        };
        tracing::info!(dispatch, "runtime: dispatching");

        let result = match intent {
            Intent::ComplexTask => {
                self.handle_complex_task(message, conversation_id, &context, &tool_descriptors)
                    .await
            }
            Intent::KnowledgeQuery => {
                self.handle_knowledge_query(
                    message, conversation_id, &context, coarse_intent, self_assessment,
                )
                .await
            }
            _ => {
                self.handle_simple(
                    message, conversation_id, &context, &intent, coarse_intent, self_assessment,
                )
                .await
            }
        };

        tracing::info!(
            dispatch,
            success = result.is_ok(),
            total_latency_ms = turn_start.elapsed().as_millis() as u64,
            "runtime: turn end"
        );
        result
    }

    /// PR2 — non-streaming `MoveKind::Ask` handler. Same shape as
    /// `handle_ask_move_stream` but returns a `Response` instead of
    /// a `StreamHandle`. CLI / server callers receive the placeholder
    /// assistant message with clarification metadata; the `Ask` event
    /// is emitted on the routing sink (no-op in headless builds).
    async fn handle_ask_move_turn(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        classification: &RouterClassification,
    ) -> Result<Response> {
        let message_id = uuid::Uuid::new_v4().to_string();
        let question = build_clarification_question(
            original_message,
            &classification.primary.intent,
        );
        let options: Vec<ClarificationOption> = classification
            .alternatives
            .iter()
            .map(|c| ClarificationOption {
                label: label_for_intent(&c.intent),
                follow_up: original_message.to_string(),
                intent_hint: intent_hint(&c.intent),
            })
            .collect();

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        let placeholder_body =
            "I want to make sure I give you the right shape of answer.".to_string();
        let metadata = serde_json::json!({
            "move_kind": "ask",
            "confidence": classification.primary.confidence,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
            "coarse_intent": classification.coarse_intent,
        });
        let assistant_msg = Message {
            id: message_id,
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body,
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;
        let response_msg = assistant_msg.clone();

        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            options = classification.alternatives.len(),
            "routing:ask — clarification requested (non-streaming path)"
        );

        Ok(Response {
            message: response_msg,
            task: None,
        })
    }

    /// PR2 — streaming `MoveKind::Ask` handler. Suppress synthesis,
    /// persist a placeholder assistant message whose metadata carries
    /// the clarification payload (so the UI's existing
    /// message-metadata plumbing can render the `ClarificationCard`
    /// without a second event channel), emit
    /// `clarification-request`, and return an already-closed stream
    /// so the desktop relay promptly fires `message-complete`.
    ///
    /// No Fast-slot synthesis runs. No retrieval runs. The only cost
    /// is saving one message + emitting one event — the whole point
    /// of the Ask move is cheap engagement when confidence is low.
    async fn handle_ask_move_stream(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        classification: &RouterClassification,
    ) -> Result<StreamHandle> {
        let message_id = uuid::Uuid::new_v4().to_string();

        // Build clarification payload from the classifier's
        // alternatives. If the heuristic surfaced fewer than two, pad
        // with a free-text prompt so the user always has a way forward.
        let question = build_clarification_question(
            original_message,
            &classification.primary.intent,
        );
        let options: Vec<ClarificationOption> = classification
            .alternatives
            .iter()
            .map(|c| ClarificationOption {
                label: label_for_intent(&c.intent),
                follow_up: original_message.to_string(),
                intent_hint: intent_hint(&c.intent),
            })
            .collect();

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        // Persist a placeholder assistant message so the turn shows
        // up in history. Body is intentionally terse — the
        // ClarificationCard above the message is the actual UX.
        let placeholder_body =
            "I want to make sure I give you the right shape of answer.".to_string();
        let metadata = serde_json::json!({
            "move_kind": "ask",
            "confidence": classification.primary.confidence,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
            "coarse_intent": classification.coarse_intent,
        });
        let assistant_msg = Message {
            id: message_id.clone(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body.clone(),
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;

        // Emit the clarification event (no-op for NoOpRoutingEventSink,
        // Tauri emit in desktop builds).
        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            options = classification.alternatives.len(),
            "routing:ask — clarification requested, synthesis suppressed"
        );

        // Return an already-closed stream. The desktop relay reads
        // until the stream ends, then fetches metadata and fires
        // `message-complete` as normal.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(1);
        // Send the placeholder text as one chunk so the bubble
        // renders immediately and the UI can read metadata. Drop `tx`
        // right after so the relay sees EOF on the next poll.
        let _ = tx.send(Ok(placeholder_body)).await;
        drop(tx);

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(StreamHandle {
            message_id,
            stream: Box::pin(stream),
        })
    }

    /// PR5 — post-retrieval Ask diversion. Fires when retrieval ran
    /// successfully but produced dispersed noise (see
    /// `EvidenceShape::is_off_target`). Classification was high
    /// enough to commit, but synthesis against off-target evidence
    /// is exactly the shape that produces confident fabrication —
    /// so we suppress synthesis and show the user their options
    /// instead: answer from general knowledge (explicit opt-in),
    /// search the web (if tool available), or rephrase.
    ///
    /// Returns a closed stream with a placeholder message carrying
    /// a `clarification` metadata field — same shape the regular
    /// Ask path uses, so the existing `ClarificationCard` renders
    /// it without any UI-layer changes.
    async fn handle_retrieval_miss_stream(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        shape: &EvidenceShape,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<StreamHandle> {
        let message_id = uuid::Uuid::new_v4().to_string();

        // Build options aimed at the miss: let the user opt in to
        // parametric synthesis, web-search if available, or
        // rephrase. Three options max — the ClarificationCard also
        // offers a free-text fallback, so adding more options would
        // just be clutter.
        let mut options: Vec<ClarificationOption> = Vec::new();
        options.push(ClarificationOption {
            label: "Answer from general knowledge (may be inaccurate)".to_string(),
            follow_up: original_message.to_string(),
            intent_hint: "simple_query".to_string(),
        });
        if let Some(web_tool) = tool_descriptors
            .iter()
            .find(|t| t.name.contains("web_search") || t.name == "search")
        {
            options.push(ClarificationOption {
                label: "Search the web".to_string(),
                follow_up: original_message.to_string(),
                intent_hint: format!("simple_action:{}", web_tool.id),
            });
        }
        options.push(ClarificationOption {
            label: "Rephrase — I'll try again".to_string(),
            // Empty follow_up signals "wait for user input" — the UI
            // surfaces the clarification card's free-text box.
            follow_up: original_message.to_string(),
            intent_hint: "deep_query".to_string(),
        });

        let question = format!(
            "I searched {} sources but nothing looked relevant to \"{}\". \
             How would you like me to proceed?",
            shape.distinct_sources,
            truncate_with_ellipsis(original_message, 80),
        );

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        let placeholder_body = format!(
            "I didn't find anything relevant in your installed knowledge bases \
             for that question. Rather than guess, I'd like to check how you'd \
             like me to proceed."
        );
        let metadata = serde_json::json!({
            "move_kind": "ask",
            "retrieval_missed": true,
            "documents_found": shape.count,
            "distinct_sources": shape.distinct_sources,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
        });
        let assistant_msg = Message {
            id: message_id.clone(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body.clone(),
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;

        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            distinct_sources = shape.distinct_sources,
            retrieval_count = shape.count,
            "routing:retrieval_miss — synthesis suppressed, clarification requested"
        );

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(1);
        let _ = tx.send(Ok(placeholder_body)).await;
        drop(tx);

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(StreamHandle {
            message_id,
            stream: Box::pin(stream),
        })
    }

    /// Test-only entry point that directly invokes
    /// `handle_retrieval_miss_stream`. Integration tests can't
    /// easily drive the full KnowledgeQuery pipeline (no corpora in
    /// the harness), so this exposes the diversion method for
    /// isolated verification.
    pub async fn invoke_retrieval_miss_stream_for_test(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        shape: &EvidenceShape,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<StreamHandle> {
        self.handle_retrieval_miss_stream(
            original_message,
            conversation_id,
            session_id,
            shape,
            tool_descriptors,
        )
        .await
    }

    /// PR5 — non-streaming sibling of `handle_retrieval_miss_stream`.
    /// Same suppression + clarification semantics; returns a
    /// Response carrying the placeholder body + metadata so CLI /
    /// server callers get a consistent behavior.
    async fn handle_retrieval_miss_response(
        &self,
        original_message: &str,
        conversation_id: &str,
        session_id: &str,
        shape: &EvidenceShape,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        let message_id = uuid::Uuid::new_v4().to_string();

        let mut options: Vec<ClarificationOption> = Vec::new();
        options.push(ClarificationOption {
            label: "Answer from general knowledge (may be inaccurate)".to_string(),
            follow_up: original_message.to_string(),
            intent_hint: "simple_query".to_string(),
        });
        if let Some(web_tool) = tool_descriptors
            .iter()
            .find(|t| t.name.contains("web_search") || t.name == "search")
        {
            options.push(ClarificationOption {
                label: "Search the web".to_string(),
                follow_up: original_message.to_string(),
                intent_hint: format!("simple_action:{}", web_tool.id),
            });
        }
        options.push(ClarificationOption {
            label: "Rephrase — I'll try again".to_string(),
            follow_up: original_message.to_string(),
            intent_hint: "deep_query".to_string(),
        });

        let question = format!(
            "I searched {} sources but nothing looked relevant to \"{}\". \
             How would you like me to proceed?",
            shape.distinct_sources,
            truncate_with_ellipsis(original_message, 80),
        );

        let clarification_payload = ClarificationRequest {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            question: question.clone(),
            options: options.clone(),
        };

        let placeholder_body =
            "I didn't find anything relevant in your installed knowledge bases \
             for that question. Rather than guess, I'd like to check how you'd \
             like me to proceed."
                .to_string();
        let metadata = serde_json::json!({
            "move_kind": "ask",
            "retrieval_missed": true,
            "documents_found": shape.count,
            "distinct_sources": shape.distinct_sources,
            "clarification": {
                "session_id": session_id,
                "question": question,
                "options": options,
            },
        });
        let assistant_msg = Message {
            id: message_id,
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: placeholder_body,
            created_at: now(),
            metadata: Some(metadata),
            version: 0,
        };
        self.store.save_message(&assistant_msg).await?;
        let response_msg = assistant_msg.clone();

        self.routing_events
            .emit_clarification_request(clarification_payload)
            .await;

        tracing::info!(
            session_id,
            conversation_id,
            distinct_sources = shape.distinct_sources,
            retrieval_count = shape.count,
            "routing:retrieval_miss — synthesis suppressed (non-streaming)"
        );

        Ok(Response {
            message: response_msg,
            task: None,
        })
    }

    /// Handle SimpleQuery, DeepQuery, and other non-plan intents.
    /// Searches all knowledge sources before generating a response.
    async fn handle_simple(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        intent: &Intent,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
    ) -> Result<Response> {
        // Search knowledge + build prompt (shared with handle_message_stream).
        let kc = self
            .prepare_knowledge_context(message, context, intent)
            .await;

        let oicp = if matches!(intent, Intent::SimpleQuery) {
            None
        } else {
            self.build_oicp(&intent)
        };

        let request = CompletionRequest {
            prompt: kc.prompt,
            system_message: Some(kc.system),
            preferred_speed: kc.speed,
            max_tokens: Some(self.inference_config.max_tokens),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(self.inference_config.think_budget),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp,
                    tools: None,
                    tool_choice: None,
                            model_id: None,
        };

        let completion = self.inference.complete(&request).await?;

        // Epistemic-humility hook (see Runtime::maybe_collaborate).
        // No-ops when disabled. Evidence is the same formatted-chunks text
        // that was injected into the synthesis prompt (or empty if no
        // corpus material was retrieved).
        let evidence = format_scored_chunks(&kc.chunks, MAX_KNOWLEDGE_CHARS);
        let final_content = self
            .maybe_collaborate(conversation_id, message, &completion.text, &evidence)
            .await;

        let provenance = ResponseProvenance {
            intent: format!("{intent:?}"),
            search_method: kc.search_method,
            sources: kc.sources,
            inference_backend: completion.model_id.clone(),
            oicp_match: completion
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: completion.latency_ms,
            tokens_used: completion.tokens_used,
            coarse_intent,
            self_assessment,
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": completion.model_id,
                "tokens": completion.tokens_used,
                "latency_ms": completion.latency_ms,
                "provenance": provenance,
                "retrieved_chunks": kc.retrieved_chunks,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    /// Build the complete synthesis plan for a KnowledgeQuery turn:
    /// retrieval + evidence-shape routing + source-cohesion expansion +
    /// request construction + metadata for the UI (retrieved_chunks
    /// summaries, source_map, result_quality).
    ///
    /// Shared between [`Self::handle_knowledge_query`] (non-streaming)
    /// and the streaming KQ branch in
    /// [`Self::handle_message_stream`] so both paths cannot diverge
    /// in how they search, expand, or build the request.
    async fn prepare_knowledge_query_plan(
        &self,
        message: &str,
        context: &ConversationContext,
    ) -> KnowledgeQueryPlan {
        use std::cmp::Ordering;

        tracing::info!(message_chars = message.len(), "handle_knowledge_query: begin");

        // 1. Embed the query using the query-side function (applies
        //    instruction prefix for asymmetric models like Qwen3-Embedding).
        let t_search = std::time::Instant::now();
        let embedding = self.inference.embed_query(message).await.unwrap_or_default();

        // 2. Search corpus-engine LanceDB indexes.
        let mut chunks = self
            .search_corpus_indexes(&embedding, message, 5, "KnowledgeQuery")
            .await;

        // 3. Sort by score, keep top 8.
        chunks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        chunks.truncate(8);

        let search_ms = t_search.elapsed().as_millis() as u64;
        tracing::info!(
            chunks_found = chunks.len(),
            search_ms,
            "handle_knowledge_query: corpus search done"
        );

        // 4a. Empty results path — answer from parametric knowledge.
        if chunks.is_empty() {
            tracing::info!("KnowledgeQuery: no chunks — answering from parametric knowledge");
            let corpora = context.installed_corpora_display();
            let prompt = format!(
                "The user asked: \"{message}\"\n\n\
                 You searched these installed knowledge sources: {corpora}\n\
                 The search returned no relevant results.\n\n\
                 Answer the question from your general knowledge. \
                 Note briefly that no corpus results were found, but do not refuse \
                 to answer or dwell on the absence of sources. \
                 If you are confident about the topic, answer directly and substantively. \
                 If you are genuinely uncertain, say so and suggest web search or \
                 installing an additional corpus."
            );
            let request = CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Fast,
                max_tokens: Some(300),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                            model_id: None,
            };
            return KnowledgeQueryPlan {
                request,
                chunks: Vec::new(),
                doc_context: String::new(),
                shape: compute_evidence_shape(&[], message),
                route: SynthesisRoute::FastFocused,
                // Empty retrieval is the strongest case for asking the
                // user to supply something — keep the gap check on.
                gap_check_enabled: true,
                search_ms,
                retrieved_chunks: Vec::new(),
                source_map: HashMap::new(),
                result_quality: "empty",
            };
        }

        // 4b. Evidence-shape routing.
        let shape = compute_evidence_shape(&chunks, message);
        let route = route_from_evidence(&shape);
        tracing::info!(
            count = shape.count,
            top1 = shape.top1_score,
            median = shape.median_score,
            median_ratio = shape.median_ratio,
            top_source_repeat = shape.top_source_repeat_count,
            distinct_sources = shape.distinct_sources,
            title_match = shape.title_match,
            top_source = %shape.top_source_label,
            route = ?route,
            "KnowledgeQuery: evidence-shape routing decision"
        );

        // 4c. Source-cohesion expansion (FastFocused + >=2 repeats).
        let expansion_fired = matches!(route, SynthesisRoute::FastFocused)
            && shape.top_source_repeat_count >= EVIDENCE_MIN_TOP_SOURCE_REPEAT;
        let (chunks, knowledge_char_budget) = if expansion_fired {
            let (expanded, _from_source, _grounding, _dropped) =
                self.expand_from_dominant_source(chunks, &shape).await;
            (expanded, EXPANDED_KNOWLEDGE_CHARS)
        } else {
            (chunks, MAX_KNOWLEDGE_CHARS)
        };

        // 4d. Build prompt. Retrieved content first, question last —
        // keeps the model from reasoning purely from training weights
        // during its <think> phase (when Primary path is taken).
        let doc_context = format_scored_chunks(&chunks, knowledge_char_budget);
        let corpus_display = context.installed_corpora_display();
        let prompt = format!(
            "RETRIEVED FROM {corpus_display}:\n\n{doc_context}\n\n\
             ════════════════════════════════════\n\n\
             Question: {message}"
        );

        // 4e. Request shape varies by route.
        let request = match route {
            SynthesisRoute::FastFocused => {
                let system = self.build_system_message(KNOWLEDGE_SYNTHESIS_SYSTEM, context);
                CompletionRequest {
                    prompt,
                    system_message: Some(system),
                    preferred_speed: Speed::Fast,
                    max_tokens: Some(FAST_KNOWLEDGE_MAX_TOKENS as usize),
                    temperature: Some(self.inference_config.temperature),
                    think_budget: Some(0),
                    structured_output: None,
                    top_k: self.inference_config.top_k,
                    top_p: None,
                    oicp: None,
                    tools: None,
                    tool_choice: None,
                                    model_id: None,
                }
            }
            SynthesisRoute::PrimarySynthesis => {
                let system = self.build_primary_system_message(
                    &format!("{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{THINKING_DIRECTIVE}"),
                    context,
                );
                CompletionRequest {
                    prompt,
                    system_message: Some(system),
                    preferred_speed: Speed::Slow,
                    max_tokens: Some(self.inference_config.max_tokens),
                    temperature: Some(self.inference_config.temperature),
                    think_budget: Some(self.inference_config.think_budget),
                    structured_output: None,
                    top_k: self.inference_config.top_k,
                    top_p: None,
                    oicp: self.build_oicp(&Intent::KnowledgeQuery),
                    tools: None,
                    tool_choice: None,
                                    model_id: None,
                }
            }
        };

        // 4f. Build retrieved_chunks summaries for the UI citation
        // expander. Same shape `prepare_knowledge_context` produces so
        // the frontend renders both paths identically.
        let retrieved_chunks: Vec<serde_json::Value> = chunks
            .iter()
            .map(|c| {
                let snippet = truncate_with_ellipsis(&c.content, 200);
                serde_json::json!({
                    "title": c.title.as_deref().unwrap_or(""),
                    "corpus_id": c.corpus_id,
                    "url": c.url,
                    "snippet": snippet,
                    "provenance_tier": if c.url.is_some() { "web" } else { "corpus" },
                })
            })
            .collect();

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }

        // Gap check gating: only on multi-source / weak-evidence
        // synthesis, where the Fast path's "skip gap check" would hide
        // a genuine hole in the answer.
        let gap_check_enabled = matches!(route, SynthesisRoute::PrimarySynthesis);

        let result_quality = if expansion_fired {
            "focused"
        } else if matches!(route, SynthesisRoute::PrimarySynthesis) {
            "synthesis"
        } else {
            "routed"
        };

        let _ = expansion_fired; // logged by expand_from_dominant_source already

        KnowledgeQueryPlan {
            request,
            chunks,
            doc_context,
            shape,
            route,
            gap_check_enabled,
            search_ms,
            retrieved_chunks,
            source_map,
            result_quality,
        }
    }

    /// Handle KnowledgeQuery: search corpus-engine LanceDB indexes → inject into prompt → synthesize.
    async fn handle_knowledge_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
    ) -> Result<Response> {
        let plan = self.prepare_knowledge_query_plan(message, context).await;

        // PR5 — non-streaming retrieval-miss diversion. Mirrors the
        // streaming path: dispersed noise → suppress synthesis +
        // surface clarification instead of confabulating.
        if plan.shape.is_off_target() {
            let session_id = self
                .sessions
                .latest_for_conversation(conversation_id)
                .map(|s| s.id)
                .unwrap_or_default();
            let tool_descriptors = self.tools.descriptors();
            tracing::info!(
                distinct_sources = plan.shape.distinct_sources,
                retrieval_count = plan.shape.count,
                "routing:retrieval_miss — non-streaming diversion"
            );
            return self
                .handle_retrieval_miss_response(
                    message,
                    conversation_id,
                    &session_id,
                    &plan.shape,
                    &tool_descriptors,
                )
                .await;
        }

        let completion = self.inference.complete(&plan.request).await?;

        let final_content = if plan.gap_check_enabled {
            tracing::debug!(
                route = ?plan.route,
                "KnowledgeQuery: running gap check (multi-source or weak evidence)"
            );
            self.maybe_collaborate(
                conversation_id,
                message,
                &completion.text,
                &plan.doc_context,
            )
            .await
        } else {
            tracing::info!(
                route = ?plan.route,
                top_source = %plan.shape.top_source_label,
                "KnowledgeQuery: skipping gap check (concentrated single-source)"
            );
            completion.text.clone()
        };

        let provenance = ResponseProvenance {
            intent: "KnowledgeQuery".to_string(),
            search_method: Some("CorpusEngine".to_string()),
            sources: plan
                .source_map
                .iter()
                .map(|(origin, &count)| SourceSummary {
                    origin: origin.clone(),
                    count,
                    from_peer: None,
                })
                .collect(),
            inference_backend: completion.model_id.clone(),
            oicp_match: completion
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: completion.latency_ms,
            tokens_used: completion.tokens_used,
            coarse_intent,
            self_assessment,
        };

        // PR3 — grounded next-step offers. Look up the most recent
        // session for this conversation (handle_turn created one
        // right before dispatching here); fall back to a synthetic
        // id when the session isn't present (e.g. legacy test
        // harnesses that don't wire the session store).
        let session_id = self
            .sessions
            .latest_for_conversation(conversation_id)
            .map(|s| s.id)
            .unwrap_or_default();
        let had_dominant_source = plan.shape.top_source_repeat_count >= 2;
        let retrieval_missed = plan.shape.is_off_target();
        let top_source_title = if plan.shape.top_source_key.1.is_empty() {
            None
        } else {
            Some(plan.shape.top_source_key.1.clone())
        };
        let offers = build_next_step_offers(&OfferContext {
            user_message: message,
            top_source_title: top_source_title.as_deref(),
            had_dominant_source,
            retrieved_chunks: &plan.retrieved_chunks,
            session_id: &session_id,
            retrieval_missed,
        });
        let offers_json = serde_json::to_value(&offers).unwrap_or_default();

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": completion.model_id,
                "tokens": completion.tokens_used,
                "latency_ms": completion.latency_ms,
                "intent": "knowledge_query",
                "documents_found": plan.chunks.len(),
                "search_ms": plan.search_ms,
                "result_quality": plan.result_quality,
                "provenance": provenance,
                "retrieved_chunks": plan.retrieved_chunks,
                "next_steps": offers_json,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    /// Handle ComplexTask: plan → execute → (replan on failure) → synthesize.
    /// Handle document analysis: bypass planner, call document_operation directly.
    ///
    /// 1. Resolve the source path from the store
    /// 2. Generate map/reduce prompts with a single inference call
    /// 3. Call document_operation tool directly with deterministic params
    /// 4. Synthesize the result into a response
    async fn handle_document_operation(
        &self,
        source_hint: &str,
        user_query: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        tracing::info!(source_hint = %source_hint, "runtime: document_operation — resolving source");

        // 1. Resolve actual source path from the store.
        let sources = self.store.list_sources().await.unwrap_or_default();
        let source_lower = source_hint.to_lowercase();
        let resolved_source = sources
            .iter()
            .find(|s| s.to_lowercase().contains(&source_lower))
            .cloned()
            .unwrap_or_else(|| source_hint.to_string());

        tracing::debug!(
            resolved_source = %resolved_source,
            available_sources = sources.len(),
            "runtime: document_operation — source resolved"
        );

        // Get chunk count for the prompt.
        let chunks = self.store.get_chunks_by_source(&resolved_source).await.unwrap_or_default();
        let chunk_count = chunks.len();
        let word_count: usize = chunks.iter().map(|c| c.content.split_whitespace().count()).sum();
        drop(chunks);

        if chunk_count == 0 {
            tracing::warn!(
                source = %resolved_source,
                "runtime: document_operation — no chunks found for source"
            );
            let assistant_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: format!(
                    "No document chunks found for '{}'. The document may not have been ingested correctly.",
                    source_hint
                ),
                created_at: now(),
                metadata: None,
                version: now(),
            };
            self.store.save_message(&assistant_msg).await?;
            self.spawn_auto_title(conversation_id);
            return Ok(Response { message: assistant_msg, task: None });
        }

        tracing::info!(
            source = %resolved_source,
            chunks = chunk_count,
            words = word_count,
            user_query_chars = user_query.len(),
            "runtime: document_operation — generating map/reduce prompts"
        );

        // 2. Generate map/reduce prompts with a single focused inference call.
        let prompt_request = CompletionRequest {
            prompt: format!(
                "The user uploaded a document ({chunk_count} chunks, ~{word_count} words) and asked:\n\
                 \"{user_query}\"\n\n\
                 Write two prompts for a map-reduce analysis of this document.\n\n\
                 MAP PROMPT — applied to each chunk of the document:\n\
                 - Extract only what's present in that chunk\n\
                 - Produce structured notes relevant to the user's request\n\
                 - Do NOT invent or assume content not in the chunk\n\n\
                 REDUCE PROMPT — merges all extracted notes into one result:\n\
                 - Synthesize into a coherent, comprehensive answer\n\
                 - Deduplicate and organize logically\n\n\
                 Respond in JSON only:\n\
                 {{\"map_prompt\": \"...\", \"reduce_prompt\": \"...\"}}"
            ),
            system_message: Some(
                "You write analysis prompts. Output ONLY the JSON object, nothing else.".to_string()
            ),
            // Use the primary model for prompt generation — it's a one-time
            // cost and the 0.6B fast model can't reliably produce JSON.
            preferred_speed: Speed::Slow,
            max_tokens: Some(512),
            temperature: Some(0.0),
            think_budget: Some(0), // no thinking — just produce the JSON
            structured_output: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "map_prompt": { "type": "string" },
                    "reduce_prompt": { "type": "string" }
                },
                "required": ["map_prompt", "reduce_prompt"]
            })),
            // think_budget already set above
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
                    model_id: None,
        };

        let prompt_response = self.inference.complete(&prompt_request).await?;
        let prompt_text = prompt_response.text.trim();

        // Parse the generated prompts. Strip think tags and code fences
        // before parsing — models often wrap JSON in these.
        let cleaned = prompt_text
            // Strip <think>...</think> blocks (Qwen3 thinking mode).
            .split("</think>")
            .last()
            .unwrap_or(prompt_text)
            .trim()
            // Strip markdown code fences.
            .strip_prefix("```json")
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(
                prompt_text
                    .split("</think>")
                    .last()
                    .unwrap_or(prompt_text)
                    .trim()
            )
            .trim();

        let (map_prompt, reduce_prompt) = match serde_json::from_str::<serde_json::Value>(cleaned) {
            Ok(v) => {
                let mp = v.get("map_prompt").and_then(|v| v.as_str()).unwrap_or(
                    "Extract key information relevant to the user's question from this passage."
                ).to_string();
                let rp = v.get("reduce_prompt").and_then(|v| v.as_str()).unwrap_or(
                    "Synthesize all extracted information into a comprehensive answer."
                ).to_string();
                (mp, rp)
            }
            Err(e) => {
                // Fallback: use specific prompts tailored to the user's question.
                tracing::warn!(
                    error = %e,
                    raw_output = %prompt_text,
                    "Failed to parse prompt JSON — using tailored fallback prompts"
                );
                (
                    format!(
                        "Read this passage carefully. The user asked: \"{user_query}\"\n\n\
                         Extract ALL information from this passage that is relevant to \
                         answering the user's question. Include:\n\
                         - Key facts, events, or arguments\n\
                         - Character names and their actions (if narrative)\n\
                         - Direct quotes that are significant\n\
                         If nothing relevant appears, respond with just: null"
                    ),
                    format!(
                        "The user asked: \"{user_query}\"\n\n\
                         You have been given extracted notes from across an entire document. \
                         Synthesize ALL the extracted information into a comprehensive, \
                         well-organized answer to the user's question. \
                         Be thorough — include every relevant detail from the notes. \
                         Organize logically with clear sections."
                    ),
                )
            }
        };

        tracing::debug!(
            map_prompt_chars = map_prompt.len(),
            reduce_prompt_chars = reduce_prompt.len(),
            "runtime: document_operation — prompts generated"
        );
        tracing::info!("runtime: document_operation — invoking map/reduce");

        // 3. Call document_operation tool directly.
        let tool = self.tools.get("document_operation")?;
        let params = serde_json::json!({
            "source": resolved_source,
            "operation": user_query,
            "map_prompt": map_prompt,
            "reduce_prompt": reduce_prompt,
            "conversation_id": conversation_id,
        });

        let tool_ctx = ToolContext {
            conversation_id: conversation_id.to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
        };

        let result = tool.execute(&params, &tool_ctx).await?;
        let result_text = match &result {
            StepOutput::Text(t) => t.clone(),
            StepOutput::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            _ => String::new(),
        };

        tracing::info!(
            output_chars = result_text.len(),
            "runtime: document_operation — complete"
        );

        // 4. Build response.
        let provenance = ResponseProvenance {
            intent: "DocumentOperation".to_string(),
            search_method: Some("document_operation".to_string()),
            sources: vec![SourceSummary {
                origin: "user_document".to_string(),
                count: chunk_count,
                from_peer: None,
            }],
            inference_backend: prompt_response.model_id.clone(),
            oicp_match: None,
            total_latency_ms: 0,
            tokens_used: 0,
            coarse_intent: None,
            self_assessment: None,
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: result_text,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "provenance": provenance,
                "document_source": resolved_source,
                "document_chunks": chunk_count,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    async fn handle_complex_task(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        // Document-attached messages are handled by handle_document_operation
        // before reaching this point. This path is for non-document ComplexTasks.

        tracing::info!("runtime: complex_task — generating plan");
        let plan = self
            .planner
            .plan(message, context, tool_descriptors)
            .await?;

        tracing::info!(
            steps = plan.steps.len(),
            "runtime: complex_task — plan generated"
        );
        for step in &plan.steps {
            tracing::debug!(
                step_id = step.id,
                description = %step.description,
                kind = ?std::mem::discriminant(&step.kind),
                "runtime: complex_task — step"
            );
        }

        // 2. Create task.
        let mut task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            goal: message.to_string(),
            plan: plan.clone(),
            status: TaskStatus::Running,
            completed_steps: Vec::new(),
            created_at: now(),
            updated_at: now(),
            version: now(),
        };
        self.store.save_task(&task).await?;

        // 3. Execute.
        let executor = Executor::new(
            Arc::clone(&self.inference),
            Arc::clone(&self.tools),
            Arc::clone(&self.store),
            Arc::clone(&self.approval),
            Arc::clone(&self.skills),
        );

        let mut ctx = TaskContext {
            task: task.clone(),
            completed: HashMap::new(),
        };

        let mut result = executor.run(&plan, &mut ctx).await?;

        // 4. Replan on failure (one retry).
        if let Some(ref error) = result.error {
            tracing::warn!(
                step_id = error.step_id,
                error = %error.message,
                "runtime: complex_task — step failed, attempting replan"
            );

            let completed_vec: Vec<(usize, StepOutput)> =
                result.completed.iter().map(|(&k, v)| (k, v.clone())).collect();

            match self.planner.replan(&plan, &completed_vec, error).await {
                Ok(new_plan) => {
                    tracing::info!(
                        steps = new_plan.steps.len(),
                        "runtime: complex_task — replan generated"
                    );
                    task.plan = new_plan.clone();
                    task.status = TaskStatus::Running;
                    task.updated_at = now();

                    let mut retry_ctx = TaskContext {
                        task: task.clone(),
                        completed: HashMap::new(),
                    };

                    result = executor.run(&new_plan, &mut retry_ctx).await?;

                    if result.error.is_some() {
                        tracing::warn!("runtime: complex_task — replan also failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "runtime: complex_task — replan failed");
                }
            }
        }

        // 5. Synthesize final answer from step outputs.
        let step_summaries: Vec<String> = result
            .completed
            .iter()
            .filter_map(|(id, output)| match output {
                StepOutput::Text(t) => Some(format!("Step {id}: {t}")),
                StepOutput::Json(v) => {
                    // For search tool output, use the "answer" field.
                    let text = v
                        .get("answer")
                        .and_then(|a| a.as_str())
                        .unwrap_or_else(|| {
                            // Fallback: serialize the whole JSON.
                            ""
                        });
                    if text.is_empty() {
                        Some(format!("Step {id}: {}", serde_json::to_string_pretty(v).unwrap_or_default()))
                    } else {
                        Some(format!("Step {id}: {text}"))
                    }
                }
                StepOutput::ReasonWithToolsResult { ref text, iterations, capped, .. } => {
                    let note = if *capped { " (search cap reached)" } else { "" };
                    Some(format!("Step {id} ({iterations} searches{note}): {text}"))
                }
                _ => None,
            })
            .collect();

        let synthesis_prompt = format!(
            "Goal: {message}\n\nStep results:\n{}\n\nProvide a comprehensive final answer that synthesizes all the step results above.",
            step_summaries.join("\n\n")
        );

        let synthesis_system = self.build_primary_system_message(
            "Synthesize the given step results into a clear, comprehensive answer.",
            context,
        );

        let synthesis = self
            .inference
            .complete(&CompletionRequest {
                prompt: synthesis_prompt,
                system_message: Some(synthesis_system),
                preferred_speed: Speed::Slow,
                max_tokens: Some(self.inference_config.max_tokens),
                temperature: Some(self.inference_config.temperature),
                think_budget: Some(self.inference_config.think_budget),
                structured_output: None,
                top_k: self.inference_config.top_k,
                top_p: None,
                oicp: self.build_oicp(&Intent::ComplexTask),
            tools: None,
            tool_choice: None,
                        model_id: None,
            })
            .await?;

        // 6. Update task status.
        task.completed_steps = result.completed.iter().map(|(&k, v)| (k, v.clone())).collect();
        task.status = if result.error.is_some() {
            TaskStatus::Failed
        } else {
            TaskStatus::Completed
        };
        task.updated_at = now();
        self.store.save_task(&task).await?;

        // 7. Extract search provenance from tool step outputs.
        let mut search_method: Option<String> = None;
        let mut all_sources: Vec<SourceSummary> = Vec::new();
        for (_step_idx, output) in &task.completed_steps {
            match output {
                StepOutput::Json(ref val) => {
                    if let Some(method) = val.get("search_method").and_then(|v| v.as_str()) {
                        search_method = Some(method.to_string());
                    }
                    if let Some(sources) = val.get("sources").and_then(|v| v.as_array()) {
                        for src in sources {
                            if let (Some(origin), Some(count)) = (
                                src.get("origin").and_then(|v| v.as_str()),
                                src.get("count").and_then(|v| v.as_u64()),
                            ) {
                                all_sources.push(SourceSummary {
                                    origin: origin.to_string(),
                                    count: count as usize,
                                    from_peer: None,
                                });
                            }
                        }
                    }
                }
                StepOutput::ReasonWithToolsResult {
                    search_log,
                    iterations,
                    ..
                } => {
                    search_method = Some(format!("ReasonWithTools ({iterations} iterations)"));
                    // Aggregate search log into source summaries.
                    let mut tool_counts: HashMap<String, usize> = HashMap::new();
                    for entry in search_log {
                        *tool_counts
                            .entry(entry.tool_id.clone())
                            .or_insert(0) += entry.result_count;
                    }
                    for (tool_id, count) in tool_counts {
                        all_sources.push(SourceSummary {
                            origin: tool_id,
                            count,
                            from_peer: None,
                        });
                    }
                }
                _ => {}
            }
        }

        // Save and return assistant message.
        let provenance = ResponseProvenance {
            intent: "ComplexTask".to_string(),
            search_method,
            sources: all_sources,
            inference_backend: synthesis.model_id.clone(),
            oicp_match: synthesis
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: synthesis.latency_ms,
            tokens_used: synthesis.tokens_used,
            coarse_intent: None,
            self_assessment: None,
        };

        // Epistemic-humility hook (see Runtime::maybe_collaborate).
        // Evidence is the same `step_summaries` the synthesis prompt saw
        // — keeps the gap check grounded in exactly what the model had.
        let evidence = step_summaries.join("\n\n");
        let final_content = self
            .maybe_collaborate(conversation_id, message, &synthesis.text, &evidence)
            .await;

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": synthesis.model_id,
                "tokens": synthesis.tokens_used,
                "latency_ms": synthesis.latency_ms,
                "task_id": task.id,
                "steps_completed": task.completed_steps.len(),
                "provenance": provenance,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: Some(task),
        })
    }
}

#[cfg(test)]
mod score_normalisation_tests {
    use super::normalise_scores_per_corpus;
    use corpus_engine::ScoredChunk;
    use std::collections::HashMap;

    fn chunk(corpus: &str, content: &str, score: f32) -> ScoredChunk {
        ScoredChunk {
            content: content.into(),
            title: Some(content.into()),
            url: None,
            corpus_id: corpus.into(),
            score,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn sep_beats_code_after_normalisation() {
        // Reconstruct the observed scenario from the 08:40 demo
        // logs: a BM25 outlier from a small code corpus (score
        // 21.37) was out-ranking SEP's genuine top match (19.25)
        // in the merged list even though SEP had 8 consistently-
        // strong hits and code had 1 outlier + a long tail.
        //
        // After per-corpus max normalisation the code outlier
        // reduces to 1.0, SEP's top also reduces to 1.0, but
        // SEP's *second* chunk at 0.954 outranks code's *second*
        // chunk at 0.700 — so top-8 ends up dominated by SEP.
        let mut chunks = vec![
            // SEP: 8 strong, clustered hits (realistic shape).
            chunk("sep", "compatibilism", 19.25),
            chunk("sep", "incompatibilism-arguments-1", 18.37),
            chunk("sep", "locke-freedom", 18.16),
            chunk("sep", "providence-divine", 16.95),
            chunk("sep", "incompatibilism-arguments-2", 16.83),
            chunk("sep", "incompatibilism-arguments-3", 16.57),
            chunk("sep", "frankfurt-aim", 15.65),
            chunk("sep", "moral-responsibility", 15.48),
            // corpus-engine: 1 spurious outlier + long tail.
            chunk("corpus-engine", "extract_questions_prefers_canonical", 21.37),
            chunk("corpus-engine", "test_skeleton", 14.96),
            chunk("corpus-engine", "mock_inference_fn", 14.80),
            // sovereign: a code corpus with middling matches.
            chunk("sovereign", "needs_deep_reasoning", 16.44),
            chunk("sovereign", "LlmRouter", 12.91),
        ];

        normalise_scores_per_corpus(&mut chunks);
        chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let top8: Vec<&str> = chunks
            .iter()
            .take(8)
            .map(|c| c.corpus_id.as_str())
            .collect();

        // The code outlier still appears (score 1.0, tied for
        // top) — that's correct: it IS its corpus's top hit, so
        // the merge can't hide it without losing evidence from
        // that corpus entirely. But SEP should contribute at
        // least 5 of the top 8, because its within-corpus ranks
        // 2..=6 all score above code's rank 2 after rescaling.
        let sep_count = top8.iter().filter(|&&c| c == "sep").count();
        assert!(
            sep_count >= 5,
            "expected SEP to dominate top-8 after normalisation; \
             got corpus list {top8:?}"
        );
    }

    #[test]
    fn preserves_within_corpus_ranking() {
        // Within a single corpus, the rescaling must not reorder
        // hits — just compress the scale to [0, 1].
        let mut chunks = vec![
            chunk("one", "best", 20.0),
            chunk("one", "mid", 10.0),
            chunk("one", "worst", 5.0),
        ];
        normalise_scores_per_corpus(&mut chunks);
        assert_eq!(chunks[0].content, "best");
        assert!((chunks[0].score - 1.0).abs() < 1e-6);
        assert!((chunks[1].score - 0.5).abs() < 1e-6);
        assert!((chunks[2].score - 0.25).abs() < 1e-6);
    }

    #[test]
    fn empty_input_is_a_noop() {
        // Guard against divide-by-zero or panic on empty slice —
        // `search_corpus_indexes` can legitimately return zero
        // hits when the query embedding is empty AND FTS returns
        // nothing.
        let mut chunks: Vec<ScoredChunk> = Vec::new();
        normalise_scores_per_corpus(&mut chunks);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_corpus_zero_max_stays_zero() {
        // If every hit in a corpus scored 0.0 (shouldn't happen
        // in practice — ScoredChunk implies at least a bare
        // match), the rescaler must not divide by zero.
        let mut chunks = vec![
            chunk("empty", "a", 0.0),
            chunk("empty", "b", 0.0),
        ];
        normalise_scores_per_corpus(&mut chunks);
        assert_eq!(chunks[0].score, 0.0);
        assert_eq!(chunks[1].score, 0.0);
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
        assert!(!is_grounding_candidate(&chunk("conversation-history", None)));
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
mod truncate_chunk_tests {
    use super::{truncate_chunk_content, MAX_CHUNK_CHARS};

    /// Em-dash (U+2014, 3 bytes as UTF-8) placed so its first byte
    /// lands inside the truncation window and the char straddles the
    /// `MAX_CHUNK_CHARS` boundary. Naive `&content[..MAX_CHUNK_CHARS]`
    /// panics with "byte index N is not a char boundary"; the fixed
    /// helper must walk back to the last char boundary.
    #[test]
    fn truncate_does_not_panic_inside_multibyte_char() {
        let a_block = "a".repeat(MAX_CHUNK_CHARS - 1); // byte 0..=598
        // Inject em-dash at byte 598..601 so byte 600 lands inside it.
        let content = format!("{a_block}—tail");
        let out = truncate_chunk_content(&content);
        assert!(out.ends_with("..."), "should have truncation marker");
        // The slice must have stopped at or before byte 598 (start of
        // the em-dash), so the em-dash itself is excluded.
        assert!(
            !out.contains('—'),
            "em-dash straddling boundary must be dropped, not split"
        );
    }

    /// Smart double-quote (U+201C/U+201D, 3 bytes) at the boundary:
    /// same class of failure as em-dash. Belt-and-suspenders test.
    #[test]
    fn truncate_handles_smart_quote_at_boundary() {
        let a_block = "a".repeat(MAX_CHUNK_CHARS - 2);
        let content = format!("{a_block}“word”tail");
        let out = truncate_chunk_content(&content);
        assert!(out.ends_with("..."));
    }

    /// Content shorter than the limit: returned verbatim, no marker.
    #[test]
    fn truncate_passthrough_when_short() {
        let content = "Joan Robinson was an economist.";
        assert_eq!(truncate_chunk_content(content), content);
    }

    /// ASCII-only content at the exact boundary length: no truncation.
    #[test]
    fn truncate_at_exact_boundary_no_marker() {
        let content = "a".repeat(MAX_CHUNK_CHARS);
        let out = truncate_chunk_content(&content);
        assert_eq!(out.len(), MAX_CHUNK_CHARS);
        assert!(!out.ends_with("..."));
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
        assert_eq!(
            strip_leading_title_duplicate(body, Some("Joan")),
            body
        );
    }
}

#[cfg(test)]
mod evidence_shape_tests {
    use super::{
        compute_evidence_shape, route_from_evidence, SynthesisRoute,
    };
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
        assert!(shape.title_match, "'robinson' must match the top chunk's title");
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
        assert_eq!(
            route,
            SynthesisRoute::FastFocused,
            "shape = {shape:?}"
        );
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
        assert!(!shape.title_match, "only overlap is stopwords — should not count");
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
        let shape =
            compute_evidence_shape(&chunks, "Tell me about the Commonwealth scheduler");
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
        let shape =
            compute_evidence_shape(&chunks, "Can you tell me about Joan Robinson?");
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
