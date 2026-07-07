# h.1 Run-length encoding — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`. Pick exactly
one anchor; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Algorithmic clarity** — is the run-scanning logic a
clean single pass with correct multi-digit count handling in decode?

### 0
No coherent run-scanning logic; hardcoded cases or broken loops.

### 1
Works for basic runs but decode mishandles multi-digit counts or
singles, or encode double-counts boundaries.

### 2
Correct single-pass encode and decode including multi-digit counts
and uncounted singles, with minor redundancy.

### 3
Clean, correct, single-pass encode/decode — e.g. groupby or an
explicit scanner — handling all edge cases without special-case
sprawl.

## dim_c

`dim_c` is **Code quality and efficiency** — linear time, no
quadratic string building in hot loops, readable names.

### 0
Mechanically broken or grossly inefficient (O(n^2) concatenation in
long loops with no justification).

### 1
Works but with awkward structure — repeated slicing, index
arithmetic that obscures the run logic.

### 2
Linear, readable, minor stylistic rough edges.

### 3
Idiomatic, linear, minimal — a reviewer would merge it unchanged.
