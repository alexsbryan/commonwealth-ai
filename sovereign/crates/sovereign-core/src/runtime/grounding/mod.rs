// SPDX-License-Identifier: AGPL-3.0-or-later
//! Production grounding gate — the runtime port of the chaos-bench
//! critic (`bench_cmd/live_runner.rs::verify_grounding`), wired into
//! the KQ synthesis stream as **hold → gate → retry → abstain**.
//!
//! Mechanism (validated on two corpora, 2026-06-11): the synthesized
//! answer's single central claim is extracted (one small completion),
//! then checked per-retrieved-chunk with a forced-choice logprob pass
//! ("does THIS passage support THIS claim"); `violation_prob = 1 −
//! max(per-chunk support)`. On the contamination-free holdout bank the
//! verdicts separated cleanly: fabricated claims scored 0.96–1.00,
//! the highest-scoring CORRECT answer 0.45 — and every gated answer on
//! both banks contained genuine confabulation (zero false positives).
//!
//! Gate semantics, in order:
//!   1. `vp < τ`  → release the answer unchanged.
//!   2. `vp ≥ τ`  → ONE retry: re-synthesize with the failed claim
//!      quoted back as a constraint (minimal best-of-N — the second
//!      draft knows exactly which assertion failed verification).
//!   3. retry still `≥ τ` → release a grounded abstention instead.
//!      The user gets "your sources don't establish this" — never the
//!      confabulation.
//!
//! Long-form answers (past the profile's `longform_chars` pivot) take
//! the per-claim ladder instead: audit (each claim judged against the
//! prompt snapshot ∪ claim-conditioned sealed search) → ONE rewrite
//! fed the failed claims' corrective passages → visible verification
//! note on anything still unverified. An essay is never abstained.
//!
//! Surfaces, budgets, and the env surface (`SOVEREIGN_GROUNDING_GATE`,
//! per-surface overrides, `SOVEREIGN_GV_THRESHOLD`) live in
//! `config.rs` (`GateSurface`/`GroundingProfile`/
//! `grounding_gate_flags`); judges in `judge.rs` (prompts byte-pinned
//! to the bench critic); sealed claim search in `search.rs`.
//!
//! Scope guards (same as the bench critic): declines and explicitly
//! GK-attributed answers extract as NO_CLAIM and pass (the honest
//! OOD-caveat case must not be gated) — except on entity-anchored
//! questions, where a GK caveat cannot exempt an in-world claim.

mod audit_pass;
mod call_census;
mod citation;
mod citation_attribution;
// `pub(crate)` so the evidence loop can reach `debug_enabled` — one reader of
// `SOVEREIGN_AGENTIC_KQ_DEBUG` for the whole crate (TOPOLOGY §10 phase 10).
pub(crate) mod config;
mod judge;
pub mod native_grounding;
mod pipeline;
mod sealed;
mod search;
mod surgical;
mod value_presence;

// The gold-free groundedness primitive: the gate consumes it to DECIDE, the
// chaos scorer consumes it to MEASURE `blatant_confab_rate`. Re-exported up to
// `sovereign_core::runtime` (see runtime.rs) so the bench shares one
// implementation rather than re-deriving the check.
pub use value_presence::{assess_asserted_value, value_present_in_chunks, AssertedValue};

// The supporting-specifics half of groundedness: `value_presence` checks the
// answer's top-line VALUE, this strips `[Source: …]` citations whose title is
// absent from the evidence. The gate consumes it in `gate_held_answer`.
// `CitationAttribution` (the return type) is exported alongside but not yet named
// by a consumer — same `#[allow]` idiom as `grounding_gate_flags`.
#[allow(unused_imports)]
pub use citation_attribution::{attribute_citations, CitationAttribution};
// The pairing half of citation trust: a real label cited next to a value that
// lives in a DIFFERENT chunk (gen75 NARA misattribution). Consumed in
// `gate_held_answer` after the label-fidelity pass.
pub(crate) use citation_attribution::align_citation_values;

// The one funnel every gate model call goes through — see the module docs
// for why a call site that reaches `inference.complete` directly is a
// defect, not a shortcut.
pub(crate) use call_census::{gate_call, CallCensus};

pub(crate) use config::{dbg, grounding_gate_enabled, GateSurface, GroundingProfile};
// Registry export: consumed by the config-module coverage test today;
// the docs flag table renders from it (same contract as
// `retrieval_pipeline_flags`).
#[allow(unused_imports)]
pub use config::{grounding_gate_flags, grounding_gate_threshold};
#[allow(unused_imports)]
pub(crate) use judge::{verify_grounding, GateVerdict};
// THE CALIBRATED FORCED-CHOICE REGISTER, exported for the bench critic
// (`sovereign-cli-llm/src/bench_cmd/live_runner.rs`). This module's header
// claims the two are byte-identical so that tau=0.9's calibration transfers;
// before this export that identity was two copies of a literal in two crates,
// kept in step by hand. Sharing the renderer is what makes the claim
// structural (ARCH §10.6) — and it means a future change to the register
// moves BOTH sides, instead of leaving the calibration instrument behind.
pub use judge::{chunk_judge_prompt, CHUNK_JUDGE_PASSAGE_CHARS, CHUNK_JUDGE_SYSTEM};
pub use judge::{claim_extraction_prompt, CLAIM_EXTRACTION_SYSTEM};
// The FR-6 decorrelation driver (order deep-research-t0b, `tests/fr6_decorrelation.rs`)
// measures these two strings against the labeled bank as a genuine out-of-crate
// consumer; visibility per directives 13efc5dc + e39f87b2. Import-block addition only.
pub use judge::{
    claim_violation_joint, scan_unsupported_specifics, spans_supporting_claim_batched,
};
pub(crate) use pipeline::StreamingVerifier;
// `ClaimSearcher` is constructed via `Runtime::claim_searcher`; the
// type re-exports are for call sites that name them.
#[allow(unused_imports)]
pub(crate) use search::{
    conversation_pinned_evidence, seal_conversation_evidence, AttachedAssetSearcher, ClaimSearcher,
    SealedEvidenceSearch,
};

use std::collections::HashSet;
use std::sync::Arc;

// The kernel's grain, reached through the crate that publishes `Evidence`
// rather than by a second direct dep on the kernel leaf (ARCH §8.3) — the
// same door this file already takes `ScoredChunk` through. Replaced the
// local `EvidenceSource` enum 2026-08-20 (rung nc-4-evidence): two
// variants, identical meaning, one of them a copy.
use corpus_engine::Grain;

use crate::traits::InferenceProvider;
use crate::types::CitationTarget;
use crate::types::CompletionRequest;

// `claim_violation_joint` + `scan_unsupported_specifics` are bound via the
// `pub use` above (directive e39f87b2); this private import carries the rest.
use judge::unwrap_unverified_excerpts;

/// How much of the verified supporting quote ships with a citation-grounded
/// release. Raised from 220 on 2026-08-05: under the multi-quote contract the
/// quote is one verified sentence PER sub-question joined together, and 220
/// chars showed only the first — a citation the reader cannot look up is not a
/// citation. Display only; every character here is corpus text that already
/// passed the verbatim check, so a larger budget can reveal more but never
/// assert more.
const CITATION_QUOTE_DISPLAY_CHARS: usize = 900;

