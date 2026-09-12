// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checked report — rendered from the verb's own artifacts.
//!
//! Carved out of `deep_research_commands.rs` (ARCH §3.1).

use super::*;

/// The checked report + its verdict dimensions, rendered from the verb's
/// artifacts — never re-invented. `report_md` is the verb's own report.md;
/// the dimensions come from verdict-set.json (corroboration), manifest.json
/// (residue, reframe, alignment, not-covered), and the constitution check
/// over the evidence windows (the (g) position property).
#[derive(Debug, Serialize, Clone)]
pub struct DrReport {
    pub run_id: String,
    pub question: String,
    pub terminal_state: String,
    pub report_md: String,
    /// Verdict-set claims with their corroboration records (the gate's own
    /// accounting — origins, floor, pass).
    pub claims: Vec<DrFinalClaim>,
    /// Open questions (could-not-judge) from the manifest.
    pub not_covered: Vec<String>,
    /// The epistemic residue — every searched-but-absent query.
    pub residue: Vec<DrResidueRow>,
    pub reframe: Option<DrReframe>,
    pub alignment: Option<DrAlignment>,
    pub budget: DrBudget,
    pub rounds: Vec<DrRoundRow>,
    pub consent: Option<DrConsent>,
    /// The (g) constitution position: zero untraced figures in [passed].
    /// `violations` names each offending claim; `unresolved` counts claims
    /// whose evidence ids did not resolve to window chunks (reported, never
    /// defaulted).
    pub constitution: DrConstitution,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrFinalClaim {
    pub id: String,
    pub text: String,
    pub verdict: String,
    pub status: String,
    pub citations: Vec<DrCitation>,
    pub corroboration: Option<DrCorroboration>,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrCitation {
    pub evidence_id: String,
    pub url: String,
    pub chunk_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrCorroboration {
    pub origins: Vec<String>,
    pub support_chunks: usize,
    pub floor: usize,
    pub passes_floor: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrResidueRow {
    pub query: String,
    pub round: u32,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrReframe {
    pub round: u32,
    pub original_question: String,
    pub reframed_question: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrAlignment {
    pub round: u32,
    pub original_question: String,
    pub redirected_question: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DrRoundRow {
    pub round: u32,
    pub gaps_before: usize,
    pub gaps_after: usize,
    pub fetched: usize,
    pub search_calls: u32,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct DrConstitution {
    /// Number of [passed] claims checked.
    pub passed_claims: usize,
    /// Every untraced-figure violation: "claim c1 [passed] carries untraced
    /// figures: 2024". Empty = the position property holds.
    pub violations: Vec<String>,
    /// [passed] claims whose evidence ids resolved to no window chunk — the
    /// check could not run on them; reported, never defaulted.
    pub unresolved: usize,
}

/// Open the checked report for a completed run (the report view's data —
/// the verb's artifacts are the only source).
#[tauri::command]
pub async fn dr_open_report(run_id: String) -> Result<DrReport, String> {
    let dir = runs_base().join(&run_id);
    if !dir.is_dir() {
        return Err(format!("no run {run_id} under {}", runs_base().display()));
    }
    build_report(&dir)
        .ok_or_else(|| format!("run {run_id} has no report.md — it did not reach a report"))
}

/// Assemble the DrReport from a run dir's artifacts.
pub(super) fn build_report(run_dir: &Path) -> Option<DrReport> {
    let report_md = std::fs::read_to_string(run_dir.join("report.md")).ok()?;
    let charter = std::fs::read(run_dir.join("charter.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok());
    let manifest = std::fs::read(run_dir.join("manifest.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok());
    let verdict_set = std::fs::read(run_dir.join("verdict-set.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<VerdictSet>(&raw).ok());

    let claims = verdict_set
        .as_ref()
        .map(|v| {
            v.claims
                .iter()
                .map(|c| DrFinalClaim {
                    id: c.id.clone(),
                    text: c.text.clone(),
                    verdict: c.verdict.as_str().to_string(),
                    status: c.status.clone(),
                    citations: c
                        .citations
                        .iter()
                        .map(|ct| DrCitation {
                            evidence_id: ct.evidence_id.clone(),
                            url: ct.url.clone(),
                            chunk_id: ct.chunk_id.clone(),
                        })
                        .collect(),
                    corroboration: c.corroboration.as_ref().map(|cor| DrCorroboration {
                        origins: cor.origins.clone(),
                        support_chunks: cor.support_chunks,
                        floor: cor.floor,
                        passes_floor: cor.passes_floor,
                    }),
                })
                .collect()
        })
        .unwrap_or_default();

    let constitution = constitution_check(run_dir, verdict_set.as_ref());

    Some(DrReport {
        run_id: charter
            .as_ref()
            .map(|c| c.run_id.clone())
            .unwrap_or_else(|| {
                run_dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string()
            }),
        question: charter
            .as_ref()
            .map(|c| c.question.clone())
            .unwrap_or_default(),
        terminal_state: manifest
            .as_ref()
            .map(|m| m.terminal_state.clone())
            .unwrap_or_else(|| "interrupted".to_string()),
        report_md,
        claims,
        not_covered: manifest
            .as_ref()
            .map(|m| m.not_covered.clone())
            .unwrap_or_default(),
        residue: manifest
            .as_ref()
            .map(|m| {
                m.residue
                    .iter()
                    .map(|r| DrResidueRow {
                        query: r.query.clone(),
                        round: r.round,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        reframe: manifest
            .as_ref()
            .and_then(|m| m.reframe.as_ref())
            .map(|r| DrReframe {
                round: r.round,
                original_question: r.original_question.clone(),
                reframed_question: r.reframed_question.clone(),
                reason: r.reason.clone(),
            }),
        alignment: manifest
            .as_ref()
            .and_then(|m| m.alignment.as_ref())
            .map(|a| DrAlignment {
                round: a.round,
                original_question: a.original_question.clone(),
                redirected_question: a.redirected_question.clone(),
                reason: a.reason.clone(),
            }),
        budget: manifest
            .as_ref()
            .map(|m| DrBudget {
                spent: m.budget.spent.clone().into_iter().collect(),
                remaining: m.budget.remaining.clone().into_iter().collect(),
            })
            .unwrap_or_default(),
        rounds: manifest
            .as_ref()
            .map(|m| {
                m.rounds
                    .iter()
                    .map(|r| DrRoundRow {
                        round: r.round,
                        gaps_before: r.gaps_before,
                        gaps_after: r.gaps_after,
                        fetched: r.fetched,
                        search_calls: r.search_calls,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        consent: manifest
            .as_ref()
            .and_then(|m| m.consent.clone())
            .map(|c| DrConsent {
                release_floor: c.release_floor.as_str().to_string(),
                granted_at_unix: c.granted_at_unix,
            }),
        constitution,
    })
}

/// The (g) position property, over the verb's own artifacts: every figure
/// token in a [passed] claim must appear in the claim's evidence chunks.
/// Uses the loop's own decider (`containment::missing_claim_figures`) — one
/// figure parser, one implementation. Claims whose evidence ids resolve to
/// no window chunk are counted `unresolved` — reported, never defaulted.
pub(super) fn constitution_check(
    run_dir: &Path,
    verdict_set: Option<&VerdictSet>,
) -> DrConstitution {
    let mut out = DrConstitution::default();
    let Some(vs) = verdict_set else {
        // No verdict set — the run never reached the claim gate; the
        // position property is vacuous and the report view shows no claims.
        return out;
    };
    // All window chunks, keyed by id (a chunk id is unique per run — the
    // window's dedup convention).
    let mut chunks_by_id: HashMap<String, String> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(run_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("evidence-window-") || !name.ends_with(".json") {
                continue;
            }
            if let Ok(raw) = std::fs::read(e.path()) {
                if let Ok(window) = serde_json::from_slice::<EvidenceWindow>(&raw) {
                    for c in window.chunks {
                        chunks_by_id.entry(c.id.clone()).or_insert(c.content);
                    }
                }
            }
        }
    }
    for claim in &vs.claims {
        if claim.verdict != Verdict::Passed {
            continue;
        }
        out.passed_claims += 1;
        let evidence: Vec<String> = claim
            .evidence_ids
            .iter()
            .filter_map(|id| chunks_by_id.get(id).cloned())
            .collect();
        if evidence.is_empty() && !claim.evidence_ids.is_empty() {
            out.unresolved += 1;
            continue;
        }
        let untraced = missing_claim_figures(&claim.text, &evidence);
        if !untraced.is_empty() {
            out.violations.push(format!(
                "claim {} [passed] carries untraced figures: {}",
                claim.id,
                untraced.join(", ")
            ));
        }
    }
    out
}
