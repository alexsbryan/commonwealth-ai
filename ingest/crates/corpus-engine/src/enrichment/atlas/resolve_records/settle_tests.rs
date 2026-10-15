//! E3: statements held in their document are settled after the last one.

use serde_json::json;

use super::super::settle::HeldStatement;
use super::*;

/// Records x ("Fire downtown") and y ("Flood uptown"), then statement z of
/// d1, held between them. A statement the model was asked about is never
/// held (its choice decides, `select.rs`), so the hold is the one a
/// statement no candidate was offered for gets when its sources split.
async fn held_between_x_and_y() -> (Resolver, Vec<HeldStatement>) {
    let mut res = fire_downtown(weights(&[
        ("model_choice", 0.6, 0.3),
        ("proposed_answer", 0.6, 0.3),
    ]))
    .await;
    let y = "Flood uptown.";
    res.resolve_document(
        &criterion(&[]),
        doc("d0b", y),
        &[stmt("y", y, "Flood uptown", 0, &[])],
        &[],
        Answerer::Proposed,
    )
    .await;
    let held = HeldStatement {
        document: 0,
        statement: "z".into(),
        held: Held {
            alternatives: vec![("x".into(), 0.4), ("y".into(), 0.4)],
            none: 0.2,
            sources: Vec::new(),
            read: BTreeMap::new(),
            supplied: BTreeMap::new(),
        },
    };
    (res, vec![held])
}

const B: &str = "Fire downtown, again.";

fn statements() -> Vec<Statement> {
    vec![stmt("z", B, "Fire downtown", 0, &[])]
}

/// Asked once more over the records it was held between, the model's choice
/// decides, the posterior the whole corpus's weights give it kept beside it,
/// so it is not left held.
#[tokio::test]
async fn a_held_statement_is_settled_after_the_last_document_by_the_models_choice() {
    let (mut res, held) = held_between_x_and_y().await;
    assert_eq!(held.len(), 1);
    let s = statements();
    let documents = [(doc("d1", B), s.as_slice())];
    let (infer, seen) = scripted(vec![json!({"A": 0.1, "B": 0.8, "0": 0.1})]);
    let settled = res
        .settle(&barred(), &documents, held, Answerer::Select(&infer))
        .await;
    assert_eq!(settled.len(), 1);
    let (k, r) = &settled[0];
    assert!(*k == 0 && r.settles && r.calls == 1);
    let o = &r.outcomes[0];
    assert_eq!(
        (o.outcome.label(), o.outcome.record()),
        ("chosen", Some("y"))
    );
    let Outcome::Decided(Decision::Chosen { posterior, .. }) = &o.outcome else {
        unreachable!()
    };
    // The model's choice decides; the weighed posterior, below the bar of .5, is kept beside it.
    assert!(*posterior < 0.5, "{posterior}");
    assert!(o.choice.is_some() && !o.by.is_empty());
    assert!(res
        .records()
        .iter()
        .any(|rec| rec.id == "y" && rec.statements.contains(&"z".to_string())));
    // One question, over the two records it was held between only.
    let prompts = seen.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].phase_id.as_deref(), Some("resolve_select"));
    assert!(prompts[0].user.contains("\nA ") && prompts[0].user.contains("\nB "));
    assert!(!prompts[0].user.contains("\nC "));
}

/// A settled statement no source raises any record for opens its own.
#[tokio::test]
async fn a_held_statement_the_model_puts_in_none_opens_its_own_record() {
    let (mut res, held) = held_between_x_and_y().await;
    let s = statements();
    let documents = [(doc("d1", B), s.as_slice())];
    let (infer, _) = scripted(vec![json!({"A": 0.05, "B": 0.05, "0": 0.9})]);
    let settled = res
        .settle(&barred(), &documents, held, Answerer::Select(&infer))
        .await;
    let o = &settled[0].1.outcomes[0];
    assert_eq!(
        (o.outcome.label(), o.outcome.record()),
        ("opened", Some("z"))
    );
    // Opened because the model put it in none of the two it was asked about.
    assert_eq!(settled[0].1.calls, 1);
    assert_eq!(settled[0].1.candidates, ["x", "y"]);
    assert_eq!(res.records().len(), 3);
}

/// A call that answers no distribution refuses the statement, never a default.
#[tokio::test]
async fn a_refused_settling_call_refuses_the_statement() {
    let (mut res, held) = held_between_x_and_y().await;
    let s = statements();
    let documents = [(doc("d1", B), s.as_slice())];
    let (infer, _) = scripted(vec![json!("B")]);
    let settled = res
        .settle(&barred(), &documents, held, Answerer::Select(&infer))
        .await;
    assert_eq!(
        settled[0].1.outcomes[0].outcome.label(),
        "refused:no_answer"
    );
    assert_eq!(res.records().len(), 2);
}
