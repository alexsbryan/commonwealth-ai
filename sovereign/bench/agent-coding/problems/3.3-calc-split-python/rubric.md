# 3.3 Calc split — judge rubric anchors (stub)

Multi-file refactor problem. The meaningful gate is `dim_a` (tests
pass) plus the structural metric the multi-file solver reports
out-of-band. The judged dimensions below are stubs so the bench's
schema is satisfied.

## dim_b

`dim_b` is **Refactor coherence** — did the candidate preserve the
public API and avoid introducing new dependencies?

### 0
Candidate produced a result that breaks the public API
(`evaluate`, `solve_linear`, `statistics` no longer importable from
`calc`), or pulled in a new third-party dependency.

### 1
Candidate maintained the public API and didn't introduce new
dependencies. The default anchor for this stub.

### 2
Candidate not only preserved the public API but produced a clean
extraction with descriptive module names.

### 3
Candidate produced an exemplary decomposition: public API stable,
modules cohesive, helpers grouped by responsibility.

## dim_c

`dim_c` is **File organization** — are the resulting files
syntactically valid, importable, and reasonably named?

### 0
Files left in a mechanically-broken state — syntax errors, missing
imports, dead references between modules.

### 1
Files are syntactically valid and importable. The default anchor
for this stub.

### 2
Files are well-named and the import graph is acyclic with no dead
re-exports.

### 3
Files demonstrate strong separation of concerns; a new reader can
find each helper in the file its name suggests.
