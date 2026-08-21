// SPDX-License-Identifier: AGPL-3.0-or-later
//! STEER 2 (directive 3c5d8b53) golden fixtures: `run-align-proceed-1`
//! + `run-align-redirect-1` — deterministic gym-deck drills (garbage
//! judge + scripted draft + clean deck) recorded 2026-08-14. The pair
//! pins the pre-acquisition alignment gate BOTH ways:
//!
//! - proceed: no staged input — the gate proceeds, the run is
//!   byte-identical in shape to a run without the gate (no alignment
//!   artifact, no re-plan, report unchanged);
//! - redirect: a staged `alignment-input.json` redirects the question
//!   at round 0 (before any acquisition), the record lands in the
//!   manifest + alignment-1.json, the re-plan is plan-2.json, and the
//!   report names the substitution (ARCH_PRINCIPLES §18.3).

use sovereign_core::deep_research::audit::ClaimAudit;
use sovereign_core::deep_research::icd::{Artifact, GateAction, Verdict, WitnessRecord};
use sovereign_core::deep_research::render::{final_claims, render_report};

const PROCEED: &str = "tests/golden/run-align-proceed-1";
const PROCEED_RUN_ID: &str = "run-align-proceed-1";
const PROCEED_HASH: &str = "ad0f08121798e53f";
const REDIRECT: &str = "tests/golden/run-align-redirect-1";
const REDIRECT_RUN_ID: &str = "run-align-redirect-1";
const REDIRECT_HASH: &str = "1f435bc79768dbdb";
const QUESTION: &str = "What did OpenAI and Anthropic do in March 2025?";
const REDIRECTED: &str = "What did OpenAI and Anthropic do in March 2025, in one sentence?";

fn load(dir: &str, name: &str) -> String {
    std::fs::read_to_string(format!("{dir}/{name}"))
        .unwrap_or_else(|e| panic!("golden fixture {dir}/{name}: {e}"))
}

fn parse(dir: &str, name: &str) -> Artifact {
    Artifact::parse(&load(dir, name))
        .unwrap_or_else(|e| panic!("fixture {dir}/{name} must parse: {e}"))
}

/// Cross-artifact identity: every artifact carries the run id and the
/// charter hash — the alignment record and the re-plan included.
fn assert_identity(a: &Artifact, run_id: &str, hash: &str) {
    match a {
        Artifact::Charter(c) => assert_eq!(c.run_id, run_id, "charter run_id"),
        Artifact::Plan(p) => {
            assert_eq!(p.run_id, run_id);
            assert_eq!(p.charter_hash, hash);
        }
        Artifact::Survey(s) => {
            assert_eq!(s.run_id, run_id);
            assert_eq!(s.charter_hash, hash);
        }
        Artifact::GapList(g) => {
            assert_eq!(g.run_id, run_id);
            assert_eq!(g.charter_hash, hash);
        }
        Artifact::FetchList(f) => {
            assert_eq!(f.run_id, run_id);
            assert_eq!(f.charter_hash, hash);
        }
        Artifact::SkipLedger(s) => {
            assert_eq!(s.run_id, run_id);
            assert_eq!(s.charter_hash, hash);
        }
        Artifact::BudgetLedger(b) => {
            assert_eq!(b.run_id, run_id);
            assert_eq!(b.charter_hash, hash);
        }
        Artifact::EvidenceWindow(w) => {
            assert_eq!(w.run_id, run_id);
            assert_eq!(w.charter_hash, hash);
        }
        Artifact::Draft(d) => {
            assert_eq!(d.run_id, run_id);
            assert_eq!(d.charter_hash, hash);
        }
        Artifact::VerdictSet(v) => {
            assert_eq!(v.run_id, run_id);
            assert_eq!(v.charter_hash, hash);
        }
        Artifact::Reframe(r) => {
            assert_eq!(r.run_id, run_id);
            assert_eq!(r.charter_hash, hash);
        }
        Artifact::Alignment(a) => {
            assert_eq!(a.run_id, run_id);
            assert_eq!(a.charter_hash, hash);
        }
        Artifact::Manifest(m) => {
            assert_eq!(m.run_id, run_id);
            assert_eq!(m.charter_hash, hash);
        }
    }
}

#[test]
fn every_proceed_boundary_parses_with_identity() {
    for name in [
        "charter.json",
        "plan.json",
        "survey-1.json",
        "gap-list-1.json",
        "gap-list-2.json",
        "gap-list-3.json",
        "fetch-list-1.json",
        "fetch-list-2.json",
        "fetch-list-3.json",
        "skip-ledger-1.json",
        "skip-ledger-2.json",
        "skip-ledger-3.json",
        "budget-ledger.json",
        "evidence-window-1.json",
        "evidence-window-2.json",
        "evidence-window-3.json",
        "draft-1.json",
        "draft-2.json",
        "draft-3.json",
        "verdict-set.json",
        "manifest.json",
    ] {
        assert_identity(&parse(PROCEED, name), PROCEED_RUN_ID, PROCEED_HASH);
    }
}

