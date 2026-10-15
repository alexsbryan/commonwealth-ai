// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the records themselves carry for a reader who never opens the build's
//! audit files: every fold's outcome beside the attribute it folds, decided or
//! not, with the readings it came from, each cited by its statement, document
//! and words; and every link a statement's subject makes, with the model's
//! choice scores.

use super::*;

/// The fold record an atom keeps for `attr` (`__folds`).
fn fold<'a>(atom: &'a Value, attr: &str) -> &'a Value {
    &atom["data"]["attributes"]["__folds"][attr]
}

/// The mail fixture with an order attribute its statements read from closed
/// values and the record folds by `agree` (`finish`).
fn with_finish() -> Fixture {
    let mut f = Fixture::load("mail");
    let piece = "  { name = \"piece\", type = \"text\", description = \"what is to be made\" },\n";
    assert!(f.recipe.contains(piece), "the mail recipe declares `piece`");
    f.recipe = f.recipe.replace(
        piece,
        &format!(
            "{piece}  {{ name = \"finish\", type = \"text\", values = [\"oiled\", \"waxed\", \"painted\"], \
             by = \"agree\", description = \"how the piece is finished\" }},\n"
        ),
    );
    f
}

/// A record whose statements read one value holds it, its readings cited; a
/// record whose statements disagree holds no value, so no filter can match
/// it, and keeps the disagreement as a tagged outcome with every reading
/// cited. The oracle picks each document's value from its own bytes
/// (`Choose::Spread`), so an order's documents read different finishes.
#[tokio::test]
async fn a_record_whose_statements_disagree_shows_each_reading_cited() {
    let store = tempfile::tempdir().unwrap();
    let run = run_choosing(&with_finish(), Asker::Daemon, store.path(), Choose::Spread).await;
    let atoms = run.atoms();
    let claims: BTreeSet<&str> = atoms
        .iter()
        .filter(|a| a["atom_type"] == "Claim")
        .filter_map(|a| a["data"]["id"].as_str())
        .collect();
    let lines: Vec<Value> = run
        .lines(DERIVED_FILE)
        .into_iter()
        .filter(|l| l["type"] == "order" && l["attribute"] == "finish")
        .collect();
    assert!(!lines.is_empty(), "no order folds `finish`");
    let (mut decided, mut disagreed) = (0, 0);
    for line in &lines {
        let atom = atoms
            .iter()
            .find(|a| a["data"]["id"] == line["atom"])
            .unwrap_or_else(|| panic!("no record for {line}"));
        let folded = fold(atom, "finish");
        assert_eq!(folded["outcome"], line["outcome"], "{atom}");
        let readings = folded["readings"].as_array().cloned().unwrap_or_default();
        for r in &readings {
            assert!(
                r["claim"].as_str().is_some_and(|c| claims.contains(c)),
                "a reading cites no statement: {r}"
            );
            assert!(r["document"].is_string(), "{r}");
            assert!(
                r["evidence"].as_str().is_some_and(|e| !e.is_empty()),
                "a reading quotes nothing: {r}"
            );
        }
        let values: BTreeSet<String> = readings.iter().map(|r| r["value"].to_string()).collect();
        if line["outcome"] == "decided" {
            decided += 1;
            assert!(
                !readings.is_empty(),
                "a decided value cites no reading: {atom}"
            );
            assert!(atom["data"]["attributes"]["finish"].is_string(), "{atom}");
        } else if line["outcome"] != "nothing" {
            disagreed += 1;
            assert!(
                atom["data"]["attributes"].get("finish").is_none(),
                "a {} outcome is held as a value: {atom}",
                line["outcome"]
            );
            assert!(values.len() > 1, "a disagreement shows one value: {folded}");
        }
    }
    assert!(decided + disagreed > 0, "mail: no order read a finish");
    assert!(
        disagreed > 0,
        "mail: no order's statements disagree on `finish`: {lines:?}"
    );
}

/// Every statement RESOLVE placed carries how its subject was decided, and a
/// statement the model chose for carries the model's choice scores: the
/// chosen record's, the runner-up record's and none's. They are the model's
/// uncalibrated choice over the candidates it was shown, kept for inspection,
/// never read as the probability a link is right.
#[tokio::test]
async fn every_placed_statement_says_how_its_subject_was_decided() {
    const PARTIES: &[(&str, &str, &str)] = &[
        ("company", "Birch Hall Trust", "birchhall.example"),
        ("company", "Alder and Pine Workshop", "alderpine.example"),
        ("order", "oak chairs", "oak chairs"),
    ];
    let run = run_knowing(&Fixture::ingest("mailraw").await, PARTIES).await;
    let atoms = run.atoms();
    let ids: BTreeSet<&str> = atoms
        .iter()
        .filter_map(|a| a["data"]["id"].as_str())
        .collect();
    let placed: Vec<&Value> = atoms
        .iter()
        .filter(|a| a["atom_type"] == "Claim" && a["data"]["subject"].is_string())
        .collect();
    assert!(!placed.is_empty(), "no statement was placed");
    let mut chosen = 0;
    for c in &placed {
        let link = &c["data"]["attributes"]["__resolve_link"];
        let how = link["decided_by"]
            .as_str()
            .unwrap_or_else(|| panic!("a placed statement says nothing of its link: {c}"));
        if how != "chosen" {
            continue;
        }
        chosen += 1;
        let score = link["choice_score"]
            .as_f64()
            .unwrap_or_else(|| panic!("{link}"));
        assert!(link["none_score"].is_f64(), "{link}");
        if let Some(runner) = link.get("runner_up").filter(|r| !r.is_null()) {
            assert!(
                runner["score"].as_f64().is_some_and(|s| s <= score),
                "{link}"
            );
            assert!(
                runner["record"].as_str().is_some_and(|r| ids.contains(r)),
                "the runner-up is no record: {link}"
            );
        }
    }
    assert!(chosen > 0, "the model chose no statement's record");
}
