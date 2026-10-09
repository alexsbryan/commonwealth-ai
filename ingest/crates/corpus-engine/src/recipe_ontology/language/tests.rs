use super::*;

#[test]
fn v1_retired_reader_keys_are_known_ignored_and_never_persisted() {
    let declared = r#"
[[types]]
name = "case"
kind = "entity"
identity_criterion = "same case record"

[[types]]
name = "membership"
kind = "claim"
force = "assertive"
subject = "case"
"#;
    let registry = OntologyLanguageRegistry::builtin();
    let language = registry.get(1).unwrap();
    let plain: toml::Table = toml::from_str(declared).unwrap();
    let keyed: toml::Table = toml::from_str(&format!(
        "document_reading = false\ndocument_reader = \"one_shot\"\n{declared}"
    ))
    .unwrap();
    assert!(
        registry.unknown_keys(&keyed).is_empty(),
        "a retired key is named as retired, never as a typo"
    );
    assert_eq!(
        retired_keys(&keyed)
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>(),
        ["document_reading", "document_reader"]
    );
    let policies = language.parse(&keyed).unwrap();
    assert_eq!(policies, language.parse(&plain).unwrap());
    assert!(policies.reads_documents());
    let wire = serde_json::to_value(&policies).unwrap();
    assert!(wire.get("document_reading").is_none() && wire.get("document_reader").is_none());
}

#[test]
fn v1_document_reading_supports_a_commissive_metadata_subject() {
    let body: toml::Table = toml::from_str(
        r#"
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

/// PRIMITIVES §0: an event identified by its criterion is a subject the reader
/// reads claims about, as a source-free entity is (RESOLVE decides both).
#[test]
fn v1_a_claim_about_an_event_with_a_criterion_is_read() {
    let body: toml::Table = toml::from_str(
        r#"
[[types]]
name = "happening"
kind = "event"
identity_criterion = "the same occurrence"

[[types]]
name = "report"
kind = "claim"
force = "assertive"
subject = "happening"
"#,
    )
    .unwrap();
    let policies = OntologyLanguageRegistry::builtin()
        .get(1)
        .unwrap()
        .parse(&body)
        .unwrap();
    let report = policies.type_decl("report").unwrap();
    assert!(report.is_document_reading_eligible(&policies.shape.types));
    assert!(policies.reads_documents());
}
