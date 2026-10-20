use super::*;
use std::fs::OpenOptions;
use std::io::Write;

fn rule(n: usize) -> AtomId {
    AtomId::claim(n)
}
fn tension(n: usize) -> EdgeId {
    EdgeId::new(n)
}

/// Build an op at a monotonic, explicit timestamp so ids are
/// deterministic and distinct within a test.
fn op(kind: GovernanceOpKind, ts: i64, actor: &str) -> Op<GovernanceOpKind> {
    Op::new(kind, ts, actor)
}

#[test]
fn assert_then_supersede() {
    let old = rule(1);
    let new = rule(2);
    let ops = vec![
        op(
            GovernanceOpKind::AssertRule {
                rule: old.clone(),
                source_doc: Some("charter.md".into()),
            },
            1000,
            "ingest",
        ),
        op(
            GovernanceOpKind::Supersede {
                new_rule: new.clone(),
                old_rules: vec![old.clone()],
                rationale: "May 12 house meeting".into(),
            },
            1001,
            "human:alex",
        ),
    ];
    let set = derive_active(&ops);
    assert!(set.is_active(&new));
    assert!(matches!(
        set.rules.get(&old),
        Some(RuleStatus::Superseded { by_rules, .. }) if by_rules == &vec![new.clone()]
    ));
    assert_eq!(set.active_rules(), vec![&new]);
}

#[test]
fn retract_rule_drops_it() {
    let r = rule(1);
    let ops = vec![
        op(
            GovernanceOpKind::AssertRule {
                rule: r.clone(),
                source_doc: None,
            },
            1000,
            "ingest",
        ),
        op(
            GovernanceOpKind::RetractRule {
                rule: r.clone(),
                rationale: "no longer applies".into(),
            },
            1001,
            "human:sam",
        ),
    ];
    let set = derive_active(&ops);
    assert!(!set.is_active(&r));
    assert!(matches!(
        set.rules.get(&r),
        Some(RuleStatus::Retracted { .. })
    ));
    assert!(set.active_rules().is_empty());
}

#[test]
fn accept_tension_is_first_class_and_closes_open() {
    let t = tension(1);
    let other = tension(2);
    let ops = vec![op(
        GovernanceOpKind::AcceptTension {
            tension: t.clone(),
            rationale: "quiet-hours vs private-room is intentional".into(),
            endpoints: None,
        },
        1000,
        "human:alex",
    )];
    let set = derive_active(&ops);
    assert!(matches!(
        set.tensions.get(&t),
        Some(TensionStatus::Accepted { .. })
    ));
    // The unadjudicated tension is still open; the accepted one is not.
    assert_eq!(set.open_tensions(&[t.clone(), other.clone()]), vec![&other]);
}

#[test]
fn dismiss_tension_closes_open_and_is_revertible() {
    let t = tension(1);
    let other = tension(2);
    let dismiss = op(
        GovernanceOpKind::DismissTension {
            tension: t.clone(),
            endpoints: None,
            rationale: String::new(),
        },
        1000,
        "human:alex",
    );
    let set = derive_active(std::slice::from_ref(&dismiss));
    assert!(matches!(
        set.tensions.get(&t),
        Some(TensionStatus::Dismissed { .. })
    ));
    // Dismissed leaves the open set; the untouched tension stays open.
    assert_eq!(set.open_tensions(&[t.clone(), other.clone()]), vec![&other]);

    // Reverting the dismissal reopens the tension.
    let revert = op(
        GovernanceOpKind::Revert {
            targets: vec![dismiss.id.clone()],
            rationale: "mis-click".into(),
        },
        1001,
        "human:alex",
    );
    let set = derive_active(&[dismiss, revert]);
    assert!(set.tensions.is_empty());
    assert_eq!(
        set.open_tensions(&[t.clone(), other.clone()]),
        vec![&t, &other]
    );
}

#[test]
fn adjudication_by_endpoint_pair_survives_edge_renumbering() {
    // Adjudicate edge-0001 carrying its endpoint rule pair. After a
    // rebuild the same conceptual tension is re-minted as a NEW edge
    // id — the pair map is what lets the view still see it settled.
    let (a, b) = (rule(1), rule(2));
    let ops = vec![op(
        GovernanceOpKind::AcceptTension {
            tension: tension(1),
            rationale: "both can stand".into(),
            endpoints: Some((b.clone(), a.clone())), // reversed on purpose
        },
        1000,
        "human:alex",
    )];
    let set = derive_active(&ops);
    // Pair lookup is order-independent (normalized key).
    assert!(matches!(
        set.tension_pairs.get(&PairKey::new(&a, &b)),
        Some(TensionStatus::Accepted { .. })
    ));
    assert!(matches!(
        set.tension_pairs.get(&PairKey::new(&b, &a)),
        Some(TensionStatus::Accepted { .. })
    ));
    // The renumbered edge id itself is unknown to the edge-id map…
    assert!(set.tensions.get(&tension(99)).is_none());
    // …and an op without endpoints populates no pair entry.
    let bare = derive_active(&[op(
        GovernanceOpKind::AcceptTension {
            tension: tension(3),
            rationale: "legacy".into(),
            endpoints: None,
        },
        1001,
        "human:alex",
    )]);
    assert!(bare.tension_pairs.is_empty());
}

