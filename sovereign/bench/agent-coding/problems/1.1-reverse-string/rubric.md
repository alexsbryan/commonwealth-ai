# 1.1 Reverse a string — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`. Pick exactly
one anchor; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Implementation choice** — did the candidate pick a
Unicode-correct strategy that matches the problem's specified
contract (reverse Unicode scalar values, not bytes), and avoid the
naive byte-reversal trap?

### 0
The candidate does not produce a runnable function, or produces a
function that reverses bytes (`s.bytes().rev().collect()`, manual
byte loop, `s.as_bytes().to_vec().reverse()`) and would return
invalid UTF-8 on multi-byte input. Includes implementations that
attempt to use indices into `s` byte-by-byte without char-boundary
awareness, or that depend on an external crate the prompt forbids.

### 1
The candidate recognises that bytes don't work and reaches for
scalar-aware iteration, but the implementation is awkward:
- collects to `Vec<char>`, reverses, re-builds via `String::from_iter`
  with extra copies; or
- uses `String::with_capacity(s.len())` then pushes chars without
  reasoning about why `s.len()` is a sufficient byte-capacity for
  the reversed string; or
- correct for ASCII but subtly fails on a corner like the empty
  string or a single-character input.
Right family of approach; implementation has rough edges.

### 2
The candidate writes a clean scalar-based reverser, typically
`s.chars().rev().collect()` or equivalent direct loop, that handles
ASCII, multi-byte UTF-8, CJK, and emoji correctly. Pre-sizes the
output buffer with `s.len()` when a manual loop is used. No unsafe;
no external crates.

### 3
The candidate writes the idiomatic `s.chars().rev().collect()` (or
the morally identical manual `for c in s.chars()` push-front loop
using `String::with_capacity(s.len())`), pre-sizes correctly, and
either:
- mentions briefly that this reverses by Unicode scalar value (not
  by grapheme cluster), so combining-mark sequences would
  re-order — matching the prompt's clarification; or
- the choice is so cleanly expressed that the doc-vs-comments
  surface is obviously zero-noise.

## dim_c

`dim_c` is **Code quality** — given the chosen approach, how cleanly
is it expressed and how appropriately does it use the standard
library?

### 0
Code is incoherent or non-idiomatic Rust: leftover scaffolding,
unused imports, panics on the happy path, or hand-rolled UTF-8
decoding that re-implements what `str::chars` already does. May not
compile.

### 1
Code compiles and runs but is noisier than it needs to be:
unnecessary intermediate `Vec` allocations, helper functions that
add no clarity, doc comments explaining what the code itself says
(`// reverses the string`), or imports that aren't used.

### 2
Code is idiomatic: one-line `chars().rev().collect()` or a tight
manual loop with `String::with_capacity(s.len())`. No leftover
scaffolding, no dead code, no unnecessary comments. Compiles
warning-free.

### 3
Code is idiomatic and minimal — typically a one-liner body — and
matches the project's surrounding style. No noise, no premature
abstraction, no commentary about what the code does. The body is
short enough to read at a glance and obviously correct.
