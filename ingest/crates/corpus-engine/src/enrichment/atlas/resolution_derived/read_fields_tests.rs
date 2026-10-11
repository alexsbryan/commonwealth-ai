// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{json, Value};

use super::super::*;
use crate::enrichment::atlas::atoms::Event;
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

const DECLARATION: &str = r#"
[[types]]
name = "happening"
kind = "event"
identity_criterion = "the same occurrence"
attributes = [
  { name = "kind", type = "text", values = ["fire", "flood"], by = "agree" },
  { name = "size", type = "quantity", by = "latest" },
  { name = "note", type = "text" },
]

[[types]]
name = "report"
kind = "claim"
force = "assertive"
subject = "happening"

[change]
document = { date = "date" }
"#;

fn documents() -> SectionDocuments {
    let rows: Vec<EnrichmentChunkRow> = (1..=4u64)
        .map(|i| EnrichmentChunkRow {
            id: i,
            content: format!("Report {i}."),
            title: None,
            url: None,
            metadata_raw: Some(json!({"date": format!("2001-01-0{i}T10:00:00Z")}).to_string()),
            source_doc_id: Some(format!("m{i}")),
        })
        .collect();
    SectionDocuments::from_chunk_rows([("s1", &[1u64, 2, 3, 4][..])], &rows)
}

fn happening(id: &str) -> Event {
    serde_json::from_value(json!({
        "id": id, "description": id, "event_type": "happening", "evidence": [],
        "section_position": {"section_id": "s1"}, "enrichment_depth": "extracted",
    }))
    .unwrap()
}

/// A statement from document `doc` about `subject`, reading its subject's
/// fields as `fields` gives them (value, or null for an unknown).
fn report(doc: &str, subject: &str, fields: Value) -> Claim {
    let readings: serde_json::Map<String, Value> = fields
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| {
            let reading = match value {
                Value::Null => json!({"status": "unknown", "reason": "not stated"}),
                v => json!({"status": "supported", "value": v, "evidence": "Report"}),
            };
            (name.clone(), reading)
        })
        .collect();
    serde_json::from_value(json!({
        "id": format!("claim-{doc}"), "content": "Report", "discourse_act": "assert",
        "epistemic_status": "attributed", "scope": "universal", "subject": subject,
        "evidence": [{"chunk_id": "s1", "passage_preview": "Report", "source_doc_id": doc}],
        "claim_kind": "report", "enrichment_depth": "extracted",
        "attributes": {"__document_read_subject_fields": readings},
    }))
    .unwrap()
}

/// A record holds its statements' one reading when they agree, none when they
/// differ (said as a conflict), the latest document's reading under `latest`,
/// and nothing of a field no `by` folds; each outcome is a line, cited by its
/// documents, and only after RESOLVE.
#[test]
fn a_record_folds_what_its_statements_read_by_the_declared_policy() {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(DECLARATION).unwrap(),
    }
    .policies()
    .unwrap();
    let docs = documents();
    let mut entities = Vec::new();
    let mut events = vec![happening("event-h1"), happening("event-h2")];
    let mut claims = vec![
        report(
            "m1",
            "event-h1",
            json!({"kind": "fire", "size": 10, "note": "a"}),
        ),
        report(
            "m2",
            "event-h1",
            json!({"kind": "fire", "size": 12, "note": "b"}),
        ),
        report("m3", "event-h2", json!({"kind": "fire", "size": null})),
        report("m4", "event-h2", json!({"kind": "flood", "size": null})),
    ];
    let (mut states, mut relations, mut ars) = (Vec::new(), Vec::new(), Vec::new());
    let (mut positions, mut oppositions, mut edges) = (Vec::new(), Vec::new(), Vec::new());
    let mut trajectories = BTreeMap::new();
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
    let participants = Participants::new();
    let mut lines = Vec::new();
    derive_attributes(
        &mut atoms,
        &docs,
        &participants,
        &p,
        DeriveStage::BeforeResolve,
        &mut |v| lines.push(v.clone()),
    )
    .unwrap();
    assert!(
        lines.is_empty(),
        "a record's fields wait for RESOLVE: {lines:?}"
    );
    derive_attributes(
        &mut atoms,
        &docs,
        &participants,
        &p,
        DeriveStage::AfterResolve,
        &mut |v| lines.push(v.clone()),
    )
    .unwrap();

    let attrs = |id: &str| {
        events
            .iter()
            .find(|e| e.id.as_str() == id)
            .unwrap()
            .attributes
            .clone()
    };
    assert_eq!(attrs("event-h1").get("kind"), Some(&json!("fire")));
    assert_eq!(
        attrs("event-h1").get("size"),
        Some(&json!(12)),
        "the latest document's reading"
    );
    assert_eq!(
        attrs("event-h1").get("note"),
        None,
        "no `by`, no record value"
    );
    assert_eq!(attrs("event-h2").get("kind"), None, "the statements differ");
    let line = |atom: &str, attr: &str| {
        lines
            .iter()
            .find(|l| l.atom == atom && l.attribute == attr)
            .unwrap_or_else(|| panic!("no line for {atom}.{attr}"))
    };
    assert!(matches!(
        &line("event-h1", "kind").outcome,
        DerivedOutcome::Decided { documents, .. } if documents == &["m1", "m2"]
    ));
    assert!(matches!(
        &line("event-h2", "kind").outcome,
        DerivedOutcome::Conflict { values } if values == &["fire", "flood"]
    ));
    assert_eq!(line("event-h2", "size").outcome, DerivedOutcome::Nothing);
    assert_eq!(line("event-h1", "kind").derived, "by agree");
}
