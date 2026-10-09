use serde_json::json;

use super::*;

/// Ring 2: the model's choice (on y) and the proposed answer (.8, on x) are
/// weighed together, three alternatives (x, y, none). A far more precise
/// choice carries y, a less precise one leaves x ahead, and two that are as
/// precise as each other hold the statement: no record opens.
#[tokio::test]
async fn the_choice_and_the_proposed_answer_are_weighed_together_and_a_split_is_held() {
    for (model_choice, want, record) in [
        (0.9, "weighed", Some("y")),
        (0.7, "weighed", Some("x")),
        (0.8, "held", None),
    ] {
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
        let o = &r.outcomes[0].outcome;
        assert_eq!(
            (o.label(), o.record()),
            (want, record),
            "model_choice {model_choice}"
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
