// SPDX-License-Identifier: AGPL-3.0-or-later
//! A reference to a type no declaration records lands on a record once the
//! method decides that type by name (`recipe_ontology::resolve_defaults`).

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::*;
use crate::enrichment::atlas::resolution_records::resolve_declared_types;
use crate::enrichment::atlas::resolve_records::{Answerer, DocumentResolution};
use crate::enrichment::pipeline::document_read::{
    ABOUT_ATTRIBUTE, CHAPTER_SUBJECT_ATTRIBUTE, CLAIM_FIELDS_ATTRIBUTE, LOCAL_REF_ATTRIBUTE,
    SOURCE_DOCUMENT_ATTRIBUTE, SUBJECT_FIELDS_ATTRIBUTE, SUBJECT_NAME_ATTRIBUTE,
};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

/// 02-contracts n1: `owner` refers to the base kind `person`, undeclared.
const CONTRACTS: &str = r#"
[[types]]
name = "contract"
kind = "entity"
description = "An agreement with a counterparty."
identity_criterion = "Two mentions are one contract when they name the same agreement."

[[types]]
name = "signed"
kind = "claim"
force = "assertive"
subject = "contract"
description = "A contract is signed."
attributes = [{ name = "owner", type = "ref", of = "person", description = "Who owns it on our side." }]
"#;

/// 07-inventory n3: `from` refers to a declared warehouse with no criterion,
/// which no statement is about.
const INVENTORY: &str = r#"
[[types]]
name = "part"
kind = "entity"
description = "A stocked part."
identity_criterion = "Two mentions are one part when they give the same part number."

[[types]]
name = "warehouse"
kind = "entity"
description = "A place parts are kept."

[[types]]
name = "moved"
kind = "claim"
force = "assertive"
subject = "part"
description = "Parts move between warehouses."
attributes = [{ name = "from", type = "ref", of = "warehouse", description = "Where they left." }]
"#;

fn policies(toml: &str) -> OntologyPolicies {
    OntologyBlock {
        version: 1,
        body: toml::from_str(toml).unwrap(),
    }
    .policies()
    .unwrap()
}

fn read(value: &str) -> Value {
    json!({"status": "supported", "value": value, "evidence": value})
}

/// A statement as a window read hands it to RESOLVE: of `kind` (none for a
/// particular, which is `about` a type), in section `section` and document
/// `doc` on `line`, its subject the window's `local` read as `name`, its own
/// fields `own`.
#[allow(clippy::too_many_arguments)]
fn handed(
    id: &str,
    kind: Option<&str>,
    about: Option<&str>,
    section: &str,
    doc: &str,
    line: &str,
    local: &str,
    name: &str,
    own: Value,
) -> Claim {
    let mut attributes = Map::new();
    for (field, reading) in own.as_object().unwrap() {
        attributes.insert(field.clone(), reading["value"].clone());
    }
    attributes.insert(CLAIM_FIELDS_ATTRIBUTE.into(), own);
    attributes.insert(LOCAL_REF_ATTRIBUTE.into(), json!(local));
    attributes.insert(
        CHAPTER_SUBJECT_ATTRIBUTE.into(),
        json!(format!("{section}/{local}")),
    );
    attributes.insert(SUBJECT_NAME_ATTRIBUTE.into(), json!(name));
    attributes.insert(SOURCE_DOCUMENT_ATTRIBUTE.into(), json!(doc));
    attributes.insert(SUBJECT_FIELDS_ATTRIBUTE.into(), json!({}));
    if let Some(about) = about {
        attributes.insert(ABOUT_ATTRIBUTE.into(), json!(about));
    }
    serde_json::from_value(json!({
        "id": id, "content": line, "discourse_act": "assert",
        "epistemic_status": "attributed", "scope": "universal", "subject": null,
        "evidence": [{"chunk_id": section, "passage_preview": line}],
        "claim_kind": kind, "anchor": line, "attributes": attributes,
        "enrichment_depth": "extracted",
    }))
    .unwrap()
}

fn row(id: u64, doc: &str, content: &str) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: content.into(),
        title: None,
        url: None,
        metadata_raw: Some(json!({ "id": doc }).to_string()),
        source_doc_id: Some(doc.into()),
    }
}

/// RESOLVE, then the after-RESOLVE derivations, over `claims`.
async fn resolve_and_derive(
    p: &OntologyPolicies,
    docs: &SectionDocuments,
    claims: &mut Vec<Claim>,
    answerer: Answerer<'_>,
) -> (Vec<Entity>, Vec<DerivedValue>) {
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
        claims,
        argument_reconstructions: &mut ars,
        positions: &mut positions,
        oppositions: &mut oppositions,
        edges: &mut edges,
        trajectories: &mut trajectories,
    };
    let participants = Participants::default();
    let mut on_document = |_: &str, _: &DocumentResolution| {};
    let (_, failures) = resolve_declared_types(
        &mut atoms,
        docs,
        &participants,
        p,
        "ref-targets",
        None,
        answerer,
        &mut on_document,
    )
    .await;
    assert!(failures.is_empty(), "{failures:?}");
    let mut lines = Vec::new();
    derive_attributes(
        &mut atoms,
        docs,
        &participants,
        p,
        DeriveStage::AfterResolve,
        &mut |v| lines.push(v.clone()),
    )
    .unwrap();
    (entities, lines)
}

