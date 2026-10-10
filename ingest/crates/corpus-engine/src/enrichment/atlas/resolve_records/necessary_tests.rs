use std::collections::BTreeSet;

use super::*;

fn necessary(keys: &[&str]) -> Criterion {
    let mut c = criterion(keys);
    c.necessary = vec![ClosedAttr {
        name: "kind".into(),
        description: String::new(),
        values: ["firing", "death"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    }];
    c
}

fn values(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[tokio::test]
async fn necessary_identity_key_conflict_refuses_instead_of_opening_duplicate() {
    let criterion = necessary(&["deal"]);
    let mut resolver = Resolver::default();
    let first = "The report describes a firing.";
    resolver
        .resolve_document(
            &criterion,
            doc("d0", first),
            &[stmt(
                "source",
                first,
                "firing",
                0,
                &[("deal", "D-1"), ("kind", "firing")],
            )],
            &[],
            Answerer::Proposed,
        )
        .await;

    let second = "The same deal reports a death.";
    let result = resolver
        .resolve_document(
            &criterion,
            doc("d1", second),
            &[stmt(
                "conflict",
                second,
                "death",
                0,
                &[("deal", "D-1"), ("kind", "death")],
            )],
            &[],
            Answerer::Proposed,
        )
        .await;

    assert_eq!(
        result.outcomes[0].outcome,
        Outcome::Refused(Refusal::Contradiction {
            targets: vec!["source".into()]
        })
    );
    assert_eq!(result.vetoed, 1);
    assert_eq!(
        resolver.records().len(),
        1,
        "a conflicting sufficient key must not open a duplicate"
    );
    assert_eq!(resolver.records()[0].statements, ["source"]);
}

#[tokio::test]
async fn necessary_identity_a_read_value_never_overrides_a_sufficient_key() {
    let criterion = necessary(&["deal"]);
    let mut resolver = Resolver::default();
    let first = "A firing is reported.";
    let (seed_read, _) = scripted(vec![json!({"A": 0.9, "B": 0.05, "0": 0.05})]);
    resolver
        .resolve_document(
            &criterion,
            doc("d0", first),
            &[stmt("source", first, "firing", 0, &[("deal", "D-1")])],
            &[],
            Answerer::Select(&seed_read),
        )
        .await;

    // The key D-1 says one particular; the model READ a different kind. A
    // read value is a weighed source, not a supplied one: the key links.
    let second = "A death is reported for the same deal.";
    let (conflict_read, seen) = scripted(vec![json!({"A": 0.05, "B": 0.9, "0": 0.05})]);
    let result = resolver
        .resolve_document(
            &criterion,
            doc("d1", second),
            &[stmt("conflict", second, "death", 0, &[("deal", "D-1")])],
            &[],
            Answerer::Select(&conflict_read),
        )
        .await;

    assert!(matches!(
        result.outcomes[0].outcome,
        Outcome::Decided(Decision::Key { .. })
    ));
    assert_eq!(
        (result.calls, result.vetoed, seen.lock().unwrap().len()),
        (1, 0, 1)
    );
    assert_eq!(resolver.records().len(), 1);
    assert_eq!(
        resolver.records()[0].fields["kind"],
        values(&["death", "firing"])
    );
}

#[tokio::test]
async fn necessary_identity_invalid_supplied_value_is_read_against_the_closed_set() {
    let criterion = necessary(&["deal"]);
    let mut resolver = Resolver::default();
    let first = "A firing is reported.";
    resolver
        .resolve_document(
            &criterion,
            doc("d0", first),
            &[stmt(
                "source",
                first,
                "firing",
                0,
                &[("deal", "D-1"), ("kind", "firing")],
            )],
            &[],
            Answerer::Proposed,
        )
        .await;

    let second = "A later report gives the same deal.";
    let (infer, seen) = scripted(vec![json!({"A": 0.9, "B": 0.05, "0": 0.05})]);
    let result = resolver
        .resolve_document(
            &criterion,
            doc("d1", second),
            &[stmt(
                "repeat",
                second,
                "deal",
                0,
                &[("deal", "D-1"), ("kind", "unregistered")],
            )],
            &[],
            Answerer::Select(&infer),
        )
        .await;

    assert!(matches!(
        result.outcomes[0].outcome,
        Outcome::Decided(Decision::Key { .. })
    ));
    assert_eq!((result.calls, seen.lock().unwrap().len()), (1, 1));
    assert_eq!(resolver.records()[0].fields["kind"], values(&["firing"]));
}

#[tokio::test]
async fn necessary_identity_contaminated_record_matches_neither_member_value() {
    let criterion = necessary(&[]);
    let mut resolver = Resolver::default();
    let seed = "A fire was reported.";
    resolver
        .resolve_document(
            &criterion,
            doc("d0", seed),
            &[stmt("source", seed, "fire", 0, &[("kind", "firing")])],
            &[],
            Answerer::Proposed,
        )
        .await;
    resolver.records[0]
        .fields
        .insert("kind".into(), values(&["firing", "death"]));
    resolver.records[0]
        .supplied
        .insert("kind".into(), values(&["firing", "death"]));

    let (infer, _) = scripted(vec![
        json!({"A": 0.9, "B": 0.05, "0": 0.05}),
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.05, "B": 0.9, "0": 0.05}),
        json!({"A": 0.9, "0": 0.1}),
    ]);
    for (id, body, surface, value) in [
        ("firing", "A fire was seen.", "fire", "firing"),
        ("death", "A death was reported.", "death", "death"),
    ] {
        let result = resolver
            .resolve_document(
                &criterion,
                doc(id, body),
                &[stmt(id, body, surface, 0, &[("kind", value)])],
                &[prop("source")],
                Answerer::Select(&infer),
            )
            .await;
        assert_eq!(
            result.outcomes[0].outcome.record(),
            Some(id),
            "the conflicted record must not match {value}"
        );
        assert_eq!(result.outcomes[0].outcome.label(), "opened");
        assert_eq!(result.vetoed, 1);
    }
    assert_eq!(resolver.records().len(), 3);
    assert_eq!(resolver.records()[0].statements, ["source"]);
}

