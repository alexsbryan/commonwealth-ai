"""Held-out integration tests for 3.2 Light's Out (Python).

Copied into the agent's workdir by the witness pipeline AFTER the
agent exits. Anything the agent wrote under `tests/` is overwritten;
the held-out cases below are canonical.

Validation strategy: for each test grid we
  1. ask the solver for a press sequence,
  2. assert the sequence isn't None for solvable cases (and IS None
     for the unsolvable case),
  3. apply the presses to a fresh copy of the grid and assert every
     cell ends at 0,
  4. for cases where we know the minimum count exactly, assert the
     candidate's count is no larger than that minimum.

Press order is never relied upon — solver may produce presses in any
order; applying them all on a fresh copy must yield the all-zeros
grid.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lights_out import solve  # noqa: E402


def apply_presses(grid, presses):
    n = len(grid)
    assert all(len(row) == n for row in grid), "grid must be square"
    g = [row[:] for row in grid]
    for r, c in presses:
        assert 0 <= r < n and 0 <= c < n, f"press out of bounds: ({r}, {c})"
        for dr, dc in ((0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)):
            nr, nc = r + dr, c + dc
            if 0 <= nr < n and 0 <= nc < n:
                g[nr][nc] ^= 1
    return g


def is_all_off(grid):
    return all(v == 0 for row in grid for v in row)


def check_solves(grid):
    presses = solve(grid)
    assert presses is not None, f"expected a solution, got None; grid={grid!r}"
    final = apply_presses(grid, presses)
    assert is_all_off(final), (
        f"applying solver presses did not turn all lights off; "
        f"original={grid!r} presses={presses!r} final={final!r}"
    )
    return len(presses)


# --------------------------------------------------------------------
# Solvable cases
# --------------------------------------------------------------------


def test_single_lit_1x1_one_press_solves():
    grid = [[1]]
    n = check_solves(grid)
    assert n == 1


def test_all_off_2x2_no_presses():
    grid = [[0, 0], [0, 0]]
    presses = solve(grid)
    assert presses is not None and len(presses) == 0


def test_single_lit_2x2_solvable():
    grid = [[1, 0], [0, 0]]
    check_solves(grid)


def test_all_lit_3x3_solvable():
    grid = [[1, 1, 1], [1, 1, 1], [1, 1, 1]]
    check_solves(grid)


def test_checkerboard_3x3_solvable():
    grid = [
        [1, 0, 1],
        [0, 1, 0],
        [1, 0, 1],
    ]
    check_solves(grid)


def test_all_lit_5x5_solvable():
    grid = [[1] * 5 for _ in range(5)]
    check_solves(grid)


def test_one_lit_5x5_corner_solvable():
    grid = [[0] * 5 for _ in range(5)]
    grid[0][0] = 1
    check_solves(grid)


def test_diagonal_lit_4x4_solvable():
    n = 4
    grid = [[0] * n for _ in range(n)]
    for i in range(n):
        grid[i][i] = 1
    check_solves(grid)


def test_dense_5x5_solvable():
    grid = [
        [1, 0, 1, 0, 1],
        [0, 1, 1, 1, 0],
        [1, 1, 0, 1, 1],
        [0, 1, 1, 1, 0],
        [1, 0, 1, 0, 1],
    ]
    check_solves(grid)


# --------------------------------------------------------------------
# Unsolvable case — see Anderson & Feil, "Turning lights out with
# linear algebra": single corner lit on 4x4 is NOT in the image of
# the press matrix.
# --------------------------------------------------------------------


def test_known_unsolvable_4x4_corner_returns_none():
    grid = [[0] * 4 for _ in range(4)]
    grid[0][0] = 1
    presses = solve(grid)
    assert presses is None, (
        f"expected single-corner-lit 4x4 to be unsolvable; got {presses!r}"
    )


# --------------------------------------------------------------------
# Scale check — n=10 within witness budget.
# --------------------------------------------------------------------


def test_all_lit_10x10_solvable():
    grid = [[1] * 10 for _ in range(10)]
    check_solves(grid)


def test_one_lit_center_10x10_solvable():
    grid = [[0] * 10 for _ in range(10)]
    grid[5][5] = 1
    check_solves(grid)
