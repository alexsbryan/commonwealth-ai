// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::super::tests::{input_chapter, row};
use super::super::{DocumentReadField, DocumentReadOutcome, DocumentReadStatus};
use super::*;
use crate::enrichment::atlas::resolve_records::{wave_singles, wave_text};
use crate::enrichment::ontology::OntologyV1;
use crate::enrichment::pipeline::types::ChatPrompt;

/// A mailbox declaration in the shape of Ward's, its names given: a deal RESOLVE
/// decides, a stage read as an act, a company a reference targets, a
/// commitment about a person the headers name.
fn mail(n: [&str; 6]) -> OntologyPolicies {
    let [deal, stage_update, company, commitment, person, counterparty] = n;
    let toml = format!(
        r#"
[[types]]
name = "{person}"
kind = "entity"
identity = ["email"]
source = {{ metadata = ["from"], attributes = {{ email = "address" }} }}
attributes = [{{ name = "email", type = "text" }}]

[[types]]
name = "{company}"
kind = "entity"
attributes = [{{ name = "domain", type = "text" }}]

[[types]]
name = "{deal}"
kind = "entity"
description = "one sale or purchase with one counterparty"
identity_criterion = "same transaction with the same counterparty"
attributes = [
  {{ name = "{counterparty}", type = "ref", of = "{company}" }},
  {{ name = "amount", type = "quantity", unit = "USD" }},
  {{ name = "term", type = "text", values = ["a fixed term shorter than a year", "a fixed term of a year or more"] }},
  {{ name = "stage", type = "text", values = ["lead", "proposal"], derived = "deal_stage" }},
]

[[types]]
name = "{stage_update}"
kind = "claim"
force = "assertive"
subject = "{deal}"
attributes = [{{ name = "stage", type = "text", values = ["lead", "proposal"] }}]

[[types]]
name = "{commitment}"
kind = "claim"
force = "commissive"
subject = "{person}"
attributes = [{{ name = "due", type = "time" }}]

[[folds]]
id = "deal_stage"
by = "protocol"
from = ["^subject"]

[folds.protocol]
identity = "stage"

[[folds.protocol.rules]]
id = "lead"
claim_kind = "{stage_update}"
state = "lead"
when = {{ stage = "lead" }}

[[folds.protocol.rules]]
id = "proposal"
claim_kind = "{stage_update}"
state = "proposal"
when = {{ stage = "proposal" }}
"#
    );
    toml::from_str::<OntologyV1>(&toml).unwrap().into_policies()
}

const NAMES: [&str; 6] = [
    "deal",
    "stage_update",
    "company",
    "commitment",
    "person",
    "counterparty",
];

fn shape(
    policies: &OntologyPolicies,
) -> (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
) {
    let plan = Plan::of(policies);
    let levels = Levels::of(&plan);
    let names = |v: &[&OntologyTypeDecl]| v.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    let kinds = |v: &[usize]| {
        v.iter()
            .map(|&k| plan.kinds[k].decl.name.clone())
            .collect::<Vec<_>>()
    };
    (
        names(&levels.decided),
        names(&levels.mentioned),
        kinds(&levels.at_document),
        kinds(&levels.per_particular),
        kinds(&levels.acts),
    )
}

#[test]
fn the_levels_are_compiled_from_the_contract_and_renaming_renames_them_only() {
    let (decided, mentioned, at_document, per_particular, acts) = shape(&mail(NAMES));
    assert_eq!(decided, ["deal"]);
    assert_eq!(mentioned, ["company"]);
    assert_eq!(at_document, ["commitment"]);
    assert!(per_particular.is_empty());
    assert_eq!(acts, ["stage_update"]);
    let renamed = shape(&mail(["t1", "t2", "t3", "t4", "t5", "a1"]));
    assert_eq!(
        renamed,
        (
            vec!["t1".into()],
            vec!["t3".into()],
            vec!["t4".into()],
            vec![],
            vec!["t2".into()]
        )
    );
}

#[test]
fn a_kind_about_a_decided_type_is_enumerated_per_particular() {
    // GVC's shape: a report claim about a happening RESOLVE decides, no protocol.
    let policies = toml::from_str::<OntologyV1>(
        r#"
[[types]]
name = "happening"
kind = "event"
identity_criterion = "same happening"
attributes = [{ name = "kind", type = "text", values = ["a", "b"] }]

[[types]]
name = "report"
kind = "claim"
force = "assertive"
subject = "happening"
"#,
    )
    .unwrap()
    .into_policies();
    let (decided, mentioned, at_document, per_particular, acts) = shape(&policies);
    assert_eq!(decided, ["happening"]);
    assert!(mentioned.is_empty() && at_document.is_empty() && acts.is_empty());
    assert_eq!(per_particular, ["report"]);
}

