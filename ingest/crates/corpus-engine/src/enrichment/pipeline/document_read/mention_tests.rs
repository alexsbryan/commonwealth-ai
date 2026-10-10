use serde_json::json;

use super::super::passes::lines;
use super::super::tests::scripted;
use super::*;

fn dist(labels: &[&str], pick: &str) -> serde_json::Value {
    let m: serde_json::Map<String, serde_json::Value> = labels
        .iter()
        .map(|l| (l.to_string(), json!(if *l == pick { 0.9 } else { 0.01 })))
        .collect();
    serde_json::Value::Object(m)
}

fn ty(name: &str, description: &str) -> OntologyTypeDecl {
    OntologyTypeDecl {
        name: name.into(),
        description: description.into(),
        ..Default::default()
    }
}

/// Each asked line is one choice over the types; a line naming one is
/// pointed at, and its words are the mention. A line not asked (a quote) is
/// never asked; a line naming none is not pointed at.
#[tokio::test]
async fn a_line_naming_a_type_is_pointed_at_and_its_words_are_the_mention() {
    let body = "We spoke with Birch Hall today.\n> quoted line\nNothing here.";
    let ls = lines(body);
    let firm = ty("firm", "an organization");
    let place = ty("place", "");
    let (infer, seen) = scripted(vec![
        // Line 1 names a firm: start at "Birch", end at "Hall".
        dist(&["A", "B", NONE], "A"),
        dist(&["A", "B", "C", "D", "E", "F", NONE], "D"),
        dist(&["A", "B", "C"], "B"),
        // Line 3 names none.
        dist(&["A", "B", NONE], NONE),
    ]);
    let mut calls = 0;
    let got = mentions(
        &infer,
        "FACTS\n\n",
        "doc",
        body,
        &ls,
        &[true, false, true],
        &[&firm, &place],
        &mut calls,
    )
    .await;
    assert_eq!(
        got,
        [Mention {
            of: "firm".into(),
            line: 1,
            text: "Birch Hall".into()
        }]
    );
    assert_eq!(calls, 4);
    let prompts = seen.lock().unwrap();
    assert!(prompts[0].user.starts_with(
        "FACTS\n\nDocument, its lines numbered:\n<<<\n1 We spoke with Birch Hall today.\n2 > quoted line\n3 Nothing here.\n>>>\n\nLine 1:"
    ));
    assert!(
        prompts[0]
            .user
            .contains("A firm (an organization)\nB place\n0 none of them\n"),
        "{}",
        prompts[0].user
    );
    assert_eq!(prompts[0].phase_id.as_deref(), Some(MENTION_PHASE));
    assert!(prompts[1]
        .user
        .contains("Kind of thing: firm (an organization)"));
    assert!(!prompts.iter().any(|p| p.user.contains("Line 2:")));
}

#[tokio::test]
async fn no_target_asks_nothing() {
    let (infer, seen) = scripted(vec![]);
    let mut calls = 0;
    let body = "One line.";
    let got = mentions(
        &infer,
        "",
        "doc",
        body,
        &lines(body),
        &[true],
        &[],
        &mut calls,
    )
    .await;
    assert!(got.is_empty() && calls == 0 && seen.lock().unwrap().is_empty());
}
