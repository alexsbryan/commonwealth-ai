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

/// The Locate questions of `run`.
fn locates(run: &Run) -> Vec<&ChatPrompt> {
    run.prompts
        .iter()
        .filter(|p| p.phase_id.as_deref() == Some("document_passes_locate"))
        .collect()
}

/// Whether `prompt` is about the document whose declared id is `id`.
fn in_doc(prompt: &ChatPrompt, id: &str) -> bool {
    prompt.user.contains(&format!("\nid: {id}\n"))
}

/// Line classes: the mail fixture's reply carries a line of the message it
/// answers under a mark, and it is never sent to Locate there, while it is
/// asked in the message it comes from. The greeting its author opens two
/// messages with is asked in both: repetition alone is no evidence.
#[tokio::test]
async fn a_quoted_line_is_never_located_and_a_repeated_one_is() {
    let run = run(&Fixture::load("mail")).await;
    let locate = locates(&run);
    let quoted = "The offer stands for thirty days; tell us if you would like to go ahead.";
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
    }
    for doc in ["q1@birchhall.example", "q3@birchhall.example"] {
        assert!(
            locate
                .iter()
                .any(|p| in_doc(p, doc) && asked_line(p) == "Hello Tom,"),
            "{doc}: the repeated greeting was never asked"
        );
    }
}

/// Review 2026-10-10 (note 2347f4c6), on the default path: a tracker act
/// written in the same words in two threads, and again in its own thread
/// after a reopen, is asked in each document and each becomes a placed
/// statement of its own document.
#[tokio::test]
async fn a_repeated_act_is_read_and_placed_in_every_document() {
    let mut f = Fixture::load("issues");
    let closed = "Closed this issue as resolved in the latest release.";
    for (id, thread, at, body) in [
        ("event-101-closed", 101, "2026-02-11T09:00:00Z", closed),
        ("event-102-closed", 102, "2026-02-12T09:00:00Z", closed),
        (
            "event-101-reopened",
            101,
            "2026-02-13T09:00:00Z",
            "Reopened this issue after a report from another user.",
        ),
        (
            "event-101-closed-again",
            101,
            "2026-02-14T09:00:00Z",
            closed,
        ),
    ] {
        let doc = json!({"id": id, "thread": thread, "kind": "event", "author": "jon",
            "author_association": "MEMBER", "created_at": at, "body": body});
        f.documents.push(doc.as_object().unwrap().clone());
    }
    let run = run(&f).await;
    let locate = locates(&run);
    let placed: BTreeSet<String> = run
        .atoms()
        .iter()
        .filter(|a| a["atom_type"] == "Claim" && a["data"]["subject"].is_string())
        .filter_map(|a| a["data"]["attributes"]["document_id"].as_str())
        .map(str::to_string)
        .collect();
    for doc in [
        "event-101-closed",
        "event-102-closed",
        "event-101-closed-again",
    ] {
        assert!(
            locate
                .iter()
                .any(|p| in_doc(p, doc) && asked_line(p) == closed),
            "{doc}: the act never reached Locate"
        );
        assert!(
            placed.contains(doc),
            "{doc}: no placed statement: {placed:?}"
        );
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

/// Review 2026-10-10 (note 2347f4c6): a subject field the reader reads (mail's
/// `order.piece`, pointed at in every statement) stays on the statement that
/// read it, as the claim's own fields do, through RESOLVE into the atlas: cited
/// in that statement's own document, with its source and precision. RESOLVE
/// writes no value of it on the record. Statements of one order that read it
/// differently each keep their own reading; a record holds a value of a read
/// field only by a declared fold.
#[tokio::test]
async fn a_read_subject_field_stays_on_its_statement_cited() {
    let f = Fixture::load("mail");
    let run = run(&f).await;
    let atoms = run.atoms();
    let folded = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let body_of: BTreeMap<&str, String> = f
        .documents
        .iter()
        .map(|d| {
            (
                d["id"].as_str().unwrap(),
                folded(d["body"].as_str().unwrap()),
            )
        })
        .collect();
    let mut statements_of: BTreeMap<&str, usize> = BTreeMap::new();
    for c in atoms
        .iter()
        .filter(|a| a["atom_type"] == "Claim" && a["data"]["claim_kind"] == "order_update")
    {
        let reading = &c["data"]["attributes"]["__document_read_subject_fields"]["piece"];
        assert!(
            reading.is_object(),
            "a statement lost its reading of its order's piece: {c}"
        );
        if reading["status"] != "supported" {
            assert!(
                reading["reason"].is_string(),
                "an unknown without why: {reading}"
            );
            continue;
        }
        assert!(
            super::contracts::says_its_precision(&reading["by"]),
            "{reading}"
        );
        let document = c["data"]["evidence"][0]["source_doc_id"]
            .as_str()
            .unwrap_or_else(|| panic!("a statement with no document: {c}"));
        let evidence = folded(
            reading["evidence"]
                .as_str()
                .unwrap_or_else(|| panic!("an uncited reading: {reading}")),
        );
        assert!(
            !evidence.is_empty() && body_of[document].contains(&evidence),
            "a reading not cited in its own document {document}: {reading}"
        );
        let order = c["data"]["subject"]
            .as_str()
            .unwrap_or_else(|| panic!("a statement placed on no order: {c}"));
        *statements_of.entry(order).or_default() += 1;
    }
    assert!(
        statements_of.values().any(|n| *n >= 2),
        "mail: no order has two statements' readings: {statements_of:?}"
    );
    for order in statements_of.keys() {
        let record = atoms
            .iter()
            .find(|a| a["atom_type"] == "Entity" && a["data"]["id"] == *order)
            .unwrap_or_else(|| panic!("no record {order}"));
        assert!(
            record["data"]["attributes"].get("piece").is_none(),
            "RESOLVE wrote a value on record {order}: {record}"
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
