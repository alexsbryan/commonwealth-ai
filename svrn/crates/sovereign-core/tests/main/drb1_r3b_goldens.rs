// SPDX-License-Identifier: AGPL-3.0-or-later
//! drb1-r3b goldens — flag-graded render (order drb1-r3b, RED-FIRST:
//! this file was written and watched FAILING before the grading landed
//! in render.rs). The fixtures build FinalClaim rows directly — the
//! recorded-verdict-set path render_report/render_race actually
//! consume when re-rendering a flight (the rescan harness feeds them
//! verdict-set.json rows), not the in-process audit shape.
//!
//! The flag strings below are PINS, not a second decider: if the
//! recorded-flag vocabulary changes, these tests fail and force a
//! conscious re-pin of the grading match (measured vocabulary,
//! 2026-08-21, all 168 verdict sets on disk: five distinct flags —
//! 1108 single-origin / 727 specifics-absent / 124 refuted /
//! 107 not-judgeable / 69 no-citation-handle — a closed set).

use sovereign_core::deep_research::audit::ClaimAudit;
use sovereign_core::deep_research::icd::{
    ClaimCitation, CorroborationRecord, EvidenceWindow, FinalClaim, GateAction, Verdict,
    WitnessRecord,
};
use sovereign_core::deep_research::render::{final_claims, render_race, render_report};
use sovereign_core::types::Custody;

const FLAG_SINGLE_ORIGIN: &str = "open question: single-origin support (corroboration floor)";
const FLAG_SPECIFICS_ABSENT: &str = "open question: extracted specifics absent from the evidence";
const FLAG_UNKNOWN: &str = "open question: invented flag (drb1-r3b golden)";

fn claim(
    id: &str,
    text: &str,
    verdict: Verdict,
    flag: Option<&str>,
    corroboration: Option<CorroborationRecord>,
    citations: Vec<ClaimCitation>,
) -> FinalClaim {
    FinalClaim {
        id: id.to_string(),
        text: text.to_string(),
        verdict,
        status: verdict.as_str().to_string(),
        evidence_ids: citations.iter().map(|c| c.chunk_id.clone()).collect(),
        citations,
        flag: flag.map(str::to_string),
        corroboration,
    }
}

fn citation(id: &str, url: &str) -> ClaimCitation {
    ClaimCitation {
        evidence_id: id.to_string(),
        url: url.to_string(),
        chunk_id: id.to_string(),
    }
}

fn floor_record(passes: bool, origins: &[&str]) -> CorroborationRecord {
    CorroborationRecord {
        origins: origins.iter().map(|s| s.to_string()).collect(),
        support_chunks: if passes { 2 } else { 1 },
        floor: 2,
        passes_floor: passes,
    }
}

/// The order's mixed fixture: corroborated, single-origin-capped
/// (could-not-judge at the floor — the graded tier), witness-abstain
/// (walled, honestly), refuted.
fn mixed_fixture() -> Vec<FinalClaim> {
    vec![
        claim(
            "c1",
            "The bridge was completed in 1873 by two independent records.",
            Verdict::Passed,
            None,
            Some(floor_record(
                true,
                &["https://a.example", "https://b.example"],
            )),
            vec![citation("ev-1", "https://a.example")],
        ),
        claim(
            "c2",
            "Its span is 240 meters, the longest in the county at completion.",
            Verdict::CouldNotJudge,
            Some(FLAG_SINGLE_ORIGIN),
            Some(floor_record(false, &["https://a.example"])),
            Vec::new(),
        ),
        claim(
            "c3",
            "The bridge's lattice trusses were the first of their kind.",
            Verdict::CouldNotJudge,
            Some(FLAG_SPECIFICS_ABSENT),
            None,
            Vec::new(),
        ),
        claim(
            "c4",
            "The engineer was Helena Voss.",
            Verdict::Failed,
            Some("refuted by the evidence"),
            None,
            Vec::new(),
        ),
    ]
}

fn section<'a>(page: &'a str, heading: &str) -> &'a str {
    for seg in page.split("\n## ") {
        if seg.starts_with(heading) {
            return seg;
        }
    }
    panic!("no section `{heading}` in page:\n{page}");
}

fn line_containing<'a>(page: &'a str, needle: &str) -> &'a str {
    page.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in page:\n{page}"))
}

