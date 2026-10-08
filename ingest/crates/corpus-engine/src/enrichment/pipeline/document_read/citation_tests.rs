use super::*;
use std::sync::Arc;

fn context(prompt: &super::super::super::types::ChatPrompt) -> Value {
    serde_json::from_str(&prompt.user[prompt.user.find('{').unwrap()..]).unwrap()
}

fn handle(document: &Value, quote: &str) -> String {
    document["citation_choices"]
        .as_object()
        .expect("the actual source supplies selectable citation handles")
        .iter()
        .find(|(_, value)| value.as_str() == Some(quote))
        .map(|(key, _)| key.clone())
        .unwrap_or_else(|| panic!("no source choice for {quote:?}"))
}

fn cited_status(number: &str, status: &str, citation: &str) -> Value {
    let mut value = claim(
        "reported_status",
        number,
        &format!("Case {number}"),
        citation,
        "supported",
    );
    value["fields"]["status"]["value"] = json!(status);
    value["subject_fields"]["number"]["value"] = json!(number);
    value["subject_fields"]["number"]["evidence"] = json!(citation);
    value
}

#[tokio::test]
async fn source_citation_handles_replay_to_exact_qualified_fields_in_the_production_runner() {
    let first = "q:842 | uv | closed";
    let second = "159 | uv | open";
    let chapter = input_chapter(&[row(
        1,
        "doc-a",
        &format!("Case | Project | Status\n{first}\n{second}"),
        r#"{"author":"Alice","date":"2024-02-03"}"#,
    )]);
    let chat: crate::types::InferenceFn = Arc::new(move |prompt, _| {
        let input = context(prompt);
        let document = &input["documents"][0];
        let answer = response(json!([{
            "document_id":"doc-a", "status":"read",
            "claims":[
                cited_status("842", "closed", &handle(document, first)),
                cited_status("159", "open", &handle(document, second)),
            ]
        }]));
        Box::pin(async move { Ok(answer) })
    });
    let directory = tempfile::tempdir().unwrap();
    let result = reader_runner(directory.path(), &policies(), chat, false)
        .phase_1_extract_questions(
            &[chapter.clone()],
            &super::super::super::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    assert!(result.output.failures.is_empty());
    let mut extraction = result.output.questions_by_chapter[0]
        .section_extraction
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(extraction.claims.len(), 2);
    for (claim, quote) in extraction.claims.iter().zip([first, second]) {
        assert_eq!(claim.anchor, quote);
        assert_eq!(
            claim.attributes[CLAIM_FIELDS_ATTRIBUTE]["status"]["evidence"],
            quote
        );
    }
    let document = &extraction.document_read.as_ref().unwrap().documents[0];
    assert!(document.refused.is_empty());
    assert_eq!(document.claims[0].evidence, first);
    assert!(matches!(
        document.claims[0].subject_fields["project"],
        DocumentReadField::Unknown { .. }
    ));
    let qualified = extraction.clone();
    validate_and_stamp(&chapter, &policies(), &mut extraction).unwrap();
    assert_eq!(
        serde_json::to_value(extraction).unwrap(),
        serde_json::to_value(qualified).unwrap(),
        "cached quotes are never decoded as handles again"
    );
}

#[test]
fn source_citation_decoder_is_document_scoped_and_does_not_admit_composed_quotes() {
    let chapter = input_chapter(&[
        row(1, "doc-a", "842 | uv | closed", r#"{"date":"2024-02-03"}"#),
        row(2, "doc-b", "159 | uv | open", r#"{"date":"2024-02-04"}"#),
    ]);
    let prompt = compose(&chapter, &policies(), "phase1");
    let input = context(&prompt);
    let first = handle(&input["documents"][0], "842 | uv | closed");
    let other = handle(&input["documents"][1], "159 | uv | open");
    let schema = prompt.response_schema.unwrap();
    let read = schema["properties"]["documents"]["items"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| {
            branch["properties"]["document_id"]["const"] == "doc-a"
                && branch["properties"]["status"]["const"] == "read"
        })
        .unwrap();
    let branch = read["properties"]["claims"]["items"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| branch["properties"]["kind"]["const"] == "reported_status")
        .unwrap();
    let choices = branch["properties"]["fields"]["properties"]["status"]["oneOf"][0]["properties"]
        ["evidence"]["enum"]
        .as_array()
        .unwrap();
    assert!(choices.contains(&json!(first)));
    assert!(!choices.contains(&json!(other)));
    assert!(!choices.contains(&json!("Status closed")));

    let response = response(json!([
        {"document_id":"doc-a", "status":"read", "claims":[cited_status("842", "closed", &other)]},
        {"document_id":"doc-b", "status":"nothing_applicable", "reason":"negative control", "claims":[]},
    ]));
    let mut extraction = parse_response(&response, &policies())
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies(), &mut extraction).unwrap();
    assert!(extraction.claims.is_empty());
    assert_eq!(
        extraction.document_read.unwrap().documents[0].refused.len(),
        1
    );
}
