# h.4 Ledger package — judge rubric anchors

Two judged dimensions. Pick exactly one anchor each; return
`{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Cross-file diagnosis** — did the candidate trace each
failure to the module that owns it and fix it there, rather than
patching symptoms at the call site or rewriting whole modules?

### 0
Rewrote the package wholesale or moved logic between modules to
mask bugs.

### 1
Fixed one or two bugs, but patched at wrong layers or churned
working modules.

### 2
Most bugs fixed in their home modules with targeted edits.

### 3
All five bugs fixed in their owning modules with minimal diffs.

## dim_c

`dim_c` is **Contract fidelity** — signed amounts, overdraft
atomicity, zero-default reads, report ordering and formatting all
match the stated contract exactly?

### 0
Contract violated in multiple places.

### 1
Visible tests pass; contract corners loose.

### 2
Contract honored with minor looseness.

### 3
Contract honored precisely, including atomic overdraft rejection
and tie-breaking.
