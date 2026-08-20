# 3.2 Light's Out (Python) — judge rubric anchors

Two judged dimensions (`dim_b` and `dim_c`). Each has four anchor
paragraphs corresponding to scores `0`, `1`, `2`, `3`. Pick exactly
one anchor; return `{"anchor": <0|1|2|3>, "rationale": "<short>"}`.

**This is the PYTHON variant.** The candidate writes `lights_out.py`
exposing `solve(grid: list[list[int]]) -> list[tuple[int, int]] | None`,
standard library only (no `numpy`, no `scipy`), correct for `n` up to
20 in **under one second per solve**. Judge Python on Python's terms —
anchors that reward another language's idioms make the top anchors
unreachable and defeat this problem's whole purpose, which is to
isolate language fluency from algorithmic capability by comparing
against the Rust variant.

## dim_b

`dim_b` is **Algorithmic insight** — does the candidate recognise the
problem's structure (linear system over GF(2)) and choose an
appropriate solution shape, or do they reach for an exponential brute
force / hand-tuned heuristic? This dimension is language-neutral: score
the algorithm, not the syntax.

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
- relies on a third-party package (which the problem forbids —
  standard library only).
The right algorithmic family is present but the implementation is
incomplete or off-spec.

### 2
The candidate identifies the problem as a linear system over GF(2)
and reduces to Gaussian elimination over an `n^2 × n^2` matrix, or
implements the `2^n`-over-top-row chase variant correctly. Solution
runs in polynomial time, handles the n=20 case within the one-second
budget, and detects unsolvable grids (returning `None`). The
minimum-cardinality requirement may not be perfectly addressed (e.g.
returns any consistent solution, not necessarily minimum, but
documents the limitation).

### 3
The candidate identifies the GF(2) linear-system view, implements
clean Gaussian elimination (or equivalently, the `2^n` chase
variant), and either:
- returns a genuinely minimum-cardinality solution by enumerating
  the kernel of the press matrix and minimising over it; or
- correctly argues why the produced solution is minimum in the
  ranks where it is, with a clear path to enumerate the kernel for
  full minimality.
The GF(2) operations are explained or self-evident, and the
implementation makes the algorithm clear at a glance.

## dim_c

`dim_c` is **Code quality and efficiency** — given the algorithmic
choice the candidate made, how cleanly is it expressed in **Python**,
how appropriately are the data structures chosen, and does it meet the
stated budget (n=20 under one second, standard library only)?

Note on what "efficient" means here: at n=20 the system is 400×400 over
GF(2). Representing each row as a Python `int` bitmask and eliminating
with `^=` is the idiom that comfortably meets the budget; a nested
`list[list[int]]` with per-element loops is roughly 400× more
interpreter work and is the usual reason a correct solution misses the
one-second constraint. That distinction is the main efficiency axis on
this problem.

### 0
Code is incoherent or non-idiomatic Python: mutable module-level state
mutated per call, bare `except:` swallowing real errors, `eval`/`exec`
where a data structure would serve, or a representation that makes the
n=20 case hopeless (e.g. materialising `2^(n^2)`). May raise on the
happy path, or import a forbidden third-party package.

### 1
Code runs but has obvious efficiency problems for the chosen
algorithm — per-element Python loops over a `list[list[int]]` matrix
where row-level `int` XOR would serve, the press matrix rebuilt from
scratch on every call, or repeated `list` copies inside the
elimination loop. Likely misses the one-second budget at n=20. Naming
is acceptable; the algorithm is recognisable but obscured by
unnecessary intermediate state.

### 2
Code is idiomatic Python: a coherent representation (row bitmasks as
`int`, or a flat `bytearray`/`list[int]` with clear indexing), no
redundant copying on the hot path, clean separation between building
the press matrix and running the elimination. Type hints match the
declared signature. Comfortably inside the one-second budget at n=20.
Comments where the algorithm isn't self-evident from the code.

### 3
Code is genuinely tight: rows carried as `int` bitmasks with XOR
elimination, the kernel enumeration bounded and explained, allocations
kept out of the inner loop, and the public surface is exactly
`solve(grid) -> list[tuple[int, int]] | None` with helpers kept private
(module-private names, no incidental exports). Reads like a published
reference implementation. Performance at n=20 well inside the budget.
