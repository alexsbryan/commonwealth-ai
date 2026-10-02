// SPDX-License-Identifier: AGPL-3.0-or-later
//! `governance_view`'s tests, split out under `#[path]` by fp-60 to keep the
//! module under its arch-gate ceiling (ARCH §3.1); the names are unchanged.

use super::*;
use crate::atoms::AtomsFile;

fn rule(n: usize, text: &str) -> RuleAtom {
    RuleAtom {
        id: AtomId::claim(n),
        text: text.into(),
        deontic: Some("forbids".into()),
        scope: Some(AtomId::entity(99)),
        citation: Some(ChunkRef::new(format!("chunk-{n}"), Some(text.into()))),
    }
}
fn tension(n: usize, a: usize, b: usize, why: &str, conf: f32) -> RuleTension {
    RuleTension {
        id: EdgeId::new(n),
        rule_a: AtomId::claim(a),
        rule_b: AtomId::claim(b),
        why: Some(why.into()),
        confidence: conf,
    }
}
fn op(kind: GovernanceOpKind, ts: i64, actor: &str) -> Op<GovernanceOpKind> {
    Op::new(kind, ts, actor)
}
fn assert_rule(n: usize, ts: i64) -> Op<GovernanceOpKind> {
    op(
        GovernanceOpKind::AssertRule {
            rule: AtomId::claim(n),
            source_doc: None,
        },
        ts,
        "ingest",
    )
}

#[test]
fn rules_join_status_and_content() {
    let ops = vec![
        assert_rule(1, 1000),
        op(
            GovernanceOpKind::Supersede {
                new_rule: AtomId::claim(2),
                old_rules: vec![AtomId::claim(1)],
                rationale: String::new(),
            },
            1001,
            "human:alex",
        ),
    ];
    let rules = vec![rule(1, "old rule"), rule(2, "new rule")];
    let view = build_view(&rules, &[], &ops);

    let active: Vec<_> = view.active_rules().collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, AtomId::claim(2));
    assert_eq!(active[0].text, "new rule");
    assert_eq!(active[0].deontic.as_deref(), Some("forbids"));

    let old = view
        .rules
        .iter()
        .find(|r| r.id == AtomId::claim(1))
        .unwrap();
    assert!(matches!(old.status, RuleStatus::Superseded { .. }));
    assert_eq!(old.text, "old rule");
    assert!(view.issues.is_empty());
}

/// Like `rule`, but with an explicit *section* id citation (an atom's
/// evidence `chunk_id` is a section id like `"sec_00001"`, not a chunk
/// row id — see [`chunk_to_section_map`] for the bridge to row ids).
fn rule_at(n: usize, section: &str, text: &str) -> RuleAtom {
    RuleAtom {
        id: AtomId::claim(n),
        text: text.into(),
        deontic: Some("forbids".into()),
        scope: Some(AtomId::entity(99)),
        citation: Some(ChunkRef::new(section.to_string(), Some(text.into()))),
    }
}

#[test]
fn dead_law_sections_are_the_superseded_rules_sections() {
    // claim 1 (sec-a) superseded by claim 2 (sec-b); claim 3 (sec-c)
    // active and untouched.
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(3, 1000),
        op(
            GovernanceOpKind::Supersede {
                new_rule: AtomId::claim(2),
                old_rules: vec![AtomId::claim(1)],
                rationale: String::new(),
            },
            1001,
            "human:alex",
        ),
    ];
    let rules = vec![
        rule_at(1, "sec-a", "guests may stay two nights"),
        rule_at(2, "sec-b", "no overnight guests"),
        rule_at(3, "sec-c", "quiet hours begin at 10pm"),
    ];
    let dead = build_view(&rules, &[], &ops).dead_law_sections();
    assert!(
        dead.contains("sec-a"),
        "the superseded rule's section is dead law"
    );
    assert!(
        !dead.contains("sec-b"),
        "the active successor's section is kept"
    );
    assert!(
        !dead.contains("sec-c"),
        "an untouched active section is kept"
    );
    assert_eq!(dead.len(), 1);
}

