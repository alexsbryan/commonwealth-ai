// SPDX-License-Identifier: AGPL-3.0-or-later
//! A type whose source names only who took part (`decides_mentions`): its
//! source's records are seeded under their own ids, its mentions are
//! statements RESOLVE decides, each its words on its line.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::enrichment::pipeline::document_read::WORDS_ATTRIBUTE;
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

const DECLARATION: &str = r#"
[[types]]
name = "company"
kind = "entity"
identity_criterion = "the same organisation, however it is named"
attributes = [{ name = "domain", type = "text" }]
identity = ["domain"]
source = { metadata = ["from", "to"], attributes = { domain = "domain" } }

[[types]]
name = "deal"
kind = "entity"
identity_criterion = "the same transaction"
attributes = [{ name = "counterparty", type = "ref", of = "company", by = "most" }]

[[types]]
name = "stage"
kind = "claim"
force = "assertive"
subject = "deal"
"#;

fn policies(declaration: &str) -> OntologyPolicies {
    OntologyBlock {
        version: 1,
        body: toml::from_str(declaration).unwrap(),
    }
    .policies()
    .unwrap()
}

#[test]
fn only_a_criterion_type_with_a_metadata_source_has_its_mentions_decided() {
    let p = policies(DECLARATION);
    let index = TypeIndex::from_policies(&p);
    assert!(decides_mentions(&index, "company"));
    assert!(!decides(&index, "company"), "its records are its source's");
    assert!(
        !decides_mentions(&index, "deal"),
        "no source: RESOLVE decides it whole"
    );
    assert!(!decides_mentions(&index, "stage"), "a claim kind");
    let bare = policies(&DECLARATION.replace(
        "identity_criterion = \"the same organisation, however it is named\"\n",
        "",
    ));
    assert!(
        !decides_mentions(&TypeIndex::from_policies(&bare), "company"),
        "no criterion: nothing to decide a mention by"
    );
}

const LINE: &str = "PG&E agreed to the April price, and so did Tucson Electric.";
const ALONE: &str = "Tucson Electric asked for a quote.";

fn documents() -> SectionDocuments {
    let row = |i: u64, meta: serde_json::Value, body: &str| EnrichmentChunkRow {
        id: i,
        content: body.into(),
        title: None,
        url: None,
        metadata_raw: Some(meta.to_string()),
        source_doc_id: Some(format!("m{i}")),
    };
    let rows = [
        row(
            1,
            json!({"date": "2001-01-01T10:00:00Z", "from": "kim@enron.com", "to": "carl@pge.com"}),
            &format!("Carl,\n{LINE}\nKim"),
        ),
        row(2, json!({"date": "2001-01-02T10:00:00Z"}), ALONE),
    ];
    SectionDocuments::from_chunk_rows([("s1", &[1u64, 2][..])], &rows)
}

/// A mention particular as the projection hands it to RESOLVE.
fn mention(id: &str, doc: &str, line: &str, words: &str, n: usize) -> Claim {
    let mut attributes = Map::new();
    attributes.insert(ABOUT_ATTRIBUTE.into(), json!("company"));
    attributes.insert(LOCAL_REF_ATTRIBUTE.into(), json!(format!("l{n}:{words}")));
    attributes.insert(SOURCE_DOCUMENT_ATTRIBUTE.into(), json!(doc));
    attributes.insert(SUBJECT_FIELDS_ATTRIBUTE.into(), json!({}));
    attributes.insert(WORDS_ATTRIBUTE.into(), json!(words));
    serde_json::from_value(json!({
        "id": id, "content": line, "discourse_act": "assert",
        "epistemic_status": "attributed", "scope": "universal", "subject": null,
        "evidence": [{"chunk_id": "s1", "passage_preview": line}],
        "anchor": line, "attributes": attributes, "enrichment_depth": "extracted",
    }))
    .unwrap()
}

