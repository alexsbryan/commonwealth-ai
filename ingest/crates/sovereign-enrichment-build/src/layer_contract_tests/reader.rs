// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reader's own passes on the default path, beside C1-C6: each pass
//! ONTOLOGY_METHOD §Reading specifies is asked on the fixtures, and what it
//! must never ask is never asked.

use super::*;

/// The line a Locate question asks about.
fn asked_line(prompt: &ChatPrompt) -> &str {
    prompt
        .user
        .rsplit_once("\n\nLine ")
        .and_then(|(_, rest)| rest.split_once('"'))
        .and_then(|(_, rest)| rest.split_once("\"\n"))
        .map(|(text, _)| text)
        .expect("a Locate question names its line")
}

/// Prefill: every question about a document opens with its declared facts.
#[tokio::test]
async fn every_question_opens_with_its_documents_declared_facts() {
    for shape in SHAPES {
        let run = run(&Fixture::load(shape)).await;
        for p in run
            .prompts
            .iter()
            .filter(|p| p.phase_id.as_deref() != Some("resolve_select"))
        {
            assert!(
                p.user.starts_with("Declared facts of this document:\n"),
                "{shape}: {}",
                p.user
            );
        }
    }
}

/// Line classes: the mail fixture's reply quoting an earlier message, and the
/// greeting its author opens two messages with, are never sent to Locate,
/// while the quoted line is still asked in the message it comes from.
#[tokio::test]
async fn a_quoted_or_repeated_line_is_never_located() {
    let run = run(&Fixture::load("mail")).await;
    let locate: Vec<&ChatPrompt> = run
        .prompts
        .iter()
        .filter(|p| p.phase_id.as_deref() == Some("document_passes_locate"))
        .collect();
    let quoted = "The offer stands for thirty days; tell us if you would like to go ahead.";
    let in_doc = |p: &&ChatPrompt, id: &str| p.user.contains(&format!("\nid: {id}\n"));
    assert!(
        locate
            .iter()
            .any(|p| in_doc(p, "q2@alderpine.example") && asked_line(p) == quoted),
        "the quoted line is asked where it was written"
    );
    for p in &locate {
        let line = asked_line(p);
        assert!(
            !(in_doc(p, "q3@birchhall.example") && line.ends_with(quoted)),
            "a quote reached Locate: {line}"
        );
        assert_ne!(line, "Hello Tom,", "boilerplate reached Locate");
    }
}
