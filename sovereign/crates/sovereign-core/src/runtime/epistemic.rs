//! Epistemic-ledger assembly — deterministic collation of the turn's
//! already-computed judgments into an [`EpistemicState`].
//!
//! Design: `sovereign/docs/EPISTEMIC_STATE.md`. Two invariants are
//! load-bearing here and pinned by the unit tests below:
//!
//! - **I2 — the verdict is derived, never model-asserted.**
//!   [`derive_verdict`] is a pure function of the assembled inputs.
//! - **I5 — assembly never blocks or degrades the answer.** No model
//!   calls, no I/O; the assembler runs post-release on data the turn
//!   already produced. The `SOVEREIGN_EPISTEMIC_STATE=0` kill switch
//!   suppresses assembly entirely.
//!
//! Milestone A scope (P0): `demands`/`gaps` are empty — they arrive
//! with the deterministic demand builder (Milestone B). Holdings come
//! from the grounding gate's retained claim records, the referenced
//! memory recall, and the plan's general-knowledge signal.

use crate::runtime::grounding::GateClaim;
use crate::runtime::types::{GkReason, RecallVerificationProv, RecalledMemoryProv};
use crate::types::{
    CoverageLevel, Demand, DemandFacet, EpistemicState, Gap, GapCoverage, Holding, Intent,
    MemoryBand, Provenance, TurnVerdict, Verification, EPISTEMIC_STATE_VERSION,
};

/// Kill switch: `SOVEREIGN_EPISTEMIC_STATE=0|false|off|no` disables
/// ledger assembly (the metadata key is simply absent). Default ON —
/// assembly is pure collation with no latency or model cost.
pub(crate) fn epistemic_state_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_EPISTEMIC_STATE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Everything the assembler collates. All fields are turn-local data
/// already computed by the pipeline — the assembler adds no judgment.
///
/// No `Default`: the only way in is [`EpistemicInputs::over`], which takes
/// the turn's [`PoolContext`], so no ledger site can leave its pool (and
/// with it the member attribution) on a default.
pub(crate) struct EpistemicInputs<'a> {
    /// The gate's `grounding_gate` meta blob (reads `action` only).
    pub gate_meta: Option<&'a serde_json::Value>,
    /// The gate's retained per-claim records.
    pub gate_claims: Option<&'a [GateClaim]>,
    /// Why the plan answered from general knowledge, when it did.
    pub general_knowledge: Option<GkReason>,
    /// The evidence pool the answer drew on; see [`pool_context`].
    pub pool: PoolContext,
    /// Memories recalled into the turn (relational surfaces).
    pub recalled: &'a [RecalledMemoryProv],
    /// Outcome of the recall-grounding verifier, when it ran.
    pub recall_verification: Option<&'a RecallVerificationProv>,
    /// Demand set with coverage stamps (Milestone B; empty in P0).
    pub demands: Vec<Demand>,
    /// Gap rows (Milestone B; empty in P0).
    pub gaps: Vec<Gap>,
    /// Deterministic tool-derived holdings the caller already computed
    /// (I2-A: the complex-task surface passes the `parcel_analytics`
    /// cited figures here — the "no confabulated numbers" guarantee made
    /// visible on the ledger). Each is emitted as a
    /// [`Provenance::ToolDerived`] holding; skipped on abstained turns.
    pub tool_holdings: Vec<Holding>,
}

impl<'a> EpistemicInputs<'a> {
    /// Inputs over `pool`, every other field empty.
    pub(crate) fn over(pool: PoolContext) -> Self {
        Self {
            gate_meta: None,
            gate_claims: None,
            general_knowledge: None,
            pool,
            recalled: &[],
            recall_verification: None,
            demands: Vec::new(),
            gaps: Vec::new(),
            tool_holdings: Vec::new(),
        }
    }
}

