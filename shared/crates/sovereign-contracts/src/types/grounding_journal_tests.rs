// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for `grounding_journal`, moved out under `#[path]` to keep the
//! module under its size band (ARCH §3.1).

use super::*;

fn decision(verdict: GateJudgeVerdict, audited: bool, ms: u64) -> GroundingLine {
    let mut d = GroundingDecisionLine::new("chat", 0.55, ms);
    d.verdict = verdict;
    d.claim_audited = audited;
    GroundingLine::Decision(d)
}

/// A correction is its own line kind, joined by `episode_id`: it
/// round-trips, and the counts keep it apart from decisions, so a
/// correction never moves a flag rate.
#[test]
fn a_correction_round_trips_and_is_counted_apart_from_decisions() {
    let c = GroundingLine::Correction(GroundingCorrectionLine::new(
        "f23b7122-16c2-4e94-aea1-961e1835d750",
        CorrectedVerdict::Contradicted,
        Some(WitnessRef {
            corpus: "c".into(),
            text_sha256: "ab".repeat(32),
            start: 3,
            end: 9,
        }),
        WitnessCheck::Verified,
    ));
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["kind"], "correction");
    assert_eq!(v["expected"], "contradicted");
    assert_eq!(v["witness_check"], "verified");
    let back: GroundingLine = serde_json::from_value(v).unwrap();
    assert_eq!(back, c);
    let s = stats(&[decision(GateJudgeVerdict::Unsupported, true, 5), c], 0);
    assert_eq!((s.decisions, s.corrections, s.unsupported), (1, 1, 1));
}

/// THE guard this module exists for: nothing content-bearing can
/// reach a line, because the record has nowhere to put it. Every
/// field a caller could conceivably route prose through is exercised
/// with a canary; if someone adds a free-form field, the canary
/// appears in the serialized line and this fails.
#[test]
fn no_content_bearing_field_can_reach_a_line() {
    const CANARY: &str = "THE_USERS_CLAIM_TEXT_MUST_NOT_APPEAR";
    let mut d = GroundingDecisionLine::new("chat", 0.55, 12);
    // The closed-set token fields are the only strings a caller
    // controls beyond ids; a hostile caller putting prose there is
    // the thing the reviewer of that call site must catch — but the
    // struct itself must offer no field DOCUMENTED for text.
    d.action = Some("released".into());
    d.evidence = vec![EvidenceRef {
        corpus: "corpus-a".into(),
        chunk: 41,
    }];
    // The per-call census is the newest place prose could have hidden:
    // it names a mechanism and counts characters, and carries neither
    // the prompt nor the reply.
    d.calls = vec![GateCallRow {
        mechanism: GateCallMechanism::SpecificsScan,
        ms: 17_800,
        prompt_chars: CANARY.len(),
        out_chars: 4,
        ok: true,
        stable_prefix_bytes: None,
        start_offset_ms: 12,
    }];
    let line = serde_json::to_string(&GroundingLine::Decision(d)).unwrap();
    assert!(!line.contains(CANARY));
    // The serialized field set is exactly the declared one — a new
    // field (where content could hide) must show up here and be
    // justified.
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "action",
            "calls",
            "chunks",
            "claim_audited",
            "entity_anchored",
            "episode_id",
            "evidence",
            "evidence_unresolved",
            "gate_ms",
            "kind",
            "retried",
            "schema",
            "surface",
            "tau",
            "top_similarity",
            "ts",
            "verdict",
            "violation_prob",
        ]
    );
}

#[test]
fn nothing_judged_is_none_not_zero_percent() {
    let s = stats(&[decision(GateJudgeVerdict::CouldNotJudge, false, 5)], 0);
    assert_eq!(s.flag_rate(), None, "0/0 must be could-not-judge, not 0%");
    assert_eq!(s.could_not_judge, 1);
}

