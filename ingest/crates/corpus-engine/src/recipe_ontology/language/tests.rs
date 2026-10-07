use super::*;

#[test]
fn v1_document_reading_is_a_registered_persisted_policy() {
    let body: toml::Table = toml::from_str(
        r#"
document_reading = true

[[types]]
name = "case"
kind = "entity"
identity_criterion = "same case record"

[[types]]
name = "membership"
kind = "claim"
force = "assertive"
subject = "case"
"#,
    )
    .unwrap();
    let registry = OntologyLanguageRegistry::builtin();

    assert!(
        registry.unknown_keys(&body).is_empty(),
        "the V1 registry must recognize the opt-in key"
    );
    let language = registry.get(1).unwrap();
    let policies = language.parse(&body).unwrap();
    assert_eq!(
        serde_json::to_value(policies).unwrap()["document_reading"],
        true,
        "the parsed policy carrier must retain the opt-in"
    );
    assert!(
        serde_json::to_value(OntologyPolicies::default())
            .unwrap()
            .get("document_reading")
            .is_none(),
        "default-off policies must preserve legacy serialized bytes"
    );
}

#[test]
fn v1_document_reading_supports_a_commissive_metadata_subject() {
    let body: toml::Table = toml::from_str(
        r#"
document_reading = true

[[types]]
name = "case"
kind = "entity"
identity_criterion = "the same issue"

[[types]]
name = "person"
kind = "entity"
attributes = [{ name = "email", type = "text" }, { name = "name", type = "text" }]
identity = ["email"]
source = { metadata = ["from", "to", "cc"], attributes = { email = "address", name = "display_name" } }

[[types]]
name = "stage_update"
kind = "claim"
force = "assertive"
subject = "case"

[[types]]
name = "commitment"
kind = "claim"
force = "commissive"
subject = "person"
"#,
    )
    .unwrap();
    let registry = OntologyLanguageRegistry::builtin();
    let policies = registry.get(1).unwrap().parse(&body).unwrap();
    let commitment = policies
        .shape
        .types
        .iter()
        .find(|ty| ty.name == "commitment")
        .unwrap();
    assert_eq!(commitment.force, Some(Force::Commissive));
    assert!(commitment.is_document_reading_eligible(&policies.shape.types));
}
