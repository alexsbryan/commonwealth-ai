//! E3: statements held in their document are settled after the last one.

use serde_json::json;

use super::super::settle::held_in;
use super::*;

/// Records x ("Fire downtown") and y ("Flood uptown"), then statement z,
/// held between them in d1 (the model's choice on y split against the
/// proposed answer on x at equal weight). Returns the resolver and d1's
/// resolution.
async fn held_between_x_and_y() -> (Resolver, DocumentResolution) {
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
    let (infer, _) = scripted(vec![json!({"A": 0.1, "B": 0.8, "0": 0.1})]);
    let r = res
        .resolve_document(
            &barred(),
            doc("d1", B),
            &[stmt("z", B, "Fire downtown", 0, &[])],
            &[similar("x", 0.6), similar("y", 0.5)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(labels(&r), ["held"]);
    (res, r)
}

const B: &str = "Fire downtown, again.";

fn statements() -> Vec<Statement> {
    vec![stmt("z", B, "Fire downtown", 0, &[])]
}

/// Asked once more over the records it was held between, the model's choice
/// is weighed again with the whole corpus's weights; unsettled at the bar,
/// the most probable decides, so it is not left held.
#[tokio::test]
async fn a_held_statement_is_settled_after_the_last_document_by_the_most_probable() {
    let (mut res, r) = held_between_x_and_y().await;
    let mut held = Vec::new();
    held_in(0, &r, &mut held);
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
        ("weighed", Some("y"))
    );
    let Outcome::Decided(Decision::Weighed { posterior, .. }) = &o.outcome else {
        unreachable!()
    };
    // Below the bar of .5: no later evidence can come, so the most probable decides.
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
    let (mut res, r) = held_between_x_and_y().await;
    let mut held = Vec::new();
    held_in(0, &r, &mut held);
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
    let (mut res, r) = held_between_x_and_y().await;
    let mut held = Vec::new();
    held_in(0, &r, &mut held);
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