#[test]
fn dead_law_sections_flags_a_section_mixing_live_and_dead() {
    // claim 2 (sec-a) superseded; claim 1 (sec-a) STILL ACTIVE in the
    // same section. The aggressive RL-3 choice flags sec-a wholesale —
    // chunk-level retrieval can't excise one rule's sentence from a
    // chunk it shares, so the amended section is dropped and the
    // superseding decision (sec-b, kept) carries the current rule.
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(2, 1000),
        op(
            GovernanceOpKind::Supersede {
                new_rule: AtomId::claim(3),
                old_rules: vec![AtomId::claim(2)],
                rationale: String::new(),
            },
            1001,
            "human:alex",
        ),
    ];
    let rules = vec![
        rule_at(1, "sec-a", "members must accompany daytime visitors"),
        rule_at(2, "sec-a", "a guest may stay two nights"),
        rule_at(3, "sec-b", "overnight guests are not permitted"),
    ];
    let dead = build_view(&rules, &[], &ops).dead_law_sections();
    assert!(
        dead.contains("sec-a"),
        "a section with any superseded rule is dead-law wholesale"
    );
    assert!(!dead.contains("sec-b"), "the successor's section is kept");
}

#[test]
fn open_tensions_carry_both_texts_and_rank_by_confidence() {
    let rules = vec![
        rule(1, "no guests in common areas after 11"),
        rule(2, "guests may stay two nights"),
        rule(3, "quiet hours begin at 10"),
    ];
    let tensions = vec![
        tension(2, 1, 3, "weak overlap", 0.4),
        tension(1, 1, 2, "overnight vs curfew?", 0.9),
    ];
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(2, 1001),
        assert_rule(3, 1002),
    ];
    let view = build_view(&rules, &tensions, &ops);

    let open: Vec<_> = view.open_tensions().collect();
    assert_eq!(open.len(), 2);
    // Highest confidence first.
    assert_eq!(open[0].id, EdgeId::new(1));
    assert_eq!(open[0].text_a, "no guests in common areas after 11");
    assert_eq!(open[0].text_b, "guests may stay two nights");
    assert_eq!(open[0].why.as_deref(), Some("overnight vs curfew?"));
    assert_eq!(open[1].id, EdgeId::new(2));
    assert!(view.issues.is_empty());
}

#[test]
fn accepted_tension_leaves_the_open_set() {
    let rules = vec![rule(1, "a"), rule(2, "b")];
    let tensions = vec![tension(1, 1, 2, "why", 0.9)];
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(2, 1001),
        op(
            GovernanceOpKind::AcceptTension {
                tension: EdgeId::new(1),
                rationale: "intentional".into(),
                endpoints: None,
            },
            1002,
            "human:alex",
        ),
    ];
    let view = build_view(&rules, &tensions, &ops);
    assert_eq!(view.open_tensions().count(), 0);
    assert!(matches!(
        view.tensions[0].disposition,
        TensionDisposition::Accepted { .. }
    ));
}

#[test]
fn resolved_tension_shows_resolved_disposition() {
    let rules = vec![rule(1, "a"), rule(2, "b")];
    let tensions = vec![tension(1, 1, 2, "why", 0.9)];
    let supersede = op(
        GovernanceOpKind::Supersede {
            new_rule: AtomId::claim(2),
            old_rules: vec![AtomId::claim(1)],
            rationale: String::new(),
        },
        1002,
        "human:alex",
    );
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(2, 1001),
        supersede.clone(),
        op(
            GovernanceOpKind::ResolveTension {
                tension: EdgeId::new(1),
                via: supersede.id.clone(),
                endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
                rationale: String::new(),
            },
            1003,
            "human:alex",
        ),
    ];
    let view = build_view(&rules, &tensions, &ops);
    assert_eq!(view.open_tensions().count(), 0);
    assert!(matches!(
        view.tensions[0].disposition,
        TensionDisposition::Resolved { .. }
    ));
}

