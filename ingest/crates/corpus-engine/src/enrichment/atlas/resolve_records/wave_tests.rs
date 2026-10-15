// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use oicp_types::forced_choice;

use super::*;
use crate::enrichment::asker::chat_key;

fn question(user: &str, labels: &[&str]) -> ChatPrompt {
    ChatPrompt::new("system", user)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id("document_passes_locate")
        .with_temperature(0.0)
}

fn three() -> Vec<ChatPrompt> {
    vec![
        question("Document\n1 a\n2 b\n\nLine 1: \"a\"", &["A", "0"]),
        question("Document\n1 a\n2 b\n\nLine 2: \"b\"", &["A", "B", "0"]),
        question("Document\n1 a\n2 b\n\nLine 2: \"b\" again", &["A", "0"]),
    ]
}

/// A scripted oracle: answers every single forced choice by putting 0.75 on
/// the label its user turn's last character names, if it is one, else on the
/// first. Counts what it is asked.
fn oracle(asked: Arc<Mutex<Vec<String>>>) -> InferenceFn {
    Arc::new(move |p: &ChatPrompt, _| {
        let asked = Arc::clone(&asked);
        let p = p.clone();
        Box::pin(async move {
            asked.lock().unwrap().push(p.user.clone());
            let labels = forced_choice::candidates(p.response_schema.as_ref().unwrap())
                .expect("the oracle answers single forced choices only");
            let mut d = BTreeMap::new();
            for l in &labels {
                d.insert(l.clone(), 0.25f32 / (labels.len() - 1).max(1) as f32);
            }
            let last = p.user.chars().last().map(String::from).unwrap_or_default();
            let top = if labels.contains(&last) {
                last
            } else {
                labels[0].clone()
            };
            d.insert(top, 0.75);
            Ok(forced_choice::answer(&d))
        })
    })
}

#[test]
fn a_wave_splits_back_into_the_very_prompts_and_keys_it_was_made_of() {
    let qs = three();
    let wave = compose(&qs).unwrap();
    assert_eq!(wave.user, "Document\n1 a\n2 b\n\nLine ");
    assert!(forced_choice::wave_branches(wave.response_schema.as_ref().unwrap()).is_some());
    let back = singles(&wave).unwrap().unwrap();
    assert_eq!(back.len(), qs.len());
    for (a, b) in back.iter().zip(&qs) {
        assert_eq!(chat_key(a, None), chat_key(b, None));
    }
    assert!(singles(&qs[0]).is_none(), "a single question is not a wave");
}

#[test]
fn questions_that_differ_beyond_their_user_turn_make_no_wave() {
    let mut qs = three();
    qs[1] = qs[1].clone().with_phase_id("document_passes_mention");
    assert!(compose(&qs).is_err());
    let mut qs = three();
    let mut titled = forced_choice::schema(&["A", "0"]);
    titled["title"] = serde_json::json!("extra");
    qs[2].response_schema = Some(titled);
    assert!(
        compose(&qs).is_err(),
        "a body a single key would not reproduce"
    );
    assert!(compose(&[]).is_err());
}

#[test]
fn the_split_point_never_cuts_a_character() {
    let qs = vec![
        question("doc é1", &["A", "0"]),
        question("doc è2", &["A", "0"]),
    ];
    let wave = compose(&qs).unwrap();
    assert_eq!(wave.user, "doc ");
    let back = singles(&wave).unwrap().unwrap();
    assert_eq!(back[0].user, "doc é1");
    assert_eq!(back[1].user, "doc è2");
}

#[tokio::test]
async fn a_wave_answers_each_question_as_it_would_be_answered_alone() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let infer = one_by_one(oracle(Arc::clone(&asked)));
    let qs = three();
    let labels: Vec<Vec<&str>> = vec![vec!["A", "0"], vec!["A", "B", "0"], vec!["A", "0"]];
    let items: Vec<String> = (1..=3).map(|i| format!("q{i}")).collect();
    let waved = decision_wave(&infer, &qs, &labels, "doc", &items).await;
    let mut alone = Vec::new();
    for ((q, l), item) in qs.iter().zip(&labels).zip(&items) {
        alone.push(decision_call(&infer, q, l, "doc", item).await);
    }
    assert_eq!(waved, alone);
    // The wave went out as one request the adapter split, three questions
    // each asked once by the wave and once alone.
    assert_eq!(asked.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn a_port_that_answers_no_wave_refuses_every_branch_by_name() {
    let single_only: InferenceFn =
        Arc::new(|_, _| Box::pin(async { Ok("{\"A\":1.0,\"0\":0.0}".to_string()) }));
    let qs = three();
    let labels: Vec<Vec<&str>> = vec![vec!["A", "0"], vec!["A", "B", "0"], vec!["A", "0"]];
    let items: Vec<String> = (1..=3).map(|i| format!("q{i}")).collect();
    let out = decision_wave(&single_only, &qs, &labels, "doc", &items).await;
    assert_eq!(out.len(), 3);
    for r in out {
        match r {
            Err(Refusal::NoAnswer { reason }) => assert!(reason.contains("not a wave answer")),
            other => panic!("expected a named refusal, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn one_failed_branch_is_refused_alone() {
    let flaky: InferenceFn = one_by_one(Arc::new(|p: &ChatPrompt, _| {
        let p = p.clone();
        Box::pin(async move {
            if p.user.ends_with("again") {
                Err(crate::error::Error::Extraction("no answer".into()))
            } else {
                Ok("{\"0\":0.5,\"A\":0.25,\"B\":0.25}".to_string())
            }
        })
    }));
    let qs = three();
    let labels: Vec<Vec<&str>> = vec![vec!["A", "0"], vec!["A", "B", "0"], vec!["A", "0"]];
    let items: Vec<String> = (1..=3).map(|i| format!("q{i}")).collect();
    let out = decision_wave(&flaky, &qs, &labels, "doc", &items).await;
    assert!(out[0].is_ok() && out[1].is_ok());
    assert!(matches!(&out[2], Err(Refusal::NoAnswer { reason }) if reason.contains("no answer")));
}
