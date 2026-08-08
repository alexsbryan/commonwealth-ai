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

mod citation;
mod citation_attribution;
mod config;
mod judge;
mod pipeline;
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

pub(crate) use config::{dbg, grounding_gate_enabled, GateSurface, GroundingProfile};
// Registry export: consumed by the config-module coverage test today;
// the docs flag table renders from it (same contract as
// `retrieval_pipeline_flags`).
#[allow(unused_imports)]
pub use config::{grounding_gate_flags, grounding_gate_threshold};
#[allow(unused_imports)]
pub(crate) use judge::{verify_grounding, GateVerdict};
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

use crate::traits::InferenceProvider;
use crate::types::CitationTarget;
use crate::types::CompletionRequest;

use judge::{claim_violation_joint, scan_unsupported_specifics, unwrap_unverified_excerpts};

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
    pub chunk_sources: Vec<EvidenceSource>,
}

/// Where an evidence chunk's text came from (T1 P1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidenceSource {
    /// A real retrieved source chunk — verbatim corpus text.
    Leaf,
    /// A derived RAPTOR summary node — LLM prose ABOUT source text.
    /// May support thematic/structural claims; never factual ones.
    Summary,
}

impl EvidenceContext {
    /// Provenance of chunk `idx`. Indices past `chunk_sources` (late
    /// appends, or an empty vec entirely) read as `Leaf` — the
    /// conservative pre-P1.4 degradation.
    pub(crate) fn source_of(&self, idx: usize) -> EvidenceSource {
        self.chunk_sources
            .get(idx)
            .copied()
            .unwrap_or(EvidenceSource::Leaf)
    }

