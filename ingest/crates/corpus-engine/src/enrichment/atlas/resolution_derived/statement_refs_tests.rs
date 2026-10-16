// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{json, Map, Value};

use super::*;
use crate::enrichment::atlas::resolution_records::resolve_declared_types;
use crate::enrichment::atlas::resolve_records::{Answerer, DocumentResolution};
use crate::enrichment::pipeline::document_read::{
    ABOUT_ATTRIBUTE, CLAIM_FIELDS_ATTRIBUTE, LOCAL_REF_ATTRIBUTE, OTHER_NAMES_ATTRIBUTE,
    SOURCE_DOCUMENT_ATTRIBUTE, SUBJECT_FIELDS_ATTRIBUTE, SUBJECT_NAME_ATTRIBUTE, WORDS_ATTRIBUTE,
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

const LINE: &str = "Ivo trusted Nell with the ledger.";

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

/// A relation's other endpoint is a record: the statement `Ivo trusted Nell`
/// is about Ivo's record, and its `to`, read as the words "Nell", holds the
/// record RESOLVE put the same document's mention of Nell on, never the
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
            "ivo",
            "Ivo",
            json!({ "to": read("Nell") }),
            json!({}),
        ),
        handed(
            "doubt",
            Some("trusts"),
            "ivo",
            "Ivo",
            json!({ "to": read("Grist") }),
            json!({}),
        ),
        handed(
            "nell",
            None,
            "nell",
            "Nell",
            json!({}),
            json!({ ABOUT_ATTRIBUTE: "character", WORDS_ATTRIBUTE: "Nell" }),
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
    let (trust, doubt, nell) = (claim("trust"), claim("doubt"), claim("nell"));
    let record_of = |c: &Claim| c.subject.as_ref().map(|s| s.as_str().to_string());
    assert!(
        record_of(trust).is_some(),
        "the relation is about Ivo's record"
    );
    assert_eq!(record_of(trust), record_of(doubt));
    assert_ne!(record_of(trust), record_of(nell), "Ivo and Nell are two");
    assert_eq!(
        trust.attributes.get("to"),
        record_of(nell).map(Value::String).as_ref(),
        "`to` holds Nell's record, not her name"
    );
    assert_eq!(
        doubt.attributes.get("to"),
        None,
        "no record is mentioned as Grist"
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
        [record_of(trust), record_of(nell)],
        "from Ivo to Nell"
    );
    assert_eq!(relation.label, LINE);
    let line = lines.iter().find(|l| l.atom == "doubt").expect("a line");
    assert_eq!(line.unresolved.len(), 1, "{line:?}");
    assert_eq!(line.unresolved[0].words, json!("Grist"));
    assert_eq!(
        doubt.attributes[CLAIM_FIELDS_ATTRIBUTE]["to"]["value"], "Grist",
        "the reading stays on the statement, cited"
    );
}

const TOLD: &str = "Nell said Ivo crossed the cobbles at six.";
const HEARD: &str = "Grist swore Nell was at the quay.";

const ACCOUNT: &str = r#"
[[types]]
name = "character"
kind = "entity"
identity_criterion = "the same person"
identity = ["name"]
attributes = [{ name = "name", type = "text" }]

[[types]]
name = "account"
kind = "claim"
force = "assertive"
subject = "character"
attributes = [{ name = "speaker", type = "ref", of = "character" }]
"#;

/// A window read names a speaker by the name it read another statement's
/// subject under: `Nell said Ivo crossed the cobbles` holds, as `speaker`,
/// the record RESOLVE put the same document's statement about Nell on, with
/// no mention of her read. A name read nowhere is not held, and says why.
#[tokio::test]
async fn a_window_ref_holds_the_record_its_window_read_under_that_name() {
    let (claims, _, lines) = accounts_derived("Nell", None).await;
    let claim = |id: &str| claims.iter().find(|c| c.id.as_str() == id).unwrap();
    let (told, heard) = (claim("told"), claim("heard"));
    let record_of = |c: &Claim| c.subject.as_ref().map(|s| s.as_str().to_string());
    assert!(
        record_of(heard).is_some(),
        "the account is about Nell's record"
    );
    assert_ne!(record_of(told), record_of(heard), "Ivo and Nell are two");
    assert_eq!(
        told.attributes.get("speaker"),
        record_of(heard).map(Value::String).as_ref(),
        "`speaker` holds Nell's record, not her name"
    );
    assert_eq!(
        heard.attributes.get("speaker"),
        None,
        "no statement is read about Grist"
    );
    let line = lines.iter().find(|l| l.atom == "heard").expect("a line");
    assert_eq!(line.unresolved.len(), 1, "{line:?}");
    assert_eq!(line.unresolved[0].words, json!("Grist"));
}

