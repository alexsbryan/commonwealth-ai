//! What keeps a statement from a record (`apart.rs`): the window reader's
//! distinctions and the declared necessary values.

use super::*;
/// A record holding another reference of the asked statement's window is
/// offered, marked as the window's reader's other particular, and the model's
/// choice decides. Failing input, named: world-s1 coyoteville-d1 (2026-10-14),
/// a window's two references of one deal, one joined the deal's record first
/// and the other was never offered it, so the deal stayed split.
#[tokio::test]
async fn a_record_holding_another_reference_of_the_window_is_offered_marked() {
    let (infer, seen) = scripted(vec![
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.9, "0": 0.1}),
    ]);
    let mut res = weights(&[("proposed_answer", 0.5, 0.4)]);
    let c = criterion(&[]);
    let mark = "read this record's particular as another";
    for (id, body, chapter) in [
        ("a", "The storage deal is proposed.", "sec_1/p1"),
        ("b", "The storage deal is off.", "sec_2/p1"),
        ("c", "The storage deal is back on.", "sec_2/p2"),
    ] {
        let proposals = if id == "a" {
            vec![]
        } else {
            vec![similar("a", 0.5)]
        };
        res.resolve_document(
            &c,
            doc(id, body),
            &[stmt(
                id,
                body,
                "storage",
                0,
                &[(CHAPTER_SUBJECT_KEY, chapter)],
            )],
            &proposals,
            Answerer::Select(&infer),
        )
        .await;
    }
    assert_eq!(
        res.records()[0].statements,
        ["a", "b", "c"],
        "c was offered a's record and chose it"
    );
    let asked = seen.lock().unwrap();
    assert_eq!(asked.len(), 2);
    assert!(
        !asked[0].user.contains(mark),
        "b's window holds nothing of a's record yet"
    );
    assert!(asked[1].user.contains(mark), "{}", asked[1].user);
}

/// A record wholly of the statement's own window's other particulars stays
/// apart (Ward's TEP agreements, read apart in one window, folded into one
/// record when RESOLVE was asked, 2026-10-14); one that also holds another
/// window's reading is RESOLVE's to decide (world-s1 coyoteville-d1).
#[test]
fn a_record_wholly_of_the_statements_window_stays_apart() {
    use super::super::apart::read_apart;
    let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert!(read_apart(
        Some(&set(&["sec_1/p2"])),
        Some(&set(&["sec_1/p1"]))
    ));
    assert!(read_apart(
        Some(&set(&["sec_1/p2"])),
        Some(&set(&["sec_1/p1", "sec_1/p4"]))
    ));
    assert!(!read_apart(
        Some(&set(&["sec_2/p5"])),
        Some(&set(&["sec_1/p7", "sec_2/p18"]))
    ));
    assert!(!read_apart(
        Some(&set(&["sec_2/p1"])),
        Some(&set(&["sec_1/p1"]))
    ));
    assert!(!read_apart(
        Some(&set(&["sec_1/p1"])),
        Some(&set(&["sec_1/p1"]))
    ));
    assert!(!read_apart(Some(&set(&["sec_1/p1"])), None));
}

/// Failing input, named: the Ward window read eight deals, p1 to p8, and
/// RESOLVE's forced choices folded them into three records (2026-10-13). Two
/// references of one window never join; one of another window says nothing.
#[test]
fn two_references_of_one_window_never_join() {
    let c = criterion(&[]);
    let of = |values: &[&str]| {
        BTreeMap::from([(
            CHAPTER_SUBJECT_KEY.to_string(),
            values
                .iter()
                .map(|v| v.to_string())
                .collect::<BTreeSet<_>>(),
        )])
    };
    let p2 = of(&["sec_00001/p2"]);
    assert!(!super::super::apart::necessary_compatible(
        &c,
        &p2,
        &of(&["sec_00001/p1"])
    ));
    assert!(super::super::apart::necessary_compatible(
        &c,
        &p2,
        &of(&["sec_00001/p2"])
    ));
    assert!(super::super::apart::necessary_compatible(
        &c,
        &p2,
        &of(&["sec_00002/p1"])
    ));
    assert!(!super::super::apart::necessary_compatible(
        &c,
        &p2,
        &of(&["sec_00002/p1", "sec_00001/p7"])
    ));
    assert!(super::super::apart::necessary_compatible(
        &c,
        &p2,
        &BTreeMap::new()
    ));
}

