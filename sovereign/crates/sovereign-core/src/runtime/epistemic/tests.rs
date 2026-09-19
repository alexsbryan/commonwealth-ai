use super::*;

/// Invariant I1 (EPISTEMIC_STATE §7) — every ANSWER surface persists
/// an `epistemic_state`. This is the closed-set pin on the
/// `GateSurface` precedent: the match below is exhaustive, so adding
/// a surface variant FAILS COMPILATION here until its ledger story
/// is decided and recorded. The strings name the persistence sites
/// (grep for `"epistemic_state"` to verify them).
#[test]
fn every_answer_surface_has_a_ledger_story() {
    use crate::runtime::grounding::GateSurface;
    fn ledger_site(s: GateSurface) -> Option<&'static str> {
        match s {
            GateSurface::KnowledgeQuery => {
                Some("handlers/knowledge_query.rs (sync) + streaming.rs (KQ spawn)")
            }
            GateSurface::DeepQuery => Some("streaming.rs (deep spawn)"),
            GateSurface::AttachedDoc => Some("handlers/attached_doc.rs"),
            GateSurface::ComplexTask => Some("handlers/complex_task.rs"),
            GateSurface::SimpleQuery => Some("handlers/simple.rs"),
            // Not standalone answer surfaces: Refinement re-verifies
            // an existing answer inside its owning surface's turn;
            // Governance/ProxyArgument are gate-calibration profiles
            // that fire inside the KQ handler and inherit its
            // persistence site.
            GateSurface::Refinement => None,
            GateSurface::Governance => None,
            GateSurface::ProxyArgument => None,
        }
    }
    // Every answer surface must name a persistence site.
    for s in [
        GateSurface::KnowledgeQuery,
        GateSurface::DeepQuery,
        GateSurface::AttachedDoc,
        GateSurface::ComplexTask,
        GateSurface::SimpleQuery,
    ] {
        assert!(
            ledger_site(s).is_some(),
            "answer surface {s:?} has no ledger site"
        );
    }
}

fn corpus_holding(verification: Verification) -> Holding {
    Holding {
        claim: "c".into(),
        provenance: Provenance::Corpus {
            corpus_id: Some("wiki".into()),
            chunk_id: None,
            member: None,
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
        derive_verdict(
            &[corpus_holding(Verification::Verified)],
            true,
            false,
            true,
            false
        ),
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
        derive_verdict(
            &[memory_holding(Verification::FailOpen)],
            false,
            false,
            false,
            false
        ),
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

/// covers: GR-47
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
        ..EpistemicInputs::over(PoolContext::none())
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
        ..EpistemicInputs::over(PoolContext::none())
    });
    assert!(state.holdings.is_empty());
    assert_eq!(state.verdict, TurnVerdict::CannotKnowFromHere);
}

/// covers: GR-46
///
/// The three halves of the ledger are assembled in ONE pass and must agree
/// with each other. Every other test here exercises a single slice —
/// verdict truth table, holdings-only, gaps-only — so the failure this one
/// catches is invisible to all of them: a change that populates `gaps`
/// correctly while leaving `holdings` stale, or a verdict that disagrees
/// with the holdings it is derived from.
#[test]
fn one_assembly_returns_holdings_verdict_and_gaps_that_agree_with_each_other() {
    let meta = serde_json::json!({"action": "released", "retried": false});
    let claims = vec![GateClaim {
        text: "The ferry leaves Ardrossan at 07:00".into(),
        supported: true,
        failed_once: false,
        unjudged: false,
        violation_prob: Some(0.02),
        address: None,
    }];
    // Two demands: one the answer actually covered, one nothing in the
    // pool reached. The gap row names the second by index.
    let demands = vec![
        Demand {
            facet: DemandFacet::Query,
            text: "when does the ferry leave Ardrossan".into(),
            covered: CoverageLevel::Supported,
        },
        Demand {
            facet: DemandFacet::Entity,
            text: "Brodick pier reconstruction".into(),
            covered: CoverageLevel::Absent,
        },
    ];
    let gaps = vec![Gap {
        demand_idx: 1,
        statement: "no installed corpus covers the Brodick pier works".into(),
        coverage: GapCoverage::TopicUncovered,
        routes: Vec::new(),
    }];

    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        gate_claims: Some(&claims),
        demands,
        gaps,
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["arran-ferries".into()],
            members: vec![],
        })
    });

    // 1. All three are populated by the SAME call. A ledger carrying gaps
    //    but no holdings (or the reverse) is the stale-half failure.
    assert_eq!(
        state.holdings.len(),
        1,
        "the audited claim must reach holdings in the same pass that carries the gaps"
    );
    assert_eq!(state.demands.len(), 2);
    assert_eq!(state.gaps.len(), 1);

    // 2. The verdict agrees with the holdings it derives from: one
    //    corpus holding, verified, single-corpus pool.
    assert_eq!(state.holdings[0].verification, Verification::Verified);
    assert!(matches!(
        state.holdings[0].provenance,
        Provenance::Corpus { .. }
    ));
    assert_eq!(
        state.verdict,
        derive_verdict(&state.holdings, false, false, true, false),
        "the verdict must be the derivation over the holdings actually shipped, not a stale one"
    );
    assert_eq!(state.verdict, TurnVerdict::Grounded);

    // 3. Every gap resolves to a real demand, and that demand is not one
    //    the same assembly called covered. A gap pointing at a Supported
    //    demand is the two halves disagreeing.
    for gap in &state.gaps {
        let demand = state
            .demands
            .get(gap.demand_idx)
            .expect("every gap must index a demand in the same ledger");
        assert_ne!(
            demand.covered,
            CoverageLevel::Supported,
            "gap {:?} points at a demand this same assembly reported covered",
            gap.statement
        );
    }

    // 4. And the converse: nothing marked covered acquires a gap row.
    for (i, demand) in state.demands.iter().enumerate() {
        if demand.covered == CoverageLevel::Supported {
            assert!(
                !state.gaps.iter().any(|g| g.demand_idx == i),
                "covered demand {i} must not also be reported as a gap"
            );
        }
    }
}