/// The options a question offers, `(label, text)`, read after the document.
fn options(user: &str) -> Vec<(String, String)> {
    let tail = &user[user.rfind(">>>\n\n").map_or(0, |i| i + 5)..];
    tail.lines()
        .filter_map(|l| {
            let (label, rest) = l.split_once(' ')?;
            let is_label =
                label == "0" || (label.len() == 1 && label.chars().all(|c| c.is_ascii_uppercase()));
            is_label.then(|| (label.to_string(), rest.to_string()))
        })
        .collect()
}

/// A model that answers each question by `rules` (the first whose question
/// text the prompt holds; its option the one whose text holds the second, or
/// equals it after `=`), "0" when no rule fits, and records every request.
fn oracle(
    rules: &'static [(&'static str, &'static str)],
) -> (crate::InferenceFn, Arc<Mutex<Vec<ChatPrompt>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    let answer = |q: &ChatPrompt| -> String {
        let opts = options(&q.user);
        let question = &q.user[q.user.rfind(">>>\n\n").map_or(0, |i| i + 5)..];
        let chosen = rules
            .iter()
            .find(|(asks, _)| question.contains(asks))
            .and_then(|(_, pick)| {
                opts.iter().find(|(_, text)| match pick.strip_prefix('=') {
                    Some(exact) => text == exact,
                    None => text.contains(pick),
                })
            })
            .map_or_else(
                || {
                    if opts.iter().any(|(l, _)| l == "0") {
                        "0".into()
                    } else {
                        opts[0].0.clone()
                    }
                },
                |(l, _)| l.clone(),
            );
        let n = opts.len() as f64;
        let dist: serde_json::Map<String, Value> = opts
            .iter()
            .map(|(l, _)| {
                (
                    l.clone(),
                    json!(if *l == chosen {
                        0.9
                    } else {
                        0.1 / (n - 1.0).max(1.0)
                    }),
                )
            })
            .collect();
        Value::Object(dist).to_string()
    };
    let f: crate::InferenceFn = Arc::new(move |p: &ChatPrompt, _| {
        kept.lock().unwrap().push(p.clone());
        let text = match wave_singles(p) {
            Some(Ok(qs)) => wave_text(&qs.iter().map(|q| Ok(answer(q))).collect::<Vec<_>>()),
            _ => answer(p),
        };
        Box::pin(async move { Ok(text) })
    });
    (f, seen)
}

const BODY: &str = "Ann, we can offer Acme a one-year supply at Henry Hub.\nThe price is 4,200,000 USD.\nI will send the contract by Friday.";

const RULES: &[(&str, &str)] = &[
    ("another", "none of these"),
    ("Is the document about a particular deal?", "yes, one"),
    ("name a particular company by name?", "yes, one"),
    ("Does the document state a commitment?", "yes, one"),
    ("On which line is the deal", "line 1:"),
    ("On which line is a company first named?", "line 1:"),
    ("On which line is a commitment", "line 3:"),
    ("name or describe the deal start?", "=a"),
    ("name or describe the deal end?", "=Hub."),
    ("name the company start?", "=Acme"),
    ("name the company end?", "=Acme"),
    ("this deal's counterparty?", "Acme"),
    ("state this deal's amount?", "line 2:"),
    ("this deal's amount start?", "=4,200,000"),
    ("this deal's amount end?", "=4,200,000"),
    (
        "Which term does the document give this deal?",
        "a year or more",
    ),
    ("standing?", "=proposal"),
    ("stand?", "=proposal"),
    ("shows this fact?", "offer Acme"),
    ("this commitment's due start?", "=Friday."),
    ("Which person is this commitment about?", "kim@enron.com"),
];

fn supported(fields: &BTreeMap<String, DocumentReadField>, name: &str) -> Value {
    match fields.get(name) {
        Some(DocumentReadField::Supported { value, .. }) => value.clone(),
        other => panic!("{name} not supported: {other:?}"),
    }
}

