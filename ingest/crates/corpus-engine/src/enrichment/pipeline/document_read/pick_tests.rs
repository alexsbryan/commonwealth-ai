use serde_json::json;

use super::super::tests::{input_chapter, row, scripted};
use super::*;
use crate::enrichment::ontology::OntologyV1;

fn declared() -> OntologyPolicies {
    let ontology: OntologyV1 = toml::from_str(
        r#"
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
"#,
    )
    .unwrap();
    ontology.into_policies()
}

fn mention(line: usize, text: &str) -> Mention {
    Mention {
        of: "firm".into(),
        line,
        text: text.into(),
    }
}

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

/// The source's records less the exclusion set, each once, then the mentions
/// that name no listed record.
#[test]
fn the_candidates_are_the_records_less_exclusions_then_new_mentions() {
    let p = declared();
    let firm = p.type_decl("firm").unwrap();
    let got = candidates(
        &document(),
        &p,
        firm,
        &[
            mention(1, "Ann"),
            mention(2, "Quay Market"),
            mention(3, "Quay  Market"),
        ],
    );
    assert_eq!(
        got.shown,
        [
            Candidate::Record {
                identity: vec!["away.example".into()],
                names: vec!["Ann".into()],
                field: "sender".into(),
                scalar: "Ann <ann@away.example>".into(),
            },
            Candidate::Mention {
                line: 2,
                text: "Quay Market".into()
            },
        ]
    );
    assert_eq!(got.dropped, 0);
}

fn dist(labels: &[&str], pick: &str) -> serde_json::Value {
    let m: serde_json::Map<String, serde_json::Value> = labels
        .iter()
        .map(|l| (l.to_string(), json!(if *l == pick { 0.9 } else { 0.01 })))
        .collect();
    serde_json::Value::Object(m)
}

#[tokio::test]
async fn a_pick_is_a_candidates_value_cited_where_it_is_named() {
    let p = declared();
    let firm = p.type_decl("firm").unwrap();
    let note = p.type_decl("note").unwrap();
    let party = &note.attributes[0];
    let offered: BTreeMap<String, Candidates> = [(
        "firm".to_string(),
        candidates(&document(), &p, firm, &[mention(2, "Quay Market")]),
    )]
    .into();
    let ask = |infer| Ask {
        infer,
        facts: "",
        document: "d1",
        statement: "l1-1",
        body: "We ordered from Quay Market.",
        at: 0..28,
        evidence: "We ordered from Quay Market.",
        candidates: &offered,
    };
    let labels = ["A", "B", NONE];
    let (infer, seen) = scripted(vec![
        dist(&labels, "B"),
        dist(&labels, "A"),
        dist(&labels, NONE),
    ]);
    let mut calls = 0;
    let read = pick(&ask(&infer), note, party, "firm", &mut calls).await;
    let by = Some(SourcePrecision::new("reader_pick", Precision::Unmeasured));
    assert_eq!(
        read,
        DocumentReadField::Supported {
            value: json!("Quay Market"),
            evidence: "Quay Market".into(),
            by: by.clone(),
        }
    );
    let read = pick(&ask(&infer), note, party, "firm", &mut calls).await;
    assert_eq!(
        read,
        DocumentReadField::Supported {
            value: json!("away.example"),
            evidence: "Ann <ann@away.example>".into(),
            by,
        }
    );
    let read = pick(&ask(&infer), note, party, "firm", &mut calls).await;
    assert!(
        matches!(read, DocumentReadField::Unknown { .. }),
        "{read:?}"
    );
    let prompts = seen.lock().unwrap();
    assert!(
        prompts[0].user.contains(
            "Which firm does the statement's party refer to?\n\
             A firm away.example (Ann), named in the field `sender`\n\
             B firm \"Quay Market\", named on line 2\n\
             0 none of them, or the statement does not say\n"
        ),
        "{}",
        prompts[0].user
    );
    assert_eq!(prompts[0].phase_id.as_deref(), Some(PICK_PHASE));
}

#[tokio::test]
async fn a_reference_with_no_candidate_is_not_asked() {
    let p = declared();
    let note = p.type_decl("note").unwrap();
    let none = BTreeMap::new();
    let (infer, seen) = scripted(vec![]);
    let ask = Ask {
        infer: &infer,
        facts: "",
        document: "d1",
        statement: "l1-1",
        body: "x",
        at: 0..1,
        evidence: "x",
        candidates: &none,
    };
    let mut calls = 0;
    let read = pick(&ask, note, &note.attributes[0], "firm", &mut calls).await;
    assert!(
        matches!(&read, DocumentReadField::Unknown { reason } if reason.contains("no `firm` candidate")),
        "{read:?}"
    );
    assert!(calls == 0 && seen.lock().unwrap().is_empty());
}
