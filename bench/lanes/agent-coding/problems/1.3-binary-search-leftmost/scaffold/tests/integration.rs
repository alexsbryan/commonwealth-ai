// SPDX-License-Identifier: AGPL-3.0-or-later
//! Smoke tests visible to the agent during development.
//!
//! These three cases are a SUBSET of the held-out grading suite. The
//! grader copies its full 12-fixture suite over this file (same
//! path, same name) AFTER the agent exits — so this file is your
//! iteration substrate, not the final scoring oracle. Passing all
//! three smoke tests is necessary but not sufficient.

use lower_bound::lower_bound;

#[test]
fn smoke_exact_and_insertion_point() {
    assert_eq!(lower_bound(&[1, 3, 5, 7], 5), 2);
    assert_eq!(lower_bound(&[1, 3, 5, 7], 4), 2);
}

#[test]
fn smoke_boundaries() {
    assert_eq!(lower_bound(&[1, 3, 5, 7], 0), 0);
    assert_eq!(lower_bound(&[1, 3, 5, 7], 100), 4);
    assert_eq!(lower_bound(&[], 42), 0);
}

#[test]
fn smoke_leftmost_among_equals() {
    assert_eq!(lower_bound(&[5, 5, 5, 5], 5), 0);
}
