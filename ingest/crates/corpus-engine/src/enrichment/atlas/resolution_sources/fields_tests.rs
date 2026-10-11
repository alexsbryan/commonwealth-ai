use serde_json::json;

use super::*;

fn source(exclude: &[&str]) -> MetadataSourceDecl {
    MetadataSourceDecl {
        metadata: vec!["from".into()],
        attributes: [("site".to_string(), FieldReader::Domain)].into(),
        exclude: exclude.iter().map(|e| e.to_string()).collect(),
        refs: Default::default(),
    }
}

fn sites(src: &MetadataSourceDecl, value: &str) -> Vec<String> {
    field_records(src, &["site".to_string()], &json!(value))
        .into_iter()
        .map(|r| r.identity.join(" "))
        .collect()
}

/// E5: no list is in code. A source excludes what its `exclude` names, a
/// value or a bundled list, and a value covers its subdomains.
#[test]
fn a_source_excludes_only_what_it_declares() {
    let to = "a <a@hotmail.com>, b <b@mail.example.org>, c <c@notexample.org>, d <d@acme.org>";
    assert_eq!(
        sites(&source(&[]), to),
        [
            "hotmail.com",
            "mail.example.org",
            "notexample.org",
            "acme.org"
        ]
    );
    assert_eq!(
        sites(&source(&["example.org"]), to),
        ["hotmail.com", "notexample.org", "acme.org"]
    );
    assert_eq!(
        sites(&source(&["@bundled:mailbox_providers"]), to),
        ["mail.example.org", "notexample.org", "acme.org"]
    );
}

#[test]
fn a_bundled_list_that_does_not_exist_is_refused() {
    assert!(Exclusion::of(&source(&["@bundled:no_such_list"])).is_err());
    // The reader names no record of a declaration the projection refuses.
    assert!(sites(&source(&["@bundled:no_such_list"]), "d <d@acme.org>").is_empty());
}
