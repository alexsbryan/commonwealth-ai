# 2.1 balanced-parens — judge rubric anchors

## dim_b

### 0
No runnable function, or attempts string-level pair-stripping in
a loop (O(n²)) without recognising the stack pattern. Or only
handles one bracket type.

### 1
Stack approach but with bugs: counts openers/closers without
matching types (passes `(]` as balanced), or pops without checking
emptiness (panics on `)`).

### 2
Correct stack-based O(n) walk: push opener, on closer check the
top of stack matches the expected opener type. Empty stack on
closer = false. Non-empty stack at end = false. All twelve fixture
tests pass.

### 3
Clean canonical implementation: ~10 lines or less. Single match
over each char, single `Vec<char>` stack, idiomatic `pop`/`push`.
Type-table for opener→closer matching is implicit or expressed as
a `match`.

## dim_c

### 0
Incoherent code, panics on happy path, doesn't compile.

### 1
Compiles but noisy: HashMap allocation for the matching table when
a match would do, mutable variables that could be immutable, unused
imports.

### 2
Idiomatic Rust: `Vec<char>` stack, no heap allocations beyond
that, `for c in s.chars() { match c { ... } }` shape. No warnings.

### 3
Minimal — function body ≤ 12 lines. No noise.
