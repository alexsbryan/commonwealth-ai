# Group strings by anagram (Scaffolded tier)

Given a vector of strings, partition them into groups of anagrams.

## Signature (fixed)

```rust
pub fn group_anagrams(strs: Vec<String>) -> Vec<Vec<String>>
```

## Behaviour

Return a vector of groups. Two strings belong to the same group iff
they are anagrams of each other (same multiset of characters).

Examples:
- `group_anagrams(vec!["eat", "tea", "tan", "ate", "nat", "bat"])`
  → groups: `["eat", "tea", "ate"]`, `["tan", "nat"]`, `["bat"]`
  (in some order; see "Ordering" below)
- `group_anagrams(vec!["a"])` → `[["a"]]`
- `group_anagrams(vec![])` → `[]`
- `group_anagrams(vec!["", ""])` → `[["", ""]]`

## Ordering (CRITICAL — half the tests check this)

Within each group, preserve the **order of first appearance** in the
input. Across groups, order groups by the **first appearance of any
member** in the input (so the group containing `strs[0]` comes
first).

A plain `HashMap::into_values().collect()` returns groups in
**arbitrary** order and will fail those tests. You MUST maintain a
separate insertion-order list of keys (or use a structure like
`IndexMap`) to emit groups in input order.

## Constraints

- All input strings are ASCII lowercase letters (or empty).
- Standard library only.
- Total time should be O(N · K log K) where N is the number of
  strings and K is the maximum string length.

## What's in the workdir

```
.
├── Cargo.toml
└── src/
    └── lib.rs   # `group_anagrams` stub with `todo!()`
```

## How to deliver

You are running in a tools-driven harness. Mandatory loop:

1. `write` the full file body to `src/lib.rs`.
2. `bash` with command `cargo build 2>&1` — if errors, fix and write again.
3. `bash` with command `cargo test --quiet --test integration` — if any test fails, fix and write again.
4. ONLY after step 3 shows `test result: ok` for all tests, signal completion with the `done` tool.

You MUST NOT signal `done` before `cargo test` has reported all
tests passing. Skipping verification scores zero — the grader only
trusts a clean test report.

Prefer `write` over `edit`. Files written via tools are the only
thing the grader sees.
