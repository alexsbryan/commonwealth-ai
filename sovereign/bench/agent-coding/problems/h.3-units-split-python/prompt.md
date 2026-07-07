# Units — split a working module (Python, refactor)

The workdir contains `units.py`, a working ~95-line module: length,
mass and temperature conversion, quantity parsing, end-to-end
conversion, and formatting. All 6 behavior tests pass.

Your task: **split `units.py` into source files where every source
file is ≤ 30 lines**, while keeping all 6 behavior tests passing.

- Public functions must remain importable as
  `from units import convert_length, convert_mass,
  convert_temperature, parse_quantity, convert_quantity,
  format_quantity`.
- The test suite includes a size ladder
  (`max(line_count(f) for f in *.py)` at 80/60/45/30) so each
  extraction that shrinks the largest file makes progress.
- Group helpers by responsibility; keep imports between your new
  modules working.
- Standard library only.