/// Two particulars the window's reader read in one document are two: once a
/// question joins one of them to a record, that record is not offered the
/// other, decided later in the same document or in a later one. Failing
/// input, named: a rollover message ("the old piece is being replaced by the
/// new one") whose ending and replacing particulars each picked the old deal's
/// record when asked alone, so one record held both (two worlds' rollovers,
/// arm particulars-all2, 2026-10-15).
#[tokio::test]
async fn particulars_read_in_one_document_never_share_a_record() {
    let (infer, seen) = scripted(vec![
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.9, "0": 0.1}),
    ]);
    let mut res = weights(&[("proposed_answer", 0.5, 0.4)]);
    let c = criterion(&[CHAPTER_SUBJECT_KEY]);
    let a = "The spring storage deal is proposed.";
    let key = |chapter| [(CHAPTER_SUBJECT_KEY, chapter)];
    res.resolve_document(
        &c,
        doc("a", a),
        &[stmt("a", a, "spring storage deal", 0, &key("sec_1/p1"))],
        &[],
        Answerer::Select(&infer),
    )
    .await;
    let b = "The spring storage deal is replaced by the summer storage deal.";
    let r = res
        .resolve_document(
            &c,
            doc("b", b),
            &[
                stmt("old", b, "spring storage deal", 0, &key("sec_2/p1")),
                stmt("new", b, "summer storage deal", 0, &key("sec_2/p2")),
            ],
            &[similar("a", 0.5)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(res.records()[0].statements, ["a", "old"]);
    assert_eq!(r.outcomes[1].outcome.record(), Some("new"), "{r:?}");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the new particular is offered nothing"
    );
}

/// The same when the record took the other particular in an earlier document:
/// it is the reader's distinction in THIS document that forbids it.
#[tokio::test]
async fn a_record_holding_a_particular_read_beside_the_statement_is_not_offered() {
    let (infer, seen) = scripted(vec![
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.9, "0": 0.1}),
    ]);
    let mut res = weights(&[("proposed_answer", 0.5, 0.4)]);
    let c = criterion(&[CHAPTER_SUBJECT_KEY]);
    let key = |chapter| [(CHAPTER_SUBJECT_KEY, chapter)];
    for (id, body, chapter) in [
        ("a", "The spring storage deal is proposed.", "sec_1/p1"),
        ("b", "The spring storage deal slipped.", "sec_2/p1"),
    ] {
        let proposals = if id == "a" {
            vec![]
        } else {
            vec![similar("a", 0.5)]
        };
        res.resolve_document(
            &c,
            doc(id, body),
            &[stmt(id, body, "spring storage deal", 0, &key(chapter))],
            &proposals,
            Answerer::Select(&infer),
        )
        .await;
    }
    let d = "The spring storage deal is replaced by the summer storage deal.";
    res.resolve_document(
        &c,
        doc("d", d),
        &[
            stmt("old", d, "spring storage deal", 0, &key("sec_2/p1")),
            stmt("new", d, "summer storage deal", 0, &key("sec_2/p2")),
        ],
        &[similar("a", 0.5)],
        Answerer::Select(&infer),
    )
    .await;
    assert_eq!(res.records()[0].statements, ["a", "b", "old"]);
    assert_eq!(seen.lock().unwrap().len(), 1, "only b was asked");
}

#[test]
fn a_reference_read_beside_is_one_of_the_documents_own() {
    use super::super::apart::read_beside;
    let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    let doc = set(&["sec_2/p1", "sec_2/p2"]);
    let beside =
        |mine: &[&str], held: &[&str]| read_beside(Some(&set(mine)), &set(held), &doc).cloned();
    assert_eq!(
        beside(&["sec_2/p2"], &["sec_1/p7", "sec_2/p1"]),
        Some("sec_2/p1".into())
    );
    assert_eq!(
        beside(&["sec_2/p2"], &["sec_1/p7", "sec_2/p5"]),
        None,
        "p5 is of another document"
    );
    assert_eq!(
        beside(&["sec_2/p2"], &["sec_3/p1"]),
        None,
        "another window's p1"
    );
    assert_eq!(
        beside(&["sec_2/p2"], &["sec_2/p1", "sec_2/p2"]),
        None,
        "it holds the statement's own"
    );
    assert_eq!(read_beside(None, &set(&["sec_2/p1"]), &doc), None);
}

