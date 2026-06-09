//! Smoke tests visible to the agent during development.
//!
//! These three cases are a SUBSET of the held-out grading suite. The
//! grader copies its full 12-fixture suite over this file (same
//! path, same name) AFTER the agent exits — so this file is your
//! iteration substrate, not the final scoring oracle. Passing all
//! three smoke tests is necessary but not sufficient.

use lights_out::solve;

fn apply_presses(grid: &mut Vec<Vec<u8>>, presses: &[(usize, usize)]) {
    let n = grid.len();
    for &(r, c) in presses {
        toggle(grid, r, c);
        if r + 1 < n { toggle(grid, r + 1, c); }
        if r > 0 { toggle(grid, r - 1, c); }
        if c + 1 < n { toggle(grid, r, c + 1); }
        if c > 0 { toggle(grid, r, c - 1); }
    }
}

fn toggle(grid: &mut Vec<Vec<u8>>, r: usize, c: usize) {
    grid[r][c] ^= 1;
}

fn is_all_off(grid: &Vec<Vec<u8>>) -> bool {
    grid.iter().flatten().all(|&v| v == 0)
}

#[test]
fn smoke_all_off_2x2_returns_empty_or_zero_presses() {
    let grid = vec![vec![0, 0], vec![0, 0]];
    let presses = solve(&grid).expect("all-off is trivially solvable");
    let mut copy = grid.clone();
    apply_presses(&mut copy, &presses);
    assert!(is_all_off(&copy));
}

#[test]
fn smoke_single_lit_1x1_one_press_solves() {
    let grid = vec![vec![1]];
    let presses = solve(&grid).expect("single lit cell solvable");
    let mut copy = grid.clone();
    apply_presses(&mut copy, &presses);
    assert!(is_all_off(&copy));
}

#[test]
fn smoke_all_lit_3x3_solvable() {
    let grid = vec![
        vec![1, 1, 1],
        vec![1, 1, 1],
        vec![1, 1, 1],
    ];
    let presses = solve(&grid).expect("all-lit 3x3 is solvable");
    let mut copy = grid.clone();
    apply_presses(&mut copy, &presses);
    assert!(is_all_off(&copy));
}
