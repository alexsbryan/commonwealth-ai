// SPDX-License-Identifier: AGPL-3.0-or-later
//! The golden fixture qualification surface (icd-schemas.md §0):
//! one fixture per ICD boundary, all from ONE consistent synthetic run
//! ("run-meridian-1", the Meridian Bridge). Every fixture parses
//! through `Artifact::parse` (unknown icd / unsupported version
//! refuses), the charter validates, and the cross-artifact identity
//! (run_id + charter_hash) holds everywhere. The renderer is pinned
//! against the golden report text.

use sovereign_core::deep_research::audit::{split_claims, ClaimAudit};
use sovereign_core::deep_research::containment::strip_citation_spans;
use sovereign_core::deep_research::icd::{
    Artifact, Charter, Draft, EvidenceWindow, FetchList, GapList, GateAction, Manifest, SkipLedger,
    Survey, Verdict, VerdictSet,
};
use sovereign_core::deep_research::render::{final_claims, render_report};

const GOLDEN: &str = "tests/golden";
const RUN_ID: &str = "run-meridian-1";
const HASH: &str = "3ab42923e19a639d";

fn load(name: &str) -> String {
    std::fs::read_to_string(format!("{GOLDEN}/{name}"))
        .unwrap_or_else(|e| panic!("golden fixture {name}: {e}"))
}

fn parse(name: &str) -> Artifact {
    Artifact::parse(&load(name)).unwrap_or_else(|e| panic!("fixture {name} must parse: {e}"))
}

/// The cross-artifact identity: every artifact carries the run id and
/// the charter hash.
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
fn every_boundary_parses_with_identity() {
    let names = [
        "charter.json",
        "plan.json",
        "survey-1.json",
        "gap-list-1.json",
        "gap-list-2.json",
        "gap-list-3.json",
        "fetch-list-1.json",
        "fetch-list-2.json",
        "skip-ledger-1.json",
        "skip-ledger-2.json",
        "budget-ledger.json",
        "evidence-window-1.json",
        "evidence-window-2.json",
        "draft-1.json",
        "draft-2.json",
        "draft-3.json",
        "verdict-set.json",
        "manifest.json",
    ];
    for name in names {
        let artifact = parse(name);
        assert_identity(&artifact);
    }
    assert_eq!(parse("charter.json").icd_name(), "charter");
    assert_eq!(parse("manifest.json").icd_name(), "manifest");
}

#[test]
fn unknown_icd_and_unsupported_version_refuse() {
    let unknown = r#"{"icd": "telemetry_stream", "version": 1, "run_id": "x"}"#;
    let err = Artifact::parse(unknown).unwrap_err();
    assert!(err.contains("unknown icd boundary"), "got: {err}");
    let bad_version = r#"{"icd": "charter", "version": 99, "run_id": "x"}"#;
    let err = Artifact::parse(bad_version).unwrap_err();
    assert!(err.contains("unsupported icd version"), "got: {err}");
    let not_json = "not json";
    assert!(Artifact::parse(not_json).is_err());
}

#[test]
fn charter_validates_and_is_frozen() {
    let Artifact::Charter(charter) = parse("charter.json") else {
        panic!("charter fixture must parse as Charter");
    };
    charter.validate().expect("golden charter validates");
    // A tampered charter (unfrozen) refuses — FR-3.
    let mut tampered = charter.clone();
    tampered.frozen = false;
    assert!(tampered.validate().is_err());
    let mut tampered = charter.clone();
    tampered.charter.containment.trigger = "always".to_string();
    assert!(tampered.validate().is_err());
    // The golden charter hash matches the fixture identity.
    let json = serde_json::to_string(&charter).unwrap();
    let h = fnv1a_for_test(json.as_bytes());
    assert_eq!(
        format!("{h:016x}"),
        HASH,
        "charter hash must match the recorded identity"
    );
}

