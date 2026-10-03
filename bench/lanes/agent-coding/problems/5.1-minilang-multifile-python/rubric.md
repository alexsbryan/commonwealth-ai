# 5.1 minilang multi-file — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`. Pick exactly
one anchor; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Cross-file diagnosis discipline** — does the candidate
attribute each failing test to the correct STAGE FILE (tokenizer vs
parser vs evaluator), recognise the cross-file cascade (a tokenizer
SyntaxError masking a parser or evaluator defect), and avoid
mis-blaming the file the traceback happens to point at?

### 0
No meaningful diagnosis. Fixes are scattered edits with no apparent
connection to specific failures, or the candidate edits one file
repeatedly while the real defect is in another. Never recognises that
a `SyntaxError` from `tokenizer.py` is masking a downstream bug.

### 1
Sometimes finds the right file but treats the traceback's file as
the culprit even when the defect is upstream — e.g. tries to fix
`parser.py` because the error surfaced there, when the cause is a
missing two-char token in `tokenizer.py`. Speculative cross-file
edits; the cascade is not understood.

### 2
Attributes most failures to the correct stage file and recognises
the cascade by cycle 3–4 (notices that a tokenizer fix unblocks
tests that then reveal parser/evaluator bugs). Occasional misroutes,
but recovers by re-reading the new failure on its own terms.

### 3
Localises each bug to the right file with precision and reads the
cascade fluently: when a tokenizer fix changes which tests fail, the
candidate doesn't conclude "my fix broke things" but follows the
failure downstream into the parser, then the evaluator. Fixes are
attributed to a specific function in a specific file. Reaches a full
pass without thrashing across files.

## dim_c

`dim_c` is **Multi-file navigation discipline** — does the candidate
open and edit the FILE that owns each bug, make surgical edits to it,
and avoid churning unrelated files or rewriting whole modules?

### 0
Edits files almost at random, or rewrites whole modules via
`write_file` every cycle. Repeatedly re-edits a file it already fixed,
or introduces regressions in a file unrelated to the current failure.
Token cost balloons.

### 1
Eventually edits the right files but with poor economy: full-file
rewrites where a `replace_function` would do, or several wrong-file
edits before landing on the right one. Little reuse of the prior
workdir state.

### 2
Edits the correct file for most fixes and prefers `patch_file` /
`replace_function` over full rewrites. Occasional whole-file rewrite
for a multi-line change. Navigation across the three files is mostly
purposeful.

### 3
Navigates the package fluently: goes straight to the owning file for
each bug, makes a surgical `patch_file` / `replace_function` edit, and
touches no file that doesn't need it. Minimal or zero full-file
rewrites; no redundant re-edits of already-fixed stages.
