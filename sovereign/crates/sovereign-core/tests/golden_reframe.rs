// SPDX-License-Identifier: AGPL-3.0-or-later
//! GAP-4 golden fixture: `run-reframe-1` — a deterministic gym-deck
//! drill (garbage judge + scripted draft + clean deck) with a staged
//! `reframe-input.json`, recorded 2026-08-14. The fixture pins the
//! re-frame shape: cross-artifact identity, the record's round/trigger,
//! the re-plan (plan-2.json), and the report's named substitution
//! (ARCH_PRINCIPLES §18.3 — the swap is never silent).

use sovereign_core::deep_research::audit::ClaimAudit;
use sovereign_core::deep_research::icd::{Artifact, GateAction, Verdict, WitnessRecord};
use sovereign_core::deep_research::render::{final_claims, render_report};

const GOLDEN: &str = "tests/golden/run-reframe-1";
const RUN_ID: &str = "run-reframe-1";
const HASH: &str = "fbde4b8a913686db";
const ORIGINAL: &str = "What did OpenAI and Anthropic do in March 2025?";
const REFRAMED: &str = "What did OpenAI and Anthropic do in March 2025, in one sentence?";

fn load(name: &str) -> String {
    std::fs::read_to_string(format!("{GOLDEN}/{name}"))
        .unwrap_or_else(|e| panic!("golden fixture {name}: {e}"))
}

fn parse(name: &str) -> Artifact {
    Artifact::parse(&load(name)).unwrap_or_else(|e| panic!("fixture {name} must parse: {e}"))
}

/// Cross-artifact identity: every artifact carries the run id and the
/// charter hash — the reframe record and the re-plan included.
fn assert_identity(a: &Artifact) {
    match a {
        Artifact::Charter(c) => assert_eq!(c.run_id, RUN_ID, "charter run_id"),
        Artifact::Plan(p) => {
            assert_eq!(p.run_id, RUN_ID);
            assert_eq!(p.charter_hash, HASH);
        }
        Artifact::Survey(s) => {
            assert_eq!(s.run_id, RUN_ID);
            assert_eq!(s.charter_hash, HASH);
        }
        Artifact::GapList(g) => {
            assert_eq!(g.run_id, RUN_ID);
            assert_eq!(g.charter_hash, HASH);
        }
        Artifact::FetchList(f) => {
            assert_eq!(f.run_id, RUN_ID);
            assert_eq!(f.charter_hash, HASH);
        }
        Artifact::SkipLedger(s) => {
            assert_eq!(s.run_id, RUN_ID);
            assert_eq!(s.charter_hash, HASH);
        }
        Artifact::BudgetLedger(b) => {
            assert_eq!(b.run_id, RUN_ID);
            assert_eq!(b.charter_hash, HASH);
        }
        Artifact::EvidenceWindow(w) => {
            assert_eq!(w.run_id, RUN_ID);
            assert_eq!(w.charter_hash, HASH);
        }
        Artifact::Draft(d) => {
            assert_eq!(d.run_id, RUN_ID);
            assert_eq!(d.charter_hash, HASH);
        }
        Artifact::VerdictSet(v) => {
            assert_eq!(v.run_id, RUN_ID);
            assert_eq!(v.charter_hash, HASH);
        }
        Artifact::Reframe(r) => {
            assert_eq!(r.run_id, RUN_ID);
            assert_eq!(r.charter_hash, HASH);
        }
        Artifact::Alignment(a) => {
            assert_eq!(a.run_id, RUN_ID);
            assert_eq!(a.charter_hash, HASH);
        }
        Artifact::Manifest(m) => {
            assert_eq!(m.run_id, RUN_ID);
            assert_eq!(m.charter_hash, HASH);
        }
    }
}