fn fnv1a_for_test(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[test]
fn estate_first_survey() {
    let Artifact::Survey(survey) = parse("survey-1.json") else {
        panic!()
    };
    assert!(survey.estate_precondition.asserted);
    assert!(survey.estate_precondition.estate_searchable);
    assert_eq!(survey.estate_corpora.len(), 1);
    assert_eq!(survey.estate_corpora[0].corpus_id, "meridian-docs");
    assert_eq!(survey.estate_corpora[0].custody, "personal");
    assert_eq!(survey.searched.len(), 1);
    assert_eq!(survey.searched[0].hits.len(), 2);
    // The estate answer is the round-1 draft text (estate alone).
    let Artifact::Draft(d1) = parse("draft-1.json") else {
        panic!()
    };
    assert_eq!(survey.estate_answer, d1.text);
    assert_eq!(d1.round, 1);
}

#[test]
fn strict_subset_arc_false_then_true() {
    let Artifact::GapList(g1) = parse("gap-list-1.json") else {
        panic!()
    };
    let Artifact::GapList(g2) = parse("gap-list-2.json") else {
        panic!()
    };
    let Artifact::GapList(g3) = parse("gap-list-3.json") else {
        panic!()
    };
    // Round 1: baseline (true), 2 gaps.
    assert!(g1.strict_subset_of_prior);
    assert_eq!(g1.gaps.len(), 2);
    // Round 2: the draft introduced a NEW claim (lattice trusses) whose
    // specifics could not be witnessed → the bar fires (false).
    assert!(!g2.strict_subset_of_prior);
    assert_eq!(g2.gaps.len(), 2);
    // Round 3: the persisted gap set shrank (larkhall resolved, only
    // lattice remains) → strict subset holds.
    assert!(g3.strict_subset_of_prior);
    assert_eq!(g3.gaps.len(), 1);
    assert!(g3.gaps[0].text.contains("lattice trusses"));
    // Gap → actionable query is the deterministic template.
    assert_eq!(g1.gaps[0].actionable_query, g1.gaps[0].text);
    assert_eq!(g1.gaps[0].from_claim_id.as_deref(), Some("c3"));
}

#[test]
fn triage_and_skip_ledger_agree() {
    let Artifact::FetchList(fl1) = parse("fetch-list-1.json") else {
        panic!()
    };
    let Artifact::SkipLedger(sl1) = parse("skip-ledger-1.json") else {
        panic!()
    };
    assert_eq!(fl1.triage.code_set_k, vec!["h1", "h2"]);
    assert_eq!(fl1.triage.eps_admits, vec!["h3"]);
    assert_eq!(fl1.triage.threshold, 0.72);
    assert_eq!(fl1.queries.len(), 2);
    // The skip ledger records exactly the hit the triage cut.
    assert_eq!(sl1.entries.len(), 1);
    assert_eq!(sl1.entries[0].url, "https://meridian-myths.org/legend");
    assert_eq!(sl1.entries[0].reason, "below-cut");
    assert_eq!(sl1.entries[0].rank, 4);
    let Artifact::SkipLedger(sl2) = parse("skip-ledger-2.json") else {
        panic!()
    };
    assert!(sl2.entries.is_empty());
}

#[test]
fn budget_ledger_sums_consistently() {
    let Artifact::BudgetLedger(b) = parse("budget-ledger.json") else {
        panic!()
    };
    assert_eq!(b.allowance["web-search:duckduckgo"], 4);
    assert_eq!(b.allowance["web-fetch:pages"], 4);
    // Spent = the sum of allowed entries per meter.
    let mut search = 0u32;
    let mut fetch = 0u32;
    for e in &b.entries {
        assert_eq!(e.decision, "allow");
        match e.family.as_str() {
            "web-search" => search += e.units,
            "web-fetch" => fetch += e.units,
            other => panic!("unknown family {other}"),
        }
    }
    assert_eq!(b.spent["web-search:duckduckgo"], search);
    assert_eq!(b.spent["web-fetch:pages"], fetch);
    assert_eq!(b.remaining["web-search:duckduckgo"], 0);
    assert_eq!(b.remaining["web-fetch:pages"], 0);
}

#[test]
fn evidence_is_custody_stamped() {
    let Artifact::EvidenceWindow(w1) = parse("evidence-window-1.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w2) = parse("evidence-window-2.json") else {
        panic!()
    };
    for w in [&w1, &w2] {
        for c in &w.chunks {
            assert_eq!(c.custody, "public-web", "fetched content is public-web");
            assert_eq!(
                c.provenance_class, "known",
                "no unknown provenance in this run"
            );
            assert!(!c.source_url.is_empty());
        }
        assert_eq!(w.derived_custody, "public-web");
    }
    assert_eq!(w1.chunks.len(), 3);
    assert_eq!(w2.chunks.len(), 1);
    assert!(w1.fetch_failures.is_empty());
    // Tags are the deterministic derive_tags output.
    assert_eq!(
        w1.chunks[0].tags,
        vec!["bridge", "construction", "funding", "meridian"]
    );
}

#[test]
fn the_loops_own_splitter_pins_the_draft() {
    let Artifact::Draft(d3) = parse("draft-3.json") else {
        panic!()
    };
    let claims = split_claims(&d3.text);
    assert_eq!(claims.len(), 5);
    // Spans attach to their sentence; the splitter strips nothing.
    for c in &claims {
        assert!(c.contains("[Source: "), "claim missing its span: {c}");
    }
    assert!(claims[0].contains("1873"));
    assert!(claims[4].contains("lattice trusses"));
    // strip_citation_spans matches the golden content (no spans in the
    // estate locators after stripping).
    let stripped = strip_citation_spans(&d3.text);
    assert!(!stripped.contains("[Source:"));
}

#[test]
fn verdict_set_verdicts_and_citations() {
    let Artifact::VerdictSet(v) = parse("verdict-set.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w1) = parse("evidence-window-1.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w2) = parse("evidence-window-2.json") else {
        panic!()
    };
    let urls: Vec<&str> = w1
        .chunks
        .iter()
        .chain(w2.chunks.iter())
        .map(|c| c.source_url.as_str())
        .collect();
    let passed = v
        .claims
        .iter()
        .filter(|c| c.verdict == Verdict::Passed)
        .count();
    let open = v
        .claims
        .iter()
        .filter(|c| c.verdict == Verdict::CouldNotJudge)
        .count();
    // GAP-2 regeneration: every single-origin support set capped — 4
    // passed claims (each citing one chunk from one source) downgraded
    // to could-not-judge with the floor's record; the 5th claim was
    // already open (all-absent witness).
    assert_eq!(passed, 0);
    assert_eq!(open, 5);
    // Every floor-capped claim carries the corroboration record: the
    // single origin named, the chunk count on the record (counted, never
    // the origin count), the floor constant, and the false verdict.
    let capped: Vec<_> = v
        .claims
        .iter()
        .filter(|c| c.corroboration.as_ref().is_some_and(|r| !r.passes_floor))
        .collect();
    assert_eq!(
        capped.len(),
        4,
        "the four former passes must be floor-capped"
    );
    for c in &capped {
        let rec = c.corroboration.as_ref().unwrap();
        assert_eq!(rec.floor, 2);
        assert_eq!(rec.support_chunks, 1, "claim {} cited one chunk", c.id);
        assert_eq!(rec.origins.len(), 1);
        assert!(
            urls.contains(&rec.origins[0].as_str()),
            "claim {} names an unresolvable origin {}",
            c.id,
            rec.origins[0]
        );
        assert_eq!(
            c.evidence_ids.len(),
            0,
            "a capped claim carries no citations"
        );
        assert!(c.flag.as_deref().unwrap().contains("single-origin support"));
    }
    // The all-absent open question keeps its own flag (always flag,
    // never remove) and NO corroboration record — the witness downgrade
    // fired before the floor.
    let open_claim = v.claims.iter().find(|c| c.corroboration.is_none()).unwrap();
    assert!(open_claim
        .flag
        .as_deref()
        .unwrap()
        .contains("extracted specifics absent"));
}

#[test]
fn manifest_closes_the_run() {
    let Artifact::Manifest(m) = parse("manifest.json") else {
        panic!()
    };
    assert_eq!(m.terminal_state, "done-partial");
    assert!(m.truncation_declared);
    assert_eq!(m.rounds.len(), 3);
    assert_eq!(m.rounds[0].gaps_after, 2);
    assert_eq!(m.rounds[0].search_calls, 2);
    assert_eq!(m.rounds[2].fetched, 6, "final row counts the merged window");
    assert_eq!(m.sources.fetched.len(), 4);
    assert!(m.sources.failed.is_empty());
    assert_eq!(m.budget.spent["web-search:duckduckgo"], 4);
    // GAP-2 regeneration: the four single-origin claims joined the open
    // set — the run-close record must not hide them.
    assert_eq!(m.not_covered.len(), 5);
    assert!(m.not_covered.iter().any(|t| t.contains("lattice trusses")));
    assert!(m
        .not_covered
        .iter()
        .any(|t| t.contains("completed in 1873")));
    assert!(m.lock.released_at_unix.is_some());
    assert!(m.lock.released_at_unix.unwrap() >= m.lock.acquired_at_unix);
}

#[test]
fn renderer_is_pinned_by_the_golden_report() {
    // Reconstruct the final audits from the verdict set and re-render —
    // the output must byte-match the golden report.
    let Artifact::VerdictSet(v) = parse("verdict-set.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w2) = parse("evidence-window-2.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w1) = parse("evidence-window-1.json") else {
        panic!()
    };
    let mut window = w1.clone();
    window.chunks.extend(w2.chunks.iter().cloned());
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
            witness: sovereign_core::deep_research::icd::WitnessRecord {
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
            corroboration: c.corroboration.clone(),
        })
        .collect();
    let claims = final_claims(&audits, &window);
    assert_eq!(claims.len(), v.claims.len());
    let rendered = render_report(
        "What is known about the Meridian Bridge across the Selune river?",
        &claims,
        RUN_ID,
        None,
        None,
        &[],
        &[],
    );
    let golden = load("report.md");
    assert_eq!(
        rendered, golden,
        "renderer output must match the golden report"
    );
}