#[tokio::test]
async fn necessary_identity_same_document_key_bridge_does_not_merge_conflicting_values() {
    let criterion = necessary(&["deal", "thread"]);
    let body = "April report. May report. May follow-up.";
    let statements = [
        stmt(
            "april",
            body,
            "April",
            0,
            &[("deal", "D-1"), ("kind", "firing")],
        ),
        stmt(
            "bridge",
            body,
            "May",
            0,
            &[("deal", "D-1"), ("thread", "T-2"), ("kind", "death")],
        ),
        stmt(
            "followup",
            body,
            "May",
            1,
            &[("thread", "T-2"), ("kind", "death")],
        ),
    ];
    let (infer, seen) = scripted(vec![
        json!({"A": 0.9, "B": 0.05, "0": 0.05}),
        json!({"A": 0.05, "B": 0.9, "0": 0.05}),
        json!({"A": 0.05, "B": 0.9, "0": 0.05}),
    ]);
    let mut resolver = Resolver::default();
    let result = resolver
        .resolve_document(
            &criterion,
            doc("d", body),
            &statements,
            &[],
            Answerer::Select(&infer),
        )
        .await;

    assert_eq!(
        labels(&result),
        ["opened", "refused:contradiction", "refused:contradiction"]
    );
    assert_eq!(result.vetoed, 2);
    assert_eq!(
        result.calls, 0,
        "declared necessary values avoid re-reading"
    );
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(resolver.records().len(), 1);
    assert_eq!(resolver.records()[0].statements, ["april"]);
    assert_eq!(resolver.records()[0].fields["kind"], values(&["firing"]));
}

