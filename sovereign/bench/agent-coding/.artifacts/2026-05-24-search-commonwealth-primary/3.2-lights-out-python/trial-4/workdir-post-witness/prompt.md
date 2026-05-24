# Light's Out — minimum-press solver (Python, Scaffolded tier)

You're solving the classic **Light's Out** puzzle on an `n × n` grid.

Rules:
- Each cell is either lit (`1`) or dark (`0`).
- Pressing a cell at row `r`, column `c` **toggles** that cell **and** its 4 orthogonal neighbors (up, down, left, right), if they exist.
- Each cell can be pressed any number of times (but pressing twice has no net effect).
- Goal: starting from an arbitrary initial grid, find a sequence of presses that turns **every light off**.

The grid is bounded — there is no wrap-around. Corners have 3 neighbors, edges have 4 (including themselves), and interior cells have 5.

## Your task

Implement, in `lights_out.py` at the workdir root:

```python
def solve(grid: list[list[int]]) -> list[tuple[int, int]] | None
```

- `grid` is square; `grid[r][c]` is `0` (dark) or `1` (lit).
- Returns a list of `(row, column)` tuples that, when applied in any order, turn every cell off. The list must be **minimum-cardinality** (any minimum-count solution is fine — ties broken however you like).
- Returns `None` if the initial grid is unsolvable.

## Constraints

- Must work correctly for any `n` up to `n = 20` in under one second per solve on a modern laptop.
- Standard library only — no `numpy`, no `scipy`, no external installs.
- Module name is `lights_out` at the workdir root (i.e. `lights_out.py`); the grader does `from lights_out import solve` and binds against the function exactly as declared.

## Hint — but think about it yourself first

Brute force is `2^(n^2)` — infeasible past `n ≈ 5`. There's a much cleaner approach that runs in polynomial time. If your solution is exponential in `n`, you'll want to reconsider before submitting.

## How to deliver

Check the workdir-state preamble above for what files exist. **Use
the `write_file` tool** to author whatever is missing and the `solve`
body. Files written via tools are the only thing the grader sees.

When the implementation is in place, signal completion with the
`agent_done` tool.

**Do NOT paste the solution into chat.** Only files written via tools count.