#[test]
fn draft_citations_cover_the_windows() {
    let Artifact::Draft(d3) = parse("draft-3.json") else {
        panic!()
    };
    assert_eq!(
        d3.citations.len(),
        6,
        "the final draft cites the merged window"
    );
    assert!(d3.url_constraint.enabled);
    assert_eq!(
        d3.url_constraint.layer,
        "sovereign-inference:UrlAllowlistConstraint"
    );
    // Every cited evidence id resolves to a window chunk (the estate
    // ids resolve to the estate window's chunks, which carry estate:
    // locators — that is the estate's custody-shaped identity).
    let Artifact::EvidenceWindow(w1) = parse("evidence-window-1.json") else {
        panic!()
    };
    let Artifact::EvidenceWindow(w2) = parse("evidence-window-2.json") else {
        panic!()
    };
    let ids: Vec<&str> = w1
        .chunks
        .iter()
        .chain(w2.chunks.iter())
        .map(|c| c.id.as_str())
        .collect();
    for c in &d3.citations {
        if c.evidence_id.starts_with("estate-") {
            assert!(
                c.url.starts_with("estate:"),
                "estate citation must carry an estate: locator"
            );
        } else {
            assert!(
                ids.contains(&c.evidence_id.as_str()),
                "unresolved citation {}",
                c.evidence_id
            );
        }
    }
}