/// The gate's claim-extraction primitive, exported for callers OUTSIDE the
/// gate (`svrn bench verifier extract-claims`, the Stream B corruption
/// harness). Delegates to the same `judge::extract_claim_list` the longform
/// gate runs, so offline-extracted claims are in the exact register the
/// verifier sees at runtime — re-implementing the prompt in a script is the
/// drift this seam exists to prevent (VERIFIER_V0.md §3 Stream B).
pub async fn extract_claim_list(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    max_claims: usize,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<Vec<String>> {
    judge::extract_claim_list(inference, question, answer, max_claims, posture).await
}

/// The gate's per-chunk support primitive, exported for the same reason as
/// [`extract_claim_list`]: the bench faithfulness lane (T1 P0.3) judges
/// RAPTOR-summary claims against member-chunk texts, and it must do so in
/// the exact register the runtime gate uses — passage cap, prompt, and
/// forced-choice normalization included — or lane rates stop predicting
/// gate behavior. Returns support in [0,1]; `None` = judge failure.
pub async fn claim_chunk_support(
    inference: &Arc<dyn InferenceProvider>,
    passage: &str,
    claim: &str,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<f64> {
    judge::claim_chunk_support(inference, passage, claim, posture).await
}

/// The gate's JOINT per-claim register, exported for the judge-replay
/// harness (`svrn bench judge-replay`) — the third seam in the
/// [`extract_claim_list`] / [`claim_chunk_support`] family, for the same
/// reason: an offline verdict transfers to the production gate only if it was
/// produced by the EXACT production register (family renderer, system turn,
/// forced-choice normalization). `replay_` prefix because `judge::
/// claim_violation_joint` is already imported unqualified in this module;
/// this is pure delegation, not a second implementation (ARCH §10.6).
///
/// `chunks` is shared window + appended claim-conditioned passages, in that
/// order; `n_stable` is the shared-window length — exactly the
/// (`judged`, `n_shared`) pair the longform loop passes at its own call site.
pub async fn replay_claim_violation_joint(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[String],
    n_stable: usize,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<f64> {
    claim_violation_joint(inference, claim, chunks, chunks.len(), n_stable, posture).await
}

/// The joint register's PROMPT, without the model call — the replay
/// harness's bit-stability surface: two builds whose rendered bytes differ
/// are different judge configurations whatever their diff says. Delegates to
/// the one renderer ([`judge::EvidenceFamily`]).
pub fn replay_render_claim_prompt(
    shared: &[String],
    appended: &[String],
    claim: &str,
) -> (String, Option<usize>) {
    judge::replay_render_claim_prompt(shared, appended, claim)
}

/// The BATCHED support register, exported for the judge-replay harness
/// (order `audit-economy` D1: the batched text-A/B verdict is recalibrated
/// offline against the calibrated per-claim register before
/// `SOVEREIGN_GATE_BATCH_VERIFY` can flip). Pure delegation; `shared` is the
/// full shared window (the batched pre-pass judges the family window only —
/// exactly what `gate_longform` passes at its own call site). Returns one
/// entry per claim; `None` = no clean aligned verdict for that row.
pub async fn replay_claims_support_batched(
    inference: &Arc<dyn InferenceProvider>,
    claims: &[String],
    shared: &[String],
    posture: crate::oicp::ShardingPrivacy,
) -> Vec<Option<bool>> {
    judge::claims_support_batched(inference, claims, shared, shared.len(), posture).await
}

/// The batched register's PROMPT, without the model call — the replay
/// harness's bit-stability surface for the batched shape. Delegates to the
/// one renderer ([`judge::EvidenceFamily`]).
pub fn replay_render_batched_claims_prompt(
    shared: &[String],
    claims: &[String],
) -> (String, Option<usize>) {
    judge::replay_render_batched_claims_prompt(shared, claims)
}

/// The system turn every forced-choice judge call carries, behind an
/// accessor so the replay harness fingerprints WHATEVER constant this build
/// compiled in — the constant's *name* is exactly what judge-register lands
/// change (land C renames `CHUNK_JUDGE_SYSTEM` to `GATE_EVIDENCE_SYSTEM`),
/// and a harness naming one of them would silently stop compiling against
/// the other side of the very comparison it exists to make.
pub fn replay_judge_system_turn() -> &'static str {
    CHUNK_JUDGE_SYSTEM
}

/// The holistic specifics scan, exported for the judge-replay harness.
/// `evidence_chunks` is what the production call site passes: the leaf
/// window followed by the summary chunks (`gate_longform`'s
/// `scan_evidence`). Pure delegation; see [`replay_claim_violation_joint`].
pub async fn replay_scan_unsupported_specifics(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    leaf_chunks: &[String],
    summary_chunks: &[String],
    max_items: usize,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<Vec<String>> {
    scan_unsupported_specifics(
        inference,
        question,
        answer,
        leaf_chunks,
        summary_chunks,
        max_items,
        posture,
    )
    .await
}

/// WHAT one released answer is verified against — the sealed evidence
/// universe for one turn. Owned values throughout (the gate runs in
/// spawned stream tasks that hold no `&Runtime`).
pub(crate) struct EvidenceContext {
    /// Prompt-snapshot evidence the draft was synthesized from.
    pub chunks: Vec<String>,
    /// Legitimate citation labels for the citation-attribution check — each
    /// retrieved chunk's title and corpus id (what the synthesis presents as
    /// `[Source: …]` headers and the model cites). A `[Source: X]` whose words are
    /// absent from the chunk BODY but present in a label is grounded, not a
    /// fabrication. Empty when labels are unavailable (tool-transcript / step-
    /// summary evidence) — the check is then body-only.
    pub source_labels: Vec<String>,
    /// Per-chunk labels PARALLEL to `chunks` (`gate_evidence_chunk_labels`) —
    /// the mapping the citation-value ALIGNMENT check needs (WHICH chunk does a
    /// cited label name). Empty when unavailable — alignment is then skipped.
    pub chunk_labels: Vec<Vec<String>>,
    /// Claim-conditioned widening WITHIN the sealed universe.
    /// `None` = the snapshot IS the universe (e.g. tool transcripts).
    pub searcher: Option<Arc<dyn SealedEvidenceSearch>>,
    /// In-world question: a general-knowledge attribution cannot
    /// exempt a claim from extraction (see `verify_grounding`).
    pub entity_anchored: bool,
    /// Best retrieval similarity (max cosine = `1 - vector_distance`) over the
    /// chunks the draft saw, when known. Used ONLY by the env-gated retry floor
    /// (`SOVEREIGN_KQ_RETRY_FLOOR`): the gate's retry exists for the
    /// good-evidence-but-bad-draft case, so a high value means "the answer is in
    /// the evidence — re-synthesise" while a low value means "the evidence can't
    /// ground an answer — skip the second 35B synthesis and abstain now". `None`
    /// (FTS-only / surfaces that don't thread it) disables the floor → the retry
    /// fires exactly as before. Default behaviour is unchanged.
    pub top_similarity: Option<f32>,
    /// Human locator for each chunk, PARALLEL to `chunks` — the section
    /// heading a released citation names ("CHAPTER VII"). Resolved from the
    /// corpus's chunk→section join (`chapters.json` `chunk_ids` →
    /// `governance_view::section_titles`).
    ///
    /// Deliberately NOT folded into `chunk_labels`: that field widens what the
    /// citation-attribution check counts as legitimately grounded, so adding
    /// section headings to it would quietly loosen a fabrication guard. This
    /// one is display-only and can never make a claim pass.
    ///
    /// `None` per entry, or empty overall, whenever the corpus supplies no
    /// locator — no section structure, or an unjoined manifest. The release
    /// then omits the locator rather than inventing one.
    pub chunk_locators: Vec<Option<String>>,
    /// `(corpus, chunk)` handles aligned with `chunks` by index — the click
    /// target a released citation carries into the reading surface.
    ///
    /// EMPTY overall, or `None` per entry, whenever no handle is available;
    /// the citation then releases exactly as it did before this field existed,
    /// with a locator the reader can read and not open. Independent of
    /// `chunk_locators`: a corpus with no section structure yields a target
    /// and no locator, and one built from synthetic chunks yields the reverse.
    pub chunk_targets: Vec<Option<CitationTarget>>,
    /// Per-chunk provenance aligned with `chunks` by index (T1 P1.4).
    /// May be SHORTER than `chunks`: entries appended after the builder
    /// ran (sealed conversation evidence, code traces) have no source
    /// row and default to `Leaf` via [`EvidenceContext::source_of`].
    /// EMPTY = provenance unknown → the gate behaves exactly as before
    /// this field existed (additive, mesh-safe).
    pub chunk_sources: Vec<Grain>,
    /// Per-chunk custody aligned with `chunks` by index (custody.md
    /// §1-§2, reds R-2/R-3). May be SHORTER than `chunks`: entries
    /// appended after the builder ran (sealed conversation evidence,
    /// code traces) have no stamp and read as unknown via index — the
    /// refusal trigger (custody.md §4). EMPTY = no stamp anywhere, the
    /// gate behaves exactly as before this field existed (additive,
    /// mesh-safe).
    pub chunk_custodies: Vec<Option<crate::types::Custody>>,
    /// Per-chunk source URLs aligned with `chunks` by index — the
    /// `source_url` the gate's custody ledger releases (custody.md §5).
    /// `None` per entry whenever the chunk carries no URL.
    pub chunk_urls: Vec<Option<String>>,
    /// H1's typed admission verdict for this turn, when the native
    /// grounding path ran — which since 2026-08-11 is every turn by
    /// default. `None` whenever it did not (opted out with
    /// `SOVEREIGN_NATIVE_GROUNDING=0`, or no instrument), and `None` is
    /// what makes that path byte-identical to the incumbent: every read
    /// of this field is `if let Some(..)`.
    ///
    /// **Why the gate needs it.** The decline guard below
    /// (`released_pure_decline`) exists to RECOVER a decision the system
    /// already made but did not carry: the model wrote decline prose, and
    /// the gate reads 17 phrases back out of that prose to work out that
    /// the turn abstained. When H1 decided, that decision is already
    /// typed and the prose scan is re-deriving upstream work — the exact
    /// coupling this integration exists to remove. So a turn carrying a
    /// verdict takes its action from [`GroundingVerdict::to_gate_action`]
    /// (the single compatibility shim) and the phrase list is not
    /// consulted.
    ///
    /// Nothing is deleted for this: the zoo stays, every incumbent turn
    /// still uses it, and deletion is the graduation cutover.
    pub native_verdict: Option<crate::types::GroundingVerdict>,
}

impl EvidenceContext {
    /// Provenance of chunk `idx`. Indices past `chunk_sources` (late
    /// appends, or an empty vec entirely) read as `Leaf` — the
    /// conservative pre-P1.4 degradation.
    pub(crate) fn source_of(&self, idx: usize) -> Grain {
        self.chunk_sources.get(idx).copied().unwrap_or(Grain::Leaf)
    }

    /// True when any chunk is Summary-class — i.e. the P1.4 policy has
    /// something to decide. False short-circuits the claim loop to the
    /// exact pre-P1.4 code path.
    pub(crate) fn has_summary_evidence(&self) -> bool {
        self.chunk_sources.iter().any(|s| *s == Grain::Summary)
    }
}

/// The three per-chunk parallel arrays the gate consumes, built in ONE
/// ordering so they can never misalign (T1 P1.4).
///
/// History: Fix B (2026-06-17) EXCLUDED derived RAPTOR summaries from
/// gate evidence wholesale — an abstractive paraphrase must never be
/// the source-of-truth a factual claim is verified against (witnessed:
/// "the Russian agent Vladimir" with "Russian" absent from the source
/// — a fabrication grounding a fabrication). P1.4 keeps that bar for
/// FACTUAL claims while letting THEMATIC/STRUCTURAL claims use summary
/// evidence; the builder below owns both behaviors and their env
/// baselines.
pub(crate) struct GateEvidenceParts {
    pub chunks: Vec<String>,
    pub chunk_sources: Vec<Grain>,
    pub chunk_labels: Vec<Vec<String>>,
    /// Human section locators, built HERE rather than at the call sites so
    /// they pass through the same summary filter and Leaf-first reordering as
    /// `chunks`. Resolving them from the raw `ScoredChunk` slice outside this
    /// builder would leave them index-misaligned the moment a RAPTOR summary
    /// is dropped, and a misaligned locator names the wrong chapter with full
    /// confidence.
    pub chunk_locators: Vec<Option<String>>,
    /// `(corpus, chunk)` handles PARALLEL to `chunks` — what a released
    /// citation becomes clickable by. Built in this same builder for the
    /// alignment reason spelled out on `chunk_locators`; a target that has
    /// slipped a slot opens a different passage than the one quoted.
    ///
    /// `None` for a chunk with no stable row id (synthetic chunks, atlas
    /// summaries) — those simply ship as un-openable citations, exactly as
    /// every citation did before this field existed.
    pub chunk_targets: Vec<Option<CitationTarget>>,
    /// Per-chunk custody PARALLEL to `chunks` (custody.md §1-§2, red
    /// R-2/R-3): `Some(class)` when the acquisition path stamped the
    /// chunk, `None` when it carries no stamp. Built HERE through the
    /// same summary filter and Leaf-first reordering as every other
    /// parallel array, for the alignment reason spelled out on
    /// `chunk_locators`. An empty vec — no stamp anywhere — is the
    /// pre-custody shape: the gate behaves exactly as it always did.
    pub chunk_custodies: Vec<Option<crate::types::Custody>>,
    /// Per-chunk source URLs PARALLEL to `chunks` — what the gate's
    /// per-chunk custody ledger releases as `source_url` (custody.md
    /// §5). `None` for a chunk with no URL (synthetic chunks, late
    /// appends).
    pub chunk_urls: Vec<Option<String>>,
}

/// T1 P1.4 refinement of Fix B: instead of DROPPING derived RAPTOR
/// summaries from the gate's evidence, keep them — marked `Summary`,
/// appended AFTER every Leaf chunk — and let the claim loop apply the
/// class policy (factual/specific claims need Leaf support; summary
/// evidence may support thematic/structural claims). Leaf-first
/// ordering keeps the leaf window a byte-stable judge-prompt prefix
/// shared by both claim classes (the pinned-prefix cache survives).
///
/// `SOVEREIGN_GATE_SUMMARY_EVIDENCE=0` restores exact Fix B behavior
/// (summaries dropped, all-Leaf sources). `SOVEREIGN_GATE_EXCLUDE_RAPTOR=0`
/// (the pre-Fix-B A/B baseline) keeps summaries in retrieval ORDER and
/// marked Leaf — byte-identical to the historical baseline.
pub(crate) fn gate_evidence_with_sources(
    chunks: &[corpus_engine::ScoredChunk],
) -> GateEvidenceParts {
    let labels_of = |c: &corpus_engine::ScoredChunk| {
        let mut labels = Vec::with_capacity(2);
        if let Some(t) = c.title.as_deref() {
            let t = t.trim();
            if !t.is_empty() {
                labels.push(t.to_string());
            }
        }
        let cid = c.corpus_id.trim();
        if !cid.is_empty() {
            labels.push(cid.to_string());
        }
        labels
    };
    // The custody class the ACQUISITION DOOR recorded, read off the typed
    // stamp rather than re-parsed from the metadata bag (TOPOLOGY §10 rung
    // 9.1). `None` still means unstamped and still leaves the custody
    // machinery disengaged below — `stamped_custody` is `Option` precisely
    // so this site keeps that distinction; a pool where nothing is stamped
    // must not become a pool where everything refuses.
    let custody_of = |c: &corpus_engine::ScoredChunk| c.provenance.stamped_custody();
    let url_of = |c: &corpus_engine::ScoredChunk| c.url.clone();
    let exclude_raptor = std::env::var("SOVEREIGN_GATE_EXCLUDE_RAPTOR")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    if !exclude_raptor {
        // Historical pre-Fix-B baseline: summaries are source-equivalent.
        return GateEvidenceParts {
            chunks: chunks.iter().map(|c| c.content.clone()).collect(),
            chunk_sources: vec![Grain::Leaf; chunks.len()],
            chunk_labels: chunks.iter().map(labels_of).collect(),
            chunk_locators: gate_evidence_locators(chunks),
            chunk_targets: gate_evidence_targets(chunks),
            chunk_custodies: chunks.iter().map(custody_of).collect(),
            chunk_urls: chunks.iter().map(url_of).collect(),
        };
    }
    let summary_evidence = std::env::var("SOVEREIGN_GATE_SUMMARY_EVIDENCE")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    // Leaf or summary, off the same typed stamp. Was
    // `metadata["source"] == "raptor"`, which matched an indexed rollup row
    // and an in-process one by accident of a shared tag; `grain()` answers
    // for both arms on purpose.
    let is_summary = |c: &corpus_engine::ScoredChunk| c.provenance.grain() == Grain::Summary;
    // Resolved once over the ORIGINAL indices, then carried through the same
    // filter and reordering below, so `chunk_locators[i]` always names
    // `chunks[i]`.
    let locators = gate_evidence_locators(chunks);
    let locator_at = |i: usize| locators.get(i).cloned().flatten();
    // Resolved over the ORIGINAL indices for the same reason as the locators
    // above, and carried through the identical filter and reordering. A
    // misaligned target opens the WRONG passage under a correct-looking
    // heading, which is a worse failure than a misaligned locator: the reader
    // clicks a citation they were told is verbatim and lands somewhere else.
    let targets = gate_evidence_targets(chunks);
    let target_at = |i: usize| targets.get(i).cloned().flatten();
    let mut parts = GateEvidenceParts {
        chunks: Vec::with_capacity(chunks.len()),
        chunk_sources: Vec::with_capacity(chunks.len()),
        chunk_labels: Vec::with_capacity(chunks.len()),
        chunk_locators: Vec::with_capacity(chunks.len()),
        chunk_targets: Vec::with_capacity(chunks.len()),
        chunk_custodies: Vec::with_capacity(chunks.len()),
        chunk_urls: Vec::with_capacity(chunks.len()),
    };
    for (i, c) in chunks.iter().enumerate().filter(|(_, c)| !is_summary(c)) {
        parts.chunks.push(c.content.clone());
        parts.chunk_sources.push(Grain::Leaf);
        parts.chunk_labels.push(labels_of(c));
        parts.chunk_locators.push(locator_at(i));
        parts.chunk_targets.push(target_at(i));
        parts.chunk_custodies.push(custody_of(c));
        parts.chunk_urls.push(url_of(c));
    }
    if summary_evidence {
        for (i, c) in chunks.iter().enumerate().filter(|(_, c)| is_summary(c)) {
            parts.chunks.push(c.content.clone());
            parts.chunk_sources.push(Grain::Summary);
            parts.chunk_labels.push(labels_of(c));
            parts.chunk_locators.push(locator_at(i));
            parts.chunk_targets.push(target_at(i));
            parts.chunk_custodies.push(custody_of(c));
            parts.chunk_urls.push(url_of(c));
        }
    }
    parts
}

/// `(corpus, chunk)` handles PARALLEL to `chunks` — the click target a
/// released citation carries.
///
/// Pure projection of what retrieval already knows: no I/O, no manifest read,
/// no join. That is the whole difference from [`gate_evidence_locators`],
/// which has to resolve a chunk→section mapping off disk and can therefore
/// fail for reasons a target cannot. A chunk either has a stable row id or it
/// does not.
///
/// `None` for a chunk with no `chunk_id` — synthetic chunks, atlas-virtual
/// summaries, and local-doc chunks with String ids (see `ScoredChunk::chunk_id`).
/// Those citations release exactly as they always did, without a click target.
/// Note this is INDEPENDENT of the locator: a corpus with no section structure
/// yields `Some` target and `None` locator, and such a citation is openable
/// even though it can name no chapter.
pub(crate) fn gate_evidence_targets(
    chunks: &[corpus_engine::ScoredChunk],
) -> Vec<Option<CitationTarget>> {
    chunks
        .iter()
        .map(|c| {
            let chunk_id = c.chunk_id?;
            let corpus_id = c.corpus_id.trim();
            // A chunk id is only unique WITHIN a corpus, so a blank corpus id
            // makes the pair unresolvable. Drop it rather than ship half a
            // handle the reading surface would fail to deref at click time.
            if corpus_id.is_empty() {
                return None;
            }
            Some(CitationTarget {
                corpus_id: corpus_id.to_string(),
                chunk_id,
            })
        })
        .collect()
}

/// Human locators PARALLEL to `chunks` — the section heading a released
/// citation names ("CHAPTER VII"), resolved through the corpus's chunk→section
/// join.
///
/// `None` for a chunk whenever any link is missing: no `chunk_id` (synthetic
/// or atlas-virtual chunks), no `chapters.json`, an unjoined manifest (repair
/// with `svrn enrich backfill-sections`), or a section with no title. Silence
/// is the correct output in every one of those cases — a citation pointing at
/// the wrong chapter is worse than one pointing nowhere.
///
/// TWO LAYOUTS. A self-indexed corpus keeps its manifest under its own id. A
/// SIBLING layout keeps the text in a parent corpus while each document's
/// sections live in `<parent>-<doc>` — SEP is 1771 of these, and retrieval
/// hands back chunks tagged with the PARENT id (`sep`), so the direct lookup
/// finds nothing. The chunk's own title names the document, so
/// `<corpus_id>-<title>` is tried as a fallback. That is a naming convention,
/// not a guarantee; a corpus that pairs differently simply gets no locator
/// rather than a wrong one.
///
/// Manifests are read at most once per (corpus, document) per turn.
pub(crate) fn gate_evidence_locators(chunks: &[corpus_engine::ScoredChunk]) -> Vec<Option<String>> {
    use corpus_engine::enrichment::governance_view::{chunk_to_section_map, section_titles};
    use std::collections::HashMap;

    let indexes_root = crate::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes");

    // (corpus, doc-title) → (chunk id → section id, section id → heading).
    type Joined = (HashMap<u64, String>, HashMap<String, String>);
    let mut cache: HashMap<(String, String), Option<Joined>> = HashMap::new();

    chunks
        .iter()
        .map(|c| {
            let chunk_id = c.chunk_id?;
            let title = c.title.as_deref().unwrap_or_default().trim().to_string();
            let key = (c.corpus_id.clone(), title.clone());
            let entry = cache.entry(key).or_insert_with(|| {
                // Direct first (self-indexed), then the sibling convention.
                let mut roots = vec![indexes_root.join(&c.corpus_id)];
                if !title.is_empty() {
                    roots.push(indexes_root.join(format!("{}-{title}", c.corpus_id)));
                }
                roots.into_iter().find_map(|root| {
                    let map = chunk_to_section_map(&root);
                    (!map.is_empty()).then(|| (map, section_titles(&root)))
                })
            });
            let (by_chunk, titles) = entry.as_ref()?;
            let section = by_chunk.get(&chunk_id)?;
            titles
                .get(section)
                .filter(|t| !t.trim().is_empty())
                .cloned()
        })
        .collect()
}

/// The legitimate citation LABELS for `attribute_citations`: each chunk's title
/// and corpus id — the source identifiers the synthesis presents as `[Source: …]`
/// headers and the model cites. Unlike `gate_evidence_chunks` these are NOT body
/// text; they only WIDEN what the citation check counts as grounded, so a citation
/// naming a source by its corpus or section title is not mistaken for a fabrication.
/// RAPTOR summaries are NOT excluded here: a summary's title/corpus is still a real
/// label, and since labels never narrow groundedness, including them is always safe.
pub(crate) fn gate_evidence_source_labels(chunks: &[corpus_engine::ScoredChunk]) -> Vec<String> {
    let mut out = Vec::with_capacity(chunks.len() * 2);
    for c in chunks {
        if let Some(t) = c.title.as_deref() {
            let t = t.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
        let cid = c.corpus_id.trim();
        if !cid.is_empty() {
            out.push(cid.to_string());
        }
    }
    out
}

/// One audit-failed claim plus the claim-conditioned passages its
/// targeted search returned — the rewrite's correction material.
struct FailedClaim {
    claim: String,
    evidence: Vec<String>,
}

/// The grounded abstention released when both drafts fail the gate.
///
/// Deliberately does NOT restate the rejected claim's value. The old wording
/// ("The draft answer asserted that Heat's first name is Vernon …") re-uttered
/// the fabrication even while disclaiming it: a strict judge reads the named
/// value as an answer (measured — the primary judge scored these as "answered",
/// so the gate's abstentions didn't count), and a skimming user sees the
/// fabricated specific anyway. The failed claim is preserved in the gate's
/// glassbox `meta` / trace, not in the user-facing text — observability without
/// leakage.
///
/// Wording is a SELF-SCOPED epistemic hedge ("I couldn't confirm …"), NOT a
/// universal claim about the sources ("none of them cover it"). Measured
/// 2026-07-08 (8h chaos run, class-A "evidence-denial"): the gate's short
/// citation path abstains far more often than the evidence warrants (single-digit
/// answers filtered, verbatim quote-match misses), so the abstention frequently
/// fires when the answer IS in the passages. A universal negative is then a FALSE
/// statement about the sources — the trust rubric scores it as confabulation, and
/// it reads to the user as the app denying its own evidence. An assistant-scoped
/// "I couldn't verify this against them" is honest in BOTH the true-miss and the
/// mis-abstain case (it claims only the assistant's confidence, never the
/// sources' content), and the calibrated judge's decline-shape override already
/// treats it as an honest limitation rather than a fabrication.
pub(crate) fn grounded_abstention(_claim: &str, chunks_checked: usize) -> String {
    format!(
        "I couldn't confirm an answer to this against the {chunks_checked} passages \
         your sources turned up — so rather than guess at something I can't verify \
         from them, I'd flag that instead. If you think it's there, try rephrasing \
         with the specific names or terms involved and I'll take another look."
    )
}

/// Remove a leading general-knowledge caveat ("Not in your sources — from
/// general knowledge: …") so the gate verifies the asserted CLAIM, not the
/// hedge. Applied ONLY on entity-anchored questions: there a GK caveat can never
/// legitimately answer an in-world question, so the value after it must be
/// grounded or dropped. For genuinely out-of-domain questions (not
/// entity-anchored) the caveat IS the honest move and is left intact — this is
/// why the strip is gated on `entity_anchored`, not applied unconditionally.
fn strip_gk_caveat(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(crate::runtime::prompts::GK_CAVEAT_PREFIX) {
        return rest.trim_start().to_string();
    }
    // Robustness: the marker may not sit at the very start.
    let low = text.to_lowercase();
    if let Some(p) = low.find("from general knowledge:") {
        if let Some(after) = text[p..].split_once(':').map(|x| x.1) {
            return after.trim().to_string();
        }
    }
    text.to_string()
}

/// System-message suffix for the single gated retry. Quotes the failed
/// claim back — the second draft knows exactly which assertion failed
/// verification and must either ground it or drop it.
pub(crate) fn retry_system_note(claim: &str, corrective: &[String]) -> String {
    const RETRY_EVIDENCE_PER_CLAIM: usize = 2;
    const RETRY_EVIDENCE_CHARS: usize = 700;
    let mut note = format!(
        "\n\nGROUNDING CHECK FAILED on your previous draft. It asserted: \"{claim}\" — \
         no retrieved passage supports that assertion."
    );
    if corrective.is_empty() {
        note.push_str(
            " Write a new answer using ONLY what the passages state. If the passages \
             do not contain the asked-for fact, say plainly that the sources do not \
             state it. Do not repeat the unsupported assertion.",
        );
    } else {
        // Parity with the long-form rewrite (measured v13c–v15): a
        // retry told only WHICH assertion failed, with no passages
        // stating the truth, can only delete and disclaim.
        note.push_str("\n  What the sources actually say on this point:");
        for p in corrective.iter().take(RETRY_EVIDENCE_PER_CLAIM) {
            let trimmed: String = p.chars().take(RETRY_EVIDENCE_CHARS).collect();
            note.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
        }
        note.push_str(
            "\nWrite a new answer using ONLY what the passages state — if the \
             passages above contain the asked-for fact, state it (with citations); \
             do not repeat the unsupported assertion.",
        );
    }
    note
}

/// Final outcome of a full gate ladder over one draft answer.
pub(crate) struct GateOutcome {
    /// What the turn releases, and what it stands on.
    ///
    /// Was `text: String` until 2026-08-26 (TOPOLOGY phase 9, rung 9.2 —
    /// hazard 2). An [`Answer`](kernel_types::Answer) has no door that does
    /// not take a [`Judgement`](kernel_types::Judgement) by value, so a gate
    /// exit can no longer release text without saying how far the gate got
    /// with it. Sixteen exits constructed this struct and exactly ONE went
    /// through `Draft::release`; the other fifteen assigned a `String`, and
    /// the one that released flattened its `Answer` back to a `String` on the
    /// next line.
    ///
    /// Read the text with `outcome.answer.text()`.
    pub answer: kernel_types::Answer,
    /// `grounding_gate` metadata for the message (action, retried,
    /// violation_prob / failed_claims, threshold).
    pub meta: serde_json::Value,
    /// Per-claim audit records retained for the epistemic ledger
    /// (EPISTEMIC_STATE.md §4.2): the claims the ladder actually
    /// judged, with their FINAL verdicts. Empty when no claim was
    /// audited (NO_CLAIM release, judge fail-open, extraction
    /// failure). Purely additive — `meta` stays byte-identical.
    pub claims: Vec<GateClaim>,
}

/// What the two `native_*` keys carry when H1 did not run on this turn.
///
/// A stated value, not a silence. Until 2026-08-12 only ONE of this
/// file's fifteen `GateOutcome` sites attached the pair (the decline
/// guard); the other fourteen — including every `citation_grounded`
/// release, which is the bulk of a soak — omitted it, so anything
/// reading the meta back saw the same empty cell for "the instrument
/// scored nothing" and "this code path never attached the instrument".
/// That is absence reported as a value (ARCH §18.3), and the
/// native-grounding flip soak of 2026-08-11 misread it exactly that way:
/// 69 of its 73 grounding turns took non-decline actions, so the columns
/// would have read empty EVEN IF an instrument had been wired.
///
/// With this sentinel the two readings separate:
/// * key present, numeric / `released` / `abstained` → H1 ran, this is what it said;
/// * key present, `not_computed` → H1 did not run on this turn;
/// * key MISSING → the outcome was built without [`with_native_verdict`].
///
/// Deliberately distinct from every `GroundingVerdict::to_gate_action`
/// literal (`released`, `abstained`), so no reader can mistake the
/// sentinel for a decision.
///
/// Named for exactly what the gate knows, and no more. WHY H1 produced
/// nothing is decided upstream — `AdmissionOutcome::Disabled` (flag off)
/// and `AdmissionOutcome::NoInstrument { reason }` (flag on, nothing to
/// measure with) are distinct there — but `knowledge_query.rs` collapses
/// both to `None` when it fills `EvidenceContext::native_verdict`, so by
/// the time the gate sees the turn that distinction is gone. Carrying it
/// this far is a change to `EvidenceContext` and its every construction
/// site, not to this file; until then the sentinel says "not computed"
/// rather than guessing which of the two it was.
pub(crate) const NATIVE_VERDICT_NOT_COMPUTED: &str = "not_computed";

/// Attach H1's verdict to a gate outcome's metadata: the ONE place the
/// `native_answerability` / `native_decision` keys are named, and the
/// ONE place their absence is spelled (ARCH §10.6 — one decider, one
/// name; §18.3 — absence is reported, never defaulted). Every
/// `GateOutcome` built in this file runs its `meta` through here.
///
/// Telemetry only, in both directions. The keys are prefixed `native_`
/// so the desktop and the bench read them as "what the instrument
/// scored", never "what decided this turn": the action beside them is
/// decided by the ladder, and this function has no way to touch it
/// (`NATIVE_GROUNDING_PARITY_PLAN.md` §4.1 — H1's verdict is reported
/// beside the decision, never in place of it).
fn with_native_verdict(
    mut meta: serde_json::Value,
    native: Option<&crate::types::GroundingVerdict>,
) -> serde_json::Value {
    let (answerability, decision) = match native {
        Some(v) => (
            serde_json::json!(v.answerability),
            serde_json::json!(v.to_gate_action()),
        ),
        None => (
            serde_json::json!(NATIVE_VERDICT_NOT_COMPUTED),
            serde_json::json!(NATIVE_VERDICT_NOT_COMPUTED),
        ),
    };
    match meta.as_object_mut() {
        Some(m) => {
            m.insert("native_answerability".to_string(), answerability);
            m.insert("native_decision".to_string(), decision);
        }
        // Unreachable from this file (every site passes a `json!({…})`
        // object) — but a silent drop of the instrument is the exact
        // failure this helper exists to end, so it says so.
        None => tracing::warn!(
            target: "grounding_gate",
            "gate meta is not a JSON object — H1 telemetry not attached"
        ),
    }
    meta
}

/// One audited claim's retained record (see `GateOutcome::claims`).
#[derive(Debug, Clone)]
pub(crate) struct GateClaim {
    /// The claim text as extracted/judged.
    pub text: String,
    /// Whether the FINAL released text's version of this claim
    /// verified against the sealed evidence.
    pub supported: bool,
    /// True when the first check failed and the claim went through a
    /// retry / rewrite / annotation before release.
    pub failed_once: bool,
    /// The judge never returned a verdict for this claim (provider
    /// error, admission-queue shed, parse gap). The ladder fails open
    /// per claim, so the text shipped — but the record says nobody
    /// judged it, the audit exits `judge_failed_open`, and the epistemic
    /// ledger renders it FailOpen, never Verified (ARCH §18.3). Never
    /// `true` together with `failed_once`. Issue #57.
    pub unjudged: bool,
    /// The judge's violation probability, when a forced-choice verdict
    /// produced one (single-claim path only; long-form and citation
    /// records carry `None`).
    pub violation_prob: Option<f64>,
    /// Where in the sealed evidence this claim's text was located, when
    /// the span resolver found it verbatim in a single chunk
    /// (`NATIVE_GROUNDING.md` §6 — "a grounded segment is a holding with
    /// a real address"). `None` on every flag-off turn and on every
    /// claim that did not resolve to one contiguous span.
    ///
    /// **An address, never a verdict.** This field can only ever ADD a
    /// location to a claim whose `supported` was already decided by the
    /// judge. It does not set `supported`, cannot change it, and is not
    /// consulted by anything that does — because the resolver certifies
    /// at 0.7429 precision against that same judge
    /// (`bench/calibration/resolver-precision/FINDINGS.md`). Letting it
    /// touch `supported` would ship a wrong "Grounded" badge roughly one
    /// time in four.
    pub address: Option<ClaimAddress>,
}

/// Where a claim's text sits in the sealed evidence. Display provenance
/// attached to a holding — see [`GateClaim::address`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimAddress {
    /// Index into the turn's sealed chunk pool.
    pub chunk: usize,
    /// Byte range within that chunk.
    pub start: usize,
    pub end: usize,
}

/// Live claim-check progress out of the gate ladder — the frames the
/// desktop's verification panel renders (claims stamped one by one).
/// The receiver (streaming's `gate_held_answer`) forwards each frame
/// as a `turn-narration` event. Emission is `try_send` throughout:
/// perception, never backpressure — a full channel drops the frame and
/// the judge calls proceed untouched. `None` everywhere except the
/// streaming spawns keeps every other gated surface byte-identical.
pub(crate) type GateProgressSender = tokio::sync::mpsc::Sender<crate::types::NarrationPhase>;

/// Fire-and-forget progress emit (see `GateProgressSender`).
fn emit_gate_progress(progress: Option<&GateProgressSender>, frame: crate::types::NarrationPhase) {
    if let Some(tx) = progress {
        let _ = tx.try_send(frame);
    }
}

/// Wire-safe claim text for progress frames: the UI stamps one row per
/// claim, so a bounded prefix is enough (full texts stay in gate meta).
fn wire_claim(claim: &str) -> String {
    const CAP: usize = 160;
    if claim.chars().count() <= CAP {
        claim.to_string()
    } else {
        let mut s: String = claim.chars().take(CAP).collect();
        s.push('…');
        s
    }
}

/// The complete gate ladder, shared by every gated surface (see
/// `GateSurface`): short answers go through the single-claim
/// verify → retry → abstain ladder; long-form answers (past the
/// profile's `longform_chars` pivot) go through the per-claim
/// audit → rewrite → annotate ladder. Fail-open on judge failure
/// everywhere — the gate is a quality lever, not an availability
/// risk.
/// Env-gated retry floor (`SOVEREIGN_KQ_RETRY_FLOOR`, absolute cosine
/// similarity in 0..1): when the best retrieval similarity for a turn is below
/// this, the gate skips its second-synthesis retry and abstains directly. Unset
/// (or out of range) → no floor, the retry fires exactly as before.
fn retry_floor_env() -> Option<f32> {
    std::env::var("SOVEREIGN_KQ_RETRY_FLOOR")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|f| *f > 0.0 && *f < 1.0)
}

/// How far the gate got with the text it is about to release.
///
/// Until 2026-08-26 this was derived by PREFIX-MATCHING the wire action
/// string (`!action.starts_with("abstained") && !action.starts_with("judge_failed")`,
/// the old `:2297`), which is §2.1's smell and, worse, meant the verdict was
/// re-derived downstream from a value chosen upstream. One value now carries
/// both, so the wire id and the verdict cannot disagree (ARCH §10.6).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateReach {
    /// Judged, and it held.
    Held,
    /// Judged, and at least one claim was flagged. Released with its caveat.
    Flawed,
    /// The turn declined to answer.
    Declined,
    /// The gate ran and could not reach a verdict — extraction failed, the
    /// judge was unavailable, the ladder fell open. ARCH §18.2: not a pass.
    Unjudged,
}

/// What the gate did with this turn: the `meta["action"]` wire value, and how
/// far the gate got. The ids are byte-identical to the string literals they
/// replaced, so nothing on the wire moved.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct GateAction {
    pub id: &'static str,
    pub reach: GateReach,
}

