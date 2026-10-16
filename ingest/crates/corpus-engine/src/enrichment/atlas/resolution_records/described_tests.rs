use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use super::{of_claims, ReadNames};
use crate::enrichment::pipeline::document_read::{OTHER_NAMES_ATTRIBUTE, SUBJECT_NAME_ATTRIBUTE};

fn fields(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// Failing input, named: a window's reply whose own document states no field
/// of its subject. Its description is the union of what the window read of
/// that subject in its other documents (claim 0's product, claim 1's place
/// and number), never the keys or necessary values, which travel apart; a
/// claim of another subject, and one of no chapter subject, keep their own.
#[test]
fn a_window_subjects_claims_share_everything_the_window_read_of_it() {
    let fields_of = HashMap::from([
        (
            0,
            fields(json!({"product": "index gas", "term": "a year or more"})),
        ),
        (1, fields(json!({"delivery_point": "Waha", "number": 7}))),
        (2, fields(json!({}))),
        (3, fields(json!({"product": "power"}))),
        (4, fields(json!({"product": "storage"}))),
    ]);
    let chapter_subject_of = HashMap::from([
        (0, "sec_1/p1".to_string()),
        (1, "sec_1/p1".to_string()),
        (2, "sec_1/p1".to_string()),
        (3, "sec_1/p2".to_string()),
    ]);
    let carried = BTreeSet::from(["term"]);
    let d = of_claims(&fields_of, &HashMap::new(), &chapter_subject_of, &carried);
    let reply = &d[&2];
    assert_eq!(
        reply.values["product"],
        BTreeSet::from(["index gas".into()])
    );
    assert_eq!(
        reply.values["delivery_point"],
        BTreeSet::from(["Waha".into()])
    );
    assert_eq!(reply.values["number"], BTreeSet::from(["7".into()]));
    assert!(
        !reply.values.contains_key("term"),
        "a necessary value travels as a read"
    );
    assert_eq!(d[&0], *reply);
    assert_eq!(d[&1], *reply);
    assert_eq!(d[&3].values["product"], BTreeSet::from(["power".into()]));
    assert_eq!(d[&3].values.len(), 1);
    assert_eq!(d[&4].values["product"], BTreeSet::from(["storage".into()]));
    assert_eq!(d[&4].values.len(), 1);
}

/// Failing input, named: a novel's reader named the subject of each passage
/// ("the old ledger", "the tally book"), and RESOLVE's question showed none of
/// it: an object no field is declared for reached RESOLVE described by
/// nothing, and the model judged the passage's topic instead of its subject
/// (five wrong RESOLVE answers, saltgrass, 2026-10-15). The names a window
/// gave a subject travel with its description, shared across its claims like
/// its values; a claim of no chapter subject keeps its own.
#[test]
fn a_window_subjects_names_travel_with_its_description() {
    let fields_of = HashMap::from([(0, fields(json!({"craft": "canoe"})))]);
    let named = |name: &str| ReadNames {
        names: vec![name.to_string()],
        others: Map::new(),
    };
    let names_of = HashMap::from([
        (0, named("the old ledger")),
        (1, named("the tally book")),
        (3, named("the iron bar")),
        (5, named("a lantern")),
    ]);
    let chapter_subject_of = HashMap::from([
        (0, "sec_1/p1".to_string()),
        (1, "sec_1/p1".to_string()),
        (2, "sec_1/p1".to_string()),
        (3, "sec_1/p2".to_string()),
    ]);
    let d = of_claims(&fields_of, &names_of, &chapter_subject_of, &BTreeSet::new());
    let both = BTreeSet::from(["the old ledger".to_string(), "the tally book".to_string()]);
    assert_eq!(
        d[&2].names, both,
        "a claim of the subject with no name of its own"
    );
    assert_eq!(d[&1].names, both);
    assert_eq!(d[&1].values["craft"], BTreeSet::from(["canoe".into()]));
    assert_eq!(d[&3].names, BTreeSet::from(["the iron bar".to_string()]));
    assert_eq!(d[&5].names, BTreeSet::from(["a lantern".to_string()]));
}

/// Failing input, named: world-s5's window read a counterparty a document
/// said had a new name, and RESOLVE's cards showed one name on each side, so
/// the model read two companies (pellica-d1, arm bundle1-fresh). The other
/// names a window gave the things a statement names (`OTHER_NAMES_ATTRIBUTE`,
/// the answer's typed `other_names`) show beside its subject's name and its
/// readings' values; a name with none, or a value no other name is given
/// for, shows alone; a name is never split.
#[test]
fn other_names_show_beside_a_subjects_name_and_a_readings_value() {
    let fields_of = HashMap::from([
        (0, fields(json!({"counterparty": "Coastline", "product": "gas"}))),
        (1, fields(json!({"counterparty": "Input / Output"}))),
    ]);
    let read = ReadNames::of(&fields(json!({
        SUBJECT_NAME_ATTRIBUTE: "the Q1 job",
        OTHER_NAMES_ATTRIBUTE: {"Coastline": ["Bayside"], "the Q1 job": ["the winter job"]},
    })))
    .expect("the statement names things with other names");
    let names_of = HashMap::from([(0, read)]);
    let d = of_claims(&fields_of, &names_of, &HashMap::new(), &BTreeSet::new());
    assert_eq!(
        d[&0].values["counterparty"],
        BTreeSet::from(["Coastline".to_string(), "Bayside".to_string()])
    );
    assert_eq!(d[&0].values["product"], BTreeSet::from(["gas".to_string()]));
    assert_eq!(
        d[&0].names,
        BTreeSet::from(["the Q1 job".to_string(), "the winter job".to_string()])
    );
    assert_eq!(
        d[&1].values["counterparty"],
        BTreeSet::from(["Input / Output".to_string()]),
        "one name, never split"
    );
}