#[test]
fn could_not_judge_is_never_in_the_flag_rate_denominator() {
    let lines = vec![
        decision(GateJudgeVerdict::Supported, true, 10),
        decision(GateJudgeVerdict::Unsupported, true, 20),
        decision(GateJudgeVerdict::CouldNotJudge, false, 30),
    ];
    let s = stats(&lines, 0);
    assert_eq!(
        s.flag_rate(),
        Some(0.5),
        "2 judged, 1 flagged — the third is not a verdict"
    );
}

#[test]
fn evidence_coverage_reports_the_mining_bound() {
    let mut d = GroundingDecisionLine::new("chat", 0.55, 10);
    d.chunks = 4;
    d.evidence = vec![
        EvidenceRef {
            corpus: "c".into(),
            chunk: 1,
        },
        EvidenceRef {
            corpus: "c".into(),
            chunk: 2,
        },
    ];
    d.evidence_unresolved = 2;
    let s = stats(&[GroundingLine::Decision(d)], 0);
    assert_eq!(s.evidence_coverage(), Some(0.5));
    let s0 = stats(&[], 0);
    assert_eq!(s0.evidence_coverage(), None);
}

#[test]
fn a_real_append_round_trips_through_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    assert!(append(dir.path(), &decision(GateJudgeVerdict::Supported, true, 10)).unwrap());
    let (lines, bad) = read_all(dir.path());
    assert_eq!(bad, 0);
    assert_eq!(lines.len(), 1);
    assert_eq!(stats(&lines, bad).supported, 1);
}

/// Every mechanism carries its own name. A duplicated label would make
/// two mechanisms indistinguishable in the census — which is precisely
/// the blindness this type was added to end (ARCH §10.6).
#[test]
fn every_gate_call_mechanism_has_a_distinct_label() {
    use std::collections::HashSet;
    let all = [
        GateCallMechanism::ClaimExtraction,
        GateCallMechanism::ValueExtraction,
        GateCallMechanism::ClaimList,
        GateCallMechanism::PerClaimJudge,
        GateCallMechanism::ChunkJudge,
        GateCallMechanism::SpecificsScan,
        GateCallMechanism::BatchedSupport,
        GateCallMechanism::LocatedSpanTriage,
        GateCallMechanism::Surgery,
        GateCallMechanism::Rewrite,
        GateCallMechanism::Retry,
        GateCallMechanism::ShortGuardRetry,
        GateCallMechanism::Citation,
        GateCallMechanism::SentenceSweep,
        GateCallMechanism::EvidenceSufficiency,
    ];
    let labels: HashSet<&str> = all.iter().map(|m| m.label()).collect();
    assert_eq!(labels.len(), all.len(), "two mechanisms share a name");
    // The wire token and the reader's word are the same string, so a
    // census joining on either cannot pick the wrong one.
    for m in all {
        let wire = serde_json::to_string(&m).unwrap();
        assert_eq!(wire, format!("\"{}\"", m.label()));
    }
}

/// A journal written before the census existed must still read: the
/// field defaults to empty rather than failing the whole line, because
/// a reader that drops old lines would silently shrink every historical
/// count computed over this stream.
#[test]
fn a_pre_census_line_still_parses_with_no_calls() {
    let mut d = GroundingDecisionLine::new("chat", 0.9, 37_200);
    d.calls = vec![GateCallRow {
        mechanism: GateCallMechanism::PerClaimJudge,
        ms: 1_500,
        prompt_chars: 31_600,
        out_chars: 30,
        ok: true,
        stable_prefix_bytes: Some(28_000),
        start_offset_ms: 4_100,
    }];
    let mut v: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&GroundingLine::Decision(d)).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("calls");
    let back: GroundingLine = serde_json::from_value(v).unwrap();
    let GroundingLine::Decision(back) = back else {
        panic!("a decision round-trips as a decision")
    };
    assert!(back.calls.is_empty(), "absent census reads as no rows");
}

/// This stream's own switch must gate THIS stream only.
#[test]
fn this_streams_own_marker_stops_the_writes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(GROUNDING_STREAM.marker_in(dir.path()), "").unwrap();
    assert!(!GROUNDING_STREAM.enabled(dir.path()));
    assert!(!append(dir.path(), &decision(GateJudgeVerdict::Supported, true, 1)).unwrap());
}
