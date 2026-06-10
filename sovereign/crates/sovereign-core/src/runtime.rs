// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};

use crate::context::{build_context, format_history_as_prompt};
use crate::error::{Error, Result};
use crate::executor::{Executor, TaskContext};
use crate::memory;
use crate::query_session::{SessionStore, SharedSessionStore};
use crate::registry::ToolRegistry;
use crate::skills::{SkillRegister, SkillRegistry};
use crate::traits::{
    ApprovalChannel, InferenceProvider, NoOpRoutingEventSink, Planner, Router, RoutingEventSink,
    StateStore,
};
use crate::types::*;

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

pub use self::voice_prompts::{
    __voice_test_epistemic_contract_for, __voice_test_factual_base_prompt,
    __voice_test_relational_base_prompt, __voice_test_relational_expressive_prompt,
    __voice_test_render_temporal_tensions,
};
pub(crate) use self::voice_prompts::{
    build_witness_grounding, epistemic_contract_for, render_temporal_tensions,
    RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT,
};

mod voice_prompts;

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
Use them together with your general knowledge to answer the question.\n\
\n\
ANSWER, don't deflect. A broad topic the passages and your knowledge \
cover (a history, an overview, an analysis) is ALWAYS answerable: write \
the fullest treatment the material supports, in sections, and note any \
thin spots in one line at the end. If asked for more than the sources \
hold, open with \"Thorough overview from available sources, not \
exhaustive\" and proceed — \"exhaustive / every / complete\" mean be \
thorough, NOT fabricate, and are NEVER a reason to refuse, stall to \
\"clarify first,\" or offer to search. Exception: a specific named fact \
the passages don't contain is \"not in your sources\" — say so plainly, \
don't invent it.\n\
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
- Chunks may be cut mid-sentence by the retrieval layer. If a chunk \
  appears to attribute a book or fact in a way that contradicts or \
  surprises your training knowledge, TRUST YOUR TRAINING on the \
  factual attribution and do not assert the chunk's reading. Example: \
  a chunk that reads \"Author X\\n\\nBook Y\" is not necessarily \
  claiming X wrote Y — it may be a title heading followed by a \
  continuing sentence.\n\
- Do not refuse to engage because retrieval was incomplete.\n\
- Do not use [unverified] tags.\n\
- If the passages don't cover it, flag provenance in one line (\"Not in \
  your sources, but from general knowledge…\") then answer from general \
  knowledge — never give a parametric fact as if it were retrieved.\n\
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
  concrete factual confidence, say so plainly.\n\
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

pub(crate) use self::text_utils::{
    audit_pipeline_stage, format_conversation_history, now, today_anchor_block, truncate_chars,
    truncate_with_ellipsis,
};

mod text_utils;

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

/// Number of top-scoring distinct article titles to seed graph
/// expansion from. Three is enough to catch a handful of relevant
/// neighborhoods without the title-anchored retrieval blowing up
/// cost; tuned for KQ_PER_CORPUS_LIMIT = 20.
pub(crate) const GRAPH_SEEDS_PER_QUERY: usize = 3;

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
         answer beats opening every door."
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
    let mut s = String::with_capacity(
        KNOWLEDGE_SYNTHESIS_SYSTEM.len() + gap_note.len() + budget_note.len() + 256,
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

/// Context budget when source-expansion has fired.
///
/// Sized for the multi-source expansion path, which is additive on top
/// of the merged top-K: the initial 15 chunks (RRF-ranked, fills ~8000
/// chars) come first, then up to 12 fetched-by-title chunks from the
/// top source documents follow. With ~530 chars per chunk after
/// per-chunk truncation, 27 chunks ≈ 14300 chars; 16000 leaves
/// headroom and avoids the v6 failure mode where the formatter ate
/// the initial top-K with the budget and never reached the appended
/// depth-fetched chunks. 16000 chars ≈ 4k prompt tokens, well below
/// gemma-4-E4B's 32k context window after the system prompt and the
/// model's own output budget.
pub(crate) const EXPANDED_KNOWLEDGE_CHARS: usize = 16000;

pub(crate) use self::collaboration::{
    emit_ask_deliberation_chip, run_collaboration, run_post_stream_refinement, ContradictionCheck,
    ASK_MOVE_DELIBERATION_LINGER_MS,
};
pub use self::evidence::build_test_evidence_shape;
pub(crate) use self::evidence::{
    compute_evidence_shape, decide_expansion_strategy, is_grounding_candidate, operation_of,
    resolve_synthesis_route, EvidenceShape, ExpansionStrategy, SynthesisRoute,
    EVIDENCE_MIN_TOKEN_COVERAGE,
};
pub(crate) use self::intent_helpers::{
    build_clarification_question, default_oicp_for_intent, format_interpretation, intent_hint,
    label_for_intent, parse_intent_hint,
};
pub(crate) use self::question_analysis::{
    cap_chunks_per_article, comparison_axis, extract_commitment_phrase,
    extract_comparison_entities, extract_question_entities, parse_metalingual_locator,
    project_retrieved_chunks, raptor_late_inject_enabled, reserve_atom_enum_chunks,
    reserve_raptor_chunks, reserve_chunks_per_entity, MetalingualLocator,
};
pub(crate) use self::retrieval_helpers::{
    atlas_grounding_enabled, build_per_corpus_k_overrides, build_retrieval_query,
    collect_hot_corpora, cross_corpus_sort_cmp, drop_no_overlap_chunks, inject_meta_atlas_hits,
    reweight_by_query_relevance,
};
pub use self::types::{
    ContradictionProv, HistoryEntryProv, HistorySummaryProv, MetaAtlasHitRecord,
    RecalledMemoryProv, StreamHandle, TurnProvenance,
};
pub(crate) use self::types::{KnowledgeContext, KnowledgeQueryPlan};

pub(crate) use self::formatters::{
    build_coverage_gaps_note, build_provenance_components, format_scored_chunks,
    format_scored_chunks_with_kinds, MAX_KNOWLEDGE_CHARS,
};

mod collaboration;
mod evidence;
mod formatters;
mod handlers;
mod intent_helpers;
mod numeric_audit;
mod prompt_budget;
mod question_analysis;
mod retrieval;
mod retrieval_helpers;
/// Public (doc-hidden) so the integration-test harness can drive the
/// runner with mocked steps against a real `Runtime` — the in-crate
/// unit-test route is blocked by the sovereign-store circular dev-dep
/// (two `sovereign_core` identities). Not a supported external API.
#[doc(hidden)]
pub mod retrieval_pipeline;
mod system_message;
mod types;

pub(crate) use self::retrieval_pipeline::{deep_pipeline, kq_pipeline, PipelineState};

pub struct Runtime {
    pub inference: Arc<dyn InferenceProvider>,
    pub router: Box<dyn Router>,
    pub planner: Box<dyn Planner>,
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
    pub skills: Arc<SkillRegistry>,
    pub approval: Arc<dyn ApprovalChannel>,
    pub inference_config: InferenceConfig,
    /// Per-conversation record of the last turn's REAL assembled
    /// prompt sizes, written by the prompt-budget guard at the two
    /// request-construction sites. Phase 2 of the budget-sensor
    /// redesign: `estimate_compaction_pressure` uses it as a floor
    /// (its component estimate sees ~⅓ of the prompt), and the
    /// Phase-3 allocator derives next-turn knowledge/history budgets
    /// from it. Bounded (cleared past 512 conversations); never
    /// persisted — a fresh process re-learns within one turn.
    pub(crate) assembly_memo:
        std::sync::RwLock<std::collections::HashMap<String, prompt_budget::MeasuredAssembly>>,
    pub corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
    /// Optional structural link graph for a corpus that exposes one
    /// (today: Wikipedia, via metadata `outgoing_links` /
    /// `pov_count` / `section_path`). Populated by the bootstrap
    /// when a `wikipedia_graph.db` is found alongside the corpus's
    /// LanceDB table. When present, the retrieval path can opt into
    /// one-hop neighbor expansion (env-gated) and surfaces
    /// `(contested)` markers on chunks whose source has at least
    /// one editor-flagged contested section. `None` preserves the
    /// pre-graph behaviour.
    pub wikipedia_graph: Option<Arc<corpus_engine::WikipediaGraph>>,
    /// Optional note store. Populated by the daemon bootstrap; absent
    /// in the chat-CLI path where commitment persistence isn't wired.
    /// Consumed by `handle_commissive_query` to write `kind="commitment"`
    /// and `kind="todo"` notes anchored to `working_memory.current_goal`
    /// (or honestly anchorless when no situated goal is loaded).
    pub note_store: Option<Arc<corpus_engine_notes::NoteStore>>,
    /// Optional rolling-summary compaction worker. When present,
    /// `end_conversation` notifies it after writing extracted
    /// memories so a conversation that crossed the threshold gets
    /// its oldest memories folded into a `MemoryKind::Summary` in
    /// the background. `None` preserves the pre-2026-05-23
    /// uncompacted behaviour exactly.
    pub compaction: Option<Arc<crate::memory_compaction::CompactionWorker>>,
    /// Read-side handle for conversation tiered-retrieval enrichment
    /// (`conv_skeletons` / `conv_raptor_nodes` / `conv_motifs`). Spec
    /// `sovereign/docs/specs/CONV_TIERED_PORT.md`. When present, the
    /// prompt-assembly path renders per-conversation briefings ahead
    /// of the raw chunk block via
    /// [`crate::conv_briefing::build_conv_tiered_briefings`].
    /// `None` preserves the pre-tiered behaviour exactly — the model
    /// gets only the standard `format_scored_chunks_with_kinds`
    /// output for conv corpora.
    pub conv_tiered_reader: Option<Arc<dyn crate::conv_tiered::ConvTieredReader>>,
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
    /// Source of pre-embedded atlas Entity contexts, looked up at
    /// query time and fused into chunk-retrieval results as virtual
    /// `ScoredChunk`s. The daemon's `AtlasContextManager` populates
    /// this once at boot per installed corpus that has an `atlas/`
    /// dir. `None` = atlas-grounded retrieval is off (the pre-atlas
    /// chunk-only behaviour is preserved exactly).
    pub atlas_context_provider: Option<Arc<dyn crate::atlas_context::AtlasContextProvider>>,
    /// Reports which `corpus_id`s are flagged sensitive (e.g.
    /// folder-ingest v1 §3.4 watched-folder sensitivity). Consulted
    /// by [`Runtime::search_corpus_indexes`] before fanning out
    /// retrieval — sensitive corpora are dropped from the
    /// ambient-retrieval candidate set so they never contribute to
    /// pre-turn situated context.
    ///
    /// `None` = no sensitivity gate applied (all corpora eligible),
    /// which matches the pre-v1 behaviour exactly. The bootstrap
    /// wires sovereign-tools' `LocalCorpusManager` here.
    pub sensitive_corpora: Option<Arc<dyn crate::traits::SensitiveCorpusOracle>>,
    /// Per-folder metadata oracle. Folder-ingest v1 §6.3 — when
    /// retrieval pulls chunks from a watched-folder corpus, this
    /// provides the user-typed display name and the "what I don't
    /// have" gap counters so the synthesis prompt can say "your
    /// case-files folder" and surface skipped/failed-file notes.
    /// `None` = no folder corpora known (CLI fallback / tests),
    /// which preserves the pre-Phase-F label rendering exactly.
    pub folder_metadata: Option<Arc<dyn crate::traits::FolderMetadataOracle>>,
    /// Optional cross-encoder reranker. When `Some`, every call to
    /// `search_corpus_indexes` (and its filtered companion) hits
    /// `CorpusIndex::search_with_rerank` instead of `search`; the
    /// hybrid result gets re-ordered by a model trained to score
    /// (query, doc) relevance directly. `None` preserves baseline
    /// fusion-only behaviour exactly.
    ///
    /// Bootstrapped from `SOVEREIGN_RERANK=1` (or wired explicitly
    /// by the daemon when models.toml carries a `[rerank]` slot).
    pub rerank_fn: Option<corpus_engine::RerankFn>,
    /// Configuration for the rerank pass — overfetch size, optional
    /// threshold. Always present; `enabled = false` makes
    /// `search_with_rerank` no-op back to baseline regardless of
    /// `rerank_fn`'s presence.
    pub rerank_config: corpus_engine::RerankConfig,
    /// Cross-corpus meta-atlas index (Move 5). Built at bootstrap
    /// from `~/.sovereign/meta-atlas/canonical_atoms.json` (produced
    /// by `sovereign meta-atlas build`). The chat-path boost pass
    /// `Self::meta_atlas_boost` consults the index on every
    /// knowledge-query turn to surface stream-tagged anchors per
    /// question entity. `None` (or empty index) = no boost; retrieval
    /// falls back to cosine + entity-boost search exactly as before.
    pub meta_atlas: Option<Arc<corpus_engine::meta_atlas::MetaAtlasIndex>>,
    /// Per-conversation last-turn provenance snapshot, written at
    /// dispatch inside [`Self::handle_expressive_query_stream`] and
    /// read by [`Self::get_last_turn_provenance`]. Last-write-wins
    /// per `conversation_id`; not persisted across restarts.
    ///
    /// The desktop's inner-work surface pulls this via a Tauri
    /// command bound to Cmd+? to surface "what did the model
    /// actually see on the most recent witness turn." Capture is
    /// scoped to the streaming witness path because that's where
    /// the bad-response signal originates; if the non-streaming
    /// path needs the same surface later, mirror the capture in
    /// `handle_expressive_query`.
    pub turn_provenance: Arc<std::sync::RwLock<HashMap<String, TurnProvenance>>>,
    /// Optional GLiNER entity extractor. Wired by the CLI/daemon bootstrap
    /// when the gliner_small-v2.1 ONNX model is installed. Used by
    /// `maybe_retrieve_relevant_history` for entity-aware query
    /// enrichment + hybrid cosine/jaccard scoring. `None` = pre-GLiNER
    /// behaviour preserved (pure cosine + MMR).
    pub gliner: Option<Arc<dyn crate::traits::EntityExtractor>>,
}

impl Runtime {
    /// Resolve the active-mode skill id for a conversation.
    ///
    /// Single source of truth post-2026-05-24 architecture redesign:
    /// the conversation's `skill_id` column (set at create-time by
    /// the surface that owns it) drives routing. Registry state is
    /// no longer consulted for workspace skills — that was the
    /// brittle lifecycle-glue path where every surface enter/leave
    /// triggered `rebuild_runtime` (~15s) plus a race-prone
    /// activate/deactivate dance across mount/destroy hooks.
    ///
    /// Validation: the tag is silently dropped when (a) the skill
    /// id isn't registered (skill removed since the conversation
    /// was tagged), or (b) the skill exists but is `Background`
    /// kind (frontend bug — backgrounds aren't surface skills).
    /// Both fall through to default-chat routing rather than
    /// crashing.
    ///
    /// Returns `None` for untagged conversations (default chat).
    pub(crate) async fn resolve_active_mode(&self, conversation_id: &str) -> Option<String> {
        let conv = self.store.get_conversation(conversation_id).await.ok()?;
        let tag = conv.skill_id?;
        let skill = self.skills.skill_by_id(&tag)?;
        if skill.activation_kind != crate::skills::ActivationKind::Workspace {
            tracing::debug!(
                conversation_id,
                skill_id = %tag,
                "resolve_active_mode: conversation tagged with non-workspace \
                 skill; falling through to default routing"
            );
            return None;
        }
        Some(tag)
    }

    /// Record a turn's measured assembly for this conversation —
    /// Phase 2 of the budget-sensor redesign. Bounded: clears the map
    /// past 512 conversations (per-process working set is far below
    /// this; the memo re-learns within one turn).
    pub(crate) fn record_assembly(
        &self,
        conversation_id: &str,
        measured: prompt_budget::MeasuredAssembly,
    ) {
        let mut memo = self
            .assembly_memo
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if memo.len() >= 512 && !memo.contains_key(conversation_id) {
            memo.clear();
        }
        memo.insert(conversation_id.to_string(), measured);
    }

    /// Last turn's real assembled sizes for this conversation, if the
    /// budget guard has run on it this process lifetime.
    pub(crate) fn last_assembly(
        &self,
        conversation_id: &str,
    ) -> Option<prompt_budget::MeasuredAssembly> {
        self.assembly_memo
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(conversation_id)
            .copied()
    }

    /// Phase-3 allocation for this conversation's NEXT assembly,
    /// derived from the previous turn's measured demand.
    pub(crate) fn allocation_for(&self, conversation_id: &str) -> prompt_budget::Allocation {
        prompt_budget::allocate(self.last_assembly(conversation_id).as_ref())
    }

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
            wikipedia_graph: None,
            note_store: None,
            assembly_memo: std::sync::RwLock::new(std::collections::HashMap::new()),
            compaction: None,
            conv_tiered_reader: None,
            mesh_knowledge: None,
            landscape_digests: None,
            sessions: Arc::new(SessionStore::new()),
            confidence_thresholds: ConfidenceThresholds::default(),
            routing_events: Arc::new(NoOpRoutingEventSink),
            atlas_context_provider: None,
            sensitive_corpora: None,
            folder_metadata: None,
            rerank_fn: None,
            rerank_config: corpus_engine::RerankConfig::default(),
            meta_atlas: None,
            turn_provenance: Arc::new(std::sync::RwLock::new(HashMap::new())),
            gliner: None,
        }
    }

    /// Install a GLiNER entity extractor for entity-aware retrieval
    /// over conversation history. Used by
    /// `maybe_retrieve_relevant_history` to compute a hybrid
    /// cosine/jaccard score: 0.6·cosine(query, pair) +
    /// 0.4·jaccard(query_entities, pair_entities). When `None`,
    /// retrieval falls back to pure cosine + MMR (pre-GLiNER
    /// behaviour preserved).
    pub fn with_gliner(mut self, gliner: Arc<dyn crate::traits::EntityExtractor>) -> Self {
        self.gliner = Some(gliner);
        self
    }

    /// Install a cross-encoder reranker. Pure-additive: when enabled,
    /// every corpus search overfetches `config.candidates_k` candidates
    /// from the hybrid fusion path, scores them with `fn`, sorts by
    /// rerank score, and truncates to the caller's limit. When `fn`
    /// errors at runtime, the search-side fallback preserves baseline
    /// fusion ordering — enabling the reranker can never make retrieval
    /// worse than without it.
    pub fn with_rerank(
        mut self,
        rerank_fn: corpus_engine::RerankFn,
        config: corpus_engine::RerankConfig,
    ) -> Self {
        self.rerank_fn = Some(rerank_fn);
        self.rerank_config = config;
        self
    }

    /// Install rerank *config* without a reranker function. Used by
    /// the per-article-dedup-only ablation: overfetch + dedup using
    /// fusion scores only, no cross-encoder calls. Validates whether
    /// the SEP source-recall lift attributed to the reranker
    /// experiment is actually driven by dedup or by the
    /// cross-encoder logits.
    pub fn with_rerank_config(mut self, config: corpus_engine::RerankConfig) -> Self {
        self.rerank_fn = None;
        self.rerank_config = config;
        self
    }

    /// Fetch the most recent witness-turn provenance for `conversation_id`,
    /// if any. Returns `None` when no provenance has been captured for
    /// that conversation in this Runtime's lifetime (e.g. a fresh
    /// daemon, a non-relational classification, or a conversation that
    /// only ran on the non-streaming witness path).
    pub fn get_last_turn_provenance(&self, conversation_id: &str) -> Option<TurnProvenance> {
        let guard = self.turn_provenance.read().ok()?;
        guard.get(conversation_id).cloned()
    }

    /// Test-only knob: replace the default `SessionStore` so a
    /// suite can drive the runtime with a relaxed narration gate
    /// (e.g. `Duration::ZERO` so an instant stubbed turn still
    /// emits its `NarrationPhase` events). Production callers
    /// inherit the `NARRATION_MIN_ELAPSED` const default from
    /// [`SessionStore::new`].
    pub fn with_session_store(mut self, sessions: SharedSessionStore) -> Self {
        self.sessions = sessions;
        self
    }

    /// Install a `RoutingEventSink` to receive interpretation,
    /// clarification, and narration events. The desktop bootstrap
    /// calls this with a `TauriRoutingEventSink`; headless harnesses
    /// inherit the `NoOpRoutingEventSink` default from `new`.
    pub fn with_routing_events(mut self, sink: Arc<dyn RoutingEventSink>) -> Self {
        self.routing_events = sink;
        self
    }

    pub fn with_corpus_engine(mut self, engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        self.corpus_engine = Some(engine);
        self
    }

    /// Install the cross-corpus meta-atlas index. Built by the
    /// bootstrap by loading `~/.sovereign/meta-atlas/canonical_atoms.json`
    /// (produced by `sovereign meta-atlas build`). Optional — when
    /// `None`, [`Self::meta_atlas_boost`] short-circuits and retrieval
    /// behaves exactly as before the meta-atlas substrate landed.
    pub fn with_meta_atlas(
        mut self,
        index: Arc<corpus_engine::meta_atlas::MetaAtlasIndex>,
    ) -> Self {
        self.meta_atlas = Some(index);
        self
    }

    /// Install a source of pre-embedded atlas Entity contexts.
    /// Usually `sovereign-tools::AtlasContextManager` constructed by
    /// the daemon bootstrap; the eval CLI builds inline contexts and
    /// can call this with a one-shot provider for symmetry.
    pub fn with_atlas_context_provider(
        mut self,
        provider: Arc<dyn crate::atlas_context::AtlasContextProvider>,
    ) -> Self {
        self.atlas_context_provider = Some(provider);
        self
    }

    /// Install a structural link graph. The bootstrap does this
    /// when a graph DB is found alongside a corpus's LanceDB table;
    /// callers that don't wire one (e.g. tests, code-corpus chat)
    /// leave it `None` and retrieval behaves exactly as before.
    pub fn with_wikipedia_graph(mut self, graph: Arc<corpus_engine::WikipediaGraph>) -> Self {
        self.wikipedia_graph = Some(graph);
        self
    }

    /// Install a note store for commitment persistence. Daemon bootstrap
    /// wires this; CLI eval path leaves it `None`, in which case the
    /// commissive handler degrades to a clear "no notes store wired"
    /// reply rather than dropping the commitment silently.
    pub fn with_note_store(mut self, store: Arc<corpus_engine_notes::NoteStore>) -> Self {
        self.note_store = Some(store);
        self
    }

    /// Install the rolling-summary compaction worker. The daemon
    /// bootstrap constructs the worker via
    /// [`crate::memory_compaction::CompactionWorker::spawn`] (which
    /// starts the background drain task) and hands the resulting
    /// `Arc` here. The CLI eval path leaves `None`; `end_conversation`
    /// then skips the enqueue and the pre-compaction shape is
    /// preserved exactly.
    pub fn with_compaction(
        mut self,
        worker: Arc<crate::memory_compaction::CompactionWorker>,
    ) -> Self {
        self.compaction = Some(worker);
        self
    }

    /// Install the conversation tiered-retrieval reader so the
    /// prompt-assembly path surfaces per-conv briefings + signposts
    /// alongside the raw chunk block. The daemon wires this with the
    /// same `Arc<SqliteStateStore>` it hands to the
    /// `ConvTieredProvider` writer — one store, two views.
    pub fn with_conv_tiered_reader(
        mut self,
        reader: Arc<dyn crate::conv_tiered::ConvTieredReader>,
    ) -> Self {
        self.conv_tiered_reader = Some(reader);
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

    /// Install a sensitive-corpus oracle (folder-ingest v1 §3.4).
    /// When wired, [`Runtime::search_corpus_indexes`] consults the
    /// oracle for each ambient retrieval and drops any corpus the
    /// oracle reports as sensitive *before* fanning out the search.
    /// Leaving this `None` preserves the pre-v1 behaviour exactly
    /// (no corpus is treated as sensitive).
    ///
    /// Per ARCH §7.4 (defence in depth), this is the runtime-side
    /// layer of enforcement — sovereign-tools' `WatchedFolderConfig`
    /// holds the flag, the on-disk state mirrors it, and the
    /// runtime applies the structural exclusion at the assembly
    /// seam. A failure at any single layer doesn't compromise the
    /// invariant because the other layers still apply.
    pub fn with_sensitive_corpora(
        mut self,
        oracle: Arc<dyn crate::traits::SensitiveCorpusOracle>,
    ) -> Self {
        self.sensitive_corpora = Some(oracle);
        self
    }

    /// Install the per-folder metadata oracle (Folder-ingest v1
    /// §6.3 source attribution + coverage). The runtime uses the
    /// snapshot to (a) replace `corpus_id`-as-label with the user's
    /// typed display name in the prompt's `[Source: …]` headers
    /// and (b) surface a "what I don't have" line when matched
    /// folders carry many failed/skipped files.
    ///
    /// `None` (the default) preserves the pre-Phase-F behaviour
    /// exactly, so test harnesses and the bare CLI path don't have
    /// to wire sovereign-tools' `LocalCorpusManager` to keep
    /// running.
    pub fn with_folder_metadata(
        mut self,
        oracle: Arc<dyn crate::traits::FolderMetadataOracle>,
    ) -> Self {
        self.folder_metadata = Some(oracle);
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

        tracing::info!(
            count = extracted.len(),
            "memory: extracted long-term memories"
        );
        // Read the conversation's skill_id once before the loop. The
        // tag is denormalized onto each extracted memory so the
        // recall layer can wall scoped pools (e.g. inner-work) at the
        // SQL level without a join. `None` here means "general pool"
        // — the conversation predates the skill-tagging migration or
        // ran outside any skill.
        let source_skill_id = context.conversation.skill_id.clone();
        for mut mem in extracted {
            // Tag each extracted memory with the conversation it
            // came from. Enables the `personal-knowledge`
            // KnowledgeView to surface cluster membership
            // alongside conversation-level metadata (title, skill)
            // at digest time, and makes `memories.source_conversation_id`
            // no longer NULL on fresh writes post-migration.
            mem.source_conversation_id = Some(conversation_id.to_string());
            mem.source_skill_id = source_skill_id.clone();
            memory::save_with_contradiction_check(
                self.inference.as_ref(),
                self.store.as_ref(),
                mem,
            )
            .await?;
        }

        // Save-time hook for rolling-summary compaction. Fire-and-
        // forget — the worker re-checks the threshold before doing
        // real work, so over-enqueuing is harmless. Pre-2026-05-23
        // path (no worker wired) skips the notification.
        if let Some(worker) = &self.compaction {
            worker.maybe_enqueue(conversation_id);
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
            rationale: Some(format!("session continuation from {}", &resume.session_id)),
            coarse_intent: Some("CONTINUATION".to_string()),
            self_assessment: None,
            timing: None,
            scope: None,
        };
        self.handle_message_stream_with_classification(message, conversation_id, Some(synthetic))
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
            timing: None,
            scope: None,
        };
        let message = session.input.clone();
        let conversation_id = session.conversation_id.clone();
        drop(session);
        self.handle_message_stream_with_classification(&message, &conversation_id, Some(synthetic))
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

        // 1a. Embedding-based memory recall on relational/witness paths.
        // Mirrors the non-streaming path (see `handle_turn`). FTS
        // keyword recall misses concrete-event seed memories on
        // abstract self-referential queries (hard-mode H05).
        //
        // Scope-aware: the recall is walled by the conversation's
        // skill_id so an inner-work conversation only surfaces
        // inner-work memories, and a general conversation never sees
        // them. See `MemoryScope` for the invariant.
        if context.turn_register() == SkillRegister::Relational {
            let scope = crate::traits::MemoryScope::from_conversation_skill(
                context.conversation.skill_id.as_deref(),
            );
            match memory::recall_relevant_memories_embed(
                self.inference.as_ref(),
                self.store.as_ref(),
                &scope,
                message,
                5,
            )
            .await
            {
                Ok(top) if !top.is_empty() => {
                    tracing::debug!(
                        before = context.memories.len(),
                        after = top.len(),
                        "runtime: stream memories overridden via embedding recall"
                    );
                    context.memories = top;
                }
                _ => {}
            }
        }

        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 1b. Update topic context for turn-aware routing. The
        //     incoming user `message` is passed in so the extractor
        //     can detect a pivot off the prior arc — otherwise the
        //     topic stays anchored to the last assistant turn and a
        //     learner question that shifts subject ("Why didn't
        //     relativity win the Nobel?" after a photoelectric chain)
        //     keeps the stale topic, dragging retrieval off course.
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
            Some(message),
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

        // 2a. Compact dropped history. Once the conversation exceeds
        //     the visible window OR crosses the budget-pressure
        //     threshold (added 2026-05-25), the synthesis prompt
        //     would drop the oldest turns silently — coreference and
        //     topic anchors established in T0/T1 would vanish from
        //     view at T10+. Fast-slot summary preserves them as a
        //     compact preamble. Surfaced by
        //     sovereign/bench/wikipedia_learn 2026-05-17 marathon
        //     thread + the upcoming marathon_graceful bench.
        //
        //     session_id is None on this code path because
        //     `self.sessions.begin` doesn't run until further down —
        //     the narration chip is gated behind Some, so compaction
        //     still fires (and traces) but the user sees no chip on
        //     this entry point. The chip surface fires from the
        //     handler-level paths that have a session in scope.
        self.maybe_compact_dropped_history(&mut context, conversation_id, None)
            .await;

        // 2a.5. Retrieval-over-history spike (2026-05-26). Gated on
        //       SOVEREIGN_HISTORY_RETRIEVAL=1. Embeds prior turn pairs
        //       OUTSIDE the visible window, picks top-K cosine-near
        //       the current user message, stashes hits on the context
        //       for the renderer. Mechanism A/B vs the lossy-summary
        //       compaction arm — see `maybe_retrieve_relevant_history`.
        self.maybe_retrieve_relevant_history(&mut context, message)
            .await;

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
        //
        // Pre-classification narrowing: mode-only. The router sees
        // the broadest catalog the surface admits so classification
        // isn't artificially constrained by an as-yet-unknown
        // intent. Handlers downstream can re-narrow via
        // `narrow_tools_for_intent` once the intent is in hand.
        //
        // Resolve `active_mode` from the conversation tag BEFORE the
        // narrow so workspace-tagged conversations (recipe-author
        // being the load-bearing case) see their narrowed catalog
        // at classification time. The registry-side lookup inside
        // the plain `narrow_tools_pre_classification` misses the
        // conv-tag path; calling `_for_mode` with the resolved tag
        // is what prevents the router from picking generic tools
        // (e.g. `shell`) on a recipe-author turn. See decision note
        // 2026-05-23 for the silent-misroute history.
        let early_active_mode = self.resolve_active_mode(conversation_id).await;
        let tool_descriptors =
            self.narrow_tools_pre_classification_for_mode(early_active_mode.as_deref());
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
        // The cancel token is LIVE plumbing, not bookkeeping: the
        // desktop's `cancel_stream` (and `redirect_turn`) cancels it,
        // and the streaming forward loops below select! on it —
        // terminating the turn with FinishReason::Cancelled and
        // dropping the provider stream (the embedded engine stops
        // decoding on receiver-drop). Before 2026-06-10 this binding
        // was discarded (`_cancel_token`) and cancel was a no-op:
        // "cancelled" turns ran to natural completion (harness note
        // df66cb8d).
        let (_session_id, cancel_token) = self.sessions.begin(
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
        //
        // Build the per-turn IntentPolicy and stash it on context
        // so downstream consumers read register/effective_intent
        // from one source of truth rather than re-querying
        // `SkillRegistry::primary_skill_register()` at ~16 sites.
        // The witness-intent override is now folded into
        // `intent_policy::policy_for`; the effective intent we
        // dispatch on is `policy.effective_intent`.
        let raw_intent = classification.primary.intent.clone();
        // Conversation-driven active mode was resolved early (above)
        // so the pre-classification narrow could consult it. Re-use
        // that resolution here rather than paying the
        // store.get_conversation round-trip a second time.
        let active_mode = early_active_mode.clone();
        let declared_register = active_mode
            .as_deref()
            .and_then(|id| self.skills.skill_by_id(id))
            .map(|s| s.inference.register)
            .unwrap_or_default();
        let intent_policy = crate::intent_policy::policy_for(
            &raw_intent,
            declared_register,
            active_mode.as_deref(),
        );
        let intent = intent_policy
            .effective_intent
            .clone()
            .unwrap_or_else(|| raw_intent.clone());
        context.intent_policy = Some(intent_policy);
        let coarse_intent = classification.coarse_intent.clone();
        let self_assessment = classification.self_assessment.clone();
        let scope = classification.scope.clone();

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            active_mode = ?active_mode,
            tier = ?policy.tier,
            "runtime: stream routed"
        );

        // Recipe-author workspace dispatch. When the conversation is
        // tagged with `recipe-author`, every meaningful turn is a
        // long-lived tool-using loop (draft → validate → test →
        // checkpoint) — wrong shape for the generic ComplexTask
        // planner that follows below, and the streaming ComplexTask
        // path was returning NotImplemented anyway (desktop sat in
        // loading state forever). Route to the agent-loop handler
        // here, BEFORE the ComplexTask bailout. The narrowed
        // `tool_descriptors` (from the pre-classification narrow
        // above with `early_active_mode`) already carries the
        // recipe-author tool catalog. See handlers/recipe_author.rs
        // for the loop shape and the 2026-05-23 history note.
        if active_mode.as_deref() == Some(crate::intent_policy::MODE_RECIPE_AUTHOR) {
            tracing::info!(
                intent = ?intent,
                "runtime: dispatching recipe-author workspace turn to agent loop"
            );
            return self
                .handle_recipe_author_turn_stream(
                    message,
                    conversation_id,
                    &context,
                    &tool_descriptors,
                )
                .await;
        }

        // PR2 — Ask move. Suppress synthesis entirely, emit a
        // `clarification-request` event, save a placeholder assistant
        // message with the clarification metadata so the UI's
        // existing message-metadata listener can render the
        // ClarificationCard (same delivery path as retrieved_chunks).
        // Return an already-closed stream so the desktop relay exits
        // its token loop and promptly fires `message-complete`.
        if matches!(policy.move_kind, MoveKind::Ask) {
            return self
                .handle_ask_move_stream(message, conversation_id, &_session_id, &classification)
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

        // ── Team-pipeline gate (Phase 4 of the situated-team plan) ──
        //
        // When `SOVEREIGN_TEAM_PIPELINE` is on AND the intent is one
        // the orchestrator handles end-to-end, route this turn through
        // `pipeline::run_team_pipeline` instead of the legacy
        // intent-specific dispatch below. Read the env var per-turn
        // (not at boot) so flipping it on a running daemon takes
        // effect immediately. Default-off until T2 bench validates
        // a default-on flip — see `pipeline::runner` for the
        // rationale and the constant to flip.
        //
        // Conation / Commissive / Expressive / ComplexTask /
        // MetalingualQuery keep the legacy path even when the
        // gate is on; their handlers depend on situated-skill
        // wiring that v1 of the orchestrator doesn't replicate.
        // Tool-calls and OICP/mesh peer routing reach `Runtime`
        // through different entry points and never hit this
        // branch (per plan §4.3).
        if crate::pipeline::is_team_pipeline_enabled()
            && matches!(
                intent,
                Intent::SimpleQuery
                    | Intent::DeepQuery
                    | Intent::KnowledgeQuery
                    | Intent::ComparisonQuery
                    | Intent::ExpressiveQuery
            )
        {
            tracing::info!(
                intent = ?intent,
                "team-pipeline: kill-switch enabled — routing turn through orchestrator"
            );
            let candidates = self.retrieve_candidates(message, &context, &intent).await;
            let register = context.turn_register();
            let witness_grounding = build_witness_grounding(&context, register);
            let inputs = crate::pipeline::TeamPipelineInputs {
                provider: Arc::clone(&self.inference),
                message,
                classification: &classification,
                register,
                candidates,
                max_tokens: crate::pipeline::DEFAULT_TEAM_PIPELINE_MAX_TOKENS,
                judge_enabled: true,
                witness_grounding,
            };
            let sink: Arc<dyn crate::pipeline::NarrationSink> =
                Arc::new(crate::pipeline::RoutingEventNarrationSink {
                    inner: Arc::clone(&self.routing_events),
                });
            let output = crate::pipeline::run_team_pipeline(
                inputs,
                sink,
                _session_id.clone(),
                conversation_id.to_string(),
            )
            .await?;
            let message_id = uuid::Uuid::new_v4().to_string();
            return Ok(StreamHandle {
                message_id,
                stream: output.stream,
            });
        }

        // Document attached or ComplexTask → fall back to non-streaming.
        // (KnowledgeQuery used to live here too, but that triggered a desktop
        // fallback that re-ran build_context + compress_working_memory +
        // update_topic_context + classify — ~17 seconds of pure duplicated
        // work. Instead we now run KnowledgeQuery inline below and emit the
        // response as a single stream chunk.)
        // ExpressiveQuery now has a streaming variant — see
        // `handle_expressive_query_stream`. Dispatch to it directly
        // before the document-fallback gate; the witness path
        // (Pass A → strip-thinking-stream → cleaned tokens) replaces
        // the prior NotImplemented + non-streaming-fallback dance.
        if matches!(intent, Intent::ExpressiveQuery) {
            tracing::info!(
                intent = ?intent,
                register = ?context.turn_register(),
                "runtime: dispatching ExpressiveQuery to streaming witness"
            );
            return self
                .handle_expressive_query_stream(message, conversation_id, &context)
                .await;
        }

        // Document-attached turns are owned by the document-operation path and
        // never reach the streaming surface for synthesis — keep the explicit
        // bail.
        if message.starts_with("[Document attached: ") {
            tracing::info!("runtime: document-attached stream — falling back");
            return Err(Error::NotImplemented(
                "Streaming not supported for document-attached turns".into(),
            ));
        }

        // These four intents don't token-stream, but they must NOT dead-end
        // with "Not implemented" (the streaming endpoint is the ONLY one both
        // apps use, so a follow-up like "can you continue?" — classified
        // Metalingual/Conation — would error). Run the handler with the context
        // we already built (no re-classification), persist its assistant
        // message so the WS Complete frame can project the metadata, and emit
        // the full answer as a single chunk through the same StreamHandle.
        if matches!(
            intent,
            Intent::ComplexTask
                | Intent::MetalingualQuery
                | Intent::ConationQuery
                | Intent::CommissiveQuery
        ) {
            tracing::info!(
                intent = ?intent,
                "runtime: non-streaming intent — single-chunk graceful fallback"
            );
            let response = match intent {
                Intent::MetalingualQuery => {
                    self.handle_metalingual_query(message, conversation_id, &context)
                        .await?
                }
                Intent::ConationQuery => {
                    self.handle_conation_query(message, conversation_id, &context)
                        .await?
                }
                Intent::CommissiveQuery => {
                    self.handle_commissive_query(message, conversation_id, &context)
                        .await?
                }
                Intent::ComplexTask => {
                    self.handle_complex_task(message, conversation_id, &context, &tool_descriptors)
                        .await?
                }
                _ => unreachable!("matched above"),
            };
            // Persist the assistant message (the non-streaming handlers return a
            // Response for the caller to save — mirror handle_turn) so
            // `tr.message_metadata` in ws.rs finds it for the terminal frame.
            self.store.save_message(&response.message).await?;
            let message_id = response.message.id.clone();
            let content = response.message.content.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(1);
            let _ = tx.send(Ok(content)).await;
            drop(tx);
            let stream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
            return Ok(StreamHandle { message_id, stream });
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
            // Conversation-tag-driven active skill (2026-05-24
            // redesign): the digest suppression should follow the
            // surface that owns the conversation, not registry state.
            let active_skill = self.resolve_active_mode(conversation_id).await;
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

        // R3 — temporal tension pre-pass. Active for relational
        // skills only; zero-cost no-op for factual skills.
        self.maybe_splice_temporal_tensions(&mut context, message)
            .await;

        // Tool-Mastery Layer 2 — compute the tool dossier so
        // `build_system_message` can splice it. No-op on relational
        // skills (the helper short-circuits) and when no NoteStore
        // is wired.
        self.maybe_compute_tool_dossier(&mut context, conversation_id)
            .await;

        // KnowledgeQuery: real streaming path. Prepare the synthesis
        // plan synchronously (retrieval + evidence-shape routing +
        // source expansion + request build + retrieved_chunks
        // summaries), then spawn a tokio task that drives
        // `complete_stream_with_id` and forwards each token to the
        // caller as it arrives. This replaces the old one-shot wrapper
        // which made the desktop chat window sit inert for ~35s while
        // the full response was assembled server-side.
        if matches!(intent, Intent::KnowledgeQuery | Intent::ComparisonQuery) {
            tracing::info!(
                intent = ?intent,
                "runtime: stream path — KnowledgeQuery/ComparisonQuery with token streaming"
            );

            // RetrievalStart — fire immediately so the desktop chip
            // appears before the corpus search begins. Bypasses
            // `try_emit_narration` (which suppresses below 1.5s
            // elapsed) because the user is staring at typing-dots
            // and needs to see activity within 200ms. RetrievalComplete
            // below remains gated by the suppression rules.
            let retrieval_start_at = std::time::Instant::now();
            self.routing_events
                .emit_turn_narration(TurnNarration {
                    session_id: _session_id.clone(),
                    conversation_id: conversation_id.to_string(),
                    event: NarrationEvent {
                        phase: NarrationPhase::RetrievalStart,
                        text: "Searching your knowledge…".to_string(),
                        elapsed_ms: 0,
                    },
                })
                .await;

            let plan = self
                .prepare_knowledge_query_plan(message, &context, &intent, scope.as_deref())
                .await;
            tracing::debug!(
                retrieval_ms = retrieval_start_at.elapsed().as_millis() as u64,
                chunks = plan.chunks.len(),
                "runtime:retrieval_start_to_complete"
            );

            // PR5 — post-retrieval retrieval-miss diversion. Off-
            // target evidence shape (dispersed across ≥3 sources,
            // no source concentration, no title match) was
            // historically the exact input that produced confident
            // parametric fabrication. Suppress synthesis and emit a
            // clarification card instead.
            if plan.shape.is_off_target() {
                tracing::info!(
                    session_id = %_session_id,
                    retrieval_count = plan.shape.count,
                    distinct_sources = plan.shape.distinct_sources,
                    title_match = plan.shape.title_match,
                    top_source_repeat = plan.shape.top_source_repeat_count,
                    top_source = %plan.shape.top_source_label,
                    top1_score = plan.shape.top1_score,
                    median_ratio = plan.shape.median_ratio,
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

            // Narration: report retrieval shape on long turns.
            // Suppressed internally when total elapsed is below the
            // `NARRATION_MIN_ELAPSED` window or the per-turn cap is
            // hit. The session store guards both; this call is safe
            // on short turns — it just returns `None`.
            //
            // Emit on every non-empty retrieval (not just on
            // `top_source_repeat_count >= 2`). The user is staring at
            // the typing-dots spinner and the most useful thing we
            // can tell them after retrieval finishes is "we read N
            // chunks across these sources." When the top source
            // dominates we say so; otherwise we report the spread.
            if !plan.chunks.is_empty() {
                let txt = if plan.shape.top_source_repeat_count >= 2 {
                    format!(
                        "Read {} chunks — {} from one source, so I'll keep the answer focused.",
                        plan.chunks.len(),
                        plan.shape.top_source_repeat_count,
                    )
                } else {
                    format!(
                        "Read {} chunks across {} sources — drafting the response.",
                        plan.chunks.len(),
                        plan.shape.distinct_sources.max(1),
                    )
                };
                if let Some(event) = self.sessions.try_emit_narration(
                    &_session_id,
                    NarrationPhase::RetrievalComplete {
                        chunks_in: plan.chunks.len(),
                        corpora: plan.source_map.keys().cloned().collect(),
                    },
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
            // Tool-Mastery Layer 3 — cloned so the nested
            // post-stream gap-check spawn can write a
            // `tool_decision` outcome note after refinement
            // resolves. Soft-fail when no NoteStore is wired
            // (test harnesses): `record_tool_outcome` no-ops.
            let notes_for_outcome: Option<Arc<corpus_engine_notes::NoteStore>> =
                self.note_store.clone();
            // Cloned into the outer spawn so the post-stream gap-
            // check can emit narration chips that reach the desktop
            // UI alongside the INFORMATION REQUEST card. Without
            // these the chip-then-card glassbox UX silently drops
            // for the streaming path. See `run_collaboration` for
            // how they're consumed.
            let collab_routing_events: Option<Arc<dyn RoutingEventSink>> =
                Some(Arc::clone(&self.routing_events));
            let collab_session_id: Option<String> = Some(_session_id.clone());
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
                prompt_budget_note,
                folder_meta,
                meta_atlas_hits,
            } = plan;
            let documents_found = chunks.len();
            // Answerable-context gate for the refusal-retry (KQ path), mirroring
            // the DeepQuery spawn: only retry a refusal when evidence WAS
            // retrieved — a genuine "no sources" stays an honest abstention.
            let had_retrieved_chunks = documents_found > 0;
            let top_source_label = shape.top_source_label.clone();
            let coarse_intent_for_prov = coarse_intent.clone();
            let self_assessment_for_prov = self_assessment.clone();
            let routing_trigger_for_prov = classification.rationale.clone();

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

            // Narration: synthesis-start chip. Bridges the silent
            // gap between retrieval-complete and the first streamed
            // token — which on a cold primary slot can be 90+
            // seconds (model load) plus another minute or two of
            // CPU decode for a 35B Q6. Without this the user sees
            // the same "Working on it…" placeholder for the entire
            // wait. Emitted on the main task (we still hold `&self`)
            // immediately before the spawn that calls
            // `complete_stream_with_id`. The 1.5s narration gate
            // suppresses this on short DeepQuery turns where
            // synthesis is fast enough that no chip is needed.
            {
                let txt = "Generating a deep answer with the primary model — \
                           first use after a restart can take a minute."
                    .to_string();
                if let Some(event) = self.sessions.try_emit_narration(
                    &_session_id,
                    NarrationPhase::PrimarySynthesisStart,
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

            let cancel_for_stream = cancel_token.clone();
            tokio::spawn(async move {
                let started = std::time::Instant::now();

                let (mut s, mut model_id) =
                    match inference.complete_stream_with_id_and_finish(&request).await {
                        Ok(pair) => pair,
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    };

                let mut full_text = String::new();
                let mut observed_finish: Option<crate::types::FinishReason> = None;
                let mut observed_completion_tokens: Option<u32> = None;

                // Refusal-retry (mirror of the DeepQuery spawn): hold the head;
                // if it opens with the model's own refusal signal AND evidence
                // was retrieved, discard and re-synthesize once with the
                // guardrail-stripping system + answer prefill. One retry max.
                let mut head = String::new();
                let mut head_flushed = false;
                let mut retried = false;

                'synth: loop {
                    loop {
                        // Cancellation races the next frame. `biased`
                        // so a pending cancel wins over buffered
                        // tokens — the user asked us to stop NOW.
                        // Dropping `s` (when the spawn unwinds past
                        // 'synth) closes the provider channel; the
                        // embedded engine breaks on the failed send.
                        let frame = tokio::select! {
                            biased;
                            _ = cancel_for_stream.cancelled() => {
                                tracing::info!(
                                    chars_streamed = full_text.chars().count(),
                                    "kq-stream: cancelled by session token — terminating with FinishReason::Cancelled"
                                );
                                observed_finish = Some(crate::types::FinishReason::Cancelled);
                                break 'synth;
                            }
                            f = s.next() => match f {
                                Some(fr) => fr,
                                None => break,
                            },
                        };
                        use crate::types::StreamFrame;
                        match frame {
                            StreamFrame::Token(chunk) => {
                                if head_flushed {
                                    full_text.push_str(&chunk);
                                    if tx.send(Ok(chunk)).await.is_err() {
                                        return;
                                    }
                                } else {
                                    head.push_str(&chunk);
                                    full_text.push_str(&chunk);
                                    if head.chars().count() >= REFUSAL_HEAD_CHARS {
                                        if !retried
                                            && had_retrieved_chunks
                                            && looks_like_refusal_opener(&head)
                                        {
                                            retried = true;
                                            tracing::info!(
                                                target: "synth.refusal_retry",
                                                head = %head.chars().take(80).collect::<String>(),
                                                "kq-stream: refusal opener detected with evidence present — retrying with answer prefill"
                                            );
                                            full_text.clear();
                                            full_text.push_str(REFUSAL_RETRY_PREFIX);
                                            if tx
                                                .send(Ok(REFUSAL_RETRY_PREFIX.to_string()))
                                                .await
                                                .is_err()
                                            {
                                                return;
                                            }
                                            head_flushed = true;
                                            let mut retry_req = request.clone();
                                            retry_req.assistant_prefix =
                                                Some(REFUSAL_RETRY_PREFIX.to_string());
                                            retry_req.system_message =
                                                Some(REFUSAL_RETRY_SYSTEM.to_string());
                                            match inference
                                                .complete_stream_with_id_and_finish(&retry_req)
                                                .await
                                            {
                                                Ok((s2, mid2)) => {
                                                    s = s2;
                                                    model_id = mid2;
                                                    observed_finish = None;
                                                    observed_completion_tokens = None;
                                                    continue 'synth;
                                                }
                                                Err(e) => {
                                                    let _ = tx.send(Err(e)).await;
                                                    return;
                                                }
                                            }
                                        } else if tx
                                            .send(Ok(std::mem::take(&mut head)))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        } else {
                                            head_flushed = true;
                                        }
                                    }
                                }
                            }
                            StreamFrame::Finish { reason, usage } => {
                                observed_completion_tokens =
                                    usage.as_ref().map(|u| u.completion_tokens);
                                // FinishReason::Error means the slot bailed
                                // mid-stream (context overflow, decode failure,
                                // tokenizer rejection). Surface it so the
                                // post-stream path doesn't save a 0-char message
                                // + fire a misleading InformationRequest.
                                if let crate::types::FinishReason::Error(ref msg) = reason {
                                    tracing::warn!(
                                        finish_reason = "error",
                                        error = %msg,
                                        chars_streamed = full_text.len(),
                                        "kq-stream: slot terminated with Finish::Error — propagating as error frame"
                                    );
                                    let _ = tx
                                        .send(Err(crate::error::Error::Inference(msg.clone())))
                                        .await;
                                    return;
                                }
                                observed_finish = Some(reason);
                            }
                            StreamFrame::Error(msg) => {
                                let _ = tx.send(Err(crate::error::Error::Inference(msg))).await;
                                return;
                            }
                        }
                    }

                    // Stream ended while still buffering the head (a short
                    // answer below the threshold): decide on what we have.
                    if !head_flushed {
                        if !retried && had_retrieved_chunks && looks_like_refusal_opener(&head) {
                            retried = true;
                            tracing::info!(
                                target: "synth.refusal_retry",
                                head = %head.chars().take(80).collect::<String>(),
                                "kq-stream: short refusal detected with evidence present — retrying with answer prefill"
                            );
                            full_text.clear();
                            full_text.push_str(REFUSAL_RETRY_PREFIX);
                            if tx.send(Ok(REFUSAL_RETRY_PREFIX.to_string())).await.is_err() {
                                return;
                            }
                            head_flushed = true;
                            let mut retry_req = request.clone();
                            retry_req.assistant_prefix = Some(REFUSAL_RETRY_PREFIX.to_string());
                            retry_req.system_message = Some(REFUSAL_RETRY_SYSTEM.to_string());
                            match inference.complete_stream_with_id_and_finish(&retry_req).await {
                                Ok((s2, mid2)) => {
                                    s = s2;
                                    model_id = mid2;
                                    observed_finish = None;
                                    observed_completion_tokens = None;
                                    continue 'synth;
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(e)).await;
                                    return;
                                }
                            }
                        } else {
                            let _ = tx.send(Ok(std::mem::take(&mut head))).await;
                            head_flushed = true;
                        }
                    }
                    break 'synth;
                }

                // Post-synthesis guardrail: demote any quoted span that
                // isn't verbatim-present in the evidence shown to the
                // model before it's persisted, so the stored record (and
                // any reload of this bubble) can't present a composite /
                // fabricated quotation as verbatim. The live token stream
                // already went out unmodified — this hardens the durable
                // copy; the refinement path (collaboration.rs) re-verifies
                // any gap-check rewrite. Empty doc_context (parametric
                // path) is a no-op.
                let full_text = {
                    let v = crate::quote_verification::verify_answer_against_evidence(
                        &full_text,
                        &doc_context,
                    );
                    if v.demoted_count > 0 {
                        tracing::warn!(
                            demoted = v.demoted_count,
                            verified = v.verified_count,
                            "kq-stream: post-synthesis guardrail demoted unverified quotations"
                        );
                    }
                    v.rewritten
                };

                // Persist final assistant message with full KQ metadata
                // so the UI citation expander and provenance header
                // have everything they had on the non-streaming path.
                let (sources_for_prov, coverage_for_prov) = build_provenance_components(
                    &source_map,
                    &std::collections::HashMap::new(),
                    &folder_meta,
                    // KnowledgeQueryPlan doesn't carry the
                    // display-category lookup; the chip-label rename
                    // for conversation corpora only fires on the
                    // DeepQuery path (see `prepare_knowledge_context`).
                    // Threading the lookup through the plan is a
                    // follow-up if we want the KQ streaming surface
                    // to render "Your conversations" as well.
                    None,
                );
                // Phase 5 — typed Finish frame from the provider is
                // now the source of truth for length truncation, no
                // more chars-per-token heuristic. Falls back to
                // `Stop` when the provider closed the stream without
                // a terminal frame (older test stubs); the trait
                // `complete_stream_with_finish` default guarantees a
                // terminal frame on every provider that ships today.
                let finish_reason_typed =
                    observed_finish.unwrap_or(crate::types::FinishReason::Stop);
                let max_budget = inference_config.max_tokens;
                // Provider-reported count when present; otherwise fall
                // back to a chars-per-token estimate so the UI's
                // "(N generated)" line stays useful even on providers
                // that don't emit usage. The estimate is signposted
                // — tracing makes the source legible to the operator
                // post-hoc.
                let completion_tokens_val = observed_completion_tokens
                    .unwrap_or_else(|| (full_text.chars().count() / 4) as u32);
                if observed_completion_tokens.is_none() {
                    tracing::debug!(
                        chars = full_text.chars().count(),
                        est_completion_tokens = completion_tokens_val,
                        "runtime: kq-stream - usage absent, completion_tokens estimated from chars"
                    );
                }
                let provenance = ResponseProvenance {
                    intent: "KnowledgeQuery".to_string(),
                    search_method: Some("CorpusEngine".to_string()),
                    sources: sources_for_prov,
                    inference_backend: model_id,
                    oicp_match: None,
                    total_latency_ms: started.elapsed().as_millis() as u64,
                    tokens_used: completion_tokens_val as usize,
                    coarse_intent: coarse_intent_for_prov,
                    self_assessment: self_assessment_for_prov,
                    routing_trigger: routing_trigger_for_prov,
                    coverage: coverage_for_prov,
                    finish_reason: Some(finish_reason_typed),
                    max_tokens_budget: Some(max_budget),
                    completion_tokens: Some(completion_tokens_val),
                    context_window: inference.effective_context_size(),
                };
                let metadata_json = serde_json::json!({
                    "streamed": true,
                    "intent": "knowledge_query",
                    "documents_found": documents_found,
                    "search_ms": search_ms,
                    "result_quality": result_quality,
                    "provenance": provenance,
                    "retrieved_chunks": retrieved_chunks,
                    // Glassbox for the prompt-budget guard: non-null
                    // when assembly exceeded the context window and
                    // the prompt was trimmed (runtime::prompt_budget).
                    "prompt_budget": prompt_budget_note,
                    // Move 4 — canonical-entity-boost echo for the
                    // bench's fourth legibility lens. Empty when the
                    // registry was unset or matched no entities.
                    "meta_atlas_hits": meta_atlas_hits,
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
                    // Per the humility principle (see
                    // `prepare_knowledge_query_plan` for the long
                    // form): always run the gap check on KQ paths.
                    // The retrieval-shape route (FastFocused vs
                    // PrimarySynthesis) decides synthesis style; it
                    // does NOT decide whether the answer is actually
                    // grounded. The gap check is the LLM-based
                    // judge of "did the model answer the question?"
                    // and has to fire regardless of how concentrated
                    // the retrieval looked. Top-source label is
                    // included in the log so a grep on
                    // `gap_check_scheduled` reconstructs which
                    // retrieval-shape paths reach the check.
                    tracing::debug!(
                        route = ?route_for_log,
                        top_source = %top_source_label,
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
                    // Clone the routing-events sink + session id
                    // into the spawn so the gap-check chips ("now
                    // auditing the answer", "found something to
                    // ask about") reach the desktop UI alongside
                    // the in-flight INFORMATION REQUEST card.
                    let collab_events = collab_routing_events.clone();
                    let collab_sid = collab_session_id.clone();
                    let collab_sid_for_outcome = collab_sid.clone();
                    let collab_notes_for_outcome = notes_for_outcome.clone();
                    // Tool-Mastery Layer 3 — record what happened
                    // on this KQ turn so the next turn's dossier
                    // can read it. Outcome resolves from the
                    // post-stream refinement result (Stale =
                    // gap-check fired and rewrote the answer),
                    // plus the evidence-presence signal captured
                    // before the spawn (NoResults = retrieval was
                    // empty; Useful = chunks landed and the
                    // original answer stood). All writes are
                    // best-effort — see `dossier::record_tool_outcome`.
                    // Tool-Mastery Layer 3 — synchronous baseline
                    // write BEFORE the gap-check spawn fires. Writing
                    // here (not inside the spawn) guarantees the
                    // tool_decision lands even when the bench / CLI
                    // exits before the gap-check spawn completes —
                    // run_post_stream_refinement can take 10-30s and
                    // the next turn's dossier read would otherwise
                    // see nothing. The spawn below MAY overwrite with
                    // `Stale` when refinement actually rewrites the
                    // answer; the dossier reader returns
                    // most-recent-first so the later write supersedes
                    // when it lands in time.
                    // Decide outcome from three orthogonal signals so a
                    // turn whose retrieval LANDED but whose answer
                    // landed in "I don't know" territory still records
                    // `no-results` (the snapshot-freshness shape: the
                    // hybrid retriever happily returns 30+ historical
                    // Tour de France articles for a "2027 Tour" query
                    // even though none of them are about 2027). The
                    // answer-content check uses general English
                    // negation + absence patterns, not bank vocabulary,
                    // so it transfers across questions.
                    let answer_is_honest_negation = {
                        let lower = full_text.to_lowercase();
                        let has_negation = [
                            "don't",
                            "do not",
                            "cannot",
                            "can't",
                            "doesn't have",
                            "no information",
                            "no data",
                            "no record",
                            "outside",
                            "unable to",
                        ]
                        .iter()
                        .any(|w| lower.contains(w));
                        let has_scope_token = [
                            "information",
                            "data",
                            "record",
                            "snapshot",
                            "knowledge base",
                            "details",
                            "results",
                        ]
                        .iter()
                        .any(|w| lower.contains(w));
                        has_negation && has_scope_token
                    };
                    let retrieval_missed = documents_found == 0
                        || answer_is_honest_negation
                        || (!shape.title_match
                            && shape.query_token_coverage < EVIDENCE_MIN_TOKEN_COVERAGE);
                    let baseline_outcome = if retrieval_missed {
                        crate::memory::ToolDecisionOutcome::NoResults
                    } else {
                        crate::memory::ToolDecisionOutcome::Useful
                    };
                    let baseline_reasoning = if documents_found == 0 {
                        "knowledge retrieval returned 0 chunks".to_string()
                    } else if answer_is_honest_negation {
                        format!(
                            "retrieval returned {documents_found} chunks but \
                             the assistant's answer acknowledged a gap \
                             (snapshot-freshness or scope mismatch)"
                        )
                    } else if retrieval_missed {
                        format!(
                            "retrieval returned {documents_found} chunks but \
                             title_match=false and query_token_coverage={:.2} \
                             (corpus does not cover this topic)",
                            shape.query_token_coverage
                        )
                    } else {
                        format!("synthesised over {documents_found} chunks")
                    };
                    // Tier 1 (result memory): populate summary +
                    // turn_index so the next turn's dossier can
                    // render addressable references. `evidence_ids`
                    // stays empty here because the legacy KQ path
                    // doesn't route through knowledge_lookup tool —
                    // when it does (follow-up PR), this site will
                    // also pass the per-call ev-Tn-NNNN handles.
                    let summary = if shape.top_source_label.is_empty() {
                        None
                    } else {
                        Some(shape.top_source_label.clone())
                    };
                    // Turn index: count of prior user messages
                    // (zero-based). The current in-flight user
                    // message is already pushed onto
                    // conversation.messages by the time we reach
                    // this site, so subtract 1.
                    let turn_index_for_outcome = context
                        .conversation
                        .messages
                        .iter()
                        .filter(|m| matches!(m.role, Role::User))
                        .count()
                        .saturating_sub(1);
                    let baseline_extras = crate::memory::ToolDecisionExtras {
                        summary: summary.clone(),
                        evidence_ids: Vec::new(),
                        turn_index: turn_index_for_outcome,
                    };
                    crate::dossier::record_tool_outcome(
                        notes_for_outcome.as_deref(),
                        collab_sid_for_outcome.as_deref().unwrap_or(""),
                        Some(&conversation_id_owned),
                        "knowledge_lookup",
                        baseline_outcome,
                        &baseline_reasoning,
                        baseline_extras,
                    )
                    .await;

                    let outcome_notes = collab_notes_for_outcome.clone();
                    let outcome_notes_present = outcome_notes.is_some();
                    // Capture Tier-1 extras for the stale-write
                    // path inside the spawn (closure can't reach
                    // back to the dispatch-frame locals).
                    let stale_summary_for_capture = summary.clone();
                    let turn_index_for_capture = turn_index_for_outcome;
                    tokio::spawn(async move {
                        tracing::info!(
                            conversation_id = %collab_cid,
                            has_notes = outcome_notes_present,
                            documents_found,
                            "dossier:streaming_kq_outcome_spawn_fired"
                        );
                        let refined = run_post_stream_refinement(
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
                            collab_events,
                            collab_sid,
                        )
                        .await;
                        if refined.is_some() {
                            // Stale write supersedes the baseline
                            // entry from above. Preserve summary +
                            // turn_index so the dossier history
                            // stays addressable; flag the outcome
                            // change via the new reasoning.
                            let stale_extras = crate::memory::ToolDecisionExtras {
                                summary: stale_summary_for_capture.clone(),
                                evidence_ids: Vec::new(),
                                turn_index: turn_index_for_capture,
                            };
                            crate::dossier::record_tool_outcome(
                                outcome_notes.as_deref(),
                                collab_sid_for_outcome.as_deref().unwrap_or(""),
                                Some(&collab_cid),
                                "knowledge_lookup",
                                crate::memory::ToolDecisionOutcome::Stale,
                                "gap-check refined the post-stream answer",
                                stale_extras,
                            )
                            .await;
                        }
                    });
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

        // RetrievalStart — DeepQuery streaming path. Skipped for
        // SimpleQuery because that intent is a quick factual answer
        // and the existing RetrievalComplete narration is also gated
        // off for it (chunks typically empty). Fires immediately so
        // the chip is on screen before `prepare_knowledge_context`
        // returns.
        if !matches!(intent, Intent::SimpleQuery) {
            self.routing_events
                .emit_turn_narration(TurnNarration {
                    session_id: _session_id.clone(),
                    conversation_id: conversation_id.to_string(),
                    event: NarrationEvent {
                        phase: NarrationPhase::RetrievalStart,
                        text: "Searching your knowledge…".to_string(),
                        elapsed_ms: 0,
                    },
                })
                .await;
        }

        // 4. Search knowledge + build prompt (shared with handle_simple).
        let kc = self
            .prepare_knowledge_context(message, &context, &intent, scope.as_deref())
            .await;

        // Narration — DeepQuery / SimpleQuery streaming path. Mirrors
        // the KnowledgeQuery/ComparisonQuery branch above, but keyed
        // off `KnowledgeContext` (no `plan.shape` available here).
        // Suppressed by the session store when total elapsed < 1.5s
        // or the per-turn cap is hit, so this is safe on fast paths.
        if !matches!(intent, Intent::SimpleQuery) && !kc.chunks.is_empty() {
            let txt = format!(
                "Read {} chunks across {} sources — drafting the response.",
                kc.chunks.len(),
                kc.sources.len().max(1),
            );
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::RetrievalComplete {
                    chunks_in: kc.chunks.len(),
                    corpora: kc.sources.iter().map(|s| s.origin.clone()).collect(),
                },
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
        //
        // Tier 2: populate evidence_id_allowlist from the
        // conversation's prior tool_decision payloads so the
        // sampler's EvidenceIdAllowlistConstraint can block
        // fabrications of `[ev-Tn-NNNN]` ids the model hasn't
        // actually been given. Soft-fails to None when no prior
        // ids exist (Tier 1 prompt discipline is then the only
        // safety net — same posture as today).
        let evidence_id_allowlist = self.gather_evidence_id_allowlist(conversation_id).await;
        let mut request = CompletionRequest {
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
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist,
            lark_grammar: None,
        };
        // Phase-1 prompt-budget guard: assembled input + response
        // reservation must fit the context window, or the engine's
        // "Prompt too long" rejection becomes a terminal user-facing
        // error loop (note 2cd9227e). See `prompt_budget` for the
        // degradation ladder; the note lands in message metadata.
        let budget_note = match self.inference.effective_context_size() {
            Some(ctx) => {
                let (outcome, measured) = prompt_budget::enforce(
                    &mut request,
                    &|s| self.inference.count_tokens(s),
                    ctx,
                );
                // Phase 2: the memo records pre-trim DEMAND so the
                // compaction sensor and next-turn allocator see what
                // assembly actually wanted.
                self.record_assembly(conversation_id, measured);
                match outcome {
                    prompt_budget::BudgetOutcome::Trimmed { note } => Some(note),
                    _ => None,
                }
            }
            None => None,
        };

        let search_method = kc.search_method;
        let sources = kc.sources;
        let coverage = kc.coverage;
        let retrieved_chunks = kc.retrieved_chunks;
        // Answerable-context gate for the refusal-retry: only retry a refusal
        // when evidence WAS retrieved (a genuine "no sources" must still be an
        // honest abstention, never force-answered).
        let had_retrieved_chunks = !retrieved_chunks.is_empty();

        // Format the corpus evidence now so the post-stream epistemic-
        // humility hook can feed it to the gap checker. Moved into the
        // streaming spawn; not used before the synthesis completes.
        let evidence = format_scored_chunks(&kc.chunks, MAX_KNOWLEDGE_CHARS);
        let question = message.to_string();

        let intent_label = format!("{intent:?}");
        let message_id = uuid::Uuid::new_v4().to_string();

        // Narration — synthesis-start chip on the DeepQuery /
        // SimpleQuery streaming path. Bridges the silence between
        // retrieval-complete and the first streamed token. With
        // primary-slot prewarm in place this is typically a no-op
        // wait, but it's still the right time to acknowledge the
        // long phase to the user.
        if matches!(request.preferred_speed, Speed::Slow) {
            let txt = "Generating a deep answer with the primary model.".to_string();
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::PrimarySynthesisStart,
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

        // 5. Spawn streaming task.
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let approval = Arc::clone(&self.approval);
        let inference_config = self.inference_config.clone();
        // Cloned into the spawn so the post-stream gap-check chips
        // can reach the desktop UI. See the matching block in the
        // KnowledgeQuery streaming branch above for the rationale.
        let routing_events_for_spawn: Option<Arc<dyn RoutingEventSink>> =
            Some(Arc::clone(&self.routing_events));
        let session_id_for_spawn: Option<String> = Some(_session_id.clone());
        let conversation_id_owned = conversation_id.to_string();
        let message_id_owned = message_id.clone();
        // Capture recalled memories on the relational/witness path so
        // the desktop's inner-work surface can render echo dots in the
        // gutter beside the just-committed paragraph. Gated to the
        // relational register so non-relational turns don't leak
        // memory contents into UI metadata they don't need. Thin
        // shape — id + content + created_at is what the echo overlay
        // displays; the rest of the Memory record stays internal.
        let recalled_memories_for_metadata: Option<serde_json::Value> =
            if context.turn_register() == SkillRegister::Relational && !context.memories.is_empty()
            {
                Some(serde_json::Value::Array(
                    context
                        .memories
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "id": m.id,
                                "content": m.content,
                                "created_at": m.created_at,
                            })
                        })
                        .collect(),
                ))
            } else {
                None
            };

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);

        let cancel_for_stream = cancel_token.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut full_text = String::new();

            let (mut s, mut model_id) =
                match inference.complete_stream_with_id_and_finish(&request).await {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };
            let mut observed_finish: Option<crate::types::FinishReason> = None;
            let mut observed_completion_tokens: Option<u32> = None;

            // Refusal-retry: hold the head of the stream; if it opens with the
            // model's OWN refusal signal AND evidence was retrieved, discard and
            // re-synthesize ONCE with an answer-prefill that forces engagement
            // past the refusal. One retry max (`retried`); the retry streams
            // live (no second buffering). See `looks_like_refusal_opener`.
            let mut head = String::new();
            let mut head_flushed = false;
            let mut retried = false;

            'synth: loop {
                loop {
                    // Cancellation races the next frame — see the
                    // matching note on the KQ loop above.
                    let frame = tokio::select! {
                        biased;
                        _ = cancel_for_stream.cancelled() => {
                            tracing::info!(
                                chars_streamed = full_text.chars().count(),
                                "deep-stream: cancelled by session token — terminating with FinishReason::Cancelled"
                            );
                            observed_finish = Some(crate::types::FinishReason::Cancelled);
                            break 'synth;
                        }
                        f = s.next() => match f {
                            Some(fr) => fr,
                            None => break,
                        },
                    };
                    use crate::types::StreamFrame;
                    match frame {
                        StreamFrame::Token(chunk) => {
                            if head_flushed {
                                full_text.push_str(&chunk);
                                if tx.send(Ok(chunk)).await.is_err() {
                                    return;
                                }
                            } else {
                                head.push_str(&chunk);
                                full_text.push_str(&chunk);
                                if head.chars().count() >= REFUSAL_HEAD_CHARS {
                                    if !retried
                                        && had_retrieved_chunks
                                        && looks_like_refusal_opener(&head)
                                    {
                                        retried = true;
                                        tracing::info!(
                                            target: "synth.refusal_retry",
                                            head = %head.chars().take(80).collect::<String>(),
                                            "deep-stream: refusal opener detected with evidence present — retrying with answer prefill"
                                        );
                                        full_text.clear();
                                        full_text.push_str(REFUSAL_RETRY_PREFIX);
                                        if tx
                                            .send(Ok(REFUSAL_RETRY_PREFIX.to_string()))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                        head_flushed = true;
                                        let mut retry_req = request.clone();
                                        retry_req.assistant_prefix =
                                            Some(REFUSAL_RETRY_PREFIX.to_string());
                                        retry_req.system_message =
                                            Some(REFUSAL_RETRY_SYSTEM.to_string());
                                        match inference
                                            .complete_stream_with_id_and_finish(&retry_req)
                                            .await
                                        {
                                            Ok((s2, mid2)) => {
                                                s = s2;
                                                model_id = mid2;
                                                observed_finish = None;
                                                observed_completion_tokens = None;
                                                continue 'synth;
                                            }
                                            Err(e) => {
                                                let _ = tx.send(Err(e)).await;
                                                return;
                                            }
                                        }
                                    } else if tx
                                        .send(Ok(std::mem::take(&mut head)))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    } else {
                                        head_flushed = true;
                                    }
                                }
                            }
                        }
                        StreamFrame::Finish { reason, usage } => {
                            observed_completion_tokens = usage.as_ref().map(|u| u.completion_tokens);
                            // Finish::Error means the slot bailed mid-stream
                            // (context overflow, decode failure, etc.). Forward
                            // as an error frame so the desktop doesn't save a
                            // 0-char message and trigger a misleading gap check.
                            if let crate::types::FinishReason::Error(ref msg) = reason {
                                tracing::warn!(
                                    finish_reason = "error",
                                    error = %msg,
                                    chars_streamed = full_text.len(),
                                    "deep-stream: slot terminated with Finish::Error — propagating as error frame"
                                );
                                let _ = tx
                                    .send(Err(crate::error::Error::Inference(msg.clone())))
                                    .await;
                                return;
                            }
                            observed_finish = Some(reason);
                        }
                        StreamFrame::Error(msg) => {
                            let _ = tx.send(Err(crate::error::Error::Inference(msg))).await;
                            return;
                        }
                    }
                }

                // Stream ended while still buffering the head (a short answer
                // below the threshold): decide on what we have.
                if !head_flushed {
                    if !retried && had_retrieved_chunks && looks_like_refusal_opener(&head) {
                        retried = true;
                        tracing::info!(
                            target: "synth.refusal_retry",
                            head = %head.chars().take(80).collect::<String>(),
                            "deep-stream: short refusal detected with evidence present — retrying with answer prefill"
                        );
                        full_text.clear();
                        full_text.push_str(REFUSAL_RETRY_PREFIX);
                        if tx.send(Ok(REFUSAL_RETRY_PREFIX.to_string())).await.is_err() {
                            return;
                        }
                        head_flushed = true;
                        let mut retry_req = request.clone();
                        retry_req.assistant_prefix = Some(REFUSAL_RETRY_PREFIX.to_string());
                        retry_req.system_message = Some(REFUSAL_RETRY_SYSTEM.to_string());
                        match inference.complete_stream_with_id_and_finish(&retry_req).await {
                            Ok((s2, mid2)) => {
                                s = s2;
                                model_id = mid2;
                                observed_finish = None;
                                observed_completion_tokens = None;
                                continue 'synth;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    } else {
                        let _ = tx.send(Ok(std::mem::take(&mut head))).await;
                        head_flushed = true;
                    }
                }
                break 'synth;
            }

            // Phase 5 — typed Finish frame from the provider is the
            // source of truth for length truncation. Falls back to
            // `Stop` when the provider closed without a terminal
            // frame (older test stubs); the trait
            // `complete_stream_with_finish` default guarantees a
            // terminal frame on every provider that ships today.
            let finish_reason_typed = observed_finish.unwrap_or(crate::types::FinishReason::Stop);
            let max_budget = inference_config.max_tokens;
            let completion_tokens_val = observed_completion_tokens
                .unwrap_or_else(|| (full_text.chars().count() / 4) as u32);
            if observed_completion_tokens.is_none() {
                tracing::debug!(
                    chars = full_text.chars().count(),
                    est_completion_tokens = completion_tokens_val,
                    "runtime: deep-stream - usage absent, completion_tokens estimated from chars"
                );
            }
            let provenance = ResponseProvenance {
                intent: intent_label,
                search_method,
                sources,
                inference_backend: model_id,
                oicp_match: None,
                total_latency_ms: started.elapsed().as_millis() as u64,
                tokens_used: completion_tokens_val as usize,
                coarse_intent,
                self_assessment,
                routing_trigger: classification.rationale.clone(),
                coverage,
                finish_reason: Some(finish_reason_typed),
                max_tokens_budget: Some(max_budget),
                completion_tokens: Some(completion_tokens_val),
                context_window: inference.effective_context_size(),
            };
            let metadata_json = serde_json::json!({
                "streamed": true,
                "provenance": provenance,
                "retrieved_chunks": retrieved_chunks,
                // Phase 3b: present only on the relational/witness
                // path; absent or null elsewhere. The desktop's
                // inner-work surface renders these as gutter echo
                // dots; chat ignores the field.
                "recalled_memories": recalled_memories_for_metadata,
                // Glassbox for the prompt-budget guard: non-null when
                // assembly exceeded the context window and the prompt
                // was trimmed to fit (see runtime::prompt_budget).
                "prompt_budget": budget_note,
            });
            // Post-synthesis guardrail (DeepQuery / reasoning stream):
            // same contract as the KnowledgeQuery stream — demote any
            // quoted span not verbatim-present in the evidence before
            // it's persisted. Empty evidence (pure-reasoning, no
            // retrieval) is a no-op. The refinement path
            // (collaboration.rs) re-verifies any gap-check rewrite.
            let full_text = {
                let v = crate::quote_verification::verify_answer_against_evidence(
                    &full_text,
                    &evidence,
                );
                if v.demoted_count > 0 {
                    tracing::warn!(
                        demoted = v.demoted_count,
                        verified = v.verified_count,
                        "deep-stream: post-synthesis guardrail demoted unverified quotations"
                    );
                }
                v.rewritten
            };
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
            // Routing-events sink + session id for gap-check
            // narration chips. Same rationale as the KnowledgeQuery
            // spawn above — without these the chip-then-card UX
            // silently drops on the streaming path. The clones
            // were already lifted above the outer spawn so this
            // is a cheap inner re-clone.
            let collab_events = routing_events_for_spawn.clone();
            let collab_sid = session_id_for_spawn.clone();
            // Post-stream tasks (epistemic-humility audit + auto-title)
            // share the fast-slot inflight semaphore with user-facing
            // requests. Under sequential load — eval bench, atlas
            // pipeline, anyone calling the daemon back-to-back — the
            // next request's routing classify queues behind these,
            // adding 30–60s of latency per turn for ~zero observable
            // benefit on the bench (the streamed answer is already
            // delivered; the refinement is a server-side rewrite).
            // Set `SOVEREIGN_SKIP_POST_STREAM=1` to disable both tasks.
            // The right architectural fix is a priority queue or
            // separate slot for background work; this env knob is the
            // diagnostic + bench-iteration lever.
            let skip_post_stream = std::env::var("SOVEREIGN_SKIP_POST_STREAM")
                .map(|v| v == "1")
                .unwrap_or(false);
            if !skip_post_stream {
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
                        collab_events,
                        collab_sid,
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
            }
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
    pub async fn handle_message(&self, message: &str, conversation_id: &str) -> Result<Response> {
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

    /// Seed an empty conversation row with an optional workspace skill
    /// tag BEFORE the first message — the daemon `/v1/conversations`
    /// surface's analog of the desktop "new chat" flow
    /// (`commands/conversation.rs`). Setting `skill_id = "recipe-author"`
    /// here is what makes [`Self::handle_message_any`] route the
    /// conversation into the recipe-author agent loop. INSERT-OR-IGNORE:
    /// a no-op if the row already exists.
    pub async fn seed_conversation(
        &self,
        id: &str,
        created_at: i64,
        skill_id: Option<&str>,
    ) -> Result<()> {
        self.store
            .insert_empty_conversation(id, created_at, skill_id)
            .await
    }

    /// Non-streaming entry that honours workspace agent-loops. A
    /// conversation tagged `recipe-author` runs the long-lived tool
    /// loop (the same dispatch the desktop streaming path uses at
    /// [`Self::handle_message_stream`]), drained to a single
    /// [`Response`]; every other conversation falls through to the
    /// standard [`Self::handle_message`] turn chain, unchanged. The
    /// daemon conversation API calls this so a headless caller reaches
    /// the real recipe-author loop rather than a side-channel.
    pub async fn handle_message_any(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        if self.resolve_active_mode(conversation_id).await.as_deref()
            == Some(crate::intent_policy::MODE_RECIPE_AUTHOR)
        {
            return self
                .handle_message_stream_drain(message, conversation_id)
                .await;
        }
        self.handle_message(message, conversation_id).await
    }

    /// Drive the streaming turn pipeline and drain it into a single
    /// [`Response`]. Reuses [`Self::handle_message_stream`] wholesale —
    /// context build, user-message persistence, routing, and the
    /// workspace agent-loop dispatch — so a non-streaming caller gets
    /// identical behaviour to the desktop streaming surface without
    /// re-implementing any of it.
    pub async fn handle_message_stream_drain(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        use futures::StreamExt;
        let StreamHandle {
            message_id,
            mut stream,
        } = self.handle_message_stream(message, conversation_id).await?;
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            text.push_str(&item?);
        }
        Ok(Response {
            message: Message {
                id: message_id,
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: text,
                created_at: now(),
                metadata: Some(
                    serde_json::json!({ "intent": "RecipeAuthor", "via": "stream_drain" }),
                ),
                version: now(),
            },
            task: None,
            metrics: None,
        })
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
    pub async fn handle_turn(&self, message: &str, conversation_id: &str) -> Result<Response> {
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

        // Iter5: per-stage timing. We accumulate millisecond costs
        // upstream of dispatch and then attach them to the response
        // metrics if the handler populated metrics (witness paths
        // only). Stages we don't instrument (build_context FTS,
        // working-memory compression, topic context, KV digests)
        // are sub-100ms in practice — the relational latency
        // budget lives in routing, memory recall, Pass A, tensions,
        // and synthesis.
        let mut upstream_metrics = RuntimeMetrics::default();

        // 1a. Embedding-based memory recall on relational/witness paths.
        // FTS keyword retrieval misses concrete-event memories on
        // abstract self-referential queries (hard-mode H05:
        // *"what kind of person am I?"* shares zero keywords with
        // *"I left my last job because the team was burning out"*).
        // Re-rank/replace `context.memories` via cosine over batched
        // embeddings. Falls back to the FTS list on any error.
        if context.turn_register() == SkillRegister::Relational {
            let recall_start = std::time::Instant::now();
            let scope = crate::traits::MemoryScope::from_conversation_skill(
                context.conversation.skill_id.as_deref(),
            );
            match memory::recall_relevant_memories_embed(
                self.inference.as_ref(),
                self.store.as_ref(),
                &scope,
                message,
                5,
            )
            .await
            {
                Ok(top) if !top.is_empty() => {
                    tracing::debug!(
                        before = context.memories.len(),
                        after = top.len(),
                        "runtime: memories overridden via embedding recall"
                    );
                    context.memories = top;
                }
                _ => {}
            }
            upstream_metrics.memory_recall_ms = Some(recall_start.elapsed().as_millis() as u64);
        }

        // 1b. Compress working memory from conversation history (now including
        //     the latest user message — gives working-memory extraction a
        //     crisper view of current intent).
        let working_memory_start = std::time::Instant::now();
        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        upstream_metrics.working_memory_ms =
            Some(working_memory_start.elapsed().as_millis() as u64);
        context.working_memory = working_memory;

        // 1c. Update topic context for turn-aware routing. Latest user
        //     message is part of the extraction input — see the
        //     streaming-path equivalent comment above for rationale.
        let topic_context_start = std::time::Instant::now();
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
            Some(message),
        )
        .await
        .ok();
        upstream_metrics.topic_context_ms = Some(topic_context_start.elapsed().as_millis() as u64);
        context.topic_context = topic_context;

        // 2. Route.
        //
        // Pre-classification narrowing (mode-only). See the
        // streaming-path comment for rationale; this keeps the two
        // dispatch surfaces symmetric so a turn classified the same
        // way sees the same tool catalog regardless of the
        // streaming/non-streaming distinction. Resolve the conv-tag
        // mode here so the narrow picks up the recipe-author catalog
        // (registry-side lookup misses workspace tags stored only on
        // the conversation row).
        let early_active_mode = self.resolve_active_mode(conversation_id).await;
        let tool_descriptors =
            self.narrow_tools_pre_classification_for_mode(early_active_mode.as_deref());
        let routing_start = std::time::Instant::now();
        let classification = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;
        upstream_metrics.routing_ms = Some(routing_start.elapsed().as_millis() as u64);
        upstream_metrics.routing_breakdown = classification.timing.clone();

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

        // Build the per-turn IntentPolicy on the non-streaming path
        // too, with the same shape as the streaming dispatch. See
        // that block for the contract; this stays symmetric so a
        // turn classified the same way sees the same policy on
        // either dispatch surface.
        let raw_intent = classification.primary.intent.clone();
        // Reuse the early resolution from the pre-classification
        // narrow site above — same conversation, same answer.
        let active_mode = early_active_mode.clone();
        let declared_register = active_mode
            .as_deref()
            .and_then(|id| self.skills.skill_by_id(id))
            .map(|s| s.inference.register)
            .unwrap_or_default();
        let intent_policy = crate::intent_policy::policy_for(
            &raw_intent,
            declared_register,
            active_mode.as_deref(),
        );
        let intent = intent_policy
            .effective_intent
            .clone()
            .unwrap_or_else(|| raw_intent.clone());
        context.intent_policy = Some(intent_policy);
        let coarse_intent = classification.coarse_intent.clone();
        let self_assessment = classification.self_assessment.clone();
        let scope = classification.scope.clone();

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            scope = ?scope,
            active_mode = ?active_mode,
            tier = ?policy.tier,
            "runtime: routed"
        );

        // Recipe-author workspace dispatch on the non-streaming path
        // (mesh peer, OICP caller, CLI). Symmetric with the streaming
        // dispatch above — same handler, drained into a Response.
        if active_mode.as_deref() == Some(crate::intent_policy::MODE_RECIPE_AUTHOR) {
            tracing::info!(
                intent = ?intent,
                "runtime: dispatching recipe-author workspace turn to agent loop (non-stream)"
            );
            return self
                .handle_recipe_author_turn(message, conversation_id, &context, &tool_descriptors)
                .await;
        }

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
            // Conversation-tag-driven active skill (2026-05-24
            // redesign): the digest suppression should follow the
            // surface that owns the conversation, not registry state.
            let active_skill = self.resolve_active_mode(conversation_id).await;
            provider
                .splice_landscape_digests(&mut context, active_skill.as_deref())
                .await;
        }

        // 2c. R3 — temporal tension pre-pass. Mirror of the
        // streaming path: active for relational skills only,
        // zero-cost no-op for factual skills.
        let tensions_start = std::time::Instant::now();
        self.maybe_splice_temporal_tensions(&mut context, message)
            .await;
        upstream_metrics.tensions_ms = Some(tensions_start.elapsed().as_millis() as u64);

        // 2d. Tool-Mastery Layer 2 — compute the dossier. Same
        // pattern as the streaming path: pre-pass populates the
        // field, `build_system_message` splices it.
        self.maybe_compute_tool_dossier(&mut context, conversation_id)
            .await;

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

        // ── Attached-document branch (the new path, sovereign decision 7693f16b) ──
        //
        // When this conversation has an active `DocumentSession`, the
        // user has attached a document and the answer probably lives in
        // it. Bypass intent classification + corpus-shaped retrieval
        // entirely and dispatch through a `ReasonWithTools`-style loop
        // over `[attached_doc_search, knowledge_lookup, web_fetch]`.
        // The model picks which tool to call (and how many times).
        //
        // The book-report bench (2026-05-20) surfaced the failure mode
        // this fixes: a question about Conrad's novel was classified as
        // `KnowledgeQuery` → corpus retrieval → 32 chunks from
        // `sep`+`wikipedia`, zero from the attached novel → answer about
        // the 2005 London bombings. The KQ handler doesn't consult the
        // tool catalog, so registering `attached_doc_search` alone
        // didn't change behaviour. Branching here is what makes the
        // tool actually fire.
        if context.document_session.is_some() {
            tracing::info!(
                conversation_id,
                "runtime: dispatching to handle_attached_doc_turn (document_session present)"
            );
            let result = self
                .handle_attached_doc_turn(message, conversation_id)
                .await;
            tracing::info!(
                success = result.is_ok(),
                total_latency_ms = turn_start.elapsed().as_millis() as u64,
                "runtime: turn end (attached_doc)"
            );
            return result;
        }

        // ── Team-pipeline gate (Phase 4 of the situated-team plan) ──
        //
        // Symmetric to the streaming-path gate at ~line 5087. When
        // `SOVEREIGN_TEAM_PIPELINE` is on AND the intent is one the
        // orchestrator handles, route through `run_team_pipeline`,
        // drain the Presenter stream into a single string, and
        // synthesize a `Response`. This is the entry point that
        // `voice_eval` exercises (it calls `handle_message` →
        // `handle_turn`, not the streaming path), so without this
        // branch flipping the kill-switch has no effect on the
        // bench harness.
        if crate::pipeline::is_team_pipeline_enabled()
            && matches!(
                intent,
                Intent::SimpleQuery
                    | Intent::DeepQuery
                    | Intent::KnowledgeQuery
                    | Intent::ComparisonQuery
                    | Intent::ExpressiveQuery
            )
        {
            tracing::info!(
                intent = ?intent,
                "team-pipeline: kill-switch enabled — routing turn through orchestrator (non-streaming path)"
            );
            let candidates = self.retrieve_candidates(message, &context, &intent).await;
            let register = context.turn_register();
            let witness_grounding = build_witness_grounding(&context, register);
            let inputs = crate::pipeline::TeamPipelineInputs {
                provider: Arc::clone(&self.inference),
                message,
                classification: &classification,
                register,
                candidates,
                max_tokens: crate::pipeline::DEFAULT_TEAM_PIPELINE_MAX_TOKENS,
                judge_enabled: true,
                witness_grounding,
            };
            let sink: Arc<dyn crate::pipeline::NarrationSink> =
                Arc::new(crate::pipeline::RoutingEventNarrationSink {
                    inner: Arc::clone(&self.routing_events),
                });
            let mut output = crate::pipeline::run_team_pipeline(
                inputs,
                sink,
                _session_id.clone(),
                conversation_id.to_string(),
            )
            .await?;

            // Drain the Presenter token stream into a single string.
            // Errors mid-stream produce a partial response — log and
            // continue rather than failing the whole turn, since the
            // user (or bench) still gets whatever was produced.
            let mut raw_text = String::new();
            while let Some(chunk) = output.stream.next().await {
                match chunk {
                    Ok(token) => raw_text.push_str(&token),
                    Err(e) => {
                        tracing::warn!(error = %e, "team-pipeline (non-stream): mid-stream error");
                        break;
                    }
                }
            }
            // iter4: strip mechanical artifacts from the Presenter
            // output here, in code, instead of asking the LLM to do
            // it (which iter1–iter3 showed caused the small Fast
            // slot to narrate the cleanup task instead of executing
            // it). The desktop streaming path applies the same
            // helper post-stream so users see clean text too — see
            // `pipeline::presenter::strip_presenter_artifacts`.
            let full_text = crate::pipeline::presenter::strip_presenter_artifacts(&raw_text);

            let total_turn_ms = turn_start.elapsed().as_millis() as u64;
            let assistant_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: None,
                version: now(),
            };
            self.store.save_message(&assistant_msg).await?;

            let mut metrics = upstream_metrics.clone();
            metrics.total_turn_ms = Some(total_turn_ms);

            tracing::info!(
                dispatch = "team_pipeline",
                total_latency_ms = total_turn_ms,
                "runtime: turn end (team-pipeline non-stream)"
            );
            return Ok(Response {
                message: assistant_msg,
                task: None,
                metrics: Some(metrics),
            });
        }

        // 3. Dispatch based on intent.
        // ComparisonQuery rides the same retrieval+synthesis path as
        // KnowledgeQuery; the difference is in (a) the OICP envelope
        // (Fast latency_class → fast slot) and (b) the comparison-aware
        // synthesis prompt branch built downstream by intent matching.
        // MetalingualQuery has its own handler — source-anchored
        // retrieval against a filtered corpus subset, distinct from
        // KnowledgeQuery's broad retrieval. ConationQuery,
        // CommissiveQuery, and ExpressiveQuery each have dedicated
        // situated handlers. Conation/Commissive still operate on
        // prior-turn / notes-store. ExpressiveQuery also operates
        // situated, but its Relational branch now consumes the
        // upstream FTS retrieval (`context.memories`) + any
        // temporal tensions so the witness contract can execute
        // its contradiction-across-time moves.
        let dispatch = match intent {
            Intent::ComplexTask => "handle_complex_task",
            Intent::KnowledgeQuery | Intent::ComparisonQuery => "handle_knowledge_query",
            Intent::MetalingualQuery => "handle_metalingual_query",
            Intent::ConationQuery => "handle_conation_query",
            Intent::CommissiveQuery => "handle_commissive_query",
            Intent::ExpressiveQuery => "handle_expressive_query",
            _ => "handle_simple",
        };
        tracing::info!(dispatch, "runtime: dispatching");

        let result = match intent {
            Intent::ComplexTask => {
                self.handle_complex_task(message, conversation_id, &context, &tool_descriptors)
                    .await
            }
            Intent::KnowledgeQuery | Intent::ComparisonQuery => {
                self.handle_knowledge_query(
                    message,
                    conversation_id,
                    &context,
                    &intent,
                    coarse_intent,
                    self_assessment,
                    classification.rationale.clone(),
                )
                .await
            }
            Intent::MetalingualQuery => {
                self.handle_metalingual_query(message, conversation_id, &context)
                    .await
            }
            Intent::ConationQuery => {
                self.handle_conation_query(message, conversation_id, &context)
                    .await
            }
            Intent::CommissiveQuery => {
                self.handle_commissive_query(message, conversation_id, &context)
                    .await
            }
            Intent::ExpressiveQuery => {
                self.handle_expressive_query(message, conversation_id, &context)
                    .await
            }
            _ => {
                self.handle_simple(
                    message,
                    conversation_id,
                    &context,
                    &intent,
                    coarse_intent,
                    self_assessment,
                    classification.rationale.clone(),
                    scope.as_deref(),
                )
                .await
            }
        };

        // Iter5: stitch upstream timings into the handler's metrics
        // when the witness path was active. Handlers fill in
        // pass_a_ms / synthesis_ms; we add routing / recall / tensions
        // here so the report sees the full waterfall.
        // Iter6: also stitch routing_breakdown, working_memory,
        // topic_context, and total_turn_ms.
        let total_turn_ms = turn_start.elapsed().as_millis() as u64;
        let result = result.map(|mut r| {
            if let Some(m) = r.metrics.as_mut() {
                m.routing_ms = upstream_metrics.routing_ms;
                m.routing_breakdown = upstream_metrics.routing_breakdown.clone();
                m.memory_recall_ms = upstream_metrics.memory_recall_ms;
                m.working_memory_ms = upstream_metrics.working_memory_ms;
                m.topic_context_ms = upstream_metrics.topic_context_ms;
                m.tensions_ms = upstream_metrics.tensions_ms;
                m.total_turn_ms = Some(total_turn_ms);
            }
            r
        });

        tracing::info!(
            dispatch,
            success = result.is_ok(),
            total_latency_ms = turn_start.elapsed().as_millis() as u64,
            "runtime: turn end"
        );
        result
    }
}

pub(crate) use self::attached_doc_render::{
    parse_tool_call_inline, render_attached_doc_conversation, truncate_for_chip, AttachedDocSegment,
};

mod attached_doc_render;

#[cfg(test)]
mod relational_intent_override_tests {
    use super::*;

    #[test]
    fn non_relational_register_is_passthrough() {
        let intent = Intent::MetalingualQuery;
        let out =
            crate::intent_policy::apply_witness_intent_override(&intent, SkillRegister::Factual);
        assert!(matches!(out, Intent::MetalingualQuery));
    }

    #[test]
    fn relational_overrides_metalingual_to_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::MetalingualQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_overrides_knowledge_to_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::KnowledgeQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_overrides_complex_task_to_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::ComplexTask,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_preserves_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::ExpressiveQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_preserves_deep_query() {
        // DeepQuery + Relational rides handle_simple's witness branch
        // and benefits from extended-thinking budget; don't downgrade.
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::DeepQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::DeepQuery));
    }

    #[test]
    fn relational_preserves_continuation() {
        // Continuation routes from the prior turn's rebound intent;
        // overriding here would mask the actual continuation context.
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::Continuation {
                task_id: "t-1".into(),
            },
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::Continuation { .. }));
    }
}
