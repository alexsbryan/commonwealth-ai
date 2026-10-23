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
/// and a field declared without `by` (`note`) by the default `agree`; each outcome is a line, cited by its
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
        "no `by`: folded by agree, and its readings differ"
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
    assert!(matches!(
        &line("event-h1", "note").outcome,
        DerivedOutcome::Conflict { values } if values == &["a", "b"]
    ));
}

/// Failing input, named: the census gap `subject-field-unheld` (03-policy
/// n4 `clause.policy`, 07-inventory n1 `part.part_number`): a decided type's
/// read field declared without `by` stayed on each statement and never
/// reached the record. Folded by the default `agree`, a record whose two
/// statements read it alike holds it; one whose statements read it two ways
/// holds none and shows the conflict, each reading cited.
#[test]
fn a_field_declared_without_by_folds_by_agree() {
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
        report("m1", "event-h1", json!({"note": "PN-7"})),
        report("m2", "event-h1", json!({"note": "PN-7"})),
        report("m3", "event-h2", json!({"note": "PN-7"})),
        report("m4", "event-h2", json!({"note": "PN-9"})),
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
    derive_attributes(
        &mut atoms,
        &docs,
        &Participants::new(),
        &p,
        DeriveStage::AfterResolve,
        &mut |_| {},
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
    assert_eq!(
        attrs("event-h1").get("note"),
        Some(&json!("PN-7")),
        "read alike, held"
    );
    let h2 = attrs("event-h2");
    assert_eq!(h2.get("note"), None, "read two ways, no value");
    let fold = &h2[super::super::outcome::FOLDS_ATTRIBUTE]["note"];
    assert_eq!(fold["outcome"], json!("conflict"), "{fold}");
    let cited: Vec<(&Value, &Value)> = fold["readings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (&r["value"], &r["document"]))
        .collect();
    assert_eq!(
        cited,
        [
            (&json!("PN-7"), &json!("m3")),
            (&json!("PN-9"), &json!("m4"))
        ],
        "{fold}"
    );
}

const REFERENCES: &str = r#"
[[types]]
name = "company"
kind = "entity"
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

[change]
document = { date = "date" }

[[sets]]
id = "ours"
type = "company"
where = { domain = { suffix = "enron.com" } }

[[paths]]
id = "outside"
path = "document / (from | to) [!ours]"
"#;

/// A statement `id` in document `doc` about `deal`, reading its counterparty as
/// `read` (what a Pick gave: a listed company's identity value, or a mention).
fn stage(id: &str, doc: &str, deal: &str, read: &str) -> Claim {
    serde_json::from_value(json!({
        "id": id, "content": "a stage", "discourse_act": "assert",
        "epistemic_status": "attributed", "scope": "universal", "subject": deal,
        "evidence": [{"chunk_id": "s1", "passage_preview": "Report", "source_doc_id": doc}],
        "claim_kind": "stage", "enrichment_depth": "extracted",
        "attributes": {"__document_read_subject_fields": {"counterparty":
            {"status": "supported", "value": read, "evidence": read}}},
    }))
    .unwrap()
}

fn deal(id: &str) -> Entity {
    serde_json::from_value(json!({
        "id": id, "canonical_name": id, "entity_type": "deal", "first_appearance": {"chunk_id": "s1"},
        "description": "", "salience": 0.0, "enrichment_depth": "extracted",
    }))
    .unwrap()
}

/// A reference holds a record or nothing: a reading that folds equal to a
/// company the statement's own message names in its fields counts as that
/// company's record, never its words, and is dropped when the company is in
/// the exclusion set `ours`; a company only another message names is held by
/// its exact identity value (`sourced_anywhere`, 2026-10-20: before, it was
/// never held); a mention is never held and stays on the derivation line,
/// cited.
#[test]
fn a_record_holds_the_record_its_statements_pick_or_nothing() {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(REFERENCES).unwrap(),
    }
    .policies()
    .unwrap();
    let row = |i: u64, from: &str, to: &str| EnrichmentChunkRow {
        id: i,
        content: "Report.".into(),
        title: None,
        url: None,
        metadata_raw: Some(
            json!({"date": format!("2001-01-0{i}T10:00:00Z"), "from": from, "to": to}).to_string(),
        ),
        source_doc_id: Some(format!("m{i}")),
    };
    let rows = [
        row(1, "Ann Lee <ann@city.gov>", "kim@enron.com"),
        row(2, "kim@enron.com", "Ann Lee <ann@city.gov>"),
        row(3, "kim@enron.com", "carl@pge.com, dana@sdge.com"),
        row(4, "kim@enron.com", "ed@enron.com"),
    ];
    let docs = SectionDocuments::from_chunk_rows([("s1", &[1u64, 2, 3, 4][..])], &rows);
    let projection = crate::enrichment::atlas::project_source_atoms(&docs, &p, "c").unwrap();
    let company = |domain: &str| {
        projection
            .atoms
            .iter()
            .find(|e| e.attributes.get("domain").and_then(Value::as_str) == Some(domain))
            .unwrap_or_else(|| panic!("no company {domain}"))
            .id
            .as_str()
            .to_string()
    };
    let (city, pge) = (company("city.gov"), company("pge.com"));
    let mut entities = projection.atoms.clone();
    let enron = company("enron.com");
    entities.extend(["entity-d1", "entity-d2", "entity-d3", "entity-d4"].map(deal));
    let mut claims = vec![
        stage("claim-1", "m1", "entity-d1", "city.gov"),
        stage("claim-2", "m2", "entity-d1", "city.gov"),
        stage("claim-3", "m3", "entity-d1", "Tucson"),
        stage("claim-4", "m3", "entity-d1", "Tucson"),
        stage("claim-5", "m3", "entity-d2", "pge.com"),
        stage("claim-6", "m4", "entity-d3", "pge.com"),
        stage("claim-7", "m2", "entity-d4", "enron.com"),
    ];
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
    let mut lines = Vec::new();
    derive_attributes(
        &mut atoms,
        &docs,
        &projection.participants,
        &p,
        DeriveStage::AfterResolve,
        &mut |v| lines.push(v.clone()),
    )
    .unwrap();
    let held = |id: &str| {
        entities
            .iter()
            .find(|e| e.id.as_str() == id)
            .unwrap()
            .attributes
            .get("counterparty")
            .cloned()
    };
    let line = |id: &str| lines.iter().find(|l| l.atom == id).unwrap();
    assert_eq!(held("entity-d1"), Some(json!(city)));
    let d1 = line("entity-d1");
    assert!(
        matches!(&d1.outcome, DerivedOutcome::Decided { values, documents, .. }
            if values == &[city.clone()] && documents == &["m1", "m2"]),
        "{d1:?}"
    );
    let cited: Vec<(&str, Option<&str>, &Value)> = d1
        .unresolved
        .iter()
        .map(|u| (u.claim.as_str(), u.document.as_deref(), &u.words))
        .collect();
    assert_eq!(
        cited,
        [
            ("claim-3", Some("m3"), &json!("Tucson")),
            ("claim-4", Some("m3"), &json!("Tucson"))
        ],
        "the mention is kept, cited, and never counted"
    );
    assert_eq!(held("entity-d2"), Some(json!(pge)));
    assert_eq!(
        held("entity-d3"),
        Some(json!(pge)),
        "m4's fields name no pge.com, but m3's metadata projected it: held by its exact identity value"
    );
    let d3 = line("entity-d3");
    assert!(
        matches!(&d3.outcome, DerivedOutcome::Decided { values, .. } if values == &[pge.clone()]),
        "{d3:?}"
    );
    assert!(d3.unresolved.is_empty(), "{d3:?}");
    assert_eq!(held("entity-d4"), None, "our own side is never held");
    let d4 = line("entity-d4");
    assert_eq!(d4.outcome, DerivedOutcome::Nothing);
    assert_eq!(d4.excluded, [enron]);
    assert!(d4.unresolved.is_empty(), "{d4:?}");
}