#[test]
fn resolve_tension_links_to_supersede() {
    let old = rule(1);
    let new = rule(2);
    let t = tension(1);
    let supersede = op(
        GovernanceOpKind::Supersede {
            new_rule: new.clone(),
            old_rules: vec![old.clone()],
            rationale: String::new(),
        },
        1001,
        "human:alex",
    );
    let supersede_id = supersede.id.clone();
    let ops = vec![
        op(
            GovernanceOpKind::AssertRule {
                rule: old.clone(),
                source_doc: None,
            },
            1000,
            "ingest",
        ),
        supersede,
        op(
            GovernanceOpKind::ResolveTension {
                tension: t.clone(),
                via: supersede_id.clone(),
                endpoints: None,
                rationale: String::new(),
            },
            1002,
            "human:alex",
        ),
    ];
    let set = derive_active(&ops);
    assert!(matches!(
        set.tensions.get(&t),
        Some(TensionStatus::Resolved { .. })
    ));
    assert!(matches!(
        set.rules.get(&old),
        Some(RuleStatus::Superseded { .. })
    ));
    assert!(set.is_active(&new));
}

#[test]
fn revert_supersede_reactivates_old_rule() {
    // Realistic resolution bundle: draft the new rule (AssertRule),
    // supersede the old, resolve the tension — then a human reverts
    // the whole bundle. The old rule must come back to force and the
    // tension must reopen.
    let old = rule(1);
    let new = rule(2);
    let t = tension(1);

    let assert_old = op(
        GovernanceOpKind::AssertRule {
            rule: old.clone(),
            source_doc: None,
        },
        1000,
        "ingest",
    );
    let assert_new = op(
        GovernanceOpKind::AssertRule {
            rule: new.clone(),
            source_doc: None,
        },
        1001,
        "human:alex",
    );
    let supersede = op(
        GovernanceOpKind::Supersede {
            new_rule: new.clone(),
            old_rules: vec![old.clone()],
            rationale: String::new(),
        },
        1002,
        "human:alex",
    );
    let resolve = op(
        GovernanceOpKind::ResolveTension {
            tension: t.clone(),
            via: supersede.id.clone(),
            endpoints: None,
            rationale: String::new(),
        },
        1003,
        "human:alex",
    );
    let revert = op(
        GovernanceOpKind::Revert {
            targets: vec![
                assert_new.id.clone(),
                supersede.id.clone(),
                resolve.id.clone(),
            ],
            rationale: "decision reversed at next meeting".into(),
        },
        1004,
        "human:sam",
    );

    let ops = vec![assert_old, assert_new, supersede, resolve, revert];
    let set = derive_active(&ops);

    // Old rule is active again; the orphan new rule and the tension
    // adjudication are gone.
    assert!(set.is_active(&old));
    assert!(
        !set.rules.contains_key(&new),
        "reverted draft rule should not exist"
    );
    assert_eq!(set.active_rules(), vec![&old]);
    assert_eq!(set.open_tensions(&[t.clone()]), vec![&t]);
}

#[test]
fn revert_of_revert_reapplies() {
    let old = rule(1);
    let new = rule(2);

    let assert_old = op(
        GovernanceOpKind::AssertRule {
            rule: old.clone(),
            source_doc: None,
        },
        1000,
        "ingest",
    );
    let supersede = op(
        GovernanceOpKind::Supersede {
            new_rule: new.clone(),
            old_rules: vec![old.clone()],
            rationale: String::new(),
        },
        1001,
        "human:alex",
    );
    let revert1 = op(
        GovernanceOpKind::Revert {
            targets: vec![supersede.id.clone()],
            rationale: "undo".into(),
        },
        1002,
        "human:sam",
    );
    let revert2 = op(
        GovernanceOpKind::Revert {
            targets: vec![revert1.id.clone()],
            rationale: "actually, keep the supersession".into(),
        },
        1003,
        "human:alex",
    );

    // After revert1 alone: old active.
    let after_one = derive_active(&[assert_old.clone(), supersede.clone(), revert1.clone()]);
    assert!(after_one.is_active(&old));

    // After revert2 (revert of the revert): supersession restored.
    let after_two = derive_active(&[assert_old, supersede, revert1, revert2]);
    assert!(after_two.is_active(&new));
    assert!(matches!(
        after_two.rules.get(&old),
        Some(RuleStatus::Superseded { .. })
    ));
}

#[test]
fn reassert_is_idempotent() {
    let r = rule(1);
    let ops = vec![
        op(
            GovernanceOpKind::AssertRule {
                rule: r.clone(),
                source_doc: None,
            },
            1000,
            "ingest",
        ),
        op(
            GovernanceOpKind::AssertRule {
                rule: r.clone(),
                source_doc: None,
            },
            1001,
            "ingest",
        ),
    ];
    let set = derive_active(&ops);
    assert_eq!(set.active_rules(), vec![&r]);
    assert_eq!(set.rules.len(), 1);
}