#[test]
fn dismissed_tension_leaves_the_open_set() {
    let rules = vec![rule(1, "a"), rule(2, "b")];
    let tensions = vec![tension(1, 1, 2, "why", 0.9)];
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(2, 1001),
        op(
            GovernanceOpKind::DismissTension {
                tension: EdgeId::new(1),
                endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
                rationale: "detector noise".into(),
            },
            1002,
            "human:alex",
        ),
    ];
    let view = build_view(&rules, &tensions, &ops);
    assert_eq!(view.open_tensions().count(), 0);
    assert!(matches!(
        view.tensions[0].disposition,
        TensionDisposition::Dismissed { .. }
    ));
    assert!(view.issues.is_empty());
}

/// covers: EN-19
///
/// The clause's first half: an adjudication recorded on the endpoint RULE PAIR
/// survives a rebuild that re-mints every edge id.
#[test]
fn pair_matched_disposition_survives_rebuild_edge_ids() {
    // Week 1: accept the conflict between rules 1 and 2, adjudicating
    // edge-0001 and recording the endpoint pair. Week 2: the atlas is
    // rebuilt and the same conflict re-surfaces under a NEW edge id
    // (edge-0005). The decision must carry over via the pair map, and
    // no false "not surfaced" issue may fire.
    let rules = vec![rule(1, "a"), rule(2, "b")];
    let rebuilt_tensions = vec![tension(5, 1, 2, "why", 0.9)];
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(2, 1001),
        op(
            GovernanceOpKind::AcceptTension {
                tension: EdgeId::new(1),
                rationale: "both can stand".into(),
                endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
            },
            1002,
            "human:alex",
        ),
    ];
    let view = build_view(&rules, &rebuilt_tensions, &ops);
    assert_eq!(view.tensions[0].id, EdgeId::new(5));
    assert!(
        matches!(
            view.tensions[0].disposition,
            TensionDisposition::Accepted { .. }
        ),
        "the re-minted edge inherits the pair's accepted disposition"
    );
    assert_eq!(view.open_tensions().count(), 0);
    assert!(
        view.issues.is_empty(),
        "a pair re-detected under a new edge id is not drift"
    );
}

/// covers: EN-19
///
/// The clause's mootness half: a conflict whose rule has been superseded is
/// not open.
#[test]
fn tension_with_superseded_endpoint_is_moot_not_open() {
    // Rule 1 was superseded by rule 2. A tension the detector surfaces
    // between the dead rule 1 and a live rule 3 is not a live question —
    // it is moot, off the agenda, with no adjudication needed.
    let rules = vec![rule(1, "dead"), rule(2, "successor"), rule(3, "live")];
    let tensions = vec![tension(7, 1, 3, "stale overlap", 0.8)];
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(3, 1001),
        op(
            GovernanceOpKind::Supersede {
                new_rule: AtomId::claim(2),
                old_rules: vec![AtomId::claim(1)],
                rationale: String::new(),
            },
            1002,
            "human:alex",
        ),
    ];
    let view = build_view(&rules, &tensions, &ops);
    assert_eq!(view.open_tensions().count(), 0);
    assert!(matches!(
        view.tensions[0].disposition,
        TensionDisposition::Moot { .. }
    ));
    if let TensionDisposition::Moot { dead_endpoint } = &view.tensions[0].disposition {
        assert_eq!(dead_endpoint, &AtomId::claim(1));
    }
}

