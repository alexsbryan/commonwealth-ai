// SPDX-License-Identifier: AGPL-3.0-or-later
//! Held-out integration tests for 3.2 Light's Out.
//!
//! Copied into the agent's workdir by the witness pipeline AFTER the
//! agent exits. Anything the agent wrote under `tests/` is
//! overwritten — the held-out cases are canonical.
//!
//! Validation strategy: for each test grid we
//!   1. ask the solver to produce a press sequence,
//!   2. assert the sequence isn't `None` for solvable cases (and IS
//!      `None` for the unsolvable case),
//!   3. apply the presses to a copy of the grid and assert every cell
//!      ends at `0`,
//!   4. for cases where we know the minimum count exactly, assert
//!      the candidate's solution is no longer than that minimum.
//!
//! We never rely on press order: the solver may produce presses in
//! any order; applying them all on a fresh copy of the grid must
//! produce the all-zeros grid.

use lights_out::solve;

fn apply_presses(grid: &mut Vec<Vec<u8>>, presses: &[(usize, usize)]) {
    let n = grid.len();
    for &(r, c) in presses {
        assert!(r < n && c < n, "press out of bounds: ({r}, {c})");
        toggle(grid, r, c);
        if r + 1 < n {
            toggle(grid, r + 1, c);
        }
        if r > 0 {
            toggle(grid, r - 1, c);
        }
        if c + 1 < n {
            toggle(grid, r, c + 1);
        }
        if c > 0 {
            toggle(grid, r, c - 1);
        }
    }
}

fn toggle(grid: &mut Vec<Vec<u8>>, r: usize, c: usize) {
    grid[r][c] ^= 1;
}

fn is_all_off(grid: &Vec<Vec<u8>>) -> bool {
    grid.iter().all(|row| row.iter().all(|&x| x == 0))
}

fn check_solves(grid: Vec<Vec<u8>>) -> usize {
    let original = grid.clone();
    let presses = solve(&grid).expect("expected a solution");
    let mut g = original.clone();
    apply_presses(&mut g, &presses);
    assert!(
        is_all_off(&g),
        "applying solver presses did not turn all lights off; original={original:?} presses={presses:?} final={g:?}"
    );
    presses.len()
}

// --------------------------------------------------------------------
// Solvable cases
// --------------------------------------------------------------------

#[test]
fn single_lit_1x1_one_press_solves() {
    let grid = vec![vec![1u8]];
    let n = check_solves(grid);
    assert_eq!(n, 1);
}

#[test]
fn all_off_2x2_no_presses() {
    let grid = vec![vec![0u8; 2]; 2];
    let presses = solve(&grid).expect("zero presses is a valid solution");
    assert!(presses.is_empty());
}

#[test]
fn single_lit_2x2_solvable() {
    // Press the lit corner; toggles 3 lights (itself + 2 neighbors).
    // The remaining state has 2 lit lights at the neighbors; the
    // minimum-press solution will involve more presses than the
    // naive "press the lit cell" — solver must find one.
    let grid = vec![vec![1u8, 0], vec![0, 0]];
    check_solves(grid);
}

#[test]
fn all_lit_3x3_solvable() {
    let grid = vec![vec![1u8; 3]; 3];
    check_solves(grid);
}

#[test]
fn checkerboard_3x3_solvable() {
    let grid = vec![
        vec![1u8, 0, 1],
        vec![0, 1, 0],
        vec![1, 0, 1],
    ];
    check_solves(grid);
}

#[test]
fn all_lit_5x5_solvable() {
    let grid = vec![vec![1u8; 5]; 5];
    check_solves(grid);
}

#[test]
fn one_lit_5x5_corner_solvable() {
    let mut grid = vec![vec![0u8; 5]; 5];
    grid[0][0] = 1;
    check_solves(grid);
}

#[test]
fn diagonal_lit_4x4_solvable() {
    let n = 4;
    let mut grid = vec![vec![0u8; n]; n];
    for i in 0..n {
        grid[i][i] = 1;
    }
    check_solves(grid);
}

#[test]
fn dense_5x5_solvable() {
    let grid = vec![
        vec![1, 0, 1, 0, 1],
        vec![0, 1, 1, 1, 0],
        vec![1, 1, 0, 1, 1],
        vec![0, 1, 1, 1, 0],
        vec![1, 0, 1, 0, 1],
    ];
    check_solves(grid);
}

// --------------------------------------------------------------------
// Unsolvable case — on the standard 5x5 board, exactly two grids in
// the press image's null-space complement are unreachable. We use
// a small known-unsolvable 4x4 pattern.
//
// A grid is unsolvable iff its lit-cell vector is not in the image
// of the press matrix; the most accessible unsolvable example on a
// small board is a single lit corner on a 4x4 grid.
// --------------------------------------------------------------------

#[test]
fn known_unsolvable_4x4_corner_returns_none() {
    let mut grid = vec![vec![0u8; 4]; 4];
    grid[0][0] = 1;
    // For 4x4 the kernel of the press matrix is non-trivial; a
    // single corner lit IS NOT in the image. (See Anderson &
    // Feil's "Turning lights out with linear algebra".)
    let presses = solve(&grid);
    assert!(
        presses.is_none(),
        "expected single-corner-lit 4x4 to be unsolvable; got {presses:?}"
    );
}

// --------------------------------------------------------------------
// Scale check — solver must handle n=10 within the witness budget.
// (n=20 is the spec target but we keep the held-out grids small here
// to keep the witness fast and let the judge dimension carry the
// efficiency-at-n=20 signal.)
// --------------------------------------------------------------------

#[test]
fn all_lit_10x10_solvable() {
    let grid = vec![vec![1u8; 10]; 10];
    check_solves(grid);
}

#[test]
fn one_lit_center_10x10_solvable() {
    let mut grid = vec![vec![0u8; 10]; 10];
    grid[5][5] = 1;
    check_solves(grid);
}
