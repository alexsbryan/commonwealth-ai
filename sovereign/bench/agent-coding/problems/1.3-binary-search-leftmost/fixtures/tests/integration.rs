// SPDX-License-Identifier: AGPL-3.0-or-later
//! Held-out integration tests for problem 1.3 — `lower_bound`.

use lower_bound::lower_bound;

#[test]
fn empty_array_returns_zero() {
    assert_eq!(lower_bound(&[], 42), 0);
    assert_eq!(lower_bound(&[], -1), 0);
    assert_eq!(lower_bound(&[], 0), 0);
}

#[test]
fn target_smaller_than_all() {
    assert_eq!(lower_bound(&[1, 3, 5, 7], 0), 0);
    assert_eq!(lower_bound(&[10, 20, 30], -5), 0);
}

#[test]
fn target_larger_than_all() {
    assert_eq!(lower_bound(&[1, 3, 5, 7], 100), 4);
    assert_eq!(lower_bound(&[1, 3, 5, 7], 8), 4);
}

#[test]
fn target_exact_match_unique() {
    assert_eq!(lower_bound(&[1, 3, 5, 7], 5), 2);
    assert_eq!(lower_bound(&[1, 3, 5, 7], 1), 0);
    assert_eq!(lower_bound(&[1, 3, 5, 7], 7), 3);
}

#[test]
fn target_insertion_point_no_match() {
    assert_eq!(lower_bound(&[1, 3, 5, 7], 4), 2); // between 3 and 5
    assert_eq!(lower_bound(&[1, 3, 5, 7], 6), 3); // between 5 and 7
    assert_eq!(lower_bound(&[1, 3, 5, 7], 2), 1); // between 1 and 3
}

#[test]
fn target_all_equal_returns_leftmost() {
    // Every element equals target — must return 0 (leftmost).
    assert_eq!(lower_bound(&[5, 5, 5, 5], 5), 0);
    assert_eq!(lower_bound(&[0, 0, 0], 0), 0);
}

#[test]
fn target_with_duplicates_returns_leftmost() {
    // Multiple equal elements — return the FIRST one.
    assert_eq!(lower_bound(&[1, 2, 5, 5, 5, 7], 5), 2);
    assert_eq!(lower_bound(&[1, 1, 1, 4, 5], 1), 0);
    assert_eq!(lower_bound(&[1, 4, 4, 4, 5], 4), 1);
}

#[test]
fn single_element_array() {
    assert_eq!(lower_bound(&[5], 5), 0);
    assert_eq!(lower_bound(&[5], 3), 0);
    assert_eq!(lower_bound(&[5], 10), 1);
}

#[test]
fn negative_values_supported() {
    let arr = [-10, -5, 0, 5, 10];
    assert_eq!(lower_bound(&arr, -10), 0);
    assert_eq!(lower_bound(&arr, -7), 1);
    assert_eq!(lower_bound(&arr, 0), 2);
    assert_eq!(lower_bound(&arr, -100), 0);
    assert_eq!(lower_bound(&arr, 100), 5);
}

#[test]
fn long_array_correct_insertion() {
    // 0, 2, 4, ..., 198 (100 elements).
    let arr: Vec<i64> = (0..100).map(|i| (i as i64) * 2).collect();
    assert_eq!(lower_bound(&arr, 50), 25); // arr[25] == 50
    assert_eq!(lower_bound(&arr, 51), 26); // insert at 26
    assert_eq!(lower_bound(&arr, 0), 0);
    assert_eq!(lower_bound(&arr, 198), 99);
    assert_eq!(lower_bound(&arr, 200), 100);
}

#[test]
fn duplicates_at_start_and_target_matches() {
    // arr[0..3] all equal target; lower bound is 0.
    let arr = [4, 4, 4, 5, 6, 7];
    assert_eq!(lower_bound(&arr, 4), 0);
}

#[test]
fn duplicates_at_end_and_target_matches() {
    let arr = [1, 2, 3, 9, 9, 9];
    assert_eq!(lower_bound(&arr, 9), 3);
}
