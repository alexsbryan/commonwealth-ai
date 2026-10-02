// SPDX-License-Identifier: AGPL-3.0-or-later
//! Smoke tests visible to the agent during development.
//!
//! These three cases are a SUBSET of the held-out grading suite. The
//! grader copies its full 12-fixture suite over this file (same
//! path, same name) AFTER the agent exits — so this file is your
//! iteration substrate, not the final scoring oracle. Passing all
//! three smoke tests is necessary but not sufficient.

use two_sum::two_sum;

#[test]
fn smoke_pair_found_sorted_indices() {
    assert_eq!(two_sum(&[1, 2, 4, 7, 11], 9), Some((1, 3)));
}

#[test]
fn smoke_no_pair_returns_none() {
    assert_eq!(two_sum(&[1, 2, 3, 4], 100), None);
    assert_eq!(two_sum(&[], 0), None);
}

#[test]
fn smoke_duplicate_values_distinct_indices() {
    assert_eq!(two_sum(&[3, 3], 6), Some((0, 1)));
}