#[test]
fn op_ids_are_deterministic_and_distinct() {
    let kind = GovernanceOpKind::AcceptTension {
        tension: tension(1),
        rationale: "ok".into(),
        endpoints: None,
    };
    // Same (kind, ts, actor) → same id.
    let a = Op::new(kind.clone(), 5, "human:alex");
    let b = Op::new(kind.clone(), 5, "human:alex");
    assert_eq!(a.id, b.id);
    // Different ts → different id.
    let c = Op::new(kind, 6, "human:alex");
    assert_ne!(a.id, c.id);
    // Id is a content-hash shape.
    assert!(a.id.as_str().starts_with("gov-"));
    assert_eq!(a.id.as_str().len(), "gov-".len() + 16);
}

#[test]
fn unattended_adjudication_is_detected() {
    // An AssertRule by ingest is allowed.
    let assert = op(
        GovernanceOpKind::AssertRule {
            rule: rule(1),
            source_doc: None,
        },
        1000,
        "ingest",
    );
    assert!(first_unattended_act(std::slice::from_ref(&assert)).is_none());

    // A Supersede must be human-authored.
    let forged = op(
        GovernanceOpKind::Supersede {
            new_rule: rule(2),
            old_rules: vec![rule(1)],
            rationale: String::new(),
        },
        1001,
        "ingest",
    );
    assert!(first_unattended_act(&[assert.clone(), forged.clone()]).is_some());

    let proper = op(
        GovernanceOpKind::Supersede {
            new_rule: rule(2),
            old_rules: vec![rule(1)],
            rationale: String::new(),
        },
        1001,
        "human:alex",
    );
    assert!(first_unattended_act(&[assert, proper]).is_none());
}

#[test]
fn replay_round_trips_through_disk_byte_for_byte() {
    // INV-1: write → read → fold reproduces the in-memory fold, and
    // the read-back ops equal what was written.
    let dir = tempfile::tempdir().unwrap();
    let log = Oplog::<GovernanceOpKind>::new(dir.path());

    let old = rule(1);
    let new = rule(2);
    let t = tension(1);
    let assert_old = op(
        GovernanceOpKind::AssertRule {
            rule: old.clone(),
            source_doc: Some("charter.md".into()),
        },
        1000,
        "ingest",
    );
    let supersede = op(
        GovernanceOpKind::Supersede {
            new_rule: new.clone(),
            old_rules: vec![old.clone()],
            rationale: "meeting".into(),
        },
        1001,
        "human:alex",
    );
    let resolve = op(
        GovernanceOpKind::ResolveTension {
            tension: t.clone(),
            via: supersede.id.clone(),
            endpoints: Some((old.clone(), new.clone())),
            rationale: String::new(),
        },
        1002,
        "human:alex",
    );
    let dismiss = op(
        GovernanceOpKind::DismissTension {
            tension: tension(2),
            endpoints: Some((rule(3), rule(4))),
            rationale: "parking vs stays — not a conflict".into(),
        },
        1003,
        "human:alex",
    );
    let written = vec![assert_old, supersede, resolve, dismiss];
    for o in &written {
        log.append(o).unwrap();
    }

    let read_back = log.read_all().unwrap();
    assert_eq!(read_back, written, "ops must survive a disk round-trip");
    assert_eq!(
        derive(&log).unwrap(),
        derive_active(&written),
        "fold over disk == fold over memory"
    );
}

#[test]
fn read_skips_future_version_lines() {
    // Forward-compat: a line declaring a newer `v` is skipped, not
    // misinterpreted (schema-back-compat rule).
    let dir = tempfile::tempdir().unwrap();
    let log = Oplog::<GovernanceOpKind>::new(dir.path());
    let good = op(
        GovernanceOpKind::AssertRule {
            rule: rule(1),
            source_doc: None,
        },
        1000,
        "ingest",
    );
    log.append(&good).unwrap();
    // Hand-write a structurally-valid line from a future version.
    let future = format!(
        r#"{{"id":"gov-future00000000","v":{},"ts_unix":1001,"actor":"human:x","op":"assert_rule","rule":"claim-9999"}}"#,
        GOVERNANCE_OPLOG_VERSION + 1
    );
    let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
    f.write_all(future.as_bytes()).unwrap();
    f.write_all(b"\n").unwrap();

    let read = log.read_all().unwrap();
    assert_eq!(read.len(), 1, "future-version line should be skipped");
    assert_eq!(read[0].id, good.id);
}

#[test]
fn missing_log_folds_to_empty() {
    let dir = tempfile::tempdir().unwrap();
    let set = derive(&Oplog::<GovernanceOpKind>::new(dir.path())).unwrap();
    assert!(set.rules.is_empty());
    assert!(set.tensions.is_empty());
}
