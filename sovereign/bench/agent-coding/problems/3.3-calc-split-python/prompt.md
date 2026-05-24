# Calc — split a 97-line module (Python, Scaffolded tier)

The workdir contains `calc.py`, a 97-line module with three public
functions and three private helpers. Tests under `tests/test_calc.py`
pin the public API.

Your task: **split `calc.py` into source files where every source
file is ≤ 30 lines**, while keeping all 6 tests passing.

## Public API (must remain importable from `calc`)

- `evaluate(expression: str) -> float`
- `solve_linear(a, b) -> float | None`
- `statistics(values) -> (mean, variance, stddev)`

Private helpers may move freely to sibling modules.

## Constraints

- Standard library only — no new deps.
- Public functions must remain importable as `from calc import evaluate, solve_linear, statistics`.
- `python3 -m pytest -q tests/test_calc.py` must report 6 passed when you finish.

## Aggregate metric

`max(line_count(f) for f in *.py)` — the multi-file solver uses
this as the gating signal. The goal is met when this metric drops
to ≤ 30.

## How to deliver

Emit one single-file edit per turn. The harness will sequence your
moves, run tests between each, and roll back any move that breaks
them.
