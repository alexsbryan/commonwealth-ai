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

/// For each line of `body`, a document's text, what showed it carried and
/// from where.
fn classes_of(
    classes: &LineClasses,
    document: &str,
    body: &str,
) -> Vec<Option<(LineClass, String)>> {
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    classes
        .classes(document, &lines)
        .into_iter()
        .map(|c| c.map(|c| (c.class, c.from.to_string())))
        .collect()
}

/// Whether each line of `body`, a document's text, is kept from Locate.
fn classed(classes: &LineClasses, document: &str, body: &str) -> Vec<bool> {
    classes_of(classes, document, body)
        .iter()
        .map(Option::is_some)
        .collect()
}

/// a's two lines reach b as a marked passage (a bare marker line between
/// them), c as an unmarked one; d carries one of them under a mark.
fn quoting() -> crate::enrichment::pipeline::types::ChapterInput {
    input_chapter(&[
        row(
            1,
            "a",
            "Hello,\nThe kiln is fixed and fires again.\nWe open on Monday.\nAnn",
            r#"{"date": "2026-01-02"}"#,
        ),
        row(
            2,
            "b",
            "Good news.\n> The kiln is fixed and fires again.\n>\n> We open on Monday.\nBo",
            r#"{"date": "2026-01-05"}"#,
        ),
        row(
            3,
            "c",
            "Agreed.\nThe kiln is fixed and fires again.\nWe open on Monday.\nCy",
            r#"{"date": "2026-01-06"}"#,
        ),
        row(
            4,
            "d",
            "> We open on Monday.\nSee you there.",
            r#"{"date": "2026-01-07"}"#,
        ),
        // a's twin, dated alike: neither quotes the other, and a trace names
        // the same one of them however the documents are stored.
        row(
            5,
            "a2",
            "Hello,\nThe kiln is fixed and fires again.\nWe open on Monday.\nAnn",
            r#"{"date": "2026-01-02"}"#,
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

/// Review 2026-10-10 (note 2347f4c6): a tracker's act repeats in the same
/// words, in other threads and in its own after a reopen, and each one is an
/// act. Repetition is no evidence of a quote: every one is asked, and so is
/// the same words as one line of a longer document.
#[test]
fn independent_repeated_acts_are_asked_wherever_they_repeat() {
    let c = input_chapter(&[
        row(1, "e1", "closed as completed", r#"{"date": "2026-01-02"}"#),
        row(2, "e2", "closed as completed", r#"{"date": "2026-01-03"}"#),
        row(3, "e3", "reopened", r#"{"date": "2026-01-04"}"#),
        row(4, "e4", "closed as completed", r#"{"date": "2026-01-05"}"#),
        row(
            5,
            "c1",
            "Thanks, that fixed it.\nclosed as completed\nI will reopen it if it comes back.",
            r#"{"date": "2026-01-06"}"#,
        ),
    ]);
    let classes = LineClasses::of(&c.source_documents, &declared(None));
    for doc in ["e1", "e2", "e4"] {
        assert_eq!(
            classed(&classes, doc, "closed as completed"),
            [false],
            "{doc}"
        );
    }
    assert_eq!(
        classed(
            &classes,
            "c1",
            "Thanks, that fixed it.\nclosed as completed\nI will reopen it if it comes back."
        ),
        [false, false, false]
    );
}

/// A signature its author repeats is read where it first appears and is a
/// quote after; the author field suppresses nothing, so a passage an author
/// sends twice is always read once.
#[test]
fn a_signature_is_read_where_it_first_appears_and_quoted_after() {
    let c = input_chapter(&[
        row(
            1,
            "s1",
            "The kiln is fixed.\nAnn Lee\nThe Studio, 4 Mill Lane",
            r#"{"date": "2026-01-02", "from": "ann@x.example"}"#,
        ),
        row(
            2,
            "s2",
            "A new glaze arrived.\nAnn Lee\nThe Studio, 4 Mill Lane",
            r#"{"date": "2026-01-04", "from": "ann@x.example"}"#,
        ),
    ]);
    let classes = LineClasses::of(&c.source_documents, &declared(Some("from")));
    assert_eq!(
        classed(
            &classes,
            "s1",
            "The kiln is fixed.\nAnn Lee\nThe Studio, 4 Mill Lane"
        ),
        [false, false, false]
    );
    assert_eq!(
        classed(
            &classes,
            "s2",
            "A new glaze arrived.\nAnn Lee\nThe Studio, 4 Mill Lane"
        ),
        [false, true, true]
    );
}

#[test]
fn a_carried_passage_or_a_marked_line_is_a_quote_and_never_the_reverse() {
    let c = quoting();
    let classes = LineClasses::of(&c.source_documents, &declared(None));
    // The earlier document's own lines are its own, whatever was stored first.
    assert_eq!(
        classed(
            &classes,
            "a",
            "Hello,\nThe kiln is fixed and fires again.\nWe open on Monday.\nAnn"
        ),
        [false, false, false, false]
    );
    assert_eq!(
        classed(
            &classes,
            "b",
            "Good news.\n> The kiln is fixed and fires again.\n>\n> We open on Monday.\nBo"
        ),
        [false, true, false, true, false]
    );
    assert_eq!(
        classed(
            &classes,
            "c",
            "Agreed.\nThe kiln is fixed and fires again.\nWe open on Monday.\nCy"
        ),
        [false, true, true, false]
    );
    assert_eq!(
        classed(&classes, "d", "> We open on Monday.\nSee you there."),
        [true, false]
    );
    // What showed each one carried: d's line has no neighbour a carries.
    assert_eq!(
        classes_of(
            &classes,
            "c",
            "Agreed.\nThe kiln is fixed and fires again.\nWe open on Monday.\nCy"
        )[1],
        Some((LineClass::Passage, "a".to_string()))
    );
    assert_eq!(
        classes_of(&classes, "d", "> We open on Monday.\nSee you there.")[0],
        Some((LineClass::Marked, "a".to_string()))
    );
}

#[test]
fn a_mark_is_what_a_line_is_written_under_before_its_first_letter_or_digit() {
    assert_eq!(mark("> - The kiln"), ">-");
    assert_eq!(mark("   The kiln"), "");
    assert_eq!(mark("1. Fire the kiln"), "");
}

#[test]
fn the_classes_do_not_depend_on_the_order_documents_are_indexed() {
    let c = quoting();
    let p = declared(None);
    let forward = LineClasses::of(&c.source_documents, &p);
    let backward = LineClasses::of(c.source_documents.iter().rev(), &p);
    assert_eq!(
        forward.digest(&c.source_documents),
        backward.digest(&c.source_documents)
    );
    assert!(forward.digest(&c.source_documents).contains("2:"));
    // The document a trace names as the source is order-free as well.
    for d in &c.source_documents {
        let body = d.raw_body();
        assert_eq!(
            classes_of(&forward, d.key(), &body),
            classes_of(&backward, d.key(), &body),
            "{}",
            d.key()
        );
    }
}

#[test]
fn an_undated_document_quotes_nothing_and_is_quoted_by_nothing() {
    let c = input_chapter(&[
        row(
            1,
            "a",
            "One line here.\nAnother line.",
            r#"{"date": "2026-01-02"}"#,
        ),
        row(2, "b", "> One line here.\n> Another line.", r#"{}"#),
        row(3, "u", "Only u says this.\nAnd this.", r#"{}"#),
        row(
            4,
            "d",
            "> Only u says this.\n> And this.",
            r#"{"date": "2026-01-04"}"#,
        ),
    ]);
    let classes = LineClasses::of(&c.source_documents, &declared(None));
    assert_eq!(
        classed(&classes, "b", "> One line here.\n> Another line."),
        [false, false]
    );
    assert_eq!(
        classed(&classes, "d", "> Only u says this.\n> And this."),
        [false, false]
    );
}
