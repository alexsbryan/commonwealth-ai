"""Smoke tests visible to the agent during development.

These three cases are a SUBSET of the held-out grading suite. The
grader copies its full 12-fixture suite over this file (same path,
same name) AFTER the agent exits — so this file is your iteration
substrate, not the final scoring oracle. Passing all three smoke
tests is necessary but not sufficient.
"""

import sys
from pathlib import Path

# Make sibling `lights_out.py` importable.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lights_out import solve  # noqa: E402


def apply_presses(grid, presses):
    n = len(grid)
    g = [row[:] for row in grid]
    for r, c in presses:
        for dr, dc in ((0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)):
            nr, nc = r + dr, c + dc
            if 0 <= nr < n and 0 <= nc < n:
                g[nr][nc] ^= 1
    return g


def is_all_off(grid):
    return all(v == 0 for row in grid for v in row)


def test_smoke_all_off_2x2_returns_solution():
    grid = [[0, 0], [0, 0]]
    presses = solve(grid)
    assert presses is not None
    final = apply_presses(grid, presses)
    assert is_all_off(final)


def test_smoke_single_lit_1x1_one_press_solves():
    grid = [[1]]
    presses = solve(grid)
    assert presses is not None
    final = apply_presses(grid, presses)
    assert is_all_off(final)


def test_smoke_all_lit_3x3_solvable():
    grid = [[1, 1, 1], [1, 1, 1], [1, 1, 1]]
    presses = solve(grid)
    assert presses is not None
    final = apply_presses(grid, presses)
    assert is_all_off(final)