/// The evidence pool a ledger attributes, built from ONE chunk slice so
/// its corpora and members cannot come from different pools. No
/// `Default`: a parametric turn says so with [`PoolContext::none`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PoolContext {
    /// Distinct corpus ids, order-preserving (empty on parametric turns).
    pub corpora: Vec<String>,
    /// The mesh member each pool chunk came from, one entry per chunk
    /// (`None` = local).
    pub members: Vec<Option<String>>,
}

impl PoolContext {
    /// No evidence pool: a parametric or non-corpus turn.
    pub(crate) fn none() -> Self {
        Self {
            corpora: Vec::new(),
            members: Vec::new(),
        }
    }
}

/// Actions whose release shipped WITHOUT a completed verification
/// (judge unavailable / verdict unparseable) — holdings under these
/// actions are `FailOpen`, per the gate's documented posture.
///
/// **The one decider for "did this release ship unverified"** (ARCH
/// §10.6). The gate's journal funnel (`grounding::gate`) asks the same
/// question to decide which turns must carry a `judge_failure` reason, so
/// the ledger's FailOpen holdings and the reason on the wire can never
/// disagree about which turns they are talking about.
pub(crate) fn action_is_fail_open(action: &str) -> bool {
    matches!(
        action,
        "judge_failed_open" | "retry_released_unverified" | "rewrite_released_unverified"
    )
}

/// Assemble the ledger. Pure collation — no I/O, no inference.
pub(crate) fn assemble_epistemic_state(inputs: EpistemicInputs<'_>) -> EpistemicState {
    let gate_action = inputs
        .gate_meta
        .and_then(|m| m.get("action"))
        .and_then(|a| a.as_str())
        .unwrap_or("");
    let abstained = gate_action.starts_with("abstained");
    let fail_open = action_is_fail_open(gate_action);
    // Per-claim corpus attribution: honest only when the pool is
    // single-corpus (the sealed-notebook common case). Multi-corpus
    // pools carry `corpus_id: None` until claim-level search binding
    // lands (initiative I2).
    let sole_corpus = match inputs.pool.corpora.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    // The same rule for members: named only when every chunk in the
    // pool came from one member; a local or mixed pool stays `None`.
    let sole_member = match inputs.pool.members.split_first() {
        Some((Some(first), rest)) if rest.iter().all(|m| m.as_ref() == Some(first)) => {
            Some(first.clone())
        }
        _ => None,
    };
    tracing::debug!(
        target: "sovereign::epistemic",
        pool_chunks = inputs.pool.members.len(),
        sole_member = ?sole_member,
        "holding member attribution"
    );

    let mut holdings: Vec<Holding> = Vec::new();
    // Gate-audited claims → corpus-provenance holdings. An abstained
    // turn asserts nothing: its failed claims stay in the gate meta
    // (glassbox), not in holdings.
    if !abstained {
        for c in inputs.gate_claims.unwrap_or_default() {
            // A per-claim `unjudged` outranks the gate's action string: a
            // claim nobody judged is FailOpen even on a `released` turn
            // (issue #57 — eight shed judges rendered as eight Verified).
            let verification = if fail_open || c.unjudged {
                Verification::FailOpen
            } else if c.supported {
                Verification::Verified
            } else {
                Verification::FailedOnce
            };
            holdings.push(Holding {
                claim: c.text.clone(),
                provenance: Provenance::Corpus {
                    corpus_id: sole_corpus.clone(),
                    chunk_id: None,
                    member: sole_member.clone(),
                },
                verification,
            });
        }
    }
    // Tool-derived holdings: deterministic figures the system (not the
    // model) originated. An abstained turn asserts nothing, so they are
    // dropped there like gate claims.
    if !abstained {
        holdings.extend(inputs.tool_holdings.iter().cloned());
    }
    // Memory holdings: only the entry the recall verifier ATTRIBUTED
    // the reply to. Recalled-but-unreferenced memories are context,
    // not assertions — recording them as holdings would overclaim.
    if let Some(rv) = inputs.recall_verification {
        if let Some(idx) = rv.referenced {
            if let Some(m) = inputs.recalled.get(idx.saturating_sub(1)) {
                let band = m
                    .confidence
                    .map(crate::memory::band_for_confidence)
                    .unwrap_or(MemoryBand::Tentative);
                holdings.push(Holding {
                    claim: m.content.chars().take(200).collect(),
                    provenance: Provenance::Memory {
                        band,
                        entry_id: m.id.clone(),
                    },
                    verification: if rv.fail_open {
                        Verification::FailOpen
                    } else if rv.grounded {
                        Verification::Verified
                    } else {
                        Verification::FailedOnce
                    },
                });
            }
        }
    }

    let verdict = derive_verdict(
        &holdings,
        abstained,
        inputs.general_knowledge.is_some(),
        !inputs.pool.corpora.is_empty(),
        gate_action.is_empty(),
    );
    // Released passages, read straight off the gate's meta. Collation, not
    // judgment: the gate already decided which quotes it released and which
    // of those resolve to a passage, and re-deriving either here would be a
    // second decider for a fact the gate owns (§10.6).
    //
    // An abstained turn asserts nothing, so it cites nothing — the same rule
    // holdings follow three blocks up. A malformed or absent `citations` key
    // reads as empty, which is the honest degradation: no citation is shown
    // as openable rather than a guess being rendered.
    let citations = if abstained {
        Vec::new()
    } else {
        inputs
            .gate_meta
            .and_then(|m| m.get("citations"))
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default()
    };
    let state = EpistemicState {
        version: EPISTEMIC_STATE_VERSION,
        demands: inputs.demands,
        holdings,
        gaps: inputs.gaps,
        verdict,
        citations,
    };
    let (n_corpus, n_memory) =
        state
            .holdings
            .iter()
            .fold((0usize, 0usize), |acc, h| match h.provenance {
                Provenance::Corpus { .. } => (acc.0 + 1, acc.1),
                Provenance::Memory { .. } => (acc.0, acc.1 + 1),
                _ => acc,
            });
    // Claims that failed a first check and went through revision
    // before release — the retry/rewrite cost this turn actually paid.
    let revised = inputs
        .gate_claims
        .unwrap_or_default()
        .iter()
        .filter(|c| c.failed_once)
        .count();
    tracing::info!(
        target: "epistemic.ledger",
        verdict = ?state.verdict,
        holdings = state.holdings.len(),
        corpus_holdings = n_corpus,
        memory_holdings = n_memory,
        claims_revised = revised,
        demands = state.demands.len(),
        gaps = state.gaps.len(),
        gate_action = %gate_action,
        "epistemic state assembled"
    );
    state
}