    /// True when any chunk is Summary-class — i.e. the P1.4 policy has
    /// something to decide. False short-circuits the claim loop to the
    /// exact pre-P1.4 code path.
    pub(crate) fn has_summary_evidence(&self) -> bool {
        self.chunk_sources
            .iter()
            .any(|s| *s == EvidenceSource::Summary)
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
    pub chunk_sources: Vec<EvidenceSource>,
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
    let exclude_raptor = std::env::var("SOVEREIGN_GATE_EXCLUDE_RAPTOR")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    if !exclude_raptor {
        // Historical pre-Fix-B baseline: summaries are source-equivalent.
        return GateEvidenceParts {
            chunks: chunks.iter().map(|c| c.content.clone()).collect(),
            chunk_sources: vec![EvidenceSource::Leaf; chunks.len()],
            chunk_labels: chunks.iter().map(labels_of).collect(),
            chunk_locators: gate_evidence_locators(chunks),
            chunk_targets: gate_evidence_targets(chunks),
        };
    }
    let summary_evidence = std::env::var("SOVEREIGN_GATE_SUMMARY_EVIDENCE")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    let is_summary = |c: &corpus_engine::ScoredChunk| {
        c.metadata.get("source").map(String::as_str) == Some("raptor")
    };
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
    };
    for (i, c) in chunks.iter().enumerate().filter(|(_, c)| !is_summary(c)) {
        parts.chunks.push(c.content.clone());
        parts.chunk_sources.push(EvidenceSource::Leaf);
        parts.chunk_labels.push(labels_of(c));
        parts.chunk_locators.push(locator_at(i));
        parts.chunk_targets.push(target_at(i));
    }
    if summary_evidence {
        for (i, c) in chunks.iter().enumerate().filter(|(_, c)| is_summary(c)) {
            parts.chunks.push(c.content.clone());
            parts.chunk_sources.push(EvidenceSource::Summary);
            parts.chunk_labels.push(labels_of(c));
            parts.chunk_locators.push(locator_at(i));
            parts.chunk_targets.push(target_at(i));
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
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".sovereign")
        })
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
    pub text: String,
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
    /// The judge's violation probability, when a forced-choice verdict
    /// produced one (single-claim path only; long-form and citation
    /// records carry `None`).
    pub violation_prob: Option<f64>,
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
    let mut outcome = gate_answer_inner(
        inference,
        question,
        draft,
        evidence,
        base_request,
        profile,
        progress,
    )
    .await;
    record_gate_decision(
        &mut outcome,
        evidence,
        profile,
        started.elapsed().as_millis() as u64,
    );
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
fn record_gate_decision(
    outcome: &mut GateOutcome,
    evidence: &EvidenceContext,
    profile: &GroundingProfile,
    gate_ms: u64,
) {
    use sovereign_contracts::types::{
        grounding_journal_append, journal_dir, EvidenceRef, GateJudgeVerdict, GroundingDecision,
        GroundingLine,
    };
    let mut d = GroundingDecision::new(profile.surface.id(), profile.tau, gate_ms);
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
    d.violation_prob = meta.and_then(|m| m.get("violation_prob")).and_then(|v| v.as_f64());
    // Verdict, from what the ladder reported. The vp comparison mirrors
    // the gate's own `>= tau` act condition; paths that judge without a
    // vp (citation-grounded) speak through their claim verdicts; a path
    // with neither judged nothing — could-not-judge, never a pass
    // (ARCH §18.1).
    d.verdict = match d.violation_prob {
        Some(vp) if vp >= profile.tau => GateJudgeVerdict::Unsupported,
        Some(_) => GateJudgeVerdict::Supported,
        None if !outcome.claims.is_empty() => {
            if outcome.claims.iter().all(|c| c.supported) {
                GateJudgeVerdict::Supported
            } else {
                GateJudgeVerdict::Unsupported
            }
        }
        None => GateJudgeVerdict::CouldNotJudge,
    };
    d.chunks = evidence.chunks.len();
    d.evidence = evidence
        .chunk_targets
        .iter()
        .flatten()
        .map(|t| EvidenceRef { corpus: t.corpus_id.clone(), chunk: t.chunk_id })
        .collect();
    d.evidence_unresolved = d.chunks.saturating_sub(d.evidence.len());
    d.top_similarity = evidence.top_similarity;
    if let Some(m) = outcome.meta.as_object_mut() {
        m.insert(
            "episode_id".to_string(),
            serde_json::Value::String(d.episode_id.clone()),
        );
    }
    let line = GroundingLine::Decision(d);
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
    let (chunks, locators, targets): (&[String], &[Option<String>], &[Option<CitationTarget>]) =
        if evidence.has_summary_evidence() {
            let keep: Vec<usize> = (0..evidence.chunks.len())
                .filter(|i| evidence.source_of(*i) == EvidenceSource::Leaf)
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
            (&leaf_owned, &leaf_locators, &leaf_targets)
        } else {
            (
                &evidence.chunks,
                &evidence.chunk_locators,
                &evidence.chunk_targets,
            )
        };
    let entity_anchored = evidence.entity_anchored;
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
        if let citation::CitationOutcome::Grounded { answer, quotes } =
            citation::citation_grounded_answer(
                &**inference,
                question,
                chunks,
                locators,
                targets,
                crate::slot_policy::posture_of(base_request),
            )
            .await
        {
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
            let released_citations: Vec<crate::types::ReleasedCitation> = quotes
                .iter()
                .filter_map(|q| {
                    Some(crate::types::ReleasedCitation {
                        text: q.text.clone(),
                        locator: q.locator.clone(),
                        target: q.target.clone()?,
                    })
                })
                .collect();
            let openable = released_citations.len();
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
            )
            .await
            {
                return guarded;
            }
            return GateOutcome {
                text: cited,
                meta: serde_json::json!({
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
                claims: vec![GateClaim {
                    text: answer,
                    supported: true,
                    failed_once: false,
                    violation_prob: None,
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
    let mut action = "released";
    let mut retried = false;
    let mut final_vp: Option<f64> = None;
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
    match verify_grounding(
        inference,
        question,
        &verify_text,
        chunks,
        entity_anchored,
        evidence.searcher.as_ref(),
        crate::slot_policy::posture_of(base_request),
    )
    .await
    {
        Some(v) => {
            final_vp = Some(v.violation_prob);
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
                    failed_once: v.violation_prob >= tau,
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
                        action = "abstained_no_retry";
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimCheckComplete {
                                confirmed: 0,
                                flagged: 1,
                            },
                        );
                        return GateOutcome {
                            text,
                            meta: serde_json::json!({
                                "surface": profile.surface.id(),
                                "action": action,
                                "retried": false,
                                "violation_prob": final_vp,
                                "threshold": tau,
                                "mode": "single_claim",
                                "draft": draft_for_meta,
                            }),
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
                            action = "abstained_weak_evidence";
                            emit_gate_progress(
                                progress,
                                NarrationPhase::ClaimCheckComplete {
                                    confirmed: 0,
                                    flagged: 1,
                                },
                            );
                            return GateOutcome {
                                text,
                                meta: serde_json::json!({
                                    "surface": profile.surface.id(),
                                    "action": action,
                                    "retried": false,
                                    "violation_prob": final_vp,
                                    "threshold": tau,
                                    "top_similarity": sim,
                                    "retry_floor": floor,
                                    "mode": "single_claim",
                                    "draft": draft_for_meta,
                                }),
                                claims: gate_claims,
                            };
                        }
                    }
                    retried = true;
                    emit_gate_progress(progress, NarrationPhase::ClaimRevisionStart { failed: 1 });
                    let mut retry_req = base_request.clone();
                    let base_sys = retry_req.system_message.clone().unwrap_or_default();
                    retry_req.system_message = Some(format!(
                        "{base_sys}{}",
                        retry_system_note(&claim, &v.claim_evidence)
                    ));
                    retry_req.assistant_prefix = None;
                    match inference.complete(&retry_req).await {
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
                            match verify_grounding(
                                inference,
                                question,
                                &verify_second,
                                chunks,
                                entity_anchored,
                                evidence.searcher.as_ref(),
                                crate::slot_policy::posture_of(base_request),
                            )
                            .await
                            {
                                Some(v2) if v2.violation_prob < tau => {
                                    final_vp = Some(v2.violation_prob);
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
                                        action = "abstained_decline";
                                        emit_gate_progress(
                                            progress,
                                            NarrationPhase::ClaimVerdict {
                                                index: 0,
                                                supported: false,
                                            },
                                        );
                                    } else {
                                        text = second;
                                        action = "retry_released";
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
                                    text = grounded_abstention(&claim, chunks.len().min(12));
                                    action = "abstained";
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
                                        action = "abstained_decline";
                                    } else {
                                        action = "retry_released_unverified";
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
                            action = "abstained_retry_error";
                        }
                    }
                }
            }
        }
        None => {
            action = "judge_failed_open";
        }
    }
    // Terminal progress frame for the short path. Only when a claim
    // was actually audited (NO_CLAIM releases verified nothing) and
    // only on the verdicts this fall-through exit owns — the abstain
    // early-returns above emit their own completion frames.
    if claim_audited {
        let (confirmed, flagged) = match action {
            "released" => (1, 0),
            "retry_released" | "retry_released_unverified" => (1, 1),
            a if a.starts_with("abstained") => (0, 1),
            _ => (0, 0),
        };
        if confirmed + flagged > 0 {
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete { confirmed, flagged },
            );
        }
    }
    dbg(&format!(
        "verdict action={action} retried={retried} vp={final_vp:?} tau={tau}"
    ));
    tracing::info!(
        target: "grounding_gate",
        action,
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
    if action == "released"
        && text.trim().chars().count() < 15
        && !text.contains("Grounded in the source")
        && question.trim().chars().count() > 40
    {
        dbg(&format!(
            "fragment guard: released text {:?} answers nothing — abstaining",
            text.trim()
        ));
        return GateOutcome {
            text: grounded_abstention(question, chunks.len().min(12)),
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "abstained_fragment",
                "retried": retried,
                "violation_prob": final_vp,
                "threshold": tau,
                "mode": "single_claim",
                "draft": draft_for_meta,
            }),
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
    if action == "released" && !claim_audited && released_pure_decline(&text) {
        dbg("decline guard: NO_CLAIM release is a pure decline — reclassifying as abstention");
        tracing::info!(
            target: "grounding_gate",
            "grounding gate: released text is a 0-holding decline — action reclassified to abstained_decline"
        );
        return GateOutcome {
            text,
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "abstained_decline",
                "retried": retried,
                "violation_prob": final_vp,
                "threshold": tau,
                "mode": "single_claim",
                "draft": draft_for_meta,
            }),
            claims: gate_claims,
        };
    }
    // Second-opinion fabrication guard on a RELEASED single-claim answer — the
    // per-claim verify grounds the load-bearing value but is blind to fabricated
    // SUPPORTING specifics (a cited flag/number/entity absent from the
    // evidence). Skip when the path already abstained (nothing asserted). On a
    // flag: correct-or-abstain via one grounded rewrite.
    if !action.starts_with("abstained") && !action.starts_with("judge_failed") {
        if let Some(guarded) = short_specifics_guard(
            inference,
            question,
            &text,
            chunks,
            evidence.searcher.as_ref(),
            base_request,
            profile,
        )
        .await
        {
            return guarded;
        }
    }
    GateOutcome {
        text,
        meta: serde_json::json!({
            "surface": profile.surface.id(),
            "action": action,
            "retried": retried,
            "violation_prob": final_vp,
            "threshold": tau,
            "mode": "single_claim",
            "draft": draft_for_meta,
        }),
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
    let second = match inference.complete(&retry_req).await {
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
        budget,
        crate::slot_policy::posture_of(base_request),
    )
    .await
    {
        Some(v) if !v.is_empty() => {
            tracing::info!(
                target: "grounding_gate",
                action = "abstained_specifics",
                flagged = specifics.len(),
                "short specifics guard: rewrite still fabricates — abstaining"
            );
            let claims = specifics
                .iter()
                .map(|s| GateClaim {
                    text: s.clone(),
                    supported: false,
                    failed_once: true,
                    violation_prob: None,
                })
                .collect();
            Some(GateOutcome {
                text: grounded_abstention("", chunks.len().min(12)),
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "abstained_specifics",
                    "retried": true,
                    "flagged_specifics": specifics,
                    "mode": "short_specifics",
                }),
                claims,
            })
        }
        _ => {
            tracing::info!(
                target: "grounding_gate",
                action = "retry_released_specifics",
                flagged = specifics.len(),
                "short specifics guard: corrective rewrite released"
            );
            let claims = specifics
                .iter()
                .map(|s| GateClaim {
                    text: s.clone(),
                    supported: true,
                    failed_once: true,
                    violation_prob: None,
                })
                .collect();
            Some(GateOutcome {
                text: second,
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "retry_released_specifics",
                    "retried": true,
                    "flagged_specifics": specifics,
                    "mode": "short_specifics",
                }),
                claims,
            })
        }
    }
}