impl GateAction {
    const fn new(id: &'static str, reach: GateReach) -> Self {
        Self { id, reach }
    }
}

pub(crate) const ACT_RELEASED: GateAction = GateAction::new("released", GateReach::Held);
pub(crate) const ACT_RETRY_RELEASED: GateAction =
    GateAction::new("retry_released", GateReach::Held);
pub(crate) const ACT_RETRY_RELEASED_SPECIFICS: GateAction =
    GateAction::new("retry_released_specifics", GateReach::Held);
pub(crate) const ACT_ABSTAINED: GateAction = GateAction::new("abstained", GateReach::Declined);
pub(crate) const ACT_ABSTAINED_NO_RETRY: GateAction =
    GateAction::new("abstained_no_retry", GateReach::Declined);
pub(crate) const ACT_ABSTAINED_WEAK_EVIDENCE: GateAction =
    GateAction::new("abstained_weak_evidence", GateReach::Declined);
pub(crate) const ACT_ABSTAINED_DECLINE: GateAction =
    GateAction::new("abstained_decline", GateReach::Declined);
pub(crate) const ACT_ABSTAINED_RETRY_ERROR: GateAction =
    GateAction::new("abstained_retry_error", GateReach::Declined);
pub(crate) const ACT_ABSTAINED_SPECIFICS: GateAction =
    GateAction::new("abstained_specifics", GateReach::Declined);
pub(crate) const ACT_JUDGE_FAILED_OPEN: GateAction =
    GateAction::new("judge_failed_open", GateReach::Unjudged);
/// A retry that released text the gate never got to verify. It used to share
/// a door with a verified release; §18.2 says those are different words.
pub(crate) const ACT_RETRY_RELEASED_UNVERIFIED: GateAction =
    GateAction::new("retry_released_unverified", GateReach::Unjudged);

// ─── The LONGFORM ladder's exits ─────────────────────────────────────────
//
// Added 2026-08-26. These six wire ids existed as bare string literals sitting
// beside a hand-picked `release_*` call, so the id and the verdict were chosen
// at two places and nothing stopped them disagreeing — the §10.6 defect the
// short-form path closed on 2026-08-26 and this one did not. The tell was a
// compiler warning: `GateReach::Flawed` was never constructed, because the
// three exits that ARE flawed released through `release_flawed` directly and
// never named a reach at all.
//
// The ids are byte-identical to the literals they replace.

/// The repair ladder is tombstoned; the audited draft is released with its
/// failed claims marked.
pub(crate) const ACT_ANNOTATED_MARKED: GateAction =
    GateAction::new("annotated_marked", GateReach::Flawed);
/// Claims flagged and released with a caveat, retry disarmed.
pub(crate) const ACT_ANNOTATED_NO_RETRY: GateAction =
    GateAction::new("annotated_no_retry", GateReach::Flawed);
/// The surgical rewrite itself errored; the flagged draft is released with a
/// caveat rather than lost.
pub(crate) const ACT_ANNOTATED_REWRITE_ERROR: GateAction =
    GateAction::new("annotated_rewrite_error", GateReach::Flawed);
/// Rewritten, re-audited, and it held.
pub(crate) const ACT_REWRITE_RELEASED: GateAction =
    GateAction::new("rewrite_released", GateReach::Held);
/// Rewritten, re-audited, and claims are still flagged — released with the
/// caveat.
pub(crate) const ACT_REWRITE_ANNOTATED: GateAction =
    GateAction::new("rewrite_annotated", GateReach::Flawed);
/// The rewrite produced text the gate never re-audited.
pub(crate) const ACT_REWRITE_RELEASED_UNVERIFIED: GateAction =
    GateAction::new("rewrite_released_unverified", GateReach::Unjudged);

/// The one dispatch from "how far did the gate get" to a `kernel-types` door.
///
/// Every gate exit goes through here, so there is exactly one answer to "what
/// judgement does a turn that ended THIS way carry" (ARCH §10.6).
fn release_as(
    action: GateAction,
    text: impl Into<String>,
    citations: Vec<kernel_types::Citation>,
    inference: &Arc<dyn InferenceProvider>,
    speed: crate::oicp::Speed,
) -> kernel_types::Answer {
    let why = format!("grounding gate: {}", action.id);
    release_as_because(action, text, citations, inference, speed, why)
}

/// [`release_as`] with the reason stated rather than derived from the id.
///
/// The longform ladder's exits know things the id cannot say — how many claims
/// were flagged, whether a rewrite ran — and that sentence becomes the
/// `Judgement`'s `Reason`. Same single dispatch: this is where the match
/// lives, and `release_as` is the caller that supplies a default.
fn release_as_because(
    action: GateAction,
    text: impl Into<String>,
    citations: Vec<kernel_types::Citation>,
    inference: &Arc<dyn InferenceProvider>,
    speed: crate::oicp::Speed,
    why: String,
) -> kernel_types::Answer {
    match action.reach {
        GateReach::Held => release_held(text, citations, inference, speed, why),
        GateReach::Flawed => release_flawed(text, citations, inference, speed, why),
        GateReach::Declined => abstain(text, inference, speed, why),
        GateReach::Unjudged => release_unjudged(text, citations, inference, speed, why),
    }
}

/// Release text this gate judged and found held.
///
/// One of the four doors out of this module, each wrapping exactly one
/// `kernel-types` constructor. They exist to NAME the four cases, not to
/// decide anything: the fold stays `Judgement::roll_up` inside
/// `Draft::release`, and no second reducer is written here (ARCH §10.6).
fn release_held(
    text: impl Into<String>,
    citations: Vec<kernel_types::Citation>,
    inference: &Arc<dyn InferenceProvider>,
    speed: crate::oicp::Speed,
    why: String,
) -> kernel_types::Answer {
    kernel_types::Draft::composed(text, citations).release(
        sealed::engine_attribution(&**inference, speed),
        &[kernel_types::Judgement::passed(
            kernel_types::TURN_SUBJECT,
            reason_or(why, "the gate found the released text held"),
        )],
    )
}

/// Release text the gate judged and found wanting — a known-failed claim
/// travels with its caveat, and now with its verdict.
fn release_flawed(
    text: impl Into<String>,
    citations: Vec<kernel_types::Citation>,
    inference: &Arc<dyn InferenceProvider>,
    speed: crate::oicp::Speed,
    why: String,
) -> kernel_types::Answer {
    kernel_types::Draft::composed(text, citations).release(
        sealed::engine_attribution(&**inference, speed),
        &[kernel_types::Judgement::failed(
            kernel_types::TURN_SUBJECT,
            reason_or(why, "the gate flagged at least one released claim"),
        )],
    )
}

/// The turn declined to answer, and the text says so.
fn abstain(
    text: impl Into<String>,
    inference: &Arc<dyn InferenceProvider>,
    speed: crate::oicp::Speed,
    why: String,
) -> kernel_types::Answer {
    kernel_types::Answer::abstained(
        text,
        sealed::engine_attribution(&**inference, speed),
        reason_or(why, "the gate declined to release an answer"),
    )
}

/// The gate ran and could not reach a verdict — claim extraction failed, the
/// judge was unavailable, the ladder fell open.
///
/// ARCH §18.2: a check that could not judge is not a check that passed, and
/// until this rung both released the same `String`.
fn release_unjudged(
    text: impl Into<String>,
    citations: Vec<kernel_types::Citation>,
    inference: &Arc<dyn InferenceProvider>,
    speed: crate::oicp::Speed,
    why: String,
) -> kernel_types::Answer {
    kernel_types::Draft::composed(text, citations).release(
        sealed::engine_attribution(&**inference, speed),
        &[kernel_types::Judgement::could_not_judge(
            kernel_types::TURN_SUBJECT,
            reason_or(why, "the gate could not reach a verdict on this turn"),
        )],
    )
}

/// `Reason::new` refuses placeholder text ("n/a", "unknown", ...). A refused
/// reason falls back to a named literal rather than to an empty one — the
/// substitution is visible in the source, never silent (ARCH §18.3).
fn reason_or(why: String, fallback: &'static str) -> kernel_types::Reason {
    kernel_types::Reason::new(why).unwrap_or_else(|| kernel_types::Reason::literal(fallback))
}

pub(crate) async fn gate_answer(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
) -> GateOutcome {
    gate_answer_with_progress(
        inference,
        question,
        draft,
        evidence,
        base_request,
        profile,
        None,
    )
    .await
}

/// `gate_answer` plus a live claim-check progress channel (see
/// `GateProgressSender`). The streaming spawns call this form; all
/// other surfaces keep the plain `gate_answer` signature.
///
/// This wrapper is also the ONE funnel through which every gate decision
/// reaches the local grounding journal (VERIFIER_V0.md §6.1, phase 0) —
/// wrapping rather than instrumenting each of the inner ladder's return
/// sites, so no exit path can forget to record (ARCH §10.6). It stamps
/// `episode_id` into the outcome meta, which the daemon persists with
/// the message row: that id is the join between the journal line, the
/// stored claim/answer text, and any future escalation line.
pub(crate) async fn gate_answer_with_progress(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    progress: Option<&GateProgressSender>,
) -> GateOutcome {
    let started = std::time::Instant::now();
    // G4 — open the gate's attribution window. This funnel is the only
    // place that holds the gate's wall clock independently of the stage
    // rows recorded inside it, so it is the only place that can compute
    // the in-gate residual. See `runtime::stage_ledger::gate_close`: a
    // gate mechanism that runs while recording no row shows up there as
    // seconds nobody claimed, which is how "a mechanism with no row is a
    // defect in the strip" becomes detectable outside a debug build.
    let ledger_window = crate::runtime::stage_ledger::gate_open();
    // D0 — open the per-CALL census (`call_census`). Same funnel, same
    // reason as the stage ledger above, one grain finer: the ledger says
    // which STAGE spent the seconds, this says which model CALL. They are
    // separate instruments on purpose — see `call_census`'s module docs for
    // why merging them would break the ledger's residual arithmetic.
    let census = CallCensus::new();
    let mut outcome = census
        .clone()
        .scope(gate_answer_inner(
            inference,
            question,
            draft,
            evidence,
            base_request,
            profile,
            progress,
        ))
        .await;
    let gate_ms = started.elapsed().as_millis() as u64;
    crate::runtime::stage_ledger::gate_close(ledger_window, gate_ms);
    record_gate_decision(&mut outcome, evidence, profile, gate_ms, census.take());
    outcome
}

/// Build the journal line for one gate decision and hand it to the
/// grounding stream. Metadata only, by construction: the claim, answer
/// and chunk text stay where they already live (conversation store,
/// corpus) — the line carries identity, scores, and what the gate did,
/// with the evidence as `(corpus, chunk_id)` handles from
/// `EvidenceContext::chunk_targets`. The append runs on a dropped-handle
/// blocking task and swallows IO errors into a `tracing::warn!`, so
/// journaling can neither delay nor fail a turn (the next-edit journal's
/// contract, note 43770c85 rule 4).
/// Project a gate decision onto the journal's four-valued verdict.
///
/// Pure so it can be watched fail. `claim_check_measured` is the guard
/// that the ladder's `violation_prob` is a MEASUREMENT rather than a
/// placeholder: `verify_grounding` returns `violation_prob: 0.0` from
/// three paths that never ran a check — no input, a long-form answer
/// outside the single-claim gate's scope, and NO_CLAIM (the assistant
/// declined, which is an honesty SUCCESS, not an audited claim that
/// passed). Until 2026-08-19 all three landed in the `Supported` arm, so
/// a turn the gate never evaluated was rendered to the user as verified.
/// Four verdicts, not two (ARCH §18.1); absence reported, never
/// defaulted (§18.3).
///
/// `claims_all_supported` is `Some(all_supported)` when the per-claim
/// ladder produced verdicts, `None` when it produced none.
fn project_verdict(
    violation_prob: Option<f64>,
    claim_check_measured: bool,
    tau: f64,
    claims_all_supported: Option<bool>,
) -> sovereign_contracts::types::GateJudgeVerdict {
    use sovereign_contracts::types::GateJudgeVerdict;
    match violation_prob {
        // A vp from a path that never judged is a fact about the
        // instrument, not a verdict about the answer.
        Some(_) if !claim_check_measured => GateJudgeVerdict::CouldNotJudge,
        Some(vp) if vp >= tau => GateJudgeVerdict::Unsupported,
        Some(_) => GateJudgeVerdict::Supported,
        None => match claims_all_supported {
            Some(true) => GateJudgeVerdict::Supported,
            Some(false) => GateJudgeVerdict::Unsupported,
            None => GateJudgeVerdict::CouldNotJudge,
        },
    }
}