/// A model that answers RESOLVE right for this fixture: PG&E is the pge.com
/// record; Tucson Electric is the record that holds no domain and was said
/// as Tucson Electric in another message, or none while there is no such
/// record. Any other question is refused. Each question is kept.
fn knowing(asked: Arc<Mutex<Vec<ChatPrompt>>>) -> crate::InferenceFn {
    super::super::resolve_records::one_by_one(Arc::new(move |prompt: &ChatPrompt, _| {
        asked.lock().unwrap().push(prompt.clone());
        let labels: Option<Vec<String>> = prompt
            .response_schema
            .as_ref()
            .and_then(|s| s["enum"].as_array())
            .map(|a| a.iter().map(|l| l.as_str().unwrap().to_string()).collect());
        let answer = match labels {
            Some(labels) => {
                let user = &prompt.user;
                let words = user
                    .split_once("its words in [[ ]]:")
                    .and_then(|(_, rest)| rest.split_once("[["))
                    .and_then(|(_, rest)| rest.split_once("]]"))
                    .map_or("", |(words, _)| words);
                let marks: &[&str] = match words {
                    "PG&E" => &["domain: pge.com"],
                    "Tucson Electric" => &["no declared value", "Tucson Electric"],
                    _ => &[],
                };
                // Each candidate's lines: its label's line and the quotes under it.
                let shown = user
                    .split_once("Candidate records:\n")
                    .and_then(|(_, rest)| rest.split_once("\nStatement, its words"))
                    .map_or("", |(shown, _)| shown);
                let mut blocks: Vec<String> = Vec::new();
                for line in shown.lines() {
                    if labels
                        .get(blocks.len())
                        .is_some_and(|l| line.starts_with(&format!("{l} ")))
                    {
                        blocks.push(String::new());
                    }
                    if let Some(b) = blocks.last_mut() {
                        b.push_str(line);
                    }
                }
                let pick = blocks
                    .iter()
                    .position(|b| !marks.is_empty() && marks.iter().all(|m| b.contains(m)))
                    .unwrap_or(labels.len() - 1);
                let rest = 0.03 / labels.len() as f64;
                let dist: Map<String, Value> = labels
                    .iter()
                    .enumerate()
                    .map(|(i, l)| (l.clone(), json!(if i == pick { 0.97 } else { rest })))
                    .collect();
                Ok(Value::Object(dist).to_string())
            }
            None => Err(crate::Error::Extraction(
                "the scripted answerer answers forced choices only".into(),
            )),
        };
        Box::pin(async move { answer })
    }))
}

/// The source's records are RESOLVE's first records, under the atom ids the
/// source gave them, and each is offered to its own message's mentions as
/// named in a field; two names on one line are two statements, each its own
/// words. Answered right, PG&E is the pge.com record, and Tucson Electric,
/// which no header names, opens a record that holds no domain (its text
/// states none) and is the same record when the next message names it.
#[tokio::test]
async fn a_mention_is_decided_against_the_records_its_message_names() {
    let p = policies(DECLARATION);
    let docs = documents();
    let projection = crate::enrichment::atlas::project_source_atoms(&docs, &p, "c").unwrap();
    let sourced: BTreeSet<String> = projection
        .atoms
        .iter()
        .map(|e| e.id.as_str().to_string())
        .collect();
    assert_eq!(sourced.len(), 2, "enron.com and pge.com");
    let mut entities = projection.atoms.clone();
    let mut claims = vec![
        mention("claim-1", "m1", LINE, "PG&E", 2),
        mention("claim-2", "m1", LINE, "Tucson Electric", 2),
        mention("claim-3", "m2", ALONE, "Tucson Electric", 1),
    ];
    let (mut events, mut states, mut relations) = (Vec::new(), Vec::new(), Vec::new());
    let (mut ars, mut positions, mut oppositions) = (Vec::new(), Vec::new(), Vec::new());
    let (mut edges, mut trajectories) = (Vec::new(), BTreeMap::new());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let infer = knowing(Arc::clone(&asked));
    let (reports, failures) = {
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
        resolve_declared_types(
            &mut atoms,
            &docs,
            &projection.participants,
            &p,
            "c",
            None,
            Answerer::Select(&infer),
            &mut |_, _| {},
        )
        .await
    };
    let report = reports.iter().find(|r| r.type_name == "company").unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(report.seeded, 2, "{}", report.summary());
    assert_eq!(report.retired, 0, "a source's record is never retired");
    for id in &sourced {
        assert!(
            entities.iter().any(|e| e.id.as_str() == id),
            "the source's record {id} is gone"
        );
    }
    let asked = asked.lock().unwrap();
    let m1: Vec<&str> = asked
        .iter()
        .map(|q| q.user.as_str())
        .filter(|u| u.contains("PG&E agreed"))
        .collect();
    assert_eq!(m1.len(), 2, "one question per statement of m1");
    for (q, words) in m1
        .iter()
        .zip(["[[PG&E]] agreed", "so did [[Tucson Electric]]"])
    {
        assert!(q.contains(words), "the statement is its words: {q}");
        assert!(
            q.contains("(named in this document's field `from`)")
                && q.contains("(named in this document's field `to`)"),
            "m1's records are offered to its mentions: {q}"
        );
    }
    let subject = |claim: &str| {
        let c = claims.iter().find(|c| c.id.as_str() == claim).unwrap();
        let id = c.subject.as_ref().expect("every mention reaches a record");
        entities.iter().find(|e| e.id == *id).unwrap()
    };
    for claim in ["claim-1", "claim-2", "claim-3"] {
        assert_eq!(subject(claim).entity_type.as_str_repr(), "company");
    }
    let pge = projection
        .atoms
        .iter()
        .find(|e| e.attributes.get("domain") == Some(&json!("pge.com")))
        .expect("pge.com is a source record");
    assert_eq!(subject("claim-1").id, pge.id, "PG&E is the pge.com record");
    let alone = subject("claim-3");
    assert!(
        !sourced.contains(alone.id.as_str()),
        "Tucson Electric is no header's company"
    );
    assert_eq!(
        alone.id,
        subject("claim-2").id,
        "m2's Tucson Electric is the record m1's opened"
    );
    assert!(
        !alone.attributes.contains_key("domain"),
        "identity its text does not state stays absent: {:?}",
        alone.attributes
    );
}
