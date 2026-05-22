# 1.2 Two-sum — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`.

## dim_b

### 0
Does not produce a runnable function, or implements an O(n²) nested
loop / brute-force search. Misses that the array is sorted.

### 1
Recognises sorted-array structure but implements something suboptimal:
e.g. binary search for each element (O(n log n) instead of O(n)), or
correct two-pointer but with bugs around equal-value indices that
trip the `(3, 3)` and same-index-twice tests.

### 2
Implements an O(n) two-pointer walk OR an O(n) HashMap pass. Handles
the edge cases (empty array, single element, duplicates) correctly.
Returns `i < j` consistently. May not produce the leftmost pair in
ambiguous cases but the contract permits any valid pair.

### 3
Clean idiomatic two-pointer: one `lo`/`hi` pair walking from both
ends, `arr[lo] + arr[hi]` compared to target, increment/decrement on
the smaller/larger side. Short body, no allocations. Optionally
returns the leftmost pair (smallest `lo`) on ambiguity.

## dim_c

### 0
Code is incoherent or non-idiomatic Rust: panics on the happy path,
allocates unnecessarily, leaves unused imports, doesn't compile.

### 1
Compiles and runs but is noisier than needed: unnecessary `Vec`
allocations on the hot path, mutable variables that could be
immutable, casts between integer types where none are needed.

### 2
Idiomatic Rust: index-typed `usize` cleanly, no casts, no
allocations on the hot path. Compiles warning-free.

### 3
Idiomatic and minimal — the function body is ~10 lines or less for
the two-pointer version. No noise, no leftover scaffolding.