/// Failing input, named: world-s5's window wrote a counterparty for a thing
/// a document said had a new name ("Pellica Chemical", now "Elk Bluff Cement
/// Company"; arm bundle1-fresh, 2026-10-16), and a reading by one name named
/// nothing the other name was read under. A reading of a thing the window
/// read gave other names (`OTHER_NAMES_ATTRIBUTE`, the answer's typed
/// `other_names`) lands by any of them: `told`'s speaker "Mrs Varden", whose
/// other name is "Nell", holds the record of the statement read about Nell.
#[tokio::test]
async fn a_reading_of_a_thing_with_other_names_lands_by_any() {
    let (claims, _, _) =
        accounts_derived("Mrs Varden", Some(json!({ "Mrs Varden": ["Nell"] }))).await;
    let claim = |id: &str| claims.iter().find(|c| c.id.as_str() == id).unwrap();
    let (told, heard) = (claim("told"), claim("heard"));
    assert_eq!(
        told.attributes.get("speaker"),
        heard
            .subject
            .as_ref()
            .map(|s| Value::String(s.as_str().to_string()))
            .as_ref(),
        "`speaker` holds Nell's record by her second name"
    );
}

/// Failing input, named: saltgrass's records were called by their opening
/// passages, so a resolved reference printed a passage where a name belonged
/// (arm bundle1-fresh-sg: records 27.6k -> 50.7k chars, `speaker` alone 10k).
/// A record RESOLVE builds is called by the name its window read gave the
/// statement that opened it: Ivo's record is "Ivo", Nell's "Nell".
#[tokio::test]
async fn a_record_is_called_by_the_name_its_reader_gave_it() {
    let (claims, entities, _) = accounts_derived("Nell", None).await;
    let name_of = |id: &str| {
        let record = claims
            .iter()
            .find(|c| c.id.as_str() == id)
            .unwrap()
            .subject
            .clone();
        entities
            .iter()
            .find(|e| Some(&e.id) == record.as_ref())
            .map(|e| e.canonical_name.clone())
    };
    assert_eq!(name_of("told").as_deref(), Some("Ivo"));
    assert_eq!(name_of("heard").as_deref(), Some("Nell"));
    // Its opening passage stays on it as its first passage (the record view's
    // text), not as its name: what a reader first read the record from is not
    // dropped when a name replaces the passage (saltgrass bank 31 -> 27 on
    // arm guard-replay-sg, 2026-10-16).
    let ivo = entities
        .iter()
        .find(|e| e.canonical_name == "Ivo")
        .expect("Ivo's record");
    assert!(
        ivo.description.contains("crossed the cobbles"),
        "{:?}",
        ivo.description
    );
}

/// The two accounts of `TOLD` and `HEARD`, resolved and derived, `told`'s
/// speaker read as `told_speaker`: the claims, the records and the derivation
/// lines.
async fn accounts_derived(
    told_speaker: &str,
    told_others: Option<Value>,
) -> (Vec<Claim>, Vec<Entity>, Vec<DerivedValue>) {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(ACCOUNT).unwrap(),
    }
    .policies()
    .unwrap();
    let rows = [EnrichmentChunkRow {
        id: 1,
        content: format!("{TOLD} {HEARD}"),
        title: None,
        url: None,
        metadata_raw: Some(json!({ "id": "ch1" }).to_string()),
        source_doc_id: Some("ch1".into()),
    }];
    let docs = SectionDocuments::from_chunk_rows([("s1", &[1u64][..])], &rows);
    let mut claims = vec![
        handed(
            "told",
            Some("account"),
            "ivo",
            "Ivo",
            json!({ "speaker": read(told_speaker) }),
            match told_others {
                Some(others) => {
                    json!({ SUBJECT_NAME_ATTRIBUTE: "Ivo", OTHER_NAMES_ATTRIBUTE: others })
                }
                None => json!({ SUBJECT_NAME_ATTRIBUTE: "Ivo" }),
            },
        ),
        handed(
            "heard",
            Some("account"),
            "nell",
            "Nell",
            json!({ "speaker": read("Grist") }),
            json!({ SUBJECT_NAME_ATTRIBUTE: "Nell" }),
        ),
    ];
    for (claim, line) in claims.iter_mut().zip([TOLD, HEARD]) {
        claim.content = line.into();
        claim.anchor = Some(line.into());
        claim.evidence[0].passage_preview = Some(line.into());
    }
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
        "accounts",
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
    (claims, entities, lines)
}
