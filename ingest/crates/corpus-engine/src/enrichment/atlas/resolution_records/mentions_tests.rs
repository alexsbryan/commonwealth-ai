// SPDX-License-Identifier: AGPL-3.0-or-later
//! A type whose source names only who took part (`decides_mentions`): RESOLVE
//! decides its statements, starting from its source's records.

use super::*;
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