#[tokio::test]
async fn necessary_identity_missing_value_remains_permissive_and_supplied_evidence_is_folded() {
    let criterion = necessary(&["deal"]);
    let mut resolver = Resolver::default();
    let first = "A firing is reported.";
    resolver
        .resolve_document(
            &criterion,
            doc("d0", first),
            &[stmt(
                "source",
                first,
                "firing",
                0,
                &[("deal", "D-1"), ("kind", "firing")],
            )],
            &[],
            Answerer::Proposed,
        )
        .await;
    let second = "The deal is mentioned again.";
    let result = resolver
        .resolve_document(
            &criterion,
            doc("d1", second),
            &[stmt("repeat", second, "deal", 0, &[("deal", "D-1")])],
            &[],
            Answerer::Proposed,
        )
        .await;

    assert!(matches!(
        result.outcomes[0].outcome,
        Outcome::Decided(Decision::Key { .. })
    ));
    assert_eq!(resolver.records().len(), 1);
    assert_eq!(resolver.records()[0].statements, ["source", "repeat"]);
    assert_eq!(resolver.records()[0].fields["kind"], values(&["firing"]));
    assert_eq!(resolver.records()[0].evidence.len(), 2);

    let mut missing_candidate = Resolver::default();
    let blank = "A report without a readable kind.";
    missing_candidate
        .resolve_document(
            &criterion,
            doc("d2", blank),
            &[stmt("blank-source", blank, "report", 0, &[])],
            &[],
            Answerer::Proposed,
        )
        .await;
    let known = "A firing is described.";
    let (infer, _) = scripted(vec![answer(vec![part("r0", &[("s0", "firing")])])]);
    let joined = missing_candidate
        .resolve_document(
            &criterion,
            doc("d3", known),
            &[stmt("known", known, "firing", 0, &[("kind", "firing")])],
            &[prop("blank-source")],
            Answerer::Model(&infer),
        )
        .await;
    assert_eq!(joined.outcomes[0].outcome.record(), Some("blank-source"));
    assert_eq!(missing_candidate.records().len(), 1);
}

#[tokio::test]
async fn necessary_identity_a_read_conflict_is_weighed_against_the_field() {
    let seed_criterion = necessary(&[]);
    let mut resolver = weights(&[]);
    let first = "The fire happened.";
    let (seed_read, _) = scripted(vec![json!({"A": 0.9, "B": 0.05, "0": 0.05})]);
    resolver
        .resolve_document(
            &seed_criterion,
            threaded("d0", first, "thread-7"),
            &[stmt("source", first, "fire", 0, &[])],
            &[],
            Answerer::Select(&seed_read),
        )
        .await;

    let mut field_criterion = barred();
    field_criterion.necessary = seed_criterion.necessary.clone();
    let second = "The death followed.";
    let (field_read, seen) = scripted(vec![json!({"A": 0.05, "B": 0.9, "0": 0.05})]);
    let result = resolver
        .resolve_document(
            &field_criterion,
            threaded("d1", second, "thread-7"),
            &[stmt("conflict", second, "death", 0, &[])],
            &[],
            Answerer::Select(&field_read),
        )
        .await;

    // The thread agrees (+2.9) and the read kind differs (-2.5), against the
    // prior and the proposed answer's none: opened, and nothing vetoed.
    assert_eq!(result.outcomes[0].outcome.record(), Some("conflict"));
    assert_eq!(result.outcomes[0].outcome.label(), "opened");
    assert_eq!((result.calls, seen.lock().unwrap().len()), (1, 1));
    assert_eq!(result.vetoed, 0);
    assert_eq!(resolver.records().len(), 2);
    assert_eq!(resolver.records()[0].statements, ["source"]);
}

