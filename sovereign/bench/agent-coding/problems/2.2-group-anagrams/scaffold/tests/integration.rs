//! Smoke tests visible to the agent during development.
//!
//! These three cases are a SUBSET of the held-out grading suite. The
//! grader copies its full 12-fixture suite over this file (same
//! path, same name) AFTER the agent exits — so this file is your
//! iteration substrate, not the final scoring oracle. Passing all
//! three smoke tests is necessary but not sufficient.

use group_anagrams::group_anagrams;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn smoke_empty_input() {
    let out = group_anagrams(vec![]);
    assert!(out.is_empty());
}

#[test]
fn smoke_single_string() {
    let out = group_anagrams(s(&["a"]));
    assert_eq!(out, vec![s(&["a"])]);
}

#[test]
fn smoke_classic_three_groups_preserve_order() {
    let out = group_anagrams(s(&["eat", "tea", "tan", "ate", "nat", "bat"]));
    let expected = vec![
        s(&["eat", "tea", "ate"]),
        s(&["tan", "nat"]),
        s(&["bat"]),
    ];
    assert_eq!(out, expected);
}
