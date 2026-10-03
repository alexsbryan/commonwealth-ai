# h.3 Units split — judge rubric anchors

Two judged dimensions. Pick exactly one anchor each; return
`{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Refactor coherence** — public API preserved, no new
dependencies, modules cohesive?

### 0
Public API broken or third-party dependencies introduced.

### 1
API preserved but the split is arbitrary (functions scattered
without responsibility grouping).

### 2
API preserved with a clean extraction and descriptive module names.

### 3
Exemplary decomposition — cohesive modules grouped by
responsibility (conversion tables, temperature, parsing/formatting),
stable API.

## dim_c

`dim_c` is **File organization** — files syntactically valid,
importable, reasonably named, all within the size budget?

### 0
Broken state — syntax errors, dead cross-module references.

### 1
Importable but sloppy (dumping-ground module names, size budget
missed).

### 2
Valid, importable, within budget, minor naming roughness.

### 3
Valid, within budget, well named — a reviewer would merge unchanged.