fn record_gate_decision(
    outcome: &mut GateOutcome,
    evidence: &EvidenceContext,
    profile: &GroundingProfile,
    gate_ms: u64,
    calls: Vec<sovereign_contracts::types::GateCallRow>,
) {
    #[cfg(not(test))]
    use sovereign_contracts::types::{grounding_journal_append, journal_dir};
    use sovereign_contracts::types::{EvidenceRef, GroundingDecisionLine, GroundingLine};
    let mut d = GroundingDecisionLine::new(profile.surface.id(), profile.tau, gate_ms);
    // The per-call census (D0). The journal line below is the exact join for
    // the census script; these two surfaces exist because a reader should
    // not have to open a file (the log line) or replay a turn (the meta
    // summary) to learn which mechanism owns the gate's seconds (ARCH §9).
    if !calls.is_empty() {
        let call_ms: u64 = calls.iter().map(|c| c.ms).sum();
        let mut by_mech: std::collections::BTreeMap<&'static str, (u32, u64)> =
            std::collections::BTreeMap::new();
        for c in &calls {
            let e = by_mech.entry(c.mechanism.label()).or_insert((0, 0));
            e.0 += 1;
            e.1 += c.ms;
        }
        let breakdown = by_mech
            .iter()
            .map(|(m, (n, ms))| format!("{m}x{n}={ms}ms"))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!(
            target: "grounding_gate",
            gate_ms,
            calls = calls.len(),
            call_ms,
            // gate_ms minus the model calls: deterministic gate work plus
            // anything a mechanism spent without going through the funnel.
            unattributed_ms = gate_ms.saturating_sub(call_ms),
            breakdown = %breakdown,
            "gate call census"
        );
        // The compact form on the outcome's meta: counts and milliseconds
        // per mechanism, never the rows. Small enough to ride the message
        // row, and it is what makes the census assertable in-process — a
        // task-local that silently failed to install would pass every unit
        // test of the funnel while recording nothing in production, so the
        // instrument is checked on the real path (ARCH §18.4).
        if let Some(m) = outcome.meta.as_object_mut() {
            m.insert(
                "gate_call_ms".to_string(),
                serde_json::Value::Object(
                    by_mech
                        .iter()
                        .map(|(k, (_, ms))| ((*k).to_string(), serde_json::json!(ms)))
                        .collect(),
                ),
            );
            m.insert(
                "gate_call_n".to_string(),
                serde_json::Value::Object(
                    by_mech
                        .iter()
                        .map(|(k, (n, _))| ((*k).to_string(), serde_json::json!(n)))
                        .collect(),
                ),
            );
        }
    }
    d.calls = calls;
    d.entity_anchored = evidence.entity_anchored;
    d.claim_audited = !outcome.claims.is_empty();
    let meta = outcome.meta.as_object();
    d.action = meta
        .and_then(|m| m.get("action"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    d.retried = meta
        .and_then(|m| m.get("retried"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    d.violation_prob = meta
        .and_then(|m| m.get("violation_prob"))
        .and_then(|v| v.as_f64());
    // Verdict, from what the ladder reported. The vp comparison mirrors
    // the gate's own `>= tau` act condition; paths that judge without a
    // vp (citation-grounded) speak through their claim verdicts; a path
    // with neither judged nothing — could-not-judge, never a pass
    // (ARCH §18.1).
    // Did the gate actually judge, or is this a placeholder? Three paths
    // return `violation_prob: 0.0` WITHOUT running a check — no input,
    // long-form out-of-scope, and NO_CLAIM (a decline, i.e. an honesty
    // success). Before 2026-08-19 all three fell into the `Some(_) =>
    // Supported` arm below, so a turn the gate never evaluated was
    // rendered to the user as `Supported` — the exact overclaim the
    // comment above forbids. `gate_outcome` is written beside
    // `violation_prob` by every meta site; absent (older rows, or a path
    // that predates it) is treated as measured, preserving prior
    // behaviour rather than silently reclassifying history.
    let claim_check_measured = meta
        .and_then(|m| m.get("claim_check_outcome"))
        .and_then(|v| v.as_str())
        .map(|s| s == "measured")
        .unwrap_or(true);
    d.verdict = project_verdict(
        d.violation_prob,
        claim_check_measured,
        profile.tau,
        // A claim the judge never reached makes the whole check
        // could-not-judge (ARCH §18.1): `None` here projects to
        // CouldNotJudge, never to Supported.
        (!outcome.claims.is_empty() && !outcome.claims.iter().any(|c| c.unjudged))
            .then(|| outcome.claims.iter().all(|c| c.supported)),
    );
    d.chunks = evidence.chunks.len();
    d.evidence = evidence
        .chunk_targets
        .iter()
        .flatten()
        .map(|t| EvidenceRef {
            corpus: t.corpus_id.clone(),
            chunk: t.chunk_id,
        })
        .collect();
    d.evidence_unresolved = d.chunks.saturating_sub(d.evidence.len());
    d.top_similarity = evidence.top_similarity;
    if let Some(m) = outcome.meta.as_object_mut() {
        m.insert(
            "episode_id".to_string(),
            serde_json::Value::String(d.episode_id.clone()),
        );
    }
    // The per-chunk custody ledger the judge's evidence universe held
    // (custody.md §5, reds R-2/R-3): emitted for EVERY decision through
    // this funnel, so a refusal is auditable in the same shape as a
    // release. Emitted only when the stamp machinery engaged (at least
    // one stamped chunk) — a turn with no stamp anywhere carries no
    // custody record, and fabricating all-unknown rows would misread
    // every pre-custody surface as a refusal case.
    if evidence.chunk_custodies.iter().any(|c| c.is_some()) {
        let ledger: Vec<serde_json::Value> = (0..evidence.chunks.len())
            .map(|i| {
                let custody = evidence
                    .chunk_custodies
                    .get(i)
                    .copied()
                    .flatten()
                    .unwrap_or(crate::types::Custody::Unknown);
                // The chunk's stable id when it has one, else its URL —
                // else an index fallback, labeled as such (a store chunk
                // has no chunk id in this slice).
                let locator = evidence
                    .chunk_targets
                    .get(i)
                    .cloned()
                    .flatten()
                    .map(|t| t.chunk_id.to_string())
                    .or_else(|| evidence.chunk_urls.get(i).cloned().flatten())
                    .unwrap_or_else(|| format!("chunk-{i}"));
                let row = sovereign_contracts::types::ChunkCustody::new(
                    locator,
                    custody,
                    evidence.chunk_urls.get(i).cloned().flatten(),
                );
                serde_json::to_value(row).unwrap_or(serde_json::Value::Null)
            })
            .collect();
        if let Some(m) = outcome.meta.as_object_mut() {
            m.insert(
                "chunk_custody".to_string(),
                serde_json::Value::Array(ledger),
            );
        }
    }
    // Structural backstop for the H1 telemetry pair (ARCH §10 — make it
    // structural, not remembered). Every `GateOutcome` site in this file
    // builds its meta through `with_native_verdict`; this funnel is what a
    // future site that forgets trips on, because every outcome reaches it.
    // Warns, never panics: the gate is a quality lever, not an availability
    // risk, and a missing key is itself the readable "not attached" state.
    if let Some(m) = outcome.meta.as_object() {
        if !m.contains_key("native_answerability") || !m.contains_key("native_decision") {
            tracing::warn!(
                target: "grounding_gate",
                action = ?d.action,
                "gate outcome reached the journal with no H1 telemetry — a GateOutcome site skipped with_native_verdict"
            );
        }
    }
    let line = GroundingLine::Decision(d);
    // The line is BUILT under test — every branch above this point is
    // exercised — but not WRITTEN. Unit tests drive this funnel with mock
    // providers at millisecond gate times, and appending those to the
    // operator's real journal corrupts the one stream the latency census
    // reads by index: one `cargo test -p sovereign-core --lib grounding::`
    // run added 12 synthetic turns to `grounding-2026-08-13.jsonl`, four of
    // them with `gate_ms: 0`. A measurement instrument that its own test
    // suite writes into is not an instrument (ARCH §18.4).
    #[cfg(test)]
    let _ = line;
    #[cfg(not(test))]
    drop(tokio::task::spawn_blocking(move || {
        if let Err(e) = grounding_journal_append(&journal_dir(), &line) {
            tracing::warn!(target: "grounding_gate", error = %e, "grounding journal append failed");
        }
    }));
}

/// The gate ladder itself. Callers go through
/// [`gate_answer_with_progress`], which journals the decision — calling
/// this directly would be an unrecorded gate decision, which is the
/// thing the wrapper exists to make impossible.
async fn gate_answer_inner(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    progress: Option<&GateProgressSender>,
) -> GateOutcome {
    use crate::types::NarrationPhase;
    let tau = profile.tau;
    // H1's verdict for this turn, bound ONCE at the top so every exit of
    // this ladder can report it (see `with_native_verdict`). It was bound
    // at the decline guard before 2026-08-12, i.e. after four of this
    // function's six exits — which is why those four journaled nothing.
    // Telemetry: nothing below reads this to decide anything.
    let native = evidence.native_verdict.as_ref();
    // T1 P1.4: the short path audits ONE central FACTUAL claim, and its
    // citation / value-presence / name checks are all factual-class — so
    // this whole ladder reads the Leaf view only. A quote or value that
    // exists solely inside a derived RAPTOR summary is LLM prose quoting
    // LLM prose and must not ground a release. With no Summary-class
    // chunks (every pre-P1.4 surface) this is `evidence.chunks` itself.
    let leaf_owned: Vec<String>;
    // Locators travel through the SAME filter as the chunks they name. They
    // are looked up by index, so a Leaf-only chunk view paired with the full
    // locator list would attribute every quote to the wrong passage — the one
    // failure mode a citation label must not have.
    let leaf_locators: Vec<Option<String>>;
    // Targets travel through the SAME filter for the same reason, and with a
    // sharper consequence: a target read off the unfiltered list opens a
    // passage the reader was never shown.
    let leaf_targets: Vec<Option<CitationTarget>>;
    // Custody + URL travel through the SAME filter for the same reason:
    // a custody view read off the unfiltered list would pin a stamp to
    // the wrong passage (or worse, read a stamped chunk as unstamped).
    let leaf_custodies: Vec<Option<crate::types::Custody>>;
    let leaf_urls: Vec<Option<String>>;
    // Grain travels through the SAME filter, for the reason above and one
    // more: it is what the released citation's [`kernel_types::Origin`]
    // carries, so a grain read off the unfiltered list would stamp a quote
    // with another chunk's provenance. Binding it here rather than assuming
    // `Leaf` keeps the seal honest if this filter ever changes (rung
    // nc-20-turn-adoption).
    let leaf_grains: Vec<Grain>;
    // `_urls`: the leaf view's URLs exist for the ledger's locator
    // fallback, which the funnel derives from the FULL evidence; nothing
    // in the ladder reads the filtered view, so it is not bound.
    let (chunks, locators, targets, custodies, _urls, grains): (
        &[String],
        &[Option<String>],
        &[Option<CitationTarget>],
        &[Option<crate::types::Custody>],
        &[Option<String>],
        &[Grain],
    ) = if evidence.has_summary_evidence() {
        let keep: Vec<usize> = (0..evidence.chunks.len())
            .filter(|i| evidence.source_of(*i).may_be_quoted())
            .collect();
        leaf_owned = keep.iter().map(|i| evidence.chunks[*i].clone()).collect();
        leaf_locators = keep
            .iter()
            .map(|i| evidence.chunk_locators.get(*i).cloned().flatten())
            .collect();
        leaf_targets = keep
            .iter()
            .map(|i| evidence.chunk_targets.get(*i).cloned().flatten())
            .collect();
        leaf_custodies = keep
            .iter()
            .map(|i| evidence.chunk_custodies.get(*i).copied().flatten())
            .collect();
        leaf_urls = keep
            .iter()
            .map(|i| evidence.chunk_urls.get(*i).cloned().flatten())
            .collect();
        leaf_grains = keep.iter().map(|i| evidence.source_of(*i)).collect();
        (
            &leaf_owned,
            &leaf_locators,
            &leaf_targets,
            &leaf_custodies,
            &leaf_urls,
            &leaf_grains,
        )
    } else {
        leaf_grains = (0..evidence.chunks.len())
            .map(|i| evidence.source_of(i))
            .collect();
        (
            &evidence.chunks,
            &evidence.chunk_locators,
            &evidence.chunk_targets,
            &evidence.chunk_custodies,
            &evidence.chunk_urls,
            &leaf_grains,
        )
    };
    let entity_anchored = evidence.entity_anchored;
    // Custody refusal (custody.md §4, red R-3). When the stamp machinery
    // ENGAGED this turn — at least one chunk arrived with a stamp — an
    // unstamped chunk in the judged leaf view (sealed/pinned late
    // appends have no source row) must not ground a release: refuse
    // BEFORE any judge call, and let the funnel's ledger
    // (`record_gate_decision`) record the unknown row. Pure-unstamped
    // turns — every pre-custody surface, no stamp anywhere — are
    // untouched: with nothing stamped there is nothing to contrast the
    // unknown against, and this integration stays additive by
    // construction.
    let custody_engaged = evidence.chunk_custodies.iter().any(|c| c.is_some());
    if custody_engaged
        && custodies
            .iter()
            .any(|c| c.map(|c| !c.is_released_class()).unwrap_or(true))
    {
        let unstamped = custodies
            .iter()
            .filter(|c| c.map(|c| !c.is_released_class()).unwrap_or(true))
            .count();
        tracing::info!(
            target: "grounding_gate",
            unstamped,
            stamped = custodies.len() - unstamped,
            "gate refused: evidence holds unknown-provenance chunks (custody.md §4)"
        );
        return GateOutcome {
            answer: abstain(
                grounded_abstention(question, chunks.len().min(12)),
                inference,
                base_request.preferred_speed,
                "evidence holds unknown-provenance chunks (custody.md §4)".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "refused_unknown_custody",
                    "retried": false,
                    "violation_prob": null,
                    "threshold": tau,
                    "mode": "custody",
                    "draft": config::debug_enabled().then(|| draft.clone()),
                }),
                native,
            ),
            claims: Vec::new(),
        };
    }
    // Glassbox: whether the call-graph block reached the sealed universe. A
    // code-intel answer whose caller facts land in the verification note is
    // either "the trace was never sealed" or "the judge rejected it" — these
    // two counts tell you which, from any entry path.
    tracing::info!(
        target: "grounding_gate",
        evidence_chunks = chunks.len(),
        has_code_trace = chunks.iter().any(|c| c.contains("Call-graph trace for")),
        trace_labels = evidence
            .source_labels
            .iter()
            .filter(|l| l.starts_with("Call-graph trace for"))
            .count(),
        "gate entry: sealed evidence universe"
    );
    // Verify-correct pivot. gate_longform is the BS-catcher: it extracts each
    // asserted claim, RE-SEARCHES the sealed corpus for that claim's evidence,
    // and REWRITES the ones the corpus won't support — catching the load-bearing-
    // specific confabulation ("Ernest Rhys Jones" for "Ernest Rhys") that the
    // single-claim path waves through. Short factual answers skip it by default
    // (pivot 1_800); `SOVEREIGN_LONGFORM_CHARS` A/Bs routing them through it
    // (0 = always per-claim, the resilient default complex_task already uses) so
    // the architecture catches a model's first-pass BS rather than trusting it.
    let longform_pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(profile.longform_chars);
    if draft.chars().count() > longform_pivot {
        return gate_longform(
            inference,
            question,
            draft,
            evidence,
            base_request,
            profile,
            progress,
        )
        .await;
    }
    // Glassbox (debug-gated): record the pre-gate draft into the message meta so
    // the measurement layer can tell a gate-killed-CORRECT answer from a
    // confabulation the gate correctly caught — the partition's gate-vs-model
    // split (docs/CHAOS_MEASUREMENT_REDESIGN.md). `None` (→ null) in production:
    // the rejected draft can be the very confab the gate suppressed. Short-path
    // only — gate_longform never produces a clean abstention, so the split there
    // is vacuous. Moved into each diverging return below.
    let draft_for_meta: Option<String> = config::debug_enabled().then(|| draft.clone());
    // Active citation-grounding (entity-anchored fact queries, flag-gated).
    // Replaces generate-then-substring-verify with quote-then-answer: the model
    // must copy a verbatim supporting sentence before it answers, which forces
    // it to read the retrieved context (curing the measured A3B confabulations
    // where the answer was present but unused — "blowpipe" for the carving
    // knife) and grounds by quote-existence rather than value-substring (curing
    // the STOP-list/paraphrase false-negatives that killed "Chief Inspector").
    // Inconclusive (extraction error/unparseable) falls through to the legacy
    // ladder — fail-open, never a refusal from a hiccup. Does not consume the
    // draft, so the fall-through path is unchanged.
    if config::citation_grounding_enabled() && (entity_anchored || config::citation_broad_enabled())
    {
        // G4 — the quote-then-answer path. ECONOMY §4.1 labels it FUNCTION,
        // WRONG TIER: it buys adherence by spending output tokens on a
        // rehearsal, which is a prompt-shaped surrogate for a decode-time
        // constraint. Incumbent tier either way, and it is recorded whether
        // it grounds or falls through, because both cost the same call.
        //
        // THIS ROW EXISTS BECAUSE THE STRIP CAUGHT ITS OWN OMISSION. On the
        // first `citation_grounded` turn measured (2026-08-12) this path was
        // uninstrumented, so 11.08s of gate work landed in the
        // `gate_unattributed` residual and the turn rendered as
        // "no grounding stack ran". That is the defect-detection property
        // working as designed — and the fix is a row, not a smaller residual.
        let citation_started = std::time::Instant::now();
        let citation_outcome = citation::citation_grounded_answer(
            &**inference,
            question,
            chunks,
            locators,
            targets,
            crate::slot_policy::posture_of(base_request),
        )
        .await;
        crate::runtime::stage_ledger::Stage::new(
            sovereign_contracts::types::StageId::Citation,
            sovereign_contracts::types::StackOwner::Incumbent,
        )
        .cause(sovereign_contracts::types::StageCause::EveryTurn)
        .calls(1)
        .record(citation_started.elapsed().as_millis() as u64);
        if let citation::CitationOutcome::Grounded { answer, quotes } = citation_outcome {
            let quote_chars: usize = quotes.iter().map(|q| q.text.len()).sum();
            let located = quotes.iter().filter(|q| q.locator.is_some()).count();
            // The released passages as STRUCTURED rows, so a reading surface
            // can open the one the reader clicked. Until now the gate's
            // citation existed downstream only as prose inside the answer
            // string — which is why the system's best-attested citation, the
            // verbatim gate-verified one, was the only citation in the product
            // a user could not click.
            //
            // A quote with no target is DROPPED rather than emitted with a
            // null handle: a row here is a promise that clicking it opens the
            // passage quoted, and a row that cannot keep that promise is worse
            // than no row (§18.3 — absence is reported, never defaulted). The
            // prose rendering below is unchanged and still shows every quote,
            // so nothing disappears from what the reader can READ.
            // The turn, in kernel vocabulary (rung nc-20-turn-adoption).
            //
            // The seal is the leaf view — what this ladder is allowed to quote.
            // Each released quote is minted through
            // `kernel_types::Citation::pointing_into`, the ONE door: it refuses
            // a quote the seal does not hold verbatim, and refuses one landing
            // in material that may not be quoted. Both rules held here before,
            // as an upstream guarantee stated in three doc comments; they are
            // now a constructor, so no future quote path can skip either.
            //
            // BEHAVIOUR IS UNCHANGED, and that was checked rather than assumed:
            // a `GroundedQuote` carrying `Some(target)` is already one
            // contiguous run of ONE chunk (`QuoteMatch::Exact`), and seal
            // membership is exactly "has a `(corpus, chunk)` handle" — the same
            // predicate the old `target.clone()?` fold applied. What is new is
            // that a drop is a NAMED value carrying the quote and the seal size
            // instead of a `None` vanishing inside a `filter_map`.
            let seal = sealed::SealedEvidence::over(chunks, targets, custodies, grains);
            let mut turn_citations: Vec<kernel_types::Citation> = Vec::new();
            // Human section headings, index-parallel to `turn_citations` — the
            // display half of a citation, which the kernel `Origin` deliberately
            // does not carry (its `Locator` is the machine handle).
            let mut headings: Vec<Option<String>> = Vec::new();
            let mut refusals: Vec<kernel_types::Refused> = Vec::new();
            for q in &quotes {
                // No handle => no seal member => no row, exactly as the
                // `target.clone()?` fold decided before. Counted as a refusal
                // so the trace below distinguishes it from a quote the member
                // did not hold.
                let Some(target) = q.target.as_ref() else {
                    refusals.push(kernel_types::Refused::NotInSeal {
                        quote: q.text.clone(),
                        sealed_len: 0,
                    });
                    continue;
                };
                match seal.cite(target, q.text.as_str()) {
                    Ok(c) => {
                        turn_citations.push(c);
                        headings.push(q.locator.clone());
                    }
                    Err(r) => refusals.push(r),
                }
            }
            tracing::debug!(
                target: "grounding.seal",
                sealed = seal.len(),
                unhandled = seal.unhandled(),
                quotes = quotes.len(),
                cited = turn_citations.len(),
                refused = refusals.len(),
                why = ?refusals.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "citation release: quotes checked against the sealed leaf view"
            );
            dbg(&format!(
                "citation: GROUNDED → release (answer={:?} quotes={} located={located}/{} \
                 quote_chars={quote_chars})",
                answer.chars().take(60).collect::<String>(),
                quotes.len(),
                quotes.len()
            ));
            // Release the grounded value WITH its supporting quote as a
            // citation: glassbox (the user sees the exact sentence that grounds
            // the answer) AND a bare value ("the Doctor") is otherwise mis-read
            // as an abstention by the downstream answer/abstain classifier, which
            // wants a fuller response. The terse `answer` is what was verified
            // against the quote.
            //
            // EACH quote gets its OWN `"..."` span. The post-hoc
            // `quote_verification` pass re-checks a quoted span as one
            // contiguous source substring, so joining two verbatim sentences
            // inside one pair of quotes makes a correct citation fail that
            // re-check and ship as `[unverified excerpt: ...]` — measured on the
            // first arm-C run, 2026-08-05, where it hid a genuinely grounded
            // two-part answer behind an "unverified" label.
            //
            // The locator goes OUTSIDE the quote marks. That same re-check
            // reads whatever sits between the quotes as source text, so a
            // heading placed inside them would be read as part of the quote
            // and fail verbatim verification — the label would break the
            // citation it was added to explain.
            //
            // A quote with no locator renders exactly as it always did. The
            // corpus may have no section structure at all, or an unjoined
            // manifest, or the quote may have matched only across a chunk
            // boundary, or only as a partial run inside one; none of those
            // licence inventing a chapter.
            //
            // The renderer does NOT have to ask whether the post-hoc
            // `quote_verification` pass will demote a span before labelling it:
            // `GroundedQuote` guarantees a `Some(locator)` is source text
            // copied out of a single chunk, which that pass cannot demote. The
            // guarantee is upstream and structural, because a check here would
            // be a second decider re-deriving the first's verdict
            // (ARCH_PRINCIPLES §10.6). Measured 2026-08-05, before that
            // guarantee existed: a run-only match shipped as
            // `CHAPTER III — [unverified excerpt: …]`.
            let rendered = quotes
                .iter()
                .map(|q| {
                    let text = format!(
                        "\"{}\"",
                        q.text
                            .chars()
                            .take(CITATION_QUOTE_DISPLAY_CHARS)
                            .collect::<String>()
                    );
                    match &q.locator {
                        Some(loc) => format!("{loc} — {text}"),
                        None => text,
                    }
                })
                .collect::<Vec<_>>()
                .join("\n  ");
            let cited = format!("{answer}\n\nGrounded in the source:\n  {rendered}");
            // Second-opinion fabrication guard: the citation path grounds the
            // asserted VALUE against a quote, but a confabulated quote wearing a
            // real-passage shape can still slip a fabricated named entity
            // through (measured: "David Hart, COO of Knowledge Process Software"
            // over Enron evidence). Scan the asserted answer holistically; on a
            // flag, correct-or-abstain instead of releasing the fabrication.
            if let Some(guarded) = short_specifics_guard(
                inference,
                question,
                &answer,
                chunks,
                evidence.searcher.as_ref(),
                base_request,
                profile,
                native,
            )
            .await
            {
                return guarded;
            }
            // ── Release (rung nc-20-turn-adoption) ───────────────────────────
            //
            // The composed text becomes a `Draft`, whose text CANNOT BE READ,
            // and the only exit from a `Draft` is a release that says what is
            // known about it. This turn's verdict is a pass and it is one the
            // gate genuinely established: every released quote was re-checked
            // verbatim against the seal three lines up. The fold is
            // `Judgement::roll_up` inside `Draft::release` — one reducer, not a
            // second one written here (ARCH §10.6).
            //
            // Nothing about the released STRING changes: `Answer::text` is the
            // `cited` value that used to be assigned to `GateOutcome::text`
            // directly. What changes is that it can no longer be assigned
            // WITHOUT a judgement, because there is no other door.
            let verdict_reason = kernel_types::Reason::new(format!(
                "{} of {} released quote(s) verified verbatim against {} sealed chunk(s)",
                turn_citations.len(),
                quotes.len(),
                seal.len()
            ))
            .unwrap_or_else(|| kernel_types::Reason::literal("quotes verified against the seal"));
            // Through the same door every other exit uses. This site was
            // already correct before rung 9.2 and was the only one that was;
            // routing it through `release_held` too is what makes "one
            // decider" true rather than "one decider plus the original"
            // (ARCH §10.6).
            let released: kernel_types::Answer = release_held(
                cited,
                turn_citations,
                inference,
                base_request.preferred_speed,
                verdict_reason.to_string(),
            );
            // The wire rows are PROJECTED from the released answer rather than
            // assembled beside it: one decider for "what did this turn cite"
            // (ARCH §10.6). Before this, `meta["citations"]` and the answer's
            // own citations were two hand-built lists that happened to agree.
            let released_citations =
                crate::types::EpistemicState::citations_of(&released, &headings);
            let openable = released_citations.len();
            tracing::debug!(
                target: "grounding.seal",
                verdict = %released.judgement().verdict(),
                citations = released.citations().len(),
                openable,
                custody = ?released.evidence_custody().map(|c| c.as_str()),
                "citation release: answer sealed with its judgement"
            );
            return GateOutcome {
                // Was `released.text().to_string()` — the `Answer` was built
                // correctly here and then thrown away on the next line, which
                // is what made this the only judged exit of sixteen.
                answer: released,
                meta: with_native_verdict(
                    serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "citation_grounded",
                    "retried": false,
                    "mode": "citation",
                    "quote_chars": quote_chars,
                    "quotes": quotes.len(),
                    // How many released quotes carry a section locator. The
                    // gate already knew this — it was computed for a `dbg`
                    // line above and then dropped, so "did this answer tell
                    // the reader WHERE to look" existed only as prose in the
                    // released text and as a debug string.
                    //
                    // That gap is load-bearing, not cosmetic. The situated
                    // bench's `cites_a_source` criterion was reduced to
                    // re-deriving it with an LLM judge reading the answer, and
                    // measured 5/7 against this count's 7/7 (2026-08-05): it
                    // credited the one answer that did NOT disclose a gap and
                    // declined both that did, because a trailing "the passages
                    // do not answer X" distracted it. A fact the system
                    // computes must not be re-litigated downstream by a weaker
                    // decider (§10.6) — so it ships here, deterministically.
                    //
                    // `located <= quotes`, and `located == 0` is a legitimate
                    // reading, not a failure: a corpus with no section
                    // structure, an unjoined manifest, or a quote that matched
                    // only across chunks all release with no locator by design
                    // (see `gate_evidence_locators`).
                    "located": located,
                    // The openable passages, in release order. Rides the meta
                    // blob because the epistemic assembler already receives it
                    // — no new parameter through the handler chain for data
                    // the gate has already finished computing.
                    //
                    // `openable <= quotes`, and it is INDEPENDENT of `located`
                    // in both directions: a corpus with no section structure
                    // yields openable quotes with no chapter name, and a
                    // synthetic chunk yields the reverse. Reading either as a
                    // proxy for the other would misreport both.
                    "citations": released_citations,
                    "openable": openable,
                    "draft": draft_for_meta,
                    }),
                    native,
                ),
                claims: vec![GateClaim {
                    text: answer,
                    supported: true,
                    failed_once: false,
                    unjudged: false,
                    violation_prob: None,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                }],
            };
        }
        // Not tightly grounded (no quote, quote not verbatim, or answer not
        // supported by its own quote) → fall through to the legacy verify →
        // retry → abstain ladder. Citation is purely ADDITIVE: it can only
        // upgrade a legacy abstention into a grounded release — it never causes
        // an abstention itself nor replaces a correct draft, so the legacy path
        // stays the honesty floor (measured 1.00).
        dbg("citation: not tightly grounded → fall through to legacy ladder");
    }
    let mut text = draft;
    let mut action = ACT_RELEASED;
    let mut retried = false;
    let mut final_vp: Option<f64> = None;
    // Why `final_vp` is what it is. A vp of 0.0 from a path the gate
    // never ran (long-form out-of-scope, no input) is NOT a pass —
    // without this the UI rendered it as `Supported` (ARCH §18.1).
    let mut final_outcome: Option<judge::ClaimCheckOutcome> = None;
    // Whether the short path actually extracted and judged a claim —
    // gates the ClaimCheckComplete frame (a NO_CLAIM release audited
    // nothing, so reporting "1 claim confirmed" would be a lie).
    let mut claim_audited = false;
    // Retained per-claim record for the epistemic ledger (at most one
    // on the single-claim path). Mirrors the narration frames but
    // SURVIVES the return — the frames are transient by design.
    let mut gate_claims: Vec<GateClaim> = Vec::new();
    dbg(&format!(
        "gate_answer entity_anchored={entity_anchored} chunks={} draft={:?}",
        chunks.len(),
        text.chars().take(240).collect::<String>()
    ));
    // Structural exemption-closing: strip any GK caveat before extraction so the
    // asserted claim is actually verified rather than exempted as NO_CLAIM. This
    // runs UNCONDITIONALLY now (not just entity_anchored): gate_answer only fires
    // on the gated path, where documents WERE retrieved (gate_on requires
    // documents_found > 0), so the grounding contract applies — a "from general
    // knowledge" escape hatch must not ship confident specifics the retrieved
    // evidence can't support (observed 2026-07-01: "Winnie's former lover was
    // Eddie Henderson", a name absent from the Secret-Agent corpus, shipped under
    // a GK caveat on a non-entity-anchored question). If the GK content is
    // genuinely unsupported the gate now abstains — an honest "the sources don't
    // cover it" beats a labelled-but-confident fabrication. strip_gk_caveat is a
    // no-op when there is no caveat, so grounded answers are unaffected. The
    // released `text` is unchanged; only what the verifier reads is de-caveated.
    // Env-gated (SOVEREIGN_EXACTVAL_FIX=0 restores the prior entity_anchored-only
    // strip) for a clean replay A/B.
    let verify_text = if entity_anchored || config::exactval_fix_enabled() {
        strip_gk_caveat(&text)
    } else {
        text.clone()
    };
    // G4 — the short path's assurance stage. Two-stage generative critic:
    // incumbent tier by construction (ECONOMY §4.1 labels it FUNCTION,
    // WRONG TIER).
    let verify_started = std::time::Instant::now();
    let verify_outcome = verify_grounding(
        inference,
        question,
        &verify_text,
        chunks,
        entity_anchored,
        evidence.searcher.as_ref(),
        crate::slot_policy::posture_of(base_request),
    )
    .await;
    crate::runtime::stage_ledger::Stage::new(
        sovereign_contracts::types::StageId::Verify,
        sovereign_contracts::types::StackOwner::Incumbent,
    )
    .mechanism(sovereign_contracts::types::StageMechanism::PerClaimJudge)
    .cause(sovereign_contracts::types::StageCause::EveryTurn)
    .record(verify_started.elapsed().as_millis() as u64);
    match verify_outcome {
        Some(v) => {
            final_vp = Some(v.violation_prob);
            final_outcome = Some(v.outcome);
            dbg(&format!(
                "  verify: vp={:.3} tau={tau} claim={:?}",
                v.violation_prob,
                v.claim
                    .as_deref()
                    .map(|c| c.chars().take(70).collect::<String>())
            ));
            // Short path audits one central claim — surface it and its
            // verdict on the progress channel (extraction + judging is
            // one verify call here, so the frames land together).
            if let Some(c) = v.claim.as_deref() {
                claim_audited = true;
                gate_claims.push(GateClaim {
                    text: c.to_string(),
                    supported: v.violation_prob < tau,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                    failed_once: v.violation_prob >= tau,
                    unjudged: false,
                    violation_prob: Some(v.violation_prob),
                });
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimCheckStart {
                        claims: vec![wire_claim(c)],
                        recheck: false,
                    },
                );
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimVerdict {
                        index: 0,
                        supported: v.violation_prob < tau,
                    },
                );
            }
            if v.violation_prob >= tau {
                if let Some(claim) = v.claim.clone() {
                    if !profile.retry {
                        // Verify-only surfaces (Refinement): no second
                        // synthesis — the caller decides what replaces
                        // the failed text (typically: keep the prior
                        // verified answer).
                        text = grounded_abstention(&claim, chunks.len().min(12));
                        action = ACT_ABSTAINED_NO_RETRY;
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimCheckComplete {
                                confirmed: 0,
                                flagged: 1,
                            },
                        );
                        return GateOutcome {
                            answer: release_as(
                                action,
                                text,
                                Vec::new(),
                                inference,
                                base_request.preferred_speed,
                            ),
                            meta: with_native_verdict(
                                serde_json::json!({
                                                "surface": profile.surface.id(),
                                                "action": action.id,
                                                "retried": false,
                                                "violation_prob": final_vp,
                                "claim_check_outcome": final_outcome,
                                                "threshold": tau,
                                                "mode": "single_claim",
                                                "draft": draft_for_meta,
                                            }),
                                native,
                            ),
                            claims: gate_claims,
                        };
                    }
                    // Env-gated retry floor: the retry below is a SECOND full
                    // 35B synthesis, justified only when the evidence could
                    // ground a better answer (the good-evidence-but-bad-draft
                    // case the retry exists for). When the best retrieval
                    // similarity is below the floor, the evidence can't ground an
                    // answer — the retry would near-certainly fail again after
                    // paying for it (the observed 50-160s slow-abstention) — so
                    // abstain now. This never changes the answer/abstain DECISION
                    // on a turn the gate already failed; it only skips a wasted
                    // retry (gates COST, not competence), so it can't trigger the
                    // Critic-as-gate over-abstain regression. Default-off no-op.
                    if let (Some(floor), Some(sim)) = (retry_floor_env(), evidence.top_similarity) {
                        if sim < floor {
                            tracing::info!(
                                target: "grounding_gate",
                                top_similarity = sim,
                                retry_floor = floor,
                                vp = v.violation_prob,
                                "grounding gate: retry skipped — evidence below retry floor, abstaining without a second synthesis"
                            );
                            text = grounded_abstention(&claim, chunks.len().min(12));
                            action = ACT_ABSTAINED_WEAK_EVIDENCE;
                            emit_gate_progress(
                                progress,
                                NarrationPhase::ClaimCheckComplete {
                                    confirmed: 0,
                                    flagged: 1,
                                },
                            );
                            return GateOutcome {
                                answer: release_as(
                                    action,
                                    text,
                                    Vec::new(),
                                    inference,
                                    base_request.preferred_speed,
                                ),
                                meta: with_native_verdict(
                                    serde_json::json!({
                                                        "surface": profile.surface.id(),
                                                        "action": action.id,
                                                        "retried": false,
                                                        "violation_prob": final_vp,
                                    "claim_check_outcome": final_outcome,
                                                        "threshold": tau,
                                                        "top_similarity": sim,
                                                        "retry_floor": floor,
                                                        "mode": "single_claim",
                                                        "draft": draft_for_meta,
                                                    }),
                                    native,
                                ),
                                claims: gate_claims,
                            };
                        }
                    }
                    retried = true;
                    // G4 — the retry ladder. ECONOMY §4.1: INCUMBENCY, no
                    // grounding function; it is the control loop of the
                    // rewrite. Clocked from here through the re-verify.
                    let retry_started = std::time::Instant::now();
                    emit_gate_progress(progress, NarrationPhase::ClaimRevisionStart { failed: 1 });
                    let mut retry_req = base_request.clone();
                    let base_sys = retry_req.system_message.clone().unwrap_or_default();
                    retry_req.system_message = Some(format!(
                        "{base_sys}{}",
                        retry_system_note(&claim, &v.claim_evidence)
                    ));
                    retry_req.assistant_prefix = None;
                    match gate_call(
                        &**inference,
                        &retry_req,
                        sovereign_contracts::types::GateCallMechanism::Retry,
                    )
                    .await
                    {
                        Ok(resp) => {
                            // Truncation trace (2026-06-30): the gate's non-streaming
                            // retry bypasses the synth.truncation glassbox — log its
                            // finish vs cap so a silent Length cut here is visible.
                            tracing::info!(
                                target: "gate.call",
                                kind = "retry",
                                finish = ?resp.finish_reason,
                                completion_tokens = ?resp.completion_tokens,
                                max_tokens = ?retry_req.max_tokens,
                                resp_chars = resp.text.chars().count(),
                                "gate internal completion"
                            );
                            let second = resp.text;
                            // Same structural strip on the retry, matching the
                            // first-pass strip above (env-gated): the documented
                            // leak is a retry that re-asserts the fabrication
                            // wearing the GK caveat and slips the exemption.
                            let verify_second = if entity_anchored || config::exactval_fix_enabled()
                            {
                                strip_gk_caveat(&second)
                            } else {
                                second.clone()
                            };
                            emit_gate_progress(
                                progress,
                                NarrationPhase::ClaimCheckStart {
                                    claims: vec![wire_claim(&claim)],
                                    recheck: true,
                                },
                            );
                            let reverify_outcome = verify_grounding(
                                inference,
                                question,
                                &verify_second,
                                chunks,
                                entity_anchored,
                                evidence.searcher.as_ref(),
                                crate::slot_policy::posture_of(base_request),
                            )
                            .await;
                            // The retry pass (re-synthesis + its re-verify)
                            // is done. One row: it is one mechanism, and the
                            // re-verify exists only because the retry ran.
                            crate::runtime::stage_ledger::Stage::new(
                                sovereign_contracts::types::StageId::Retry,
                                sovereign_contracts::types::StackOwner::Incumbent,
                            )
                            .cause(sovereign_contracts::types::StageCause::ViolationOverThreshold)
                            .calls(2)
                            .record(retry_started.elapsed().as_millis() as u64);
                            match reverify_outcome {
                                Some(v2) if v2.violation_prob < tau => {
                                    final_vp = Some(v2.violation_prob);
                                    final_outcome = Some(v2.outcome);
                                    if v2.claim.is_none() && released_pure_decline(&second) {
                                        // The retry asserted NOTHING — a pure
                                        // decline extracted as NO_CLAIM (vp=0).
                                        // Releasing it "supported" forges a
                                        // Verified holding for a claim the
                                        // final text no longer asserts
                                        // (observed: ood-table-salt shipped
                                        // verdict `grounded` on "I don't have
                                        // reliable information on this.",
                                        // 2026-07-20). A 0-assertion decline
                                        // is an abstention — same contract as
                                        // the NO_CLAIM decline guard below.
                                        text = second;
                                        action = ACT_ABSTAINED_DECLINE;
                                        emit_gate_progress(
                                            progress,
                                            NarrationPhase::ClaimVerdict {
                                                index: 0,
                                                supported: false,
                                            },
                                        );
                                    } else {
                                        text = second;
                                        action = ACT_RETRY_RELEASED;
                                        if let Some(rec) = gate_claims.first_mut() {
                                            rec.supported = true;
                                            rec.violation_prob = Some(v2.violation_prob);
                                        }
                                        emit_gate_progress(
                                            progress,
                                            NarrationPhase::ClaimVerdict {
                                                index: 0,
                                                supported: true,
                                            },
                                        );
                                    }
                                }
                                Some(v2) => {
                                    final_vp = Some(v2.violation_prob);
                                    final_outcome = Some(v2.outcome);
                                    text = grounded_abstention(&claim, chunks.len().min(12));
                                    action = ACT_ABSTAINED;
                                    if let Some(rec) = gate_claims.first_mut() {
                                        rec.violation_prob = Some(v2.violation_prob);
                                    }
                                    emit_gate_progress(
                                        progress,
                                        NarrationPhase::ClaimVerdict {
                                            index: 0,
                                            supported: false,
                                        },
                                    );
                                }
                                None => {
                                    // Retry verdict unavailable — fail open
                                    // on the retry (written under the
                                    // grounding constraint; safer than
                                    // draft 1). Unless the retry is a pure
                                    // decline: nothing is asserted, so there
                                    // is nothing to fail open ON — it's an
                                    // abstention (same contract as above).
                                    text = second;
                                    if released_pure_decline(&text) {
                                        action = ACT_ABSTAINED_DECLINE;
                                    } else {
                                        action = ACT_RETRY_RELEASED_UNVERIFIED;
                                    }
                                    if let Some(rec) = gate_claims.first_mut() {
                                        rec.violation_prob = None;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "grounding_gate",
                                error = %e,
                                "gated retry synthesis failed — releasing abstention"
                            );
                            text = grounded_abstention(&claim, chunks.len().min(12));
                            action = ACT_ABSTAINED_RETRY_ERROR;
                        }
                    }
                }
            }
        }
        None => {
            action = ACT_JUDGE_FAILED_OPEN;
        }
    }
    // Terminal progress frame for the short path. Only when a claim
    // was actually audited (NO_CLAIM releases verified nothing) and
    // only on the verdicts this fall-through exit owns — the abstain
    // early-returns above emit their own completion frames.
    if claim_audited {
        // Reads the action's REACH rather than its spelling. The old form
        // matched four string arms and a `starts_with` prefix — §2.1's smell,
        // and a fifth action id would have fallen into `_ => (0, 0)` silently.
        let (confirmed, flagged) = match action.reach {
            GateReach::Held if action.id.starts_with("retry_") => (1, 1),
            GateReach::Held => (1, 0),
            GateReach::Unjudged if action.id.starts_with("retry_") => (1, 1),
            GateReach::Declined => (0, 1),
            GateReach::Flawed | GateReach::Unjudged => (0, 0),
        };
        if confirmed + flagged > 0 {
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete { confirmed, flagged },
            );
        }
    }
    dbg(&format!(
        "verdict action={} retried={retried} vp={final_vp:?} tau={tau}",
        action.id
    ));
    tracing::info!(
        target: "grounding_gate",
        action = action.id,
        retried,
        vp = ?final_vp,
        tau,
        top_similarity = ?evidence.top_similarity,
        "grounding gate verdict"
    );
    // Fragment guard (gen75c: the answer to a three-variable code question was
    // the single word "Start", released via NO_CLAIM — a fragment extracts no
    // claim, so the verify ladder waves it through). A released answer this
    // short, with no grounding suffix and no decline shape, answers nothing:
    // convert it to the honest abstention instead of shipping noise. Terse
    // GROUNDED answers are unaffected — the citation path formats them with
    // their supporting quote, well past this floor.
    if action == ACT_RELEASED
        && text.trim().chars().count() < 15
        && !text.contains("Grounded in the source")
        && question.trim().chars().count() > 40
    {
        dbg(&format!(
            "fragment guard: released text {:?} answers nothing — abstaining",
            text.trim()
        ));
        return GateOutcome {
            answer: abstain(
                grounded_abstention(question, chunks.len().min(12)),
                inference,
                base_request.preferred_speed,
                "released text answers nothing — fragment guard".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "abstained_fragment",
                    "retried": retried,
                    "violation_prob": final_vp,
                    "claim_check_outcome": final_outcome,
                    "threshold": tau,
                    "mode": "single_claim",
                    "draft": draft_for_meta,
                }),
                native,
            ),
            claims: gate_claims,
        };
    }
    // Decline guard (EPISTEMIC_STATE P0, 2026-07-20): a NO_CLAIM release whose
    // text is a pure provenance-flagged decline asserts nothing — it IS an
    // abstention, and releasing it ships the wrong epistemic standing
    // downstream (verdict `Unverified` instead of `CannotKnowFromHere`; the
    // coverage probe never fires; the gap mis-routes as ClaimUncovered —
    // observed on `ood-australia-capital` over 10 retrieved distractors).
    // Reclassify the ACTION only: the model's own decline prose is already the
    // honest user-facing abstention, so the text ships unchanged. Caveated
    // parametric answers are excluded by `released_pure_decline`; audited
    // claims (`claim_audited`) exclude every turn that asserted something.
    //
    // P1 (`NATIVE_GROUNDING_PARITY_PLAN.md` §4.1): the zoo decides this on
    // BOTH arms. H1's verdict rides the turn as telemetry and is reported
    // beside the decision, never in place of it — see `abstention_action`
    // for why the typed shortcut was retired and when it comes back.
    let reclassify = (action == ACT_RELEASED && !claim_audited)
        .then(|| abstention_action(&text))
        .flatten();
    if let Some(reclassified) = reclassify {
        dbg("decline guard: NO_CLAIM release is a pure decline — reclassifying as abstention");
        tracing::info!(
            target: "grounding_gate",
            gate_action = reclassified,
            // What H1 scored, when it ran. `None` on every flag-off turn.
            // Named `native_*` so no reader can mistake it for the decider.
            native_answerability = native.map(|v| v.answerability),
            native_decision = native.map(|v| v.to_gate_action()),
            "grounding gate: released text is a 0-holding decline — action reclassified to abstained_decline"
        );
        return GateOutcome {
            answer: abstain(
                text,
                inference,
                base_request.preferred_speed,
                "released text is a 0-holding decline".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": reclassified,
                    "retried": retried,
                    "violation_prob": final_vp,
                    "claim_check_outcome": final_outcome,
                    "threshold": tau,
                    "mode": "single_claim",
                    "draft": draft_for_meta,
                }),
                // This site carried the pair inline from 2026-07-20 — the
                // seed for `with_native_verdict`, now one of its callers.
                // Absent turns changed shape here (`null` → `not_computed`);
                // the reason is in that constant's docs.
                native,
            ),
            claims: gate_claims,
        };
    }
    // Second-opinion fabrication guard on a RELEASED single-claim answer — the
    // per-claim verify grounds the load-bearing value but is blind to fabricated
    // SUPPORTING specifics (a cited flag/number/entity absent from the
    // evidence). Skip when the path already abstained (nothing asserted). On a
    // flag: correct-or-abstain via one grounded rewrite.
    // Skip when the gate did not release an asserted answer — the verdict is
    // read off the action rather than re-derived from its spelling.
    if matches!(action.reach, GateReach::Held | GateReach::Flawed) {
        if let Some(guarded) = short_specifics_guard(
            inference,
            question,
            &text,
            chunks,
            evidence.searcher.as_ref(),
            base_request,
            profile,
            native,
        )
        .await
        {
            return guarded;
        }
    }
    GateOutcome {
        answer: release_as(
            action,
            text,
            Vec::new(),
            inference,
            base_request.preferred_speed,
        ),
        meta: with_native_verdict(
            serde_json::json!({
                "surface": profile.surface.id(),
                "action": action.id,
                "retried": retried,
                "violation_prob": final_vp,
                    "claim_check_outcome": final_outcome,
                "threshold": tau,
                "mode": "single_claim",
                "draft": draft_for_meta,
            }),
            native,
        ),
        claims: gate_claims,
    }
}

