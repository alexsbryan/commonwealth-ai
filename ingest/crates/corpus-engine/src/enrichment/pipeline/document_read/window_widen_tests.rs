// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quote a claim cites, widened to its sentence (`sentence_of`) within
//! the document's own lines.
use serde_json::json;

use super::super::tests::{input_chapter, policies, row, scripted};
use super::super::{parse_response, validate_and_stamp};
use super::read;

/// A quote cut at a mail's line wrap is cited as its whole sentence, across
/// the wrap and no further; a price's decimal point ends no sentence; a quote
/// whose words are not in the text is left for validation to refuse.
#[test]
fn a_quote_is_widened_to_its_sentence_across_line_wraps() {
    use super::sentence_of;
    let text = "Thanks for the call.\nHartosn the Border piece is .50, subject to credit and\nKim's got the attached confirm. Talk soon.\n\nNext para.";
    assert_eq!(
        sentence_of(text, "Hartosn the Border piece is .50").as_deref(),
        Some("Hartosn the Border piece is .50, subject to credit and Kim's got the attached confirm.")
    );
    assert_eq!(
        sentence_of(text, "talk SOON.").as_deref(),
        Some("Talk soon.")
    );
    assert_eq!(sentence_of(text, "Next").as_deref(), Some("Next para."));
    assert_eq!(sentence_of(text, "a sentence the mail never says"), None);
}

/// A quote is widened only within the run of own lines it stands in: where
/// carried lines were removed the sentence stops, so what code cites is a
/// passage of the body validation checks. Failing input, named: ward-story
/// pasadena/37. (2026-10-17), "6. Saleto Enron at SoCalBorder, ..." cited
/// verbatim, widened back across the four carried lines above it to item 1,
/// and refused as no exact passage in four reads of four.
#[tokio::test]
async fn a_widened_sentence_stops_where_carried_lines_were_removed() {
    let mut policies = policies();
    policies.change.document = Some(crate::enrichment::ontology::DocumentFieldsDecl {
        date: Some("date".into()),
        thread: None,
        id: None,
        author: None,
    });
    let chapter = input_chapter(&[
        row(
            1,
            "doc-a",
            "2. Issue 7 is triaged;\n3. Issue 8 waits on review;\n4. Issue 9 is a duplicate;",
            r#"{"date":"Mon, 14 May 2001 09:00:00 -0700"}"#,
        ),
        row(
            2,
            "doc-b",
            "1. Issue 6 ships in March (after review, 4,124 lines);\n2. Issue 7 is triaged;\n3. Issue 8 waits on review;\n4. Issue 9 is a duplicate;\n5. Issue 42 closed at 2.68 per seat\nThe May notes are final.",
            r#"{"date":"Tue, 15 May 2001 09:00:00 -0700"}"#,
        ),
    ]);
    let classes = super::super::LineClasses::of(&chapter.source_documents, &policies);
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {}}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "5. Issue 42 closed at 2.68 per seat", "fields": {"status": "Closed"}}]});
    let (infer, _) = scripted(vec![answer]);
    let raw = read(&chapter, &policies, &classes, &infer).await.unwrap();
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
    let d2 = &extraction.document_read.as_ref().unwrap().documents[1];
    assert!(d2.refused.is_empty(), "{:?}", d2.refused);
    let read: Vec<&str> = d2.claims.iter().map(|c| c.evidence.as_str()).collect();
    assert_eq!(
        read,
        ["5. Issue 42 closed at 2.68 per seat The May notes are final."]
    );
}