/// Fold a long-form audit's outcome into retained per-claim records
/// for the epistemic ledger: audited claims get their final verdict;
/// synthetic failures (specifics scan, sentence sweep) that never
/// appeared in the extracted list are appended as unsupported records.
fn longform_claims(audited: &[String], failed: &[FailedClaim]) -> Vec<GateClaim> {
    let mut out: Vec<GateClaim> = audited
        .iter()
        .map(|c| {
            let is_failed = failed.iter().any(|f| &f.claim == c);
            GateClaim {
                text: c.clone(),
                supported: !is_failed,
                failed_once: is_failed,
                violation_prob: None,
            }
        })
        .collect();
    for f in failed {
        if !audited.iter().any(|c| c == &f.claim) {
            out.push(GateClaim {
                text: f.claim.clone(),
                supported: false,
                failed_once: true,
                violation_prob: None,
            });
        }
    }
    out
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
    // T1 P1.4 — split the evidence by provenance once per turn. With no
    // Summary-class chunks (the common case, and every pre-P1.4
    // surface) `leaf_chunks == chunks` and the claim loop below is
    // byte-identical to its pre-P1.4 self. The deterministic checks
    // (name veto, specifics scan, batched pre-pass) always read the
    // Leaf view: they are factual-class by construction.
    let leaf_chunks: Vec<String> = chunks
        .iter()
        .enumerate()
        .filter(|(i, _)| evidence.source_of(*i) == EvidenceSource::Leaf)
        .map(|(_, c)| c.clone())
        .collect();
    let summary_chunks: Vec<String> = chunks
        .iter()
        .enumerate()
        .filter(|(i, _)| evidence.source_of(*i) == EvidenceSource::Summary)
        .map(|(_, c)| c.clone())
        .collect();
    let per_claim_chunks = profile.max_chunks;
    let min_claims = profile.max_claims;
    // Session posture for the judge envelopes, resolved once from the
    // synthesis turn's request; the audit closure captures it by copy.
    let posture = crate::slot_policy::posture_of(base_request);
    // Reference-shadow so the audit closure (called twice: draft +
    // rewrite) captures Copy references, not the Vecs themselves.
    let leaf_chunks = &leaf_chunks;
    let summary_chunks = &summary_chunks;
    let audit = |text: String, recheck: bool| {
        let inference = inference.clone();
        let searcher = evidence.searcher.clone();
        let evidence_labels = evidence.source_labels.clone();
        async move {
            // Budget scales with THIS text's length — audited afresh for the
            // draft and again for the (possibly different-length) rewrite.
            let budget = claim_budget(text.chars().count(), min_claims);
            let claims = extract_claim_list(&inference, question, &text, budget, posture).await?;
            // Progress: the extracted claim list opens (or re-opens,
            // on the rewrite's re-audit) the desktop's check panel.
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckStart {
                    claims: claims.iter().take(budget).map(|c| wire_claim(c)).collect(),
                    recheck,
                },
            );
            let mut failed: Vec<FailedClaim> = Vec::new();
            // Evidence + labels, lowercased once, for the deterministic
            // in-world attribution veto below.
            let hay_lower = {
                let mut h = leaf_chunks.join(" ").to_lowercase();
                for l in &evidence_labels {
                    h.push(' ');
                    h.push_str(&l.to_lowercase());
                }
                h
            };
            // Batched support pre-pass (SOVEREIGN_GATE_BATCH_VERIFY, default OFF):
            // one call judges all claims with the evidence prefilled ONCE, so the
            // N per-claim re-prefills of the same evidence collapse to one on the
            // prefix-cache-vetoed qwen35moe. Indexed by the same enumerate() index
            // as the loop below. Empty when the flag is off → the loop runs exactly
            // as before. GATED on claim count: with only a few claims the single
            // batched prefill does not amortise (measured net-negative below ~6
            // claims), so small answers keep the per-claim path.
            let claim_texts: Vec<String> = claims.iter().take(budget).cloned().collect();
            let shadow_mode = config::gate_batch_shadow_enabled();
            // The LADDER also drives this pass, as a TRIAGE signal only (it
            // decides who gets a corpus search; it never becomes a verdict —
            // see `batch_v` below). That distinction is what lets the ladder
            // use a mechanism still marked STUDY for verdict purposes: the
            // batched verdict is a text A/B whose tau semantics differ from the
            // calibrated logit, which matters for judging a claim and does not
            // matter for choosing whether to widen its evidence.
            let ladder_enabled = config::claim_search_ladder_enabled();
            // `claim_search_shadow_enabled` is in this list so the triage
            // verdict exists to be LOGGED even with the ladder off. That is the
            // only configuration in which the ladder's safety can actually be
            // measured: ladder off means every claim is still searched, so the
            // true rescue set is observable, while `triage` records which of
            // them the ladder WOULD have skipped. With the ladder on, a skipped
            // claim is never searched and its rescue is unobservable by
            // construction.
            let batched_support: Vec<Option<bool>> = if (config::gate_batch_verify_enabled()
                || shadow_mode
                || ladder_enabled
                || config::claim_search_shadow_enabled())
                && claim_texts.len() >= config::gate_batch_min_claims()
            {
                judge::claims_support_batched(
                    &inference,
                    &claim_texts,
                    &leaf_chunks,
                    per_claim_chunks,
                    posture,
                )
                .await
            } else {
                Vec::new()
            };
            for (claim_idx, claim) in claims.iter().take(budget).enumerate() {
                // Jurisdiction: honesty meta-language is not a world-claim —
                // "the system does not have access to X" can never be stated
                // by a passage, and auditing it prosecutes the answer's own
                // honesty (observed: refined honest declines rejected at vp
                // 0.85–0.98 on exactly these sentences). Deterministic shape
                // check; see is_self_referential_decline.
                if judge::is_self_referential_decline(claim) {
                    dbg(&format!(
                        "longform claim EXEMPT — self-referential decline: {claim:?}"
                    ));
                    // Exempt = ships unflagged; stamp the row so the
                    // panel never shows a permanently-pending claim.
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimVerdict {
                            index: claim_idx,
                            supported: true,
                        },
                    );
                    continue;
                }
                // Deterministic pre-check: an in-world attribution naming a
                // person absent from the ENTIRE evidence is fabricated — do
                // not ask the yes-biased joint judge (measured: "Betty
                // Alexander sent an email…" cleared at vp=0.010 despite the
                // name existing nowhere in the corpus; it shipped in 3 runs).
                let vetoed = judge::absent_name_attribution(claim, &hay_lower)
                    .map(|n| ("person", n))
                    .or_else(|| {
                        judge::absent_identifier_attribution(claim, &hay_lower)
                            .map(|i| ("identifier", i))
                    });
                if let Some((kind, name)) = vetoed {
                    dbg(&format!(
                        "longform claim VETOED — in-world attribution names {kind} {name:?}, absent from evidence: {claim:?}"
                    ));
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimVerdict {
                            index: claim_idx,
                            supported: false,
                        },
                    );
                    let extra = match &searcher {
                        Some(s) => s.search(claim).await,
                        None => Vec::new(),
                    };
                    failed.push(FailedClaim {
                        claim: claim.clone(),
                        evidence: extra,
                    });
                    continue;
                }
                // Claim-conditioned retrieval: verify against the
                // sealed CORPUS, not just the prompt snapshot. The
                // SHARED prompt window goes first and claim-specific
                // hits are APPENDED after it, so every per-claim judge
                // prompt shares one byte-stable evidence prefix — the
                // pinned-prefix state cache (SOVEREIGN_PREFIX_STATE)
                // can restore that prefix instead of re-prefilling the
                // ~10K-token evidence per claim on prefix-cache-vetoed
                // hybrids (hits-FIRST ordering diverged the prompts at
                // the first passage and thrashed the pin). Duplicates
                // resolve in favor of the shared copy; novel hits widen
                // the cap by their count, so they never displace a
                // shared chunk the old audit would have judged.
                // NOTE: the claim-conditioned search itself is issued BELOW,
                // after the shared window exists — the ladder needs to judge
                // against that window before deciding whether to pay for it.
                // With the ladder off the search still happens unconditionally,
                // exactly as before, just a few lines later.
                //
                // T1 P1.4 class policy: FACTUAL/SPECIFIC claims verify
                // against Leaf evidence only (a derived summary must
                // never be the source-of-truth for a fact); THEMATIC/
                // STRUCTURAL claims may additionally rest on Summary-
                // class chunks — appended AFTER the leaf window so the
                // leaf prefix stays byte-stable across both classes
                // (mixed prefix declarations cost some pin efficiency,
                // never correctness). With no summaries in evidence the
                // window is exactly the pre-P1.4 one.
                let factual = summary_chunks.is_empty()
                    || judge::claim_is_factual_specific(&inference, claim).await;
                let mut shared: Vec<String> =
                    leaf_chunks.iter().take(per_claim_chunks).cloned().collect();
                if !factual {
                    shared.extend(summary_chunks.iter().take(per_claim_chunks).cloned());
                    dbg(&format!(
                        "longform claim THEMATIC — {} summary chunk(s) admitted as evidence: {claim:?}",
                        summary_chunks.len().min(per_claim_chunks)
                    ));
                }
                let seen: HashSet<String> = shared
                    .iter()
                    .map(|c| c.chars().take(120).collect::<String>())
                    .collect();
                // Every sibling claim-check declares this same shared-window
                // boundary (judge::stable_passages_prefix_len), so the engine
                // pins the evidence state once per turn and restores it for
                // claims 2..N — including claims that append extra hits.
                let n_shared = shared.len();
                // SHADOW (SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW, default OFF):
                // keep a copy of the prompt-only window so the SAME claim can
                // be re-judged without the re-searched hits. Unlike the
                // single-claim path, `claim_violation_joint` judges all
                // passages in ONE forced-choice — there is no per-chunk max to
                // decompose — so the counterfactual costs one extra call per
                // claim. That call's passages are exactly the pinned shared
                // prefix, so it restores rather than re-prefills.
                let shadow_claim_search = config::claim_search_shadow_enabled();
                let shared_only: Option<Vec<String>> = if shadow_claim_search {
                    Some(shared.clone())
                } else {
                    None
                };
                // ── THE LADDER (SOVEREIGN_GATE_CLAIM_SEARCH_LADDER, default OFF) ──
                // Judge the claim against the prompt window FIRST, and pay for
                // the corpus fan-out only when it fails without one.
                //
                // NOT LOSSLESS BY CONSTRUCTION — that claim was made and then
                // WITHDRAWN (2026-08-05), and the withdrawal is the reason this
                // flag is still default OFF.
                //
                // The argument was: a "rescue" is exactly a claim that fails
                // without re-search and passes with it, so every rescue has
                // stage-1 vp >= tau and always reaches stage 2. That holds only
                // while stage 1 IS the calibrated per-claim judge — true of the
                // first shape, false of this one. Stage 1 is now
                // `claims_support_batched`, a text A/B whose tau semantics
                // differ from the calibrated logit (see the note directly above
                // `batched_support`). A batch false-"supported" on a claim the
                // calibrated judge would have failed skips stage 2 and loses the
                // rescue. Two instruments, so their agreement is an empirical
                // question about the sample, not a property of the definition.
                //
                // 7/7 rescues kept at 61% of the fan-out on
                // `summary_cosmological_argument` (18 claims, 2026-08-05) is
                // therefore EVIDENCE, not proof — and one specimen at that.
                // Before this can go default-on it owes a bank-level
                // `lost_rescue` count from the shadow event below, which exists
                // to measure exactly this.
                //
                // ONE BEHAVIOUR CHANGE, NAMED: today a re-searched hit can
                // DILUTE a claim the shared window alone would have supported —
                // all passages land in one joint forced-choice, so unlike the
                // single-claim path there is no per-chunk max and no rescue
                // floor to stop it. Under the ladder such a claim is released on
                // its stage-1 verdict and never re-searched. Measured
                // `newly_failed = 0` on the specimen above: real in principle,
                // unobserved in practice. Watch `ladder_diluted_avoided`.
                //
                // STAGE 1 IS THE BATCHED PRE-PASS, NOT A PER-CLAIM JUDGE. The
                // first cut of this used one `claim_violation_joint` per claim
                // and MEASURED NET-NEGATIVE (+5.0s wall: -19.5s of avoided
                // search against +11 extra forced-choice calls at ~2.2s each).
                // A restored pinned prefix does not make a forced-choice free —
                // it costs the same order as the corpus search it replaces.
                // `claims_support_batched` scores every claim in ONE generation
                // off a single evidence prefill, so triage costs ~1 call for the
                // whole turn instead of one per claim, and the per-claim judge
                // count stays exactly what the baseline pays. See note
                // a4be8afd for the failed first shape.
                let ladder = ladder_enabled && searcher.is_some();
                // Triage only — deliberately NOT gated on
                // gate_batch_verify_enabled, and deliberately never a verdict.
                let triage = batched_support.get(claim_idx).and_then(|v| *v);
                let mut stage2_searched = true;
                let extra: Vec<String> = if ladder {
                    match triage {
                        // The shared window alone already supports it; a wider
                        // net could only have confirmed what we have. Skip.
                        Some(true) => {
                            stage2_searched = false;
                            Vec::new()
                        }
                        // Unsupported, OR no clean batched verdict, OR the batch
                        // never ran (too few claims to amortise): widen. Every
                        // ambiguous case searches, so triage errs toward the
                        // status quo and a rescue can never be lost to a parse
                        // gap.
                        _ => match &searcher {
                            Some(s) => s.search(claim).await,
                            None => Vec::new(),
                        },
                    }
                } else {
                    match &searcher {
                        Some(s) => {
                            let hits = s.search(claim).await;
                            if !hits.is_empty() {
                                dbg(&format!(
                                    "claim_search hits={} for {:?}",
                                    hits.len(),
                                    claim.chars().take(60).collect::<String>()
                                ));
                            }
                            hits
                        }
                        None => Vec::new(),
                    }
                };
                if ladder {
                    tracing::info!(
                        target: "grounding_gate",
                        event = "claim_search_ladder",
                        claim = %claim.chars().take(90).collect::<String>(),
                        triage = match triage {
                            Some(true) => "supported",
                            Some(false) => "unsupported",
                            None => "no-verdict",
                        },
                        searched = stage2_searched,
                        extras = extra.len(),
                        "claim search ladder: corpus fan-out spent only on claims the prompt window failed"
                    );
                }
                let mut judged = shared;
                judged.extend(
                    extra
                        .iter()
                        .filter(|c| !seen.contains(&c.chars().take(120).collect::<String>()))
                        .cloned(),
                );
                let cap = judged.len();
                // Use the batched pre-pass's verdict for this claim (both
                // directions — the fan-out is dominated by UNSUPPORTED claims, so
                // trusting only SUPPORTED yields no net win). A parse gap (None)
                // falls back to the calibrated per-claim forced-choice. The
                // deterministic in-world name/identifier veto already ran ABOVE, so
                // blatant fabrication is caught before this LLM verdict either way.
                // VERDICT source. Gated on the batch flags ONLY: when the ladder
                // populated `batched_support` for triage, that must NOT become
                // the released verdict — the batched text A/B is not calibrated
                // against tau. A ladder-skipped claim still takes the ordinary
                // calibrated `claim_violation_joint` below, on `judged ==
                // shared`, which is exactly the call the baseline makes for it.
                let batch_v = if config::gate_batch_verify_enabled() || shadow_mode {
                    batched_support.get(claim_idx).and_then(|v| *v)
                } else {
                    None
                };
                let vp_opt = if shadow_mode {
                    // SHADOW: keep BASELINE behavior (calibrated per-claim) but log
                    // the batched verdict alongside so batch-vs-calibrated agreement
                    // can be scored without changing any answer.
                    let cal =
                        claim_violation_joint(&inference, claim, &judged, cap, n_shared, posture)
                            .await;
                    dbg(&format!(
                        "shadow claim {claim_idx}: batch={batch_v:?} cal_vp={cal:?} cal_supported={:?}",
                        cal.map(|vp| vp < tau)
                    ));
                    cal
                } else {
                    match batch_v {
                        Some(true) => Some(0.0),  // batch: supported → vp below tau
                        Some(false) => Some(1.0), // batch: unsupported → flagged (vp ≥ tau)
                        None => {
                            claim_violation_joint(
                                &inference, claim, &judged, cap, n_shared, posture,
                            )
                            .await
                        }
                    }
                };
                // The counterfactual, logged next to the production verdict.
                // Nothing here feeds `vp_opt` — the released answer is
                // untouched; this only prices what the re-search bought.
                if let (Some(so), Some(vp), false) =
                    (shared_only.as_ref(), vp_opt, extra.is_empty())
                {
                    let vp_wo =
                        claim_violation_joint(&inference, claim, so, so.len(), n_shared, posture)
                            .await;
                    match vp_wo {
                        Some(vp_wo) => tracing::info!(
                            target: "grounding_gate",
                            event = "claim_search_shadow",
                            claim = %claim.chars().take(90).collect::<String>(),
                            extras = extra.len(),
                            n_shared,
                            vp_production = format!("{vp:.3}").as_str(),
                            vp_chunks_only = format!("{vp_wo:.3}").as_str(),
                            delta = format!("{:.3}", vp_wo - vp).as_str(),
                            tau = format!("{tau:.3}").as_str(),
                            verdict_flips = (vp < tau) != (vp_wo < tau),
                            rescued = (vp < tau) && (vp_wo >= tau),
                            newly_failed = (vp >= tau) && (vp_wo < tau),
                            // THE LADDER'S SAFETY, MEASURED RATHER THAN ARGUED.
                            // The ladder skips stage 2 on `triage == Some(true)`,
                            // but `triage` is the BATCHED text A/B while a rescue
                            // is defined against the CALIBRATED forced-choice.
                            // Two different instruments, so "a rescue always
                            // reaches stage 2" is an empirical claim about their
                            // agreement, not a property of the definition.
                            // `lost_rescue` counts the case that breaks it: a
                            // real rescue on a claim the ladder would have
                            // skipped. Sum it over a bank — nonzero means the
                            // ladder is lossy and must not go default-on.
                            triage = ?triage,
                            ladder_would_skip = triage == Some(true),
                            triage_agrees = ?triage.map(|t| t == (vp_wo < tau)),
                            lost_rescue = triage == Some(true)
                                && (vp < tau)
                                && (vp_wo >= tau),
                            "claim search shadow: with re-search vs prompt chunks alone (no answer changed)"
                        ),
                        None => tracing::info!(
                            target: "grounding_gate",
                            event = "claim_search_shadow",
                            claim = %claim.chars().take(90).collect::<String>(),
                            extras = extra.len(),
                            vp_production = format!("{vp:.3}").as_str(),
                            vp_chunks_only = "unavailable",
                            "claim search shadow: counterfactual judge returned no verdict"
                        ),
                    }
                }
                match vp_opt {
                    Some(vp) => {
                        dbg(&format!("longform claim vp={vp:.3} {claim:?}"));
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimVerdict {
                                index: claim_idx,
                                supported: vp < tau,
                            },
                        );
                        if vp >= tau {
                            failed.push(FailedClaim {
                                claim: claim.clone(),
                                evidence: extra,
                            });
                        }
                    }
                    None => {
                        // Unverifiable claim — fail open per claim; the
                        // row still resolves (it ships unflagged).
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimVerdict {
                                index: claim_idx,
                                supported: true,
                            },
                        );
                    }
                }
            }
            // Holistic supporting-specifics scan: catches the fabricated
            // details the load-bearing claim extraction misses (misattribution,
            // fake values, phantom section refs). One extra judge pass over the
            // WHOLE text vs the FULL evidence; its findings join `failed` and
            // ride the same rewrite/annotate path. Each flagged specific gets a
            // claim-conditioned search so the rewrite has corrective material —
            // which ALSO self-corrects a false positive: a truly-grounded
            // specific gets its grounding passage back, so the rewrite keeps it.
            if specifics_scan_enabled() {
                if let Some(specifics) = scan_unsupported_specifics(
                    &inference,
                    question,
                    &text,
                    &leaf_chunks,
                    budget,
                    posture,
                )
                .await
                {
                    for spec in specifics {
                        // Citations are validated by the deterministic snap pass
                        // BEFORE this audit — a scan finding about a `[Source:]`
                        // marker is out of its jurisdiction (observed 2026-07-01:
                        // the scan flagged REAL label citations, which then read
                        // as self-indictment in the verification note).
                        if spec.to_lowercase().contains("[source:") {
                            continue;
                        }
                        // Same jurisdiction rule as the claim loop: the
                        // answer's own honesty meta-language is exempt.
                        if judge::is_self_referential_decline(&spec) {
                            continue;
                        }
                        // Skip specifics already surfaced by the per-claim audit.
                        if failed
                            .iter()
                            .any(|f| f.claim.contains(&spec) || spec.contains(&f.claim))
                        {
                            continue;
                        }
                        let corrective = match &searcher {
                            Some(s) => s.search(&spec).await,
                            None => Vec::new(),
                        };
                        dbg(&format!(
                            "specifics_scan flagged {:?} (corrective_hits={})",
                            spec.chars().take(60).collect::<String>(),
                            corrective.len()
                        ));
                        failed.push(FailedClaim {
                            claim: spec,
                            evidence: corrective,
                        });
                    }
                }
            }
            // Sentence-level identifier sweep: the vetoes above only see
            // EXTRACTED claims, and ghost identifiers ride non-load-bearing
            // sentences the extractor never surfaces (gen75d s2: `cmd_init` /
            // `found.rs`, receipt-absent from the corpus, released inside a
            // rewrite despite the claim-level veto). Sweep every sentence of
            // the text with the same scoped checks; hits become synthetic
            // failed claims and ride the existing rewrite/annotate ladder.
            for sentence in text.split(['.', '\n']) {
                let sentence = sentence.trim();
                if sentence.chars().count() < 20 {
                    continue;
                }
                let hit = judge::absent_identifier_attribution(sentence, &hay_lower)
                    .or_else(|| judge::absent_name_attribution(sentence, &hay_lower));
                if let Some(ident) = hit {
                    if failed.iter().any(|f| f.claim.contains(&ident)) {
                        continue;
                    }
                    dbg(&format!(
                        "longform sentence sweep VETOED {ident:?} (absent from evidence)"
                    ));
                    let synthetic = format!(
                        "The answer references \"{ident}\", which does not appear in the sources."
                    );
                    let extra = match &searcher {
                        Some(s) => s.search(&synthetic).await,
                        None => Vec::new(),
                    };
                    failed.push(FailedClaim {
                        claim: synthetic,
                        evidence: extra,
                    });
                }
            }
            let audited: Vec<String> = claims.into_iter().take(budget).collect();
            Some((text, audited, failed))
        }
    };

    let draft_backup = draft.clone();
    let Some((text, audited, failed)) = audit(draft, false).await else {
        // Claim-list extraction failed — fail open with the draft.
        return GateOutcome {
            text: draft_backup,
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "judge_failed_open", "retried": false,
                "threshold": tau, "mode": "per_claim",
            }),
            claims: Vec::new(),
        };
    };
    let n_claims = audited.len();
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
            text,
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "released", "retried": false,
                "claims_checked": n_claims, "failed_claims": [],
                "threshold": tau, "mode": "per_claim",
            }),
            claims: longform_claims(&audited, &failed),
        };
    }
    if !profile.retry {
        // Verify-only surfaces: annotate the draft with the failed
        // claims — no second synthesis. The caller decides whether
        // an annotated draft is acceptable (Refinement keeps the
        // prior verified answer instead).
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims.saturating_sub(failed.len()),
                flagged: failed.len(),
            },
        );
        let claim_records = longform_claims(&audited, &failed);
        let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
        let note = verification_note(&failed_claims);
        return GateOutcome {
            text: append_note(text, &note),
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "annotated_no_retry", "retried": false,
                "claims_checked": n_claims, "failed_claims": failed_claims,
                "threshold": tau, "mode": "per_claim",
            }),
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
    // unsupported claims. When most claims fail the draft is fundamentally
    // broken — a coherent full re-synthesis beats a Frankenstein of patched
    // sentences (and saves little), so cap surgery at a small failure count
    // (env-tunable: SOVEREIGN_SURGICAL_MAX_FAILURES, default 3).
    let surgical_cap = std::env::var("SOVEREIGN_SURGICAL_MAX_FAILURES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(3);
    // Corrected text: surgical span-edits when every failed claim maps, else a
    // full re-synthesis. EITHER result runs the FULL re-audit ladder below — an
    // earlier scoped re-audit (verify only the changed spans) was faster but
    // leaked a GK-caveated fabrication the holistic scan catches (calibration
    // 2026-07-17, CONFAB-LEAKED 0→1), so surgery now only changes HOW the
    // corrected text is produced, never the safety floor.
    let second: String = 'produce: {
        if config::surgical_rewrite_enabled() && !failed.is_empty() && failed.len() <= surgical_cap
        {
            let pairs: Vec<(String, Vec<String>)> = failed
                .iter()
                .map(|f| (f.claim.clone(), f.evidence.clone()))
                .collect();
            if let Some(edited) =
                surgical::surgical_rewrite(inference, base_request, &text, &pairs).await
            {
                dbg(&format!(
                    "surgical rewrite applied — full re-audit follows ({} failed of {n_claims})",
                    failed.len()
                ));
                break 'produce edited;
            }
        }
        // Full re-synthesis fallback (flag off, too many failures, or surgery
        // could not confidently map a claim).
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
        match inference.complete(&rewrite_req).await {
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
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimCheckComplete {
                        confirmed: n_claims.saturating_sub(failed.len()),
                        flagged: failed.len(),
                    },
                );
                let claim_records = longform_claims(&audited, &failed);
                let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
                let note = verification_note(&failed_claims);
                return GateOutcome {
                    text: append_note(text, &note),
                    meta: serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "annotated_rewrite_error", "retried": false,
                        "claims_checked": n_claims, "failed_claims": failed_claims,
                        "threshold": tau, "mode": "per_claim",
                    }),
                    claims: claim_records,
                };
            }
        }
    };

    let second_backup = second.clone();
    match audit(second, true).await {
        Some((text2, audited2, failed2)) if failed2.is_empty() => {
            let n2 = audited2.len();
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete {
                    confirmed: n2,
                    flagged: 0,
                },
            );
            GateOutcome {
                text: text2,
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "rewrite_released", "retried": true,
                    "claims_checked": n2, "failed_claims": [],
                    "threshold": tau, "mode": "per_claim",
                }),
                claims: longform_claims(&audited2, &failed2),
            }
        }
        Some((text2, audited2, failed2)) => {
            let n2 = audited2.len();
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete {
                    confirmed: n2.saturating_sub(failed2.len()),
                    flagged: failed2.len(),
                },
            );
            let claim_records = longform_claims(&audited2, &failed2);
            let failed_claims: Vec<String> = failed2.into_iter().map(|f| f.claim).collect();
            let note = verification_note(&failed_claims);
            GateOutcome {
                text: append_note(text2, &note),
                meta: serde_json::json!({
                    "action": "rewrite_annotated", "retried": true,
                    "claims_checked": n2, "failed_claims": failed_claims,
                    "threshold": tau, "mode": "per_claim",
                }),
                claims: claim_records,
            }
        }
        None => GateOutcome {
            text: second_backup,
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "rewrite_released_unverified", "retried": true,
                "threshold": tau, "mode": "per_claim",
            }),
            claims: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::error::{Error, Result};
    use crate::types::CompletionResponse;
    use crate::types::{Depth, ProviderCapabilities};
    use futures::Stream;
    use std::pin::Pin;

    fn chunk_with(corpus_id: &str, chunk_id: Option<u64>) -> corpus_engine::ScoredChunk {
        corpus_engine::ScoredChunk {
            content: "text".into(),
            title: None,
            url: None,
            corpus_id: corpus_id.into(),
            score: 1.0,
            metadata: std::collections::HashMap::new(),
            chunk_id,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    /// A target is a pure projection of what retrieval already knows — and
    /// half a handle is worse than none, because a chunk id is unique only
    /// WITHIN a corpus. A row missing either half would be a citation the
    /// reading surface fails to deref at click time, after the reader has
    /// already been told it is openable.
    #[test]
    fn a_target_needs_both_halves_of_the_handle() {
        let targets = gate_evidence_targets(&[
            chunk_with("chaos-saltgrass", Some(41)),
            // Synthetic / atlas-virtual chunk: no stable row id.
            chunk_with("chaos-saltgrass", None),
            // Corpus id absent — the chunk id alone resolves nothing.
            chunk_with("   ", Some(9)),
        ]);
        assert_eq!(targets.len(), 3, "targets stay PARALLEL to chunks");
        let first = targets[0].as_ref().expect("a real chunk is openable");
        assert_eq!(first.corpus_id, "chaos-saltgrass");
        assert_eq!(first.chunk_id, 41);
        assert_eq!(targets[1], None, "no row id, nothing to open");
        assert_eq!(
            targets[2], None,
            "a chunk id without a corpus resolves nothing"
        );
    }

    /// Targets are index-parallel to `chunks` and must survive the same
    /// summary filter, or a click opens a passage the reader never saw.
    /// This is the alignment argument `chunk_locators` documents, with a
    /// worse failure mode.
    #[test]
    fn targets_stay_aligned_with_the_chunks_they_name() {
        let chunks = vec![
            chunk_with("ledger", Some(1)),
            chunk_with("ledger", Some(2)),
            chunk_with("ledger", Some(3)),
        ];
        let parts = gate_evidence_with_sources(&chunks);
        assert_eq!(parts.chunk_targets.len(), parts.chunks.len());
        for (i, t) in parts.chunk_targets.iter().enumerate() {
            assert_eq!(
                t.as_ref().map(|t| t.chunk_id),
                Some(i as u64 + 1),
                "slot {i} names the wrong chunk"
            );
        }
    }

    /// Prompt-routing mock for the gate's judge calls: claim
    /// extraction returns a fixed claim; every forced-choice support
    /// check returns `support` (as a logprob A/B distribution).
    struct GateMock {
        support: bool,
    }

    #[async_trait::async_trait]
    impl crate::traits::InferenceProvider for GateMock {
        async fn complete(
            &self,
            request: &crate::types::CompletionRequest,
        ) -> Result<CompletionResponse> {
            // P4-D contract: every judge call routed through this mock must
            // carry the OICP Judge envelope and NOT the old
            // `model_id: "primary"` pin (a latent privacy hole). This is the
            // capture-stub assertion — it fires on the real gate paths the
            // tests below drive (claim extraction + forced-choice support).
            assert!(
                request.model_id.is_none(),
                "P4-D: judge request must not pin model_id; got {:?}",
                request.model_id
            );
            let judge_oicp = request
                .oicp
                .as_ref()
                .expect("P4-D: judge request must carry an OICP Judge envelope");
            assert_eq!(
                judge_oicp.effective_latency_class(),
                crate::oicp::LatencyClass::Normal,
                "P4-D: Judge envelope latency class"
            );
            let text = if request
                .structured_output
                .as_ref()
                .map(|s| s.to_string().contains("x_forced_choice"))
                .unwrap_or(false)
            {
                if self.support {
                    r#"{"A": 0.98, "B": 0.02}"#.to_string()
                } else {
                    r#"{"A": 0.02, "B": 0.98}"#.to_string()
                }
            } else if request.prompt.contains("single central factual claim") {
                "The shop is located on Crescent Lane.".to_string()
            } else if request.prompt.contains("List the SPECIFIC factual claims") {
                // Longform per-claim extractor (gate_longform's audit).
                "The shop is located on Crescent Lane.\nThe shop sells loose-leaf tea.".to_string()
            } else if request
                .prompt
                .contains("Compare the ANSWER against the EVIDENCE")
            {
                // Specifics scan: nothing unsupported — keeps the
                // longform progress tests pinned to the claim loop.
                "NONE".to_string()
            } else {
                "unexpected synthesis call".to_string()
            };
            Ok(CompletionResponse {
                text,
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "gate-mock".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &crate::types::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("GateMock: no streaming".into()))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    /// Mock for the retry-decline path: the first verify extracts a claim
    /// and judges it unsupported (forcing the retry), the retry synthesis
    /// returns a pure decline, and the re-verify extracts NO_CLAIM from it.
    struct RetryDeclineMock;

    #[async_trait::async_trait]
    impl crate::traits::InferenceProvider for RetryDeclineMock {
        async fn complete(
            &self,
            request: &crate::types::CompletionRequest,
        ) -> Result<CompletionResponse> {
            let p = &request.prompt;
            let text = if request
                .structured_output
                .as_ref()
                .map(|s| s.to_string().contains("x_forced_choice"))
                .unwrap_or(false)
            {
                // Forced-choice support judge: unsupported.
                r#"{"A": 0.02, "B": 0.98}"#.to_string()
            } else if p.contains("single central factual claim") {
                if p.contains("I don't have reliable information") {
                    // Re-extraction over the retry's decline text.
                    "NO_CLAIM".to_string()
                } else {
                    "The shop is located on Crescent Lane.".to_string()
                }
            } else {
                // The retry synthesis itself → a pure decline.
                "I don't have reliable information on this.".to_string()
            };
            Ok(CompletionResponse {
                text,
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "gate-mock".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &crate::types::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented(
                "RetryDeclineMock: no streaming".into(),
            ))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    /// Mock for the NO_CLAIM extraction path: the claim extractor finds
    /// nothing to audit (a decline or an unextractable reply), so the verify
    /// ladder waves the text through as a NO_CLAIM release.
    struct NoClaimGateMock;

    #[async_trait::async_trait]
    impl crate::traits::InferenceProvider for NoClaimGateMock {
        async fn complete(
            &self,
            request: &crate::types::CompletionRequest,
        ) -> Result<CompletionResponse> {
            assert!(
                request.model_id.is_none(),
                "P4-D: judge request must not pin model_id; got {:?}",
                request.model_id
            );
            Ok(CompletionResponse {
                text: "NO_CLAIM".to_string(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "gate-mock".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &crate::types::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented(
                "NoClaimGateMock: no streaming".into(),
            ))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn refinement_evidence() -> EvidenceContext {
        EvidenceContext {
            chunks: vec!["The shop sits on Harbour Row, by the quay.".to_string()],
            source_labels: Vec::new(),
            chunk_labels: Vec::new(),
            chunk_locators: Vec::new(),
            chunk_targets: Vec::new(),
            chunk_sources: Vec::new(),
            searcher: None,
            entity_anchored: false,
            top_similarity: None,
        }
    }

    #[test]
    fn claim_budget_scales_with_length_within_bounds() {
        // Short answers keep the floor (the surface's min_claims).
        assert_eq!(claim_budget(0, 4), 4);
        assert_eq!(claim_budget(500, 4), 4);
        assert_eq!(claim_budget(2_399, 4), 4, "under 4*600 stays at floor");
        // The empirical fabrication distribution now scales meaningfully — at
        // the old 900/claim these got budget 4-9 (3630 got NO lift).
        assert_eq!(
            claim_budget(3_630, 4),
            6,
            "3630-char fabrication -> 6, not 4"
        );
        assert_eq!(claim_budget(4_550, 4), 7, "4550-char fabrication -> 7");
        // Very long answers are capped so per-claim judge latency stays bounded.
        assert_eq!(
            claim_budget(8_571, 4),
            10,
            "8571-char essay -> capped at 10"
        );
        assert_eq!(claim_budget(usize::MAX, 4), 10);
        // The floor is the surface's min, not a hardcoded 4.
        assert_eq!(claim_budget(500, 1), 1);
    }

    #[test]
    fn answer_declines_skips_honest_abstentions_only() {
        // The exact short-band answers the specifics scan flagged as GOOD-but-
        // FLAGGED on 2026-07-01 — all honest abstentions the guard must SKIP so
        // it never wastes a corrective retry re-abstaining them.
        for decline in [
            "I don't have reliable information on the specific four authors listed for Chapter E.",
            "I am not certain of the value of `SWAP_THRESHOLD`. The provided sources do not contain this.",
            "The provided knowledge base sources do not contain this specific constant or file.",
            "I looked through the 12 passages your sources turned up for this, but none of them actually cover it — so I'd rather not guess.",
            "Based on the provided knowledge base, I do not have information regarding a character named \"Winnie\".",
            "The provided Rust snippets do not contain any assignment to a variable named `b`.",
            grounded_abstention("x", 12).as_str(),
        ] {
            assert!(answer_declines(decline), "should skip decline: {decline:?}");
        }
        // Real ASSERTING short answers the guard MUST scan — including the two
        // confirmed fabrications the guard exists to catch.
        for assert_ans in [
            "David Hart\n\nGrounded in the source: \"David Hart, Chief Operations Officer, Knowledge Process Software\"",
            "The most important thing is what Tokei does: it shows file-level stats (`--files`) and sorting (`--sort`).",
            "The three operations are index_stats, extract_shard, and merge_shards.",
        ] {
            assert!(!answer_declines(assert_ans), "should scan assertion: {assert_ans:?}");
        }
    }

    #[test]
    fn released_pure_decline_separates_declines_from_caveated_answers() {
        // The P0 target shape: a provenance-flagged decline that asserts
        // nothing (observed on `ood-australia-capital` over 10 distractors).
        for decline in [
            "I don't have reliable information in my knowledge base about the capital of Australia.",
            "I do not have reliable information on that in your sources.",
            grounded_abstention("x", 12).as_str(),
        ] {
            assert!(
                released_pure_decline(decline),
                "pure decline must reclassify: {decline:?}"
            );
        }
        // Caveated PARAMETRIC ANSWERS — the decline-then-answer shapes that
        // must keep releasing (chaos hybrid honesty: OOD questions are meant
        // to be answered from general knowledge with the caveat).
        for answer in [
            "Not in your sources — from general knowledge: The capital of Australia is Canberra.",
            "I don't have reliable information in my knowledge base, but from general knowledge: Canberra is the capital of Australia.",
            "The capital of Australia is Canberra.",
        ] {
            assert!(
                !released_pure_decline(answer),
                "caveated/substantive answer must release: {answer:?}"
            );
        }
    }

    /// EPISTEMIC_STATE P0: a NO_CLAIM release whose text is a pure decline is
    /// reclassified `abstained_decline` — the ledger then derives
    /// `CannotKnowFromHere` and the coverage probe runs. The model's own
    /// decline prose ships unchanged.
    #[tokio::test]
    async fn no_claim_pure_decline_release_becomes_abstention() {
        let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(NoClaimGateMock);
        let profile = GateSurface::Refinement.profile();
        let draft =
            "I don't have reliable information in my knowledge base about the capital of Australia."
                .to_string();
        let outcome = gate_answer(
            &inference,
            "What is the capital of Australia?",
            draft.clone(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("abstained_decline")
        );
        assert_eq!(outcome.text, draft, "the model's own decline prose ships");
        assert!(outcome.claims.is_empty(), "a decline asserts nothing");
    }

    /// A retry that produces a pure decline (re-extracted as NO_CLAIM,
    /// vp=0) must NOT release as `retry_released` with the original claim
    /// marked supported — that forges a Verified holding for a claim the
    /// final text no longer asserts (observed: ood-table-salt shipped
    /// ledger verdict `grounded` on "I don't have reliable information on
    /// this.", 2026-07-20). It is an abstention.
    #[tokio::test]
    async fn retry_decline_is_abstention_not_supported_release() {
        let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(RetryDeclineMock);
        let profile = GateSurface::KnowledgeQuery.profile();
        assert!(profile.retry, "this path needs a retry-capable surface");
        let outcome = gate_answer(
            &inference,
            "Where is the shop?",
            "The shop is on Crescent Lane.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("abstained_decline")
        );
        assert_eq!(outcome.text, "I don't have reliable information on this.");
        assert!(
            !outcome.claims.iter().any(|c| c.supported),
            "no claim may be marked supported by a NO_CLAIM decline retry: {:?}",
            outcome.claims
        );
    }

    /// The exclusion half: a caveated general-knowledge ANSWER that extracts
    /// NO_CLAIM must keep releasing — reclassifying it would score an
    /// answered turn as an abstention (the unfaithful-proxy failure the
    /// I2-C parity gate exists to catch).
    #[tokio::test]
    async fn no_claim_caveated_gk_answer_still_releases() {
        let inference: Arc<dyn crate::traits::InferenceProvider> = Arc::new(NoClaimGateMock);
        let profile = GateSurface::Refinement.profile();
        let draft =
            "Not in your sources — from general knowledge: The capital of Australia is Canberra."
                .to_string();
        let outcome = gate_answer(
            &inference,
            "What is the capital of Australia?",
            draft.clone(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        assert_eq!(outcome.text, draft);
    }

    /// The Phase-6 invariant's gate half: verify-only (retry: false)
    /// on an unsupported claim must return `abstained_no_retry` — the
    /// caller (collaboration refinement) keeps the verified original.
    #[tokio::test]
    async fn verify_only_failure_is_abstained_no_retry() {
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: false });
        let profile = GateSurface::Refinement.profile();
        assert!(!profile.retry);
        let outcome = gate_answer(
            &inference,
            "Where is the shop?",
            "The shop is on Crescent Lane.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("abstained_no_retry")
        );
        // grounded_abstention was rewritten (2026-06-17) to stop restating the
        // rejected claim verbatim (it leaked the fabrication + read as "answered"
        // to the primary judge), then re-toned (2026-06-30) to drop the abrupt
        // "so I'm not going to state one" lecture for a warm, helpful refusal,
        // then re-scoped (2026-07-08) from a universal negative about the sources
        // ("none of them cover it") to a self-scoped hedge ("I couldn't confirm")
        // so a mis-abstain isn't a FALSE claim about the sources. The action is
        // the invariant; the wording is graceful and source-honest.
        assert!(outcome.text.starts_with("I couldn't confirm"));
        assert!(!outcome.text.contains("not going to state"));
        // Must NOT assert a universal negative about the sources' content.
        assert!(!outcome.text.contains("none of them"));
        assert!(!outcome.text.contains("not recorded there"));
    }

    /// Supported claims release unchanged under verify-only.
    #[tokio::test]
    async fn verify_only_supported_claim_releases() {
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: true });
        let profile = GateSurface::Refinement.profile();
        let draft = "The shop is on Harbour Row.".to_string();
        let outcome = gate_answer(
            &inference,
            "Where is the shop?",
            draft.clone(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        assert_eq!(outcome.text, draft);
    }

    /// Drain every frame the gate pushed onto a progress channel.
    async fn drain_frames(
        mut rx: tokio::sync::mpsc::Receiver<crate::types::NarrationPhase>,
    ) -> Vec<crate::types::NarrationPhase> {
        let mut frames = Vec::new();
        while let Some(f) = rx.recv().await {
            frames.push(f);
        }
        frames
    }

    /// Short path, supported claim: the progress channel carries the
    /// desktop verification panel's contract — claim list opens, the
    /// verdict stamps, the completion frame closes the pass.
    #[tokio::test]
    async fn short_path_progress_frames_on_release() {
        use crate::types::NarrationPhase;
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: true });
        let profile = GateSurface::Refinement.profile();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let outcome = gate_answer_with_progress(
            &inference,
            "Where is the shop?",
            "The shop is on Harbour Row.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
            Some(&tx),
        )
        .await;
        drop(tx);
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        let frames = drain_frames(rx).await;
        assert!(
            matches!(
                &frames[0],
                NarrationPhase::ClaimCheckStart { claims, recheck: false } if claims.len() == 1
            ),
            "first frame should open the one-claim check: {frames:?}"
        );
        assert!(matches!(
            frames[1],
            NarrationPhase::ClaimVerdict {
                index: 0,
                supported: true
            }
        ));
        assert!(matches!(
            frames[2],
            NarrationPhase::ClaimCheckComplete {
                confirmed: 1,
                flagged: 0
            }
        ));
        assert_eq!(frames.len(), 3);
    }

    /// Short path, verify-only failure: verdict stamps unsupported and
    /// the completion frame reports the flagged claim.
    #[tokio::test]
    async fn short_path_progress_frames_on_abstention() {
        use crate::types::NarrationPhase;
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: false });
        let profile = GateSurface::Refinement.profile();
        assert!(!profile.retry);
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let outcome = gate_answer_with_progress(
            &inference,
            "Where is the shop?",
            "The shop is on Crescent Lane.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
            Some(&tx),
        )
        .await;
        drop(tx);
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("abstained_no_retry")
        );
        let frames = drain_frames(rx).await;
        assert!(matches!(
            &frames[0],
            NarrationPhase::ClaimCheckStart { recheck: false, .. }
        ));
        assert!(matches!(
            frames[1],
            NarrationPhase::ClaimVerdict {
                index: 0,
                supported: false
            }
        ));
        assert!(matches!(
            frames[2],
            NarrationPhase::ClaimCheckComplete {
                confirmed: 0,
                flagged: 1
            }
        ));
    }

    /// Longform path: the audit opens with the extracted claim LIST,
    /// stamps every claim in order, and closes with the totals — the
    /// full counter-card Check-station sequence.
    #[tokio::test]
    async fn longform_progress_frames_stamp_each_claim() {
        use crate::types::NarrationPhase;
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: true });
        let profile = GateSurface::Refinement.profile();
        // Force the per-claim ladder regardless of the profile's pivot
        // (and of any SOVEREIGN_LONGFORM_CHARS ambient override — the
        // draft is longer than both).
        let pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(profile.longform_chars)
            .max(profile.longform_chars);
        let draft = "The shop sits on Harbour Row, by the quay. ".repeat(pivot / 40 + 2);
        assert!(draft.chars().count() > pivot);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let outcome = gate_answer_with_progress(
            &inference,
            "Tell me about the shop.",
            draft,
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
            Some(&tx),
        )
        .await;
        drop(tx);
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        let frames = drain_frames(rx).await;
        // Claim list (two mock claims), then one verdict per claim in
        // index order, then the completion totals.
        assert!(
            matches!(
                &frames[0],
                NarrationPhase::ClaimCheckStart { claims, recheck: false } if claims.len() == 2
            ),
            "expected two-claim list first: {frames:?}"
        );
        assert!(matches!(
            frames[1],
            NarrationPhase::ClaimVerdict {
                index: 0,
                supported: true
            }
        ));
        assert!(matches!(
            frames[2],
            NarrationPhase::ClaimVerdict {
                index: 1,
                supported: true
            }
        ));
        assert!(matches!(
            frames[3],
            NarrationPhase::ClaimCheckComplete {
                confirmed: 2,
                flagged: 0
            }
        ));
    }

    fn fc(claim: &str, evidence: &[&str]) -> FailedClaim {
        FailedClaim {
            claim: claim.to_string(),
            evidence: evidence.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn rewrite_note_includes_corrective_passages() {
        let note = rewrite_system_note(&[fc(
            "Verloc instructs The Professor to bomb the Observatory.",
            &["Mr Vladimir tells Verloc the attack must be against the first meridian."],
        )]);
        assert!(note.contains("Verloc instructs The Professor"));
        assert!(
            note.contains("first meridian"),
            "corrective passage must reach the rewrite prompt"
        );
        assert!(note.contains("REPLACE"));
    }

    #[test]
    fn rewrite_note_marks_claims_with_no_corpus_support() {
        let note = rewrite_system_note(&[fc("Mrs Veronica Verloc shoots her husband.", &[])]);
        assert!(note.contains("no corpus passage states this"));
    }

    #[test]
    fn rewrite_note_caps_evidence_per_claim_and_length() {
        let long = "x".repeat(2_000);
        let note = rewrite_system_note(&[fc("c", &[&long, &long, &long])]);
        // 2 passages max, 700 chars each — the note stays prompt-sized.
        assert!(note.matches("  | ").count() <= 2);
        assert!(note.len() < 2_200);
    }

    #[test]
    fn rewrite_note_commits_answer_shape_rules() {
        let note = rewrite_system_note(&[fc("c", &[])]);
        assert!(note.contains("Do not open with what the sources lack"));
        // The rewrite must not mint new claims about the sources (observed
        // 2026-07-01: a rewrite replaced one misattribution with "the text
        // cites Woolf's work" — a fresh unsupported claim ABOUT the text).
        assert!(note.contains("Never add a NEW statement about what the sources say"));
    }

    #[test]
    fn verification_note_dedupes_caps_and_stays_unquoted() {
        let long = "x".repeat(200);
        let claims = vec![
            "Paul Samuelson admitted defeat around 1963".to_string(),
            "\"Paul Samuelson admitted defeat around 1963\"".to_string(), // dup modulo quotes
            "[unverified excerpt: ships cannot pay tolls at sea]".to_string(),
            long.clone(),
            String::new(),
        ];
        let note = verification_note(&claims);
        // Deduped: the claim appears once, as a plain list item.
        assert_eq!(note.matches("Samuelson").count(), 1);
        assert!(note.contains("- Paul Samuelson admitted defeat around 1963"));
        // The app's own wrapper is unwrapped to its content.
        assert!(note.contains("- ships cannot pay tolls at sea"));
        assert!(!note.contains("unverified excerpt:"));
        // UNQUOTED by design: a curly-quoted item reads as a quotation claim to
        // the post-synthesis quote guardrail, which demotes non-verbatim spans
        // to "[unverified excerpt: …]" — mangling the note (probed 2026-07-01).
        assert!(!note.contains('“') && !note.contains('”'));
        // Long item capped with an ellipsis; empty item dropped.
        assert!(note.contains(&format!("{}…", "x".repeat(160))));
        // Plain language — never judge vocabulary.
        assert!(!note.to_lowercase().contains("fabricated"));
    }
}
