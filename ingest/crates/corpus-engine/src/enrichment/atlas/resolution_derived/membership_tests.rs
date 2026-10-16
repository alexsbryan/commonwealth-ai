// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;
use crate::enrichment::atlas::atoms::{AtomId, Claim, Entity};
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

const DECLARATION: &str = r#"
[[types]]
name = "deal"
kind = "entity"
identity_criterion = "the same transaction"

[[types]]
name = "stage_update"
kind = "claim"
force = "assertive"
subject = "deal"

[change]
document = { thread = "thread", id = "id" }
"#;

fn policies(declaration: &str) -> OntologyPolicies {
    OntologyBlock {
        version: 1,
        body: toml::from_str(declaration).unwrap(),
    }
    .policies()
    .unwrap()
}

/// One document per section, so every claim in a section is in its document.
/// `(key, thread, id)`; an empty thread or id leaves that field off.
fn documents(docs: &[(&str, &str, &str)]) -> SectionDocuments {
    let rows: Vec<EnrichmentChunkRow> = docs
        .iter()
        .enumerate()
        .map(|(i, (key, thread, id))| {
            let mut fields = serde_json::Map::new();
            if !thread.is_empty() {
                fields.insert("thread".into(), json!(thread));
            }
            if !id.is_empty() {
                fields.insert("id".into(), json!(id));
            }
            EnrichmentChunkRow {
                id: i as u64,
                content: format!("the words of {key}"),
                title: None,
                url: None,
                metadata_raw: Some(Value::Object(fields).to_string()),
                source_doc_id: Some((*key).into()),
            }
        })
        .collect();
    let ids: Vec<[u64; 1]> = (0..docs.len() as u64).map(|i| [i]).collect();
    SectionDocuments::from_chunk_rows(
        docs.iter()
            .zip(&ids)
            .map(|((key, _, _), id)| (*key, &id[..])),
        &rows,
    )
}

fn statement(id: &str, document: &str, on: Option<&str>) -> Claim {
    let mut c: Claim = serde_json::from_value(json!({
        "id": id, "content": "a stage", "discourse_act": "assert", "epistemic_status": "confident",
        "scope": "universal", "evidence": [{"chunk_id": document, "passage_preview": "the words"}],
        "anchor": "the words", "claim_kind": "stage_update", "enrichment_depth": "extracted",
    }))
    .unwrap();
    c.subject = on.map(AtomId::from_raw);
    c
}

fn deal(id: &str) -> Entity {
    serde_json::from_value(json!({
        "id": id, "canonical_name": id, "entity_type": "deal", "first_appearance": {"chunk_id": "a1"},
        "description": "", "salience": 0.0, "enrichment_depth": "extracted",
    }))
    .unwrap()
}

struct Run {
    entities: Vec<Entity>,
    lines: Vec<DerivedValue>,
    report: DerivedReport,
}

fn place(
    p: &OntologyPolicies,
    docs: &SectionDocuments,
    read: &BTreeMap<String, DocumentReadStatus>,
    mut entities: Vec<Entity>,
    mut claims: Vec<Claim>,
) -> Run {
    let (mut events, mut states, mut relations) = (Vec::new(), Vec::new(), Vec::new());
    let (mut ars, mut positions, mut oppositions, mut edges) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
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
    let mut lines = Vec::new();
    let report = place_thread_documents(&mut atoms, docs, read, p, &mut |v| lines.push(v.clone()));
    Run {
        entities,
        lines,
        report,
    }
}

/// Thread A: a1 states d1, a2 and a3 state nothing, a5 lost its id, j1 the
/// reader could not judge, a4 was not read. Thread B: b1 states d1, b2 d2,
/// b3 nothing. Thread C: c1 alone, stating nothing. Thread E: e1's statement
/// is on no record, e2 states nothing. n1 names no thread.
fn corpus() -> (
    SectionDocuments,
    BTreeMap<String, DocumentReadStatus>,
    Vec<Entity>,
    Vec<Claim>,
) {
    let docs = documents(&[
        ("a1", "A", "id-a1"),
        ("a2", "A", "id-a2"),
        ("a3", "A", "id-a3"),
        ("a4", "A", "id-a4"),
        ("a5", "A", ""),
        ("j1", "A", "id-j1"),
        ("b1", "B", "id-b1"),
        ("b2", "B", "id-b2"),
        ("b3", "B", "id-b3"),
        ("c1", "C", "id-c1"),
        ("e1", "E", "id-e1"),
        ("e2", "E", "id-e2"),
        ("n1", "", "id-n1"),
    ]);
    let read = [
        "a1", "a2", "a3", "a5", "j1", "b1", "b2", "b3", "c1", "e1", "e2", "n1",
    ]
    .into_iter()
    .map(|k| {
        let status = match k {
            "j1" => DocumentReadStatus::CouldNotJudge,
            "a1" | "b1" | "b2" | "e1" => DocumentReadStatus::Read,
            _ => DocumentReadStatus::NothingApplicable,
        };
        (k.to_string(), status)
    })
    .collect();
    let claims = vec![
        statement("claim-0001", "a1", Some("entity-d1")),
        statement("claim-0002", "b1", Some("entity-d1")),
        statement("claim-0003", "b2", Some("entity-d2")),
        statement("claim-0004", "e1", None),
    ];
    (
        docs,
        read,
        vec![deal("entity-d1"), deal("entity-d2")],
        claims,
    )
}