/// The graded tier: a could-not-judge claim whose RECORDED flag names
/// single-origin support renders under Findings with the
/// `[single-origin]` label and NEVER a `[passed]` stamp (the verdict
/// stands could-not-judge — §18.3, never silently graded up). The
/// witness-abstain and refuted claims stay walled in their own
/// sections. Findings is non-empty.
#[test]
fn mixed_tiers_render_distinctly_findings_non_empty_abstains_walled() {
    let claims = mixed_fixture();
    let report = render_report(
        "Meridian Bridge history",
        &claims,
        "run-r3b",
        None,
        None,
        &[],
        &[],
    );

    let findings = section(&report, "Findings");
    assert!(
        findings.contains("[single-origin]"),
        "the graded claim must render under Findings: {findings}"
    );
    assert!(
        findings.contains("[passed]"),
        "the corroborated claim must render under Findings: {findings}"
    );
    assert!(
        findings.contains("240 meters"),
        "the graded claim's text must be present in Findings: {findings}"
    );
    // The graded row carries the tier label but NEVER the passed stamp.
    let graded_line = line_containing(&report, "240 meters");
    assert!(
        graded_line.contains("[single-origin]") && !graded_line.contains("[passed]"),
        "graded rows are stamped [single-origin], never [passed]: {graded_line}"
    );
    // The single origin is named when the floor record carries it.
    assert!(
        graded_line.contains("https://a.example"),
        "the graded row names its single origin: {graded_line}"
    );

    // The section names the substitution: floor-capped presentation,
    // verdict unchanged.
    assert!(
        findings.contains("could-not-judge at the corroboration floor"),
        "the Findings note must name the floor and the standing verdict: {findings}"
    );

    let open = section(&report, "Open questions");
    assert!(
        open.contains("lattice trusses"),
        "the witness-abstain claim stays walled: {open}"
    );
    assert!(
        !open.contains("240 meters"),
        "the graded claim must NOT stay in Open questions: {open}"
    );
    let refuted = section(&report, "Refuted claims");
    assert!(
        refuted.contains("Helena Voss"),
        "the refuted claim stays in its section: {refuted}"
    );
}

/// The same grading on the RACE page, plus the header counts: the
/// graded mass is counted separately from established findings — a
/// floor-capped row is not a passed finding and must not inflate the
/// established count.
#[test]
fn race_grades_single_origin_into_findings_and_walls_abstains() {
    let claims = mixed_fixture();
    let page = render_race("Meridian Bridge history", &claims, "run-r3b");

    let findings = section(&page, "Findings");
    let graded_line = line_containing(&page, "240 meters");
    assert!(
        findings.contains("240 meters"),
        "the graded claim renders under Findings: {findings}"
    );
    assert!(
        graded_line.contains("[single-origin]") && !graded_line.contains("[passed]"),
        "graded rows are never stamped [passed]: {graded_line}"
    );
    let open = section(&page, "Open questions");
    assert!(
        open.contains("lattice trusses") && !open.contains("240 meters"),
        "witness-abstain walled, graded claim not: {open}"
    );
    assert!(section(&page, "Refuted claims").contains("Helena Voss"));
    // Header: 1 established (the passed claim), 1 floor-capped, 2 open
    // (abstain + refuted).
    assert!(
        page.contains("1 finding established"),
        "established counts passed only: {page}"
    );
    assert!(
        page.contains("1 single-origin floor-capped"),
        "the graded mass is counted and named: {page}"
    );
    assert!(
        page.contains("2 claims open"),
        "open excludes the graded mass: {page}"
    );
}

/// Unknown flags default WALLED with a glassbox WARN — never silently
/// graded up (§18.3). The WARN names the claim and the unknown flag.
#[test]
fn unknown_flag_defaults_walled_with_glassbox_warn() {
    let claims = vec![
        claim(
            "c9",
            "A claim carrying a flag outside the recorded vocabulary.",
            Verdict::CouldNotJudge,
            Some(FLAG_UNKNOWN),
            None,
            Vec::new(),
        ),
        mixed_fixture()[0].clone(),
    ];
    let sink = WarnCapture::default();
    let handle = sink.clone();
    let (report, page) = tracing::subscriber::with_default(sink, || {
        (
            render_report("Q", &claims, "run-r3b", None, None, &[], &[]),
            render_race("Q", &claims, "run-r3b"),
        )
    });
    let events = handle.events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.contains("unknown flag defaults WALLED")),
        "the glassbox WARN must fire: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.contains(FLAG_UNKNOWN)),
        "the WARN names the unknown flag: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.contains("c9")),
        "the WARN names the claim: {events:?}"
    );
    for page in [&report, &page] {
        let findings = section(page, "Findings");
        assert!(
            !findings.contains("outside the recorded vocabulary"),
            "unknown flags never grade up: {findings}"
        );
        let open = section(page, "Open questions");
        assert!(
            open.contains("outside the recorded vocabulary"),
            "unknown flags stay walled: {open}"
        );
    }
}

