use super::*;
use crate::enrichment::ontology::Force;
use serde_json::json;

#[test]
fn document_read_accepts_every_declared_claim_force() {
    let policies = policies();
    let membership = policies
        .shape
        .types
        .iter()
        .find(|ty| ty.name == "membership")
        .unwrap();
    for force in Force::ALL {
        let mut declared = membership.clone();
        declared.force = Some(force);
        assert!(
            eligible_claim(&declared, &policies),
            "document reading must include declared {force:?} claims"
        );
    }
}

#[test]
fn document_read_preserves_commissive_force_without_licensing_an_act() {
    let body = "Ada commits to add an alias for issue 842 by Friday.";
    let rows = [row(
        1,
        "doc-a",
        body,
        r#"{"author":"Alice","role":"member","date":"2024-02-03","kind":"comment","id":"doc-a"}"#,
    )];
    let chapter = input_chapter(&rows);
    let mut policies = policies();
    let assertive_contract = contract_fingerprint(&policies);
    policies
        .shape
        .types
        .iter_mut()
        .find(|ty| ty.name == "membership")
        .unwrap()
        .force = Some(Force::Commissive);
    assert_ne!(assertive_contract, contract_fingerprint(&policies));

    assert_eq!(
        super::super::schema::contract_value(&policies)["claim_types"][0]["force"],
        "commissive"
    );

    let mut commitment = claim("membership", "commitment-1", "Case 842", body, "unknown");
    commitment["content"] = json!(body);
    commitment["speaker"] = json!("Ada");
    let raw = response(json!([{
        "document_id":"doc-a",
        "status":"read",
        "claims":[commitment]
    }]));
    let mut parsed = parse_response(&raw, &policies).unwrap();
    let extraction = parsed.section_extraction.as_mut().unwrap();
    validate_and_stamp(&chapter, &policies, extraction).unwrap();

    assert_eq!(
        extraction.claims[0].discourse_act,
        crate::enrichment::pipeline::atlas::DiscourseAct::Commit
    );
    assert_eq!(extraction.claims[0].attributed_to.as_deref(), Some("Ada"));
    assert_eq!(extraction.claims[0].anchor, body);
    assert_eq!(extraction.claims.len(), 1);
    assert!(extraction.entities_developed.is_empty());
    assert!(extraction.relations_introduced.is_empty());
    assert!(extraction.relations_developed.is_empty());
    assert!(extraction.events.is_empty());
    assert!(extraction.questions_raised.is_empty());
}
