// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;
use crate::enrichment::atlas::atoms::Claim;
use crate::recipe_ontology::OntologyBlock;

const MOOD: &str = r#"
[[types]]
name = "character"
kind = "entity"
identity_criterion = "the same person"

[[types]]
name = "mood"
kind = "state"
of = "character"
"#;

/// A `mood` statement in `section` about `holder`, read as `value` (none: not read).
fn mood(id: &str, section: &str, holder: &str, value: Option<&str>) -> Claim {
    let mut attributes = serde_json::Map::new();
    if let Some(v) = value {
        attributes.insert("mood".into(), json!(v));
    }
    serde_json::from_value(json!({
        "id": id, "content": format!("{holder} feels it"), "discourse_act": "assert",
        "epistemic_status": "attributed", "scope": "universal", "subject": holder,
        "evidence": [{"chunk_id": section, "passage_preview": "feels it"}],
        "claim_kind": "mood", "attributes": attributes, "enrichment_depth": "extracted",
    }))
    .unwrap()
}

fn section(id: &str) -> SectionExtraction {
    SectionExtraction {
        section_id: id.into(),
        ..Default::default()
    }
}

/// Each read state statement is a State atom of its holder, in its own
/// section; the holder's trajectory chains them in section order, the same
/// value stated twice is two states, and a statement with no value read
/// makes none and is counted.
#[test]
fn read_state_statements_are_states_and_chain_into_the_holders_trajectory() {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(MOOD).unwrap(),
    }
    .policies()
    .unwrap();
    let mut claims = vec![
        mood("c3", "s3", "entity-tabb", Some("fear")),
        mood("c1", "s1", "entity-tabb", Some("fear")),
        mood("c2", "s2", "entity-tabb", Some("grief")),
        mood("c4", "s2", "entity-tabb", None),
    ];
    let (mut entities, mut events, mut states, mut relations) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut ars, mut positions, mut oppositions) = (Vec::new(), Vec::new(), Vec::new());
    // Ids with a gap, as RESOLVE's retire leaves them: `len() + 1` is live.
    let mut edges: Vec<Edge> = [1, 3]
        .map(|n| Edge {
            id: EdgeId::new(n),
            edge_type: EdgeType::Involves,
            source: AtomId::from_raw("entity-x"),
            target: AtomId::from_raw("entity-y"),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        })
        .into();
    let mut trajectories = std::collections::BTreeMap::new();
    let mut atoms = BuildAtoms {
        entities: &mut entities,
        events: &mut events,
        states: &mut states,
        relations: &mut relations,
        claims: &mut claims,
        argument_reconstructions: &mut ars,
        positions: &mut positions,
        oppositions: &mut oppositions,
        edges: &mut edges,
        trajectories: &mut trajectories,
    };
    let sections = [section("s1"), section("s2"), section("s3")];
    let made = project(&mut atoms, &p, &sections, "c");
    assert_eq!((made.states, made.relations), (3, 0), "{made:?}");
    assert_eq!(made.skipped.get("no state value read"), Some(&1));

    let labels: Vec<(&str, &str)> = states
        .iter()
        .map(|s| (s.section_range.start.as_str(), s.label.as_str()))
        .collect();
    assert_eq!(labels.len(), 3);
    assert_ne!(states[0].id, states[1].id, "one atom per statement");
    let trajectory = &trajectories["entity-tabb"];
    let chained: Vec<&str> = trajectory.states.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(chained, ["fear", "grief", "fear"], "in section order");
    assert_eq!(trajectory.transitions.len(), 2);
    let count = |t: EdgeType| edges.iter().filter(|e| e.edge_type == t).count();
    assert_eq!(count(EdgeType::Transition), 2);
    assert_eq!(count(EdgeType::Involves), 3 + 2);
    let ids: std::collections::BTreeSet<&str> = edges.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids.len(), edges.len(), "no edge id is reused");
    assert_eq!(count(EdgeType::Grounds), 3);
}
