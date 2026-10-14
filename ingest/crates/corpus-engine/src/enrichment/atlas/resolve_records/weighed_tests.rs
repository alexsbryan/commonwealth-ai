use serde_json::json;

use super::*;

/// The model's choice decides whatever the proposed answer and the weights
/// say: on y against a proposed answer on x it puts the statement on y, at a
/// weight that would have carried it and at one that would have held it
/// (the weighed posterior, kept beside the choice, then sits below the bar),
/// and agreeing with the proposed answer it puts it on x.
#[tokio::test]
async fn the_models_choice_decides_whatever_the_proposed_answer_and_the_weights_say() {
    for (model, argmax, record) in [
        ((0.99, 0.01), "B", "y"),
        ((0.6, 0.3), "B", "y"),
        ((0.9, 0.05), "A", "x"),
    ] {
        let mut res = fire_downtown(weights(&[
            ("model_choice", model.0, model.1),
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
        // The proposed answer names x (same wording); the model's argmax is `argmax`.
        let mut dist = json!({"A": 0.1, "B": 0.1, "0": 0.1});
        dist[argmax] = json!(0.8);
        let (infer, _) = scripted(vec![dist]);
        let b = "Fire downtown, again.";
        let r = res
            .resolve_document(
                &barred(),
                doc("d1", b),
                &[stmt("z", b, "Fire downtown", 0, &[])],
                &[similar("x", 0.6), similar("y", 0.5)],
                Answerer::Select(&infer),
            )
            .await;
        let o = &r.outcomes[0].outcome;
        assert_eq!(
            (o.label(), o.record()),
            ("chosen", Some(record)),
            "model {model:?} {argmax}"
        );
        let Outcome::Decided(Decision::Chosen { posterior, .. }) = o else {
            unreachable!()
        };
        if model == (0.6, 0.3) {
            assert!(
                *posterior < 0.5,
                "the weights alone would have held it: {posterior}"
            );
        }
        assert_eq!(res.records().len(), 2, "no record opens");
        assert!(r.outcomes[0].choice.is_some());
    }
}

/// A pick against weighed sources that put none at the type's bar (the pick
/// counted among them) does not join: the statement opens a record and its
/// outcome names the vetoed pick. Here the statement's thread is neither
/// record's. Controls: a model choice weighed strongly enough for the pick,
/// and sources that raise nothing (weights at zero, none's posterior below the
/// bar), leave the pick to decide and open nothing.
#[tokio::test]
async fn a_pick_against_sources_that_put_none_at_the_bar_opens_a_record_and_names_it() {
    for (model, thread, label, vetoed, records) in [
        ((0.6, 0.3), (0.9, 0.05), "opened", Some("y"), 3),
        ((0.99, 0.01), (0.9, 0.05), "chosen", None, 2),
        ((0.5, 0.5), (0.5, 0.5), "chosen", None, 2),
    ] {
        let mut res = weights(&[
            ("model_choice", model.0, model.1),
            ("document_thread", thread.0, thread.1),
            ("proposed_answer", 0.5, 0.5),
        ]);
        for (d, id, body, surface, t) in [
            ("d0", "x", "Fire downtown.", "Fire downtown", "t1"),
            ("d0b", "y", "Flood uptown.", "Flood uptown", "t2"),
        ] {
            res.resolve_document(
                &criterion(&[]),
                threaded(d, body, t),
                &[stmt(id, body, surface, 0, &[])],
                &[],
                Answerer::Proposed,
            )
            .await;
        }
        let (infer, _) = scripted(vec![json!({"A": 0.1, "B": 0.8, "0": 0.1})]);
        let b = "Fire downtown, again.";
        let r = res
            .resolve_document(
                &barred(),
                threaded("d1", b, "t3"),
                &[stmt("z", b, "Fire downtown", 0, &[])],
                &[similar("x", 0.6), similar("y", 0.5)],
                Answerer::Select(&infer),
            )
            .await;
        let o = &r.outcomes[0].outcome;
        let case = format!("model {model:?} thread {thread:?}");
        assert_eq!(o.label(), label, "{case}: {o:?}");
        match o {
            Outcome::Decided(Decision::Opened { vetoed: v, .. }) => {
                assert_eq!(v.as_deref(), vetoed, "{case}")
            }
            Outcome::Decided(Decision::Chosen { record, .. }) => assert_eq!(record, "y"),
            _ => unreachable!("{o:?}"),
        }
        assert_eq!(res.records().len(), records, "{case}");
    }
}

/// The model's choice decides, said to be the model's at an unmeasured
/// precision (C3); the sources that agreed with it are kept beside it at
/// their estimated weights.
#[tokio::test]
async fn the_models_choice_decides_and_the_sources_that_agreed_are_kept_at_their_estimates() {
    let (infer, _) = scripted(vec![json!({"A": 0.2, "B": 0.7, "0": 0.1})]);
    // A proposed answer that does not link alone, so the model is asked.
    let mut res = weights(&[("proposed_answer", 0.5, 0.4)]);
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
    // The weights d1 is weighed at: those the documents before it estimate.
    let est = res.estimate().clone();
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
    // The model's argmax (B) and the proposed answer (same wording) both name y.
    match &r.outcomes[0].outcome {
        Outcome::Decided(Decision::Chosen {
            record, sources, ..
        }) => {
            assert_eq!(record, "y");
            let kept: Vec<(&str, f64)> = sources
                .iter()
                .map(|v| (v.source.as_str(), v.precision))
                .collect();
            assert_eq!(
                kept,
                [
                    ("model_choice", est.precision("model_choice").unwrap()),
                    ("proposed_answer", est.precision("proposed_answer").unwrap()),
                ]
            );
        }
        o => panic!("{o:?}"),
    }
    // C3: the decision is the model's, its precision unmeasured.
    assert_eq!(
        r.outcomes[0].by,
        [SourcePrecision::new(
            "model_choice",
            crate::enrichment::atlas::precision::Precision::Unmeasured
        )]
    );
    assert_eq!(res.records()[1].statements, ["y", "z"]);
}

/// A read kind that differs from the candidate's never forbids it: the
/// candidate is offered, the model names it, and its choice decides.
#[tokio::test]
async fn a_read_kind_that_differs_never_forbids_a_candidate_the_model_picks() {
    for read_as in ["A", "B"] {
        let mut res = weights(&[]);
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
        // d1's is READ as `read_as`; the death is still offered, the model
        // names it, and the read kind is weighed with the choice.
        let read = if read_as == "A" {
            json!({"A": 0.9, "B": 0.05, "0": 0.05})
        } else {
            json!({"A": 0.05, "B": 0.9, "0": 0.05})
        };
        let (infer, seen) = scripted(vec![read, json!({"A": 0.9, "0": 0.1})]);
        let b = "Shots were fired.";
        let r = res
            .resolve_document(
                &kind_necessary(),
                doc("d1", b),
                &[stmt("z", b, "Shots", 0, &[])],
                &[similar("x", 0.6)],
                Answerer::Select(&infer),
            )
            .await;
        assert_eq!((r.vetoed, r.calls, seen.lock().unwrap().len()), (0, 2, 2));
        let o = &r.outcomes[0].outcome;
        assert_eq!(
            (o.label(), o.record()),
            ("chosen", Some("x")),
            "read as {read_as}"
        );
    }
}

/// Where the other sources would link on their own, the model is still asked
/// and its choice decides: x when it names x, its own record when it names
/// none. Nothing links on fitted weights alone.
#[tokio::test]
async fn the_model_is_asked_even_where_the_other_sources_would_link() {
    for (answer, want) in [
        (json!({"A": 0.9, "0": 0.1}), ("chosen", Some("x"))),
        (json!({"A": 0.1, "0": 0.9}), ("opened", Some("z"))),
    ] {
        let mut res = fire_downtown(weights(&[("proposed_answer", 0.9, 0.02)])).await;
        let (infer, seen) = scripted(vec![answer]);
        let b = "Fire downtown, again.";
        let r = res
            .resolve_document(
                &barred(),
                doc("d1", b),
                &[stmt("z", b, "Fire downtown", 0, &[])],
                &[similar("x", 0.6)],
                Answerer::Select(&infer),
            )
            .await;
        assert_eq!((r.calls, seen.lock().unwrap().len()), (1, 1));
        let o = &r.outcomes[0].outcome;
        assert_eq!((o.label(), o.record()), want);
    }
}
