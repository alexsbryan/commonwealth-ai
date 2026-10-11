use std::collections::BTreeMap;

use serde_json::json;

use super::super::tests::scripted;
use super::*;

fn quantity() -> AttrFamily {
    AttrFamily::Quantity { unit: None }
}

#[test]
fn words_are_the_whitespace_separated_spans() {
    let text = " a  bc\nd ";
    let got: Vec<&str> = words(text).into_iter().map(|r| &text[r]).collect();
    assert_eq!(got, ["a", "bc", "d"]);
}

#[test]
fn a_quantity_is_the_one_number_its_words_write_in_digits() {
    for (words, n) in [
        ("2,400 dollars", 2400.0),
        ("$1,234.50", 1234.5),
        ("(-3)", -3.0),
        ("about 12 units", 12.0),
    ] {
        assert_eq!(normalise(&quantity(), words).unwrap(), json!(n), "{words}");
    }
    for words in ["2.4M", "two", "2,40", "1.2.3", "from 2 to 3"] {
        assert!(normalise(&quantity(), words).is_err(), "{words}");
    }
}

#[test]
fn a_time_is_iso_when_code_reads_a_date_and_as_written_otherwise() {
    let point = AttrFamily::Time { range: false };
    assert_eq!(
        normalise(&point, "2026-04-30").unwrap(),
        json!("2026-04-30")
    );
    assert_eq!(
        normalise(&point, "the end of April").unwrap(),
        json!("the end of April")
    );
    let text = AttrFamily::Text { values: vec![] };
    assert_eq!(
        normalise(&text, "oak  chairs").unwrap(),
        json!("oak  chairs")
    );
}

fn pointing<'a>(infer: &'a InferenceFn) -> Pointing<'a> {
    Pointing {
        infer,
        facts: "FACTS\n\n",
        head: "HEAD".into(),
        what: "the thing".into(),
        phase: POINT_PHASE,
        document: "d",
        item: "s",
    }
}

fn dist(labels: &[&str], pick: &str) -> serde_json::Value {
    let m: serde_json::Map<String, serde_json::Value> = labels
        .iter()
        .map(|l| (l.to_string(), json!(if *l == pick { 0.9 } else { 0.01 })))
        .collect();
    serde_json::Value::Object(m)
}

/// A statement of 30 words: the first window's "none" moves to the second,
/// where the start is chosen, then the end among the words from there.
#[tokio::test]
async fn a_span_is_pointed_at_by_its_start_then_its_end_a_window_at_a_time() {
    let text: String = (0..30).map(|i| format!("w{i} ")).collect();
    let first: Vec<&str> = LABELS.iter().copied().chain([NONE]).collect();
    let second = ["A", "B", "C", "D", "E", NONE];
    let tail = ["A", "B", "C", "D"];
    let (infer, seen) = scripted(vec![
        dist(&first, NONE),
        dist(&second, "B"),
        dist(&tail, "C"),
    ]);
    let mut calls = 0;
    let span = point_span(&pointing(&infer), &text, &mut calls)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&text[span], "w26 w27 w28");
    assert_eq!(calls, 3);
    let prompts = seen.lock().unwrap();
    assert!(prompts[0].user.starts_with("FACTS\n\nHEAD\n\nThe words"));
    assert!(
        prompts[1].user.contains("A w25\nB w26\n"),
        "{}",
        prompts[1].user
    );
    assert!(prompts[2]
        .user
        .contains("It starts at the word \"w26\". The words from there, each under a letter:\nA w26\nB w27\nC w28\nD w29\nAt which word does the thing end?"));
    assert!(prompts
        .iter()
        .all(|p| p.phase_id.as_deref() == Some(POINT_PHASE)));
}

#[tokio::test]
async fn no_start_chosen_is_not_stated_and_a_last_word_start_asks_no_end() {
    let (infer, _) = scripted(vec![dist(&["A", "B", NONE], NONE)]);
    let mut calls = 0;
    assert_eq!(
        point_span(&pointing(&infer), "x y", &mut calls).await,
        Ok(None)
    );
    let (infer, _) = scripted(vec![dist(&["A", "B", NONE], "B")]);
    let mut calls = 0;
    let span = point_span(&pointing(&infer), "x y", &mut calls)
        .await
        .unwrap();
    assert_eq!(span, Some(2..3));
    assert_eq!(calls, 1);
    let (infer, _) = scripted(vec![json!("A")]);
    assert!(point_span(&pointing(&infer), "x y", &mut calls)
        .await
        .is_err());
}

fn ask<'a>(
    infer: &'a InferenceFn,
    candidates: &'a BTreeMap<String, super::super::pick::Candidates>,
) -> Ask<'a> {
    Ask {
        infer,
        facts: "",
        document: "d",
        statement: "l1-1",
        body: "Our price is 2,400 dollars.",
        at: 0..27,
        evidence: "Our price is 2,400 dollars.",
        candidates,
    }
}

#[tokio::test]
async fn a_pointed_quantity_is_its_number_cited_by_its_words_and_unmeasured() {
    let owner = OntologyTypeDecl {
        name: "offer".into(),
        ..Default::default()
    };
    let attr = AttrDecl {
        name: "price".into(),
        family: AttrFamily::Quantity {
            unit: Some("USD".into()),
        },
        description: String::new(),
        derived: None,
        by: None,
    };
    let none = BTreeMap::new();
    // Start at "2,400", end at "dollars." (the second of the two words left).
    let (infer, seen) = scripted(vec![
        dist(&["A", "B", "C", "D", "E", NONE], "D"),
        dist(&["A", "B"], "B"),
    ]);
    let mut calls = 0;
    let read = point(&ask(&infer, &none), &owner, &attr, &mut calls).await;
    assert_eq!(
        read,
        DocumentReadField::Supported {
            value: json!(2400.0),
            evidence: "2,400 dollars.".into(),
            by: Some(SourcePrecision::new("reader_point", Precision::Unmeasured)),
        }
    );
    assert!(seen.lock().unwrap()[0]
        .user
        .starts_with("Type: offer\nAttribute: price, a number, in USD\n\nStatement, its words in [[ ]]: \"…[[Our price is 2,400 dollars.]]…\""));
    // Pointed at words with no number: refused for this field alone.
    let (infer, _) = scripted(vec![
        dist(&["A", "B", "C", "D", "E", NONE], "A"),
        dist(&["A", "B", "C", "D", "E"], "B"),
    ]);
    let read = point(&ask(&infer, &none), &owner, &attr, &mut calls).await;
    assert!(
        matches!(&read, DocumentReadField::Unknown { reason } if reason.starts_with("refused: the words \"Our price\" hold no number")),
        "{read:?}"
    );
}
