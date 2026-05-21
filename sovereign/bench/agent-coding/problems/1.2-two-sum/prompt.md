# Two-sum on a sorted array (Scaffolded tier)

Implement a function that finds two distinct indices in a sorted
array whose values sum to a target.

## Signature (fixed)

```rust
pub fn two_sum(arr: &[i64], target: i64) -> Option<(usize, usize)>
```

## Behaviour

- Returns `Some((i, j))` with `i < j` and `arr[i] + arr[j] == target`
  whenever such a pair exists.
- Returns `None` when no such pair exists.
- `arr` is guaranteed sorted in non-decreasing order. The same index
  must NOT be used twice (so `Some((i, i))` is never a valid answer).
- Time complexity must be O(n); the O(n²) brute force is rejected.

Examples:
- `two_sum(&[1, 2, 4, 7, 11], 9) == Some((2, 3))` (4 + 5 → no, 4 + 7 = 11 → wrong;
  actually 2 + 7 = 9 → `Some((1, 3))`. Stop and recheck: indices 1 and 3, values 2 and 7, sum 9. Yes.)
- `two_sum(&[1, 2, 3, 4], 100) == None`
- `two_sum(&[], 0) == None`
- `two_sum(&[5], 10) == None`
- `two_sum(&[3, 3], 6) == Some((0, 1))`

## What's in the workdir

```
.
├── Cargo.toml      # already correct; do not modify
└── src/
    └── lib.rs      # `two_sum` stub with `todo!()`
```

## How to deliver

You are running in a tools-driven harness. **Use the `write` tool to
rewrite `src/lib.rs` in full** with your implementation. Then verify
your work by running `cargo test --quiet --test integration` via the
`bash` tool — if any tests fail, fix the implementation and write
again. Signal completion with the `done` tool.

Prefer `write` over `edit` — `edit` requires the `oldText` to match
the file byte-for-byte including whitespace, which is brittle. With
`write` you provide the entire file body (the header comments + your
`two_sum` function).

**Files written via tools are the only thing the grader sees.** Do not
paste the solution into chat.