/// Derive the turn verdict — a pure function of assembled data
/// (invariant I2: no model ever asserts its own epistemic standing).
///
/// `no_gate` = no grounding gate ran on this turn (as opposed to a
/// gate that ran and failed open).
pub(crate) fn derive_verdict(
    holdings: &[Holding],
    abstained: bool,
    general_knowledge: bool,
    evidence_present: bool,
    no_gate: bool,
) -> TurnVerdict {
    if abstained {
        return TurnVerdict::CannotKnowFromHere;
    }
    let n_corpus = holdings
        .iter()
        .filter(|h| matches!(h.provenance, Provenance::Corpus { .. }))
        .count();
    let n_memory = holdings
        .iter()
        .filter(|h| matches!(h.provenance, Provenance::Memory { .. }))
        .count();
    let n_tool = holdings
        .iter()
        .filter(|h| matches!(h.provenance, Provenance::ToolDerived { .. }))
        .count();
    if general_knowledge && n_corpus == 0 && n_tool == 0 {
        return TurnVerdict::GeneralKnowledge;
    }
    // Any turn that mixes distinct bases (corpus + memory/tool) is Mixed
    // — the answer no longer rests on a single, uniform kind of support.
    let bases = [n_corpus > 0, n_memory > 0, n_tool > 0]
        .iter()
        .filter(|present| **present)
        .count();
    if bases >= 2 {
        return TurnVerdict::Mixed;
    }
    // Tool-only holdings (deterministic figures, no corpus/memory): honest
    // as Mixed — the figures are system-originated, so the turn is neither
    // corpus-Grounded nor a memory/GK recall. It never overclaims Grounded.
    if n_tool > 0 && n_corpus == 0 && n_memory == 0 {
        return TurnVerdict::Mixed;
    }
    match (n_corpus, n_memory) {
        (0, 0) => {
            if evidence_present {
                // Evidence used, nothing audited (un-gated surface or
                // the gate ran and retained no records).
                TurnVerdict::Unverified
            } else if general_knowledge {
                TurnVerdict::GeneralKnowledge
            } else if no_gate {
                TurnVerdict::Unverified
            } else {
                TurnVerdict::GeneralKnowledge
            }
        }
        (c, 0) if c > 0 => {
            if holdings
                .iter()
                .all(|h| h.verification == Verification::Verified)
            {
                TurnVerdict::Grounded
            } else {
                TurnVerdict::Mixed
            }
        }
        (0, _m) => TurnVerdict::MemoryRecall,
        _ => TurnVerdict::Mixed,
    }
}

