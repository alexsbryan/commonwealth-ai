# 1.3 lower_bound — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`.

## dim_b

### 0
Does not produce a runnable function, or implements an O(n) linear
scan (fails complexity requirement). Misses the binary-search
structure entirely.

### 1
Binary search shape is present but with off-by-one errors. Often:
- Returns -1 when target not present (instead of insertion index).
- Returns the rightmost equal element when duplicates exist.
- Fails empty-array edge case.
- Loop bound off by one (uses `lo <= hi` with mid-1/+1 in the wrong
  configuration).

### 2
Correct binary search with the `lower_bound` invariant: maintains
`lo` and `hi` such that the answer is in `[lo, hi]`, contracts via
`mid = (lo + hi) / 2` and `if arr[mid] < target { lo = mid + 1 }
else { hi = mid }`. Handles empty array, returns `arr.len()` when
target exceeds all elements. Passes all twelve fixture tests.

### 3
The clean canonical implementation: short body (≤ 10 lines), the
half-open interval `[lo, hi)` form OR the closed-interval form done
without off-by-one bugs. Uses `let mid = lo + (hi - lo) / 2` to
avoid overflow (or `usize` ops where overflow can't happen). The
invariant is obvious from the code without needing comments.

## dim_c

### 0
Code is incoherent: panics on the happy path, allocates a sorted
copy, unused imports/variables, doesn't compile.

### 1
Compiles but is noisier than needed: redundant casts, mutable vars
that should be immutable, magic numbers without explanation, an
unnecessary `Vec` allocation.

### 2
Idiomatic Rust: `usize` indices, no allocations, compiles
warning-free. Standard binary-search shape.

### 3
Idiomatic and minimal — typically ≤ 10 lines body. Uses
`mid = lo + (hi - lo) / 2` (overflow-aware) or equivalent. No
comments explaining what the code itself says.
