# Light's Out — minimum-press solver (Scaffolded tier)

You're solving the classic **Light's Out** puzzle on an `n × n` grid.

Rules:
- Each cell is either lit (`1`) or dark (`0`).
- Pressing a cell at row `r`, column `c` **toggles** that cell **and** its 4 orthogonal neighbors (up, down, left, right), if they exist.
- Each cell can be pressed any number of times (but pressing twice has no net effect).
- Goal: starting from an arbitrary initial grid, find a sequence of presses that turns **every light off**.

The grid is bounded — there is no wrap-around. Corners have 3 neighbors, edges have 4 (including themselves), and interior cells have 5.

## Your task

Implement:

```rust
pub fn solve(grid: &[Vec<u8>]) -> Option<Vec<(usize, usize)>>
```

- `grid` is square; `grid[r][c]` is `0` (dark) or `1` (lit).
- Returns `Some(presses)` where `presses` is a list of `(row, column)` pairs that, when applied in any order, turn every cell off. The list must be **minimum-cardinality** (any minimum-count solution is fine — ties broken however you like).
- Returns `None` if the initial grid is unsolvable.

## Constraints

- Must work correctly for any `n` up to `n = 20` in under one second per solve on a modern laptop.
- Must NOT depend on any external crate. Standard library only.
- Crate name is `lights_out`. The grader rebinds `lights_out::solve`
  exactly as declared, so the Cargo.toml `[package].name` must be
  `lights_out` and the function must be public at the crate root.

## Hint — but think about it yourself first

Brute force is `2^(n^2)` — infeasible past `n ≈ 5`. There's a much cleaner approach that runs in polynomial time. If your solution is exponential in `n`, you'll want to reconsider before submitting.

## How to deliver

Check the workdir-state preamble above for what files exist. **Use
the `write` tool** to author whatever is missing (Cargo.toml,
src/lib.rs) and to write the `solve` body. Prefer `write` over
`edit`: the `edit` tool's exact-match requirement on `oldText` is
brittle for multi-line code changes. With `write` you provide the
entire file body and the harness installs it verbatim.

When the implementation is in place, signal completion with the
`done` tool: `{"name":"done","arguments":{"reason":"…"}}`.

**Do NOT paste the solution into chat.** Only files written via tools count.
