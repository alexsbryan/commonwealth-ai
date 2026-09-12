// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading a deep-research RUN DIR — the poller, the shelf, the report
//! builder and the constitution check, split out of `research_http.rs`
//! when that file crossed the 1,200-line ceiling (ARCH §3.2).
//!
//! The split is the seam the routes already had. `research_http.rs`
//! answers HTTP: accept a run, cursor its frame log, refuse a second,
//! abort one. This file answers "what does the run dir say right now",
//! and it does so by deserialising the loop's OWN ICD artifacts
//! (`charter.json`, `budget-ledger.json`, `gap-list-<round>.json`,
//! `verdict-set.json`, `evidence-window-<round>.json`, `manifest.json`,
//! `report.md`) with `sovereign_core`'s types — so a schema drift
//! between the loop that WRITES a run dir and this viewer is a compile
//! error rather than an empty screen.
//!
//! All of it came down from the desktop whole on 2026-09-11
//! (`deep_research_commands/{live,report,runs}.rs`), which is why the
//! stage ladder and the report shape read the way a UI wants them.
//!
//! The run dir is the SINGLE state source. Nothing here caches: every
//! call re-reads, and the caller decides whether the snapshot CHANGED
//! before it appends a frame.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::research_http::is_live;
use sovereign_contracts::daemon_wire::{
    ResearchAlignment, ResearchBudget, ResearchCitation, ResearchClaim, ResearchConsent,
    ResearchConstitution, ResearchCorroboration, ResearchGap, ResearchReframe, ResearchReport,
    ResearchResidueRow, ResearchRoundRow, ResearchRunSummary,
};
use sovereign_core::deep_research::containment::missing_claim_figures;
use sovereign_core::deep_research::icd::{
    BudgetLedger, Charter, EvidenceWindow, GapList, Manifest, Verdict, VerdictSet,
};

/// Everything the live view shows, read from the run dir. `None` before
/// the charter exists (the loop writes it first).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DrLiveSnapshot {
    pub(crate) round: Option<u32>,
    pub(crate) max_rounds: Option<u32>,
    pub(crate) stage: String,
    pub(crate) gaps: Vec<ResearchGap>,
    pub(crate) budget: ResearchBudget,
    pub(crate) consent: Option<ResearchConsent>,
}

/// Re-reads the artifacts on every call (a handful of small JSON files);
/// the caller decides whether the snapshot CHANGED before appending.
pub(crate) struct RunDirPoller {
    run_dir: PathBuf,
}

impl RunDirPoller {
    pub(crate) fn new(run_dir: PathBuf) -> Self {
        Self { run_dir }
    }

    pub(crate) fn report_md(&self) -> Option<PathBuf> {
        let p = self.run_dir.join("report.md");
        p.is_file().then_some(p)
    }

    pub(crate) fn snapshot(&self) -> Option<DrLiveSnapshot> {
        let dir = &self.run_dir;
        // "No charter yet" is the ordinary pre-launch answer, so it is a
        // `None`. A charter that EXISTS and does not parse is a different
        // fact and must not arrive as the same silence (ARCH principle 6):
        // it means the loop that wrote this run dir and the ICD types
        // reading it have drifted, and the whole live view would just
        // never appear.
        let charter_raw = std::fs::read(dir.join("charter.json")).ok()?;
        let charter: Charter = match serde_json::from_slice(&charter_raw) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    run_dir = %dir.display(),
                    error = %e,
                    "research_http: charter.json is present but does not parse as the \
                     ICD Charter — no live view can be built for this run"
                );
                return None;
            }
        };

        // Round + stage, derived from which artifacts exist: the newest
        // gap-list-<round>.json names the current round; verdict-set.json
        // means the writing is being checked; report.md means done.
        let mut round: Option<u32> = None;
        let mut gaps: Vec<ResearchGap> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut lists: Vec<(u32, PathBuf)> = rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let rest = name.strip_prefix("gap-list-")?.strip_suffix(".json")?;
                    Some((rest.parse::<u32>().ok()?, e.path()))
                })
                .collect();
            lists.sort_by_key(|(r, _)| *r);
            if let Some((r, path)) = lists.last() {
                round = Some(*r);
                if let Ok(raw) = std::fs::read(path) {
                    if let Ok(list) = serde_json::from_slice::<GapList>(&raw) {
                        gaps = list
                            .gaps
                            .into_iter()
                            .map(|g| ResearchGap {
                                id: g.id,
                                text: g.text,
                            })
                            .collect();
                    }
                }
            }
        }
        let stage = (if self.report_md().is_some() {
            "done"
        } else if dir.join("verdict-set.json").is_file() {
            "checking"
        } else if round.is_some() {
            "rounding"
        } else {
            "planning"
        })
        .to_string();

        let mut budget = ResearchBudget::default();
        if let Ok(raw) = std::fs::read(dir.join("budget-ledger.json")) {
            if let Ok(ledger) = serde_json::from_slice::<BudgetLedger>(&raw) {
                budget = ResearchBudget {
                    spent: ledger.spent.into_iter().collect(),
                    remaining: ledger.remaining.into_iter().collect(),
                };
            }
        }

        let max_rounds = charter.charter.max_rounds;
        let consent = charter.charter.consent.map(|c| ResearchConsent {
            release_floor: c.release_floor.as_str().to_string(),
            granted_at_unix: c.granted_at_unix,
        });

        Some(DrLiveSnapshot {
            round,
            max_rounds: Some(max_rounds),
            stage,
            gaps,
            budget,
            consent,
        })
    }
}

