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

/// A record holds its documents (ONTOLOGY_METHOD §The core): a reply that
/// states nothing (every line under the oracle's six words) is held, by its
/// declared id, by the record its thread's statements sit on when they sit on
/// one, and by none when they sit on several; a holding says its source.
#[tokio::test]
async fn a_document_that_states_nothing_is_held_by_its_threads_one_record() {
    let (mut held, mut unheld) = (0, 0);
    for (shape, quiet) in [
        (
            "issues",
            [("comment-102-1", "102"), ("comment-101-4", "101")].as_slice(),
        ),
        ("mail", &[("q5@alderpine.example", "q1@birchhall.example")]),
    ] {
        let run = run(&Fixture::load(shape)).await;
        let atoms = run.atoms();
        let claims: Vec<&Value> = atoms.iter().filter(|a| a["atom_type"] == "Claim").collect();
        for (quiet, thread) in quiet {
            assert!(
                !claims
                    .iter()
                    .any(|c| c["data"]["attributes"]["document_id"] == *quiet),
                "{shape}: {quiet} states something, so it tests nothing here"
            );
            let on: BTreeSet<&str> = claims
                .iter()
                .filter(|c| c["data"]["attributes"]["document_thread"] == *thread)
                .filter_map(|c| c["data"]["subject"].as_str())
                .collect();
            let holders: Vec<&str> = atoms
                .iter()
                .filter(|a| {
                    a["data"]["attributes"]["thread_documents"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|id| id == quiet))
                })
                .filter_map(|a| a["data"]["id"].as_str())
                .collect();
            eprintln!(
                "{shape}: thread {thread} sits on {} record(s); {quiet} is held by {holders:?}",
                on.len()
            );
            if on.len() != 1 {
                assert!(
                    holders.is_empty(),
                    "{shape}: a thread on {on:?} placed {quiet}"
                );
                unheld += 1;
                continue;
            }
            assert_eq!(holders, on.into_iter().collect::<Vec<_>>(), "{shape}");
            let line = run
                .lines(DERIVED_FILE)
                .into_iter()
                .find(|v| v["atom"] == holders[0] && v["attribute"] == "thread_documents")
                .unwrap_or_else(|| panic!("{shape}: no derived line for the holding"));
            assert_eq!(line["by"]["source"], "derived:document_thread", "{shape}");
            held += 1;
        }
    }
    assert!(
        held > 0 && unheld > 0,
        "judged {held} holding(s) and {unheld} refusal(s); want both"
    );
}
