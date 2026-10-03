# 3.2 Light's Out — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`. Pick exactly
one anchor; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

## dim_b

`dim_b` is **Algorithmic insight** — does the candidate recognise the
problem's structure (linear system over GF(2)) and choose an
appropriate solution shape, or do they reach for an exponential brute
force / hand-tuned heuristic?

### 0
The candidate either does not produce a runnable solver, or attempts
a brute force over `2^(n^2)` press subsets without recognising the
combinatorial blowup. Includes attempts that explicitly try every
subset, or a hand-rolled DFS over presses without pruning to the
chase-the-lights or Gaussian-elimination shape. A candidate that
clearly does not understand "pressing twice is a no-op" sits here.

### 1
The candidate recognises that pressing is commutative and pressing
twice is a no-op (so we only care about a subset), and either:
- attempts the **chase the lights** elimination heuristic (fix the
  top row, propagate downward, verify the last row), but misses the
  step that the top row must be searched over `2^n`; or
- writes a polynomial solver that is correct on small grids but
  doesn't return a minimum-cardinality solution; or
- relies on a third-party crate (which the problem forbids).
The right algorithmic family is present but the implementation is
incomplete or off-spec.

### 2
The candidate identifies the problem as a linear system over GF(2)
and reduces to Gaussian elimination over an `n^2 × n^2` matrix, or
implements the `2^n`-over-top-row chase variant correctly. Solution
runs in polynomial time, handles the n=20 case within budget, and
detects unsolvable grids. The minimum-cardinality requirement may
not be perfectly addressed (e.g. returns any consistent solution,
not necessarily minimum, but documents the limitation).

### 3
The candidate identifies the GF(2) linear-system view, implements
clean Gaussian elimination (or equivalently, the `2^n` chase
variant), and either:
- returns a genuinely minimum-cardinality solution by enumerating
  the kernel of the press matrix and minimising; or
- correctly argues why the produced solution is minimum in the
  ranks where it is, with a clear path to enumerate the kernel for
  full minimality. The code is idiomatic Rust, the GF(2) operations
  are explained or self-evident, and the implementation makes the
  algorithm clear at a glance.

## dim_c

`dim_c` is **Code quality and efficiency** — given the algorithmic
choice the candidate made, how cleanly is it expressed, how
appropriately are types used, and how efficiently does the
implementation run within the chosen family?

### 0
Code is incoherent or non-idiomatic Rust: heavy use of unnecessary
`unsafe`, leaked panics on the happy path, abuse of `Vec<Vec<u8>>`
where a single flat `Vec<bool>` would have served, or O(n^4) memory
where O(n^2) suffices. May not compile, or compiles but generates
huge amounts of allocator churn per call.

### 1
Code compiles and runs but has obvious efficiency issues for the
chosen algorithm (e.g. quadratic-where-linear copies, integer types
wider than needed, repeated re-allocation of the press matrix per
call). Naming is acceptable; the structure of the algorithm is
recognisable but obscured by unnecessary intermediate state.

### 2
Code is idiomatic Rust: appropriate types (`u8` or `bool` bitset for
the grid, `Vec<Vec<u8>>` for the augmented matrix or a flat backing
array), no unnecessary allocations on the hot path, clean separation
between the press-application step and the elimination step.
Comments where the algorithm isn't self-evident from the code.

### 3
Code is genuinely tight: bit-packed representations where they
matter, the elimination loop is a model of clarity, allocations are
amortised, and the public API (`solve`) is exactly the signature the
problem asks for with no extraneous helpers leaking out. Reads like
a published reference implementation. Performance on `n=20`
comfortably under 100ms, often well under.
