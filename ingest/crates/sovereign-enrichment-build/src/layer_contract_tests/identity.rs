// SPDX-License-Identifier: AGPL-3.0-or-later
//! RESOLVE's one decider on the default path, beside C1-C6 (campaign
//! ontology-layer E3; ONTOLOGY_METHOD §Identity). Every fixture type RESOLVE
//! decides declares an `identity_bar`, so the decider has an unsettled band.

use super::*;

/// E3 under the model's choice (`select.rs`, `settle.rs`): a statement the
/// model was asked about is decided in its document by its pick, the record
/// it picked or a new one when it picked none, and is never held there; one
/// held because no record was shown to the model is settled after the last
/// document, so the decisions leave nothing held. The counts are printed per
/// fixture.
#[tokio::test]
async fn an_asked_statement_is_decided_by_the_models_pick_and_nothing_is_left_held() {
    let mut chosen = 0;
    for shape in SHAPES {
        let run = run(&Fixture::load(shape)).await;
        let (mut asked, mut before, mut settled) = (0, 0, 0);
        let mut last: BTreeMap<String, bool> = BTreeMap::new();
        for d in run.lines(DECISIONS_FILE) {
            let settles = d["settles"].as_bool().unwrap_or(false);
            // A record this document opened is shown by its opening statement's id.
            let own: BTreeSet<&str> = d["outcomes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|o| o["statement"].as_str())
                .collect();
            for o in d["outcomes"].as_array().unwrap() {
                let held = o["outcome"]["held"].is_object();
                if let Some(choice) = o["choice"].as_object() {
                    asked += 1;
                    assert!(!held, "{shape}: an asked statement is held: {o}");
                    // The most probable label; a tie goes to the later one, so to none.
                    let (pick, _) = choice["candidates"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| (c[0].as_str(), c[1].as_f64().unwrap()))
                        .chain([(None, choice["none"].as_f64().unwrap())])
                        .fold(
                            (None, f64::NEG_INFINITY),
                            |a, b| if b.1 >= a.1 { b } else { a },
                        );
                    let decided = &o["outcome"]["decided"];
                    match decided["decision"].as_str() {
                        Some("chosen") => {
                            assert_eq!(decided["record"].as_str(), pick, "{shape}: {o}");
                            chosen += 1;
                        }
                        Some("opened") => assert!(
                            pick.is_none_or(|id| own.contains(id)),
                            "{shape}: picked a record yet opened one: {o}"
                        ),
                        _ => panic!(
                            "{shape}: an asked statement decided by neither its pick nor none: {o}"
                        ),
                    }
                }
                if settles {
                    settled += 1;
                } else if held {
                    before += 1;
                }
                last.insert(o["statement"].as_str().unwrap().to_string(), held);
            }
        }
        let after = last.values().filter(|h| **h).count();
        eprintln!(
            "{shape}: {asked} statement(s) asked; {before} held in their document, {after} after the last"
        );
        assert_eq!(after, 0, "{shape}: a statement is left held");
        assert_eq!(
            settled, before,
            "{shape}: settled a statement that was not held"
        );
    }
    assert!(
        chosen > 0,
        "no asked statement linked a record it was shown"
    );
}

/// The mail fixture with a sixth message in Rosa's thread asking for a second
/// piece of work, so the thread is about two orders.
fn with_a_second_order_in_the_thread() -> Fixture {
    let mut f = Fixture::load("mail");
    let desk = json!({"id": "q6@birchhall.example", "thread_id": "q1@birchhall.example",
        "from": "Rosa Lind <rosa.lind@birchhall.example>",
        "to": "Tom Avery <tom.avery@alderpine.example>",
        "date": "Fri, 06 Mar 2026 11:00:00 -0800", "subject": "A writing desk as well",
        "body": "Hello Tom,\n\nOur director would also like a cherry writing desk for her office upstairs.\n\
                 Could you price the writing desk separately and say when you could make it?\n\nRosa Lind"});
    f.documents.push(desk.as_object().unwrap().clone());
    f
}

/// A record holds its documents (ONTOLOGY_METHOD §The core): a reply that
/// states nothing (every line under the oracle's six words) is held, by its
/// declared id, by the record its thread's statements sit on when they sit on
/// one, and by none when they sit on several (Rosa's thread once it also asks
/// for a desk, answered by a model that tells the two orders apart); a holding
/// says its source.
#[tokio::test]
async fn a_document_that_states_nothing_is_held_by_its_threads_one_record() {
    const ORDERS: &[(&str, &str, &str)] = &[
        ("order", "writing desk", "writing desk"),
        ("order", "oak chairs", "oak chairs"),
    ];
    let (mut held, mut unheld) = (0, 0);
    for (shape, run, quiet) in [
        (
            "issues",
            run(&Fixture::load("issues")).await,
            [("comment-102-1", "102"), ("comment-101-4", "101")].as_slice(),
        ),
        (
            "mail",
            run(&Fixture::load("mail")).await,
            &[("q5@alderpine.example", "q1@birchhall.example")],
        ),
        (
            "mail with a desk",
            run_knowing(&with_a_second_order_in_the_thread(), ORDERS).await,
            &[("q5@alderpine.example", "q1@birchhall.example")],
        ),
    ] {
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
