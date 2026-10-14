use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use super::of_claims;

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
    let d = of_claims(&fields_of, &chapter_subject_of, &carried);
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
