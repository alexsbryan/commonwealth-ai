use serde_json::json;

use super::*;

/// The model's choice (on y) and the proposed answer (on x) are weighed
/// together at their estimated weights, three alternatives (x, y, none). A
/// choice worth far more carries y, one as reliable as the proposed answer
/// holds the statement (no record opens), and a choice agreeing with the
/// proposed answer links x.
#[tokio::test]
async fn the_choice_and_the_proposed_answer_are_weighed_together_and_a_split_is_held() {
    for (model, argmax, want, record) in [
        ((0.99, 0.01), "B", "weighed", Some("y")),
        ((0.6, 0.3), "B", "held", None),
        ((0.9, 0.05), "A", "weighed", Some("x")),
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
            (want, record),
            "model {model:?} {argmax}"
        );
        if let Outcome::Held(h) = o {
            assert_eq!(h.sources.len(), 2);
            assert_eq!(h.alternatives.len(), 2);
            assert!(h.alternatives.iter().all(|(_, p)| *p < 0.5));
            // Held is not opened: the two records are all there are.
            assert_eq!(res.records().len(), 2);
            assert!(r.outcomes[0].choice.is_some());
        }
    }
}

#[tokio::test]
async fn the_models_choice_is_weighed_at_its_estimated_weight_with_the_proposed_answer() {
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
        Outcome::Decided(Decision::Weighed {
            record, sources, ..
        }) => {
            assert_eq!(record, "y");
            let named: Vec<&str> = sources.iter().map(|v| v.source.as_str()).collect();
            assert_eq!(named, ["model_choice", "proposed_answer"]);
        }
        o => panic!("{o:?}"),
    }
    // C3: each source's precision is said to be estimated, at the estimate.
    assert_eq!(
        r.outcomes[0].by,
        [
            SourcePrecision::new(
                "model_choice",
                crate::enrichment::atlas::precision::Precision::Estimated(
                    est.precision("model_choice").unwrap()
                )
            ),
            SourcePrecision::new(
                "proposed_answer",
                crate::enrichment::atlas::precision::Precision::Estimated(
                    est.precision("proposed_answer").unwrap()
                )
            ),
        ]
    );
    assert_eq!(res.records()[1].statements, ["y", "z"]);
}

#[tokio::test]
async fn a_read_kind_that_differs_weighs_against_a_candidate_and_never_forbids_it() {
    for (read_as, want) in [("A", None), ("B", Some("x"))] {
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
        match want {
            // A differing read outweighs the choice: opened.
            None => assert_eq!((o.label(), o.record()), ("opened", Some("z"))),
            Some(x) => assert_eq!((o.label(), o.record()), ("weighed", Some(x))),
        }
    }
}

#[tokio::test]
async fn where_the_other_sources_already_link_the_model_is_not_asked() {
    let mut res = fire_downtown(weights(&[("proposed_answer", 0.9, 0.02)])).await;
    let (infer, seen) = scripted(vec![]);
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
    assert_eq!((r.calls, seen.lock().unwrap().len()), (0, 0));
    match &r.outcomes[0].outcome {
        Outcome::Decided(Decision::Weighed {
            record, posterior, ..
        }) => assert!(record == "x" && *posterior > 0.5),
        o => panic!("{o:?}"),
    }
}
