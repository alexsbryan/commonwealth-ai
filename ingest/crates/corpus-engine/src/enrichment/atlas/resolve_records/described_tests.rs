//! A statement's description: what its reader read its particular to be is
//! shown beside it, folded onto the record it opens or joins, and shown on that
//! record's card in a later question. Code never decides by it.

use super::*;

fn described(values: &[(&str, &str)]) -> Described {
    let mut d = Described::default();
    for (attr, value) in values {
        d.values
            .entry(attr.to_string())
            .or_default()
            .insert(value.to_string());
    }
    d
}

/// Failing input, named: a reply ("We can price that at .10.") whose words say
/// nothing of which particular it is about, asked against a record whose
/// statements' lines say nothing either. The question shows what the reply's
/// reader read it to be beside it, and the record by what its readers read it
/// to be; the model's pick of none opens a record, as with no description.
#[tokio::test]
async fn a_question_shows_what_readers_read_each_side_to_be() {
    let mut res = Resolver::default();
    let a = "Index gas for Faircroft, please.";
    let mut first = stmt("x", a, "Index gas for Faircroft", 0, &[]);
    first.described = described(&[("delivery_point", "Waha"), ("product", "index gas")]);
    res.resolve_document(
        &criterion(&[]),
        doc("d0", a),
        &[first],
        &[],
        Answerer::Proposed,
    )
    .await;
    assert_eq!(
        res.records()[0].described.values["delivery_point"],
        BTreeSet::from(["Waha".to_string()])
    );

    let (infer, seen) = scripted(vec![json!({"A": 0.2, "0": 0.8})]);
    let b = "We can price that at .10.";
    let mut reply = stmt("z", b, "We can price that at .10", 0, &[]);
    reply.described = described(&[("delivery_point", "San Juan")]);
    let r = res
        .resolve_document(
            &criterion(&[]),
            doc("d1", b),
            &[reply],
            &[prop("x")],
            Answerer::Select(&infer),
        )
        .await;
    let u = &seen.lock().unwrap()[0].user;
    assert!(
        u.contains("Its reader read what it is about as: delivery_point: San Juan\n"),
        "{u}"
    );
    assert!(
        u.contains("; delivery_point: Waha; product: index gas\n"),
        "the candidate's card: {u}"
    );
    assert_eq!(labels(&r), ["opened"]);
    assert_eq!(res.records().len(), 2);
}

/// Control: a statement no reader described is asked exactly as before, with
/// no description line.
#[tokio::test]
async fn an_undescribed_statement_is_asked_without_a_description() {
    let mut res = Resolver::default();
    let a = "Index gas for Faircroft, please.";
    res.resolve_document(
        &criterion(&[]),
        doc("d0", a),
        &[stmt("x", a, "Index gas for Faircroft", 0, &[])],
        &[],
        Answerer::Proposed,
    )
    .await;
    let (infer, seen) = scripted(vec![json!({"A": 0.8, "0": 0.2})]);
    let b = "We can price that at .10.";
    res.resolve_document(
        &criterion(&[]),
        doc("d1", b),
        &[stmt("z", b, "We can price that at .10", 0, &[])],
        &[prop("x")],
        Answerer::Select(&infer),
    )
    .await;
    let u = &seen.lock().unwrap()[0].user;
    assert!(!u.contains("Its reader read"), "{u}");
    assert!(u.contains("no declared value"), "{u}");
}