#[test]
fn abstained_turn_asserts_nothing() {
    let meta = serde_json::json!({"action": "abstained", "retried": true});
    let claims = vec![GateClaim {
        text: "Heat's first name is Vernon".into(),
        supported: false,
        failed_once: true,
        unjudged: false,
        violation_prob: Some(0.97),
        address: None,
    }];
    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        gate_claims: Some(&claims),
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["secret-agent".into()],
            members: vec![],
        })
    });
    assert!(state.holdings.is_empty());
    assert_eq!(state.verdict, TurnVerdict::CannotKnowFromHere);
}

/// Issue #57: eight per-claim judges were shed by the admission queue,
/// the gate exited `released`, and every holding rendered Verified. The
/// per-claim record now carries `unjudged`, and it wins over the gate's
/// action string: a claim nobody judged is FailOpen even on a `released`
/// turn, and such a turn is never `Grounded`.
#[test]
fn an_unjudged_claim_is_fail_open_even_when_the_gate_action_is_released() {
    let meta = serde_json::json!({"action": "released", "retried": false});
    let claims = vec![
        GateClaim {
            text: "The shop is on Harbour Row".into(),
            supported: true,
            failed_once: false,
            unjudged: false,
            violation_prob: Some(0.1),
            address: None,
        },
        GateClaim {
            text: "The shop opens at dawn".into(),
            supported: false,
            failed_once: false,
            unjudged: true,
            violation_prob: None,
            address: None,
        },
    ];
    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        gate_claims: Some(&claims),
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["shop".into()],
            members: vec![],
        })
    });
    assert_eq!(state.holdings.len(), 2);
    assert_eq!(state.holdings[0].verification, Verification::Verified);
    assert_eq!(
        state.holdings[1].verification,
        Verification::FailOpen,
        "a claim the judge never reached must not read as verified"
    );
    assert_ne!(
        state.verdict,
        TurnVerdict::Grounded,
        "one unjudged holding is enough to withhold Grounded"
    );
}

/// The released passages reach the ledger as structured rows a reading
/// surface can open. Before this, the gate's citation existed downstream
/// only as prose inside the answer string — the system's best-attested
/// citation was the one citation a user could not click.
#[test]
fn released_citations_reach_the_ledger() {
    let meta = serde_json::json!({
        "action": "citation_grounded",
        "citations": [
            {
                "text": "The Cold Lantern stood at the head of the quay.",
                "locator": "CHAPTER VII",
                "target": {"corpus_id": "chaos-saltgrass", "chunk_id": 41}
            },
            {
                // No locator: a corpus with no section structure is still
                // openable. The two facts are independent.
                "text": "Tabb Orrison found the body in the basin.",
                "target": {"corpus_id": "chaos-saltgrass", "chunk_id": 77}
            }
        ]
    });
    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["chaos-saltgrass".into()],
            members: vec![],
        })
    });
    assert_eq!(state.citations.len(), 2);
    assert_eq!(state.citations[0].locator.as_deref(), Some("CHAPTER VII"));
    assert_eq!(state.citations[0].target.chunk_id, 41);
    assert_eq!(state.citations[0].target.corpus_id, "chaos-saltgrass");
    assert_eq!(
        state.citations[1].locator, None,
        "a passage with no chapter heading is still openable"
    );
    assert_eq!(state.citations[1].target.chunk_id, 77);
}