fn held(run: &Run, record: &str) -> Option<Value> {
    run.entities
        .iter()
        .find(|e| e.id.as_str() == record)
        .unwrap()
        .attributes
        .get(THREAD_DOCUMENTS_ATTR)
        .cloned()
}

#[test]
fn a_document_that_states_nothing_joins_the_one_record_its_thread_is_on() {
    let p = policies(DECLARATION);
    let (docs, read, entities, claims) = corpus();
    let run = place(&p, &docs, &read, entities, claims);

    assert_eq!(held(&run, "entity-d1"), Some(json!(["id-a2", "id-a3"])));
    assert_eq!(held(&run, "entity-d2"), None, "thread B is on two records");
    let line = run.lines.iter().find(|l| l.atom == "entity-d1").unwrap();
    match &line.outcome {
        DerivedOutcome::Decided {
            values, documents, ..
        } => {
            assert_eq!(values, &["id-a2", "id-a3"]);
            assert_eq!(documents, &["a1"], "the statement that put thread A on d1");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        (line.attribute.as_str(), line.by.source.as_str()),
        (THREAD_DOCUMENTS_ATTR, "derived:document_thread")
    );
    assert_eq!(line.by.precision, Precision::Unmeasured);
    let d2 = run.lines.iter().find(|l| l.atom == "entity-d2").unwrap();
    assert_eq!(d2.outcome, DerivedOutcome::Nothing);

    let t = &run.report["deal.thread_documents"];
    assert_eq!(
        (t.atoms, t.outcomes["decided"], t.outcomes["nothing"]),
        (2, 1, 1)
    );
    let counted: BTreeMap<&str, usize> = t.documents.iter().map(|(k, n)| (*k, *n)).collect();
    assert_eq!(
        counted,
        BTreeMap::from([
            ("by_statement", 3),
            ("by_thread", 2),
            ("no_id", 1),
            ("no_thread", 1),
            ("not_judged", 1),
            ("statement_unplaced", 1),
            ("thread_on_no_record", 2),
            ("thread_on_several_records", 1),
        ]),
        "every document read is counted once, and a4, unread, not at all"
    );
}

#[test]
fn the_documents_a_record_holds_do_not_depend_on_their_order() {
    let p = policies(DECLARATION);
    let (docs, read, entities, claims) = corpus();
    let base = place(&p, &docs, &read, entities.clone(), claims.clone());
    let (mut entities, mut claims) = (entities, claims);
    entities.reverse();
    claims.reverse();
    let reversed = place(&p, &docs, &read, entities, claims);
    for record in ["entity-d1", "entity-d2"] {
        assert_eq!(held(&reversed, record), held(&base, record), "{record}");
    }
}

#[test]
fn placing_twice_holds_the_same_documents_and_replaces_nothing() {
    let p = policies(DECLARATION);
    let (docs, read, entities, claims) = corpus();
    let first = place(&p, &docs, &read, entities, claims.clone());
    let again = place(&p, &docs, &read, first.entities.clone(), claims);
    assert_eq!(held(&again, "entity-d1"), held(&first, "entity-d1"));
    assert!(
        again.lines.iter().all(|l| l.replaced.is_none()),
        "{:?}",
        again.lines
    );
}

#[test]
fn a_thread_places_only_when_declared_with_an_id_and_a_type_resolve_decides() {
    let with = |document: &str| {
        DECLARATION.replace(r#"document = { thread = "thread", id = "id" }"#, document)
    };
    assert!(applies(&policies(DECLARATION)));
    let no_id = policies(&with(r#"document = { thread = "thread" }"#));
    let no_thread = policies(&with(r#"document = { id = "id" }"#));
    assert!(!applies(&no_id));
    assert!(!applies(&no_thread));
    let undecided = policies(&DECLARATION.replace(
        r#"identity_criterion = "the same transaction""#,
        r#"identity = ["name"]
attributes = [{ name = "name", type = "text" }]"#,
    ));
    assert!(!applies(&undecided));

    let (docs, read, entities, claims) = corpus();
    let run = place(&no_id, &docs, &read, entities, claims);
    assert!(run.report.is_empty() && run.lines.is_empty());
    assert_eq!(held(&run, "entity-d1"), None);
}