/// Decode-committed opening for the long-form rewrite. Instruction-only
/// shape rules measured non-compliant (v14: the rewrite still led with
/// "I do not have access to passages detailing…" despite an explicit
/// "do not open with what the passages lack" rule — same ~60%
/// instruction-wall as the GK caveat). Committing the opening forces
/// the rewrite to continue into the supported account; the abstain
/// read of a disclaimer-led head disappears structurally. Like
/// GK_CAVEAT_PREFIX, assistant_prefix is decode-commit only — the
/// caller must prepend it to the returned text.
/// User-facing wording (grace audit 2026-07-11): the previous prefix
/// ("From the retrieved sources, here is what can be established:")
/// injected auditor-speak as the OPENING of every rewritten answer — a
/// structural jargon hit on the grace gate's `clean` component. The
/// prefix's decode-commit job (force continuation into the supported
/// account) needs no machinery reference.
pub const LONGFORM_REWRITE_PREFIX: &str = "Here's what I can say with confidence:\n\n";

/// Rewrite-request system note: every failed claim, each with the
/// passages its targeted corpus search returned (when any). The
/// correction material is the point — v13c/v14/v14b measured that a
/// rewrite told only WHICH assertions failed, with no passages
/// stating the truth, can only delete and disclaim.
fn rewrite_system_note(failed: &[FailedClaim]) -> String {
    const REWRITE_EVIDENCE_PER_CLAIM: usize = 2;
    const REWRITE_EVIDENCE_CHARS: usize = 700;
    let list = failed
        .iter()
        .map(|f| {
            let mut entry = format!("- \"{}\"", f.claim);
            if f.evidence.is_empty() {
                entry.push_str(
                    "\n  (no corpus passage states this — remove it, or say the \
                     sources do not establish it)",
                );
            } else {
                entry.push_str("\n  What the sources actually say on this point:");
                for p in f.evidence.iter().take(REWRITE_EVIDENCE_PER_CLAIM) {
                    let trimmed: String = p.chars().take(REWRITE_EVIDENCE_CHARS).collect();
                    entry.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
                }
            }
            entry
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\nGROUNDING AUDIT FAILED on your previous draft. These assertions did not \
         verify against the sources:\n{list}\n\
         Rewrite the answer: keep everything the sources support. For each failed \
         assertion that has corrective passages above, REPLACE it with what those \
         passages actually state, citing them — do not merely delete it. Never add \
         a NEW statement about what the sources say, cite, name, or omit unless a \
         passage above shows it. Structure \
         the rewrite as an ANSWER, not a disclaimer: open directly with the \
         supported account, organized to address the question. Do not open with \
         what the sources lack, and do not enumerate the removed assertions in the \
         body. If material gaps remain, note them briefly in a single short \
         paragraph at the end."
    )
}

/// The user-visible verification note. Items are answer spans / short claims
/// (`normalize_scan_item` reduces scan output toward answer wording); render
/// each one deduped and length-capped, in plain language — judge vocabulary
/// must never reach the user (observed 2026-07-01: raw scan chatter footnoted
/// a released answer with "… is a fabricated specific").
///
/// Items are deliberately UNQUOTED: the post-synthesis quote guardrail
/// (`quote_verification::verify_answer_against_turn_evidence`, streaming.rs) treats
/// any curly-quoted span as a quotation claim and demotes what it can't
/// verbatim-confirm — a quoted note item (a paraphrased claim, by nature not
/// verbatim) was rewritten to "[unverified excerpt: …]", turning the app's own
/// footer into a self-contradiction (probed 2026-07-01: the note trace showed
/// clean items; the released text showed them wrapped).
/// EXPERIMENT (`SOVEREIGN_NOTE_AS_METADATA=1`): keep the verification note
/// OUT of the answer text — the failed claims already ride
/// `GateOutcome.meta.failed_claims` → `metadata.grounding_gate`, and the
/// desktop renders them as a collapsible disclosure instead. Persona-QA
/// receipts (2026-07-11): the appended note owns the answer's final words
/// ("— The evidence states…", "[unverified excerpt:…]"), which zeroes the
/// grace gate's `agency`/`clean` components and buries the model's own
/// closing line — the honest audit trail read as auditor-speak in user
/// space. Default OFF: non-desktop surfaces (API/CLI) keep the in-text
/// note so a known-failed claim is never silently released without its
/// caveat (the never-silent invariant).
fn append_note(text: String, note: &str) -> String {
    let as_metadata = std::env::var("SOVEREIGN_NOTE_AS_METADATA")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if as_metadata {
        text
    } else {
        format!("{text}{note}")
    }
}

fn verification_note(failed_claims: &[String]) -> String {
    const NOTE_ITEM_CHARS: usize = 160;
    let mut seen = std::collections::HashSet::new();
    let items: Vec<String> = failed_claims
        .iter()
        .map(|c| {
            let c = unwrap_unverified_excerpts(c);
            let c = c.trim().trim_matches(['"', '“', '”']).trim();
            let mut item: String = c.chars().take(NOTE_ITEM_CHARS).collect();
            if c.chars().count() > NOTE_ITEM_CHARS {
                item.push('…');
            }
            item
        })
        .filter(|c| !c.is_empty() && seen.insert(c.to_lowercase()))
        .map(|c| format!("- {c}"))
        .collect();
    tracing::info!(
        target: "grounding_gate",
        n_claims = failed_claims.len(),
        n_items = items.len(),
        first_claim_head = %failed_claims.first().map(|c| c.chars().take(80).collect::<String>()).unwrap_or_default(),
        first_item_head = %items.first().map(|c| c.chars().take(80).collect::<String>()).unwrap_or_default(),
        "verification note rendered"
    );
    format!(
        "\n\n---\n*Verification note: these statements could not be confirmed \
         against your sources — treat them as unverified:*\n{}",
        items.join("\n")
    )
}

/// Cross-passage support check for ONE long-form claim: the top
/// passages are presented TOGETHER and the judge answers whether they
/// jointly state or imply the claim. Long-form synthesis legitimately
/// assembles claims across passages — per-chunk max-support is
/// structurally biased against exactly that (the bench critic's
/// documented blind spot; measured v13: a correct maximal essay was
/// rewritten into hedging because its synthesis claims had no single

/// Claim-audit budget for an answer of `chars` characters. Scales with
/// length so a long "exhaustive" answer — which buries fabricated specifics
/// in its later sections, past the first few load-bearing claims — gets
/// proportionate checking, instead of the fixed 4-claim audit that was
/// structurally blind to body fabrication (observed 2026-06-30: 3/5 shipped
/// fabrications were direct releases whose fabricated specifics were never
/// extracted). Floored at the surface's `min_claims` and capped so per-claim
/// judge latency stays bounded on very long answers.
pub(super) fn claim_budget(chars: usize, min_claims: usize) -> usize {
    // 600 chars/claim (not 900) so the empirical fabrication distribution
    // actually scales: the fixed-1h shipped fabrications sat at 3630-8571
    // chars, which at 900/claim only reached budget 4-9 (the 3630 case got
    // NO lift) — measured under-powered on the fab-fix run-1 trace. At 600 the
    // same cases get 6-10 claims audited. Cap 10 bounds the per-claim judge
    // latency on the longest answers (each audited claim is a 35B judge call).
    const MAX_AUDITED_CLAIMS: usize = 10;
    const CHARS_PER_CLAIM: usize = 600;
    (chars / CHARS_PER_CLAIM).clamp(min_claims, MAX_AUDITED_CLAIMS)
}

/// The share of a draft's audited claims that may fail and still be repaired by
/// targeted surgery rather than a full re-synthesis.
///
/// **0.5 is not a tuned number — it is what "most" means.** The rule this cap
/// implements has always been stated in prose at the call site: *"when MOST
/// claims fail the draft is fundamentally broken"*, so surgery is for the case
/// where the failures are a **minority**. A majority is more than half;
/// therefore the boundary is half. Nothing here was fitted to a latency
/// measurement, and it must not be — moving this constant to make a wall-time
/// number look better would re-create the defect it replaces (a threshold that
/// disagrees with its own rationale).
///
/// History: until 2026-08-13 the cap was an ABSOLUTE failure count (default 3,
/// `SOVEREIGN_SURGICAL_MAX_FAILURES`). `claim_budget` above scales the audited
/// claim count with answer length, so an absolute cap inverts with length: a
/// 10-claim longform answer with 4 failures (60% grounded) fell back to full
/// re-synthesis, while a 3-claim short answer with ALL THREE failing got
/// surgery. Targeted revision was structurally excluded from exactly the class
/// of answer it was built for. Measured cost of one such fallback on a real
/// desktop turn, 2026-08-12: 51.2s for the repair, against 1.7-2.7s when
/// surgery engaged on the same query (NATIVE_GROUNDING_ECONOMY.md §7.3.1).
///
/// DO NOT read that 51.2s as what this change recovered. Measured over 8 warm
/// desktop turns of the same query (§7.3.2), the ratio rule and the old
/// absolute rule disagree on 1 turn in 7, and on that turn `surgical_rewrite`
/// declined anyway because it must map EVERY failed claim or none
/// (`surgical.rs:240-252`). Net measured yield: 0 ms. This constant makes the
/// code agree with its own rationale; it did not make the query faster, and
/// the binding constraint on that query is the span resolver, not this cap.
const SURGICAL_MAX_FAILED_RATIO: f64 = 0.5;

/// The ONE place `SOVEREIGN_SURGICAL_MAX_FAILED_RATIO` is read. Unparseable or
/// out-of-range values fall back to the derived default rather than to an
/// arbitrary clamp, so a typo cannot silently widen or close the cap.
/// Declared in `quality/env-flags.toml`.
fn surgical_max_failed_ratio() -> f64 {
    parse_failed_ratio(
        std::env::var("SOVEREIGN_SURGICAL_MAX_FAILED_RATIO")
            .ok()
            .as_deref(),
    )
}

/// Pure so it is testable without mutating the process environment (which is
/// not sound under a parallel test runner).
fn parse_failed_ratio(raw: Option<&str>) -> f64 {
    let Some(raw) = raw else {
        return SURGICAL_MAX_FAILED_RATIO;
    };
    match raw.trim().parse::<f64>() {
        Ok(r) if (0.0..=1.0).contains(&r) => r,
        _ => {
            tracing::warn!(
                target: "grounding_gate",
                value = raw,
                default = SURGICAL_MAX_FAILED_RATIO,
                "SOVEREIGN_SURGICAL_MAX_FAILED_RATIO unparseable or outside 0.0..=1.0 — using default"
            );
            SURGICAL_MAX_FAILED_RATIO
        }
    }
}

/// The ONE decider for "may this draft be repaired surgically?".
///
/// `audited` is the number of claims the per-claim audit actually checked
/// (`claim_budget`), which is the population the "most claims" rule is about.
/// `failed` counts every unsupported finding on the draft and can legitimately
/// EXCEED `audited`: the specifics scan and the sentence-level identifier sweep
/// push synthetic failed claims that were never in the audited list. That is
/// not a bug in this predicate — a draft carrying more unsupported findings
/// than half its audited claims is exactly the "fundamentally broken" case, and
/// it declines, which is what should happen.
fn surgery_admits(failed: usize, audited: usize, max_failed_ratio: f64) -> bool {
    failed > 0 && audited > 0 && (failed as f64) <= (audited as f64) * max_failed_ratio
}

/// Whether the holistic supporting-specifics scan runs alongside the per-claim
/// audit in `gate_longform`. ON by default; `SOVEREIGN_SPECIFICS_SCAN=0`
/// disables it (the clean A/B lever — the per-claim audit alone is the prior
/// behaviour). The scan is one extra judge call per audited text; it catches
/// the fabricated specifics / misattributions the load-bearing claim extraction
/// structurally misses.
fn specifics_scan_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_SPECIFICS_SCAN").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// Whether the SHORT-path second-opinion specifics scan runs. SHELVED — OFF by
/// default; opt in with `SOVEREIGN_SHORT_SPECIFICS_SCAN=1`. The short-path
/// "fabrication" category it targets proved to be ~90% measurement artifact
/// (correctly-grounded answers mis-scored because the offline evidence was
/// truncated); once that capture bug was fixed the guard's live A/B was no
/// longer a meaningful composite lever, so it ships dormant as defense-in-depth
/// pending a fresh clean-evidence validation. Kept separate from
/// `SOVEREIGN_SPECIFICS_SCAN` (the long-form scan, ON) so each band is
/// independently switchable.
fn short_specifics_scan_enabled() -> bool {
    matches!(
        std::env::var("SOVEREIGN_SHORT_SPECIFICS_SCAN")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

/// True when a released short answer is itself an honest abstention / decline
/// ("the sources don't cover it", "I'm not certain", the `grounded_abstention`
/// prose). Such an answer asserts no verifiable value, so the specifics scan has
/// nothing to fabricate-check — running it only surfaces kind-(3) noise (the
/// scan second-guessing a correct "not in sources" as a false claim ABOUT the
/// evidence). Skipping is a latency optimisation and errs fail-open: a false
/// skip just preserves prior behaviour. Measured 2026-07-01: 6/7 short-band scan
/// flags on GOOD answers were exactly these honest abstentions.
pub fn answer_declines(text: &str) -> bool {
    let h = text.trim_start().to_lowercase();
    const DECLINES: &[&str] = &[
        "i don't have reliable information",
        "i do not have reliable information",
        "i am not certain",
        "i'm not certain",
        "i do not have information",
        "i don't have information",
        "couldn't confirm an answer", // grounded_abstention prose (current)
        "could not confirm an answer", // grounded_abstention prose (current)
        "none of them actually cover it", // grounded_abstention prose (legacy, still in-the-wild)
        "i'd rather not guess",       // grounded_abstention prose (legacy)
        "do not contain",
        "does not contain",
        "not recorded there",
        "the sources do not",
        "the sources don't",
        "sources do not contain",
        "no passage",
        "not in your sources",
    ];
    DECLINES.iter().any(|d| h.contains(d))
}

/// True when a NO_CLAIM release is a pure provenance-flagged decline — the
/// model saying "I don't have reliable information in my knowledge base"
/// over retrieved-but-useless evidence. Such a turn asserts nothing, so
/// releasing it as an answer mis-states the turn's epistemic standing: the
/// ledger derives `Unverified` (evidence present, nothing audited), the
/// coverage probe never runs (`gap_turn=false`), and a genuine knowledge
/// gap defaults to `ClaimUncovered` (bench/gap_check/DECISION.md, bug 2).
/// A 0-holding decline IS an abstention — reclassify the ACTION, keep the
/// model's own (honest, already provenance-flagged) prose.
///
/// Deliberately narrower than [`answer_declines`]: a caveated parametric
/// answer ("Not in your sources — from general knowledge: Canberra…")
/// declines-then-ANSWERS, and must keep releasing — so the caveat is
/// stripped first and any remaining "from general knowledge" pivot vetoes
/// the reclassification.
/// Did a claim-free release actually abstain?
///
/// ONE decider, on both arms of the native-grounding flag: the incumbent
/// 17-phrase zoo, which recovers the decision the system made but never
/// carried.
///
/// **Why the typed verdict is not consulted here.** It used to be: when
/// H1 had run, its `decision` supplied the action directly and the zoo was
/// skipped. P1 retired that (`NATIVE_GROUNDING_PARITY_PLAN.md` §4.1 —
/// admission is telemetry, "decisions traced, never enforced"), because
/// letting it stand made the flag change a turn's *action* in both
/// directions: a prose decline under a typed `Answer` stayed `released`
/// on the flag-on arm while flag-off reclassified it, and the epistemic
/// ledger, the collaboration surface and the honesty scorer all read that
/// string. That divergence is exactly what A1's arm-identity check
/// forbids, and A1 is the plan's pre-registered kill for the whole phase.
/// The typed path returns at P3c, when a verdict is enforced again by a
/// signal that earned it.
///
/// Returns the legacy action string to reclassify to, or `None` to leave
/// the action alone. Pure — no model, no env, no clock.
pub(crate) fn abstention_action(text: &str) -> Option<&'static str> {
    released_pure_decline(text).then_some("abstained_decline")
}

pub fn released_pure_decline(text: &str) -> bool {
    let stripped = strip_gk_caveat(text);
    if stripped.to_lowercase().contains("from general knowledge") {
        return false;
    }
    answer_declines(&stripped)
}

/// Second-opinion fabrication guard for the SHORT gate path (single-claim +
/// citation). Those paths verify the LOAD-BEARING value but are structurally
/// blind to fabricated SUPPORTING specifics — a named person/flag/number/quote
/// the answer cites to `[Source: …]` that is absent from the evidence (observed
/// 2026-07-01 on thin evidence: "David Hart, COO of Knowledge Process Software"
/// shipped by the citation path, and tokei "--files"/"--sort"/".tokeignore"
/// specifics padded onto a grounded top-line). Runs the holistic specifics scan
/// on an already-RELEASED short answer; on a flag it routes into ONE corrective
/// retry (each flagged specific re-searched so the rewrite has the truth) and
/// re-scans the result, abstaining only if the rewrite still fabricates.
///
/// Never a blunt abstention: a truly-grounded specific gets its passage back and
/// the rewrite keeps it (self-correcting away a false positive), and a
/// mostly-grounded answer with one bad specific is rewritten, not discarded.
/// Returns `None` to leave the release unchanged — disabled, no-retry surface,
/// abstention-shaped answer, judge failure, or a clean scan.
#[allow(clippy::too_many_arguments)]
async fn short_specifics_guard(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    released: &str,
    chunks: &[String],
    searcher: Option<&Arc<dyn SealedEvidenceSearch>>,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    // H1's verdict for the turn, threaded in for `with_native_verdict`
    // alone: this guard takes `chunks`, not the `EvidenceContext`, so the
    // two outcomes it can return had no way to report the instrument.
    // Read by nothing that decides — see `with_native_verdict`.
    native: Option<&crate::types::GroundingVerdict>,
) -> Option<GateOutcome> {
    // Only on retry-capable surfaces: the guard's whole remedy is a corrective
    // re-synthesis. Verify-only surfaces have no second synthesis to give.
    if !profile.retry {
        return None;
    }
    // Deterministic sentence sweep FIRST — receipt-grade hits (a code
    // identifier or in-world name attribution absent from the entire
    // evidence) trigger the corrective retry regardless of the LLM-scan flag.
    // gen75e s34: the `cmd_init`/`found.rs` ghosts shipped in a 1,504-char
    // answer — UNDER the 1,800 longform pivot, where none of the longform
    // vetoes run. The short path needs the same receipts-grade guard.
    let hay_lower = chunks.join(" ").to_lowercase();
    // Budget for the LLM scan paths (initial when the flag is on; the
    // post-retry re-scan always).
    let budget = claim_budget(released.chars().count(), 3);
    let mut swept: Vec<String> = Vec::new();
    for sentence in released.split(['.', '\n']) {
        let sentence = sentence.trim();
        if sentence.chars().count() < 20 {
            continue;
        }
        if let Some(x) = judge::absent_identifier_attribution(sentence, &hay_lower)
            .or_else(|| judge::absent_name_attribution(sentence, &hay_lower))
        {
            if !swept.contains(&x) {
                swept.push(x);
            }
        }
    }
    let specifics = if !swept.is_empty() {
        dbg(&format!(
            "short sweep VETOED {swept:?} (absent from evidence)"
        ));
        swept
            .iter()
            .map(|x| {
                format!("The answer references \"{x}\", which does not appear in the sources.")
            })
            .collect()
    } else {
        if !short_specifics_scan_enabled() {
            return None;
        }
        // Nothing asserted → nothing to fabricate-check (fail-open latency skip).
        if answer_declines(released) {
            return None;
        }
        // Small budget floored at 3 so even a terse citation answer ("David Hart")
        // gets a real check; scales modestly on longer short answers.
        let specifics = scan_unsupported_specifics(
            inference,
            question,
            released,
            chunks,
            &[],
            budget,
            crate::slot_policy::posture_of(base_request),
        )
        .await?;
        if specifics.is_empty() {
            return None; // clean — release unchanged
        }
        specifics
    };
    // Corrective evidence per flagged specific — the same material the long-form
    // rewrite gets, and the self-correction for a false positive (a real
    // specific's grounding passage comes back, so the rewrite keeps it).
    let mut corrective: Vec<String> = Vec::new();
    if let Some(s) = searcher {
        for spec in specifics.iter().take(4) {
            if let Some(hit) = s.search(spec).await.into_iter().next() {
                corrective.push(hit);
            }
        }
    }
    let joined = specifics.join("\"; \"");
    dbg(&format!(
        "short_specifics_guard: {} flagged specific(s) [{:?}] → corrective retry",
        specifics.len(),
        joined.chars().take(90).collect::<String>()
    ));
    let mut retry_req = base_request.clone();
    let base_sys = retry_req.system_message.clone().unwrap_or_default();
    retry_req.system_message = Some(format!(
        "{base_sys}{}",
        retry_system_note(&joined, &corrective)
    ));
    retry_req.assistant_prefix = None;
    let second = match gate_call(
        &**inference,
        &retry_req,
        sovereign_contracts::types::GateCallMechanism::ShortGuardRetry,
    )
    .await
    {
        Ok(r) => r.text,
        Err(e) => {
            tracing::warn!(
                target: "grounding_gate",
                error = %e,
                "short specifics guard retry failed — keeping prior release"
            );
            return None; // fail-open: keep the original release
        }
    };
    // Re-scan the rewrite. Still fabricating → abstain; clean → release the
    // corrected answer. A re-scan judge failure falls open to keep the rewrite
    // (written under the corrective note, no worse than the flagged draft).
    match scan_unsupported_specifics(
        inference,
        question,
        &second,
        chunks,
        &[],
        budget,
        crate::slot_policy::posture_of(base_request),
    )
    .await
    {
        Some(v) if !v.is_empty() => {
            tracing::info!(
                target: "grounding_gate",
                action = ACT_ABSTAINED_SPECIFICS.id,
                flagged = specifics.len(),
                "short specifics guard: rewrite still fabricates — abstaining"
            );
            let claims = specifics
                .iter()
                .map(|s| GateClaim {
                    text: s.clone(),
                    supported: false,
                    failed_once: true,
                    unjudged: false,
                    violation_prob: None,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                })
                .collect();
            Some(GateOutcome {
                answer: abstain(
                    grounded_abstention("", chunks.len().min(12)),
                    inference,
                    base_request.preferred_speed,
                    "second-opinion guard flagged fabricated specifics".to_string(),
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "abstained_specifics",
                        "retried": true,
                        "flagged_specifics": specifics,
                        "mode": "short_specifics",
                    }),
                    native,
                ),
                claims,
            })
        }
        _ => {
            tracing::info!(
                target: "grounding_gate",
                action = ACT_RETRY_RELEASED_SPECIFICS.id,
                flagged = specifics.len(),
                "short specifics guard: corrective rewrite released"
            );
            let claims = specifics
                .iter()
                .map(|s| GateClaim {
                    text: s.clone(),
                    supported: true,
                    failed_once: true,
                    unjudged: false,
                    violation_prob: None,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                })
                .collect();
            Some(GateOutcome {
                answer: release_as(
                    ACT_RETRY_RELEASED_SPECIFICS,
                    second,
                    Vec::new(),
                    inference,
                    base_request.preferred_speed,
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "retry_released_specifics",
                        "retried": true,
                        "flagged_specifics": specifics,
                        "mode": "short_specifics",
                    }),
                    native,
                ),
                claims,
            })
        }
    }
}

