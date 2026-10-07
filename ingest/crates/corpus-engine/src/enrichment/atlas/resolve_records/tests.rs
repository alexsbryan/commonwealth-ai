use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::answer::{strip_markers, Proposed};
use super::*;
use crate::enrichment::pipeline::types::ChatPrompt;

fn criterion(keys: &[&str]) -> Criterion {
    Criterion {
        type_name: "happening".into(),
        description: String::new(),
        same_when: Some("the same act by the same parties at the same time and place".into()),
        keys: keys.iter().map(|k| k.to_string()).collect(),
        evidential: vec![],
        bar: None,
        model_choice: None,
        reasoned_choice: None,
        proposed_answer: None,
        necessary: vec![],
    }
}

fn doc<'a>(id: &'a str, body: &'a str) -> Document<'a> {
    Document {
        id,
        title: None,
        body,
        stamps: &[],
    }
}

/// A document in declared thread `thread`.
fn threaded<'a>(id: &'a str, body: &'a str, thread: &str) -> Document<'a> {
    Document {
        id,
        title: None,
        body,
        stamps: Vec::leak(vec![(DocumentStamp::Thread, thread.to_string())]),
    }
}

/// A statement over the `nth` occurrence of `surface` in `body`.
fn stmt(id: &str, body: &str, surface: &str, nth: usize, keys: &[(&str, &str)]) -> Statement {
    let start = body
        .match_indices(surface)
        .nth(nth)
        .expect("surface in body")
        .0;
    Statement {
        id: id.into(),
        start,
        end: start + surface.len(),
        keys: keys
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

/// A model that gives `answers` in order, one per call, and keeps every prompt.
fn scripted(answers: Vec<Value>) -> (InferenceFn, Arc<Mutex<Vec<ChatPrompt>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(answers.into_iter()));
    let kept = seen.clone();
    let f: InferenceFn = Arc::new(move |p: &ChatPrompt, _| {
        kept.lock().unwrap().push(p.clone());
        let next = queue.lock().unwrap().next().map(|v| v.to_string());
        Box::pin(async move {
            next.ok_or_else(|| crate::Error::Extraction("no scripted answer left".into()))
        })
    });
    (f, seen)
}

/// A proposal for record `id` from a document `similarity` alike.
fn similar(id: &str, similarity: f32) -> Proposal {
    Proposal {
        record: id.into(),
        reasons: vec![Reason::SimilarDocument { similarity }],
    }
}

/// A proposal for record `id` from a document as alike as can be.
fn prop(id: &str) -> Proposal {
    similar(id, 1.0)
}

fn labels(r: &DocumentResolution) -> Vec<&'static str> {
    r.outcomes.iter().map(|o| o.outcome.label()).collect()
}

/// One particular of an answer: what it is the same as, and its statements with their cites.
fn part(same_as: &str, mentions: &[(&str, &str)]) -> Value {
    let mentions: Vec<Value> = mentions
        .iter()
        .map(|(s, c)| json!({"statement": s, "cite": c}))
        .collect();
    json!({"same_as": same_as, "mentions": mentions})
}

fn answer(parts: Vec<Value>) -> Value {
    json!({ "particulars": parts })
}

