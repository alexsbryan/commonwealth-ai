use serde_json::json;

use super::super::tests::{input_chapter, row, scripted};
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
/// that name no listed record and no excluded one (Bo, by name or by site).
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
            mention(4, "Bo"),
            mention(5, "desk.home.example"),
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
        view: "",
        lines: (1, 1),
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
        view: "",
        lines: (1, 1),
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

/// Whole text: a Pick question shows the statement in its whole document,
/// numbered as Locate shows it, so a party the message names far from the
/// statement is in front of the reader; the window it replaced held only the
/// statement's neighbourhood.
#[tokio::test]
async fn a_pick_question_shows_the_whole_document() {
    let p = declared();
    let note = p.type_decl("note").unwrap();
    let far = "Our client here is Quay Market, for the shop front.";
    let filler: Vec<String> = (0..12)
        .map(|i| format!("Line {i} of the thread says nothing about anyone in particular."))
        .collect();
    let body = format!(
        "{far}\n{}\nPlease send them the revised terms.",
        filler.join("\n")
    );
    let lines = super::super::passes::lines(&body);
    let view = super::super::passes::numbered(&body, &lines);
    let last = lines.last().unwrap();
    let offered: BTreeMap<String, Candidates> = [(
        "firm".to_string(),
        candidates(
            &document(),
            &p,
            p.type_decl("firm").unwrap(),
            &[mention(1, "Quay Market")],
        ),
    )]
    .into();
    let (infer, seen) = scripted(vec![dist(&["A", "B", NONE], "B")]);
    let ask = Ask {
        infer: &infer,
        facts: "",
        document: "d1",
        statement: "l14-14",
        body: &body,
        view: &view,
        lines: (last.n, last.n),
        at: last.start..last.end,
        evidence: "Please send them the revised terms.",
        candidates: &offered,
    };
    let mut calls = 0;
    pick(&ask, note, &note.attributes[0], "firm", &mut calls).await;
    let asked = &seen.lock().unwrap()[0].user;
    assert!(asked.contains(&format!("1 {far}")), "{asked}");
    assert!(
        asked.contains(&format!(
            "Statement, line {}, its words in [[ ]]: \"[[Please send them the revised terms.]]\"",
            last.n
        )),
        "{asked}"
    );
    let window =
        crate::enrichment::atlas::resolve_records::marked_context(&body, last.start, last.end);
    assert!(
        !window.contains("Quay Market"),
        "the old window already held it: {window}"
    );
}

/// A mention of a type whose mentions RESOLVE decides is a particular when
/// Pick would offer it by its words: one per such mention, its words on its
/// whole line, reading no field. A mention naming a listed record (Ann) or an
/// excluded one (Bo) is none, and a type with no criterion gives none.
#[test]
fn a_mention_pick_offers_by_its_words_is_a_particular_of_a_decided_type() {
    let body = "Ann said yes.\nQuay Market and Bo will sign.\nThanks";
    let doc = input_chapter(&[row(
        1,
        "d1",
        body,
        r#"{"sender": "Ann <ann@away.example>",
            "copied": "Bo <bo@desk.home.example>, Cy <cy@away.example>"}"#,
    )])
    .source_documents[0]
        .clone();
    let raw = doc.raw_body();
    let lines = super::super::passes::lines(&raw);
    let mentions = [
        mention(1, "Ann"),
        mention(2, "Quay Market"),
        mention(2, "Bo"),
    ];
    let with_criterion: OntologyV1 = toml::from_str(&DECLARED.replace(
        "identity = [\"site\"]",
        "identity = [\"site\"]\nidentity_criterion = \"the same firm\"",
    ))
    .unwrap();
    let p = with_criterion.into_policies();
    let firm = p.type_decl("firm").unwrap();
    let got = super::super::mention::particulars(&doc, &p, &raw, &lines, &[firm], &mentions);
    assert_eq!(
        got,
        [super::super::DocumentReadParticular {
            subject_type: "firm".into(),
            subject_local_ref: "l2:Quay Market".into(),
            subject_name: "firm l2:Quay Market".into(),
            evidence: "Quay Market and Bo will sign.".into(),
            subject_fields: BTreeMap::from([(
                "site".to_string(),
                DocumentReadField::Unknown {
                    reason: "a mention is its words; no field of it is read".into()
                }
            )]),
            words: Some("Quay Market".into()),
            other_names: Default::default(),
        }]
    );
    let p = declared();
    let firm = p.type_decl("firm").unwrap();
    assert!(
        super::super::mention::particulars(&doc, &p, &raw, &lines, &[firm], &mentions).is_empty(),
        "no criterion: Pick's proposal only"
    );
}
