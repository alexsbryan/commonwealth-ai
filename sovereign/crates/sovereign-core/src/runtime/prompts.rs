// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synthesis prompt constants + builders, retrieval/merge budget
//! constants, refusal detection, and conversation-history budgets —
//! the pure data-and-policy layer of the runtime (ARCH §6: prompts and
//! tunable tables are data). Extracted verbatim from `runtime.rs` in
//! the 2026-06-10 decomposition; every item keeps its original
//! `pub(crate)` visibility via the glob re-export in `runtime.rs`, so
//! all `super::*`-style consumers are unchanged.

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
pub(crate) const KNOWLEDGE_SYNTHESIS_SYSTEM: &str = "\
You have been given retrieved passages from an installed knowledge base. \
Answer FROM THE PASSAGES — they are your evidence.\n\
\n\
ANSWER, don't deflect. A broad topic the passages cover (a history, an \
overview, an analysis) is ALWAYS answerable: write the fullest treatment the \
PASSAGES support, in sections, and note any thin spots in one line at the \
end. If asked for more than the sources hold, open with \"Thorough overview \
from available sources, not exhaustive\" and proceed — \"exhaustive / every / \
complete\" mean be thorough, NOT fabricate, and are NEVER a reason to refuse, \
stall to \"clarify first,\" or offer to search.\n\
\n\
A REQUESTED LENGTH is a ceiling, not a quota. When the ask names a size (a \
word or page count, \"a long essay,\" \"in exhaustive detail\"), write the \
fullest treatment the PASSAGES support and then STOP — a short, complete, \
grounded answer is correct. Reaching a number by padding, repeating yourself, \
restating the question, or narrating your own reasoning aloud is the WRONG \
answer, worse than a brief one: it manufactures unsupported specifics and \
buries the real answer. When the sources hold less than was asked for, give \
what they hold and say so in one line; do not stretch to fill the request.\n\
\n\
CHECK THE QUESTION'S PREMISE against the passages first. When the question \
asserts a count, name, or fact the passages contradict (\"the five X\" when \
the passages define six), correct the premise in your first line and answer \
with what the passages actually support — never bend the evidence to fit the \
question.\n\
\n\
PRIORITISE WHAT YOU CAN JUSTIFY (basic epistemology). Lead with the facts the \
passages support — state those confidently and cite them [Source: title]. You \
may THEN add relevant general knowledge, but only with explicit humility that \
you cannot justify it from the user's sources: flag it plainly (\"I can't \
confirm this from your sources, but from general knowledge…\") so the reader \
always knows which claims are grounded and which are merely believed. The \
cardinal error is presenting an unjustified belief — a name, number, date, \
value, code symbol, or who-did-what relationship you are not certain the \
passages support — AS IF it were grounded; never do that. But do not lapse \
into brusque refusal either: give what you can justify, then what you \
reasonably believe (clearly flagged), each carried with the humility it \
warrants.\n\
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
CITATION SHAPE IS MANDATORY — the ONLY accepted citation form is \
`[Source: title]` where `title` matches a `[Source: …]` header from \
the retrieved passages above. NEVER use numeric references like \
`[1]`, `[2]`, `[3]`, `[4]`, `[5]`, footnote markers, or any other \
shape. Numeric refs are unclickable in the reader and break the \
glass-box reading surface. If you cannot recall the exact title, \
omit the citation rather than substitute a number — an honest \
unsourced claim is better than a broken citation.\n\
\n\
PRESERVE SOURCE TERMINOLOGY — when the passages use a specific \
named concept, technical term, date, place name, or proper noun \
that bears on what the question asks, reproduce that exact phrase \
in your answer. Do not paraphrase named concepts into descriptive \
prose, do not generalise specific people or places into category \
words (\"the scientist\", \"the king\", \"the institution\"), and \
do not strip dates or numerical specifics that the passages \
supplied. Specific terms, dates, and proper nouns are the \
load-bearing parts of a factual answer — paraphrasing them away \
makes the answer less correct, not more readable. When the \
passages provide a domain term that has a smoother colloquial \
rephrasing, use the domain term anyway: it is what readers will \
recognise and what makes the claim verifiable.\n\
\n\
Anti-fabrication guardrails:\n\
- NEVER invent an authorship, date, quotation, book title, statistic, or \
  roster and attach a citation to it. If you are unsure whether someone \
  wrote a particular book, say \"I believe\" or \"is often associated \
  with\" rather than asserting authorship.\n\
- Chunks may be cut mid-sentence by the retrieval layer, so layout can \
  fake an attribution: \"Author X\\n\\nBook Y\" is not necessarily \
  claiming X wrote Y — it may be a title heading followed by a \
  continuing sentence. When layout or adjacency leaves who-said-what \
  ambiguous, do not assert that attribution at all — not the chunk's \
  apparent reading, and not one from memory. State who holds a \
  position only when a passage says so in a complete sentence.\n\
- Reproduce names exactly as the passages write them. Never add a \
  first name, initial, title, or era the passages do not supply, and \
  never merge people named in different passages into one claim about \
  what they jointly argued.\n\
- Do not refuse to engage because retrieval was incomplete.\n\
- Do not use [unverified] tags.\n\
- If the passages don't cover it, flag provenance in one line (\"Not in \
  your sources, but from general knowledge…\") then answer from general \
  knowledge — never give a parametric fact as if it were retrieved.\n\
- NEVER invent or complete a list, roster, or statistic you do not fully \
  know.\n\
- CRITICAL — if neither the retrieved passages nor your confident \
  general knowledge cover the specific thing the user asked about, say \
  \"I don't have reliable information on this\" and add NOTHING \
  invented after it. Do NOT invent a plausible-sounding origin, \
  lineage, author, date, organisation, or framework. A \
  confident-sounding fabrication is worse than an honest 'I don't \
  know' — it poisons the user's mental model of what's real. If the \
  phrase the user used (e.g. a specific project name, person, API) is \
  not something you can speak to with concrete factual confidence, say \
  so plainly.\n\
- NEVER END ON A DEAD END. When you come up short — a decline, a \
  partial answer, a thin overview — your LAST line must hand the user \
  agency in ONE short sentence: name the nearest thing these sources \
  CAN answer, or offer to search the web for the missing piece. One \
  sentence, no apology, no lecture.\n\
\n\
CONTESTED SOURCES — sources whose own metadata flags them as \
carrying disputed or competing perspectives.\n\
A source label suffixed `(contested)` means the source has been \
flagged at the section level (e.g. POV-disputed, controversy \
section, opposing-views block) by the corpus's own editorial \
metadata. Treat it as evidence that interpretations differ here:\n\
- Present the strongest argument for each documented view; do not \
  paper over the disagreement with a single tidy synthesis.\n\
- Acknowledge that the source itself flags multiple readings, in \
  one short clause; do not bury the disagreement.\n\
- Do not invent which view is correct or attribute one as canonical \
  unless the source explicitly says so.\n\
This signal is editor-curated, not LLM-classified — trust it.\n\
\n\
CATALOG-AWARE SOURCES — works the system has metadata for but has \
NOT read in detail.\n\
A retrieval block prefixed with `CATALOG:` lists works whose \
metadata (title, author, era, subjects) is indexed but whose full \
text has not been ingested. Treat them as a separate evidence \
tier, distinct from RETRIEVED and PARAMETRIC:\n\
- Use catalog metadata to orient the user about what the work is \
  (author, year, subject area, themes).\n\
- State explicitly that you have not read the full text yet.\n\
- Do NOT invent passages, quotes, plot details, character motivations, \
  thematic readings, or scholarly framings beyond what the catalog \
  metadata supplies. If the user asks for close-reading detail, say \
  you don't have it from a close reading.\n\
- If the catalog hits are clearly relevant to the question, end the \
  reply with a one-sentence ingest offer — name the work and the \
  rough time estimate. Format: \"Want me to read [Title] in depth? \
  It would take about N minutes.\" If multiple are relevant, name \
  the single most central one rather than listing all.\n\
- A catalog hit tagged ALREADY INGESTED → <corpus_id> means the work \
  has already been read on a prior turn — quote and synthesise from \
  the per-work corpus's full-text passages instead of offering to \
  ingest again.";

/// Thinking directive — orients `<think>` toward substantive reasoning.
///
/// Without this, models default to source-adequacy bookkeeping in their
/// thinking blocks ("Source Analysis: [X] — no substantive content...").
/// This directive redirects the thinking budget toward the intellectual
/// content of the question.
pub(crate) const THINKING_DIRECTIVE: &str = "\
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

/// Comparison-shape directive — appended to `KNOWLEDGE_SYNTHESIS_SYSTEM`
/// when the routed intent is `ComparisonQuery`. The shape constraint
/// is what lets the fast slot serve a quality answer: instead of an
/// open-ended essay, the model produces a bounded contrast structured
/// along shared axes. Citation and source-terminology rules from the
/// base prompt still apply.
pub(crate) const COMPARISON_DIRECTIVE: &str = "\
This question asks for a contrast between two or more named things. \
Structure your answer as a bounded comparison along shared axes — \
the dimensions on which the entities differ. For each axis, state \
how each entity stands. 3–5 axes is the target; do not pad with \
unrelated background. Lead with the single sharpest contrast. Keep \
the answer compact: a short paragraph or three bullet points, not \
an essay. Use exact source terminology for technical terms, dates, \
and proper nouns — paraphrase only the connective prose.";

/// How many trailing turns (mixed user + assistant) to include when
/// rendering conversation history into the synthesis system prompt.
/// 8 covers the last 4 (user, assistant) pairs — enough for short
/// follow-up chains without bloating the prompt with stale turns.
pub(crate) const CONV_HISTORY_TURNS: usize = 8;

/// Per-message char budget for the conversation-history block when
/// the caller wants a uniform cap (pre-age-aware behaviour, kept for
/// the few callers that don't have message ordering). New code should
/// prefer `chars_for_message_age` below — recent turns keep more
/// fidelity, older turns compress more aggressively.
pub(crate) const CONV_HISTORY_CHARS_PER_MSG: usize = 500;

/// Age-aware per-message char budget. Walks from the newest visible
/// turn (age = 0) backward — recent turns keep more body so the
/// user's most current exchange stays high-fidelity in the prompt;
/// older turns compress so the cumulative conv-history block
/// doesn't dominate the budget on long chats.
///
/// Tier history (marathon_graceful bench, judge_coverage canonical
/// metric per [[feedback_bench_three_views]]):
///   v1 (2026-05-25, default): 1000 / 600 / 300   judge=0.764
///   v3 trial   (2026-05-26): 1000 / 600 / 500    judge=0.764
///
/// v3 attempted to soften the oldest tier on the hypothesis that
/// Linnaeus-phase callbacks (T16/T19, 9-10 turn gaps) were losing
/// coreference anchors to the 300-char floor. Single-trial bench
/// tied v0 on judge coverage; fact_recall dropped (0.607→0.512)
/// and src_recall rose (0.587→0.611) — both within the ±0.04-0.06
/// trial-to-trial variance band we've observed. No positive signal,
/// reverted to v1 tiers per ARCH §11.1 ("verify before claiming").
///
/// If a future bench acquires multi-trial variance bounds, retry v3
/// — the *mechanism* (Linnaeus content lossy at ages 4-7) is sound;
/// only the single-trial evidence was inconclusive.
pub(crate) fn chars_for_message_age(age: usize) -> usize {
    match age {
        0..=1 => 1000,
        2..=3 => 600,
        _ => 300,
    }
}

/// Minimum number of *dropped* messages (those that would otherwise
/// be invisible to the synthesis prompt) before paying the Fast-slot
/// cost of summarizing them. At 2 dropped messages a single coref
/// span is already at risk; below that the cost-benefit doesn't
/// justify the extra ~1s latency.
pub(crate) const CONV_HISTORY_COMPACT_MIN_DROPPED: usize = 2;

/// Fraction of `effective_context_size` above which the
/// budget-aware compaction arm fires.
///
/// **History (2026-05-25 → 2026-05-26 marathon-graceful bench):**
/// Started at 0.55, then 0.7. Both regressed the judge-coverage
/// metric on the marathon_graceful thread (v0=0.764 →
/// v1@0.55=0.694 → v2@0.7=0.639). Each compaction call generates a
/// fresh Fast-slot summary preamble; on this 21-turn thread the
/// preamble was getting re-summarized so often it lost the
/// nuance the late callback turns (T16–T20) needed to synthesise.
/// Substring fact_recall improved slightly (model used keywords
/// more) but the paraphrase-tolerant judge saw worse coverage —
/// the model's prose got more keyword-dense and less
/// comprehensive.
///
/// Reset to 0.9 = effective "emergency only" threshold. On a 16k
/// ctx slot, 0.9 × 16000 = 14400 tokens of *conversation-history-
/// only* pressure — only reachable on a >50-turn dense chat.
/// `estimate_compaction_pressure` measures conv-history + memories +
/// existing compacted_preamble; system message + retrieval bundle
/// are excluded (they fire later in the handler). The right fix is
/// to redesign the sensor to include all prompt components, then
/// re-tune the threshold — captured as a kind=todo note for the
/// next iteration cycle. For now the budget arm is a safety net
/// that fires on truly pathological pressure; everyday compaction
/// rides the turn-count arm at `CONV_HISTORY_TURNS = 8`.
pub(crate) const COMPACTION_PRESSURE_THRESHOLD: f32 = 0.9;

/// How many newly-dropped messages must accumulate before paying for
/// another fold of the running preamble (2026-07-26 rolling-compaction
/// pass).
///
/// A turn drops 2 messages, so a stride of 6 means the preamble is
/// re-folded roughly every third turn instead of every turn. Between
/// folds the un-folded messages are the NEWEST dropped ones — the two
/// or four that just left the visible window — which are exactly the
/// ones the retrieval-over-history channel is most likely to surface
/// verbatim anyway. So the stride trades a little preamble freshness
/// for a 3× cut in Housekeep calls on long threads, on the material
/// that is least dependent on the preamble.
pub(crate) const CONV_COMPACT_FOLD_STRIDE: usize = 6;

/// Hard cap on how many messages a SINGLE fold call may read.
///
/// This is the bound that makes compaction cost independent of
/// conversation length. Incremental folds sit far below it (stride-sized
/// batches); it binds on the cold fold, where a fresh process meets a
/// conversation that already has hundreds of dropped messages. Over the
/// cap, `context::fold_window` keeps a head and a tail and reports the
/// gap so the preamble is honestly partial rather than quietly so.
///
/// 24 messages × ≤400 chars ≈ 9.6k chars ≈ 2.5k tokens — comfortably
/// inside a Fast slot alongside a ≤120-word prior summary.
pub(crate) const CONV_COMPACT_MAX_FOLD_MSGS: usize = 24;

/// Below this dropped-message count, suppress the narration chip
/// (compaction fires, but silently — chips on ≤2 dropped messages
/// are spam on short chats). The compaction still runs and emits a
/// `debug!` trace; only the user-facing chip is gated.
pub(crate) const COMPACTION_CHIP_MIN_DROPPED: usize = 3;

/// Per-corpus chunk limit for KnowledgeQuery retrieval. Tuned for
/// 1M-2M chunk corpora (Wikipedia L5 scale) where the merged top-K
/// must absorb noise from cross-corpus search without losing the
/// canonical article. See `prepare_knowledge_query_plan` for the
/// budget reasoning — Lance vector search is fast at this K, prompt
/// budget is bounded downstream by `MAX_KNOWLEDGE_CHARS`.
pub(crate) const KQ_PER_CORPUS_LIMIT: usize = 20;

/// Post-merge global cap. Set high enough to support multi-article
/// synthesis (5-7 distinct articles each contributing 2-3 chunks)
/// without truncating the long tail; the prompt formatter trims to
/// `MAX_KNOWLEDGE_CHARS` regardless, so this is the cap that the
/// evidence-shape signals are computed against.
///
/// Sized for the entity-boost flow: standard hybrid search returns
/// `KQ_PER_CORPUS_LIMIT` (20) per corpus, entity boost adds up to
/// `MAX_ENTITY_QUERIES * ENTITY_QUERY_LIMIT` (12) more. At 15 the
/// entity-boost chunks displaced fact-bearing chunks from the main
/// retrieval (v11 regression: +Einstein source but -Christianity,
/// -Mercury perihelion, -mass unemployment because the expanded set
/// was forced through a too-tight cap). 20 absorbs the entity adds
/// without crowding the main retrieval; expander still tops top
/// groups to 4 chunks beyond this. 20 chunks × ~530 chars/chunk +
/// expander = ~13k chars, comfortably under the 16k prompt budget.
pub(crate) const KQ_MERGED_LIMIT: usize = 20;

// ─── Wikipedia link-graph one-hop expansion (Atlas Layer 0) ──

/// One-hop neighbor cap per seed title. Higher values pull in
/// less-relevant neighbors and inflate latency.
pub(crate) const GRAPH_NEIGHBORS_PER_HIT: usize = 5;

/// Per-title chunk pull from each LanceDB corpus when fetching a
/// neighbor's content. Small — enough to seed the article-deepening
/// expansion that already happens downstream.
pub(crate) const GRAPH_NEIGHBOR_LIMIT: usize = 5;

/// Score-decay factor applied to graph-expanded neighbor chunks.
/// At 0.6 a one-hop neighbor of the top hit (parent score 1.0)
/// starts at 0.6 — well below the original top hit but above noise-
/// floor cutoffs. Re-weighting and the cap can promote a neighbor
/// that the query genuinely matches.
pub(crate) const GRAPH_NEIGHBOR_DECAY: f32 = 0.6;

// ─── PPR structural expansion, cross-encoder gated (S4+S3) ───
//
// The walk proposes; the gate disposes. Walk params bound the sqlite
// neighbor queries (frontier × hops calls); gate params bound the
// cross-encoder prefills (candidates × chunks + calibration tail
// pairs per query). RETRIEVAL_REDESIGN.md §8 records the S3 probes
// that fixed this shape: ungated injection loses ~2 facts per
// admitted chunk, so admission must clear the displacement bar.

/// Seeds for the PPR walk: question entities + top in-pool titles.
pub(crate) const PPR_MAX_SEEDS: usize = 5;

/// Push rounds. Two hops reach "article the answer cites" shapes
/// (question entity → linked concept → its own neighbors) without
/// diffusing mass into the whole graph.
pub(crate) const PPR_HOPS: usize = 2;

/// Fraction of a node's mass pushed to neighbors per round; the
/// remainder stays put (standard PPR restart weight, inverted).
pub(crate) const PPR_DAMPING: f64 = 0.85;

/// Articles expanded per push round (top-mass first) — bounds the
/// per-hop neighbor queries.
pub(crate) const PPR_FRONTIER_CAP: usize = 6;

/// Outbound neighbors pulled per expanded article.
pub(crate) const PPR_NEIGHBORS_PER_NODE: usize = 24;

/// Seed-hop adjacency pull. Wide on purpose: the same rows serve the
/// mass push (top slice) AND the typed channel — `causal`/`contested`
/// edges sit at occurrence weight 1-2 in a near-uniform weight
/// distribution (measured: Manhattan Project = 468 of 508 neighbors
/// at w=1), so a narrow pull structurally misses them.
pub(crate) const PPR_SEED_PULL: usize = 512;

/// Cap on structural-channel candidates entering the title prerank
/// (typed causal/contested edges from all seeds + topical bridges
/// from the first two). Wide on purpose: the title-CE prerank is the
/// picker; occurrence-ordered pre-cuts measured as a lottery.
pub(crate) const PPR_TYPED_CAP: usize = 48;

/// Topical (plain-link) bridge candidates per entity seed.
pub(crate) const PPR_TOPICAL_PER_SEED: usize = 8;

/// Mass-channel candidates entering the title prerank.
pub(crate) const PPR_MASS_INTO_PRERANK: usize = 16;

/// Candidate articles that get a chunk fetch + gate score (the
/// title-prerank's output size). Raised 6→10 with the widened
/// funnels (2026-07-17): at 40-60 prerank titles, 6 slots made the
/// PRERANK the admission judge — Einstein/Hitler (chunk-gate-proven
/// admissions) lost seats to new topical candidates. The chunk gate
/// is the calibrated judge; fetches are ~ms (BTree) and gate pairs
/// are prefix-reused.
pub(crate) const PPR_CANDIDATE_ARTICLES: usize = 10;

/// Chunks kept per candidate article after the title filter.
pub(crate) const PPR_CHUNKS_PER_ARTICLE: usize = 2;

/// Whole-article pull cap for the within-article relevance pick
/// (wiki articles chunk to ~10-40 rows; 64 covers the long tail).
pub(crate) const PPR_ARTICLE_FETCH_LIMIT: usize = 64;

// ─── Entity fetch obligations (merge-select architecture) ────

/// FTS pull per entity when resolving its surface form to article
/// titles (title-contains filter applied to the hits).
pub(crate) const OBLIGATION_RESOLVE_LIMIT: usize = 24;

/// Distinct matching titles fetched per entity ("Newton" legitimately
/// resolves to both "Isaac Newton" and "Newton's law of universal
/// gravitation" — both are bank-expected sources).
pub(crate) const OBLIGATION_TITLES_PER_ENTITY: usize = 2;

/// Concept-pass obligations added on top of the `MAX_ENTITY_QUERIES`
/// named-entity budget. Kept small: a question usually turns on one or
/// two named concepts, and every extra concept is an FTS resolve + a
/// wholesale article fetch. The FTS-exact-title gate drops concepts
/// without a canonical article, so this caps cost, not correctness.
pub(crate) const MAX_CONCEPT_QUERIES: usize = 3;

/// Chunks kept per resolved title (top question-token overlap).
pub(crate) const OBLIGATION_CHUNKS_PER_TITLE: usize = 2;

/// Hard cap on injected chunks per query — every admitted chunk
/// displaces a direct hit at the truncate, so this bounds the
/// worst-case fact cost when the gate misjudges.
pub(crate) const PPR_MAX_ADMITTED: usize = 4;

// ─── Question decomposition (opt-in retrieval expansion) ─────

/// Upper bound on sub-queries the decomposer may emit. Higher values
/// inflate latency without lifting the bench in early prototyping.
pub(crate) const DECOMP_MAX_QUERIES: usize = 4;

/// Per-sub-query chunk pull from each corpus. Smaller than
/// [`KQ_PER_CORPUS_LIMIT`] because the merge already has the full
/// bag-of-words query's hits — sub-queries are supplementary depth,
/// not a replacement.
pub(crate) const DECOMP_QUERY_LIMIT: usize = 5;

/// Fast-path output budget. Enough for a focused summary with citations,
/// not enough to invite the model to ramble.
pub(crate) const FAST_KNOWLEDGE_MAX_TOKENS: u32 = 600;

/// Pre-flight budget reminder spliced into the synthesis system message so
/// the model paces itself instead of running out mid-sentence. Pairs with
/// the post-stream length-truncation chip wired in `AssistantMessage` —
/// surface (chip) tells the user when the budget was hit, contract (this
/// hint) tells the model the budget exists in the first place. Without the
/// hint, models routinely open a 5-section essay structure they can't
/// possibly close inside `max_tokens`, producing mid-paragraph cutoffs
/// that LOOK like the model failed when it never knew the limit.
pub(crate) fn build_response_length_directive(max_tokens: usize) -> String {
    // Conservative word estimate: 1 token ≈ 0.75 English words.
    let words = max_tokens.saturating_mul(3).saturating_div(4);
    format!(
        "RESPONSE LENGTH BUDGET\n\
         You have approximately {max_tokens} tokens (~{words} words) for this \
         reply before the response will be cut off mid-sentence. Plan the \
         shape of your answer accordingly. If a complete treatment wouldn't \
         fit in that budget, give a focused, concise version that LANDS \
         within the budget and offer to expand specific sections on request. \
         Do not start a multi-section essay you can't finish — landing the \
         answer beats opening every door.\n\
         If the USER asked for a specific length (a word count, \"a long \
         essay,\" \"exhaustive detail\"), treat it as a ceiling, not a target \
         to hit: answer only as far as the passages support and stop. Never \
         pad, repeat, restate the question, or think out loud to reach a \
         number — a short grounded answer is complete; a long padded one is \
         not."
    )
}

/// How many leading characters of a synthesis stream to hold before deciding
/// whether it opened with a refusal. The model's refusal openings land well
/// inside this window; the cost is ~this-many-chars of first-token latency on
/// the common (non-refusal) path.
pub(crate) const REFUSAL_HEAD_CHARS: usize = 200;

/// Answer-prefill used to force engagement on the retry after a refusal is
/// detected. Prepended as the assistant turn's opening (the model continues
/// from it) AND emitted to the stream so the user sees a coherent answer.
pub(crate) const REFUSAL_RETRY_PREFIX: &str =
    "Here is a thorough answer based on the available sources:\n\n";

/// Forceful, guardrail-stripped system message used ONLY on the refusal-retry.
/// The model refused the first time while citing the elaborate anti-fabrication
/// guardrails in the normal synthesis prompt ("I'm instructed not to
/// fabricate"); on retry we replace that prompt with a short, hard directive
/// that removes the rule it hid behind and commits it to answering. Parsimony
/// is fine here — it's only used on the rare retry path.
pub(crate) const REFUSAL_RETRY_SYSTEM: &str = "\
Relevant source passages have been retrieved for this question. Write a \
thorough, well-structured answer NOW, drawing on the passages and your \
general knowledge. Begin immediately with substantive content (a heading and \
the first section).\n\
\n\
ABSOLUTELY DO NOT: say the task is \"not possible\"; call the passages \
\"insufficient\", \"only fragments\", \"introductory\", or similar; say you \
\"need to clarify\" or \"cannot proceed\"; decline, hedge about what you \
lack, or describe what you can't do.\n\
\n\
The passages plus your general knowledge ARE enough for a substantive, scoped \
answer. Cover what they support in depth; if a sub-topic is thin, cover it \
briefly and note that in ONE closing line — never let it stop you. Use \
[Source: title] when you draw on a passage.";

/// Single source of truth for the knowledge-synthesis system prompt — the
/// **Synthesizer role's** prompt builder (ARCH §3 single-responsibility, §10.3
/// SSOT).
///
/// Collapses the three historical assembly sites — `knowledge_query` Fast
/// (Comparison-or-not, no thinking), `knowledge_query` Primary, and `retrieval`
/// DeepQuery (the latter two: optional thinking) — into ONE deterministic
/// ordering:
///
/// ```text
/// KNOWLEDGE_SYNTHESIS_SYSTEM
///   [ + "\n\n" + COMPARISON_DIRECTIVE ]   (bounded-axes comparison shape)
///   [ + "\n\n" + gap_note ]               (coverage-gap disclosure)
///   [ + "\n\n" + THINKING_DIRECTIVE ]     (hidden-reasoning channel)
///   + "\n\n" + budget_note                (always)
/// ```
///
/// Byte-equivalent to the prior inline assembly at every site (pinned by
/// `synthesis_prompt_tests`, which re-implement the pre-refactor logic and diff
/// it across the full variant matrix). Callers pass only variant inputs:
/// `gap_note`/`budget_note` are the already-built directive strings (empty
/// `gap_note` = none); `comparison`/`include_thinking` are the variant flags.
/// The chosen slot (Fast vs Primary) and the system-message wrapper
/// (`build_system_message` vs `build_primary_system_message`) remain the
/// caller's decision — this fn owns only the prompt *body*.
pub(crate) fn build_synthesis_system_prompt(
    comparison: bool,
    gap_note: &str,
    include_thinking: bool,
    budget_note: &str,
) -> String {
    build_synthesis_system_prompt_with_provenance(
        comparison,
        gap_note,
        include_thinking,
        budget_note,
        false,
    )
}

/// End-positioned restatement of the provenance rule that already lives
/// mid-prompt in `KNOWLEDGE_SYNTHESIS_SYSTEM`. The no-thinking fast
/// slot demonstrably skips the mid-prompt version: 2026-06-10 chaos
/// runs, all 5 out-of-domain answers were content-correct but BARE (no
/// "from general knowledge" flag) once thinking suppression landed —
/// the small model only honoured the rule when its leaked CoT happened
/// to restate it. Recency-positioning a short, hard restatement is the
/// prompt-side fix; an evidence-conditioned structural prefix (caveat
/// injected when retrieval scores are weak) is the durable follow-up if
/// this proves insufficient. SHAPE-level wording only — no
/// fact-category examples, per the no-teaching-to-the-test rule.
/// The canonical provenance caveat, structurally committed via
/// `assistant_prefix` when a turn is topically foreign to every
/// enabled corpus and two retrieval rounds found nothing (see the
/// agentic loop's `question_is_corpus_anchored`). This is the
/// "evidence-conditioned structural prefix" follow-up the directive's
/// doc anticipates — instruction compliance measured ~60% on the fast
/// slot (3/5 OOD caveat omissions on the 2026-06-11 holdout run).
pub(crate) const GK_CAVEAT_PREFIX: &str = "Not in your sources — from general knowledge: ";

pub(crate) const PROVENANCE_DIRECTIVE: &str = "\
FINAL CHECK — provenance (mandatory). If the key fact in your answer does not \
come from the retrieved passages above, your FIRST sentence must say so \
plainly (\"Not in your sources — from general knowledge: …\") and then \
answer. This flag is required even when the fact is famous and you are \
certain of it. If the key fact does come from the passages, cite them with \
[Source: title]. Never present a general-knowledge fact as if it were \
retrieved from the user's sources.";

/// Admits the conversation's own earlier turns as citable evidence.
///
/// The base [`KNOWLEDGE_SYNTHESIS_SYSTEM`] frames the turn's evidence as
/// "retrieved passages from an installed knowledge base". The
/// conversation-history blocks — retrieval-selected earlier turns and the
/// compacted preamble, both rendered into the system message by
/// `build_system_message` — do not read as "passages" under that framing,
/// so the model declines against them: measured 2026-07-25 (gap-probe
/// run), a question whose answer sat at rank 1 in the history-retrieval
/// block got "the available sources do not contain …" even after the
/// grounding gate was unblocked by `conversation_pinned_evidence`
/// (violation_prob 1.0 → 0.0). The gate was the first hop; this is the
/// second.
///
/// Appended to `gap_note` ONLY when that turn actually carries
/// conversation evidence (see `conversation_pinned_evidence`), so the
/// prompt delta is exactly zero on the ordinary corpus-only KQ turn.
///
/// SHAPE-level wording only, per the no-teaching-to-the-test rule: it
/// names the block and the citation form, never a fact category.
pub(crate) const CONVERSATION_EVIDENCE_DIRECTIVE: &str = "\
EARLIER TURNS OF THIS CONVERSATION COUNT AS SOURCES, exactly like the \
passages. When one holds the asked-for fact, lead with that answer and cite \
it [Source: earlier in this conversation] — never reply that the sources \
lack something this conversation already established.";

pub(crate) fn build_synthesis_system_prompt_with_provenance(
    comparison: bool,
    gap_note: &str,
    include_thinking: bool,
    budget_note: &str,
    provenance_emphasis: bool,
) -> String {
    let mut s = String::with_capacity(
        KNOWLEDGE_SYNTHESIS_SYSTEM.len() + gap_note.len() + budget_note.len() + 512,
    );
    s.push_str(KNOWLEDGE_SYNTHESIS_SYSTEM);
    if comparison {
        s.push_str("\n\n");
        s.push_str(COMPARISON_DIRECTIVE);
    }
    if !gap_note.is_empty() {
        s.push_str("\n\n");
        s.push_str(gap_note);
    }
    if include_thinking {
        s.push_str("\n\n");
        s.push_str(THINKING_DIRECTIVE);
    }
    if provenance_emphasis {
        s.push_str("\n\n");
        s.push_str(PROVENANCE_DIRECTIVE);
    }
    s.push_str("\n\n");
    s.push_str(budget_note);
    s
}

#[cfg(test)]
mod synthesis_prompt_tests {
    use super::{
        build_synthesis_system_prompt, COMPARISON_DIRECTIVE, KNOWLEDGE_SYNTHESIS_SYSTEM,
        THINKING_DIRECTIVE,
    };

    const BUDGET: &str = "[budget directive placeholder]";
    const GAP: &str = "What I don't have: 2 files failed to parse.";

    /// Tombstone for the "TRUST YOUR TRAINING" block (ECONOMY §5:
    /// CONTRADICTORY — an instruction to override the evidence,
    /// resolved by the drafter-attribution-discipline order). The
    /// replacement rule must abstain on layout-ambiguous attributions
    /// rather than substitute parametric memory, and must forbid
    /// decorating passage names with un-supplied first names — the
    /// D0-measured minting vector ("Hector-Miguel Mele", "T.L. Haji",
    /// fabrication_etiology_20260814.md specimen 03).
    #[test]
    fn trust_your_training_block_is_resolved_not_present() {
        assert!(
            !KNOWLEDGE_SYNTHESIS_SYSTEM.contains("TRUST YOUR TRAINING"),
            "the contradictory override instruction must not return"
        );
        assert!(
            KNOWLEDGE_SYNTHESIS_SYSTEM.contains("do not assert that attribution at all"),
            "the ambiguity rule must abstain, not substitute"
        );
        assert!(
            KNOWLEDGE_SYNTHESIS_SYSTEM
                .contains("Reproduce names exactly as the passages write them"),
            "surname discipline must be present"
        );
    }

    /// Re-implements the PRE-refactor `knowledge_query` FastFocused assembly
    /// (base[+comparison][+gap]+budget; never any thinking).
    fn legacy_kq_fast(comparison: bool, gap: &str, budget: &str) -> String {
        let base = if comparison {
            format!("{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{COMPARISON_DIRECTIVE}")
        } else {
            KNOWLEDGE_SYNTHESIS_SYSTEM.to_string()
        };
        let base = if gap.is_empty() {
            base
        } else {
            format!("{base}\n\n{gap}")
        };
        format!("{base}\n\n{budget}")
    }

    /// Re-implements the PRE-refactor `knowledge_query` PrimarySynthesis AND
    /// `retrieval` DeepQuery assembly — they were byte-identical
    /// (base[+gap+thinking]+budget; never comparison on these routes).
    fn legacy_primary_or_deep(gap: &str, thinking_on: bool, budget: &str) -> String {
        let thinking = if thinking_on {
            format!("\n\n{THINKING_DIRECTIVE}")
        } else {
            String::new()
        };
        let base = if gap.is_empty() {
            format!("{KNOWLEDGE_SYNTHESIS_SYSTEM}{thinking}")
        } else {
            format!("{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{gap}{thinking}")
        };
        format!("{base}\n\n{budget}")
    }

    #[test]
    fn matches_legacy_kq_fast_all_combos() {
        for &comparison in &[false, true] {
            for &gap in &["", GAP] {
                let got = build_synthesis_system_prompt(comparison, gap, false, BUDGET);
                let want = legacy_kq_fast(comparison, gap, BUDGET);
                assert_eq!(
                    got,
                    want,
                    "KQ-Fast byte mismatch: comparison={comparison} gap_empty={}",
                    gap.is_empty()
                );
            }
        }
    }

    #[test]
    fn matches_legacy_primary_and_deep_all_combos() {
        for &thinking in &[false, true] {
            for &gap in &["", GAP] {
                let got = build_synthesis_system_prompt(false, gap, thinking, BUDGET);
                let want = legacy_primary_or_deep(gap, thinking, BUDGET);
                assert_eq!(
                    got,
                    want,
                    "Primary/Deep byte mismatch: thinking={thinking} gap_empty={}",
                    gap.is_empty()
                );
            }
        }
    }

    #[test]
    fn ordering_is_synth_comparison_gap_thinking_budget() {
        let s = build_synthesis_system_prompt(true, GAP, true, BUDGET);
        let i_comp = s.find(COMPARISON_DIRECTIVE).expect("comparison present");
        let i_gap = s.find(GAP).expect("gap present");
        let i_think = s.find(THINKING_DIRECTIVE).expect("thinking present");
        let i_budget = s.find(BUDGET).expect("budget present");
        assert!(
            s.starts_with(KNOWLEDGE_SYNTHESIS_SYSTEM)
                && i_comp < i_gap
                && i_gap < i_think
                && i_think < i_budget,
            "ordering must be SYNTH < COMPARISON < gap < THINKING < budget"
        );
    }
}

/// Tightly-scoped detector for the model's OWN refusal/deflection openings — a
/// control-flow signal (like a stop-sequence), NOT a content classifier. It
/// triggers a single prefill-retry when the model declines a knowledge turn
/// for which evidence WAS retrieved. Seeded from observed refusals (the Lebanon
/// essay + the chaos tragedy/bombing cases) and unit-tested to NOT fire on
/// genuine answer/essay openings ("I'll write…", "Here is…", "# The History…").
pub(crate) fn looks_like_refusal_opener(head: &str) -> bool {
    let h = head.trim_start().to_lowercase();
    const OPENERS: &[&str] = &[
        "i'm not going to",
        "i am not going to",
        "i need to clarify something important before proceeding",
        "i don't have access to a comprehensive",
        "i don't have access to a thorough",
        "i don't have access to an authoritative",
        "i can't produce the kind of",
        "i cannot produce the kind of",
        "i'm not able to produce",
        "i am not able to produce",
        "i cannot provide a complete",
        "i can't provide a complete",
        "i'm unable to provide a",
        "i am unable to provide a",
        "i need to be honest about what i can",
        "i can and cannot provide",
        "what i can and cannot provide",
        "i need to clarify",
    ];
    if OPENERS.iter().any(|o| h.contains(o)) {
        return true;
    }
    // The "exhaustive ⇒ must fabricate" rationalization, in any phrasing.
    h.contains("would require") && h.contains("fabricat")
}

#[cfg(test)]
mod refusal_opener_tests {
    use super::looks_like_refusal_opener;

    #[test]
    fn fires_on_observed_refusals() {
        // Lebanon-essay + chaos tragedy/bombing refusals.
        assert!(looks_like_refusal_opener(
            "I'm not going to produce the kind of essay you're asking for here."
        ));
        assert!(looks_like_refusal_opener(
            "I need to clarify something important before proceeding.\n\nThe passages…"
        ));
        assert!(looks_like_refusal_opener(
            "I don't have access to a comprehensive, authoritative corpus on the history of Lebanon"
        ));
        assert!(looks_like_refusal_opener(
            "Writing a detailed chronological essay from these fragments would require extensive fabrication"
        ));
    }

    #[test]
    fn does_not_fire_on_genuine_answers() {
        // The good engagement opening (from the natural-phrasing success).
        assert!(!looks_like_refusal_opener(
            "I'll write a detailed, multi-section essay on the history of Lebanon based on the retrieved sources and my knowledge."
        ));
        // The retry prefill itself must not re-trigger.
        assert!(!looks_like_refusal_opener(super::REFUSAL_RETRY_PREFIX));
        // Real essay/prose openings.
        assert!(!looks_like_refusal_opener(
            "# The History of Lebanon: From Ancient Crossroads to Modern Nation-State\n\n## Introduction"
        ));
        assert!(!looks_like_refusal_opener(
            "Lebanon's story is one of extraordinary continuity amid constant transformation."
        ));
        // A legitimate scoped caveat opening must NOT be read as a refusal.
        assert!(!looks_like_refusal_opener(
            "This is a thorough overview based on the available sources, not an exhaustive treatment. Phoenician Lebanon…"
        ));
    }
}

/// When evidence-shape routes FastFocused and a single source dominates,
/// pull up to this many chunks from that source by title (cohesion, not
/// query similarity). Calibrated for an Obsidian note or Wikipedia article
/// — typical long-form sources have 8–15 chunks so 12 captures most
/// without forcing us to truncate narratively.
pub(crate) const EXPANSION_MAX_FROM_TOP_SOURCE: usize = 12;

/// Radius (chunks each side) of the cohesion window pulled around each
/// dominant-source HIT during expansion. The window is anchored on the
/// query-relevant chunks from the initial retrieval, not the document's
/// opening — so a single large document (a whole book under one title) no
/// longer returns its first chunks for every query. 3 each side ≈ a 7-chunk
/// passage per hit; the `EXPANSION_MAX_FROM_TOP_SOURCE` cap still bounds the total.
pub(crate) const EXPANSION_NEIGHBOR_RADIUS: usize = 3;

/// Non-dominant chunks to keep alongside expanded dominant-source chunks,
/// so the model has grounding breadth (e.g. a contradicting viewpoint, a
/// corroborating passage from a different corpus). 2 is enough to signal
/// "other sources exist" without diluting the dominant narrative.
pub(crate) const EXPANSION_GROUNDING_CHUNKS: usize = 2;

/// Maximum proper-noun entities extracted from the question to drive
/// entity-boost retrieval. Each entity gets its own focused hybrid
/// search, results are merged with the main retrieval before reweight.
/// Capped low because each entity costs an embed + per-corpus search
/// (~300-500ms together); 4 covers the typical compare/multi-entity
/// question without blowing the latency budget.
pub(crate) const MAX_ENTITY_QUERIES: usize = 4;

/// Per-entity chunk limit for entity-boost retrieval. Kept small
/// because the entity search is meant to surface the canonical article
/// for the named entity, not its full corpus footprint — depth on
/// entity articles is the multi-source expander's job.
pub(crate) const ENTITY_QUERY_LIMIT: usize = 3;

/// Per-entity chunk limit specifically when intent is ComparisonQuery.
/// Higher than the default — comparison questions guarantee ≥2 named
/// entities being contrasted, and each side needs enough candidates
/// before per-entity merge reservation can pin them. Pairs with
/// `COMPARISON_PER_ENTITY_RESERVE` below.
pub(crate) const COMPARISON_ENTITY_QUERY_LIMIT: usize = 6;

/// Canonical-entity boost — number of chunks fetched from the
/// canonical entity's *primary* corpus. The primary slot is meant to
/// anchor the bench's title-coverage signal: one canonical-overview
/// chunk in the merge bag is enough for a title-match hit, three lets
/// the merge cap reject one without losing the anchor.
pub(crate) const CANONICAL_PRIMARY_LIMIT: usize = 3;

/// For ComparisonQuery, guarantee this many entity-titled chunks per
/// named entity survive the `KQ_MERGED_LIMIT` truncation. Without
/// this, an entity-boost contribution can be out-ranked by the
/// embedded-query results and dropped at merge time — the v20
/// regression on `compare_einstein_newton_gravity` (Newton-side
/// chunks lost despite extraction returning ["Einstein", "Newton"])
/// is exactly this failure mode. 3 per entity × 2 entities = 6 of
/// the 20 merged slots reserved for entity anchors; the other 14
/// stay free for embedded-query / contrast-axis chunks.
pub(crate) const COMPARISON_PER_ENTITY_RESERVE: usize = 3;

/// Multi-source expansion: how many distinct top-ranked (corpus_id, title)
/// groups to expand by title when the question is genuinely
/// multi-article (no single source dominates). Calibrated to the
/// shape of the bank's `multi_article_synthesis` and `causal_reasoning`
/// questions — they typically require pulling depth from 3-4 distinct
/// articles ("Treaty of Versailles" + "Weimar Republic" + "Adolf Hitler"
/// for the Versailles→WWII question, say). Going higher (5-6) starts
/// dragging in tangentially-relevant articles whose title shares a
/// common token but adds noise rather than evidence.
pub(crate) const EXPANSION_MULTI_SOURCE_GROUPS: usize = 4;

/// Per-source chunk fetch limit under multi-source expansion. Smaller
/// than `EXPANSION_MAX_FROM_TOP_SOURCE` (12, single-source case)
/// because here we're fetching from N sources not 1, and the prompt
/// budget caps total chunks at ~14-20. With 4 sources × 4 chunks = 16
/// dominant + 2 grounding = 18 chunks — fits the 8000-char budget
/// after the formatter's per-chunk truncation.
pub(crate) const EXPANSION_MULTI_PER_SOURCE: usize = 4;

/// Wide-fetch pull for the multi-source top-up's question-overlap
/// ranking (whole article; BTree title index makes this ~ms).
pub(crate) const EXPANSION_WIDE_FETCH: usize = 64;

/// Maximum chunks of any one (corpus_id, title) article kept in the
/// merged top-K before expansion runs.
///
/// Hybrid search returns up to `KQ_PER_CORPUS_LIMIT` (20) chunks per
/// corpus; for queries that hit one article densely (e.g. an entire
/// SEP entry on the question's exact topic) the same article can fill
/// 8-12 of those slots and crowd out other articles when we truncate
/// to `KQ_MERGED_LIMIT`. The cap forces breadth across articles.
///
/// 5 is the calibrated value: low enough to break up the worst pile-
/// ups (single articles holding 7-12 of the merged slots, observed
/// for SEP entries on philosophy questions and Wikipedia main-subject
/// articles), high enough not to amputate a genuinely fact-rich
/// dominant article. Earlier value of 3 regressed `synth_industrial_
/// revolution_origins` — the Industrial Revolution article had 6
/// distinct fact-bearing chunks (enclosure, steam engine, colonial),
/// and capping at 3 dropped half of them.
pub(crate) const MAX_CHUNKS_PER_ARTICLE_AT_MERGE: usize = 10;

/// Context budget when source-expansion has fired, and the deep
/// path's static cap.
///
/// Sized for the multi-source expansion path, which is additive on top
/// of the merged top-K: the initial 15 chunks come first, then up to
/// 12 fetched-by-title chunks from the top source documents follow.
/// At the full `text_utils::MAX_CHUNK_CHARS` (2000) per-chunk seat,
/// 27 chunks ≈ 54000 chars; 56000 preserves the v6 lesson (the
/// formatter must reach the appended depth-fetched chunks, not eat
/// the whole budget on the initial top-K). Both call sites (KQ
/// expansion, deep synthesis) run it through the ctx-aware ceiling,
/// which clamps to the slot's real window — e.g. a 16k-ctx slot with
/// 2k output reserve and ~4k system overhead clamps to ~39k chars —
/// so the static figure is the *generous* bound, not a promise.
/// Raised from 16000 on 2026-08-10 together with `MAX_CHUNK_CHARS`
/// (600→2000) and `MAX_KNOWLEDGE_CHARS` (8000→24000); see
/// `MAX_CHUNK_CHARS` for the measurement.
pub(crate) const EXPANDED_KNOWLEDGE_CHARS: usize = 56000;

/// Appended to the synthesis system prompt on `CodeQuery` turns (Inc 4). The
/// evidence on this route is per-symbol SUMMARIES (what each function does, in
/// plain terms) plus CALL-GRAPH TRACES (compiler-resolved callers/callees), so
/// steer the answer to USE them: name the symbols/files, and treat a trace block
/// as the authoritative answer to "what calls X" / "what X calls" rather than
/// guessing from prose. Keeps the base prompt's grounding discipline; this only
/// sharpens the shape for code questions.
pub(crate) const CODE_SYNTHESIS_DIRECTIVE: &str = "\
This is a question about how THIS codebase works. Your evidence is per-symbol \
SUMMARIES (what each function does, in plain terms) and CALL-GRAPH TRACES \
(compiler-resolved). Ground every claim in them and name the specific symbols, \
files, and lines involved.\n\
\n\
When a \"Call-graph trace for `X`\" block is present it is the AUTHORITATIVE \
answer to \"what calls X\" (its callers / entry points) and \"what X calls\" \
(its callees) — read those edges off the trace rather than inferring from prose, \
and cite it [Source: Call-graph trace for `X`]. A `dyn-dispatch` marker is a \
trait / dynamic boundary the call graph followed where a text search could not. \
If the summaries and traces don't cover part of the question, say so in one line \
instead of inventing a call edge or a symbol name.";

#[cfg(test)]
mod prefill_audit {
    //! Cartridge Step-0 spike — exact rendered sizes of the STABLE
    //! prompt layers, for the prefill-economics table (which context
    //! is amortizable into a precomputed/trained KV artifact vs
    //! per-turn dynamic). Run manually:
    //!   cargo test -p sovereign-core --lib prefill_audit -- --ignored --nocapture
    use super::*;

    fn row(name: &str, s: &str) {
        eprintln!("  {name:<44} {:>6} chars  ~{:>5} tok", s.len(), s.len() / 4);
    }

    #[test]
    #[ignore = "manual audit: prints stable-layer prefill sizes"]
    fn stable_layer_sizes() {
        eprintln!("── stable prompt layers (rendered) ─────────────────");
        let budget = build_response_length_directive(2000);
        row("response_length_directive(2000)", &budget);
        row("THINKING_DIRECTIVE", THINKING_DIRECTIVE);
        row("PROVENANCE_DIRECTIVE", PROVENANCE_DIRECTIVE);
        row("CODE_SYNTHESIS_DIRECTIVE", CODE_SYNTHESIS_DIRECTIVE);
        row("GK_CAVEAT_PREFIX", GK_CAVEAT_PREFIX);
        row(
            "today_anchor_block",
            &super::super::text_utils::today_anchor_block("2026-07-12"),
        );
        row(
            "lesson block (K=1, sample prompt_form)",
            &crate::lessons::render_lesson_block("Explain like I'm five."),
        );
        let base_primary = build_synthesis_system_prompt(false, "", true, &budget);
        row("synthesis system (PRIMARY, thinking on)", &base_primary);
        let base_fast =
            build_synthesis_system_prompt_with_provenance(false, "", false, &budget, true);
        row("synthesis system (FAST, provenance)", &base_fast);
        eprintln!("  (dynamic layers — evidence chunks, history, question —");
        eprintln!("   come from live-run journals; see the audit table)");
    }
}
