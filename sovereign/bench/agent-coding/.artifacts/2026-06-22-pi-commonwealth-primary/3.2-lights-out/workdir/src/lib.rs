// SPDX-License-Identifier: AGPL-3.0-or-later
//! Light's Out solver.
//!
//! Uses Gaussian elimination over GF(2) to find a minimum-cardinality
//! set of presses that turns all lights off.
pub fn solve(grid: &[Vec<u8>]) -> Option<Vec<(usize, usize)>> {
    let n = grid.len();
    if n == 0 {
        return Some(vec![]);
    }

    // Total cells
    let N = n * n;

    // We need to solve A*x = b (mod 2), where:
    // - A is the NxN toggle matrix. A[i][j] = 1 iff pressing cell j toggles cell i.
    // - b is the initial state vector (the grid flattened).
    //
    // For N up to 400 (20x20), we use u64 bitsets with multiple words per row.
    const WORDS: usize = if N <= 63 { 1 } else { 2 }; // +1 for augmented column

    type Word = u64;
    let mut mat: Vec<[Word; 2]> = vec![[0, 0]; N];

    fn set_bit(row: &mut [Word], col: usize) {
        let word_idx = col / 64;
        let bit_idx = col % 64;
        row[word_idx] |= 1u64 << bit_idx;
    }

    fn clear_bit(row: &mut [Word], col: usize) {
        let widx = col / 64usize;
        let bidx = (col % 64) as u32;
        if widx < row.len() {
            row[widx] &= !(1u64 << bidx);
        }
    }

    #[inline]
    fn has_bit(row: &[Word], col: usize) -> bool {
        let word_idx = (col / 64).min(WORDS - 1);
        let bit_idx = (col % 64)
</parameter>
}<tool_call>{