#[tokio::test]
async fn a_document_is_read_in_levels_each_round_one_wave_off_one_prefix() {
    let policies = mail(NAMES);
    let chapter = input_chapter(&[row(
        1,
        "m1",
        BODY,
        r#"{"from": "Kim Ward <kim@enron.com>"}"#,
    )]);
    let (infer, seen) = oracle(RULES);
    let envelope = read_as(&chapter, &policies, &LineClasses::default(), &infer, Asking::Wave)
        .await
        .unwrap();
    let docs: Vec<DocumentReadOutcome> = serde_json::from_value(
        serde_json::from_str::<Value>(&envelope).unwrap()["documents"].clone(),
    )
    .unwrap();
    let doc = &docs[0];
    assert_eq!(doc.status, DocumentReadStatus::Read, "{doc:?}");

    let stage = doc
        .claims
        .iter()
        .find(|c| c.kind == "stage_update")
        .expect("the act is a claim");
    assert_eq!(supported(&stage.fields, "stage"), json!("proposal"));
    assert_eq!(
        stage.evidence, "Ann, we can offer Acme a one-year supply at Henry Hub.",
        "cited by its sentence's line"
    );
    assert_eq!(
        stage.subject_local_ref,
        "l1:a one-year supply at Henry Hub."
    );
    assert_eq!(
        supported(&stage.subject_fields, "counterparty"),
        json!("Acme")
    );
    assert_eq!(supported(&stage.subject_fields, "amount"), json!(4200000.0));
    assert_eq!(
        supported(&stage.subject_fields, "term"),
        json!("a fixed term of a year or more")
    );

    let promise = doc
        .claims
        .iter()
        .find(|c| c.kind == "commitment")
        .expect("the commitment is a claim");
    assert_eq!(promise.evidence, "I will send the contract by Friday.");
    assert_eq!(supported(&promise.fields, "due"), json!("Friday."));
    assert_eq!(
        supported(&promise.subject_fields, "email"),
        json!("kim@enron.com")
    );

    // Every request opens with the same system prompt and the same document
    // prefix; a level's round is one request. Level 1: counts, lines, start
    // words, end words; level 2: first readings, then the amount's and the
    // cite's next steps, then the amount's end word; level 3 asks nothing.
    let requests = seen.lock().unwrap();
    let prefix = &requests[0].user[..requests[0].user.find(">>>\n\n").unwrap() + 5];
    assert!(requests
        .iter()
        .all(|r| r.system == requests[0].system && r.user.starts_with(prefix)));
    assert!(requests
        .iter()
        .all(|r| r.phase_id.as_deref() == Some(super::super::waves_jobs::WAVES_PHASE)));
    assert_eq!(
        requests.len(),
        7,
        "{:#?}",
        requests.iter().map(|r| r.user.len()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_document_that_holds_nothing_declared_says_so() {
    let policies = mail(NAMES);
    let chapter = input_chapter(&[row(
        1,
        "m1",
        "Lunch at noon?",
        r#"{"from": "Kim Ward <kim@enron.com>"}"#,
    )]);
    let (infer, seen) = oracle(&[]);
    let envelope = read_as(&chapter, &policies, &LineClasses::default(), &infer, Asking::Wave)
        .await
        .unwrap();
    let docs: Vec<DocumentReadOutcome> = serde_json::from_value(
        serde_json::from_str::<Value>(&envelope).unwrap()["documents"].clone(),
    )
    .unwrap();
    // The counts say none; the act is still asked of the document, as the
    // passes reader asks it, and agrees on none.
    assert_eq!(
        docs[0].status,
        DocumentReadStatus::NothingApplicable,
        "{:?}",
        docs[0]
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "one round of counts, one of the act's two wordings"
    );
}

#[tokio::test]
async fn waves_questions_reason_then_choose() {
    let policies = mail(NAMES);
    let chapter = input_chapter(&[row(
        1,
        "m1",
        BODY,
        r#"{"from": "Kim Ward <kim@enron.com>"}"#,
    )]);
    let envelope = |text: String| -> Vec<DocumentReadOutcome> {
        serde_json::from_value(serde_json::from_str::<Value>(&text).unwrap()["documents"].clone())
            .unwrap()
    };
    let (forced_infer, _) = oracle(RULES);
    let forced = envelope(
        read_as(&chapter, &policies, &LineClasses::default(), &forced_infer, Asking::Wave)
            .await
            .unwrap(),
    );

    // The oracle reads every label question; a prompt with no forced choice
    // is a reasoning request, answered in prose that ends with a letter.
    let (choose, chosen) = oracle(RULES);
    let reasoned = Arc::new(Mutex::new(Vec::new()));
    let kept = reasoned.clone();
    let infer: crate::InferenceFn = Arc::new(move |p: &ChatPrompt, budget| {
        if p.response_schema.is_none() {
            kept.lock().unwrap().push((p.clone(), budget));
            return Box::pin(async { Ok("...reasoning... A".to_string()) });
        }
        choose(p, budget)
    });
    let read = envelope(
        read(&chapter, &policies, &LineClasses::default(), &infer)
            .await
            .unwrap(),
    );

    let reasoned = reasoned.lock().unwrap();
    assert!(!reasoned.is_empty());
    for (p, budget) in reasoned.iter() {
        assert_eq!(*budget, Some(2000));
        assert_eq!(p.phase_id.as_deref(), Some("waves_reason"));
        assert!(p.user.ends_with("Reason it through, then end with its letter."));
    }
    let chosen = chosen.lock().unwrap();
    for p in chosen.iter() {
        let singles = match wave_singles(p) {
            Some(split) => split.unwrap(),
            None => vec![p.clone()],
        };
        for q in singles {
            assert!(q.user.contains("Your reasoning:\n...reasoning... A\n\n"), "{}", q.user);
        }
    }
    let stage = read[0]
        .claims
        .iter()
        .find(|c| c.kind == "stage_update")
        .expect("the act is a claim");
    assert_eq!(supported(&stage.fields, "stage"), json!("proposal"));
    assert_eq!(
        serde_json::to_value(&read).unwrap(),
        serde_json::to_value(&forced).unwrap(),
        "reasoning first reads what the forced choice reads"
    );
}
