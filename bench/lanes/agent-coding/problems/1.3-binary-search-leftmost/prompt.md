# Binary search — leftmost insertion point

Implement the canonical `lower_bound` / `bisect_left` operation on a
sorted array.

## Signature (fixed)

```rust
pub fn lower_bound(arr: &[i64], target: i64) -> usize
```

## Behaviour

Return the leftmost index `i` such that `arr[i] >= target`. If every
element of `arr` is strictly less than `target`, return `arr.len()`.

Examples:
- `lower_bound(&[1, 3, 5, 7], 5) == 2` (first index where value >= 5)
- `lower_bound(&[1, 3, 5, 7], 4) == 2` (insertion point for 4)
- `lower_bound(&[1, 3, 5, 7], 0) == 0` (insert before everything)
- `lower_bound(&[1, 3, 5, 7], 100) == 4` (insert at end)
- `lower_bound(&[5, 5, 5, 5], 5) == 0` (leftmost equal element)
- `lower_bound(&[], 42) == 0`

## Constraints

- Time complexity must be O(log n). The O(n) linear scan is rejected.
- `arr` is guaranteed sorted in non-decreasing order.
- Must work for empty arrays.
- Crate name is `lower_bound`. The grader rebinds
  `lower_bound::lower_bound` exactly as declared.

## How to deliver

Check the workdir-state preamble above for what files exist.
**Use the `write` tool** to author whatever is missing and to
implement `lower_bound`. Verify by running `cargo test --quiet
--test integration` via the `bash` tool — if any tests fail, fix
and write again. Signal completion with the `done` tool.

Prefer `write` over `edit` (exact-match brittleness).

**Files written via tools are the only thing the grader sees.**