// ─── Milestone B: deterministic demands + coverage ────────────

/// Build the turn's demand set from signals the pipeline already
/// computed — zero model calls (EPISTEMIC_STATE.md, P1a). Facets:
/// the query itself (always), the entity-boost entities, and the
/// heuristic sub-question decomposition (env-gate-free inner form).
pub(crate) fn build_demands(message: &str, intent: &Intent, entities: &[String]) -> Vec<Demand> {
    let mut demands = vec![Demand {
        facet: DemandFacet::Query,
        text: message.to_string(),
        covered: CoverageLevel::Absent,
    }];
    let push_unique = |demands: &mut Vec<Demand>, facet: DemandFacet, text: &str| {
        let text = text.trim();
        if text.is_empty() || text.eq_ignore_ascii_case(message) {
            return;
        }
        if demands.iter().any(|d| d.text.eq_ignore_ascii_case(text)) {
            return;
        }
        demands.push(Demand {
            facet,
            text: text.to_string(),
            covered: CoverageLevel::Absent,
        });
    };
    for e in entities {
        push_unique(&mut demands, DemandFacet::Entity, e);
    }
    if let Some(subs) =
        crate::runtime::retrieval::query_expansion::decompose_question_inner(message, intent)
    {
        for s in subs {
            push_unique(&mut demands, DemandFacet::SubQuestion, &s);
        }
    }
    demands
}

/// Stamp `Retrieved` coverage against the composed evidence pool.
/// Deterministic v1 (lexical containment, the `merge_select` title
/// precedent): Query = pool non-empty; Entity = the entity's surface
/// form appears in some chunk's title or content; SubQuestion = every
/// substantive token of the sub-query appears in ONE chunk. The
/// `Supported` upgrade happens at assembly, from gate claims.
pub(crate) fn stamp_coverage(demands: &mut [Demand], chunks: &[corpus_index::types::ScoredChunk]) {
    let lowered: Vec<(String, String)> = chunks
        .iter()
        .map(|c| {
            (
                c.title.as_deref().unwrap_or("").to_lowercase(),
                c.content.to_lowercase(),
            )
        })
        .collect();
    for d in demands.iter_mut() {
        let covered = match d.facet {
            DemandFacet::Query => !chunks.is_empty(),
            // Stance poles + section labels cover like an entity: the
            // pole/section surface form appears in some chunk (I4).
            DemandFacet::Entity | DemandFacet::Stance | DemandFacet::Section => {
                let needle = d.text.to_lowercase();
                lowered
                    .iter()
                    .any(|(t, c)| t.contains(&needle) || c.contains(&needle))
            }
            DemandFacet::SubQuestion => {
                let tokens: Vec<String> = d
                    .text
                    .to_lowercase()
                    .split_whitespace()
                    .filter(|t| t.chars().count() >= 4)
                    .map(|t| t.to_string())
                    .collect();
                !tokens.is_empty()
                    && lowered
                        .iter()
                        .any(|(t, c)| tokens.iter().all(|tok| c.contains(tok) || t.contains(tok)))
            }
        };
        if covered {
            d.covered = CoverageLevel::Retrieved;
        }
    }
}