/// Fold a long-form audit's outcome into retained per-claim records
/// for the epistemic ledger: audited claims get their final verdict;
/// synthetic failures (specifics scan, sentence sweep) that never
/// appeared in the extracted list are appended as unsupported records.
fn longform_claims(
    audited: &[String],
    failed: &[FailedClaim],
    unjudged: &[String],
) -> Vec<GateClaim> {
    let mut out: Vec<GateClaim> = audited
        .iter()
        .map(|c| {
            let is_failed = failed.iter().any(|f| &f.claim == c);
            // A claim the judge never reached is neither supported nor
            // failed. It shipped because the ladder fails open per claim,
            // and the record must say so (ARCH §18.3): the ledger renders
            // it FailOpen, and the verdict projection treats the whole
            // check as could-not-judge.
            let is_unjudged = !is_failed && unjudged.iter().any(|u| u == c);
            GateClaim {
                text: c.clone(),
                supported: !is_failed && !is_unjudged,
                failed_once: is_failed,
                unjudged: is_unjudged,
                violation_prob: None,
                // Filled post-gate, display only (see GateClaim::address).
                address: None,
            }
        })
        .collect();
    for f in failed {
        if !audited.iter().any(|c| c == &f.claim) {
            out.push(GateClaim {
                text: f.claim.clone(),
                supported: false,
                failed_once: true,
                unjudged: false,
                violation_prob: None,
                // Filled post-gate, display only (see GateClaim::address).
                address: None,
            });
        }
    }
    out
}

