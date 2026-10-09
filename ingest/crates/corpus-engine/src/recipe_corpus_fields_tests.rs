// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate --corpus`: declared metadata fields against the documents
//! the recipe's own extractor reads. Each verdict has a red input here.

use std::sync::Arc;

use super::*;

/// A jsonl recipe over `path` whose ontology names `thread_field` as the
/// thread stamp, `date_field` as the date, and `author` as a metadata source.
fn recipe(path: &Path, date_field: &str, thread_field: &str) -> Recipe {
    let toml = format!(
        r#"
[corpus]
id = "fields"
name = "Fields"

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "jsonl"
content_field = "body"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
type = "atlas"

[enrichment.ontology]
version = 1
[[enrichment.ontology.types]]
name = "writer"
kind = "entity"
attributes = [{{ name = "handle", type = "text" }}]
identity = ["handle"]
source = {{ metadata = ["author"], attributes = {{ handle = "value" }} }}
[[enrichment.ontology.types]]
name = "report"
kind = "claim"
force = "assertive"
[enrichment.ontology.change]
document = {{ date = "{date_field}", thread = "{thread_field}" }}
"#,
        path.display()
    );
    Recipe::from_toml(&toml).expect("recipe loads")
}

fn engine(dir: &Path) -> CorpusEngine {
    let embed: crate::types::EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0; 4]) }));
    CorpusEngine::new(dir.join("r"), dir.join("i"), embed)
}

/// Three records: every one carries `thread` and `created`, one lacks
/// `author`, one carries a date no reader parses.
fn corpus(dir: &Path) -> std::path::PathBuf {
    let p = dir.join("docs.jsonl");
    std::fs::write(
        &p,
        concat!(
            r#"{"id":"1","thread":7,"created":"2024-01-02T03:04:05Z","author":"ann","body":"one"}"#,
            "\n",
            r#"{"id":"2","thread":7,"created":"2024-01-03","author":"bo","body":"two"}"#,
            "\n",
            r#"{"id":"3","thread":8,"created":"last tuesday","body":"three"}"#,
            "\n",
        ),
    )
    .unwrap();
    p
}

#[test]
fn a_declared_field_no_document_carries_is_an_error_naming_the_ones_they_do() {
    let dir = tempfile::tempdir().unwrap();
    let path = corpus(dir.path());
    let r = check(
        &engine(dir.path()),
        &recipe(&path, "created", "issue_number"),
        &path,
    );
    let e = r
        .errors
        .iter()
        .find(|e| e.contains("`issue_number`"))
        .unwrap_or_else(|| panic!("no error for issue_number in {:?}", r.errors));
    assert!(e.contains("change.document.thread"), "{e}");
    assert!(e.contains("none of the 3 documents"), "{e}");
    assert!(e.contains("`thread` (3)"), "{e}");
    assert!(e.contains("`author` (2)"), "{e}");
}

#[test]
fn a_stamp_some_documents_lack_is_a_warning_and_a_source_field_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = corpus(dir.path());
    // `author` is both the thread stamp here and the writer's source field.
    let r = check(&engine(dir.path()), &recipe(&path, "created", "author"), &path);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("`author` (change.document.thread) is missing from 1 of 3")),
        "{:?}",
        r.warnings
    );
    assert!(
        !r.warnings.iter().any(|w| w.contains("source.metadata")),
        "{:?}",
        r.warnings
    );
    assert!(!r.errors.iter().any(|e| e.contains("`author`")), "{:?}", r.errors);
}

#[test]
fn a_stamp_the_one_reader_cannot_read_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = corpus(dir.path());
    let r = check(
        &engine(dir.path()),
        &recipe(&path, "created", "thread"),
        &path,
    );
    let e = r
        .errors
        .iter()
        .find(|e| e.contains("`created`"))
        .unwrap_or_else(|| panic!("no error for created in {:?}", r.errors));
    assert!(e.contains("1 of the 3 documents"), "{e}");
    assert!(e.contains("last tuesday"), "{e}");
}

#[test]
fn every_carried_field_is_listed_even_with_nothing_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let path = corpus(dir.path());
    let r = check(
        &engine(dir.path()),
        &recipe(&path, "created", "thread"),
        &path,
    );
    let note = r.notes.first().expect("a fields note");
    assert!(note.contains("3 documents"), "{note}");
    for f in ["`id` (3)", "`thread` (3)", "`created` (3)", "`author` (2)"] {
        assert!(note.contains(f), "{f} missing from {note}");
    }
}

#[test]
fn a_corpus_with_no_documents_judges_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.jsonl");
    std::fs::write(&path, "").unwrap();
    let r = check(
        &engine(dir.path()),
        &recipe(&path, "created", "thread"),
        &path,
    );
    assert!(
        r.errors.iter().any(|e| e.contains("read no documents")),
        "{:?}",
        r.errors
    );
    assert!(r.notes.is_empty(), "{:?}", r.notes);
}

#[test]
fn records_without_the_named_content_field_are_named_as_the_cause() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("docs.jsonl");
    std::fs::write(&path, "{\"id\":\"1\",\"prose\":\"words\"}\n").unwrap();
    let r = check(&engine(dir.path()), &recipe(&path, "created", "thread"), &path);
    assert!(
        r.errors
            .iter()
            .any(|e| e.contains("`extract.content_field = \"body\"`")),
        "{:?}",
        r.errors
    );
}
