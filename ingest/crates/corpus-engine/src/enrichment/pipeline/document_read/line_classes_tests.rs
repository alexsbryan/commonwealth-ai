use super::super::tests::{input_chapter, policies, row};
use super::*;
use crate::enrichment::ontology::DocumentFieldsDecl;

fn declared(author: Option<&str>) -> OntologyPolicies {
    let mut p = policies();
    p.change.document = Some(DocumentFieldsDecl {
        date: Some("date".into()),
        thread: None,
        id: None,
        author: author.map(str::to_string),
    });
    p
}

/// Three documents: b quotes a's second line under a marker, c (a's author
/// again, dated before b) repeats a's sign-off.
fn chapter() -> crate::enrichment::pipeline::types::ChapterInput {
    input_chapter(&[
        row(
            1,
            "a",
            "Hello,\nThe kiln is fixed and fires again.\nAnn, the studio",
            r#"{"date": "2026-01-02", "from": "Ann <ann@x.example>"}"#,
        ),
        row(
            2,
            "b",
            "Good news.\n> The kiln is fixed and fires again.\nBo",
            r#"{"date": "2026-01-05", "from": "Bo <bo@y.example>"}"#,
        ),
        row(
            3,
            "c",
            "A new glaze arrived.\n  Ann, the studio  ",
            r#"{"date": "2026-01-03", "from": "ann <ANN@x.example>"}"#,
        ),
    ])
}

#[test]
fn a_line_key_runs_from_its_first_to_its_last_letter_or_digit() {
    assert_eq!(
        line_key("> The kiln  is fixed. ").as_deref(),
        Some("The kiln is fixed")
    );
    assert_eq!(line_key("--- * ---"), None);
}

#[test]
fn a_line_of_an_earlier_document_is_a_quote_and_never_the_reverse() {
    let c = chapter();
    let classes = LineClasses::of(&c.source_documents, &declared(None));
    assert_eq!(
        classes.class("b", "> The kiln is fixed and fires again."),
        Some(LineClass::Quote)
    );
    // The earlier document's own line is its own, whatever was stored first.
    assert_eq!(
        classes.class("a", "The kiln is fixed and fires again."),
        None
    );
    assert_eq!(classes.class("b", "Good news."), None);
    // No author declared: a repeated sign-off is only a quote where it is later.
    assert_eq!(classes.class("a", "Ann, the studio"), None);
    assert_eq!(
        classes.class("c", "Ann, the studio"),
        Some(LineClass::Quote)
    );
}

#[test]
fn a_line_its_author_repeats_is_boilerplate_in_every_document_holding_it() {
    let c = chapter();
    let classes = LineClasses::of(&c.source_documents, &declared(Some("from")));
    assert_eq!(
        classes.class("a", "Ann, the studio"),
        Some(LineClass::Boilerplate)
    );
    assert_eq!(
        classes.class("c", "  Ann, the studio  "),
        Some(LineClass::Boilerplate)
    );
    // Another author's quote of it stays a quote, never boilerplate.
    assert_eq!(
        classes.class("b", "> The kiln is fixed and fires again."),
        Some(LineClass::Quote)
    );
}

#[test]
fn the_classes_do_not_depend_on_the_order_documents_are_indexed() {
    let c = chapter();
    let p = declared(Some("from"));
    let forward = LineClasses::of(&c.source_documents, &p);
    let backward = LineClasses::of(c.source_documents.iter().rev(), &p);
    assert_eq!(
        forward.digest(&c.source_documents),
        backward.digest(&c.source_documents)
    );
    assert!(forward.digest(&c.source_documents).contains("1:quote"));
}

#[test]
fn an_undated_document_quotes_nothing() {
    let c = input_chapter(&[
        row(1, "a", "One line here.", r#"{"date": "2026-01-02"}"#),
        row(2, "b", "One line here.", r#"{}"#),
    ]);
    let classes = LineClasses::of(&c.source_documents, &declared(None));
    assert_eq!(classes.class("b", "One line here."), None);
    assert_eq!(classes.class("a", "One line here."), None);
}