#[test]
fn every_redirect_boundary_parses_with_identity() {
    for name in [
        "charter.json",
        "plan.json",
        "plan-2.json",
        "alignment-1.json",
        "survey-1.json",
        "gap-list-1.json",
        "gap-list-2.json",
        "gap-list-3.json",
        "fetch-list-1.json",
        "fetch-list-2.json",
        "fetch-list-3.json",
        "skip-ledger-1.json",
        "skip-ledger-2.json",
        "skip-ledger-3.json",
        "budget-ledger.json",
        "evidence-window-1.json",
        "evidence-window-2.json",
        "evidence-window-3.json",
        "draft-1.json",
        "draft-2.json",
        "draft-3.json",
        "verdict-set.json",
        "manifest.json",
    ] {
        assert_identity(&parse(REDIRECT, name), REDIRECT_RUN_ID, REDIRECT_HASH);
    }
}

#[test]
fn proceed_is_the_pre_gate_shape() {
    // No staged input → the gate proceeds: the manifest carries no
    // alignment record, and no alignment artifact or re-plan exists on
    // disk — the run is exactly the pre-gate shape.
    let Artifact::Manifest(m) = parse(PROCEED, "manifest.json") else {
        panic!()
    };
    assert!(
        m.alignment.is_none(),
        "a proceed run must not carry an alignment record"
    );
    assert!(
        !std::path::Path::new(PROCEED)
            .join("alignment-1.json")
            .exists(),
        "no alignment artifact on the proceed path"
    );
    assert!(
        !std::path::Path::new(PROCEED).join("plan-2.json").exists(),
        "no re-plan on the proceed path"
    );
    let report = load(PROCEED, "report.md");
    assert!(report.starts_with(&format!("# {QUESTION}")));
    assert!(
        !report.contains("redirected at alignment"),
        "a proceed report must not name a redirect that never happened"
    );
}

#[test]
fn redirect_fires_at_round_zero_with_the_record() {
    let Artifact::Manifest(m) = parse(REDIRECT, "manifest.json") else {
        panic!()
    };
    let a = m
        .alignment
        .as_ref()
        .expect("a staged input must redirect the launch plan");
    assert_eq!(a.round, 0, "the gate fires before any acquisition round");
    assert_eq!(a.original_question, QUESTION);
    assert_eq!(a.redirected_question, REDIRECTED);
    assert_eq!(
        a.reason,
        "the operator's holdout narrowed the acquisition target"
    );
    assert!(
        a.trigger.contains("alignment"),
        "the trigger names the gate: {}",
        a.trigger
    );
    assert_eq!(a.run_id, m.run_id);

    // The charter kept the ORIGINAL question — the swap is recorded in
    // the alignment record, never silent.
    let Artifact::Charter(c) = parse(REDIRECT, "charter.json") else {
        panic!()
    };
    assert_eq!(c.question, QUESTION);

    // alignment-1.json is the SAME record the manifest carries.
    let Artifact::Alignment(f) = parse(REDIRECT, "alignment-1.json") else {
        panic!()
    };
    assert_eq!(f.round, 0);
    assert_eq!(f.original_question, a.original_question);
    assert_eq!(f.redirected_question, a.redirected_question);
    assert_eq!(f.reason, a.reason);

    // The re-plan is a real plan artifact of the same run.
    let Artifact::Plan(p2) = parse(REDIRECT, "plan-2.json") else {
        panic!()
    };
    assert_eq!(p2.run_id, REDIRECT_RUN_ID);
    assert_eq!(p2.charter_hash, REDIRECT_HASH);
}

#[test]
fn the_redirect_report_names_the_substitution() {
    let report = load(REDIRECT, "report.md");
    assert!(
        report.starts_with(&format!("# {REDIRECTED}")),
        "the report answers the redirected question"
    );
    assert!(report.contains("redirected at alignment (round 0, pre-acquisition)"));
    assert!(report.contains(&format!("`{QUESTION}`")));
}

/// Reconstruct the final audits from the verdict set + the merged
/// windows and re-render: the output must byte-match the golden
/// reports — the proceed report with no record, the redirect report
/// with the alignment record.
fn render_golden(
    dir: &str,
    alignment: Option<&sovereign_core::deep_research::icd::AlignmentRecord>,
) -> String {
    let Artifact::VerdictSet(v) = parse(dir, "verdict-set.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w1) = parse(dir, "evidence-window-1.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w2) = parse(dir, "evidence-window-2.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w3) = parse(dir, "evidence-window-3.json") else {
        panic!()
    };
    let mut window = w1.clone();
    window.chunks.extend(w2.chunks.iter().cloned());
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
            corroboration: None,
        })
        .collect();
    let claims = final_claims(&audits, &window);
    assert_eq!(claims.len(), v.claims.len());
    let question = match alignment {
        Some(a) => &a.redirected_question,
        None => QUESTION,
    };
    render_report(question, &claims, v.run_id.as_str(), None, alignment, &[], &[])
}

#[test]
fn renderers_are_pinned_by_the_golden_reports() {
    // Proceed: re-render WITHOUT a record → the pre-gate report.
    let rendered = render_golden(PROCEED, None);
    let golden = load(PROCEED, "report.md");
    assert_eq!(
        rendered, golden,
        "the proceed report must match the golden — the gate changed nothing"
    );

    // Redirect: re-render WITH the alignment record → the redirect report.
    let Artifact::Alignment(a) = parse(REDIRECT, "alignment-1.json") else {
        panic!()
    };
    let rendered = render_golden(REDIRECT, Some(&a));
    let golden = load(REDIRECT, "report.md");
    assert_eq!(
        rendered, golden,
        "the redirect report must match the golden — the substitution is named"
    );
}
