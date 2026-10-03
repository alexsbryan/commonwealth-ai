# h.2 Roman numerals — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Pick exactly one anchor
each; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Diagnosis discipline** — did the candidate identify
each failing function from the test signal and fix that function
specifically, without speculatively rewriting working code?

### 0
Rewrote the module wholesale or fixed nothing relevant.

### 1
Fixed one or two bugs but disturbed working sections along the way.

### 2
Fixed most bugs with targeted edits; at most minor collateral churn.

### 3
All four bugs fixed with minimal, surgical diffs; untouched sections
byte-identical.

## dim_c

`dim_c` is **Correct-by-contract behavior** — do the fixes honor the
stated contract (canonical forms, exact error message, validation
semantics) rather than merely passing the visible tests?

### 0
Contract violated (wrong error types/messages, non-canonical
output).

### 1
Visible tests pass but contract corners are loose.

### 2
Contract honored with minor looseness.

### 3
Contract honored precisely, including error messages and canonical
round-trips.