#[tokio::test]
async fn necessary_identity_model_choice_cannot_join_conflicting_record() {
    let criterion = necessary(&[]);
    let mut resolver = Resolver::default();
    let first = "A death was reported.";
    resolver
        .resolve_document(
            &criterion,
            doc("d0", first),
            &[stmt("source", first, "death", 0, &[("kind", "death")])],
            &[],
            Answerer::Proposed,
        )
        .await;

    let second = "A firing was reported.";
    let (infer, _) = scripted(vec![answer(vec![part("r0", &[("s0", "firing")])])]);
    let result = resolver
        .resolve_document(
            &criterion,
            doc("d1", second),
            &[stmt("conflict", second, "firing", 0, &[("kind", "firing")])],
            &[prop("source")],
            Answerer::Model(&infer),
        )
        .await;

    assert_eq!(
        result.outcomes[0].outcome,
        Outcome::Refused(Refusal::Contradiction {
            targets: vec!["source".into()]
        })
    );
    assert_eq!(result.vetoed, 1);
    assert_eq!(resolver.records()[0].statements, ["source"]);
    assert_eq!(resolver.records().len(), 1);
}

#[tokio::test]
async fn necessary_identity_proposal_cannot_join_conflicting_record() {
    let criterion = necessary(&[]);
    let mut resolver = Resolver::default();
    let first = "The fire was contained.";
    resolver
        .resolve_document(
            &criterion,
            doc("d0", first),
            &[stmt("source", first, "fire", 0, &[("kind", "death")])],
            &[],
            Answerer::Proposed,
        )
        .await;

    let second = "The fire was reported again.";
    let result = resolver
        .resolve_document(
            &criterion,
            doc("d1", second),
            &[stmt("conflict", second, "fire", 0, &[("kind", "firing")])],
            &[prop("source")],
            Answerer::Proposed,
        )
        .await;

    assert_eq!(
        result.outcomes[0].outcome,
        Outcome::Refused(Refusal::Contradiction {
            targets: vec!["source".into()]
        })
    );
    assert_eq!(result.vetoed, 1);
    assert_eq!(resolver.records()[0].statements, ["source"]);
    assert_eq!(resolver.records().len(), 1);
}

/// READ shows a necessary attribute with its declared description, as it
/// shows the type's (value meanings there lifted stage on ward's gold
/// statements .525 -> .663, crm-proof loop 13 C2), and asks exactly the old
/// question when the recipe declares none.
#[tokio::test]
async fn read_shows_the_declared_description_of_the_attribute_it_asks() {
    use crate::enrichment::ontology::OntologyTypeDecl;
    let described: OntologyTypeDecl = toml::from_str(
        r#"
name = "happening"
kind = "event"
identity_necessary = ["kind"]
attributes = [{ name = "kind", type = "text", description = "firing: shots fired; death: a person died", values = ["firing", "death"] }]
"#,
    )
    .unwrap();
    let mut bare = described.clone();
    bare.attributes[0].description.clear();
    let body = "The victim died.";
    let mut users = Vec::new();
    for decl in [&described, &bare] {
        let criterion = Criterion::of(decl, vec![]).unwrap();
        let (read, seen) = scripted(vec![json!({"A": 0.1, "B": 0.85, "0": 0.05})]);
        let mut resolver = Resolver::default();
        resolver
            .resolve_document(
                &criterion,
                doc("d0", body),
                &[stmt("x", body, "died", 0, &[])],
                &[],
                Answerer::Select(&read),
            )
            .await;
        assert_eq!(resolver.records()[0].fields["kind"], values(&["death"]));
        users.push(seen.lock().unwrap()[0].user.clone());
    }
    let shown = " (firing: shots fired; death: a person died)";
    assert!(
        users[0].contains(&format!("\nAttribute: kind{shown}\n\nStatement")),
        "{}",
        users[0]
    );
    assert!(
        users[1].contains("\nAttribute: kind\n\nStatement"),
        "{}",
        users[1]
    );
    assert_eq!(users[0].replacen(shown, "", 1), users[1]);
}
