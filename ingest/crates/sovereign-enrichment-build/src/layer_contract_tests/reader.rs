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

/// Every pass the reader's plan has is asked on the fixtures (mail: Point
/// on an open claim and subject field and a quantity and a time, Mention and
/// Pick on a reference to a header-sourced type; issues and news: Point), and
/// still asked when every type and attribute is renamed (C6's fixture).
#[tokio::test]
async fn every_pass_is_asked_and_still_asked_under_rename() {
    let phases = |run: &Run| -> BTreeSet<String> {
        run.prompts
            .iter()
            .filter_map(|p| p.phase_id.clone())
            .collect()
    };
    let mail = Fixture::load("mail");
    let (renamed, _) = super::contracts::renamed(&mail);
    for (name, f) in [("mail", mail), ("mail renamed", renamed)] {
        let asked = phases(&run(&f).await);
        for phase in [
            "document_passes_locate",
            "document_passes_mention",
            "document_passes_choose",
            "document_passes_point",
            "document_passes_pick",
        ] {
            assert!(
                asked.contains(phase),
                "{name}: `{phase}` never asked: {asked:?}"
            );
        }
    }
    for shape in ["issues", "news"] {
        let asked = phases(&run(&Fixture::load(shape)).await);
        assert!(
            asked.contains("document_passes_point"),
            "{shape}: {asked:?}"
        );
    }
}

/// A read reference's value carries its source and precision (C3) and is one
/// of its own document's candidates: a record its header fields name, or
/// words of its own text; never a record of another document.
#[tokio::test]
async fn a_picked_reference_is_cited_in_its_own_document() {
    let f = Fixture::load("mail");
    let run = run(&f).await;
    let mut picked = 0;
    for a in run.atoms() {
        let fields = &a["data"]["attributes"]["__document_read_fields"];
        let Some(field) = fields.get("named_customer") else {
            continue;
        };
        if field["status"] != "supported" {
            continue;
        }
        picked += 1;
        assert_eq!(field["by"]["source"], "reader_pick", "{field}");
        assert_eq!(field["by"]["precision"], "unmeasured", "{field}");
        // Our own side is in the declared exclusion set `ours`.
        assert!(
            !field["value"].as_str().unwrap().contains("alderpine"),
            "an excluded record was offered: {field}"
        );
    }
    assert!(picked > 0, "no reference was picked on mail");
}