/// Append one audit-forensics record when `SOVEREIGN_GATE_AUDIT_FORENSICS`
/// names a file (see `config::audit_forensics_path` for why it is off by
/// default). Synchronous and best-effort: this runs only on a deliberate
/// diagnostic run, and an IO failure there must be visible rather than
/// silently producing a short file that reads as "no failures" (ARCH §18.3).
fn audit_forensics(record: &serde_json::Value) {
    let Some(path) = config::audit_forensics_path() else {
        return;
    };
    use std::io::Write;
    let line = match serde_json::to_string(record) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "audit forensics record not serialisable");
            return;
        }
    };
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    match opened {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                tracing::warn!(target: "grounding_gate", error = %e, path = %path.display(), "audit forensics append failed");
            }
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, path = %path.display(), "audit forensics file not writable");
        }
    }
}

/// The audit window: how many leaf chunks each claim's judge prompt carries.
///
/// **Derived, never a constant** (ARCH §18.6). The auditor is shown what the
/// drafter was shown, so the bound IS the retrieved leaf set — the drafter's
/// evidence already passed `prompt_budget::enforce` for this turn's context
/// window, and a judge prompt is strictly smaller than the drafter's. There is
/// no separate number to choose, and reintroducing one (the removed
/// `profile.max_chunks = 8`) silently narrows the auditor's view below the
/// drafter's without any surface saying so.
///
/// `max(1)` only guards the empty case: a claim loop over zero evidence still
/// needs a non-zero take() bound.
fn audit_window(leaf_chunk_count: usize) -> usize {
    leaf_chunk_count.max(1)
}