/// Byte-stability of the untouched path: a verdict set with NO graded
/// claims renders no Findings note, no floor-capped header segment, no
/// bare-[single-origin] rows — the pages are byte-identical to the
/// pre-r3b render for such sets (keeps the reframe/align goldens and
/// every no-cap flight pinned without re-blessing).
#[test]
fn no_graded_claims_renders_no_note_and_no_bare_single_origin_rows() {
    let claims = vec![
        mixed_fixture()[0].clone(),
        claim(
            "c3",
            "The lattice trusses claim stays open.",
            Verdict::CouldNotJudge,
            Some(FLAG_SPECIFICS_ABSENT),
            None,
            Vec::new(),
        ),
    ];
    let report = render_report("Q", &claims, "run-r3b", None, None, &[], &[]);
    let page = render_race("Q", &claims, "run-r3b");
    for out in [&report, &page] {
        assert!(
            !out.contains("could-not-judge at the corroboration floor"),
            "no Findings note without graded rows: {out}"
        );
        assert!(
            !out.contains("single-origin floor-capped"),
            "no header segment without graded rows: {out}"
        );
        let findings = section(out, "Findings");
        assert!(
            !findings.contains("[single-origin]"),
            "no bare-[single-origin] rows without graded claims: {findings}"
        );
    }
}

/// The closed-set property end to end: the PRODUCER (final_claims)
/// emits exactly the flag string the grader grades on, so an
/// in-process flight and a re-rendered recorded verdict set classify
/// identically. A floor-capped could-not-judge audit lands in Findings;
/// a no-citation-handle could-not-judge stays walled even when its
/// floor record also fails (the recorded flag names the resolved
/// reason, and no-handle wins — the grader follows the RECORD).
#[test]
fn producer_flag_and_render_grade_agree() {
    let window = EvidenceWindow {
        icd: "evidence_window".to_string(),
        version: 1,
        run_id: "run-r3b".to_string(),
        charter_hash: "h".to_string(),
        round: 1,
        chunks: vec![sovereign_core::deep_research::icd::WindowChunk {
            id: "ev-1".to_string(),
            locator: "https://a.example".to_string(),
            source_url: "https://a.example".to_string(),
            custody: Custody::PublicWeb.as_str().to_string(),
            provenance_class: "known".to_string(),
            content: "The Meridian Bridge was completed in 1873.".to_string(),
            ingested_into: None,
            tags: Vec::new(),
        }],
        fetch_failures: Vec::new(),
        dedup_refused: Vec::new(),
        content_refused: Vec::new(),
        derived_custody: Custody::PublicWeb.as_str().to_string(),
    };
    let audits = vec![
        // Floor-capped: could-not-judge, one origin, floor unmet.
        ClaimAudit {
            claim: "The span is 240 meters.".to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::CitationGrounded,
            witness: WitnessRecord {
                ran: true,
                specifics: vec!["span".to_string()],
                all_absent: false,
                reason: None,
            },
            supporting_chunk_ids: vec!["ev-1".to_string()],
            empty_evidence_window: false,
            reason: None,
            corroboration: Some(floor_record(false, &["https://a.example"])),
        },
        // No citation handle AND a failing floor record: the recorded
        // flag is the no-handle one (the producer's arm order), and the
        // grader follows the recorded flag — walled.
        ClaimAudit {
            claim: "An uncited assertion with a failing floor record.".to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::RefusedNoCitationHandle,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: None,
            corroboration: Some(floor_record(false, &["https://a.example"])),
        },
    ];
    let claims = final_claims(&audits, &window);
    assert_eq!(
        claims[0].flag.as_deref(),
        Some(FLAG_SINGLE_ORIGIN),
        "the producer emits exactly the graded vocabulary's string"
    );
    assert!(
        !claims[1].flag.as_deref().unwrap().contains("single-origin"),
        "no-handle wins the producer's arm order: {:?}",
        claims[1].flag
    );
    let report = render_report("Q", &claims, "run-r3b", None, None, &[], &[]);
    let findings = section(&report, "Findings");
    assert!(
        findings.contains("240 meters"),
        "the floor-capped claim lands in Findings via its recorded flag: {findings}"
    );
    let open = section(&report, "Open questions");
    assert!(
        open.contains("uncited assertion"),
        "the no-handle claim stays walled by its recorded flag: {open}"
    );
}

