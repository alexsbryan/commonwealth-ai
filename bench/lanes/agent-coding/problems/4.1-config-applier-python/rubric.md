# 4.1 Config applier — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`. Pick exactly
one anchor; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Diagnosis discipline** — does the candidate read each
smoke-test failure carefully, identify which of the four buggy
functions the failure points at, and fix that function specifically
without speculatively rewriting other code?

### 0
The candidate either does not attempt fixes or rewrites the whole
module on every cycle with no apparent connection to specific test
failures. Examples: every Implementer turn re-emits all four
functions from scratch; the diagnosis ignores the failing test
names entirely; the candidate "fixes" functions that no test
flagged.

### 1
The candidate sometimes targets a specific failing test but mixes
in unrelated changes (e.g., fixes `deep_merge` but also rewrites
`expand_env` even though `expand_env`'s tests were passing). Some
fixes regress previously-passing tests. The diagnosis reads more
like a summary than an analysis. Final state may have passing tests
but it took many wasted cycles to get there.

### 2
The candidate maps each failing test to its function correctly and
makes a targeted change per cycle. Occasional regressions, but they
are caught and reverted. The Evaluator's diagnoses cite the
specific test name and assertion, and the Implementer responds in
kind. Final state passes most tests; the path was reasonably
disciplined.

### 3
The candidate fixes one bug per cycle with surgical precision. Each
fix touches only the buggy function; every previously-passing test
keeps passing. Diagnoses cite the failing test by name and explain
which behavioral clause the bug violates. Convergence is fast —
ideally one cycle per bug, no regressions, ending at 12/12.

## dim_c

`dim_c` is **Surgical edits** — does the candidate use `patch_file`
for single-region fixes (smaller JSON-escape surface, less risk of
collateral damage) rather than re-emitting the entire module via
`write_file` every cycle?

### 0
Every change is a full-file rewrite via `write_file`. The candidate
never uses `patch_file` even when fixing a 3-line bug in a 150-line
module. Multiple write_file calls re-emit unchanged code verbatim;
broken token-level edits are common.

### 1
The candidate uses `write_file` predominantly but tries `patch_file`
once or twice — possibly with the wrong line range, or for a change
that genuinely needed a full rewrite. No clear principle in the
choice between primitives.

### 2
The candidate uses `patch_file` for most single-region fixes and
`write_file` only for the initial author or when restructuring.
Patch ranges are mostly correct; the candidate recovers from any
miscount by re-issuing a corrected patch_file rather than reverting
to write_file.

### 3
The candidate uses `patch_file` for every fix-after-first-write and
chooses tight line ranges that touch only the buggy block. Zero
collateral damage to unchanged code. The full file is written ONCE
(by the initial implementer turn or if the structure genuinely
needs to change); every subsequent edit is a patch.
