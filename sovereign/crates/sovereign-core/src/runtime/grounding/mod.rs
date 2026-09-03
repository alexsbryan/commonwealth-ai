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
mod gate;
mod inner;
mod longform;
// The orchestrator trio keep their historical `grounding::` paths: handlers,
// collaboration, and the moved tests all reach them through this façade.
pub(crate) use gate::{gate_answer, gate_answer_with_progress};
pub(crate) use gate::{
    audit_forensics, audit_window, longform_claims, project_verdict, short_specifics_guard,
};
pub(crate) use inner::gate_answer_inner;
pub(crate) use longform::gate_longform;
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


#[cfg(test)]
mod tests;