#[test]
fn every_reframed_boundary_parses_with_identity() {
    for name in [
        "charter.json",
        "plan.json",
        "plan-2.json",
        "reframe-1.json",
        "survey-1.json",
        "gap-list-1.json",
        "gap-list-2.json",
        "gap-list-3.json",
        "fetch-list-1.json",
        "fetch-list-3.json",
        "skip-ledger-1.json",
        "skip-ledger-3.json",
        "budget-ledger.json",
        "evidence-window-1.json",
        "evidence-window-3.json",
        "draft-1.json",
        "draft-2.json",
        "draft-3.json",
        "verdict-set.json",
        "manifest.json",
    ] {
        assert_identity(&parse(name));
    }
}

#[test]
fn the_reframe_fired_at_round_2_and_replanned() {
    let Artifact::Manifest(m) = parse("manifest.json") else {
        panic!()
    };
    let r = m
        .reframe
        .as_ref()
        .expect("the fixture is a reframed run — the manifest must carry the record");
    assert_eq!(r.round, 2, "the first possible trigger round");
    assert_eq!(r.original_question, ORIGINAL);
    assert_eq!(r.reframed_question, REFRAMED);
    assert!(
        r.trigger.contains("spinning"),
        "the trigger names the structural surprise"
    );
    assert_eq!(r.run_id, m.run_id);

    // The reframe round is a real round in the ledger: nothing searched,
    // nothing fetched.
    let row = m
        .rounds
        .iter()
        .find(|row| row.round == 2)
        .expect("the reframe round must appear in the ledger");
    assert_eq!(row.fetched, 0);
    assert_eq!(row.search_calls, 0);

    // The charter kept the ORIGINAL question — the swap is recorded in
    // the reframe record, never silent.
    let Artifact::Charter(c) = parse("charter.json") else {
        panic!()
    };
    assert_eq!(c.question, ORIGINAL);

    // The re-plan is a real plan artifact of the same run.
    let Artifact::Plan(p2) = parse("plan-2.json") else {
        panic!()
    };
    assert_eq!(p2.run_id, RUN_ID);
    assert_eq!(p2.charter_hash, HASH);
}

#[test]
fn the_report_names_the_substitution() {
    let report = load("report.md");
    assert!(
        report.starts_with(&format!("# {REFRAMED}")),
        "the report answers the reframed question"
    );
    assert!(report.contains("re-framed at round 2"));
    assert!(report.contains(&format!("`{ORIGINAL}`")));
}

#[test]
fn renderer_is_pinned_by_the_golden_reframe_report() {
    // Reconstruct the final audits from the verdict set + the merged
    // windows (rounds 1 and 3 — round 2 was the reframe round and
    // fetched nothing, so there is no window-2) and re-render WITH the
    // reframe record: the output must byte-match the golden report.
    let Artifact::VerdictSet(v) = parse("verdict-set.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w3) = parse("evidence-window-3.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w1) = parse("evidence-window-1.json") else {
        panic!()
    };
    let mut window = w1.clone();
    window.chunks.extend(w3.chunks.iter().cloned());
    let audits: Vec<ClaimAudit> = v
        .claims
        .iter()
        .map(|c| ClaimAudit {
            claim: c.text.clone(),
            verdict: c.verdict,
            action: if c.verdict == Verdict::Passed {
                GateAction::CitationGrounded
            } else {
                GateAction::AbstainedDecline
            },
            witness: WitnessRecord {
                ran: true,
                specifics: Vec::new(),
                all_absent: c
                    .flag
                    .as_deref()
                    .map(|f| f.contains("specifics absent"))
                    .unwrap_or(false),
                reason: None,
            },
            supporting_chunk_ids: c.evidence_ids.clone(),
            empty_evidence_window: false,
            reason: None,
        })
        .collect();
    let claims = final_claims(&audits, &window);
    assert_eq!(claims.len(), v.claims.len());
    let Artifact::Reframe(r) = parse("reframe-1.json") else {
        panic!()
    };
    let rendered = render_report(REFRAMED, &claims, RUN_ID, Some(&r), None, &[]);
    let golden = load("report.md");
    assert_eq!(
        rendered, golden,
        "renderer output must match the golden reframe report"
    );
}
