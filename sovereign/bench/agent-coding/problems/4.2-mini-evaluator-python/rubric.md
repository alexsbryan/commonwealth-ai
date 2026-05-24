# 4.2 Mini evaluator — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`. Pick exactly
one anchor; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Diagnosis discipline across cascading fixes** — does
the candidate track WHICH bug each cycle revealed, distinguish
cascading-failure-from-lexer-bug from genuine-parser-bug, and avoid
over-fixing when one diagnosis would have sufficed?

### 0
The candidate makes no meaningful diagnosis. Fixes are scattered
random rewrites with no apparent connection to specific test
failures. The Evaluator's handoff diagnoses are vague or absent;
the Implementer fixes things no test flagged. Final state may pass
some tests but the path is incoherent.

### 1
The candidate sometimes identifies the right buggy stage but
treats every failure as a single layer (e.g., "all parser tests are
failing, must be a parser bug" without noticing that some failures
are downstream of a lexer bug). Speculative fixes are common.
Diagnoses don't account for the cascading structure of the problem.

### 2
The candidate identifies the buggy stage for most failing tests
and recognises the cascading pattern by cycle 3-4 (e.g., notices
that fixing the lexer unblocks parser tests that revealed new bugs).
Occasional over-rewrites, but mostly disciplined. Diagnoses cite
specific failing tests and trace the error to a function.

### 3
The candidate fixes one bug per cycle with surgical precision and
correctly attributes each fix to the right stage. Recognises
cascading dependencies early — when a lexer fix changes which
parser tests fail, the candidate doesn't conclude "the lexer fix
broke things" but instead reads the new failure on its own terms.
Reaches 20/20 in close to the minimum cycle count (≤ 6 cycles).

## dim_c

`dim_c` is **Navigation discipline (outline-only anchor)** — does
the candidate use the function-signature outline (the file is
above the inline anchor cap) to navigate by name and use
`patch_file` with correctly-inferred line ranges, rather than
rewriting the whole file via `write_file` every cycle?

### 0
The candidate ignores the outline and rewrites the entire 260-line
file via `write_file` on every fix. Token cost balloons.
Occasionally introduces unrelated regressions because of token-
level errors in the long rewrite.

### 1
The candidate tries `patch_file` once or twice with wrong line
ranges, gets rejection errors, and falls back to `write_file`
rewrites for the rest of the run. The outline is referenced but
not effectively used for navigation.

### 2
The candidate uses `patch_file` for most fixes and recovers from
line-range miscounts by reading the rejection error and re-emitting
with corrected ranges. Occasional `write_file` rewrites for
structural changes that span functions.

### 3
The candidate navigates by function-signature outline reliably:
finds the buggy function from its declaration line, infers the
body range from the next declaration's line number, patches surgically.
Zero or minimal full-file rewrites. Every line range is correct on
first attempt or is corrected on the second from the rejection
hint.