/// Instrument integrity (seat finding 2026-08-21, follow-up commit):
/// bracket-stamps live on claim rows ONLY — never in presentation
/// prose. The campaign bar's regex counts raw bracket-stamps, so a
/// legend line spelling "[single-origin]" or "[passed]" adds
/// non-verdict markers to the denominator (measured on the graded
/// re-renders: 8 legends x 2 stamps = +16 pooled, regex read 55/153
/// against the true per-claim 55/137). Every line carrying a stamp
/// must be a claim row (`- **[...]** ...`); prose names tiers quoted
/// ('single-origin', 'passed'), not bracketed.
#[test]
fn prose_never_spells_a_bracket_stamp() {
    let claims = mixed_fixture();
    let report = render_report(
        "Meridian Bridge history",
        &claims,
        "run-r3b",
        None,
        None,
        &[],
        &[],
    );
    let page = render_race("Meridian Bridge history", &claims, "run-r3b");
    for out in [&report, &page] {
        for line in out.lines() {
            let carries_stamp = [
                "[passed]",
                "[single-origin]",
                "[could-not-judge]",
                "[open question]",
                "[refuted]",
                "[never-ran]",
                "[not evaluated]",
            ]
            .iter()
            .any(|s| line.contains(s));
            if carries_stamp {
                assert!(
                    line.trim_start().starts_with("- **["),
                    "bracket-stamps belong to claim rows only — this prose line spells one \
                     (the bar regex counts it): {line:?}"
                );
            }
        }
    }
    // The legend still explains the tier, in quoted form.
    assert!(
        report.contains("Rows stamped 'single-origin' without 'passed'"),
        "the legend must name the tiers without bracket stamps: {report}"
    );
    assert!(page.contains("Rows stamped 'single-origin' without 'passed'"));
}

/// A minimal tracing subscriber capturing WARN-or-worse events on the
/// `deep_research` target — enough to assert the glassbox WARN fired
/// (same shape as the rescan harness's sink).
#[derive(Default, Clone)]
struct WarnCapture {
    events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl tracing::Subscriber for WarnCapture {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        *meta.level() <= tracing::Level::WARN && meta.target() == "deep_research"
    }
    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct V(Vec<String>);
        impl tracing::field::Visit for V {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push(format!("{}={:?}", field.name(), value));
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.push(format!("{}={}", field.name(), value));
            }
        }
        let mut v = V(Vec::new());
        event.record(&mut v);
        self.events
            .lock()
            .unwrap()
            .push(format!("{} {}", event.metadata().level(), v.0.join(" ")));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

// ---------------------------------------------------------------------
// drb1-t5 (§18.3): the flag must not report an absence as a count.
// Measured 2026-08-22 over the logged t7a flight: 63 of the 72 claims
// stamped "single-origin support" carried an EMPTY corroboration.origins.
// ---------------------------------------------------------------------

const FLAG_NO_ORIGIN: &str = "open question: no supporting origin located (corroboration floor)";

/// An empty window — the render path needs one to resolve citations
/// against; these two goldens exercise the FLAG producer, not citation
/// resolution.
fn empty_window() -> EvidenceWindow {
    EvidenceWindow {
        icd: "evidence_window".to_string(),
        version: 1,
        run_id: "run-t5".to_string(),
        charter_hash: "h".to_string(),
        round: 1,
        chunks: Vec::new(),
        fetch_failures: Vec::new(),
        dedup_refused: Vec::new(),
        content_refused: Vec::new(),
        derived_custody: Custody::PublicWeb.as_str().to_string(),
    }
}

#[test]
fn zero_origin_claim_is_never_flagged_single_origin() {
    let audits = vec![ClaimAudit {
        claim: "Parkinson's disease causes tremor and rigidity.".to_string(),
        verdict: Verdict::CouldNotJudge,
        action: GateAction::CorroborationFloor,
        witness: WitnessRecord::default(),
        supporting_chunk_ids: Vec::new(),
        empty_evidence_window: false,
        reason: Some("corroboration floor: 0 supporting chunk(s) from 0 distinct origin(s)".into()),
        corroboration: Some(CorroborationRecord {
            origins: Vec::new(),
            support_chunks: 0,
            floor: 2,
            passes_floor: false,
        }),
    }];
    let window = empty_window();
    let claims = final_claims(&audits, &window);
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].flag.as_deref(),
        Some(FLAG_NO_ORIGIN),
        "a zero-origin floor cap must name the absence, not claim one origin"
    );
    assert_ne!(
        claims[0].flag.as_deref(),
        Some(FLAG_SINGLE_ORIGIN),
        "§18.3: absence is reported, never defaulted to a count"
    );
}

#[test]
fn one_origin_claim_still_flags_single_origin() {
    let audits = vec![ClaimAudit {
        claim: "A claim resting on exactly one located origin.".to_string(),
        verdict: Verdict::CouldNotJudge,
        action: GateAction::CorroborationFloor,
        witness: WitnessRecord::default(),
        supporting_chunk_ids: vec!["ev-2".to_string()],
        empty_evidence_window: false,
        reason: Some("corroboration floor: 1 supporting chunk(s) from 1 distinct origin(s)".into()),
        corroboration: Some(CorroborationRecord {
            origins: vec!["https://example.org/a".to_string()],
            support_chunks: 1,
            floor: 2,
            passes_floor: false,
        }),
    }];
    let claims = final_claims(&audits, &empty_window());
    assert_eq!(
        claims[0].flag.as_deref(),
        Some(FLAG_SINGLE_ORIGIN),
        "one located origin IS single-origin — the honest arm is unchanged"
    );
}