/// An abstained turn asserts nothing, so it cites nothing — the same rule
/// holdings follow. Without this, a turn that declined to answer would
/// still offer the reader passages as though they grounded a claim.
#[test]
fn an_abstained_turn_cites_nothing() {
    let meta = serde_json::json!({
        "action": "abstained_specifics",
        "citations": [{
            "text": "The Cold Lantern stood at the head of the quay.",
            "target": {"corpus_id": "chaos-saltgrass", "chunk_id": 41}
        }]
    });
    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["chaos-saltgrass".into()],
            members: vec![],
        })
    });
    assert!(state.citations.is_empty());
}

/// Legacy-ladder releases, parametric turns and any transcript banked
/// before this field existed carry no `citations` key. Empty is the
/// honest reading — nothing is shown as openable rather than a guess
/// being rendered.
#[test]
fn a_turn_without_citations_reads_as_empty_not_as_a_failure() {
    for meta in [
        serde_json::json!({"action": "released"}),
        serde_json::json!({"action": "released", "citations": "not-an-array"}),
    ] {
        let state = assemble_epistemic_state(EpistemicInputs {
            gate_meta: Some(&meta),
            ..EpistemicInputs::over(PoolContext {
                corpora: vec!["chaos-saltgrass".into()],
                members: vec![],
            })
        });
        assert!(state.citations.is_empty(), "meta: {meta}");
    }
}

#[test]
fn single_corpus_pool_attributes_corpus_id() {
    let meta = serde_json::json!({"action": "released"});
    let claims = vec![GateClaim {
        text: "The knife was a carving knife".into(),
        supported: true,
        failed_once: false,
        unjudged: false,
        violation_prob: Some(0.02),
        address: None,
    }];
    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        gate_claims: Some(&claims),
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["secret-agent".into()],
            members: vec![],
        })
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
        unjudged: false,
        violation_prob: None,
        address: None,
    }];
    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        gate_claims: Some(&claims),
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["wikipedia".into(), "sep".into()],
            members: vec![],
        })
    });
    assert!(matches!(
        &state.holdings[0].provenance,
        Provenance::Corpus {
            corpus_id: None,
            ..
        }
    ));
}

fn member_of_first_holding(pool_members: Vec<Option<String>>) -> Option<String> {
    let meta = serde_json::json!({"action": "released"});
    let claims = vec![GateClaim {
        text: "x".into(),
        supported: true,
        failed_once: false,
        unjudged: false,
        violation_prob: None,
        address: None,
    }];
    let state = assemble_epistemic_state(EpistemicInputs {
        gate_meta: Some(&meta),
        gate_claims: Some(&claims),
        ..EpistemicInputs::over(PoolContext {
            corpora: vec!["ring-room".into()],
            members: pool_members,
        })
    });
    match &state.holdings[0].provenance {
        Provenance::Corpus { member, .. } => member.clone(),
        other => panic!("expected a corpus holding, got {other:?}"),
    }
}

#[test]
fn sole_member_pool_names_the_member() {
    let bo = Some("Bo".to_string());
    assert_eq!(
        member_of_first_holding(vec![bo.clone(), bo.clone(), bo]),
        Some("Bo".into())
    );
}

#[test]
fn mixed_member_pool_leaves_member_open() {
    assert_eq!(member_of_first_holding(vec![Some("Bo".into()), None]), None);
    assert_eq!(
        member_of_first_holding(vec![Some("Bo".into()), Some("Al".into())]),
        None
    );
}

#[test]
fn local_pool_names_no_member() {
    assert_eq!(member_of_first_holding(vec![None, None]), None);
    assert_eq!(member_of_first_holding(vec![]), None);
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
        ..EpistemicInputs::over(PoolContext::none())
    });
    assert_eq!(state.holdings.len(), 1);
    assert!(matches!(
        &state.holdings[0].provenance,
        Provenance::Memory { band: MemoryBand::ToldDirectly, entry_id } if entry_id == "mem-1"
    ));
    assert_eq!(state.holdings[0].verification, Verification::Verified);
    assert_eq!(state.verdict, TurnVerdict::MemoryRecall);
}

mod demands;