/// A record this document opened is weighed by every particular that has
/// joined it, as a record opened before it is: a particular of the
/// statement's window that joined it keeps the statement out, and the
/// statement opens its own. Failing input, named: offered by its opener's
/// reference alone, the window-mate of a particular that had joined the
/// record was asked, chose it, and was refused at the fold's recheck
/// (`gate_plans`, which sees every joiner), so it reached no record (one
/// long document read in windows, as a novel is; audit 2026-10-16).
#[tokio::test]
async fn a_record_opened_in_the_pass_holds_the_particulars_that_joined_it() {
    let (infer, seen) = scripted(vec![
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.9, "0": 0.1}),
    ]);
    let mut res = weights(&[("proposed_answer", 0.5, 0.4)]);
    let c = criterion(&[CHAPTER_SUBJECT_KEY]);
    let key = |chapter| [(CHAPTER_SUBJECT_KEY, chapter)];
    let body = "The brass lamp went missing. The tin case of the brass lamp lay in a crate. \
                The brass lamp turned up under the sand.";
    let r = res
        .resolve_document(
            &c,
            doc("story", body),
            &[
                stmt(
                    "opened",
                    body,
                    "The brass lamp went missing",
                    0,
                    &key("sec_1/p1"),
                ),
                stmt(
                    "case",
                    body,
                    "The tin case of the brass lamp lay in a crate",
                    0,
                    &key("sec_2/p1"),
                ),
                stmt(
                    "found",
                    body,
                    "The brass lamp turned up under the sand",
                    0,
                    &key("sec_2/p2"),
                ),
            ],
            &[],
            Answerer::Select(&infer),
        )
        .await;
    let held: Vec<Vec<String>> = res.records().iter().map(|r| r.statements.clone()).collect();
    assert_eq!(
        held,
        [vec!["opened", "case"], vec!["found"]],
        "another window's particular may join; the joiner's window-mate may not: {r:?}"
    );
    assert!(
        matches!(
            r.outcomes[2].outcome,
            Outcome::Decided(Decision::Opened { .. })
        ),
        "the window-mate opens its own record, never refused: {r:?}"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the window-mate is offered nothing"
    );
}

/// A statement follows its own particular. Once a question joined a window's
/// particular to a record that also holds another particular of that window
/// (offered, marked, and chosen: they were read in different documents), the
/// particular's later statements join that record by their window key; the
/// window rule does not veto it. Failing input, named: a deal's updates in
/// later documents, refused as contradictions because the record already
/// held a sibling, so the deal's won and lost reached no record (arm
/// particulars-all2, 2026-10-15).
#[tokio::test]
async fn a_statement_follows_its_own_particular() {
    let (infer, _) = scripted(vec![
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.9, "0": 0.1}),
    ]);
    let mut res = weights(&[("proposed_answer", 0.5, 0.4)]);
    let c = criterion(&[CHAPTER_SUBJECT_KEY]);
    let mut last = None;
    for (id, body, chapter) in [
        ("a", "The storage deal is proposed.", "sec_1/p1"),
        ("b", "The storage deal is off.", "sec_2/p1"),
        ("c", "The storage deal is back on.", "sec_2/p2"),
        ("d", "The storage deal is booked.", "sec_2/p2"),
    ] {
        let proposals = if id == "a" {
            vec![]
        } else {
            vec![similar("a", 0.5)]
        };
        last = Some(
            res.resolve_document(
                &c,
                doc(id, body),
                &[stmt(
                    id,
                    body,
                    "storage deal",
                    0,
                    &[(CHAPTER_SUBJECT_KEY, chapter)],
                )],
                &proposals,
                Answerer::Select(&infer),
            )
            .await,
        );
    }
    let d = last.expect("d was resolved");
    assert_eq!(d.outcomes[0].outcome.record(), Some("a"), "{d:?}");
    assert_eq!(res.records()[0].statements, ["a", "b", "c", "d"]);
}

