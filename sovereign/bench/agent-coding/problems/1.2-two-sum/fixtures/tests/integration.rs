// SPDX-License-Identifier: AGPL-3.0-or-later
//! Held-out integration tests for problem 1.2 — `two_sum` on a
//! sorted array.

use two_sum::two_sum;

#[test]
fn empty_array_returns_none() {
    assert_eq!(two_sum(&[], 0), None);
}

#[test]
fn single_element_returns_none() {
    assert_eq!(two_sum(&[5], 10), None);
}

#[test]
fn no_valid_pair_returns_none() {
    assert_eq!(two_sum(&[1, 2, 3, 4], 100), None);
    assert_eq!(two_sum(&[1, 2, 3, 4], -1), None);
}

#[test]
fn basic_pair_at_middle() {
    let arr = [1, 2, 4, 7, 11];
    // 2 + 7 = 9 → indices (1, 3)
    assert_eq!(two_sum(&arr, 9), Some((1, 3)));
}

#[test]
fn duplicate_values_legal_pair() {
    // Two equal elements forming the pair — must use distinct indices.
    assert_eq!(two_sum(&[3, 3], 6), Some((0, 1)));
}

#[test]
fn duplicate_value_cannot_use_same_index_twice() {
    // arr[0] + arr[0] would equal 6, but that uses index 0 twice.
    // No other pair sums to 6, so the answer is None.
    assert_eq!(two_sum(&[3, 4, 5], 6), None);
}

#[test]
fn pair_at_boundaries() {
    let arr = [1, 2, 3, 4, 9];
    assert_eq!(two_sum(&arr, 10), Some((0, 4)));
}

#[test]
fn negative_values_supported() {
    let arr = [-5, -2, 0, 1, 4];
    // Multiple pairs sum to -1: (-5, 4) at (0, 4) and (-2, 1) at (1, 3).
    // Accept any valid pair satisfying the contract.
    let r1 = two_sum(&arr, -1).expect("must find a -1 sum pair");
    assert!(r1.0 < r1.1);
    assert_eq!(arr[r1.0] + arr[r1.1], -1);
    // Only one pair sums to -7: (-5, -2) at (0, 1).
    assert_eq!(two_sum(&arr, -7), Some((0, 1)));
}

#[test]
fn target_zero_with_opposite_signs() {
    let arr = [-4, -1, 0, 1, 4];
    // Either (-4, 4) at (0, 4) or (-1, 1) at (1, 3). Accept either —
    // both are correct minimum-index leftmost pairs in different
    // interpretations. The function's spec is "any valid pair" so we
    // accept any answer that satisfies the contract.
    let result = two_sum(&arr, 0).expect("must find a zero-sum pair");
    let (i, j) = result;
    assert!(i < j);
    assert_eq!(arr[i] + arr[j], 0);
}

#[test]
fn long_array_finds_pair() {
    // Sorted 0..=99 with target = 197 (= 98 + 99).
    let arr: Vec<i64> = (0..=99).collect();
    assert_eq!(two_sum(&arr, 197), Some((98, 99)));
}

#[test]
fn long_array_no_pair() {
    // Sorted 0..=99 with target = 199 (max is 99 + 98 = 197).
    let arr: Vec<i64> = (0..=99).collect();
    assert_eq!(two_sum(&arr, 199), None);
}

#[test]
fn target_at_start() {
    let arr = [1, 2, 3, 10, 20];
    // 1 + 2 = 3
    assert_eq!(two_sum(&arr, 3), Some((0, 1)));
}
