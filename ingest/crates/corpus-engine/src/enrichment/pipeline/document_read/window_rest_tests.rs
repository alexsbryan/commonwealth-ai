// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::super::super::tests::policies;
use super::super::super::{parse_response, validate_and_stamp};
use super::super::read;
use super::super::tests::window;
use super::fold;

/// The second look adds what the first did not write: a claim the first
/// wrote again (its quote spaced and cased apart) is dropped, and the first's
/// particular named again under its ref is the first's.
#[test]
fn a_second_look_adds_only_what_the_first_did_not_write() {
    let mut first = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42"}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d1",
                    "quote": "Issue 42 in uv is open.", "fields": {"status": "Open"}}]});
    let second = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "Issue 42"},
                        {"ref": "p2", "type": "case", "name": "issue 7"}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d1",
                    "quote": "issue 42  in uv is open.", "fields": {"status": "Open"}},
                   {"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "fields": {"status": "Closed"}}]});
    assert_eq!(fold(&mut first, second), (1, 1, 1, 0));
    let refs: Vec<_> = first["particulars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["ref"].clone())
        .collect();
    assert_eq!(refs, [json!("p1"), json!("p2")]);
    let documents: Vec<_> = first["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["document"].clone())
        .collect();
    assert_eq!(documents, [json!("d1"), json!("d2")]);
}

/// A ref the second look gives to another particular is renamed apart
/// wherever that answer writes it, so its claims and references stay its own.
#[test]
fn a_ref_the_second_look_reuses_for_another_particular_is_renamed_apart() {
    let mut first = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42"}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d1", "quote": "a"}]});
    let second = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 7", "fields": {"duplicate_of": {"value": "p1"}}}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d2", "quote": "b",
                    "fields": {"blocks": "p1"}}]});
    assert_eq!(fold(&mut first, second), (1, 1, 0, 1));
    assert_eq!(first["particulars"][1]["ref"], "p1~2");
    assert_eq!(
        first["particulars"][1]["fields"]["duplicate_of"]["value"],
        "p1~2"
    );
    assert_eq!(first["claims"][1]["about"], "p1~2");
    assert_eq!(first["claims"][1]["fields"]["blocks"], "p1~2");
    assert_eq!(first["claims"][0]["about"], "p1");
}

/// The window is asked twice, the second time shown the first answer; a
/// second answer that is not JSON leaves the first answer's claims, and a
/// second answer that repeats the first adds nothing.
#[tokio::test]
async fn a_window_is_read_in_two_looks_and_a_failed_second_costs_nothing() {
    let policies = policies();
    let chapter = window();
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {}}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "fields": {"status": "Closed"}}]})
    .to_string();
    for second in ["I am stuck in a loop.", answer.as_str()] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        let (first, second) = (answer.clone(), second.to_string());
        let infer: crate::InferenceFn = Arc::new(
            move |p: &crate::enrichment::pipeline::types::ChatPrompt, _| {
                let mut asked = kept.lock().unwrap();
                asked.push(p.user.clone());
                let raw = if asked.len() == 1 {
                    first.clone()
                } else {
                    second.clone()
                };
                Box::pin(async move { Ok(raw) })
            },
        );
        let out = read(&chapter, &policies, &Default::default(), &infer)
            .await
            .unwrap();
        let mut extraction = parse_response(&out, &policies)
            .unwrap()
            .section_extraction
            .unwrap();
        validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
        let read = extraction.document_read.as_ref().unwrap();
        assert_eq!(read.documents[1].claims.len(), 1, "{:?}", read.documents);

        let asked = seen.lock().unwrap();
        assert_eq!(asked.len(), 2);
        let shown = asked[1]
            .strip_prefix(asked[0].as_str())
            .expect("the second look extends the first prompt");
        assert!(
            shown.contains("Already written for this window:")
                && shown.contains("Issue 42 is now closed."),
            "{shown}"
        );
    }
}
