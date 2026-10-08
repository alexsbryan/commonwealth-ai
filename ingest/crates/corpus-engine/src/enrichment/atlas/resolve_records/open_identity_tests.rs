use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};

use super::*;
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

const BODY: &str = "The two references occupy the same passage.";

fn criterion() -> Criterion {
    Criterion {
        type_name: "case".into(),
        description: "A support case".into(),
        same_when: Some("the same case".into()),
        keys: Vec::new(),
        evidential: Vec::new(),
        bar: None,
        model_choice: None,
        reasoned_choice: None,
        proposed_answer: None,
        necessary: Vec::new(),
    }
}

fn statement(id: &str) -> Statement {
    Statement {
        id: id.into(),
        start: 0,
        end: BODY.len(),
        keys: BTreeMap::new(),
    }
}

fn document() -> Document<'static> {
    Document {
        id: "same-document",
        title: None,
        body: BODY,
        stamps: &[],
    }
}

fn model_answer(answer: Value) -> InferenceFn {
    let serialized = answer.to_string();
    Arc::new(move |_: &ChatPrompt, _| {
        let serialized = serialized.clone();
        Box::pin(async move { Ok(serialized) })
    })
}

async fn resolve(local_refs: &[&str], answer: Value) -> (Vec<String>, Vec<&'static str>) {
    let ids: Vec<String> = local_refs
        .iter()
        .map(|local_ref| format!("same-document@0..{}#\"{local_ref}\"", BODY.len()))
        .collect();
    let statements: Vec<Statement> = ids.iter().map(|id| statement(id)).collect();
    let inference = model_answer(answer);
    let mut resolver = Resolver::default();
    let result = resolver
        .resolve_document(
            &criterion(),
            document(),
            &statements,
            &[],
            Answerer::Model(&inference),
        )
        .await;
    let records = resolver
        .records()
        .iter()
        .map(|record| record.id.clone())
        .collect();
    let outcomes = result
        .outcomes
        .iter()
        .map(|outcome| outcome.outcome.label())
        .collect();
    (records, outcomes)
}

#[tokio::test]
async fn independent_open_decisions_on_equal_spans_allocate_two_records() {
    let answer = json!({
        "particulars": [
            {"same_as": "none", "mentions": [{"statement": "s0", "cite": BODY}]},
            {"same_as": "none", "mentions": [{"statement": "s1", "cite": BODY}]}
        ]
    });
    let (records, outcomes) = resolve(&["local-a", "local-b"], answer).await;

    assert_eq!(outcomes, ["opened", "opened"]);
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.into_iter().collect::<BTreeSet<_>>(),
        [
            format!("same-document@0..{}#\"local-a\"", BODY.len()),
            format!("same-document@0..{}#\"local-b\"", BODY.len()),
        ]
        .into_iter()
        .collect()
    );
}

#[tokio::test]
async fn a_linked_open_decision_allocates_one_order_stable_record() {
    let linked = json!({
        "particulars": [
            {"same_as": "none", "mentions": [
                {"statement": "s0", "cite": BODY},
                {"statement": "s1", "cite": BODY}
            ]}
        ]
    });
    let (forward, forward_outcomes) = resolve(&["local-z", "local-a"], linked.clone()).await;
    let (reverse, reverse_outcomes) = resolve(&["local-a", "local-z"], linked).await;

    assert_eq!(forward_outcomes, ["opened", "opened"]);
    assert_eq!(reverse_outcomes, ["opened", "opened"]);
    assert_eq!(forward.len(), 1);
    assert_eq!(reverse.len(), 1);
    assert_eq!(forward, reverse);
    assert_eq!(
        forward[0],
        format!("same-document@0..{}#\"local-a\"", BODY.len())
    );
}
