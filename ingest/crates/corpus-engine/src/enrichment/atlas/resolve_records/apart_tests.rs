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
