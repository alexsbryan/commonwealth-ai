# Light's Out — minimum-press solver (Scaffolded tier)

You're solving the classic **Light's Out** puzzle on an `n × n` grid.

Rules:
- Each cell is either lit (`1`) or dark (`0`).
- Pressing a cell at row `r`, column `c` **toggles** that cell **and** its 4 orthogonal neighbors (up, down, left, right), if they exist.
- Each cell can be pressed any number of times (but pressing twice has no net effect).
- Goal: starting from an arbitrary initial grid, find a sequence of presses that turns **every light off**.

The grid is bounded — there is no wrap-around. Corners have 3 neighbors, edges have 4 (including themselves), and interior cells have 5.

## What you find in the workdir (already there)

```
.
├── Cargo.toml      # already correct; do not modify
└── src/
    └── lib.rs      # contains a `solve` stub with `todo!()` — replace the body
```

## Your task

Replace the `todo!("…")` in `src/lib.rs` with a working implementation of:

```rust
pub fn solve(grid: &[Vec<u8>]) -> Option<Vec<(usize, usize)>>
```

- `grid` is square; `grid[r][c]` is `0` (dark) or `1` (lit).
- Returns `Some(presses)` where `presses` is a list of `(row, column)` pairs that, when applied in any order, turn every cell off. The list must be **minimum-cardinality** (any minimum-count solution is fine — ties broken however you like).
- Returns `None` if the initial grid is unsolvable.

## Constraints

- Must work correctly for any `n` up to `n = 20` in under one second per solve on a modern laptop.
- Must NOT depend on any external crate. Standard library only.
- Do not modify the function signature, the `Cargo.toml`, or the project layout — the grader rebinds against `lights_out::solve` exactly as declared.

## Hint — but think about it yourself first

Brute force is `2^(n^2)` — infeasible past `n ≈ 5`. There's a much cleaner approach that runs in polynomial time. If your solution is exponential in `n`, you'll want to reconsider before submitting.

## How to deliver

You are running in a tools-driven harness. The workdir already has the project scaffold. **Use the `edit` (or `write`) tool to replace the body of `solve` in `src/lib.rs`.** When the implementation is in place, reply with a one-line `DONE`.

**Do NOT paste the solution into chat.** Only files written via tools count. If your reply contains code in a fenced block but you never called `edit`/`write`, your score will be zero.
