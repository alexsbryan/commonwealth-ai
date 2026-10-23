// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeMap;

use serde_json::json;

use super::super::*;
use super::{replay, Move};
use crate::enrichment::atlas::atoms::Claim;
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

fn mv<'a>(at: &'a str, state: &'a str, from: &[&'a str]) -> Move<'a> {
    Move {
        at,
        state,
        from: (!from.is_empty()).then(|| from.iter().copied().collect()),
    }
}

/// A move with no `from` leaves any state; the last move made is selected and
/// the moves before it are superseded.
#[test]
fn moves_with_no_prior_state_fold_to_the_last_made() {
    let r = replay(
        &[mv("02", "declined", &[]), mv("01", "reported", &[])],
        None,
    );
    assert_eq!(r.state, Some(("02".into(), "declined".into())));
    assert_eq!(r.dispositions, ["selected", "superseded_by_report_time"]);
}

/// A report whose move may not leave the held state is evidence, not a move,
/// and a later move that may leave it is made.
#[test]
fn a_move_is_made_only_from_a_state_it_may_leave() {
    let open = ["reported", "confirmed"];
    let r = replay(
        &[
            mv("01", "reported", &open),
            mv("02", "declined", &[]),
            mv("03", "reported", &open),
            mv("04", "reported", &["declined", "resolved"]),
        ],
        Some("reported"),
    );
    assert_eq!(r.state, Some(("04".into(), "reported".into())));
    assert_eq!(
        r.dispositions,
        [
            "superseded_by_report_time",
            "superseded_by_report_time",
            "not_from_state",
            "selected"
        ]
    );
}

/// With no `initial`, a move that names the states it may leave is never the
/// first; moves into two states at one report time stop the replay.
#[test]
fn no_initial_state_admits_no_from_move_and_a_same_time_split_conflicts() {
    let r = replay(&[mv("01", "confirmed", &["reported"])], None);
    assert_eq!(r.state, None);
    assert_eq!(r.dispositions, ["not_from_state"]);
    let r = replay(
        &[
            mv("01", "confirmed", &[]),
            mv("01", "declined", &[]),
            mv("02", "resolved", &[]),
        ],
        None,
    );
    assert_eq!(r.state, None);
    assert_eq!(r.conflict.as_ref().map(|(at, _)| at.as_str()), Some("01"));
    assert_eq!(r.dispositions, ["conflict", "conflict", "qualified"]);
}

const TRACKER: &str = r#"
[[types]]
name = "person"
kind = "entity"
attributes = [{ name = "login", type = "text" }]
identity = ["login"]
source = { metadata = ["author"], attributes = { login = "value" } }

[[types]]
name = "case"
kind = "entity"
identity_criterion = "the same problem"
attributes = [{ name = "state", type = "text", values = ["reported", "confirmed", "resolved", "declined"], derived = "state_now" }]

[[types]]
name = "case_state"
kind = "claim"
force = "assertive"
subject = "case"
attributes = [{ name = "state", type = "text", values = ["reported", "confirmed", "resolved", "declined"] }]

[change]
document = { date = "date" }

[[sets]]
id = "maintainer_post"
type = "document"
where = { association = ["MEMBER", "OWNER"] }

[[paths]]
id = "maintainer"
path = "document / author / ^author [maintainer_post]"

[[folds]]
id = "state_now"
by = "protocol"
from = ["^subject"]

[folds.protocol]
identity = "state"
initial = "reported"

[[folds.protocol.rules]]
id = "report"
claim_kind = "case_state"
state = "reported"
when = { state = "reported" }
from = ["reported", "confirmed"]

[[folds.protocol.rules]]
id = "reopen"
claim_kind = "case_state"
state = "reported"
when = { state = "reported" }
from = ["resolved", "declined"]
by = "maintainer"

[[folds.protocol.rules]]
id = "confirm"
claim_kind = "case_state"
state = "confirmed"
when = { state = "confirmed" }
by = "maintainer"