/// covers: EN-19
///
/// The clause's last half: ONLY a genuinely dangling decision — one whose rule
/// text was edited away — surfaces as an issue. A pair awaiting re-detection
/// is weekly variance.
#[test]
fn vanished_pair_is_not_an_issue_but_missing_endpoint_atom_is() {
    // (a) A pair decision whose edge the detector simply didn't
    //     re-surface this rebuild — both rule atoms still exist — is
    //     normal weekly variance, NOT an issue.
    let rules = vec![rule(1, "a"), rule(2, "b")];
    let ops = vec![
        assert_rule(1, 1000),
        assert_rule(2, 1001),
        op(
            GovernanceOpKind::AcceptTension {
                tension: EdgeId::new(1),
                rationale: "both can stand".into(),
                endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
            },
            1002,
            "human:alex",
        ),
    ];
    let view = build_view(&rules, &[], &ops);
    assert!(
        view.issues.is_empty(),
        "a valid pair awaiting re-detection is not drift"
    );

    // (b) A pair decision one of whose rule atoms has vanished (the
    //     rule text was edited into a new atom) IS drift needing
    //     attention.
    let ops_edited = vec![
        assert_rule(1, 1000),
        op(
            GovernanceOpKind::AcceptTension {
                tension: EdgeId::new(1),
                rationale: "both can stand".into(),
                endpoints: Some((AtomId::claim(1), AtomId::claim(9))),
            },
            1002,
            "human:alex",
        ),
    ];
    let view_edited = build_view(&[rule(1, "a")], &[], &ops_edited);
    assert!(view_edited
        .issues
        .contains(&GovernanceIssue::AdjudicatedTensionNotSurfaced {
            tension: EdgeId::new(1)
        }));
}

#[test]
fn governed_rule_without_atom_is_an_issue() {
    let ops = vec![assert_rule(5, 1000)];
    let view = build_view(&[], &[], &ops);
    assert!(view.issues.contains(&GovernanceIssue::RuleHasNoAtom {
        rule: AtomId::claim(5)
    }));
    // Still listed (with empty text) so it's visible, not vanished.
    assert_eq!(view.rules.len(), 1);
    assert_eq!(view.rules[0].text, "");
}

#[test]
fn tension_endpoint_missing_is_an_issue() {
    let rules = vec![rule(1, "a")];
    let tensions = vec![tension(1, 1, 7, "x", 0.5)];
    let ops = vec![assert_rule(1, 1000)];
    let view = build_view(&rules, &tensions, &ops);
    assert!(view
        .issues
        .contains(&GovernanceIssue::TensionEndpointMissing {
            tension: EdgeId::new(1),
            endpoint: AtomId::claim(7),
        }));
    assert_eq!(view.tensions[0].text_b, "");
}

#[test]
fn adjudication_without_surfaced_edge_is_an_issue() {
    // Case (a): a legacy edge-id-only decision whose edge no longer
    // surfaces is genuine drift — nothing to re-match against.
    let ops = vec![op(
        GovernanceOpKind::AcceptTension {
            tension: EdgeId::new(9),
            rationale: "x".into(),
            endpoints: None,
        },
        1000,
        "human:alex",
    )];
    let view = build_view(&[], &[], &ops);
    assert!(view
        .issues
        .contains(&GovernanceIssue::AdjudicatedTensionNotSurfaced {
            tension: EdgeId::new(9)
        }));
}

#[test]
fn unattended_adjudication_is_an_issue() {
    let forged = op(
        GovernanceOpKind::Supersede {
            new_rule: AtomId::claim(2),
            old_rules: vec![AtomId::claim(1)],
            rationale: String::new(),
        },
        1000,
        "ingest", // not human:
    );
    let view = build_view(&[], &[], &[forged.clone()]);
    assert!(view.issues.contains(&GovernanceIssue::UnattendedAct {
        op: forged.id.clone()
    }));
}