/// Upgrade `Retrieved` demands to `Supported` when a verified gate
/// claim lexically covers the facet; then emit `Gap` rows for the
/// honest residue. `abstained` turns additionally gap the Query facet
/// itself (retrieved-but-unsupported = the claim, not the topic, is
/// uncovered). `probe` supplies the TopicUncovered/ClaimUncovered
/// verdict for Absent facets; `None` (probe off / not run) defaults
/// Absent facets to `ClaimUncovered` — the less dramatic claim.
pub(crate) fn finish_demands(
    demands: &mut [Demand],
    gate_claims: Option<&[GateClaim]>,
    abstained: bool,
    probe: Option<GapCoverage>,
) -> Vec<Gap> {
    let supported_claims: Vec<String> = gate_claims
        .unwrap_or_default()
        .iter()
        .filter(|c| c.supported)
        .map(|c| c.text.to_lowercase())
        .collect();
    let any_supported = !supported_claims.is_empty();
    for d in demands.iter_mut() {
        if d.covered != CoverageLevel::Retrieved || abstained {
            continue;
        }
        let upgraded = match d.facet {
            DemandFacet::Query => any_supported,
            _ => {
                let needle = d.text.to_lowercase();
                supported_claims.iter().any(|c| c.contains(&needle))
            }
        };
        if upgraded {
            d.covered = CoverageLevel::Supported;
        }
    }
    let mut gaps = Vec::new();
    for (idx, d) in demands.iter().enumerate() {
        let coverage = match d.covered {
            CoverageLevel::Absent => probe.unwrap_or(GapCoverage::ClaimUncovered),
            // A retrieved-but-unsupported facet on an abstained turn.
            // `Retrieved` is WEAK evidence of topic coverage — top-k
            // retrieval returns something for any query, so an OOD
            // question over distractors still stamps Retrieved. When the
            // probe ran, its calibrated nearest-sim verdict (0.49 floor,
            // measured clean split 2026-07-19, re-measured 2026-09-09 —
            // see `coverage_near_sim`) outranks the affinity
            // stamp: an in-topic claim gap reads ~0.71 → ClaimUncovered,
            // an off-topic query reads 0.17-0.49 → TopicUncovered
            // (observed mis-route: ood-australia-capital gapped
            // ClaimUncovered over 10 distractor chunks, 2026-07-20).
            // No probe → the topic-in-sources default stands.
            CoverageLevel::Retrieved if abstained => probe.unwrap_or(GapCoverage::ClaimUncovered),
            _ => continue,
        };
        let statement = match d.facet {
            DemandFacet::Query => format!(
                "Your sources didn't settle this question: {}",
                d.text.chars().take(160).collect::<String>()
            ),
            DemandFacet::Entity => format!("No source material found on \"{}\"", d.text),
            DemandFacet::SubQuestion => {
                format!("The sub-question \"{}\" went unanswered", d.text)
            }
            DemandFacet::Stance => {
                format!("Your sources don't cover the \"{}\" position", d.text)
            }
            DemandFacet::Section => {
                format!("No \"{}\" section found in your sources", d.text)
            }
        };
        gaps.push(Gap {
            demand_idx: idx,
            statement,
            coverage,
            routes: Vec::new(),
        });
    }
    gaps
}

/// Result of the cross-corpus coverage probe.
#[derive(Debug, Clone)]
pub struct CoverageProbeResult {
    /// Best (highest) nearest-chunk cosine similarity across corpora.
    pub best_similarity: f32,
    /// Corpus that produced it.
    pub best_corpus: Option<String>,
    /// The classification the similarity implies.
    pub verdict: GapCoverage,
}