/// The shelf: every `dr-*` dir under the base, newest first.
pub(crate) fn list_runs(base: &Path) -> Vec<ResearchRunSummary> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(base) {
        for e in rd.flatten() {
            let dir = e.path();
            let Some(run_id) = dir.file_name().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            if !dir.is_dir() || !run_id.starts_with("dr-") {
                continue;
            }
            let charter = std::fs::read(dir.join("charter.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Charter>(&raw).ok());
            let manifest = std::fs::read(dir.join("manifest.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok());
            let live = is_live(&run_id);
            out.push(ResearchRunSummary {
                run_id,
                question: charter.as_ref().map(|c| c.question.clone()),
                created_at_unix: charter.as_ref().map(|c| c.created_at_unix),
                terminal_state: manifest.as_ref().map(|m| m.terminal_state.clone()),
                live,
                rounds: manifest.as_ref().map(|m| m.rounds.len()).unwrap_or(0),
                report_present: dir.join("report.md").is_file(),
                consent: charter
                    .and_then(|c| c.charter.consent)
                    .map(|c| ResearchConsent {
                        release_floor: c.release_floor.as_str().to_string(),
                        granted_at_unix: c.granted_at_unix,
                    }),
            });
        }
    }
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    out
}

/// Assemble the report from a run dir's artifacts. `None` when there is
/// no `report.md` — the run did not reach a report.
pub(crate) fn build_report(run_dir: &Path) -> Option<ResearchReport> {
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
                .map(|c| ResearchClaim {
                    id: c.id.clone(),
                    text: c.text.clone(),
                    verdict: c.verdict.as_str().to_string(),
                    status: c.status.clone(),
                    citations: c
                        .citations
                        .iter()
                        .map(|ct| ResearchCitation {
                            evidence_id: ct.evidence_id.clone(),
                            url: ct.url.clone(),
                            chunk_id: ct.chunk_id.clone(),
                        })
                        .collect(),
                    corroboration: c.corroboration.as_ref().map(|cor| ResearchCorroboration {
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

    Some(ResearchReport {
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
                    .map(|r| ResearchResidueRow {
                        query: r.query.clone(),
                        round: r.round,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        reframe: manifest
            .as_ref()
            .and_then(|m| m.reframe.as_ref())
            .map(|r| ResearchReframe {
                round: r.round,
                original_question: r.original_question.clone(),
                reframed_question: r.reframed_question.clone(),
                reason: r.reason.clone(),
            }),
        alignment: manifest
            .as_ref()
            .and_then(|m| m.alignment.as_ref())
            .map(|a| ResearchAlignment {
                round: a.round,
                original_question: a.original_question.clone(),
                redirected_question: a.redirected_question.clone(),
                reason: a.reason.clone(),
            }),
        budget: manifest
            .as_ref()
            .map(|m| ResearchBudget {
                spent: m.budget.spent.clone().into_iter().collect(),
                remaining: m.budget.remaining.clone().into_iter().collect(),
            })
            .unwrap_or_default(),
        rounds: manifest
            .as_ref()
            .map(|m| {
                m.rounds
                    .iter()
                    .map(|r| ResearchRoundRow {
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
            .map(|c| ResearchConsent {
                release_floor: c.release_floor.as_str().to_string(),
                granted_at_unix: c.granted_at_unix,
            }),
        constitution,
    })
}

/// The (g) position property over the loop's own artifacts: every figure
/// token in a [passed] claim must appear in the claim's evidence chunks.
/// Uses the loop's own decider (`containment::missing_claim_figures`) —
/// one figure parser. Claims whose evidence ids resolve to no window
/// chunk are counted `unresolved` — reported, never defaulted.
fn constitution_check(run_dir: &Path, verdict_set: Option<&VerdictSet>) -> ResearchConstitution {
    let mut out = ResearchConstitution::default();
    let Some(vs) = verdict_set else {
        return out;
    };
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

// ─── Tests (down from the desktop's deep_research_commands/tests.rs) ──

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use sovereign_contracts::egress::ConsentGrant;
    use sovereign_contracts::types::Custody;
    use sovereign_core::deep_research::icd::{
        BudgetAllowance, CharterValues, ContainmentConfig, CorroborationRecord, CustodyPolicy,
        EmptyWindow, EvidenceWindow as Ew, FinalClaim, Gap, TriageConfig, UrlConstraintPolicy,
        WindowChunk,
    };

    fn write_json(dir: &Path, name: &str, value: &impl Serialize) {
        std::fs::write(dir.join(name), serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn charter_values(consent: Option<ConsentGrant>) -> CharterValues {
        CharterValues {
            max_rounds: 3,
            evidence_window_max_chunks: 20,
            containment: ContainmentConfig {
                trigger: "witness".to_string(),
                extraction_max_tokens: 256,
                specifics_max: 3,
            },
            triage: TriageConfig {
                code_set_k: 3,
                eps_quota: 0.1,
                content_coverage_floor:
                    sovereign_core::deep_research::acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR,
                prose_line_floor:
                    sovereign_core::deep_research::acquisition::DEFAULT_PROSE_LINE_FLOOR,
            },
            budget: BudgetAllowance {
                web_search_queries: 4,
                web_fetch_pages: 4,
            },
            custody: CustodyPolicy {
                stamp_required: true,
                unknown_refuses: true,
            },
            url_constraint: UrlConstraintPolicy {
                enabled: true,
                layer: "strict".to_string(),
            },
            consent,
        }
    }

    fn fixture_charter(dir: &Path, question: &str) {
        write_json(
            dir,
            "charter.json",
            &Charter {
                icd: "charter".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                question: question.to_string(),
                seed_id: None,
                created_at_unix: 100,
                charter: charter_values(Some(ConsentGrant {
                    run_id: "dr-100".to_string(),
                    granted_at_unix: 100,
                    release_floor: Custody::PublicWeb,
                })),
                frozen: true,
            },
        );
    }

    fn fixture_gap_list(dir: &Path, round: u32, gaps: Vec<Gap>) {
        write_json(
            dir,
            &format!("gap-list-{round}.json"),
            &GapList {
                icd: "gap-list".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                round,
                claims: Vec::new(),
                gaps,
                empty_evidence_windows: Vec::<EmptyWindow>::new(),
                strict_subset_of_prior: false,
            },
        );
    }

    fn fixture_budget(dir: &Path) {
        write_json(
            dir,
            "budget-ledger.json",
            &BudgetLedger {
                icd: "budget-ledger".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                allowance: HashMap::new(),
                entries: Vec::new(),
                spent: HashMap::from([("web".to_string(), 2)]),
                remaining: HashMap::from([("web".to_string(), 2)]),
                refused_urls: Vec::new(),
            },
        );
    }

    #[test]
    fn snapshot_reads_round_gaps_budget_and_consent() {
        let dir = tempfile::tempdir().unwrap();
        fixture_charter(dir.path(), "When did Apollo 11 land?");
        fixture_gap_list(
            dir.path(),
            1,
            vec![Gap {
                id: "g1".to_string(),
                text: "the landing date needs a second origin".to_string(),
                actionable_query: "Apollo 11 landing date".to_string(),
                from_claim_id: Some("c1".to_string()),
                corroboration: None,
            }],
        );
        fixture_budget(dir.path());

        let snap = RunDirPoller::new(dir.path().to_path_buf())
            .snapshot()
            .unwrap();
        assert_eq!(snap.round, Some(1));
        assert_eq!(snap.stage, "rounding");
        assert_eq!(snap.gaps.len(), 1);
        assert_eq!(snap.gaps[0].id, "g1");
        assert_eq!(snap.budget.spent.get("web"), Some(&2));
        assert_eq!(snap.budget.remaining.get("web"), Some(&2));
        let consent = snap.consent.unwrap();
        assert_eq!(consent.release_floor, "public-web");
        assert_eq!(consent.granted_at_unix, 100);
    }

    #[test]
    fn snapshot_is_none_before_the_charter_lands() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            RunDirPoller::new(dir.path().to_path_buf())
                .snapshot()
                .is_none(),
            "no charter — no run state to show"
        );
    }

    #[test]
    fn no_consent_means_default_deny_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        write_json(
            dir.path(),
            "charter.json",
            &Charter {
                icd: "charter".to_string(),
                version: 1,
                run_id: "dr-101".to_string(),
                question: "Q".to_string(),
                seed_id: None,
                created_at_unix: 101,
                charter: charter_values(None),
                frozen: true,
            },
        );
        let snap = RunDirPoller::new(dir.path().to_path_buf())
            .snapshot()
            .unwrap();
        assert!(snap.consent.is_none(), "default-deny must read as no grant");
    }

    #[test]
    fn stage_advances_with_the_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        fixture_charter(dir.path(), "Q");
        fixture_budget(dir.path());

        let poller = RunDirPoller::new(dir.path().to_path_buf());
        assert_eq!(poller.snapshot().unwrap().stage, "planning");

        fixture_gap_list(dir.path(), 1, Vec::new());
        assert_eq!(poller.snapshot().unwrap().stage, "rounding");

        write_json(
            dir.path(),
            "verdict-set.json",
            &VerdictSet {
                icd: "verdict-set".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                claims: Vec::new(),
                empty_rounds: Vec::new(),
            },
        );
        assert_eq!(poller.snapshot().unwrap().stage, "checking");

        std::fs::write(dir.path().join("report.md"), "# Report").unwrap();
        assert_eq!(poller.snapshot().unwrap().stage, "done");
        assert!(poller.report_md().is_some());
        // And the shelf reads the same dir as a report-bearing run.
        let shelf = list_runs(dir.path().parent().unwrap());
        // The tempdir's own name is not `dr-*`, so the shelf cannot see
        // it; the report builder can.
        assert!(shelf.iter().all(|r| r.run_id.starts_with("dr-")));
        let report = build_report(dir.path()).expect("report.md present");
        assert_eq!(report.report_md, "# Report");
        assert_eq!(report.question, "Q");
        assert_eq!(report.terminal_state, "interrupted", "no manifest yet");
    }

    fn window(dir: &Path, round: u32, chunks: Vec<WindowChunk>) {
        write_json(
            dir,
            &format!("evidence-window-{round}.json"),
            &Ew {
                icd: "evidence-window".to_string(),
                version: 1,
                run_id: "dr-100".to_string(),
                charter_hash: "h".to_string(),
                round,
                chunks,
                fetch_failures: Vec::new(),
                dedup_refused: Vec::new(),
                content_refused: Vec::new(),
                derived_custody: "personal".to_string(),
            },
        );
    }

    fn passed_claim_set() -> VerdictSet {
        VerdictSet {
            icd: "verdict-set".to_string(),
            version: 1,
            run_id: "dr-100".to_string(),
            charter_hash: "h".to_string(),
            claims: Vec::new(),
            empty_rounds: Vec::new(),
        }
    }

    fn chunk(content: &str) -> WindowChunk {
        WindowChunk {
            id: "c1".to_string(),
            locator: "estate:x:1".to_string(),
            source_url: "https://example.com/a".to_string(),
            custody: "personal".to_string(),
            provenance_class: "primary".to_string(),
            content: content.to_string(),
            ingested_into: None,
            tags: Vec::new(),
        }
    }

    fn passed(text: &str, evidence_ids: Vec<&str>) -> FinalClaim {
        FinalClaim {
            id: "c1".to_string(),
            text: text.to_string(),
            verdict: Verdict::Passed,
            status: "passed".to_string(),
            evidence_ids: evidence_ids.into_iter().map(String::from).collect(),
            citations: Vec::new(),
            flag: None,
            corroboration: Some(CorroborationRecord {
                origins: vec!["https://example.com/a".to_string()],
                support_chunks: 1,
                floor: 2,
                passes_floor: false,
            }),
        }
    }

    #[test]
    fn constitution_holds_when_every_passed_figure_is_traced() {
        let dir = tempfile::tempdir().unwrap();
        window(
            dir.path(),
            1,
            vec![chunk("Apollo 11 landed on July 20, 1969.")],
        );
        let mut vs = passed_claim_set();
        vs.claims
            .push(passed("Apollo 11 landed on July 20, 1969.", vec!["c1"]));
        let check = constitution_check(dir.path(), Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert!(check.violations.is_empty(), "{:?}", check.violations);
        assert_eq!(check.unresolved, 0);
    }

    #[test]
    fn constitution_names_an_untraced_figure_in_a_passed_claim() {
        let dir = tempfile::tempdir().unwrap();
        // The claim carries "2024" which the evidence never mentions.
        window(dir.path(), 1, vec![chunk("The bridge opened in 1930.")]);
        let mut vs = passed_claim_set();
        vs.claims.push(passed(
            "The bridge opened in 1930 and was restored in 2024.",
            vec!["c1"],
        ));
        let check = constitution_check(dir.path(), Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert_eq!(check.violations.len(), 1, "{:?}", check.violations);
        assert!(
            check.violations[0].contains("2024"),
            "{}",
            check.violations[0]
        );
        assert_eq!(check.unresolved, 0);
    }

    #[test]
    fn unresolved_evidence_is_reported_not_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        // No evidence windows at all — the claim's ids resolve nowhere.
        let mut vs = passed_claim_set();
        vs.claims.push(passed("Something passed.", vec!["missing"]));
        let check = constitution_check(dir.path(), Some(&vs));
        assert_eq!(check.passed_claims, 1);
        assert!(check.violations.is_empty());
        assert_eq!(check.unresolved, 1, "unresolvable evidence is counted");
    }
}
