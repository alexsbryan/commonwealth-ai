// SPDX-License-Identifier: AGPL-3.0-or-later
//! Smoke tests visible to the agent during development.
//!
//! These three cases are a SUBSET of the held-out grading suite. The
//! grader copies its full 12-fixture suite over this file (same
//! path, same name) AFTER the agent exits — so this file is your
//! iteration substrate, not the final scoring oracle. Passing all
//! three smoke tests is necessary but not sufficient.

use balanced_parens::is_balanced;

#[test]
fn smoke_empty_is_balanced() {
    assert!(is_balanced(""));
}

#[test]
fn smoke_simple_pair() {
    assert!(is_balanced("()"));
    assert!(!is_balanced("(]"));
}

#[test]
fn smoke_nested_mixed() {
    assert!(is_balanced("([{}])"));
    assert!(!is_balanced("([)]"));
}