#[test]
fn a_necessary_conflict_is_named() {
    use super::super::apart::{necessary_compatible, necessary_conflict};
    let c = criterion(&[]);
    let of = |values: &[&str]| {
        BTreeMap::from([(
            CHAPTER_SUBJECT_KEY.to_string(),
            values
                .iter()
                .map(|v| v.to_string())
                .collect::<BTreeSet<_>>(),
        )])
    };
    let p2 = of(&["sec_2/p2"]);
    assert!(necessary_compatible(
        &c,
        &p2,
        &of(&["sec_1/p1", "sec_2/p1", "sec_2/p2"])
    ));
    let why = necessary_conflict(&c, &p2, &of(&["sec_1/p1", "sec_2/p1"])).expect("a conflict");
    assert!(why.contains("sec_2/p1"), "{why}");
    assert_eq!(necessary_conflict(&c, &p2, &of(&["sec_1/p1"])), None);
}

/// Each question about a statement shows the other particulars its window's
/// reader read beside it in the same document, as read, so the first of two
/// siblings asked can leave a record that fits the other. Failing input,
/// named: a message passing on one deal and going ahead with its sibling read
/// two particulars in one sentence; the first asked chose the record of the
/// second's deal, which then could not join it (world-s4 sagewood-d2, arms
/// c12-verify and c123-verify, 2026-10-15). The model here takes the offered
/// record unless the question shows a sibling the record's name is.
#[tokio::test]
async fn a_question_shows_the_particular_read_beside_its_statement() {
    let seen = Arc::new(Mutex::new(Vec::<ChatPrompt>::new()));
    let kept = seen.clone();
    let infer: InferenceFn = Arc::new(move |p: &ChatPrompt, _| {
        kept.lock().unwrap().push(p.clone());
        let leaves = p.user.contains("about as: named: Acme Q1")
            && p.user
                .contains("in this document, as another particular: named: Acme Apr-Oct");
        let answer = if leaves {
            json!({"A": 0.1, "0": 0.9})
        } else {
            json!({"A": 0.9, "0": 0.1})
        };
        Box::pin(async move { Ok(answer.to_string()) })
    });
    let named = |s: Statement, name: &str| Statement {
        described: Described {
            names: [name.to_string()].into(),
            ..Default::default()
        },
        ..s
    };
    let mut res = weights(&[("proposed_answer", 0.5, 0.4)]);
    let c = criterion(&[CHAPTER_SUBJECT_KEY]);
    let key = |chapter| [(CHAPTER_SUBJECT_KEY, chapter)];
    let a = "Acme takes the Apr-Oct piece.";
    res.resolve_document(
        &c,
        doc("a", a),
        &[named(
            stmt("a", a, "the Apr-Oct piece", 0, &key("sec_1/p1")),
            "Acme Apr-Oct",
        )],
        &[],
        Answerer::Select(&infer),
    )
    .await;
    let b = "We pass on Q1 this time and go ahead with Apr-Oct.";
    let r = res
        .resolve_document(
            &c,
            doc("b", b),
            &[
                named(stmt("q1", b, "pass on Q1", 0, &key("sec_2/p1")), "Acme Q1"),
                named(
                    stmt("aprs", b, "go ahead with Apr-Oct", 0, &key("sec_2/p2")),
                    "Acme Apr-Oct",
                ),
            ],
            &[similar("a", 0.5)],
            Answerer::Select(&infer),
        )
        .await;
    assert_eq!(res.records()[0].statements, ["a", "aprs"], "{r:?}");
    assert_eq!(r.outcomes[0].outcome.record(), Some("q1"), "{r:?}");
    let asked = seen.lock().unwrap();
    assert!(
        asked
            .iter()
            .any(|p| p.user.contains("about as: named: Acme Apr-Oct")
                && p.user.contains("as another particular: named: Acme Q1")),
        "the second sibling's question shows the first"
    );
}
