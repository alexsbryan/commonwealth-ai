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
#[derive(Default)]
pub(crate) struct EpistemicInputs<'a> {
    /// The gate's `grounding_gate` meta blob (reads `action` only).
    pub gate_meta: Option<&'a serde_json::Value>,
    /// The gate's retained per-claim records.
    pub gate_claims: Option<&'a [GateClaim]>,
    /// Why the plan answered from general knowledge, when it did.
    pub general_knowledge: Option<GkReason>,
    /// Distinct corpus ids in the evidence pool the answer drew on
    /// (empty on parametric turns).
    pub pool_corpora: Vec<String>,
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

/// Actions whose release shipped WITHOUT a completed verification
/// (judge unavailable / verdict unparseable) — holdings under these
/// actions are `FailOpen`, per the gate's documented posture.
fn action_is_fail_open(action: &str) -> bool {
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
    let sole_corpus = match inputs.pool_corpora.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };

    let mut holdings: Vec<Holding> = Vec::new();
    // Gate-audited claims → corpus-provenance holdings. An abstained
    // turn asserts nothing: its failed claims stay in the gate meta
    // (glassbox), not in holdings.
    if !abstained {
        for c in inputs.gate_claims.unwrap_or_default() {
            let verification = if fail_open {
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
        !inputs.pool_corpora.is_empty(),
        gate_action.is_empty(),
    );
    let state = EpistemicState {
        version: EPISTEMIC_STATE_VERSION,
        demands: inputs.demands,
        holdings,
        gaps: inputs.gaps,
        verdict,
    };
    let (n_corpus, n_memory) = state.holdings.iter().fold((0usize, 0usize), |acc, h| {
        match h.provenance {
            Provenance::Corpus { .. } => (acc.0 + 1, acc.1),
            Provenance::Memory { .. } => (acc.0, acc.1 + 1),
            _ => acc,
        }
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
pub(crate) fn build_demands(
    message: &str,
    intent: &Intent,
    entities: &[String],
    plan: Option<&crate::runtime::retrieval_pipeline::DemandPlan>,
) -> Vec<Demand> {
    let mut demands = vec![Demand {
        facet: DemandFacet::Query,
        text: message.to_string(),
        covered: CoverageLevel::Absent,
    }];
    let mut push_unique = |demands: &mut Vec<Demand>, facet: DemandFacet, text: &str| {
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
    // I4: fold in the LLM demand plan's facets when present — one demand
    // model, two producers. Sub-queries become SubQuestion demands; stance
    // poles (both sides of a contested axis) become Stance demands; section
    // terms become Section demands.
    if let Some(plan) = plan {
        for s in &plan.sub_queries {
            push_unique(&mut demands, DemandFacet::SubQuestion, s);
        }
        if let Some(stance) = &plan.stance_contrast {
            for pole in &stance.poles {
                push_unique(&mut demands, DemandFacet::Stance, pole);
            }
        }
        for term in &plan.section_terms {
            push_unique(&mut demands, DemandFacet::Section, term);
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
pub(crate) fn stamp_coverage(demands: &mut [Demand], chunks: &[corpus_engine::ScoredChunk]) {
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
            // A retrieved-but-unsupported facet on an abstained turn:
            // the topic is in the sources, the claim is not.
            CoverageLevel::Retrieved if abstained => GapCoverage::ClaimUncovered,
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
/// (TopicUncovered). Tunable via `SOVEREIGN_COVERAGE_NEAR_SIM`;
/// default 0.55, to be calibrated against the chaos absent banks.
fn coverage_near_sim() -> f32 {
    std::env::var("SOVEREIGN_COVERAGE_NEAR_SIM")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.55)
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
        let infos = match engine.installed_indexes().await {
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
        let scoped: Vec<&corpus_engine::IndexInfo> = infos
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

/// Distinct corpus ids in a chunk pool, order-preserving.
pub(crate) fn pool_corpora(chunks: &[corpus_engine::ScoredChunk]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for c in chunks {
        if seen.insert(c.corpus_id.clone()) {
            out.push(c.corpus_id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_holding(verification: Verification) -> Holding {
        Holding {
            claim: "c".into(),
            provenance: Provenance::Corpus {
                corpus_id: Some("wiki".into()),
                chunk_id: None,
            },
            verification,
        }
    }

    fn memory_holding(verification: Verification) -> Holding {
        Holding {
            claim: "m".into(),
            provenance: Provenance::Memory {
                band: MemoryBand::ToldDirectly,
                entry_id: "id".into(),
            },
            verification,
        }
    }

    #[test]
    fn verdict_truth_table() {
        // Abstention dominates everything.
        assert_eq!(
            derive_verdict(&[corpus_holding(Verification::Verified)], true, false, true, false),
            TurnVerdict::CannotKnowFromHere
        );
        // All corpus-verified → Grounded.
        assert_eq!(
            derive_verdict(
                &[
                    corpus_holding(Verification::Verified),
                    corpus_holding(Verification::Verified)
                ],
                false,
                false,
                true,
                false
            ),
            TurnVerdict::Grounded
        );
        // A fail-open corpus holding degrades to Mixed, never Grounded.
        assert_eq!(
            derive_verdict(
                &[
                    corpus_holding(Verification::Verified),
                    corpus_holding(Verification::FailOpen)
                ],
                false,
                false,
                true,
                false
            ),
            TurnVerdict::Mixed
        );
        // Memory-only → MemoryRecall regardless of verification.
        assert_eq!(
            derive_verdict(&[memory_holding(Verification::FailOpen)], false, false, false, false),
            TurnVerdict::MemoryRecall
        );
        // Corpus + memory → Mixed.
        assert_eq!(
            derive_verdict(
                &[
                    corpus_holding(Verification::Verified),
                    memory_holding(Verification::Verified)
                ],
                false,
                false,
                true,
                false
            ),
            TurnVerdict::Mixed
        );
        // GK with no corpus holdings → GeneralKnowledge.
        assert_eq!(
            derive_verdict(&[], false, true, false, false),
            TurnVerdict::GeneralKnowledge
        );
        // Evidence used, nothing audited → Unverified (honesty about
        // the absent check, not a judgment).
        assert_eq!(
            derive_verdict(&[], false, false, true, true),
            TurnVerdict::Unverified
        );
    }

    fn tool_holding() -> Holding {
        Holding {
            claim: "Total assessed value = $1.2B".into(),
            provenance: Provenance::ToolDerived {
                tool: "parcel_analytics".into(),
            },
            verification: Verification::Verified,
        }
    }

    #[test]
    fn coverage_probe_scope_respects_enabled_corpora() {
        let enabled = vec!["chaos-secret-agent".to_string()];
        // Sealed turn: only the enabled corpus is in scope; an unrelated
        // installed corpus (wikipedia) is excluded — so a sealed-novel query
        // for "Australia" can't be called ClaimUncovered off a wikipedia hit.
        assert!(corpus_in_probe_scope("chaos-secret-agent", Some(&enabled)));
        assert!(!corpus_in_probe_scope("wikipedia", Some(&enabled)));
        // No scope (None) or empty → every installed corpus is admitted.
        assert!(corpus_in_probe_scope("wikipedia", None));
        assert!(corpus_in_probe_scope("wikipedia", Some(&[])));
    }

    #[test]
    fn tool_derived_verdicts() {
        // Tool-only → Mixed (never overclaims Grounded; the figures are
        // system-originated, not corpus-backed).
        assert_eq!(
            derive_verdict(&[tool_holding()], false, false, false, false),
            TurnVerdict::Mixed
        );
        // Corpus + tool → Mixed (bases mix).
        assert_eq!(
            derive_verdict(
                &[corpus_holding(Verification::Verified), tool_holding()],
                false,
                false,
                true,
                false
            ),
            TurnVerdict::Mixed
        );
        // GK signal but tool holdings present → NOT GeneralKnowledge.
        assert_eq!(
            derive_verdict(&[tool_holding()], false, true, false, false),
            TurnVerdict::Mixed
        );
    }

    #[test]
    fn tool_holdings_flow_through_assembler() {
        let meta = serde_json::json!({"action": "released"});
        let state = assemble_epistemic_state(EpistemicInputs {
            gate_meta: Some(&meta),
            tool_holdings: vec![tool_holding()],
            ..Default::default()
        });
        assert_eq!(state.holdings.len(), 1);
        assert!(matches!(
            &state.holdings[0].provenance,
            Provenance::ToolDerived { tool } if tool == "parcel_analytics"
        ));
        assert_eq!(state.verdict, TurnVerdict::Mixed);
    }

    #[test]
    fn abstained_turn_drops_tool_holdings() {
        let meta = serde_json::json!({"action": "abstained"});
        let state = assemble_epistemic_state(EpistemicInputs {
            gate_meta: Some(&meta),
            tool_holdings: vec![tool_holding()],
            ..Default::default()
        });
        assert!(state.holdings.is_empty());
        assert_eq!(state.verdict, TurnVerdict::CannotKnowFromHere);
    }

    #[test]
    fn abstained_turn_asserts_nothing() {
        let meta = serde_json::json!({"action": "abstained", "retried": true});
        let claims = vec![GateClaim {
            text: "Heat's first name is Vernon".into(),
            supported: false,
            failed_once: true,
            violation_prob: Some(0.97),
        }];
        let state = assemble_epistemic_state(EpistemicInputs {
            gate_meta: Some(&meta),
            gate_claims: Some(&claims),
            pool_corpora: vec!["secret-agent".into()],
            ..Default::default()
        });
        assert!(state.holdings.is_empty());
        assert_eq!(state.verdict, TurnVerdict::CannotKnowFromHere);
    }

    #[test]
    fn single_corpus_pool_attributes_corpus_id() {
        let meta = serde_json::json!({"action": "released"});
        let claims = vec![GateClaim {
            text: "The knife was a carving knife".into(),
            supported: true,
            failed_once: false,
            violation_prob: Some(0.02),
        }];
        let state = assemble_epistemic_state(EpistemicInputs {
            gate_meta: Some(&meta),
            gate_claims: Some(&claims),
            pool_corpora: vec!["secret-agent".into()],
            ..Default::default()
        });
        assert_eq!(state.holdings.len(), 1);
        assert!(matches!(
            &state.holdings[0].provenance,
            Provenance::Corpus { corpus_id: Some(id), .. } if id == "secret-agent"
        ));
        assert_eq!(state.holdings[0].verification, Verification::Verified);
        assert_eq!(state.verdict, TurnVerdict::Grounded);
    }

    #[test]
    fn multi_corpus_pool_leaves_attribution_open() {
        let meta = serde_json::json!({"action": "released"});
        let claims = vec![GateClaim {
            text: "x".into(),
            supported: true,
            failed_once: false,
            violation_prob: None,
        }];
        let state = assemble_epistemic_state(EpistemicInputs {
            gate_meta: Some(&meta),
            gate_claims: Some(&claims),
            pool_corpora: vec!["wikipedia".into(), "sep".into()],
            ..Default::default()
        });
        assert!(matches!(
            &state.holdings[0].provenance,
            Provenance::Corpus { corpus_id: None, .. }
        ));
    }

    #[test]
    fn referenced_memory_becomes_banded_holding() {
        let recalled = vec![RecalledMemoryProv {
            id: "mem-1".into(),
            content: "started a woodworking class in March".into(),
            created_at: 0,
            kind: Some("raw".into()),
            source_memory_ids: vec![],
            confidence: Some(0.9),
        }];
        let rv = RecallVerificationProv {
            grounded: true,
            fail_open: false,
            referenced: Some(1),
        };
        let state = assemble_epistemic_state(EpistemicInputs {
            recalled: &recalled,
            recall_verification: Some(&rv),
            ..Default::default()
        });
        assert_eq!(state.holdings.len(), 1);
        assert!(matches!(
            &state.holdings[0].provenance,
            Provenance::Memory { band: MemoryBand::ToldDirectly, entry_id } if entry_id == "mem-1"
        ));
        assert_eq!(state.holdings[0].verification, Verification::Verified);
        assert_eq!(state.verdict, TurnVerdict::MemoryRecall);
    }

    fn chunk(title: &str, content: &str, corpus: &str) -> corpus_engine::ScoredChunk {
        corpus_engine::ScoredChunk {
            content: content.to_string(),
            title: Some(title.to_string()),
            url: None,
            corpus_id: corpus.to_string(),
            score: 1.0,
            metadata: std::collections::HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn demands_build_and_stamp() {
        let entities = vec!["Isaac Newton".to_string(), "Einstein".to_string()];
        let mut demands = build_demands(
            "How did Newton and Einstein differ on gravity?",
            &Intent::KnowledgeQuery,
            &entities,
            None,
        );
        assert_eq!(demands[0].facet, DemandFacet::Query);
        assert!(demands
            .iter()
            .any(|d| d.facet == DemandFacet::Entity && d.text == "Isaac Newton"));
        let chunks = vec![chunk(
            "Isaac Newton",
            "newton's law of universal gravitation",
            "wikipedia",
        )];
        stamp_coverage(&mut demands, &chunks);
        assert_eq!(demands[0].covered, CoverageLevel::Retrieved); // pool non-empty
        let newton = demands
            .iter()
            .find(|d| d.text == "Isaac Newton")
            .expect("newton demand");
        assert_eq!(newton.covered, CoverageLevel::Retrieved);
        let einstein = demands
            .iter()
            .find(|d| d.text == "Einstein")
            .expect("einstein demand");
        assert_eq!(einstein.covered, CoverageLevel::Absent);
    }

    #[test]
    fn build_demands_folds_in_the_llm_plan() {
        use crate::runtime::retrieval_pipeline::{DemandPlan, StanceContrast};
        let plan = DemandPlan {
            sub_queries: vec!["general relativity gravity".into()],
            entities: vec![],
            stance_contrast: Some(StanceContrast {
                axis: "the nature of gravity".into(),
                poles: vec!["action at a distance".into(), "spacetime curvature".into()],
            }),
            section_terms: vec!["reception".into()],
        };
        let demands = build_demands(
            "How did Newton and Einstein differ on gravity?",
            &Intent::KnowledgeQuery,
            &[],
            Some(&plan),
        );
        // Stance poles → both sides demanded.
        assert!(demands
            .iter()
            .any(|d| d.facet == DemandFacet::Stance && d.text == "action at a distance"));
        assert!(demands
            .iter()
            .any(|d| d.facet == DemandFacet::Stance && d.text == "spacetime curvature"));
        // Section term.
        assert!(demands
            .iter()
            .any(|d| d.facet == DemandFacet::Section && d.text == "reception"));
        // Plan sub-query.
        assert!(demands
            .iter()
            .any(|d| d.facet == DemandFacet::SubQuestion && d.text == "general relativity gravity"));
    }

    #[test]
    fn stance_and_section_facets_stamp_and_gap() {
        let mut demands = vec![
            Demand {
                facet: DemandFacet::Stance,
                text: "spacetime curvature".into(),
                covered: CoverageLevel::Absent,
            },
            Demand {
                facet: DemandFacet::Section,
                text: "reception".into(),
                covered: CoverageLevel::Absent,
            },
        ];
        // A chunk covering the stance pole (surface-form containment).
        let chunks = vec![chunk(
            "General relativity",
            "gravity as spacetime curvature, per Einstein",
            "wikipedia",
        )];
        stamp_coverage(&mut demands, &chunks);
        assert_eq!(demands[0].covered, CoverageLevel::Retrieved); // stance pole present
        assert_eq!(demands[1].covered, CoverageLevel::Absent); // no "reception" text
        // The uncovered Section facet emits a gap with its own statement.
        let gaps = finish_demands(&mut demands, None, false, Some(GapCoverage::TopicUncovered));
        assert!(gaps
            .iter()
            .any(|g| g.statement.contains("reception") && g.statement.contains("section")));
    }

    #[test]
    fn finish_upgrades_supported_and_emits_gaps() {
        let mut demands = vec![
            Demand {
                facet: DemandFacet::Query,
                text: "q".into(),
                covered: CoverageLevel::Retrieved,
            },
            Demand {
                facet: DemandFacet::Entity,
                text: "Szilard".into(),
                covered: CoverageLevel::Absent,
            },
        ];
        let claims = vec![GateClaim {
            text: "supported claim".into(),
            supported: true,
            failed_once: false,
            violation_prob: None,
        }];
        let gaps = finish_demands(
            &mut demands,
            Some(&claims),
            false,
            Some(GapCoverage::TopicUncovered),
        );
        assert_eq!(demands[0].covered, CoverageLevel::Supported);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].demand_idx, 1);
        assert_eq!(gaps[0].coverage, GapCoverage::TopicUncovered);
    }

    #[test]
    fn abstained_turn_gaps_the_query_as_claim_uncovered() {
        let mut demands = vec![Demand {
            facet: DemandFacet::Query,
            text: "who is Heat".into(),
            covered: CoverageLevel::Retrieved,
        }];
        let gaps = finish_demands(&mut demands, None, true, None);
        assert_eq!(demands[0].covered, CoverageLevel::Retrieved); // never upgraded on abstain
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].coverage, GapCoverage::ClaimUncovered);
    }

    #[test]
    fn fail_open_recall_is_visible() {
        let recalled = vec![RecalledMemoryProv {
            id: "mem-2".into(),
            content: "mentioned a trip".into(),
            created_at: 0,
            kind: None,
            source_memory_ids: vec![],
            confidence: Some(0.4),
        }];
        let rv = RecallVerificationProv {
            grounded: true,
            fail_open: true,
            referenced: Some(1),
        };
        let state = assemble_epistemic_state(EpistemicInputs {
            recalled: &recalled,
            recall_verification: Some(&rv),
            ..Default::default()
        });
        assert_eq!(state.holdings[0].verification, Verification::FailOpen);
        assert!(matches!(
            &state.holdings[0].provenance,
            Provenance::Memory { band: MemoryBand::Tentative, .. }
        ));
    }
}