/// Whether a corpus is in the coverage probe's scope for this turn.
/// `enabled_corpora = Some(non-empty)` (a sealed/notebook turn) scopes the
/// probe to exactly those corpora — D4's "your corpus" is the ENABLED
/// corpus, not every corpus installed on the box. `None`/empty (an
/// all-corpora turn) admits every installed corpus.
fn corpus_in_probe_scope(corpus_id: &str, enabled_corpora: Option<&[String]>) -> bool {
    match enabled_corpora {
        Some(ids) if !ids.is_empty() => ids.iter().any(|e| e == corpus_id),
        _ => true,
    }
}

/// `SOVEREIGN_COVERAGE_PROBE=0|false|off|no` disables the probe.
pub(crate) fn coverage_probe_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_COVERAGE_PROBE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Similarity floor separating "an installed corpus is near this
/// topic" (ClaimUncovered) from "no corpus touches it"
/// (TopicUncovered). Tunable via `SOVEREIGN_COVERAGE_NEAR_SIM`.
///
/// **0.49 is the calibration this line asked for and had not had.** It read
/// `0.55, to be calibrated against the chaos absent banks` from the day it was
/// written. Run 2026-09-09 over the whole `secret_agent` bank — 43 questions,
/// both classes, `qwen-embedding-0.6b`, nearest-chunk cosine, the same signal
/// the probe reads:
///
/// ```text
///   in-topic   (38 q)   0.5089 .. 0.7982
///   off-topic  ( 5 q)   0.2015 .. 0.3323
/// ```
///
/// **0.55 sat inside the in-topic band** — six in-topic questions scored under
/// it, and one of them cost a wrong answer. Measured on the smoke subset the
/// same day: `distract-bomb-maker` ("which member of the anarchist circle is
/// the bomb-maker?", squarely in-corpus) probed 0.5475, was called
/// `TopicUncovered`, and `gk_rescue` replaced a correct abstention with a
/// parametric answer the judge scored wrong. A wrong answer reached the reader
/// on a question the corpus can answer.
///
/// **The boundary rule, fixed before the effect was read: the top of the
/// observed OFF-TOPIC band, not the midpoint of the gap.** The midpoint of the
/// 2026-09-09 gap is 0.42, and 0.42 would have been wrong — the earlier
/// calibration recorded in this module (2026-07-19, cited at the
/// `Retrieved`-affinity comment above) observed off-topic queries reading as
/// high as **0.49**, so a 0.42 floor would call a genuinely off-topic question
/// covered and suppress the rescue that is right for it. Taking the top of the
/// union of both observed off-topic ranges — 0.49 — is the only value
/// consistent with BOTH runs: no observed off-topic question is called
/// covered, and no observed in-topic question (min 0.5089) is called
/// uncovered. The two classes separate in (0.49, 0.5089) and nowhere wider.
///
/// The asymmetry that decides ties: licensing the rescue when the topic IS
/// covered puts a wrong answer in front of the reader (a red line); declining
/// it when the topic is genuinely uncovered costs a caveated general-knowledge
/// answer and keeps an honest abstention with acquisition routes (a TRACKED
/// metric). When in doubt the floor goes DOWN.
///
/// Limits, stated: two corpora, one embedding model, and the gap between the
/// classes is 0.019 wide. That is not a comfortable margin — the finding is
/// the BAND, and a third corpus that lands inside it means this scalar cannot
/// carry the decision alone and the rescue needs a second signal.
fn coverage_near_sim() -> f32 {
    std::env::var("SOVEREIGN_COVERAGE_NEAR_SIM")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.49)
}

/// Bound on corpora probed per turn (worst-case latency guard).
const COVERAGE_PROBE_MAX_CORPORA: usize = 12;