#[test]
fn from_atlas_dir_reads_and_joins_real_atlas_files() {
    use crate::edges::{EdgeProvenance, EdgesFile};
    use understanding_vocab::taxonomy::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
    };

    let dir = tempfile::tempdir().unwrap();

    // Two rule Claims → atoms.json.
    let make_claim = |n: usize, content: &str| Claim {
        attributes: Default::default(),
        subject: None,
        id: AtomId::claim(n),
        content: content.into(),
        discourse_act: DiscourseAct::Enact,
        epistemic_status: EpistemicStatus::Confident,
        scope: ClaimScope::Contextual,
        evidence: vec![ChunkRef::new(format!("chunk-{n}"), None)],
        quotable_excerpt: None,
        attributed_to: Some(AtomId::entity(1)),
        confidence: None,
        anchor: None,
        claim_kind: Some("forbids".into()),
        concession_outcome: None,
        evidence_kind: None,
        enrichment_depth: EnrichmentDepth::Extracted,
    };
    let atoms = AtomsFile::new(vec![
        AtomEnvelope::Claim(make_claim(1, "old rule")),
        AtomEnvelope::Claim(make_claim(2, "new rule")),
    ]);
    std::fs::write(
        dir.path().join("atoms.json"),
        serde_json::to_vec(&atoms).unwrap(),
    )
    .unwrap();

    // One Tension edge between them → edges.json.
    let edges = EdgesFile::new(vec![Edge {
        id: EdgeId::new(1),
        edge_type: EdgeType::Tension,
        source: AtomId::claim(1),
        target: AtomId::claim(2),
        evidence: Vec::new(),
        trigger_event: None,
        sub_question: Some("why?".into()),
        confidence: 0.8,
        provenance: EdgeProvenance::Derived,
    }]);
    std::fs::write(
        dir.path().join("edges.json"),
        serde_json::to_vec(&edges).unwrap(),
    )
    .unwrap();

    // Both rules asserted by ingest → governance_oplog.jsonl.
    let log = Oplog::<GovernanceOpKind>::new(dir.path());
    log.append(&assert_rule(1, 1000)).unwrap();
    log.append(&assert_rule(2, 1001)).unwrap();

    let view = GovernanceView::from_atlas_dir(dir.path()).unwrap();
    assert_eq!(view.active_rules().count(), 2);
    let open: Vec<_> = view.open_tensions().collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].text_a, "old rule");
    assert_eq!(open[0].text_b, "new rule");
    assert_eq!(open[0].why.as_deref(), Some("why?"));
    assert!(view.issues.is_empty());
}

/// ontology-v1 P5. The projection reads what the DECLARED claim carries:
/// `subject` as the scope (the coin being dated, not the scholar dating
/// it) and the validated `deontic` attribute as the force.
#[test]
fn project_claim_prefers_subject_and_the_declared_deontic() {
    use understanding_vocab::taxonomy::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
    };

    let base = |subject: Option<AtomId>, attrs: serde_json::Map<String, serde_json::Value>| Claim {
        attributes: attrs,
        subject,
        id: AtomId::claim(1),
        content: "the mancus was struck 805/810".into(),
        discourse_act: DiscourseAct::Assert,
        epistemic_status: EpistemicStatus::Confident,
        scope: ClaimScope::Contextual,
        evidence: vec![ChunkRef::new("chunk-1", None)],
        quotable_excerpt: None,
        // The VOICE: the scholar making the attribution.
        attributed_to: Some(AtomId::entity(7)),
        confidence: None,
        anchor: None,
        claim_kind: Some("attribution".into()),
        concession_outcome: None,
        evidence_kind: None,
        enrichment_depth: EnrichmentDepth::Extracted,
    };

    // Declared: subject wins over attributed_to, deontic off the attribute.
    let mut attrs = serde_json::Map::new();
    attrs.insert("deontic".into(), serde_json::Value::String("forbid".into()));
    let declared = project_claim(&base(Some(AtomId::entity(42)), attrs));
    assert_eq!(declared.scope, Some(AtomId::entity(42)));
    assert_ne!(declared.scope, Some(AtomId::entity(7)));
    assert_eq!(declared.deontic.as_deref(), Some("forbid"));

    // I5: no subject and no deontic attribute — an undeclared corpus's
    // claim projects exactly as it did before ontology v1.
    let undeclared = project_claim(&base(None, serde_json::Map::new()));
    assert_eq!(undeclared.scope, Some(AtomId::entity(7)));
    assert_eq!(undeclared.deontic.as_deref(), Some("attribution"));

    // A blank or non-string reserved value is an absence, not a value:
    // it falls back rather than projecting an empty deontic.
    let mut blank = serde_json::Map::new();
    blank.insert("deontic".into(), serde_json::Value::String("  ".into()));
    assert_eq!(
        project_claim(&base(None, blank)).deontic.as_deref(),
        Some("attribution")
    );
    let mut wrong_type = serde_json::Map::new();
    wrong_type.insert("deontic".into(), serde_json::json!(3));
    assert_eq!(
        project_claim(&base(None, wrong_type)).deontic.as_deref(),
        Some("attribution")
    );
}