[[folds.protocol.rules]]
id = "resolve"
claim_kind = "case_state"
state = "resolved"
when = { state = "resolved" }
by = "maintainer"

[[folds.protocol.rules]]
id = "decline"
claim_kind = "case_state"
state = "declined"
when = { state = "declined" }
by = "maintainer"
"#;

/// The tracker's documents, in order: (author, association, body, the state
/// its one statement reads, the words that cite it). Events carry no
/// association: who made one is known only through the author's other posts.
const POSTS: [(&str, Option<&str>, &str, &str, &str); 5] = [
    (
        "ann",
        Some("NONE"),
        "The export crashes on save.",
        "reported",
        "crashes on save",
    ),
    (
        "max",
        Some("MEMBER"),
        "I can reproduce the crash.",
        "confirmed",
        "reproduce the crash",
    ),
    (
        "max",
        None,
        "closed as not planned",
        "declined",
        "closed as not planned",
    ),
    (
        "ann",
        Some("NONE"),
        "Please reopen, it still crashes.",
        "reported",
        "Please reopen",
    ),
    ("ann", None, "closed", "resolved", "closed"),
];

/// One document and its one statement: who posted it, their association,
/// its text, the statement's kind, the state it reads (none: it carries no
/// field) and the words that cite it.
struct Post<'a> {
    author: &'a str,
    association: Option<&'a str>,
    body: &'a str,
    kind: &'a str,
    state: Option<&'a str>,
    cite: &'a str,
}

/// The state the tracker's case folds to under `declaration`, and each
/// statement's disposition in the fold's history.
fn fold(declaration: &str) -> (DerivedValue, BTreeMap<String, String>) {
    let posts: Vec<Post> = POSTS
        .iter()
        .map(|&(author, association, body, state, cite)| Post {
            author,
            association,
            body,
            kind: "case_state",
            state: Some(state),
            cite,
        })
        .collect();
    fold_posts(declaration, &posts)
}

