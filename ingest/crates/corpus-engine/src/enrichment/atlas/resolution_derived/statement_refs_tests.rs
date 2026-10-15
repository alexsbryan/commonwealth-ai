// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{json, Map, Value};

use super::*;
use crate::enrichment::atlas::resolution_records::resolve_declared_types;
use crate::enrichment::atlas::resolve_records::{Answerer, DocumentResolution};
use crate::enrichment::pipeline::document_read::{
    ABOUT_ATTRIBUTE, CLAIM_FIELDS_ATTRIBUTE, LOCAL_REF_ATTRIBUTE, SOURCE_DOCUMENT_ATTRIBUTE,
    SUBJECT_FIELDS_ATTRIBUTE, WORDS_ATTRIBUTE,
};
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

const RELATION: &str = r#"
[[types]]
name = "character"
kind = "entity"
identity_criterion = "the same person"
identity = ["name"]
attributes = [{ name = "name", type = "text" }]

[[types]]
name = "trusts"
kind = "relation"
from = "character"
to = "character"
"#;

const LINE: &str = "Tabb trusted Maren with the ledger.";

fn read(value: &str) -> Value {
    json!({"status": "supported", "value": value, "evidence": value})
}

/// A statement as the document reader's projection hands it to RESOLVE:
/// `kind` (none for a mention particular), its holder read as `name`, its own
/// fields `own` and the extra handoff attributes `more`.
fn handed(id: &str, kind: Option<&str>, local: &str, name: &str, own: Value, more: Value) -> Claim {
    let mut attributes: Map<String, Value> = more.as_object().cloned().unwrap();
    for (field, reading) in own.as_object().unwrap() {
        attributes.insert(field.clone(), reading["value"].clone());
    }
    attributes.insert(CLAIM_FIELDS_ATTRIBUTE.into(), own);
    attributes.insert(LOCAL_REF_ATTRIBUTE.into(), json!(local));
    attributes.insert(SOURCE_DOCUMENT_ATTRIBUTE.into(), json!("ch1"));
    attributes.insert(
        SUBJECT_FIELDS_ATTRIBUTE.into(),
        json!({ "name": read(name) }),
    );
    serde_json::from_value(json!({
        "id": id, "content": LINE, "discourse_act": "assert",
        "epistemic_status": "attributed", "scope": "universal", "subject": null,
        "evidence": [{"chunk_id": "s1", "passage_preview": LINE}],
        "claim_kind": kind, "anchor": LINE, "attributes": attributes,
        "enrichment_depth": "extracted",
    }))
    .unwrap()
}

/// A relation's other endpoint is a record: the statement `Tabb trusted Maren`
/// is about Tabb's record, and its `to`, read as the words "Maren", holds the
/// record RESOLVE put the same document's mention of Maren on, never the
/// words. Words that name no record there are not held, and say why.
#[tokio::test]
async fn a_relation_statement_holds_its_other_endpoint_as_a_record() {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(RELATION).unwrap(),
    }
    .policies()
    .unwrap();
    let rows = [EnrichmentChunkRow {
        id: 1,
        content: LINE.into(),
        title: None,
        url: None,
        metadata_raw: Some(json!({ "id": "ch1" }).to_string()),
        source_doc_id: Some("ch1".into()),
    }];
    let docs = SectionDocuments::from_chunk_rows([("s1", &[1u64][..])], &rows);
    let mut claims = vec![
        handed(
            "trust",
            Some("trusts"),
            "tabb",
            "Tabb",
            json!({ "to": read("Maren") }),
            json!({}),
        ),
        handed(
            "doubt",
            Some("trusts"),
            "tabb",
            "Tabb",
            json!({ "to": read("Pellow") }),
            json!({}),
        ),
        handed(
            "maren",
            None,
            "maren",
            "Maren",
            json!({}),
            json!({ ABOUT_ATTRIBUTE: "character", WORDS_ATTRIBUTE: "Maren" }),
        ),
    ];
    let mut entities = Vec::new();
    let mut events = Vec::new();
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
    let participants = Participants::default();
    let mut on_document = |_: &str, _: &DocumentResolution| {};
    let (reports, failures) = resolve_declared_types(
        &mut atoms,
        &docs,
        &participants,
        &p,
        "relations",
        None,
        Answerer::Proposed,
        &mut on_document,
    )
    .await;
    assert_eq!(reports[0].records, 2, "{reports:?} {failures:?}");
    let mut lines = Vec::new();
    derive_attributes(
        &mut atoms,
        &docs,
        &participants,
        &p,
        DeriveStage::AfterResolve,
        &mut |v| lines.push(v.clone()),
    )
    .unwrap();
    let sections = [crate::enrichment::pipeline::atlas::SectionExtraction {
        section_id: "s1".into(),
        ..Default::default()
    }];
    let projected = crate::enrichment::atlas::resolution_records::statement_atoms::project(
        &mut atoms,
        &p,
        &sections,
        "relations",
    );
    assert_eq!(projected.relations, 1, "{projected:?}");
    assert_eq!(projected.skipped.get("no `to` record"), Some(&1));

    let claim = |id: &str| claims.iter().find(|c| c.id.as_str() == id).unwrap();
    let (trust, doubt, maren) = (claim("trust"), claim("doubt"), claim("maren"));
    let record_of = |c: &Claim| c.subject.as_ref().map(|s| s.as_str().to_string());
    assert!(
        record_of(trust).is_some(),
        "the relation is about Tabb's record"
    );
    assert_eq!(record_of(trust), record_of(doubt));
    assert_ne!(record_of(trust), record_of(maren), "Tabb and Maren are two");
    assert_eq!(
        trust.attributes.get("to"),
        record_of(maren).map(Value::String).as_ref(),
        "`to` holds Maren's record, not her name"
    );
    assert_eq!(
        doubt.attributes.get("to"),
        None,
        "no record is mentioned as Pellow"
    );
    let [relation] = &relations[..] else {
        panic!("one relation: {relations:?}");
    };
    let ends: Vec<Option<String>> = relation
        .participants
        .iter()
        .map(|a| Some(a.as_str().to_string()))
        .collect();
    assert_eq!(
        ends,
        [record_of(trust), record_of(maren)],
        "from Tabb to Maren"
    );
    assert_eq!(relation.label, LINE);
    let line = lines.iter().find(|l| l.atom == "doubt").expect("a line");
    assert_eq!(line.unresolved.len(), 1, "{line:?}");
    assert_eq!(line.unresolved[0].words, json!("Pellow"));
    assert_eq!(
        doubt.attributes[CLAIM_FIELDS_ATTRIBUTE]["to"]["value"], "Pellow",
        "the reading stays on the statement, cited"
    );
}