#[test]
fn empty_atlas_dir_yields_empty_view() {
    let dir = tempfile::tempdir().unwrap();
    let view = GovernanceView::from_atlas_dir(dir.path()).unwrap();
    assert_eq!(view, GovernanceView::default());
}

fn write_manifest(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("chapters.json"), body).unwrap();
}

/// The distinction the whole surface exists for: an empty map because the
/// corpus has no sections, vs an empty map because its join was never
/// written. Conflating these is what hid a broken join across 1779 of
/// 1788 corpora.
/// covers: ST-25
///
/// Value 1 of the trichotomy: no structure declared.
#[test]
fn an_absent_manifest_is_no_structure_not_a_missing_join() {
    let dir = tempfile::tempdir().unwrap();
    let got = chunk_to_section_map_status(dir.path());
    assert_eq!(got.status, JoinStatus::NoSectionStructure);
    assert!(got.map.is_empty());
    assert!(
        got.warning("x").is_none(),
        "a structureless corpus must not nag"
    );
}

/// covers: ST-25
///
/// Value 2: structure declared but join missing. Conflating this with value 1
/// is what lets a broken corpus read as a flat one.
#[test]
fn sections_with_no_chunk_ids_are_a_missing_join_and_say_so() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        r#"{"corpus_id":"c","schema_version":1,"chapters":[
                 {"id":"sec_0001","title":"CHAPTER I","first_line":"","word_count":1,"chunk_ids":[]},
                 {"id":"sec_0002","title":"CHAPTER II","first_line":"","word_count":1,"chunk_ids":[]}]}"#,
    );
    let got = chunk_to_section_map_status(dir.path());
    assert_eq!(got.status, JoinStatus::JoinMissing);
    assert_eq!(got.sections_total, 2);
    assert_eq!(got.sections_with_chunks, 0);
    let w = got
        .warning("chaos-saltgrass")
        .expect("a fault must be reportable");
    assert!(
        w.contains("backfill-sections chaos-saltgrass"),
        "must name the repair: {w}"
    );
}

/// covers: ST-25
///
/// Value 3: join present.
#[test]
fn a_populated_join_is_present_and_silent() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        r#"{"corpus_id":"c","schema_version":1,"chapters":[
                 {"id":"sec_0001","title":"I","first_line":"","word_count":1,"chunk_ids":[1,2]},
                 {"id":"sec_0002","title":"II","first_line":"","word_count":1,"chunk_ids":[]}]}"#,
    );
    let got = chunk_to_section_map_status(dir.path());
    assert_eq!(got.status, JoinStatus::Present);
    assert_eq!(
        got.sections_with_chunks, 1,
        "a partial join is still present"
    );
    assert_eq!(got.map.get(&2).map(String::as_str), Some("sec_0001"));
    assert!(got.warning("x").is_none());
}

/// A manifest declaring zero sections has nothing that COULD be joined.
/// covers: ST-25
///
/// Value 1 again, by the other door — an empty chapter list rather than an
/// absent manifest. Both must land on `NoSectionStructure`.
#[test]
fn an_empty_chapter_list_is_no_structure_not_a_fault() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(
        dir.path(),
        r#"{"corpus_id":"c","schema_version":1,"chapters":[]}"#,
    );
    assert_eq!(
        chunk_to_section_map_status(dir.path()).status,
        JoinStatus::NoSectionStructure
    );
}

/// The legacy wrapper keeps its "empty means don't filter" contract.
#[test]
fn the_map_only_accessor_still_degrades_to_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(chunk_to_section_map(dir.path()).is_empty());
}