#[tokio::test]
async fn a_declared_key_decides_without_a_call() {
    let (infer, seen) = scripted(vec![]);
    let mut res = Resolver::default();
    let c = criterion(&["email"]);
    let b1 = "Kim wrote first.";
    let first = res
        .resolve_document(
            &c,
            doc("d1", b1),
            &[stmt("a", b1, "Kim", 0, &[("email", "kim@x.com")])],
            &[],
            Answerer::Model(&infer),
        )
        .await;
    let b2 = "K. Ward replied.";
    let second = res
        .resolve_document(
            &c,
            doc("d2", b2),
            &[stmt("b", b2, "K. Ward", 0, &[("email", " KIM@x.com")])],
            &[],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(labels(&first), ["opened"]);
    assert_eq!(
        second.outcomes[0].outcome,
        Outcome::Decided(Decision::Key {
            record: "a".into(),
            key: "email".into(),
            // the value as compared: folded the way every identity key is (`fold_identity_value`)
            value: "kim@x com".into()
        })
    );
    assert_eq!(first.calls + second.calls, 0);
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(res.records()[0].statements, ["a", "b"]);
}

#[tokio::test]
async fn one_call_groups_a_document_and_joins_a_shown_candidate() {
    let b1 = "A man was shot in Salisbury on Sunday. The shooting left him dead.";
    let b2 = "Police said the Salisbury shooting on Sunday was a dispute.";
    let (infer, seen) = scripted(vec![
        answer(vec![part(
            "none",
            &[
                ("s0", "shot in Salisbury on Sunday"),
                ("s1", "The shooting"),
            ],
        )]),
        answer(vec![part(
            "r0",
            &[("s0", "the Salisbury shooting on Sunday")],
        )]),
    ]);
    let mut res = Resolver::default();
    let c = criterion(&[]);
    let one = res
        .resolve_document(
            &c,
            doc("d1", b1),
            &[
                stmt("a", b1, "shot", 0, &[]),
                stmt("b", b1, "shooting", 0, &[]),
            ],
            &[],
            Answerer::Model(&infer),
        )
        .await;
    let two = res
        .resolve_document(
            &c,
            doc("d2", b2),
            &[stmt("c", b2, "shooting", 0, &[])],
            &[prop("a")],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(labels(&one), ["opened", "opened"]);
    assert_eq!(
        one.outcomes[1].outcome.record(),
        Some("a"),
        "the opener's id"
    );
    assert_eq!(
        two.outcomes[0].outcome,
        Outcome::Decided(Decision::Cited {
            record: "a".into(),
            cite: "the Salisbury shooting on Sunday".into()
        })
    );
    assert_eq!((one.calls, two.calls), (1, 1));
    assert_eq!(res.records().len(), 1);
    let prompts = seen.lock().unwrap();
    assert!(
        prompts[0].user.contains("shot[s0] in Salisbury"),
        "{}",
        prompts[0].user
    );
    assert!(
        prompts[1]
            .user
            .contains("- r0 (document similarity 1.00), said as \"shot\", \"shooting\"; cited: \"shot in Salisbury on Sunday\" | \"The shooting\"\n    \"…A man was shot in Salisbury on Sunday. The shooting left him dead.…\""),
        "{}",
        prompts[1].user
    );
    assert_eq!(
        prompts[1].response_schema.as_ref().unwrap()["properties"]["particulars"]["items"]
            ["properties"]["same_as"]["enum"],
        json!(["none", "r0"])
    );
}

#[tokio::test]
async fn an_uncitable_cite_refuses_its_statement_only_never_defaulted() {
    let b = "Two people were hurt. The injuries were minor.";
    let (infer, _) = scripted(vec![answer(vec![part(
        "none",
        &[("s0", "two people were wounded"), ("s1", "The injuries")],
    )])]);
    let mut res = Resolver::default();
    let r = res
        .resolve_document(
            &criterion(&[]),
            doc("d", b),
            &[
                stmt("a", b, "hurt", 0, &[]),
                stmt("b", b, "injuries", 0, &[]),
            ],
            &[],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(labels(&r), ["refused:cite_not_found", "opened"]);
    assert_eq!(res.records()[0].statements, ["b"]);
}

#[tokio::test]
async fn keys_that_tie_two_statements_to_two_records_refuse_the_group() {
    let mut res = Resolver::default();
    let c = criterion(&["ref"]);
    let (seed, _) = scripted(vec![answer(vec![
        part("none", &[("s0", "Deal A")]),
        part("none", &[("s1", "Deal B")]),
    ])]);
    let b0 = "Deal A and Deal B.";
    res.resolve_document(
        &c,
        doc("d0", b0),
        &[
            stmt("a", b0, "Deal A", 0, &[]),
            stmt("b", b0, "Deal B", 0, &[]),
        ],
        &[],
        Answerer::Model(&seed),
    )
    .await;
    let b = "The trade, that is the swap, closed.";
    let (infer, _) = scripted(vec![answer(vec![
        part("r0", &[("s0", "The trade")]),
        part("r1", &[("s1", "the swap")]),
    ])]);
    let r = res
        .resolve_document(
            &c,
            doc("d", b),
            &[
                stmt("x", b, "trade", 0, &[("ref", "77")]),
                stmt("y", b, "swap", 0, &[("ref", "77")]),
            ],
            &[prop("a"), prop("b")],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(
        labels(&r),
        ["refused:contradiction", "refused:contradiction"]
    );
    assert_eq!(
        r.outcomes[0].outcome,
        Outcome::Refused(Refusal::Contradiction {
            targets: vec!["a".into(), "b".into()]
        })
    );
}

#[tokio::test]
async fn missing_duplicate_and_unknown_answers_are_each_refused() {
    let b = "One, two, three.";
    let (infer, _) = scripted(vec![answer(vec![
        part("none", &[("s1", "two")]),
        part("none", &[("s1", "two")]),
        part("r9", &[("s2", "three")]),
    ])]);
    let mut res = Resolver::default();
    let r = res
        .resolve_document(
            &criterion(&[]),
            doc("d", b),
            &[
                stmt("a", b, "One", 0, &[]),
                stmt("b", b, "two", 0, &[]),
                stmt("c", b, "three", 0, &[]),
            ],
            &[],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(
        labels(&r),
        [
            "refused:unanswered",
            "refused:duplicated",
            "refused:unknown_target"
        ]
    );
}

#[tokio::test]
async fn no_criterion_asks_nothing_and_a_failed_call_refuses_the_batch() {
    let b = "First and second.";
    let ss = [
        stmt("a", b, "First", 0, &[]),
        stmt("b", b, "second", 0, &[]),
    ];
    let (infer, seen) = scripted(vec![]);
    let mut bare = criterion(&[]);
    bare.same_when = None;
    let r = Resolver::default()
        .resolve_document(&bare, doc("d", b), &ss, &[], Answerer::Model(&infer))
        .await;
    assert_eq!(labels(&r), ["refused:no_criterion", "refused:no_criterion"]);
    assert!(seen.lock().unwrap().is_empty());

    let r = Resolver::default()
        .resolve_document(
            &criterion(&[]),
            doc("d", b),
            &ss,
            &[],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(labels(&r), ["refused:no_answer", "refused:no_answer"]);
    assert_eq!(r.calls, 1);
}

#[tokio::test]
async fn a_span_outside_the_body_is_unreadable() {
    let (infer, _) = scripted(vec![]);
    let s = Statement {
        id: "a".into(),
        start: 4,
        end: 99,
        keys: Default::default(),
    };
    let r = Resolver::default()
        .resolve_document(
            &criterion(&[]),
            doc("d", "short"),
            &[s],
            &[],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(labels(&r), ["refused:unreadable"]);
}

#[test]
fn a_cite_is_found_across_markers_and_whitespace_but_not_paraphrased() {
    let body = fold_ws("The girl\n was  shot in Salisbury.");
    assert!(cite_found(&body, "was shot[s3] in Salisbury"));
    assert!(cite_found(&body, "\"was shot in Salisbury\""));
    assert!(cite_found(&body, "“shot in Salisbury.”"));
    assert!(!cite_found(&body, "\"was shot in salisbury\""));
    assert!(!cite_found(&body, "was shot in salisbury"));
    assert!(!cite_found(&body, "[s1]"));
    assert!(!cite_found(&body, "\"\""));
    assert_eq!(strip_markers("a[s]b[s12]c[x"), "a[s]bc[x");
}

#[test]
fn context_is_a_window_cut_at_whitespace() {
    let body = format!(
        "{} the girl was shot in Salisbury {}",
        "x".repeat(300),
        "y".repeat(300)
    );
    let at = body.find("shot").unwrap();
    let c = context(&body, at, at + 4);
    assert_eq!(
        c, "the girl was shot in Salisbury",
        "a partial word at either cut is dropped"
    );
    let short = "A man was\n shot today.";
    assert_eq!(context(short, 11, 15), "A man was shot today.");
}

#[tokio::test]
async fn a_particular_naming_a_candidate_joins_every_statement_in_it() {
    let b0 = "A fire broke out downtown.";
    let b1 = "Firefighters fought the downtown fire; the blaze is out.";
    let (infer, _) = scripted(vec![
        answer(vec![part("none", &[("s0", "A fire broke out downtown")])]),
        answer(vec![part(
            "r0",
            &[("s0", "the downtown fire"), ("s1", "the blaze")],
        )]),
    ]);
    let mut res = Resolver::default();
    let c = criterion(&[]);
    // Two statements so the first document is asked, not opened without a call.
    res.resolve_document(
        &c,
        doc("d0", b0),
        &[
            stmt("a", b0, "fire", 0, &[]),
            stmt("x", b0, "broke out", 0, &[]),
        ],
        &[],
        Answerer::Model(&infer),
    )
    .await;
    let r = res
        .resolve_document(
            &c,
            doc("d1", b1),
            &[
                stmt("b", b1, "fire", 0, &[]),
                stmt("c", b1, "blaze", 0, &[]),
            ],
            &[prop("a")],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(labels(&r), ["cited", "cited"]);
    assert!(r.outcomes.iter().all(|o| o.outcome.record() == Some("a")));
}

fn record(id: &str, surface: &str) -> Record {
    Record {
        id: id.into(),
        handle: format!("r{id}"),
        statements: vec![],
        keys: Default::default(),
        fields: Default::default(),
        evidence: vec![Evidence {
            document: "d0".into(),
            title: None,
            surface: surface.into(),
            cite: None,
            context: "Police said the shooting happened at noon.".into(),
        }],
    }
}

#[test]
fn the_proposed_answer_groups_one_wording_and_joins_a_similar_record_said_so() {
    let records = vec![record("0", "shooting"), record("1", "Shooting")];
    let b = "The Shooting left one dead; the shooting was at noon. A death followed.";
    let ss = [
        stmt("a", b, "Shooting", 0, &[]),
        stmt("b", b, "dead", 0, &[]),
        stmt("c", b, "shooting", 0, &[]),
        stmt("d", b, "death", 0, &[]),
    ];
    let asked = [0, 1, 2, 3];
    let render = |rule: ProposalRule, shown: &[Proposal]| {
        let shown: Vec<(usize, &Proposal)> = shown.iter().enumerate().collect();
        Proposed::of(rule, doc("d", b), &ss, &asked, &shown, &records).render(&records)
    };
    let rule = ProposalRule::default();
    let got = render(rule, &[similar("0", 0.2), similar("1", 0.6)]);
    assert!(
        got.ends_with("r1: s0 s2 (same wording, similarity 0.60)\nnone: s1\nnone: s3"),
        "{got}"
    );
    let got = render(rule, &[similar("0", 0.39), similar("1", 0.39)]);
    assert!(got.ends_with("none: s0 s2\nnone: s1\nnone: s3"), "{got}");
    let loose = ProposalRule {
        similar: Some(0.3),
        ..rule
    };
    let got = render(loose, &[similar("0", 0.39), similar("1", 0.2)]);
    assert!(
        got.ends_with(
            "r0: s0 s2 (similarity 0.39)\nr0: s1 (similarity 0.39)\nr0: s3 (similarity 0.39)"
        ),
        "every wording takes the most similar record at the looser bar: {got}"
    );
    let threaded = Proposal {
        record: "1".into(),
        reasons: vec![Reason::SameThread],
    };
    let got = render(rule, &[similar("0", 0.9), threaded]);
    assert!(
        got.ends_with("r1: s0 s2 (same thread)\nr1: s1 (same thread)\nr1: s3 (same thread)"),
        "a declared thread outranks every similarity: {got}"
    );
}

#[tokio::test]
async fn the_proposed_engine_takes_the_proposed_answer_without_a_call() {
    let mut res = Resolver::default();
    let c = criterion(&[]);
    let b0 = "A fire broke out downtown. The fire spread.";
    let r0 = res
        .resolve_document(
            &c,
            doc("d0", b0),
            &[stmt("a", b0, "fire", 0, &[]), stmt("b", b0, "fire", 1, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
    assert_eq!(labels(&r0), ["opened", "opened"]);
    assert_eq!(
        res.records()[0].statements,
        ["a", "b"],
        "one wording, one particular"
    );
    let b1 = "Crews fought the fire all night.";
    let r1 = res
        .resolve_document(
            &c,
            doc("d1", b1),
            &[
                stmt("c", b1, "fire", 0, &[]),
                stmt("d", b1, "fought", 0, &[]),
            ],
            &[similar("a", 0.5)],
            Answerer::Proposed,
        )
        .await;
    assert_eq!(
        r1.outcomes[0].outcome,
        Outcome::Decided(Decision::Cited {
            record: "a".into(),
            cite: "fire".into()
        })
    );
    assert_eq!(labels(&r1), ["cited", "opened"]);
    assert_eq!((r0.calls, r1.calls), (0, 0));
    assert_eq!(res.records().len(), 2);
}

#[tokio::test]
async fn a_forced_choice_per_statement_offers_what_the_document_opened_and_keeps_the_distribution()
{
    let (infer, seen) = scripted(vec![
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.3, "0": 0.7}),
        Value::String("A".into()),
    ]);
    let mut res = Resolver::default();
    let c = criterion(&[]);
    let b0 = "A fire broke out downtown; the blaze spread.";
    let r0 = res
        .resolve_document(
            &c,
            doc("d0", b0),
            &[
                stmt("a", b0, "fire", 0, &[]),
                stmt("b", b0, "blaze", 0, &[]),
            ],
            &[],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(labels(&r0), ["opened", "opened"]);
    assert_eq!(
        r0.calls, 1,
        "the first statement had no candidate to ask about"
    );
    assert_eq!(res.records()[0].statements, ["a", "b"]);
    assert_eq!(
        r0.outcomes[1].choice,
        Some(Choice {
            candidates: vec![("a".into(), 0.9)],
            none: 0.1,
            reasoning: None
        })
    );
    let b1 = "Crews fought a fire all night.";
    let r1 = res
        .resolve_document(
            &c,
            doc("d1", b1),
            &[stmt("c", b1, "fire", 0, &[])],
            &[similar("a", 0.5)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(labels(&r1), ["opened"], "none was the most probable");
    let b2 = "The fire was out by noon.";
    let r2 = res
        .resolve_document(
            &c,
            doc("d2", b2),
            &[stmt("d", b2, "fire", 0, &[])],
            &[similar("a", 0.5)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(
        labels(&r2),
        ["refused:no_answer"],
        "a label is not a distribution"
    );
    let prompts = seen.lock().unwrap();
    assert!(
        prompts[0]
            .user
            .contains("A (opened earlier in this document), said as \"fire\""),
        "{}",
        prompts[0].user
    );
    assert!(
        prompts[0].user.contains("the [[blaze]] spread"),
        "{}",
        prompts[0].user
    );
    assert_eq!(
        prompts[0].response_schema,
        Some(oicp_types::forced_choice::schema(&["A", "0"]))
    );
}

#[tokio::test]
async fn the_most_probable_shown_record_is_selected_with_its_probability() {
    let (infer, _) = scripted(vec![json!({"A": 0.2, "B": 0.7, "0": 0.1})]);
    let mut res = Resolver::default();
    let c = criterion(&[]);
    let seed = "Fire downtown. Flood uptown.";
    let (s, _) = scripted(vec![answer(vec![
        part("none", &[("s0", "Fire downtown")]),
        part("none", &[("s1", "Flood uptown")]),
    ])]);
    res.resolve_document(
        &c,
        doc("d0", seed),
        &[
            stmt("x", seed, "Fire", 0, &[]),
            stmt("y", seed, "Flood", 0, &[]),
        ],
        &[],
        Answerer::Model(&s),
    )
    .await;
    let b = "The flood receded.";
    let r = res
        .resolve_document(
            &c,
            doc("d1", b),
            &[stmt("z", b, "flood", 0, &[])],
            &[similar("x", 0.4), similar("y", 0.6)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(
        r.outcomes[0].outcome,
        Outcome::Decided(Decision::Selected {
            record: "y".into(),
            probability: 0.7
        })
    );
    assert_eq!(res.records()[1].statements, ["y", "z"]);
}

#[tokio::test]
async fn a_distribution_that_leaves_a_shown_label_out_is_refused_not_read_as_zero() {
    let mut res = Resolver::default();
    let c = criterion(&[]);
    let seed = "Fire downtown.";
    let (s, _) = scripted(vec![answer(vec![part("none", &[("s0", "Fire downtown")])])]);
    res.resolve_document(
        &c,
        doc("d0", seed),
        &[stmt("x", seed, "Fire", 0, &[])],
        &[],
        Answerer::Model(&s),
    )
    .await;
    // Read as 0, the missing "A" would leave "0" the argmax and open a record.
    let (infer, _) = scripted(vec![json!({"0": 0.4})]);
    let b = "The fire spread.";
    let r = res
        .resolve_document(
            &c,
            doc("d1", b),
            &[stmt("z", b, "fire", 0, &[])],
            &[similar("x", 0.6)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(
        r.outcomes[0].outcome,
        Outcome::Refused(Refusal::NoAnswer {
            reason: "distribution lacks label(s) [\"A\"]".into()
        })
    );
    assert_eq!(res.records().len(), 1);
}

/// A criterion weighing the declared thread at `precision` against `bar`.
fn thread_evidence(precision: f64, bar: f64) -> Criterion {
    Criterion {
        evidential: vec![(DocumentStamp::Thread, precision)],
        bar: Some(bar),
        ..criterion(&[])
    }
}

#[tokio::test]
async fn an_evidential_field_one_earlier_record_holds_links_without_a_call() {
    let mut res = Resolver::default();
    let a = "Install fails on Windows.";
    res.resolve_document(
        &criterion(&[]),
        threaded("d0", a, "7"),
        &[stmt("x", a, "Install fails", 0, &[])],
        &[],
        Answerer::Proposed,
    )
    .await;
    // Any call would refuse: no answer is scripted.
    let (infer, _) = scripted(vec![]);
    let b = "labeled bug";
    let r = res
        .resolve_document(
            &thread_evidence(0.83, 0.5),
            threaded("d1", b, "7"),
            &[stmt("z", b, "labeled bug", 0, &[])],
            &[],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(r.calls, 0);
    assert_eq!(
        r.outcomes[0].outcome,
        Outcome::Decided(Decision::Field {
            record: "x".into(),
            field: "document_thread",
            value: "7".into(),
            precision: 0.83
        })
    );
    assert_eq!(res.records()[0].statements, ["x", "z"]);
    assert_eq!(res.records()[0].fields["document_thread"].len(), 1);
}

#[tokio::test]
async fn a_field_below_the_bar_or_held_by_two_records_settles_nothing() {
    let mut res = Resolver::default();
    for (id, body) in [("d0", "Install fails."), ("d1", "Lockfile drifts.")] {
        res.resolve_document(
            &criterion(&[]),
            threaded(id, body, "7"),
            &[stmt(id, body, body, 0, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
    }
    let b = "labeled bug";
    // Held by two records: the answerer decides (alone, no candidate: opened).
    let two = res
        .resolve_document(
            &thread_evidence(0.83, 0.5),
            threaded("d2", b, "7"),
            &[stmt("z", b, b, 0, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
    assert_eq!(two.outcomes[0].outcome.label(), "opened");

    let mut one = Resolver::default();
    one.resolve_document(
        &criterion(&[]),
        threaded("d0", "Install fails.", "7"),
        &[stmt("x", "Install fails.", "Install fails.", 0, &[])],
        &[],
        Answerer::Proposed,
    )
    .await;
    let below = one
        .resolve_document(
            &thread_evidence(0.4, 0.5),
            threaded("d1", b, "7"),
            &[stmt("y", b, b, 0, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
    assert_eq!(below.outcomes[0].outcome.label(), "opened");
}

/// A criterion with `kind` necessary over two values.
fn kind_necessary() -> Criterion {
    Criterion {
        necessary: vec![("kind".into(), vec!["firing".into(), "death".into()])],
        ..criterion(&[])
    }
}

#[tokio::test]
async fn a_candidate_whose_read_kind_differs_is_not_offered() {
    let mut res = Resolver::default();
    let a = "The victim died.";
    // d0's statement is READ as a death (B), then opens: no candidate.
    let (read0, _) = scripted(vec![json!({"A": 0.1, "B": 0.85, "0": 0.05})]);
    res.resolve_document(
        &kind_necessary(),
        doc("d0", a),
        &[stmt("x", a, "died", 0, &[])],
        &[],
        Answerer::Select(&read0),
    )
    .await;
    assert_eq!(res.records()[0].fields["kind"].len(), 1);
    // d1's is READ as a firing (A): the death is not offered, so no choice is asked.
    let (read1, seen) = scripted(vec![json!({"A": 0.9, "B": 0.05, "0": 0.05})]);
    let b = "Shots were fired.";
    let r = res
        .resolve_document(
            &kind_necessary(),
            doc("d1", b),
            &[stmt("z", b, "Shots", 0, &[])],
            &[similar("x", 0.6)],
            Answerer::Select(&read1),
        )
        .await;
    assert_eq!((r.vetoed, r.calls, seen.lock().unwrap().len()), (1, 1, 1));
    assert_eq!(r.outcomes[0].outcome.label(), "opened");
}

/// d0 opens record "x" said as "Fire downtown"; returns the resolver.
async fn fire_downtown() -> Resolver {
    let mut res = Resolver::default();
    let a = "Fire downtown.";
    res.resolve_document(
        &criterion(&[]),
        doc("d0", a),
        &[stmt("x", a, "Fire downtown", 0, &[])],
        &[],
        Answerer::Proposed,
    )
    .await;
    res
}

#[tokio::test]
async fn a_choice_below_the_bar_is_not_asked_and_the_proposed_answer_decides() {
    let mut res = fire_downtown().await;
    let c = Criterion {
        bar: Some(0.5),
        model_choice: Some(0.3),
        proposed_answer: Some(0.8),
        ..criterion(&[])
    };
    let (infer, seen) = scripted(vec![]);
    let b = "Fire downtown, again.";
    let r = res
        .resolve_document(
            &c,
            doc("d1", b),
            &[stmt("z", b, "Fire downtown", 0, &[])],
            &[similar("x", 0.6)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!((r.calls, seen.lock().unwrap().len()), (0, 0));
    assert_eq!(
        r.outcomes[0].outcome,
        Outcome::Decided(Decision::Proposed {
            record: "x".into(),
            precision: 0.8
        })
    );
}

#[tokio::test]
async fn of_the_choice_and_the_proposed_answer_the_more_precise_decides() {
    for (model_choice, want) in [(0.9, "selected"), (0.7, "proposed")] {
        let mut res = fire_downtown().await;
        let y = "Flood uptown.";
        res.resolve_document(
            &criterion(&[]),
            doc("d0b", y),
            &[stmt("y", y, "Flood uptown", 0, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
        let c = Criterion {
            bar: Some(0.5),
            model_choice: Some(model_choice),
            proposed_answer: Some(0.8),
            ..criterion(&[])
        };
        // The proposed answer names x (same wording); the model's argmax is y (B).
        let (infer, _) = scripted(vec![json!({"A": 0.1, "B": 0.8, "0": 0.1})]);
        let b = "Fire downtown, again.";
        let r = res
            .resolve_document(
                &c,
                doc("d1", b),
                &[stmt("z", b, "Fire downtown", 0, &[])],
                &[similar("x", 0.6), similar("y", 0.5)],
                Answerer::Select(&infer),
            )
            .await;
        assert_eq!(
            r.outcomes[0].outcome.label(),
            want,
            "model_choice {model_choice}"
        );
    }
}

#[tokio::test]
async fn of_two_fields_that_settle_on_different_records_the_more_precise_decides() {
    let mut res = Resolver::default();
    let stamps = |t: &str, d: &str| -> &'static [(DocumentStamp, String)] {
        Vec::leak(vec![
            (DocumentStamp::Thread, t.to_string()),
            (DocumentStamp::Date, d.to_string()),
        ])
    };
    for (id, t, d) in [("x", "7", "2024-01-01"), ("y", "9", "2024-02-02")] {
        let body = "Opened.";
        res.resolve_document(
            &criterion(&[]),
            Document {
                id,
                title: None,
                body,
                stamps: stamps(t, d),
            },
            &[stmt(id, body, "Opened", 0, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
    }
    // Thread 7 is held by x, the date by y: the thread is the more precise.
    let c = Criterion {
        evidential: vec![
            (DocumentStamp::Date, 16.0 / 18.0),
            (DocumentStamp::Thread, 191.0 / 212.0),
        ],
        bar: Some(0.5),
        ..criterion(&[])
    };
    let b = "merged";
    let r = res
        .resolve_document(
            &c,
            Document {
                id: "z",
                title: None,
                body: b,
                stamps: stamps("7", "2024-02-02"),
            },
            &[stmt("q", b, b, 0, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
    assert_eq!(r.outcomes[0].outcome.record(), Some("x"));
}

#[tokio::test]
async fn a_reasoned_choice_is_read_after_the_models_own_reasoning() {
    let mut res = fire_downtown().await;
    // First the reasoning (generated), then the forced choice read after it.
    let (infer, seen) = scripted(vec![
        json!("Both are the downtown fire; the rule holds. A"),
        json!({"A": 0.8, "0": 0.2}),
    ]);
    let b = "The downtown blaze is out.";
    let r = res
        .resolve_document(
            &criterion(&[]),
            doc("d1", b),
            &[stmt("z", b, "downtown blaze", 0, &[])],
            &[similar("x", 0.6)],
            Answerer::Reason(&infer),
        )
        .await;
    let prompts = seen.lock().unwrap();
    assert_eq!((r.calls, prompts.len()), (2, 2));
    assert!(prompts[0].response_schema.is_none() && prompts[1].response_schema.is_some());
    assert!(
        prompts[1].user.contains("Your reasoning:") && prompts[1].user.contains("the rule holds")
    );
    assert_eq!(r.outcomes[0].outcome.record(), Some("x"));
    let choice = r.outcomes[0].choice.as_ref().expect("the choice is kept");
    assert!(choice
        .reasoning
        .as_deref()
        .is_some_and(|t| t.contains("the rule holds")));
}
