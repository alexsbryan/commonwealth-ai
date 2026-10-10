// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE's one decider on the default path, beside C1-C6 (campaign
//! ontology-layer E3; ONTOLOGY_METHOD §Identity). Every fixture type RESOLVE
//! decides declares an `identity_bar`, so the decider has an unsettled band.

use super::*;

/// E3: a statement the decider held in its document is settled after the last
/// one, so the decisions leave nothing held; the held counts before and after
/// are printed per fixture.
#[tokio::test]
async fn nothing_is_left_held_after_the_last_document() {
    let mut judged = Vec::new();
    for shape in SHAPES {
        let run = run(&Fixture::load(shape)).await;
        let (mut before, mut settled) = (0, 0);
        let mut last: BTreeMap<String, bool> = BTreeMap::new();
        for d in run.lines(DECISIONS_FILE) {
            let settles = d["settles"].as_bool().unwrap_or(false);
            for o in d["outcomes"].as_array().unwrap() {
                let held = o["outcome"]["held"].is_object();
                if settles {
                    settled += 1;
                } else if held {
                    before += 1;
                }
                last.insert(o["statement"].as_str().unwrap().to_string(), held);
            }
        }
        let after = last.values().filter(|h| **h).count();
        eprintln!("{shape}: {before} statement(s) held in their document, {after} after the last");
        assert_eq!(after, 0, "{shape}: a statement is left held");
        assert_eq!(
            settled, before,
            "{shape}: settled a statement that was not held"
        );
        if before > 0 {
            judged.push(shape);
        }
    }
    assert!(
        !judged.is_empty(),
        "no fixture held a statement, so E3 judged none"
    );
}
