use super::super::tests::{input_chapter, row};
use super::*;
use crate::enrichment::ontology::OntologyV1;

fn declared() -> OntologyPolicies {
    let ontology: OntologyV1 = toml::from_str(DECLARED).unwrap();
    ontology.into_policies()
}

const DECLARED: &str = r#"
[[types]]
name = "firm"
kind = "entity"
attributes = [{ name = "site", type = "text" }, { name = "label", type = "text" }]
identity = ["site"]
source = { metadata = ["sender", "copied"], attributes = { site = "domain", label = "display_name" } }

[[types]]
name = "job"
kind = "entity"
identity_criterion = "same job"

[[types]]
name = "note"
kind = "claim"
force = "assertive"
subject = "job"
attributes = [{ name = "party", type = "ref", of = "firm" }]

[[sets]]
id = "home"
type = "firm"
where = { site = { suffix = "home.example" } }

[[paths]]
id = "outside"
path = "document / (sender | copied) [!home]"
"#;

fn document() -> crate::enrichment::atlas::SourceDocument {
    input_chapter(&[row(
        1,
        "d1",
        "Body.",
        r#"{"sender": "Ann <ann@away.example>",
            "copied": "Bo <bo@desk.home.example>, Cy <cy@away.example>"}"#,
    )])
    .source_documents[0]
        .clone()
}

/// The source's records less the exclusion set, each once: Cy's site is
/// Ann's, and Bo's is in `home`.
#[test]
fn the_candidates_are_the_records_less_exclusions() {
    let p = declared();
    let firm = p.type_decl("firm").unwrap();
    let got = candidates(&document(), &p, firm);
    assert_eq!(
        got.shown,
        [Candidate::Record {
            identity: vec!["away.example".into()],
            names: vec!["Ann".into()],
            field: "sender".into(),
            scalar: "Ann <ann@away.example>".into(),
        }]
    );
    assert_eq!(got.dropped, 0);
}