/// [`fold`] over any posts.
fn fold_posts(declaration: &str, posts: &[Post]) -> (DerivedValue, BTreeMap<String, String>) {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(declaration).unwrap(),
    }
    .policies()
    .unwrap();
    let rows: Vec<EnrichmentChunkRow> = posts
        .iter()
        .enumerate()
        .map(
            |(
                i,
                Post {
                    author,
                    association,
                    body,
                    ..
                },
            )| {
                let mut m =
                    json!({"date": format!("2001-01-0{}T10:00:00Z", i + 1), "author": author});
                if let Some(a) = association {
                    m["association"] = json!(a);
                }
                EnrichmentChunkRow {
                    id: i as u64 + 1,
                    content: body.to_string(),
                    title: None,
                    url: None,
                    metadata_raw: Some(m.to_string()),
                    source_doc_id: Some(format!("d{}", i + 1)),
                }
            },
        )
        .collect();
    let ids: Vec<u64> = (1..=posts.len() as u64).collect();
    let docs = SectionDocuments::from_chunk_rows([("s1", &ids[..])], &rows);
    let projection = crate::enrichment::atlas::project_source_atoms(&docs, &p, "c").unwrap();
    let mut entities = projection.atoms.clone();
    entities.push(
        serde_json::from_value(json!({"id": "entity-case", "canonical_name": "case",
            "entity_type": "case", "first_appearance": {"chunk_id": "s1"}, "description": "",
            "salience": 0.0, "enrichment_depth": "extracted"}))
        .unwrap(),
    );
    let mut claims: Vec<Claim> = posts
        .iter()
        .enumerate()
        .map(|(i, Post { kind, state, cite, .. })| {
            let fields = match state {
                Some(state) => json!({"state": {"status": "supported", "value": state, "evidence": cite}}),
                None => json!({}),
            };
            serde_json::from_value(json!({
                "id": format!("c{}", i + 1), "content": cite, "discourse_act": "assert",
                "epistemic_status": "attributed", "scope": "universal", "subject": "entity-case",
                "evidence": [{"chunk_id": "s1", "passage_preview": cite, "source_doc_id": format!("d{}", i + 1)}],
                "claim_kind": kind, "enrichment_depth": "extracted",
                "attributes": {"__document_read_fields": fields},
            }))
            .unwrap()
        })
        .collect();
    let mut events = Vec::new();
    let (mut states, mut relations, mut ars) = (Vec::new(), Vec::new(), Vec::new());
    let (mut positions, mut oppositions, mut edges) = (Vec::new(), Vec::new(), Vec::new());
    let mut trajectories = BTreeMap::new();
    let mut atoms = BuildAtoms {
        entities: &mut entities,
        events: &mut events,
        states: &mut states,
        relations: &mut relations,
        claims: &mut claims,
        argument_reconstructions: &mut ars,
        positions: &mut positions,
        oppositions: &mut oppositions,
        edges: &mut edges,
        trajectories: &mut trajectories,
    };
    let mut lines = Vec::new();
    derive_attributes(
        &mut atoms,
        &docs,
        &projection.participants,
        &p,
        DeriveStage::AfterResolve,
        &mut |v| lines.push(v.clone()),
    )
    .unwrap();
    let line = lines
        .into_iter()
        .find(|l| l.atom == "entity-case" && l.attribute == "state")
        .expect("the case's state is derived");
    let dispositions = line
        .protocol
        .as_ref()
        .unwrap()
        .history
        .iter()
        .map(|h| {
            (
                h["claim"].as_str().unwrap().to_string(),
                h["disposition"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    (line, dispositions)
}

/// A maintainer's decline, made by an event that carries no association, holds
/// against the user's later plea (evidence: it may not leave `declined`) and
/// the user's own close (evidence: only a maintainer resolves). The same
/// statements folded with no mover or prior state end at the user's close.
#[test]
fn a_state_moves_only_by_whom_and_from_where_its_protocol_says() {
    let (line, d) = fold(TRACKER);
    assert!(
        matches!(&line.outcome, DerivedOutcome::Decided { values, documents, .. }
            if values == &["declined"] && documents == &["d3"]),
        "{line:?}"
    );
    let want = [
        ("c1", "superseded_by_report_time"),
        ("c2", "superseded_by_report_time"),
        ("c3", "selected"),
        ("c4", "not_from_state"),
        ("c5", "unauthorized"),
    ];
    for (claim, disposition) in want {
        assert_eq!(d[claim], disposition, "{claim}: {d:?}");
    }
    let mut plain = TRACKER.to_string();
    for cut in [
        "initial = \"reported\"\n",
        "from = [\"reported\", \"confirmed\"]\n",
        "by = \"maintainer\"\n",
        "[[folds.protocol.rules]]\nid = \"reopen\"\nclaim_kind = \"case_state\"\nstate = \"reported\"\nwhen = { state = \"reported\" }\nfrom = [\"resolved\", \"declined\"]\n",
    ] {
        assert!(plain.contains(cut), "{cut}");
        plain = plain.replace(cut, "");
    }
    let (line, _) = fold(&plain);
    assert!(
        matches!(&line.outcome, DerivedOutcome::Decided { values, .. } if values == &["resolved"]),
        "with no transitions the latest report wins: {line:?}"
    );
}

/// The `by` path's reach is what authorizes, nothing a statement supplies:
/// with no maintainer post at all, no one confirms, resolves or declines.
#[test]
fn with_no_maintainer_post_no_move_needs_one_is_made() {
    let (line, d) = fold(&TRACKER.replace("[\"MEMBER\", \"OWNER\"]", "[\"OWNER\"]"));
    assert!(
        matches!(&line.outcome, DerivedOutcome::Decided { values, .. } if values == &["reported"]),
        "{line:?}"
    );
    for claim in ["c2", "c3", "c5"] {
        assert_eq!(d[claim], "unauthorized", "{claim}: {d:?}");
    }
}

/// The tracker with no mover or prior state: the latest report wins.
fn plain_tracker() -> String {
    let mut plain = TRACKER.to_string();
    for cut in [
        "initial = \"reported\"\n",
        "from = [\"reported\", \"confirmed\"]\n",
        "[[folds.protocol.rules]]\nid = \"reopen\"\nclaim_kind = \"case_state\"\nstate = \"reported\"\nwhen = { state = \"reported\" }\nfrom = [\"resolved\", \"declined\"]\nby = \"maintainer\"\n",
    ] {
        assert!(plain.contains(cut), "{cut}");
        plain = plain.replace(cut, "");
    }
    plain.replace("by = \"maintainer\"\n", "")
}

fn post<'a>(kind: &'a str, state: Option<&'a str>, cite: &'a str) -> Post<'a> {
    Post {
        author: "ann",
        association: Some("NONE"),
        body: cite,
        kind,
        state,
        cite,
    }
}

fn decided(line: &DerivedValue) -> Option<&str> {
    match &line.outcome {
        DerivedOutcome::Decided { values, .. } => values.first().map(String::as_str),
        _ => None,
    }
}

/// Failing input, named: world-s1's 20k read wrote "please nominate 4k/d at
/// PG&E Citygate for January under the existing Index deal" as a lead and
/// moved the won blueville-d1 back (2026-10-18). Written as an act under the
/// case, a later statement moves nothing; with the `acts_under` lever it is a
/// move into the state the recipe names.
#[test]
fn an_act_under_a_record_moves_nothing_unless_the_recipe_says_where() {
    let posts = [
        post(
            "case_state",
            Some("confirmed"),
            "I can reproduce the crash.",
        ),
        post(
            "case_act_under",
            None,
            "Scheduled the fix for the next release.",
        ),
    ];
    let (line, d) = fold_posts(&plain_tracker(), &posts);
    assert_eq!(decided(&line), Some("confirmed"), "{line:?}");
    assert_eq!(d["c2"], "unmatched_kind", "{d:?}");
    let lever = plain_tracker().replace(
        "identity = \"state\"\n",
        "identity = \"state\"\nacts_under = \"resolved\"\n",
    );
    let (line, d) = fold_posts(&lever, &posts);
    assert_eq!(decided(&line), Some("resolved"), "{line:?} {d:?}");
}

/// Failing input, named: halwick-d1's "The Nov-Mar deal is being rolled into a
/// new arrangement" is lost under the unchanged recipe (2026-10-13) and went
/// unwritten at 20k. A replaced statement is a kind of the fold's claim kind,
/// so its state is read from the recipe's values; with the `replaced` lever it
/// carries none and the fold moves the record to the state named. Both opt in.
#[test]
fn a_replaced_record_takes_the_state_its_rules_or_its_lever_give() {
    let posts = [
        post(
            "case_state",
            Some("confirmed"),
            "I can reproduce the crash.",
        ),
        post(
            "case_replaced",
            Some("declined"),
            "Folded into the new import issue.",
        ),
    ];
    let opted = plain_tracker().replace(
        "identity = \"state\"\n",
        "identity = \"state\"\nlifecycle = true\n",
    );
    let (line, d) = fold_posts(&opted, &posts);
    assert_eq!(decided(&line), Some("declined"), "{line:?} {d:?}");
    let lever = plain_tracker().replace(
        "identity = \"state\"\n",
        "identity = \"state\"\nreplaced = \"declined\"\n",
    );
    let posts = [
        post(
            "case_state",
            Some("confirmed"),
            "I can reproduce the crash.",
        ),
        post("case_replaced", None, "Folded into the new import issue."),
    ];
    let (line, d) = fold_posts(&lever, &posts);
    assert_eq!(decided(&line), Some("declined"), "{line:?} {d:?}");
}
