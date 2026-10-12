// SPDX-License-Identifier: AGPL-3.0-or-later
//! A protocol's transitions on the default path: the mail fixture's order
//! stages declare who may make each move, by a path over the message's
//! sender, and from which stages.

use super::*;

/// The mail fixture whose order stages are moves: the customer asks,
/// confirms and cancels; only our side quotes, and only what was asked.
fn with_movers() -> Fixture {
    let mut f = Fixture::load("mail");
    let mut recipe = f.recipe.replace(
        "[[enrichment.ontology.folds]]\nid = \"party_of_message\"",
        "[[enrichment.ontology.paths]]\nid = \"we_write\"\npath = \"document / from / employer [ours]\"\n\n\
         [[enrichment.ontology.paths]]\nid = \"customer_writes\"\npath = \"document / from / employer [!ours]\"\n\n\
         [[enrichment.ontology.folds]]\nid = \"party_of_message\"",
    );
    for (stage, moves) in [
        ("asked", "by = \"customer_writes\""),
        (
            "quoted",
            "by = \"we_write\"\nfrom = [\"asked\", \"quoted\"]",
        ),
        ("confirmed", "by = \"customer_writes\"\nfrom = [\"quoted\"]"),
        ("cancelled", "by = \"customer_writes\""),
    ] {
        let when = format!("when = {{ stage = \"{stage}\" }}\n");
        assert!(recipe.contains(&when), "{when}");
        recipe = recipe.replace(&when, &format!("{when}{moves}\n"));
    }
    f.recipe = recipe;
    f
}

/// A stage our side states as the customer's move is evidence, never the
/// order's stage: every served stage cites a customer's message, and our own
/// messages' statements stay in the history as unauthorized.
#[tokio::test]
async fn a_served_stage_is_a_move_its_maker_may_make() {
    let f = with_movers();
    let ours = |doc: &str| {
        f.documents
            .iter()
            .find(|d| d["id"] == doc)
            .and_then(|d| d["from"].as_str())
            .is_some_and(|from| from.contains("@alderpine.example>"))
    };
    let run = run(&f).await;
    let lines: Vec<Value> = run
        .lines(DERIVED_FILE)
        .into_iter()
        .filter(|l| l["type"] == "order" && l["attribute"] == "stage")
        .collect();
    let (mut served, mut refused) = (0, 0);
    for line in &lines {
        for h in line["protocol"]["history"].as_array().into_iter().flatten() {
            let moved = h["disposition"] == "selected";
            if !moved && h["disposition"] != "unauthorized" {
                continue;
            }
            let doc = h["document"]
                .as_str()
                .unwrap_or_else(|| panic!("a decided report names no document: {h}"));
            if !moved {
                refused += 1;
                assert!(ours(doc), "a customer's statement was refused: {h}");
            } else {
                assert!(!ours(doc), "our own statement moved the order: {h}");
            }
        }
        if line["outcome"] == "decided" {
            served += 1;
            for doc in line["documents"].as_array().unwrap() {
                assert!(!ours(doc.as_str().unwrap()), "{line}");
            }
        }
    }
    assert!(served > 0, "no order holds a stage: {lines:?}");
    assert!(refused > 0, "no statement of ours was refused: {lines:?}");
}