const SIGNED_MSA: &str = "Dana Reyes signed the MSA for us.";
const SIGNED_NDA: &str = "Dana Reyes signed the NDA for us.";

/// Failing input, named: 02-contracts n1, "every contract with its owner on
/// our side", whose `owner` (`of = "person"`, never declared) was removed
/// from every statement: no `person` record existed. Two documents, each read
/// in its own window, name Dana Reyes as owner; each window names her as a
/// particular, RESOLVE decides by the default criterion (the scripted model
/// joins the second to the first), and both owners hold the one record.
#[tokio::test]
async fn a_person_no_recipe_declares_is_one_record_both_owners_hold() {
    let p = policies(CONTRACTS);
    let rows = [row(1, "d1", SIGNED_MSA), row(2, "d2", SIGNED_NDA)];
    let docs = SectionDocuments::from_chunk_rows([("s1", &[1u64][..]), ("s2", &[2u64][..])], &rows);
    let mut claims = vec![
        handed(
            "msa",
            Some("signed"),
            None,
            "s1",
            "d1",
            SIGNED_MSA,
            "p1",
            "the MSA",
            json!({ "owner": read("Dana Reyes") }),
        ),
        handed(
            "dana-1",
            None,
            Some("person"),
            "s1",
            "d1",
            SIGNED_MSA,
            "p2",
            "Dana Reyes",
            json!({}),
        ),
        handed(
            "nda",
            Some("signed"),
            None,
            "s2",
            "d2",
            SIGNED_NDA,
            "p1",
            "the NDA",
            json!({ "owner": read("Dana Reyes") }),
        ),
        handed(
            "dana-2",
            None,
            Some("person"),
            "s2",
            "d2",
            SIGNED_NDA,
            "p2",
            "Dana Reyes",
            json!({}),
        ),
    ];
    // The model: a person shown an earlier record is that record; anything
    // else opens its own.
    let seen: Arc<Mutex<Vec<ChatPrompt>>> = Arc::default();
    let kept = seen.clone();
    let infer: crate::InferenceFn = Arc::new(move |prompt: &ChatPrompt, _| {
        kept.lock().unwrap().push(prompt.clone());
        let person = prompt.user.contains("one person");
        let (same, cite) = match (person, prompt.user.contains("r0")) {
            (true, true) => ("r0", "Dana Reyes"),
            (true, false) => ("none", "Dana Reyes"),
            (false, _) => ("none", "signed the"),
        };
        let answer = json!({"particulars": [{"same_as": same, "mentions": [{"statement": "s0", "cite": cite}]}]});
        Box::pin(async move { Ok(answer.to_string()) })
    });
    let (entities, _) = resolve_and_derive(&p, &docs, &mut claims, Answerer::Model(&infer)).await;

    let people: Vec<&Entity> = entities
        .iter()
        .filter(|e| e.entity_type.as_str_repr() == "person")
        .collect();
    let prompts: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.user.clone())
        .collect();
    let [dana] = people.as_slice() else {
        panic!("one person record: {people:?}\n{prompts:#?}")
    };
    let owner = |id: &str| {
        claims
            .iter()
            .find(|c| c.id.as_str() == id)
            .unwrap()
            .attributes
            .get("owner")
            .cloned()
    };
    let held = Some(Value::String(dana.id.as_str().to_string()));
    assert_eq!(owner("msa"), held, "{prompts:#?}");
    assert_eq!(owner("nda"), held, "{prompts:#?}");
}

const MOVED: &str = "Forty pumps moved from North Depot today.";

/// Failing input, named: 07-inventory n3, "every movement: a transfer between
/// warehouses", whose `from` (a declared warehouse with no criterion and no
/// statement about it) was removed: the window reader dropped the warehouse it
/// named. Decided by name, the warehouse is a particular RESOLVE records, and
/// the movement's `from` holds it.
#[tokio::test]
async fn a_movement_from_a_warehouse_holds_its_record() {
    let p = policies(INVENTORY);
    let rows = [row(1, "d1", MOVED)];
    let docs = SectionDocuments::from_chunk_rows([("s1", &[1u64][..])], &rows);
    let mut claims = vec![
        handed(
            "move",
            Some("moved"),
            None,
            "s1",
            "d1",
            MOVED,
            "p1",
            "pumps",
            json!({ "from": read("North Depot") }),
        ),
        handed(
            "depot",
            None,
            Some("warehouse"),
            "s1",
            "d1",
            MOVED,
            "p2",
            "North Depot",
            json!({}),
        ),
    ];
    let (entities, _) = resolve_and_derive(&p, &docs, &mut claims, Answerer::Proposed).await;
    let depots: Vec<&Entity> = entities
        .iter()
        .filter(|e| e.entity_type.as_str_repr() == "warehouse")
        .collect();
    let [depot] = depots.as_slice() else {
        panic!("one warehouse record: {entities:?}")
    };
    let moved = claims.iter().find(|c| c.id.as_str() == "move").unwrap();
    assert_eq!(
        moved.attributes.get("from"),
        Some(&Value::String(depot.id.as_str().to_string()))
    );
}