/// Cross-corpus coverage probe — the `nearest_vector_distance` signal
/// (validated 2026-07-13 for the retrieval prefilter) lifted into a
/// user-meaning verdict: does ANY installed corpus have a region near
/// this query? Runs ONLY on gap/abstain turns (the caller gates),
/// reuses the pipeline's query embedding — zero extra embeds, one
/// bounded ANN probe per corpus. Free function so streaming spawns
/// (which hold an engine clone, not the Runtime) can call it.
pub async fn coverage_probe(
    engine: Option<&std::sync::Arc<corpus_engine::CorpusEngine>>,
    embedding: &[f32],
    enabled_corpora: Option<&[String]>,
) -> Option<CoverageProbeResult> {
    {
        if !coverage_probe_enabled() || embedding.is_empty() {
            return None;
        }
        let engine = engine?;
        let started = std::time::Instant::now();
        let infos = match engine.usable_indexes().await {
            Ok(i) => i,
            Err(e) => {
                tracing::debug!(target: "epistemic.ledger", error = %e, "coverage probe: installed_indexes failed");
                return None;
            }
        };
        // Scope to the turn's enabled corpora (D4: "your corpus" is the
        // ENABLED/sealed corpus, not every corpus installed on the box). On a
        // sealed notebook turn this stops the probe finding "Australia" in an
        // unrelated installed wikipedia and calling a genuine knowledge gap
        // `ClaimUncovered`. It also makes the verdict DETERMINISTIC: the prior
        // `take(12)` over `installed_indexes()` probed an arbitrary first-12
        // subset (order-dependent), so the topic/claim verdict depended on
        // which corpora happened to sort first. `None` (no scope) keeps the
        // all-installed behavior for un-scoped turns.
        let scoped: Vec<&corpus_index::types::IndexInfo> = infos
            .iter()
            .filter(|i| corpus_in_probe_scope(&i.corpus_id, enabled_corpora))
            .collect();
        let mut best: Option<(f32, String)> = None;
        for info in scoped.iter().take(COVERAGE_PROBE_MAX_CORPORA) {
            let idx = match engine.open_index(&info.path).await {
                Ok(i) => i,
                Err(_) => continue,
            };
            if let Ok(Some(d)) = idx.nearest_vector_distance(embedding, 8).await {
                let sim = 1.0 - d;
                if best.as_ref().map(|(b, _)| sim > *b).unwrap_or(true) {
                    best = Some((sim, info.corpus_id.clone()));
                }
            }
        }
        let floor = coverage_near_sim();
        let result = match best {
            Some((sim, corpus)) => CoverageProbeResult {
                best_similarity: sim,
                best_corpus: Some(corpus),
                verdict: if sim >= floor {
                    GapCoverage::ClaimUncovered
                } else {
                    GapCoverage::TopicUncovered
                },
            },
            // No corpus produced a vector verdict at all — nothing
            // installed is anywhere near this topic.
            None => CoverageProbeResult {
                best_similarity: 0.0,
                best_corpus: None,
                verdict: GapCoverage::TopicUncovered,
            },
        };
        tracing::info!(
            target: "epistemic.ledger",
            best_similarity = result.best_similarity,
            best_corpus = ?result.best_corpus,
            verdict = ?result.verdict,
            floor,
            probe_ms = started.elapsed().as_millis() as u64,
            corpora = scoped.len().min(COVERAGE_PROBE_MAX_CORPORA),
            scoped = enabled_corpora.map(|e| e.len()).unwrap_or(0),
            "coverage probe"
        );
        Some(result)
    }
}

/// The pool a chunk slice is: its distinct corpus ids, order-preserving,
/// and the mesh member each chunk came from (`metadata["peer"]`, the one
/// writer being the retrieval pipeline's mesh merge), aligned with
/// `chunks`; `None` for a local chunk.
pub(crate) fn pool_context(chunks: &[corpus_index::types::ScoredChunk]) -> PoolContext {
    let mut seen = std::collections::HashSet::new();
    let mut corpora = Vec::new();
    for c in chunks {
        if seen.insert(c.corpus_id.clone()) {
            corpora.push(c.corpus_id.clone());
        }
    }
    PoolContext {
        corpora,
        members: chunks
            .iter()
            .map(|c| c.metadata.get("peer").cloned())
            .collect(),
    }
}

#[cfg(test)]
mod tests;