/// Long-form ladder: per-claim audit → one rewrite → annotate.
/// An essay with one bad claim is REWRITTEN, not abstained; if the
/// rewrite still carries unsupported claims, they are listed in a
/// visible verification note appended to the answer — the reader sees
/// exactly which assertions didn't verify, instead of either losing
/// the whole essay or trusting it blind.
#[allow(clippy::too_many_arguments)]
async fn gate_longform(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    progress: Option<&GateProgressSender>,
) -> GateOutcome {
    use crate::types::NarrationPhase;
    let tau = profile.tau;
    let chunks: &[String] = &evidence.chunks;
    // H1's verdict for this turn, for `with_native_verdict` at each of
    // this ladder's seven exits. Telemetry: nothing below reads it.
    let native = evidence.native_verdict.as_ref();
    // T1 P1.4 — split the evidence by provenance once per turn. With no
    // Summary-class chunks (the common case, and every pre-P1.4
    // surface) `leaf_chunks == chunks` and the claim loop below is
    // byte-identical to its pre-P1.4 self. The deterministic checks
    // (name veto, specifics scan, batched pre-pass) always read the
    // Leaf view: they are factual-class by construction.
    let leaf_chunks: Vec<String> = chunks
        .iter()
        .enumerate()
        .filter(|(i, _)| evidence.source_of(*i).may_be_quoted())
        .map(|(_, c)| c.clone())
        .collect();
    let summary_chunks: Vec<String> = chunks
        .iter()
        .enumerate()
        .filter(|(i, _)| !evidence.source_of(*i).may_be_quoted())
        .map(|(_, c)| c.clone())
        .collect();
    // THE AUDITOR IS SHOWN WHAT THE DRAFTER WAS SHOWN. This was a constant
    // (`profile.max_chunks = 8`) on every surface, with no stated rationale,
    // while the drafter received the whole retrieved set — so a claim the
    // drafter grounded in leaf chunk #18 could not be cleared by the judge no
    // matter how well calibrated it was. Measured 2026-08-13 over 18 audit
    // passes: 32 of 57 failed claims (56%) had their support in a retrieved
    // leaf chunk PAST the eighth, and zero passes ever came back clean, so
    // every turn paid a rewrite and a re-audit (note 95b82f97).
    //
    // The window is now the retrieved leaf set itself, and the bound is
    // derived rather than picked: the drafter's evidence already passed
    // `prompt_budget::enforce` for this turn's context window, and a judge
    // prompt is strictly SMALLER than the drafter's (one claim in place of
    // the question, the history and the synthesis instructions), so what fit
    // the drafter fits the judge by construction. There is no separate number
    // to choose.
    //
    // Cost is bounded by a mechanism already default-on: every sibling claim
    // declares the same shared-window prefix (`judge::stable_passages_prefix_len`),
    // so `SOVEREIGN_PREFIX_STATE` — whose only consumer is this gate — pins
    // the evidence state once per turn and restores it for claims 2..N. The
    // turn pays one larger prefill, not N.
    let per_claim_chunks = audit_window(leaf_chunks.len());
    let min_claims = profile.max_claims;
    // Session posture for the judge envelopes, resolved once from the
    // synthesis turn's request; the audit closure captures it by copy.
    let posture = crate::slot_policy::posture_of(base_request);
    // Reference-shadow so the audit closure (called twice: draft +
    // rewrite) captures Copy references, not the Vecs themselves.
    let leaf_chunks = &leaf_chunks;
    let summary_chunks = &summary_chunks;
    let pass = audit_pass::AuditPass {
        inference: inference.clone(),
        searcher: evidence.searcher.clone(),
        question,
        leaf_chunks,
        summary_chunks,
        evidence_labels: evidence.source_labels.clone(),
        per_claim_chunks,
        min_claims,
        tau,
        posture,
        progress,
    };

    let draft_backup = draft.clone();
    let audit_pass::AuditPassOutcome::Judged {
        text,
        audited,
        failed,
        unjudged,
    } = pass.run(draft, audit_pass::PassKind::Draft).await
    else {
        // Claim-list extraction failed — fail open with the draft.
        return GateOutcome {
            // Claim-list extraction failed, so the gate reached no verdict.
            // ARCH §18.2: that is not a pass, and until this rung it released
            // the same bare `String` a verified answer did.
            answer: release_unjudged(
                draft_backup,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                "claim-list extraction failed — gate fell open".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "judge_failed_open", "retried": false,
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: Vec::new(),
        };
    };
    let n_claims = audited.len();
    if failed.is_empty() && !unjudged.is_empty() {
        // Nothing flagged, but not everything judged: the ladder fell open
        // on `unjudged.len()` claims. That is the fourth verdict, not the
        // first (ARCH §18.1) — the answer ships, the action says Unjudged,
        // and every unjudged row reaches the ledger as FailOpen.
        tracing::warn!(
            target: "grounding_gate",
            event = "judge_failed_open",
            unjudged = unjudged.len(),
            audited = n_claims,
            "longform audit fell open — released without a verdict on every claim"
        );
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims.saturating_sub(unjudged.len()),
                flagged: 0,
            },
        );
        return GateOutcome {
            answer: release_unjudged(
                text,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                format!(
                    "{} of {n_claims} claim(s) could not be judged — gate fell open",
                    unjudged.len()
                ),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "judge_failed_open", "retried": false,
                    "claims_checked": n_claims, "failed_claims": [],
                    "unjudged_claims": unjudged.len(),
                    "claim_check_outcome": "could-not-judge",
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: longform_claims(&audited, &failed, &unjudged),
        };
    }
    if failed.is_empty() {
        dbg(&format!("longform released claims={n_claims} failed=0"));
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims,
                flagged: 0,
            },
        );
        return GateOutcome {
            answer: release_held(
                text,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                format!("{n_claims} claim(s) audited, none flagged"),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "released", "retried": false,
                    "claims_checked": n_claims, "failed_claims": [],
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: longform_claims(&audited, &failed, &unjudged),
        };
    }
    // ── MARK, DON'T RE-SYNTHESISE ────────────────────────────────────────
    // Two different reasons reach the same release shape, and they are named
    // separately because they are different facts about the turn:
    //
    //   `profile.retry == false`  — a verify-only SURFACE (Refinement). The
    //       caller treats this as "the refined text failed, keep the prior
    //       verified answer" (`runtime/collaboration.rs`).
    //   repair tombstoned         — the surface allows repair, but the repair
    //       LADDER is tombstoned on the default configuration (ECONOMY §9
    //       Phase 4). The draft is released with its failed claims marked.
    //
    // Conflating them under one action string would tell a Refinement
    // consumer that a released, marked knowledge answer was a rejected
    // refinement (ARCH §10.6, one decider one name).
    let repair_armed = config::longform_repair_enabled();
    if !profile.retry || !repair_armed {
        // Verify-only surfaces: annotate the draft with the failed
        // claims — no second synthesis. The caller decides whether
        // an annotated draft is acceptable (Refinement keeps the
        // prior verified answer instead).
        //
        // Tombstoned surfaces: the SAME release shape, and that is the whole
        // point — the replacement was already in production here, so this
        // phase adds no mechanism (ECONOMY §9 Phase 4, "Adds: nothing").
        let action = if profile.retry {
            // Glassbox (#1): the operator is being spared a rewrite + a full
            // re-audit — the two stages that own most of a longform turn
            // (§7.2). A turn that silently skipped them would be
            // indistinguishable from a turn that had nothing to repair.
            // INFO, not DEBUG: once per repaired-turn-that-wasn't.
            tracing::info!(
                target: "grounding_gate",
                event = "repair_tombstoned",
                failed = failed.len(),
                audited = n_claims,
                "longform repair ladder is tombstoned — releasing the audited draft \
                 with its failed claims marked (SOVEREIGN_GATE_LONGFORM_REPAIR=1 re-arms)"
            );
            ACT_ANNOTATED_MARKED
        } else {
            ACT_ANNOTATED_NO_RETRY
        };
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims.saturating_sub(failed.len()),
                flagged: failed.len(),
            },
        );
        // The marking itself. `supported: false` on every failed claim
        // becomes `Verification::FailedOnce` in the epistemic ledger
        // (`runtime/epistemic.rs`), which flips the turn's verdict to
        // `mixed` and renders under the answer. Neither this call nor the
        // ledger consults the repair flag — the mark is a fact about the
        // AUDIT, and the audit is unchanged by construction.
        let claim_records = longform_claims(&audited, &failed, &unjudged);
        let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
        let note = verification_note(&failed_claims);
        return GateOutcome {
            // `append_note` is a no-op on any surface that carries the caveat
            // in its own UI (desktop sets SOVEREIGN_NOTE_AS_METADATA=1); on
            // API/CLI it appends the visible note. Either way a known-failed
            // claim is never released without its caveat (ARCH §18.3).
            answer: release_as_because(
                action,
                append_note(text, &note),
                Vec::new(),
                inference,
                base_request.preferred_speed,
                format!(
                    "{} claim(s) flagged and released with a caveat",
                    failed_claims.len()
                ),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": action.id, "retried": false,
                    "claims_checked": n_claims, "failed_claims": failed_claims,
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: claim_records,
        };
    }
    dbg(&format!(
        "longform rewrite: {} failed of {n_claims}",
        failed.len()
    ));
    emit_gate_progress(
        progress,
        NarrationPhase::ClaimRevisionStart {
            failed: failed.len(),
        },
    );
    // Surgical fast path: correct only the failed spans on the fast slot instead
    // of re-synthesising the whole answer on the 35B (measured ~44s → single
    // digits). Falls back to the full re-synthesis below whenever any failed
    // claim can't be confidently located; either way the result runs the same
    // re-audit ladder, so the fabrication guarantee is unchanged.
    // Surgery targets the COMMON case: a mostly-grounded draft with a few
    // unsupported claims. When MOST claims fail the draft is fundamentally
    // broken — a coherent full re-synthesis beats a Frankenstein of patched
    // sentences (and saves little), so cap surgery at a MINORITY of the
    // audited claims. The cap is now the ratio this sentence has always
    // reasoned about; see `surgery_admits` / `SURGICAL_MAX_FAILED_RATIO` for
    // where 0.5 comes from and why it is not a tunable latency knob.
    let max_failed_ratio = surgical_max_failed_ratio();
    let surgery_admitted = surgery_admits(failed.len(), n_claims, max_failed_ratio);
    // Glassbox (#1): the repair pass's routing decision is a decision worth
    // 25-50s of the operator's turn, and until now the only record of it was a
    // `dbg()` that is a no-op outside SOVEREIGN_AGENTIC_KQ_DEBUG=1 — which is
    // the same invisibility G4 was opened to fix. INFO, not DEBUG: it fires
    // once per longform repair (not per claim), and a production log that
    // cannot say why the operator paid for a full re-synthesis is the defect.
    // The strip says WHICH mechanism ran; this says WHY it was allowed to.
    tracing::info!(
        target: "grounding_gate",
        event = "surgical_cap",
        failed = failed.len(),
        audited = n_claims,
        max_failed_ratio,
        surgery_admitted,
        "surgical cap evaluated"
    );
    // Corrected text: surgical span-edits when every failed claim maps, else a
    // full re-synthesis. The surgical arm takes the INCREMENTAL re-audit
    // (only its repaired spans are re-judged); the full-re-synthesis arm —
    // entirely new prose — keeps the full re-audit. On BOTH arms the
    // holistic scan and the deterministic sweeps run over the whole text:
    // the 2026-07-17 scoped re-audit leaked a GK-caveated fabrication
    // (CONFAB-LEAKED 0→1) precisely by skipping that floor, and the floor
    // here is the shared closure body, so no arm can skip it.
    // G4 — the repair pass's own clock, and the ONE place that knows which
    // of its two mechanisms ran. Until now that fact was recorded only by a
    // `dbg()` that is a no-op unless SOVEREIGN_AGENTIC_KQ_DEBUG=1, so on a
    // production turn nothing outside a debug build could say whether the
    // operator paid 43.2s for a full re-synthesis or 5.36s for surgery
    // (NATIVE_GROUNDING_ECONOMY.md §7.3). It is recorded at the branch, from
    // the branch actually taken — never from `surgical_rewrite_enabled()`,
    // which is true on both arms.
    let rewrite_started = std::time::Instant::now();
    let mut rewrite_mechanism = sovereign_contracts::types::StageMechanism::FullResynthesis;
    // `Some(spans)` only on the surgical arm: the re-audit then verifies the
    // repaired spans incrementally instead of re-extracting ~9 claims from a
    // text that is byte-identical outside those spans. The full-re-synthesis
    // arm produces an entirely new text and keeps the full re-audit.
    let mut surgical_spans: Option<Vec<String>> = None;
    let second: String = 'produce: {
        if config::surgical_rewrite_enabled() && surgery_admitted {
            let pairs: Vec<(String, Vec<String>)> = failed
                .iter()
                .map(|f| (f.claim.clone(), f.evidence.clone()))
                .collect();
            if let Some(edited) =
                surgical::surgical_rewrite(inference, base_request, &text, &pairs).await
            {
                dbg(&format!(
                    "surgical rewrite applied — incremental re-audit follows ({} failed of {n_claims}, {} repaired span(s))",
                    failed.len(),
                    edited.repaired_spans.len()
                ));
                rewrite_mechanism = sovereign_contracts::types::StageMechanism::SurgicalRewrite;
                surgical_spans = Some(edited.repaired_spans);
                break 'produce edited.text;
            }
            // Admitted by the cap and still declined: `surgical_rewrite`
            // could not confidently map every failed claim to a span (or
            // over-deleted). That is a DIFFERENT fallback from the cap
            // declining, it costs the same full re-synthesis, and merging
            // the two in the log would make the cap look guilty for a span
            // resolver's miss. Named separately for that reason.
            tracing::info!(
                target: "grounding_gate",
                event = "surgical_unmapped",
                failed = failed.len(),
                audited = n_claims,
                "surgery was admitted but could not map every failed claim — full re-synthesis"
            );
        }
        // Full re-synthesis fallback (flag off, failures are a MAJORITY of the
        // audited claims, or surgery could not confidently map a claim).
        let mut rewrite_req = base_request.clone();
        let base_sys = rewrite_req.system_message.clone().unwrap_or_default();
        rewrite_req.system_message = Some(format!("{base_sys}{}", rewrite_system_note(&failed)));
        rewrite_req.assistant_prefix = Some(LONGFORM_REWRITE_PREFIX.to_string());
        // Budget ~1.5x the draft's token estimate — a faithful rewrite REPLACES
        // a short false claim with a LONGER cited correction, so a 1.0x cap ships
        // truncated; 1.5x stays under the 2x runaway floor and the re-audit still
        // guards the result (history: 2026-06-30 runaway inflation to 23.8k chars,
        // 2026-07-12 truncation at the cap).
        let draft_token_budget = (draft_backup.chars().count() * 3 / 8).max(256);
        rewrite_req.max_tokens = Some(
            rewrite_req
                .max_tokens
                .map_or(draft_token_budget, |m| m.min(draft_token_budget)),
        );
        match gate_call(
            &**inference,
            &rewrite_req,
            sovereign_contracts::types::GateCallMechanism::Rewrite,
        )
        .await
        {
            Ok(resp) => {
                // Truncation trace: the longform rewrite is non-streaming and
                // bypasses synth.truncation — log finish vs cap so a silent
                // Length cut is visible.
                tracing::info!(
                    target: "gate.call",
                    kind = "rewrite",
                    finish = ?resp.finish_reason,
                    completion_tokens = ?resp.completion_tokens,
                    max_tokens = ?rewrite_req.max_tokens,
                    resp_chars = resp.text.chars().count(),
                    "gate internal completion"
                );
                format!("{LONGFORM_REWRITE_PREFIX}{}", resp.text)
            }
            Err(e) => {
                // Rewrite unavailable: release draft 1 WITH the visible
                // verification note (never silently release known-failed
                // claims; never destroy an essay over judge availability).
                tracing::warn!(target: "grounding_gate", error = %e, "longform rewrite failed — annotating draft");
                // The repair pass spent this time and then failed. Attributed,
                // not dropped: an early return is still an execution.
                crate::runtime::stage_ledger::Stage::new(
                    sovereign_contracts::types::StageId::Rewrite,
                    sovereign_contracts::types::StackOwner::Incumbent,
                )
                .mechanism(rewrite_mechanism)
                .cause(sovereign_contracts::types::StageCause::AuditFoundFailures)
                .record(rewrite_started.elapsed().as_millis() as u64);
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimCheckComplete {
                        confirmed: n_claims.saturating_sub(failed.len()),
                        flagged: failed.len(),
                    },
                );
                let claim_records = longform_claims(&audited, &failed, &unjudged);
                let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
                let note = verification_note(&failed_claims);
                return GateOutcome {
                    answer: release_as_because(
                        ACT_ANNOTATED_REWRITE_ERROR,
                        append_note(text, &note),
                        Vec::new(),
                        inference,
                        base_request.preferred_speed,
                        format!(
                            "surgical rewrite failed; {} claim(s) flagged and released with a caveat",
                            failed_claims.len()
                        ),
                    ),
                    meta: with_native_verdict(
                        serde_json::json!({
                            "surface": profile.surface.id(),
                            "action": ACT_ANNOTATED_REWRITE_ERROR.id, "retried": false,
                            "claims_checked": n_claims, "failed_claims": failed_claims,
                            "threshold": tau, "mode": "per_claim",
                        }),
                        native,
                    ),
                    claims: claim_records,
                };
            }
        }
    };

    // G4 — the repair pass completed. Recorded BEFORE the re-audit runs, so
    // the two are separate rows: the re-audit's whole existence is caused by
    // this pass having produced new prose, and a strip that merged them would
    // hide the causal chain the operator asked to be able to read.
    crate::runtime::stage_ledger::Stage::new(
        sovereign_contracts::types::StageId::Rewrite,
        sovereign_contracts::types::StackOwner::Incumbent,
    )
    .mechanism(rewrite_mechanism)
    .cause(sovereign_contracts::types::StageCause::AuditFoundFailures)
    .record(rewrite_started.elapsed().as_millis() as u64);

    let second_backup = second.clone();
    // On the incremental arm the re-audit returns only the repaired spans as
    // its audited set; the audit#1 claims whose sentences surgery did NOT
    // touch are still true, this-turn-verified holdings of the released text,
    // so they are carried into the ledger rather than silently dropped
    // (ARCH §18.3 — a shrunken holdings list would read as "less was
    // verified", which is the opposite of what happened).
    let carried_claims: Vec<String> = if surgical_spans.is_some() {
        audited
            .iter()
            .filter(|c| !failed.iter().any(|f| &f.claim == *c))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    match pass
        .run(
            second,
            audit_pass::PassKind::ReAudit {
                incremental: surgical_spans,
            },
        )
        .await
    {
        audit_pass::AuditPassOutcome::Judged {
            text: text2,
            audited: mut audited2,
            failed: failed2,
            unjudged: unjudged2,
        } if failed2.is_empty() => {
            audited2.extend(carried_claims);
            let n2 = audited2.len();
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete {
                    confirmed: n2,
                    flagged: 0,
                },
            );
            GateOutcome {
                answer: release_as_because(
                    ACT_REWRITE_RELEASED,
                    text2,
                    Vec::new(),
                    inference,
                    base_request.preferred_speed,
                    format!("{n2} claim(s) re-audited after rewrite, none flagged"),
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": ACT_REWRITE_RELEASED.id, "retried": true,
                        "claims_checked": n2, "failed_claims": [],
                        "threshold": tau, "mode": "per_claim",
                    }),
                    native,
                ),
                claims: longform_claims(&audited2, &failed2, &unjudged2),
            }
        }
        audit_pass::AuditPassOutcome::Judged {
            text: text2,
            audited: mut audited2,
            failed: failed2,
            unjudged: unjudged2,
        } => {
            audited2.extend(carried_claims);
            let n2 = audited2.len();
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete {
                    confirmed: n2.saturating_sub(failed2.len()),
                    flagged: failed2.len(),
                },
            );
            let claim_records = longform_claims(&audited2, &failed2, &unjudged2);
            let failed_claims: Vec<String> = failed2.into_iter().map(|f| f.claim).collect();
            let note = verification_note(&failed_claims);
            GateOutcome {
                answer: release_as_because(
                    ACT_REWRITE_ANNOTATED,
                    append_note(text2, &note),
                    Vec::new(),
                    inference,
                    base_request.preferred_speed,
                    format!(
                        "{} claim(s) still flagged after rewrite, released with a caveat",
                        failed_claims.len()
                    ),
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "action": ACT_REWRITE_ANNOTATED.id, "retried": true,
                        "claims_checked": n2, "failed_claims": failed_claims,
                        "threshold": tau, "mode": "per_claim",
                    }),
                    native,
                ),
                claims: claim_records,
            }
        }
        audit_pass::AuditPassOutcome::ExtractionFailed => GateOutcome {
            // The rewrite produced text the gate never re-audited.
            answer: release_as_because(
                ACT_REWRITE_RELEASED_UNVERIFIED,
                second_backup,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                "rewrite released without re-audit".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": ACT_REWRITE_RELEASED_UNVERIFIED.id, "retried": true,
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: Vec::new(),
        },
    }
}


#[cfg(test)]
mod tests;
